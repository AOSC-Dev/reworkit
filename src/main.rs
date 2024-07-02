use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    extract::Multipart,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use tokio::fs;
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

#[tokio::main]
async fn main() -> Result<()> {
    let url = std::env::var("REWORKIT_URL").context("REWORKIT_URL is not set.")?;

    let router = Router::new().route("/push_log", post(push_log));
    let listener = tokio::net::TcpListener::bind(&url).await?;
    axum::serve(listener, router).await?;

    Ok(())
}

async fn push_log(mut form: Multipart) -> Result<(), AnyhowError> {
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
                info!("Received arch: {}", arch_field);
                arch = Some(arch_field);
            }
            Some("success") => {
                let success_field = field.text().await?;
                info!("Received success: {}", success_field);
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
    let filename = Arc::new(format!("{pkgname}-{arch}.log"));
    let fc = filename.clone();

    tokio::spawn(async move {
        if let Err(e) = fs::write(&*fc, log_content).await {
            error!("Unale to write build log {fc}: {e}");
        }
    });

    // TODO: write to database

    Ok(())
}
