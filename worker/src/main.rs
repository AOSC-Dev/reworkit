use anyhow::{ensure, Result};
use async_compression::tokio::write::GzipEncoder;
use clap::Parser;
use reqwest::{multipart::{self, Part}, Client};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{io::AsyncWriteExt, process::Command, task::spawn_blocking};
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
    let git_pull = Command::new("git")
        .arg("pull")
        .current_dir(&*tree_dir)
        .output()
        .await?;

    ensure!(git_pull.status.success(), "Failed to run git pull");

    let pkgs = spawn_blocking(move || list_packages(&tree_dir)).await?;

    let ciel_update = Command::new("ciel").arg("update-os").output().await?;
    ensure!(ciel_update.status.success(), "Failed to run ciel update-os");

    for pkg in pkgs {
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

        let mut log = vec![];
        log.extend("STDOUT:\n".as_bytes());
        log.extend(stdout);
        log.extend("STDERR:\n".as_bytes());
        log.extend(stderr);

        let mut compress_log = vec![];
        let mut encoder = GzipEncoder::new(&mut compress_log);
        encoder.write_all(&log).await?;

        let form = multipart::Form::new()
            .text("package", pkg.to_string())
            .text("arch", arch.to_string())
            .text("success", success.to_string())
            .part("log", Part::bytes(log).file_name(format!("{pkg}.log")));

        client
            .post(format!("{url}/push_log"))
            .header("SECRET", token)
            .multipart(form)
            .send()
            .await?;
    }

    Ok(())
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
