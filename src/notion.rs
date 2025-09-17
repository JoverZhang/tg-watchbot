use crate::model::{BatchState, OutboxKind};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use reqwest::StatusCode;
use serde::Serialize;
use sqlx::{Row, SqlitePool};
use tracing::instrument;

#[async_trait]
pub trait NotionClient: Send + Sync {
    async fn push_batch(&self, pool: &SqlitePool, batch_id: i64) -> Result<String>;
    async fn push_resource(&self, pool: &SqlitePool, resource_id: i64) -> Result<String>;
}

pub struct RealNotionClient {
    http: reqwest::Client,
    notion_token: String,
    batches_db: String,
    resources_db: String,
}

impl RealNotionClient {
    pub fn from_env() -> Self {
        let http = reqwest::Client::builder()
            .user_agent("tg-watchbot/0.1")
            .build()
            .expect("reqwest client");
        let notion_token = std::env::var("NOTION_TOKEN").unwrap_or_default();
        let batches_db = std::env::var("NOTION_BATCHES_DB").unwrap_or_default();
        let resources_db = std::env::var("NOTION_RESOURCES_DB").unwrap_or_default();
        Self { http, notion_token, batches_db, resources_db }
    }

    async fn create_page(&self, parent_db: &str, properties: serde_json::Value) -> Result<String> {
        #[derive(Serialize)]
        struct Req<'a> {
            parent: Parent<'a>,
            properties: serde_json::Value,
        }
        #[derive(Serialize)]
        struct Parent<'a> { database_id: &'a str }

        if self.notion_token.is_empty() || parent_db.is_empty() {
            return Err(anyhow!("Notion configuration missing"));
        }
        let req = Req { parent: Parent { database_id: parent_db }, properties };
        let res = self.http
            .post("https://api.notion.com/v1/pages")
            .header("Authorization", format!("Bearer {}", self.notion_token))
            .header("Notion-Version", "2022-06-28")
            .json(&req)
            .send()
            .await?;
        if res.status() == StatusCode::TOO_MANY_REQUESTS {
            return Err(anyhow!("rate limited"));
        }
        if !res.status().is_success() {
            let body = res.text().await.unwrap_or_default();
            return Err(anyhow!("notion error: {}", body));
        }
        let v: serde_json::Value = res.json().await?;
        Ok(v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string())
    }
}

#[async_trait]
impl NotionClient for RealNotionClient {
    #[instrument(skip_all)]
    async fn push_batch(&self, pool: &SqlitePool, batch_id: i64) -> Result<String> {
        let row = sqlx::query("SELECT title FROM batches WHERE id = ?")
            .bind(batch_id)
            .fetch_one(pool)
            .await?;
        let title: Option<String> = row.try_get("title").ok();
        let title = title.unwrap_or_else(|| "Untitled Batch".to_string());
        let props = serde_json::json!({
            "Name": { "title": [{"text": {"content": title}}] }
        });
        let page_id = self.create_page(&self.batches_db, props).await?;
        sqlx::query("UPDATE batches SET notion_page_id = ? WHERE id = ?")
            .bind(&page_id)
            .bind(batch_id)
            .execute(pool)
            .await?;
        Ok(page_id)
    }

    #[instrument(skip_all)]
    async fn push_resource(&self, pool: &SqlitePool, resource_id: i64) -> Result<String> {
        let r = sqlx::query(
            "SELECT resources.kind, resources.content, resources.batch_id, batches.notion_page_id \
             FROM resources LEFT JOIN batches ON resources.batch_id = batches.id WHERE resources.id = ?",
        )
        .bind(resource_id)
        .fetch_one(pool)
        .await?;
        let kind: String = r.get("kind");
        let content: String = r.get("content");
        let notion_page_id: Option<String> = r.try_get("notion_page_id").ok();
        let mut props = serde_json::json!({
            "Kind": { "select": {"name": kind}},
            "Content": { "rich_text": [{"text": {"content": content}}] }
        });
        if let Some(page_id) = notion_page_id {
            props["Batch"] = serde_json::json!({"relation": [{"id": page_id}]});
        }
        let page_id = self.create_page(&self.resources_db, props).await?;
        Ok(page_id)
    }
}

// A simple mock that records calls, used in tests.
pub struct MockNotionClient {
    pub pushed_batches: tokio::sync::Mutex<Vec<i64>>,
    pub pushed_resources: tokio::sync::Mutex<Vec<i64>>,
}

impl Default for MockNotionClient {
    fn default() -> Self { Self { pushed_batches: Default::default(), pushed_resources: Default::default() } }
}

#[async_trait]
impl NotionClient for MockNotionClient {
    async fn push_batch(&self, _pool: &SqlitePool, batch_id: i64) -> Result<String> {
        self.pushed_batches.lock().await.push(batch_id);
        Ok(format!("batch:{}", batch_id))
    }
    async fn push_resource(&self, _pool: &SqlitePool, resource_id: i64) -> Result<String> {
        self.pushed_resources.lock().await.push(resource_id);
        Ok(format!("res:{}", resource_id))
    }
}
