mod db;

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use axum::{
    extract::{Multipart, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use db::Db;
use tokio::{fs, sync::Mutex};
use tracing::{error, info};

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
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let url = std::env::var("REWORKIT_URL").context("REWORKIT_URL is not set.")?;
    let secret = std::env::var("REWORKIT_SECRET").context("REWORKIT_SECTRET is not set.")?;
    let redis = std::env::var("REWORKIT_REDIS_URL").context("REWORKIT_REDIS_URL is not set.")?;

    let db = Mutex::new(Db::new(&redis).await?);

    let router = Router::new()
        .route("/push_log", post(push_log))
        .with_state(Arc::new(AppState { secret, db }));
    let listener = tokio::net::TcpListener::bind(&url).await?;
    axum::serve(listener, router).await?;

    Ok(())
}

async fn push_log(
    State(state): State<Arc<AppState>>,
    header: HeaderMap,
    mut form: Multipart,
) -> Result<(), AnyhowError> {
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
        if let Err(e) = fs::write(&*fc, log_content).await {
            error!("Unale to write build log {fc}: {e}");
        }
    });

    // write to database
    let mut db = state.db.lock().await;
    let mut package = db.get(&pkgname).await?;

    package.results.push(db::BuildResult {
        arch,
        success,
        log: filename.to_string(),
    });

    db.set(&pkgname, &package).await?;

    Ok(())
}
