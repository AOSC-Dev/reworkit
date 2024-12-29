mod db;

use axum::extract::DefaultBodyLimit;
use std::{collections::HashMap, path::PathBuf, sync::Arc};

use anyhow::{anyhow, Context, Result};
use async_compression::tokio::bufread::GzipDecoder;
use axum::{
    extract::{Multipart, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use db::{BuildResult, Db, Package};
use serde::Deserialize;
use tokio::{
    fs,
    io::{self},
    sync::Mutex,
};
use tracing::{error, info, level_filters::LevelFilter};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer};

// learned from https://github.com/tokio-rs/axum/blob/main/examples/anyhow-error-response/src/main.rs
pub struct AnyhowError(anyhow::Error);

impl IntoResponse for AnyhowError {
    fn into_response(self) -> Response {
        info!("Returing internal server error for {}", self.0);
        (StatusCode::INTERNAL_SERVER_ERROR, format!("{}", self.0)).into_response()
    }
}

impl<E> From<E> for AnyhowError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

struct AppState {
    secret: String,
    db: Mutex<Db>,
    log_dir: PathBuf,
}

#[derive(Deserialize)]
struct GetPackageResultQuery {
    name: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let env_log = EnvFilter::try_from_default_env();

    if let Ok(filter) = env_log {
        tracing_subscriber::registry()
            .with(
                fmt::layer()
                    .event_format(
                        tracing_subscriber::fmt::format()
                            .with_file(true)
                            .with_line_number(true),
                    )
                    .with_filter(filter),
            )
            .init();
    } else {
        tracing_subscriber::registry()
            .with(
                fmt::layer()
                    .event_format(
                        tracing_subscriber::fmt::format()
                            .with_file(true)
                            .with_line_number(true),
                    )
                    .with_filter(LevelFilter::INFO),
            )
            .init();
    }

    let url = std::env::var("REWORKIT_URL").context("REWORKIT_URL is not set.")?;
    let secret = std::env::var("REWORKIT_SECRET").context("REWORKIT_SECRET is not set.")?;
    let redis = std::env::var("REWORKIT_REDIS_URL").context("REWORKIT_REDIS_URL is not set.")?;
    let log_dir =
        PathBuf::from(std::env::var("REWORKIT_LOG_DIR").context("REWORKIT_LOG_DIR is not set.")?);

    let db = Mutex::new(Db::new(&redis).await?);

    let router = Router::new()
        .layer(DefaultBodyLimit::disable())
        .route("/push_log", post(push_log))
        .route("/get", get(get_package_result))
        .with_state(Arc::new(AppState {
            secret,
            db,
            log_dir,
        }));
    let listener = tokio::net::TcpListener::bind(&url).await?;
    axum::serve(listener, router).await?;

    Ok(())
}

async fn get_package_result(
    State(state): State<Arc<AppState>>,
    Query(query): Query<GetPackageResultQuery>,
) -> Result<Json<Package>, AnyhowError> {
    let mut db = state.db.lock().await;
    let package = db.get(&query.name).await?;

    Ok(Json(package))
}

async fn push_log(
    State(state): State<Arc<AppState>>,
    header: HeaderMap,
    mut form: Multipart,
) -> Result<(), AnyhowError> {
    let log_dir = state.log_dir.clone();

    if header
        .get("SECRET")
        .and_then(|x| x.to_str().ok())
        .map(|x| x != state.secret)
        .unwrap_or(true)
    {
        return Err(anyhow!("Invalid secret token").into());
    }

    let mut pkgname = None;
    let mut arch = None;
    let mut log_content = Vec::new();
    let mut success = None;

    while let Some(field) = form.next_field().await? {
        match field.name() {
            Some("package") => {
                let package_name = field.text().await?;
                info!("Received package: {}", package_name);
                pkgname = Some(package_name);
            }
            Some("arch") => {
                let arch_field = field.text().await?;
                arch = Some(arch_field);
            }
            Some("success") => {
                let success_field = field.text().await?;
                success = Some(success_field);
            }
            Some("log") => {
                let log = field.bytes().await?;
                log_content.extend(log);
            }
            _ => {
                info!("Received unknown field: {:?}", field.name());
            }
        }
    }

    let pkgname = pkgname.context("Missing package field")?;
    let arch = arch.context("Missing arch field")?;
    let success = success.context("Missing success field")?;
    let success = if success == "true" { true } else { false };
    let filename = Arc::new(format!("{pkgname}-{arch}.log"));
    let fc = filename.clone();

    tokio::spawn(async move {
        if let Err(e) = write_log(log_content, log_dir, fc).await {
            error!("Error writing log: {}", e);
        }
    });

    // write to database
    let mut db = state.db.lock().await;
    let package = db.get(&pkgname).await;

    let result = BuildResult {
        success,
        log: filename.to_string(),
    };

    if let Ok(mut package) = package {
        package.results.insert(arch, result);
        db.set(&pkgname, &package).await?;
    } else {
        let mut package = Package {
            name: pkgname.clone(),
            results: HashMap::new(),
        };
        package.results.insert(arch, result);
        db.set(&pkgname, &package).await?;
    }

    Ok(())
}

async fn write_log(log_content: Vec<u8>, log_dir: PathBuf, fc: Arc<String>) -> Result<()> {
    fs::create_dir_all(&log_dir).await?;
    let mut reader = GzipDecoder::new(&*log_content);
    let mut f = fs::File::create(log_dir.join(&*fc)).await?;
    io::copy(&mut reader, &mut f).await?;

    Ok(())
}
