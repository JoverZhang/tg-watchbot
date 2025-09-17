use tg_watchbot as _; // ensure crate builds

use std::time::Duration;
use tg_watchbot::db;
use tg_watchbot::notion::MockNotionClient;
use tg_watchbot::outbox::process_next_task;

#[tokio::test]
async fn begin_commit_flow_enqueues_and_pushes() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();

    let user_id = db::get_or_create_user(&pool, 999, Some("tester"), Some("Test User"))
        .await
        .unwrap();

    // Begin a batch and add two resources in-batch
    let batch_id = db::open_batch(&pool, user_id).await.unwrap();
    let r1 = db::insert_resource(&pool, user_id, Some(batch_id), "text", "A", 1)
        .await
        .unwrap();
    let r2 = db::insert_resource(&pool, user_id, Some(batch_id), "text", "B", 2)
        .await
        .unwrap();
    // Add a standalone resource which is immediately enqueued
    let r3 = db::insert_resource(&pool, user_id, None, "text", "C", 3).await.unwrap();

    // Commit the batch; should enqueue three tasks: 1 batch + 2 resources
    db::commit_batch(&pool, user_id, Some("Title")).await.unwrap();

    let mut kinds = sqlx::query_scalar::<_, String>("SELECT kind FROM outbox ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    // Expect at least 4 items overall (3 from commit + 1 standalone)
    assert!(kinds.len() >= 4);

    let mock = MockNotionClient::default();
    // Process tasks until none left
    loop {
        if !process_next_task(&pool, &mock).await.unwrap() { break; }
    }
    // Outbox should be empty
    let cnt: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM outbox").fetch_one(&pool).await.unwrap();
    assert_eq!(cnt, 0);
}

#[tokio::test]
async fn rollback_discards_resources() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let user_id = db::get_or_create_user(&pool, 1000, Some("tester2"), Some("Test User 2")).await.unwrap();
    let batch_id = db::open_batch(&pool, user_id).await.unwrap();
    let _r1 = db::insert_resource(&pool, user_id, Some(batch_id), "text", "msg", 11).await.unwrap();
    db::rollback_batch(&pool, user_id).await.unwrap();
    // Ensure no outbox items were created
    let cnt: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM outbox").fetch_one(&pool).await.unwrap();
    assert_eq!(cnt, 0);
}
