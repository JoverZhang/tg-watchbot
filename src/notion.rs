use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::fmt;
use tracing::debug;

const NOTION_API_BASE: &str = "https://api.notion.com/";

#[derive(Clone)]
pub struct NotionClient {
    http: Client,
    base_url: Url,
    token: String,
    version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotionIds {
    pub main_db: String,
    pub resource_db: String,
    pub f_main_title: String,
    pub f_rel_parent: String,
    pub f_res_order: String,
    pub f_res_text: String,
    pub f_res_media: String,
}

impl fmt::Debug for NotionClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NotionClient")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

#[async_trait]
pub trait NotionService: Send + Sync {
    async fn create_main_page(&self, ids: &NotionIds, title: &str) -> Result<String>;

    async fn create_resource_page(
        &self,
        ids: &NotionIds,
        parent_main_page_id: Option<&str>,
        order: i64,
        text: Option<&str>,
        media_name: Option<&str>,
        media_url: Option<&str>,
    ) -> Result<String>;
}

impl NotionClient {
    pub fn new(token: String, version: String) -> Self {
        let base_url = Url::parse(NOTION_API_BASE).expect("valid default Notion URL");
        Self::with_base_url(token, version, base_url)
    }

    pub fn with_base_url(token: String, version: String, base_url: Url) -> Self {
        let http = Client::builder()
            .user_agent("tg-watchbot/0.1")
            .no_proxy()
            .build()
            .expect("reqwest client");
        Self {
            http,
            base_url,
            token,
            version,
        }
    }

    pub fn build_request(&self, body: &Value) -> Result<reqwest::Request> {
        let endpoint = self
            .base_url
            .join("v1/pages")
            .context("invalid Notion base URL")?;
        self.http
            .post(endpoint)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Notion-Version", &self.version)
            .header("Content-Type", "application/json")
            .json(body)
            .build()
            .context("failed to build Notion request")
    }

    async fn execute_create(&self, body: Value) -> Result<String> {
        let request = self.build_request(&body)?;
        debug!(url=%request.url(), payload=%body, "sending notion request");
        let res = self
            .http
            .execute(request)
            .await
            .context("failed to reach Notion")?;

        if res.status() == StatusCode::TOO_MANY_REQUESTS {
            let body = res.text().await.unwrap_or_default();
            return Err(anyhow!("received 429 from Notion: {}", body));
        }
        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            return Err(anyhow!("notion error {}: {}", status, body));
        }

        let payload: CreatePageResponse = res.json().await.context("invalid Notion response")?;
        Ok(payload.id)
    }

    pub async fn create_main_page(&self, ids: &NotionIds, title: &str) -> Result<String> {
        let body = build_main_page_request(ids, title);
        self.execute_create(body).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_resource_page(
        &self,
        ids: &NotionIds,
        parent_main_page_id: Option<&str>,
        order: i64,
        text: Option<&str>,
        media_name: Option<&str>,
        media_url: Option<&str>,
    ) -> Result<String> {
        let body = build_resource_page_request(
            ids,
            parent_main_page_id,
            order,
            text,
            media_name,
            media_url,
        );
        self.execute_create(body).await
    }
}

#[async_trait]
impl NotionService for NotionClient {
    async fn create_main_page(&self, ids: &NotionIds, title: &str) -> Result<String> {
        NotionClient::create_main_page(self, ids, title).await
    }

