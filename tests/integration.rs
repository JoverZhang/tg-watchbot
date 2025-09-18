use anyhow::{anyhow, Result};
use std::collections::VecDeque;
use std::sync::Arc;
use tg_watchbot::config;
use tg_watchbot::db;
use tg_watchbot::notion::{NotionIds, NotionService};
use tg_watchbot::outbox::process_next_task;
use tokio::sync::Mutex;
use tokio::time::Duration;

#[cfg(test)]
fn load_notion_ids() -> NotionIds {
    let cfg: config::Config = serde_yaml::from_str(config::example()).unwrap();
    cfg.notion_ids()
}

async fn setup_pool() -> sqlx::SqlitePool {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    pool
}

#[derive(Debug, Clone, Default)]
struct MainCall {
    title: String,
}

#[derive(Debug, Clone, Default)]
struct ResourceCall {
    parent: Option<String>,
    order: i64,
    text: Option<String>,
    media_name: Option<String>,
    media_url: Option<String>,
}

#[derive(Clone, Default)]
struct RecordingNotion {
    responses: Arc<Mutex<VecDeque<Result<String>>>>,
    main_calls: Arc<Mutex<Vec<MainCall>>>,
    resource_calls: Arc<Mutex<Vec<ResourceCall>>>,
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

    async fn main_calls(&self) -> Vec<MainCall> {
        self.main_calls.lock().await.clone()
    }

    async fn resource_calls(&self) -> Vec<ResourceCall> {
        self.resource_calls.lock().await.clone()
    }
}

#[async_trait::async_trait]
impl NotionService for RecordingNotion {
    async fn create_main_page(&self, _ids: &NotionIds, title: &str) -> Result<String> {
        self.main_calls.lock().await.push(MainCall {
            title: title.to_string(),
        });
        self.pop_response().await
    }

    async fn create_resource_page(
        &self,
        _ids: &NotionIds,
        parent_main_page_id: Option<&str>,
        order: i64,
        text: Option<&str>,
        media_name: Option<&str>,
        media_url: Option<&str>,
    ) -> Result<String> {
        self.resource_calls.lock().await.push(ResourceCall {
            parent: parent_main_page_id.map(str::to_string),
            order,
            text: text.map(str::to_string),
            media_name: media_name.map(str::to_string),
            media_url: media_url.map(str::to_string),
        });
        self.pop_response().await
    }
}

#[tokio::test]
async fn single_message_creates_resource_page() {
    let pool = setup_pool().await;
    let ids = load_notion_ids();
    let notion = RecordingNotion::with_responses(vec![Ok("resource-1".into())]);

    let user_id = db::get_or_create_user(&pool, 42, Some("tester"), Some("Tester"))
        .await
        .unwrap();
    let resource_id = db::insert_resource(&pool, user_id, None, "text", "hello world", 7)
        .await
        .unwrap();

    let processed = process_next_task(&pool, &notion, &ids, 60).await.unwrap();
    assert!(processed);

    let processed = process_next_task(&pool, &notion, &ids, 60).await.unwrap();
    assert!(!processed);

    let stored: Option<String> =
        sqlx::query_scalar("SELECT notion_page_id FROM resources WHERE id = ?")
            .bind(resource_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored.as_deref(), Some("resource-1"));

    let calls = notion.resource_calls().await;
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.parent, None);
    assert_eq!(call.order, 7);
    assert_eq!(call.text.as_deref(), Some("hello world"));
    assert!(call.media_url.is_none());
}

