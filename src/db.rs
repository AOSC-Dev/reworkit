use anyhow::Result;
use redis::{aio::MultiplexedConnection, AsyncCommands};
use serde::{Deserialize, Serialize};

pub struct Db {
    conn: MultiplexedConnection,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Package {
    pub name: String,
    pub results: Vec<BuildResult>
}

#[derive(Debug, Deserialize, Serialize)]
pub struct BuildResult {
    pub arch: String,
    pub success: bool,
    pub log: String,
}

impl Db {
    pub async fn new(redis: &str) -> Result<Self> {
        let client = redis::Client::open(redis)?;
        let conn = client.get_multiplexed_tokio_connection().await?;

        Ok(Self { conn })
    }

    pub async fn get(&mut self, pkg: &str) -> Result<Package> {
        let s: String = self.conn.get::<_, _>(format!("reworkit:{}", pkg)).await?;
        let package: Package = serde_json::from_str(&s)?;
        
        Ok(package)
    }

    pub async fn set(&mut self, pkg: &str, v: &Package) -> Result<()> {
        let v = serde_json::to_string(v)?;
        self.conn.set::<_, _, _>(format!("reworkit:{}", pkg), v).await?;
        
        Ok(())
    }
}
