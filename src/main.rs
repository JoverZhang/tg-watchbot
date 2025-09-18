use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use std::time::Duration;
use teloxide::{
    prelude::*,
    types::{BotCommand, KeyboardButton, KeyboardMarkup, MenuButton},
};
use tracing::{error, info};

mod config;
mod db;
mod handlers;
mod model;
mod notion;
mod outbox;
mod thumbnail;

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

    let data_dir = cfg.app.resolved_data_dir();
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| format!("sqlite://{}/watchbot.db", data_dir));

    let pool = db::init_pool(&database_url).await?;
    db::run_migrations(&pool).await?;

    // Preflight dependency check
    thumbnail::ensure_ffmpeg_available().await?;

    // Spawn outbox worker (single-threaded)
    let notion_client =
        notion::NotionClient::new(cfg.notion.token.clone(), cfg.notion.version.clone());
    // Resolve Notion property IDs at startup; builders will use property IDs as keys.
    let notion_ids = notion_client.resolve_property_ids(&cfg).await?;
    let worker_pool = pool.clone();
    let poll_sleep = Duration::from_millis(cfg.app.poll_interval_ms);
    let max_backoff = cfg.app.max_backoff_seconds as i64;
    let worker_client = notion_client.clone();
    let worker_ids = notion_ids.clone();
    tokio::spawn(async move {
        loop {
            match outbox::process_next_task(&worker_pool, &worker_client, &worker_ids, max_backoff)
                .await
            {
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
            // Show keyboard and register commands only on /start to avoid spamming every message
            if let Some(text) = msg.text() {
                if text == "/start" {
                    bot.set_chat_menu_button()
                        .chat_id(msg.chat.id)
                        .menu_button(MenuButton::Default)
                        .await?;

                    bot.set_my_commands(vec![
                        BotCommand::new("begin", "Open a new batch"),
                        BotCommand::new("commit", "Commit current batch (will ask for title)"),
                        BotCommand::new("rollback", "Rollback current batch"),
                        BotCommand::new("ping", "Health check"),
                    ])
                    .await?;

                    bot.send_message(msg.chat.id, "Please select an action:")
                        .reply_markup(KeyboardMarkup::new(vec![
                            vec![
                                KeyboardButton::new("/begin"),
                                KeyboardButton::new("/commit"),
                            ],
                            vec![
                                KeyboardButton::new("/ping"),
                                KeyboardButton::new("/rollback"),
                            ],
                        ]))
                        .await?;
                }
            }

            if let Err(err) = handlers::handle_update(&bot, &pool, &data_dir, &msg).await {
                error!(?err, "failed to handle update");
            }
            respond(())
        }
    })
    .await;

    Ok(())
}
