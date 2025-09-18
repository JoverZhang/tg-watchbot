use anyhow::{anyhow, Result};
use std::collections::VecDeque;
use std::sync::Arc;
use tg_watchbot::config;
use tg_watchbot::db;
use tg_watchbot::notion::{NotionIds, NotionService};
use tg_watchbot::outbox;
use tokio::sync::Mutex;

async fn setup_pool() -> sqlx::SqlitePool {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    pool
}

#[derive(Clone, Default)]
struct RecordingNotion {
    responses: Arc<Mutex<VecDeque<Result<String>>>>,
    main_calls: Arc<Mutex<Vec<String>>>,
    resource_calls: Arc<Mutex<Vec<(Option<String>, i64, Option<String>)>>>,
}

impl RecordingNotion {
    fn with_responses(responses: Vec<Result<String>>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::from(responses))),
            ..Default::default()
        }
    }

    async fn pop_response(&self) -> Result<String> {
        let mut guard = self.responses.lock().await;
        guard.pop_front().unwrap_or_else(|| Ok("page-id".into()))
    }

    async fn main_calls(&self) -> Vec<String> {
        self.main_calls.lock().await.clone()
    }

    async fn resource_calls(&self) -> Vec<(Option<String>, i64, Option<String>)> {
        self.resource_calls.lock().await.clone()
    }
}

#[async_trait::async_trait]
impl NotionService for RecordingNotion {
    async fn create_main_page(&self, _ids: &NotionIds, title: &str) -> Result<String> {
        self.main_calls.lock().await.push(title.to_string());
        self.pop_response().await
    }

    async fn create_resource_page(
        &self,
        _ids: &NotionIds,
        parent_main_page_id: Option<&str>,
        order: i64,
        text: Option<&str>,
        _media_name: Option<&str>,
        _media_url: Option<&str>,
    ) -> Result<String> {
        self.resource_calls.lock().await.push((
            parent_main_page_id.map(str::to_string),
            order,
            text.map(str::to_string),
        ));
        self.pop_response().await
    }
}

