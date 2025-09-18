use tg_watchbot::db;

async fn setup_pool() -> sqlx::SqlitePool {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    pool
}

#[tokio::test]
async fn test_sequence_generation_with_batch() {
    let pool = setup_pool().await;

    // Create user and batch
    let user_id = db::get_or_create_user(&pool, 123, Some("test"), Some("Test User"))
        .await
        .unwrap();
    let batch_id = db::open_batch(&pool, user_id).await.unwrap();

    // Insert resources in batch - should get sequence 1, 2, 3
    let r1 = db::insert_resource(&pool, user_id, Some(batch_id), "text", "first", 100)
        .await
        .unwrap();
    let r2 = db::insert_resource(&pool, user_id, Some(batch_id), "text", "second", 200)
        .await
        .unwrap();
    let r3 = db::insert_resource(&pool, user_id, Some(batch_id), "text", "third", 300)
        .await
        .unwrap();

    // Check sequences
    let sequences: Vec<(i64, Option<i64>, i64)> = sqlx::query_as(
        "SELECT id, batch_id, sequence FROM resources WHERE id IN (?, ?, ?) ORDER BY sequence"
    )
    .bind(r1)
    .bind(r2)
    .bind(r3)
    .fetch_all(&pool)
    .await
    .unwrap();

    println!("Batch resources: {:?}", sequences);

    assert_eq!(sequences.len(), 3);
    assert_eq!(sequences[0].2, 1); // First resource: sequence 1
    assert_eq!(sequences[1].2, 2); // Second resource: sequence 2
    assert_eq!(sequences[2].2, 3); // Third resource: sequence 3

    // All should have the same batch_id
    assert_eq!(sequences[0].1, Some(batch_id));
    assert_eq!(sequences[1].1, Some(batch_id));
    assert_eq!(sequences[2].1, Some(batch_id));
}

#[tokio::test]
async fn test_sequence_generation_without_batch() {
    let pool = setup_pool().await;

    // Create user
    let user_id = db::get_or_create_user(&pool, 456, Some("nobatch"), Some("No Batch User"))
        .await
        .unwrap();

    // Insert resources without batch - should all get sequence 1
    let r1 = db::insert_resource(&pool, user_id, None, "text", "standalone1", 400)
        .await
        .unwrap();
    let r2 = db::insert_resource(&pool, user_id, None, "text", "standalone2", 500)
        .await
        .unwrap();
    let r3 = db::insert_resource(&pool, user_id, None, "text", "standalone3", 600)
        .await
        .unwrap();

    // Check sequences
    let sequences: Vec<(i64, Option<i64>, i64)> = sqlx::query_as(
        "SELECT id, batch_id, sequence FROM resources WHERE id IN (?, ?, ?) ORDER BY id"
    )
    .bind(r1)
    .bind(r2)
    .bind(r3)
    .fetch_all(&pool)
    .await
    .unwrap();

    println!("Standalone resources: {:?}", sequences);

    assert_eq!(sequences.len(), 3);
    assert_eq!(sequences[0].2, 1); // All should have sequence 1
    assert_eq!(sequences[1].2, 1);
    assert_eq!(sequences[2].2, 1);

    // All should have no batch_id
    assert_eq!(sequences[0].1, None);
    assert_eq!(sequences[1].1, None);
    assert_eq!(sequences[2].1, None);
}

#[tokio::test]
async fn test_sequence_generation_multiple_batches() {
    let pool = setup_pool().await;

    // Create user
    let user_id = db::get_or_create_user(&pool, 789, Some("multi"), Some("Multi Batch User"))
        .await
        .unwrap();

    // Create first batch
    let batch1_id = db::open_batch(&pool, user_id).await.unwrap();
    let r1 = db::insert_resource(&pool, user_id, Some(batch1_id), "text", "batch1_item1", 700)
        .await
        .unwrap();
    let r2 = db::insert_resource(&pool, user_id, Some(batch1_id), "text", "batch1_item2", 800)
        .await
        .unwrap();

    // Commit first batch before opening second
    db::commit_batch(&pool, user_id, Some("Batch 1")).await.unwrap();

    // Create second batch
    let batch2_id = db::open_batch(&pool, user_id).await.unwrap();
    let r3 = db::insert_resource(&pool, user_id, Some(batch2_id), "text", "batch2_item1", 900)
        .await
        .unwrap();
    let r4 = db::insert_resource(&pool, user_id, Some(batch2_id), "text", "batch2_item2", 1000)
        .await
        .unwrap();

    // Check sequences
    let batch1_sequences: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT id, sequence FROM resources WHERE batch_id = ? ORDER BY sequence"
    )
    .bind(batch1_id)
    .fetch_all(&pool)
    .await
    .unwrap();

    let batch2_sequences: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT id, sequence FROM resources WHERE batch_id = ? ORDER BY sequence"
    )
    .bind(batch2_id)
    .fetch_all(&pool)
    .await
    .unwrap();

    println!("Batch 1 sequences: {:?}", batch1_sequences);
    println!("Batch 2 sequences: {:?}", batch2_sequences);

    // Batch 1 should have sequences 1, 2
    assert_eq!(batch1_sequences.len(), 2);
    assert_eq!(batch1_sequences[0].1, 1);
    assert_eq!(batch1_sequences[1].1, 2);

    // Batch 2 should have sequences 1, 2 (starting fresh)
    assert_eq!(batch2_sequences.len(), 2);
    assert_eq!(batch2_sequences[0].1, 1);
    assert_eq!(batch2_sequences[1].1, 2);
}