#[tokio::test]
async fn transactional_flow_creates_main_and_resources() {
    let pool = setup_pool().await;
    let ids = load_notion_ids();
    let notion = RecordingNotion::with_responses(vec![
        Ok("main-1".into()),
        Ok("res-1".into()),
        Ok("res-2".into()),
    ]);

    let user_id = db::get_or_create_user(&pool, 99, Some("transaction"), Some("Txn"))
        .await
        .unwrap();
    let batch_id = db::open_batch(&pool, user_id).await.unwrap();

    let r1 = db::insert_resource(&pool, user_id, Some(batch_id), "text", "note", 10)
        .await
        .unwrap();
    let r2 = db::insert_resource(&pool, user_id, Some(batch_id), "photo", "ignored", 11)
        .await
        .unwrap();
    sqlx::query("UPDATE resources SET media_name = ?, media_url = ? WHERE id = ?")
        .bind("a.jpg")
        .bind("https://cdn.example/a.jpg")
        .bind(r2)
        .execute(&pool)
        .await
        .unwrap();

    db::commit_batch(&pool, user_id, Some("Hello"))
        .await
        .unwrap();

    for _ in 0..10 {
        if process_next_task(&pool, &notion, &ids, 60).await.unwrap() {
            continue;
        }

        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM outbox")
            .fetch_one(&pool)
            .await
            .unwrap();
        if remaining == 0 {
            break;
        }

        sqlx::query("UPDATE outbox SET due_at = datetime('now', '-1 seconds')")
            .execute(&pool)
            .await
            .unwrap();
    }

    let main_calls = notion.main_calls().await;
    assert_eq!(main_calls.len(), 1);
    assert_eq!(main_calls[0].title, "Hello");

    let resource_calls = notion.resource_calls().await;
    assert_eq!(resource_calls.len(), 2);
    assert_eq!(resource_calls[0].parent.as_deref(), Some("main-1"));
    assert_eq!(resource_calls[0].order, 10);
    assert_eq!(resource_calls[0].text.as_deref(), Some("note"));
    assert!(resource_calls[0].media_url.is_none());

    assert_eq!(resource_calls[1].parent.as_deref(), Some("main-1"));
    assert_eq!(resource_calls[1].order, 11);
    assert!(resource_calls[1].text.is_none());
    assert_eq!(resource_calls[1].media_name.as_deref(), Some("a.jpg"));
    assert_eq!(
        resource_calls[1].media_url.as_deref(),
        Some("https://cdn.example/a.jpg")
    );

    let batch_page: Option<String> =
        sqlx::query_scalar("SELECT notion_page_id FROM batches WHERE id = ?")
            .bind(batch_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(batch_page.as_deref(), Some("main-1"));

    let res_pages: Vec<Option<String>> =
        sqlx::query_scalar("SELECT notion_page_id FROM resources WHERE id IN (?, ?) ORDER BY id")
            .bind(r1)
            .bind(r2)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(res_pages, vec![Some("res-1".into()), Some("res-2".into())]);
}

#[tokio::test]
async fn notion_retry_on_failure() {
    let pool = setup_pool().await;
    let ids = load_notion_ids();
    let notion = RecordingNotion::with_responses(vec![
        Err(anyhow!("temp failure")),
        Ok("resource-ok".into()),
    ]);

    let user_id = db::get_or_create_user(&pool, 55, Some("retry"), Some("Retry"))
        .await
        .unwrap();
    let resource_id = db::insert_resource(&pool, user_id, None, "text", "retry me", 21)
        .await
        .unwrap();

    let processed = process_next_task(&pool, &notion, &ids, 60).await.unwrap();
    assert!(processed);

    let attempt: i32 = sqlx::query_scalar("SELECT attempt FROM outbox LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(attempt, 1);

    sqlx::query("UPDATE outbox SET due_at = datetime('now', '-1 seconds')")
        .execute(&pool)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(10)).await;

    let processed = process_next_task(&pool, &notion, &ids, 60).await.unwrap();
    assert!(processed);

    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM outbox")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(remaining, 0);

    let stored: Option<String> =
        sqlx::query_scalar("SELECT notion_page_id FROM resources WHERE id = ?")
            .bind(resource_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored.as_deref(), Some("resource-ok"));

    let calls = notion.resource_calls().await;
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].text.as_deref(), Some("retry me"));
    assert_eq!(calls[1].text.as_deref(), Some("retry me"));
}
