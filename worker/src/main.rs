use anyhow::{ensure, Result};
use async_compression::tokio::write::GzipEncoder;
use clap::Parser;
use reqwest::{
    multipart::{self, Part},
    Client,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc, time::Duration,
};
use tokio::{io::AsyncWriteExt, process::Command, task::spawn_blocking};
use tracing::{error, info, level_filters::LevelFilter};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer};
use walkdir::WalkDir;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// CIEL! workspace path
    #[clap(short = 'd', long, env = "REWORKIT_CIEL_WORKSPACE")]
    workspace: PathBuf,
    /// Instance architecture
    #[clap(short, long, env = "REWORKIT_ARCH")]
    arch: String,
    /// Instance name (default: main)
    #[clap(short, long, default_value = "main", env = "REWORKIT_CIEL_INSTANCE")]
    name: String,
    /// ReworkIt! server url
    #[clap(short, long, env = "REWORKIT_URL")]
    url: String,
    #[clap(short, long, env = "REWORKIT_SECRET_TOKEN")]
    /// ReworkIt! secret token
    token: String,
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

    let Args {
        workspace,
        arch,
        name,
        url,
        token,
    } = Args::parse();

    let tree_dir = Arc::new(workspace.join("TREE"));
    let client = Client::builder().user_agent("reworkit").build()?;

    loop {
        if let Err(e) = work(tree_dir.clone(), &name, &client, &token, &url, &arch).await {
            eprintln!("Error: {}", e);
        }
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    }
}

async fn work(
    tree_dir: Arc<PathBuf>,
    name: &str,
    client: &Client,
    token: &str,
    url: &str,
    arch: &str,
) -> Result<()> {
    info!("Running git pull");
    let git_pull = Command::new("git")
        .arg("pull")
        .current_dir(&*tree_dir)
        .output()
        .await?;

    ensure!(git_pull.status.success(), "Failed to run git pull");

    info!("Getting packages");
    let pkgs = spawn_blocking(move || list_packages(&tree_dir)).await?;

    info!("Running ciel update-os");
    let ciel_update = Command::new("ciel").arg("update-os").output().await?;
    ensure!(ciel_update.status.success(), "Failed to run ciel update-os");

    for pkg in pkgs {
        info!("Building {pkg}");
        let ciel_build = Command::new("ciel")
            .arg("build")
            .arg("-i")
            .arg(&name)
            .arg(&pkg)
            .output()
            .await?;

        let stdout = ciel_build.stdout;
        let stderr = ciel_build.stderr;
        let success = ciel_build.status.success();

        info!("is success: {}", success);

        let mut log = vec![];
        log.extend("STDOUT:\n".as_bytes());
        log.extend(stdout);
        log.extend("STDERR:\n".as_bytes());
        log.extend(stderr);

        let compress_log = match compression_log(log).await {
            Ok(log) => log,
            Err(e) => {
                error!("Compress LOG got error: {}", e);
                continue;
            }
        };

        'a: for i in 1..=3 {
            match push_log(
                client,
                token,
                arch,
                &pkg,
                success,
                compress_log.clone(),
                url,
            )
            .await
            {
                Ok(_) => break 'a,
                Err(e) => {
                    error!("({}/3) Push LOG got error: {}", i, e);
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            }
        }
    }

    Ok(())
}

async fn push_log(
    client: &Client,
    token: &str,
    arch: &str,
    pkg: &str,
    success: bool,
    compress_log: Vec<u8>,
    url: &str,
) -> Result<()> {
    let form = multipart::Form::new()
        .text("package", pkg.to_string())
        .text("arch", arch.to_string())
        .text("success", success.to_string())
        .part(
            "log",
            Part::bytes(compress_log).file_name(format!("{pkg}.log")),
        );

    client
        .post(format!("{url}/push_log"))
        .header("SECRET", token)
        .multipart(form)
        .send()
        .await?;

    Ok(())
}

async fn compression_log(log: Vec<u8>) -> Result<Vec<u8>> {
    let mut compress_log = vec![];
    let mut encoder = GzipEncoder::new(&mut compress_log);
    encoder.write_all(&log).await?;
    encoder.shutdown().await?;

    Ok(compress_log)
}

fn list_packages(tree_dir: &Path) -> Vec<String> {
    let mut pkgs = vec![];
    for entry in WalkDir::new(tree_dir).min_depth(2).max_depth(2) {
        if let Ok(entry) = entry {
            let path = entry.path();
            if path.to_string_lossy().contains("/.git")
                || path.starts_with(tree_dir.join("groups"))
                || path.starts_with(tree_dir.join("assets"))
            {
                continue;
            }

            if entry.file_type().is_dir() {
                let package_name = entry.file_name().to_string_lossy().to_string();
                pkgs.push(package_name);
            }
        }
    }

    pkgs
}