    async fn create_resource_page(
        &self,
        ids: &NotionIds,
        parent_main_page_id: Option<&str>,
        order: i64,
        text: Option<&str>,
        media_name: Option<&str>,
        media_url: Option<&str>,
    ) -> Result<String> {
        NotionClient::create_resource_page(
            self,
            ids,
            parent_main_page_id,
            order,
            text,
            media_name,
            media_url,
        )
        .await
    }
}

pub fn build_main_page_request(ids: &NotionIds, title: &str) -> Value {
    let mut properties = Map::new();
    properties.insert(
        ids.f_main_title.clone(),
        json!({
            "title": [
                {
                    "text": {
                        "content": title,
                    }
                }
            ]
        }),
    );

    json!({
        "parent": { "database_id": ids.main_db },
        "properties": Value::Object(properties),
    })
}

pub fn build_resource_page_request(
    ids: &NotionIds,
    parent_main_page_id: Option<&str>,
    order: i64,
    text: Option<&str>,
    media_name: Option<&str>,
    media_url: Option<&str>,
) -> Value {
    let mut properties = Map::new();
    if let Some(parent_id) = parent_main_page_id {
        properties.insert(
            ids.f_rel_parent.clone(),
            json!({ "relation": [{ "id": parent_id }] }),
        );
    }

    properties.insert(
        ids.f_res_order.clone(),
        json!({
            "number": order,
        }),
    );

    if let Some(text_content) = text.filter(|t| !t.is_empty()) {
        properties.insert(
            ids.f_res_text.clone(),
            json!({
                "rich_text": [
                    {
                        "text": {
                            "content": text_content,
                        }
                    }
                ]
            }),
        );
    }

    if let Some(url) = media_url.filter(|url| !url.is_empty()) {
        let name = media_name.filter(|name| !name.is_empty()).unwrap_or(url);
        properties.insert(
            ids.f_res_media.clone(),
            json!({
                "files": [
                    {
                        "name": name,
                        "type": "external",
                        "external": { "url": url }
                    }
                ]
            }),
        );
    }

    json!({
        "parent": { "database_id": ids.resource_db },
        "properties": Value::Object(properties),
    })
}

#[derive(Deserialize)]
struct CreatePageResponse {
    id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_ids() -> NotionIds {
        NotionIds {
            main_db: "main-db".into(),
            resource_db: "resource-db".into(),
            f_main_title: "main-title".into(),
            f_rel_parent: "rel-parent".into(),
            f_res_order: "res-order".into(),
            f_res_text: "res-text".into(),
            f_res_media: "res-media".into(),
        }
    }

    #[test]
    fn build_main_page_request_includes_title() {
        let ids = sample_ids();
        let body = build_main_page_request(&ids, "hello");
        assert_eq!(body["parent"]["database_id"], "main-db");
        assert_eq!(
            body["properties"]["main-title"]["title"][0]["text"]["content"],
            "hello"
        );
    }

    #[test]
    fn build_resource_page_request_handles_all_fields() {
        let ids = sample_ids();
        let body = build_resource_page_request(
            &ids,
            Some("parent-1"),
            3,
            Some("details"),
            Some("a.jpg"),
            Some("https://cdn/a.jpg"),
        );

        assert_eq!(body["parent"]["database_id"], "resource-db");
        assert_eq!(
            body["properties"]["rel-parent"]["relation"][0]["id"],
            "parent-1"
        );
        assert_eq!(body["properties"]["res-order"]["number"], 3);
        assert_eq!(
            body["properties"]["res-text"]["rich_text"][0]["text"]["content"],
            "details"
        );
        assert_eq!(
            body["properties"]["res-media"]["files"][0]["external"]["url"],
            "https://cdn/a.jpg"
        );
    }

    #[test]
    fn build_resource_page_request_omits_optional_fields() {
        let ids = sample_ids();
        let body = build_resource_page_request(&ids, None, 7, None, None, None);
        assert_eq!(body["properties"]["res-order"]["number"], 7);
        assert!(body["properties"].get("rel-parent").is_none());
        assert!(body["properties"].get("res-text").is_none());
        assert!(body["properties"].get("res-media").is_none());
    }

    #[test]
    fn build_request_sets_headers() {
        let client = NotionClient::new("token".into(), "2022-06-28".into());
        let body = json!({ "sample": true });
        let request = client.build_request(&body).unwrap();
        assert_eq!(request.method(), reqwest::Method::POST);
        assert_eq!(request.url().path(), "/v1/pages");
        let headers = request.headers();
        assert_eq!(
            headers
                .get("Authorization")
                .and_then(|h| h.to_str().ok())
                .unwrap(),
            "Bearer token"
        );
        assert_eq!(
            headers
                .get("Notion-Version")
                .and_then(|h| h.to_str().ok())
                .unwrap(),
            "2022-06-28"
        );
        assert_eq!(
            headers
                .get("Content-Type")
                .and_then(|h| h.to_str().ok())
                .unwrap(),
            "application/json"
        );
    }
}
