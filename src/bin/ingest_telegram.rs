use anyhow::Result;
use clap::Parser;
use serde_json::to_string_pretty;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use teloxide::prelude::*;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use tg_watchbot::config;
use tg_watchbot::db;
use tg_watchbot::handlers;
use tg_watchbot::model::BatchState;
use tg_watchbot::notion::{build_main_page_request, build_resource_page_request, NotionIds};

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

    /// When set, print Notion payloads instead of sending them.
    #[arg(long)]
    dry_run_notion: bool,
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
    let notion_ids = Arc::new(cfg.notion_ids());

    let data_dir = cfg.app.resolved_data_dir();
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| format!("sqlite://{}/watchbot.db", data_dir));

    let pool = db::init_pool(&database_url).await?;
    db::run_migrations(&pool).await?;

    let dry_run_state: Option<Arc<Mutex<HashSet<i64>>>> = if args.dry_run_notion {
        Some(Arc::new(Mutex::new(HashSet::new())))
    } else {
        None
    };

    let bot = Bot::new(cfg.telegram.bot_token.clone());
    let allowed = cfg.telegram.allowed_users.clone();
    let dry_run_flag = args.dry_run_notion;

    info!(database_url=%database_url, data_dir=%data_dir, "starting ingest-only telegram bot");
    teloxide::repl(bot, move |bot: Bot, msg: Message| {
        let pool = pool.clone();
        let data_dir = data_dir.clone();
        let allowed = allowed.clone();
        let notion_ids = notion_ids.clone();
        let dry_run_state = dry_run_state.clone();
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

            if dry_run_flag {
                if let Some(state) = &dry_run_state {
                    if let Err(err) = print_pending_notion_payloads(&pool, &notion_ids, state).await
                    {
                        warn!(?err, "failed to render Notion dry-run payloads");
                    }
                }
            }
            respond(())
        }
    })
    .await;

    Ok(())
}

async fn print_pending_notion_payloads(
    pool: &sqlx::SqlitePool,
    notion_ids: &NotionIds,
    printed_ids: &Arc<Mutex<HashSet<i64>>>,
) -> Result<()> {
    let tasks = db::list_due_outbox(pool).await?;
    if tasks.is_empty() {
        return Ok(());
    }

    let mut new_tasks = Vec::new();
    {
        let mut seen = printed_ids.lock().await;
        for (id, kind, ref_id) in tasks {
            if seen.insert(id) {
                new_tasks.push((id, kind, ref_id));
            }
        }
    }

    for (id, kind, ref_id) in new_tasks {
        match kind.as_str() {
            "push_batch" => {
                let batch = db::fetch_batch_for_outbox(pool, ref_id).await?;
                if batch.state != BatchState::COMMITTED {
                    warn!(batch_id = ref_id, state = ?batch.state, "batch not committed yet; skipping dry-run payload");
                    continue;
                }
                let title = batch
                    .title
                    .as_deref()
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or("Untitled");
                let body = build_main_page_request(notion_ids, title);
                println!(
                    "\n[outbox #{id}] Notion main page request (batch {ref_id})\n{}",
                    to_string_pretty(&body)?
                );
            }
            "push_resource" => {
                let resource = db::fetch_resource_for_outbox(pool, ref_id).await?;
                let parent_page = if let Some(batch_id) = resource.batch_id {
                    match resource.batch_state {
                        Some(BatchState::COMMITTED) => {
                            if let Some(page) = resource.batch_notion_page_id.clone() {
                                Some(page)
                            } else {
                                warn!(
                                    resource_id = ref_id,
                                    batch_id, "batch missing Notion page; resource pending"
                                );
                                continue;
                            }
                        }
                        Some(other) => {
                            warn!(resource_id = ref_id, batch_id, state = ?other, "batch not ready; skipping dry-run payload");
                            continue;
                        }
                        None => {
                            warn!(
                                resource_id = ref_id,
                                batch_id, "batch state unavailable; skipping dry-run payload"
                            );
                            continue;
                        }
                    }
                } else {
                    None
                };

                let text = resource.text.as_deref();
                let media_url = sanitize_media_url(resource.media_url.as_deref());
                let body = build_resource_page_request(
                    notion_ids,
                    parent_page.as_deref(),
                    resource.sequence,
                    text,
                    resource.media_name.as_deref().filter(|s| !s.is_empty()),
                    media_url.as_deref(),
                );
                println!(
                    "\n[outbox #{id}] Notion resource request (resource {ref_id})\n{}",
                    to_string_pretty(&body)?
                );
            }
            other => {
                warn!(
                    task_kind = other,
                    "unknown outbox kind for dry-run; ignoring"
                );
            }
        }
    }

    Ok(())
}

fn sanitize_media_url(raw: Option<&str>) -> Option<String> {
    let url = raw?.trim();
    if url.is_empty() {
        return None;
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return None;
    }
    if url.contains("api.telegram.org") {
        return None;
    }
    Some(url.to_string())
}
