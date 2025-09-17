use anyhow::Result;
use std::time::Duration;
use teloxide::prelude::*;
use tracing::{error, info};

mod db;
mod handlers;
mod model;
mod notion;
mod outbox;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .compact()
        .init();

    let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://data/watchbot.db".to_string());
    let data_dir = std::env::var("DATA_DIR").unwrap_or_else(|_| "data".to_string());
    tokio::fs::create_dir_all(&data_dir).await.ok();

    let pool = db::init_pool(&database_url).await?;
    db::run_migrations(&pool).await?;

    // Spawn outbox worker (single-threaded)
    let notion_client = notion::RealNotionClient::from_env();
    let worker_pool = pool.clone();
    tokio::spawn(async move {
        loop {
            match outbox::process_next_task(&worker_pool, &notion_client).await {
                Ok(processed) => {
                    if !processed {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                    }
                }
                Err(err) => {
                    error!(?err, "outbox worker error");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    });

    let bot = Bot::from_env();

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

