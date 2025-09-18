use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use teloxide::prelude::*;
use tracing::{error, info};

use tg_watchbot::config;
use tg_watchbot::db;
use tg_watchbot::handlers;

#[derive(Debug, Parser)]
#[command(
    author,
    version,
    about = "Ingest all Telegram messages into the SQLite resource table"
)]
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

    let data_dir = cfg.app.resolved_data_dir();
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| format!("sqlite://{}/watchbot.db", data_dir));

    let pool = db::init_pool(&database_url).await?;
    db::run_migrations(&pool).await?;

    let bot = Bot::new(cfg.telegram.bot_token.clone());
    let allowed = cfg.telegram.allowed_users.clone();

    info!(database_url=%database_url, data_dir=%data_dir, "starting ingest-only telegram bot");
    teloxide::repl(bot, move |bot: Bot, msg: Message| {
        let pool = pool.clone();
        let data_dir = data_dir.clone();
        let allowed = allowed.clone();
        async move {
            if let Some(from) = msg.from() {
                let uid = from.id.0 as i64;
                if !allowed.is_empty() && !allowed.contains(&uid) {
                    return respond(());
                }
            }

            if let Err(err) = handlers::handle_update(&bot, &pool, &data_dir, &msg).await {
                error!(?err, "failed to ingest message");
            }
            respond(())
        }
    })
    .await;

    Ok(())
}
