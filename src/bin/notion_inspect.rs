use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use tg_watchbot::config::Config;
use tg_watchbot::notion::NotionClient;

#[derive(Parser, Debug)]
struct Args {
    /// Path to YAML config
    #[arg(long, default_value = "config.yaml")]
    config: PathBuf,

    /// Database ID to inspect
    #[arg(long)]
    db_id: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let raw = fs::read_to_string(&args.config)?;
    let cfg: Config = serde_yaml::from_str(&raw)?;
    let client = NotionClient::new(cfg.notion.token.clone(), cfg.notion.version.clone());

    let db = client.retrieve_database(&args.db_id).await?;
    println!("Database ID: {}", db.id);
    println!("Properties:");
    for (name, prop) in db.properties {
        println!("  {} -> {{ id: {}, type: {} }}", name, prop.id, prop.typ);
    }
    Ok(())
}
