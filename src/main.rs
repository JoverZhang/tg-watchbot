use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use std::time::Duration;
use teloxide::prelude::*;
use tracing::{error, info};

mod config;
mod db;
mod handlers;
mod model;
mod notion;
mod outbox;

#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Args {
    /// Path to YAML config file
    #[arg(long, default_value = "config.yaml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .compact()
        .init();

    let args = Args::parse();
    let cfg = config::load(Some(&args.config))?;
    cfg.ensure_dirs()?;

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| format!("sqlite://{}/watchbot.db", cfg.app.data_dir));
    let data_dir = cfg.app.data_dir.clone();

    let pool = db::init_pool(&database_url).await?;
    db::run_migrations(&pool).await?;

    // Spawn outbox worker (single-threaded)
    let notion_client = notion::RealNotionClient::from_config(&cfg);
    let worker_pool = pool.clone();
    let poll_sleep = Duration::from_millis(cfg.app.poll_interval_ms);
    let max_backoff = cfg.app.max_backoff_seconds as i64;
    tokio::spawn(async move {
        loop {
            match outbox::process_next_task(&worker_pool, &notion_client, max_backoff).await {
                Ok(processed) => {
                    if !processed {
                        tokio::time::sleep(poll_sleep).await;
                    }
                }
                Err(err) => {
                    error!(?err, "outbox worker error");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    });

    let bot = Bot::new(cfg.telegram.bot_token.clone());

    info!("starting telegram bot");
    teloxide::repl(bot, move |bot: Bot, msg: Message| {
        let pool = pool.clone();
        let data_dir = data_dir.clone();
        async move {
            if let Err(err) = handlers::handle_update(&bot, &pool, &data_dir, &msg).await {
                error!(?err, "failed to handle update");
            }
            respond(())
        }
    })
    .await;

    Ok(())
}
