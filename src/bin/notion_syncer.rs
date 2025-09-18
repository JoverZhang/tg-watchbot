use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use tracing::{error, info, warn};

use tg_watchbot::config;
use tg_watchbot::db;
use tg_watchbot::notion::NotionClient;
use tg_watchbot::outbox;

#[derive(Debug, Parser)]
#[command(
    author,
    version,
    about = "Sync all pending outbox tasks to Notion and exit when complete"
)]
struct Args {
    /// Path to YAML config file
    #[arg(long, default_value = "config.yaml")]
    config: PathBuf,

    /// Skip failed tasks that are in backoff and exit when only failed tasks remain
    #[arg(long)]
    skip_failed: bool,

    /// Maximum attempts for failed tasks before considering them permanently failed (default: 5)
    #[arg(long, default_value = "5")]
    max_failed_attempts: i32,
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

    let notion_client = NotionClient::new(cfg.notion.token.clone(), cfg.notion.version.clone());
    let notion_ids = notion_client.resolve_property_ids(&cfg).await?;
    let max_backoff = cfg.app.max_backoff_seconds as i64;

    info!("Starting Notion sync process");

    // Check initial state
    let remaining = db::count_remaining_outbox_tasks(&pool).await?;
    let last_processed = db::get_last_processed_outbox_id(&pool).await?;
    info!(
        remaining_tasks = remaining,
        last_processed_id = last_processed,
        "Initial sync state"
    );

    if remaining == 0 {
        info!("No outbox tasks to process, exiting");
        return Ok(());
    }

    let mut processed_count = 0;
    let mut last_outbox_id = last_processed;

    loop {
        // Get the next task to process BEFORE processing it
        if let Some((next_task_id, _, _, _, _)) = db::next_due_outbox(&pool).await? {
            match outbox::process_next_task(&pool, &notion_client, &notion_ids, max_backoff).await {
                Ok(processed) => {
                    if processed {
                        processed_count += 1;

                        // Update the last processed ID to the task we just completed
                        last_outbox_id = next_task_id;
                        db::update_last_processed_outbox_id(&pool, last_outbox_id).await?;

                        if processed_count % 10 == 0 {
                            let remaining = db::count_remaining_outbox_tasks(&pool).await?;
                            info!(
                                processed = processed_count,
                                remaining = remaining,
                                last_processed_id = last_outbox_id,
                                "Sync progress"
                            );
                        }
                    } else {
                        // Task was not processed (likely due to backoff), wait and retry
                        warn!(
                            task_id = next_task_id,
                            "Task not ready for processing, waiting"
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                }
                Err(err) => {
                    error!(?err, task_id = next_task_id, "Error processing outbox task");
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        } else {
            // No more tasks to process
            let remaining = db::count_remaining_outbox_tasks(&pool).await?;
            if remaining == 0 {
                info!(
                    total_processed = processed_count,
                    last_processed_id = last_outbox_id,
                    "All outbox tasks synced successfully"
                );
                break;
            } else {
                // Check if all remaining tasks are in backoff (failed tasks)
                let failed_tasks: Vec<(i64, i32, String)> = sqlx::query_as(
                    "SELECT id, attempt, due_at FROM outbox"
                )
                .fetch_all(&pool)
                .await?;

                if !failed_tasks.is_empty() {
                    let max_attempts = failed_tasks.iter().map(|(_, attempt, _)| *attempt).max().unwrap_or(0);
                    let min_due_time = failed_tasks.iter()
                        .map(|(_, _, due_at)| due_at.as_str())
                        .min()
                        .unwrap_or("unknown");

                    warn!(
                        remaining = remaining,
                        max_attempts = max_attempts,
                        next_due_at = %min_due_time,
                        "No due tasks but {} tasks remain; all tasks are in backoff.",
                        remaining
                    );

                    // Check if any task has exceeded the max failed attempts threshold
                    if max_attempts >= args.max_failed_attempts {
                        error!(
                            max_attempts = max_attempts,
                            threshold = args.max_failed_attempts,
                            "Tasks have exceeded maximum failure attempts ({}), exiting to prevent infinite retries",
                            args.max_failed_attempts
                        );
                        break;
                    }

                    if args.skip_failed {
                        warn!("--skip-failed specified, exiting with failed tasks remaining");
                        break;
                    } else {
                        warn!(
                            "Waiting for backoff to expire. Use --skip-failed to exit immediately, or tasks will be abandoned after {} attempts.",
                            args.max_failed_attempts
                        );
                        // Wait longer for backoff scenarios to avoid tight loop
                        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                    }
                } else {
                    error!("Inconsistent state: remaining tasks but no tasks found in outbox");
                    break;
                }
            }
        }
    }

    info!("Notion sync process completed successfully");
    Ok(())
}