#[tokio::test]
async fn test_syncer_logic_with_real_tasks() {
    let pool = setup_pool().await;
    let cfg = config::load(Some(std::path::Path::new("config.yaml"))).unwrap();
    let notion_ids = cfg.notion_ids();

    // Create a mock notion service that succeeds
    let notion = RecordingNotion::with_responses(vec![
        Ok("main-1".into()),
        Ok("res-1".into()),
        Ok("res-2".into()),
    ]);

    // Create test data
    let user_id = db::get_or_create_user(&pool, 99, Some("test"), Some("Test"))
        .await
        .unwrap();
    let batch_id = db::open_batch(&pool, user_id).await.unwrap();

    let _r1 = db::insert_resource(&pool, user_id, Some(batch_id), "text", "note1", 10)
        .await
        .unwrap();
    let _r2 = db::insert_resource(&pool, user_id, Some(batch_id), "text", "note2", 11)
        .await
        .unwrap();

    db::commit_batch(&pool, user_id, Some("Test Batch"))
        .await
        .unwrap();

    // Check initial outbox state
    let initial_count = db::count_remaining_outbox_tasks(&pool).await.unwrap();
    println!("Initial outbox tasks: {}", initial_count);
    assert!(initial_count > 0, "Should have outbox tasks to process");

    // Check initial sync state
    let initial_sync_id = db::get_last_processed_outbox_id(&pool).await.unwrap();
    println!("Initial last_processed_id: {}", initial_sync_id);
    assert_eq!(initial_sync_id, 0, "Initial sync ID should be 0");

    // Get the actual outbox IDs before processing
    let outbox_tasks: Vec<(i64, String, i64)> =
        sqlx::query_as("SELECT id, kind, ref_id FROM outbox ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    println!("Outbox tasks before processing: {:?}", outbox_tasks);

    let max_backoff = 60i64;
    let mut processed_count = 0;

    // Process tasks one by one (simulating syncer logic)
    loop {
        let processed = outbox::process_next_task(&pool, &notion, &notion_ids, max_backoff)
            .await
            .unwrap();

        if !processed {
            break;
        }

        processed_count += 1;
        println!("Processed task #{}", processed_count);

        // Check remaining tasks after each processing
        let remaining = db::count_remaining_outbox_tasks(&pool).await.unwrap();
        println!("Remaining tasks: {}", remaining);
    }

    println!("Total processed: {}", processed_count);

    // Verify all tasks were processed
    let final_count = db::count_remaining_outbox_tasks(&pool).await.unwrap();
    assert_eq!(final_count, 0, "All tasks should be processed");

    // Check notion calls
    let main_calls = notion.main_calls().await;
    let resource_calls = notion.resource_calls().await;
    println!("Main calls: {}", main_calls.len());
    println!("Resource calls: {}", resource_calls.len());

    assert_eq!(main_calls.len(), 1);
    assert_eq!(resource_calls.len(), 2);

    // The issue: how should we properly track last_processed_id?
    // Current syncer logic is flawed because deleted tasks can't be tracked this way
}

#[tokio::test]
async fn test_syncer_tracks_processed_ids_correctly() {
    let pool = setup_pool().await;
    let cfg = config::load(Some(std::path::Path::new("config.yaml"))).unwrap();
    let notion_ids = cfg.notion_ids();
    let max_backoff = 60i64;

    // Create a mock notion service that succeeds
    let notion = RecordingNotion::with_responses(vec![
        Ok("main-1".into()),
        Ok("res-1".into()),
        Ok("res-2".into()),
    ]);

    // Create test data
    let user_id = db::get_or_create_user(&pool, 99, Some("test"), Some("Test"))
        .await
        .unwrap();
    let batch_id = db::open_batch(&pool, user_id).await.unwrap();

    let _r1 = db::insert_resource(&pool, user_id, Some(batch_id), "text", "note1", 10)
        .await
        .unwrap();
    let _r2 = db::insert_resource(&pool, user_id, Some(batch_id), "text", "note2", 11)
        .await
        .unwrap();

    db::commit_batch(&pool, user_id, Some("Test Batch"))
        .await
        .unwrap();

    // Get the actual outbox IDs before processing
    let outbox_tasks: Vec<(i64, String, i64)> =
        sqlx::query_as("SELECT id, kind, ref_id FROM outbox ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    println!("Outbox tasks: {:?}", outbox_tasks);

    let expected_task_ids: Vec<i64> = outbox_tasks.iter().map(|(id, _, _)| *id).collect();

    // Process tasks one by one using the corrected syncer logic
    let mut processed_count = 0;
    let mut last_processed_id = 0i64;

    loop {
        // Get the next task to process BEFORE processing it (corrected syncer logic)
        if let Some((next_task_id, _, _, _, _)) = db::next_due_outbox(&pool).await.unwrap() {
            println!("About to process task ID: {}", next_task_id);

            let processed = outbox::process_next_task(&pool, &notion, &notion_ids, max_backoff)
                .await
                .unwrap();

            if processed {
                processed_count += 1;
                // Update the last processed ID to the task we just completed
                last_processed_id = next_task_id;
                db::update_last_processed_outbox_id(&pool, last_processed_id)
                    .await
                    .unwrap();

                println!(
                    "Processed task ID: {}, last_processed_id now: {}",
                    next_task_id, last_processed_id
                );
            } else {
                panic!("Task should have been processed successfully");
            }
        } else {
            break;
        }
    }

    println!("Final processed count: {}", processed_count);
    println!("Expected task IDs: {:?}", expected_task_ids);
    println!("Final last_processed_id: {}", last_processed_id);

    // Verify all tasks were processed
    assert_eq!(processed_count, expected_task_ids.len());

    // Verify the last processed ID matches the highest task ID
    assert_eq!(last_processed_id, *expected_task_ids.last().unwrap());

    // Verify sync state is properly stored
    let stored_last_id = db::get_last_processed_outbox_id(&pool).await.unwrap();
    assert_eq!(stored_last_id, last_processed_id);

    // Verify no tasks remain
    let remaining = db::count_remaining_outbox_tasks(&pool).await.unwrap();
    assert_eq!(remaining, 0);
}

#[tokio::test]
async fn test_syncer_respects_failure_threshold() {
    let pool = setup_pool().await;
    let cfg = config::load(Some(std::path::Path::new("config.yaml"))).unwrap();
    let notion_ids = cfg.notion_ids();
    let max_backoff = 60i64;

    // Create a mock notion service that always fails
    let notion = RecordingNotion::with_responses(vec![
        Err(anyhow!("persistent failure")),
        Err(anyhow!("persistent failure")),
        Err(anyhow!("persistent failure")),
        Err(anyhow!("persistent failure")),
        Err(anyhow!("persistent failure")),
        Err(anyhow!("persistent failure")), // This should exceed the threshold
    ]);

    // Create test data
    let user_id = db::get_or_create_user(&pool, 99, Some("test"), Some("Test"))
        .await
        .unwrap();

    // Create a single resource (no batch for simplicity)
    let _resource_id = db::insert_resource(&pool, user_id, None, "text", "test", 1)
        .await
        .unwrap();

    let failure_threshold = 3; // Set a low threshold for testing

    // Process tasks until failure threshold is exceeded
    let mut attempts = 0;
    loop {
        if let Some((task_id, _, _, _, _)) = db::next_due_outbox(&pool).await.unwrap() {
            println!("Processing task {} (attempt {})", task_id, attempts + 1);

            let processed = outbox::process_next_task(&pool, &notion, &notion_ids, max_backoff)
                .await
                .unwrap();

            if processed {
                // Task was "processed" (attempted), but may have failed
                // Check if the task is still in outbox (indicates failure and backoff)
                let task_exists: Option<i32> =
                    sqlx::query_scalar("SELECT attempt FROM outbox WHERE id = ?")
                        .bind(task_id)
                        .fetch_optional(&pool)
                        .await
                        .unwrap();

                if let Some(current_attempts) = task_exists {
                    println!("Task {} failed, attempts: {}", task_id, current_attempts);

                    if current_attempts >= failure_threshold {
                        println!(
                            "Failure threshold ({}) exceeded, stopping",
                            failure_threshold
                        );
                        break;
                    }

                    // Reset due_at to now so we can retry immediately in test
                    sqlx::query(
                        "UPDATE outbox SET due_at = datetime('now', '-1 seconds') WHERE id = ?",
                    )
                    .bind(task_id)
                    .execute(&pool)
                    .await
                    .unwrap();
                } else {
                    println!(
                        "Task {} completed successfully and was removed from outbox",
                        task_id
                    );
                    break; // Task succeeded, no need to continue
                }
            } else {
                println!("Task {} was not processed (likely not due yet)", task_id);
                break; // No more tasks ready to process
            }
        } else {
            break;
        }

        attempts += 1;
        if attempts > 10 {
            panic!("Too many attempts, test should have exited by now");
        }
    }

    // Verify the task has exceeded the failure threshold
    let final_attempts: i32 = sqlx::query_scalar("SELECT MAX(attempt) FROM outbox")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert!(
        final_attempts >= failure_threshold,
        "Task should have exceeded failure threshold. Expected >= {}, got {}",
        failure_threshold,
        final_attempts
    );

    println!(
        "Test completed successfully. Final attempts: {}",
        final_attempts
    );
}
