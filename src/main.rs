use axum::extract::DefaultBodyLimit;
use sqlx::{PgPool, Pool, Postgres};
use std::{path::PathBuf, sync::Arc};

use anyhow::{anyhow, Context, Result};
use async_compression::tokio::bufread::GzipDecoder;
use axum::{
    extract::{Multipart, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    io::{self},
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
    db: Pool<Postgres>,
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
    let pg = std::env::var("REWORKIT_PGCON").context("REWORKIT_PGCON is not set.")?;
    let log_dir =
        PathBuf::from(std::env::var("REWORKIT_LOG_DIR").context("REWORKIT_LOG_DIR is not set.")?);

    let db = PgPool::connect(&pg).await?;

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

#[derive(Debug, Deserialize, Serialize)]
struct Package {
    name: String,
    arch: String,
    success: bool,
    log: String,
}

async fn get_package_result(
    State(state): State<Arc<AppState>>,
    Query(query): Query<GetPackageResultQuery>,
) -> Result<Json<Vec<Package>>, AnyhowError> {
    let packages: Vec<Package> = sqlx::query_as!(
        Package,
        "SELECT name, arch, success, log FROM build_result WHERE name = $1",
        query.name
    )
    .fetch_all(&state.db)
    .await?;

    Ok(Json(packages))
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

    let pkg = Package {
        name: pkgname,
        arch,
        success,
        log: filename.to_string(),
    };

    sqlx::query!(
        r#"INSERT INTO build_result VALUES ($1, $2, $3, $4)
ON CONFLICT (name, arch) DO UPDATE SET success=$3, log=$4"#,
        pkg.name,
        pkg.arch,
        pkg.success,
        pkg.log
    )
    .fetch_one(&state.db)
    .await?;

    Ok(())
}

async fn write_log(log_content: Vec<u8>, log_dir: PathBuf, fc: Arc<String>) -> Result<()> {
    fs::create_dir_all(&log_dir).await?;
    let mut reader = GzipDecoder::new(&*log_content);
    let mut f = fs::File::create(log_dir.join(&*fc)).await?;
    io::copy(&mut reader, &mut f).await?;

    Ok(())
}
