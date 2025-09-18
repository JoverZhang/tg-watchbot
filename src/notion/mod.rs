use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::any::Any;
use std::fmt;
use std::path::Path;
use tokio::fs;
use tracing::{info, warn};

use crate::config::Config;
use crate::notion::model::RetrieveDatabaseResp;

pub mod model;

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
pub trait NotionService: Send + Sync + Any {
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

    /// Resolve property IDs for the configured databases by fetching their
    /// schemas and mapping display names -> property IDs. Returns `NotionIds`
    /// whose `f_*` fields are property IDs (not display names).
    pub async fn resolve_property_ids(&self, cfg: &Config) -> Result<NotionIds> {
        let _main_db = self
            .retrieve_database(&cfg.notion.databases.main.id)
            .await
            .context("failed to retrieve main database schema")?;
        let _res_db = self
            .retrieve_database(&cfg.notion.databases.resource.id)
            .await
            .context("failed to retrieve resource database schema")?;

        let _lookup = |db: &RetrieveDatabaseResp, name: &str| -> Result<String> {
            db.properties
                .get(name)
                .map(|p| p.id.clone())
                .ok_or_else(|| {
                    anyhow!("property '{}' not found in Notion database {}", name, db.id)
                })
        };

        Ok(NotionIds {
            main_db: cfg.notion.databases.main.id.clone(),
            resource_db: cfg.notion.databases.resource.id.clone(),
            f_main_title: cfg.notion.databases.main.fields.title.clone(),
            f_rel_parent: cfg.notion.databases.resource.fields.relation.clone(),
            f_res_order: cfg.notion.databases.resource.fields.order.clone(),
            f_res_text: cfg.notion.databases.resource.fields.text.clone(),
            f_res_media: cfg.notion.databases.resource.fields.media.clone(),
        })
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
        info!(url=%request.url(), "=== NOTION API REQUEST ===");
        info!("Request Headers:");
        for (name, value) in request.headers() {
            if name.as_str().to_lowercase().contains("authorization") {
                info!("  {}: Bearer [REDACTED]", name);
            } else {
                info!("  {}: {}", name, value.to_str().unwrap_or("[invalid]"));
            }
        }
        info!(
            "Request Payload: {}",
            serde_json::to_string_pretty(&body).unwrap_or_else(|_| format!("{:?}", body))
        );

        let res = self
            .http
            .execute(request)
            .await
            .context("failed to reach Notion")?;

        info!("=== NOTION API RESPONSE ===");
        info!("Response Status: {}", res.status());
        info!("Response Headers:");
        for (name, value) in res.headers() {
            info!("  {}: {}", name, value.to_str().unwrap_or("[invalid]"));
        }

        if res.status() == StatusCode::TOO_MANY_REQUESTS {
            let body = res.text().await.unwrap_or_default();
            warn!("Rate limited by Notion: {}", body);
            return Err(anyhow!("received 429 from Notion: {}", body));
        }
        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            warn!("Notion API error - Status: {}, Body: {}", status, body);
            return Err(anyhow!("notion error {}: {}", status, body));
        }

        let response_body = res.text().await.context("failed to read Notion response")?;
        info!("Response Body: {}", response_body);

        let payload: CreatePageResponse =
            serde_json::from_str(&response_body).context("invalid Notion response JSON")?;
        info!("Successfully created Notion page with ID: {}", payload.id);
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
            None,
        );
        self.execute_create(body).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_resource_page_with_file_upload(
        &self,
        ids: &NotionIds,
        parent_main_page_id: Option<&str>,
        order: i64,
        text: Option<&str>,
        media_name: Option<&str>,
        file_upload_id: Option<&str>,
    ) -> Result<String> {
        let body = build_resource_page_request(
            ids,
            parent_main_page_id,
            order,
            text,
            media_name,
            None,
            file_upload_id,
        );
        self.execute_create(body).await
    }

    /// Create a resource page with multiple uploaded files (e.g., thumbnail + video)
    pub async fn create_resource_page_with_file_uploads(
        &self,
        ids: &NotionIds,
        parent_main_page_id: Option<&str>,
        order: i64,
        text: Option<&str>,
        files: &[(String, String)], // (name, file_upload_id)
    ) -> Result<String> {
        let body =
            build_resource_page_request_with_uploads(ids, parent_main_page_id, order, text, files);
        self.execute_create(body).await
    }

    pub async fn retrieve_database(
        &self,
        database_id: &str,
    ) -> anyhow::Result<RetrieveDatabaseResp> {
        let url = self
            .base_url
            .join(&format!("v1/databases/{}", database_id))?;
        let res = self
            .http
            .get(url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Notion-Version", &self.version)
            .send()
            .await?;
        if !res.status().is_success() {
            return Err(anyhow::anyhow!(
                "notion retrieve db error {}: {}",
                res.status(),
                res.text().await.unwrap_or_default()
            ));
        }
        Ok(res.json::<RetrieveDatabaseResp>().await?)
    }

    /// Upload a file to Notion using the 3-step process and return the file URL
    pub async fn upload_file<P: AsRef<Path>>(&self, file_path: P) -> Result<String> {
        let file_path = file_path.as_ref();
        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow!("invalid file name"))?;

        // Read file content
        let file_content = fs::read(file_path)
            .await
            .with_context(|| format!("failed to read file: {}", file_path.display()))?;

        // Step 1: Create file upload object
        let create_upload_url = self.base_url.join("v1/file_uploads")?;
        let create_body = json!({
            "name": file_name,
            "content_type": self.get_content_type(file_path),
            "mode": "single_part"
        });

        let create_res = self
            .http
            .post(create_upload_url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Notion-Version", &self.version)
            .header("Content-Type", "application/json")
            .json(&create_body)
            .send()
            .await
            .context("failed to create file upload")?;

        if !create_res.status().is_success() {
            let status = create_res.status();
            let body = create_res.text().await.unwrap_or_default();
            return Err(anyhow!("create file upload failed {}: {}", status, body));
        }

        let create_response: CreateFileUploadResponse = create_res
            .json()
            .await
            .context("failed to parse create upload response")?;

        // Step 2: Send file content
        let content_type = self.get_content_type(file_path);
        let form = reqwest::multipart::Form::new().part(
            "file",
            reqwest::multipart::Part::bytes(file_content)
                .file_name(file_name.to_string())
                .mime_str(content_type)?,
        );

        let send_res = self
            .http
            .post(&create_response.upload_url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Notion-Version", &self.version)
            .multipart(form)
            .send()
            .await
            .context("failed to send file content")?;

        if !send_res.status().is_success() {
            let status = send_res.status();
            let body = send_res.text().await.unwrap_or_default();
            return Err(anyhow!("send file failed {}: {}", status, body));
        }

        // The file is now uploaded and ready to be used
        // Return the file upload ID which can be referenced in page properties
        info!(
            "Successfully uploaded file: {} with ID: {}",
            file_name, create_response.id
        );
        Ok(create_response.id)
    }

    fn get_content_type(&self, file_path: &Path) -> &'static str {
        match file_path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|s| s.to_ascii_lowercase())
        {
            Some(ext) if ext == "jpg" || ext == "jpeg" => "image/jpeg",
            Some(ext) if ext == "png" => "image/png",
            Some(ext) if ext == "gif" => "image/gif",
            Some(ext) if ext == "mp4" => "video/mp4",
            Some(ext) if ext == "mov" => "video/quicktime",
            Some(ext) if ext == "avi" => "video/x-msvideo",
            _ => "application/octet-stream",
        }
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
    file_upload_id: Option<&str>,
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
            "title": [
                {
                    "text": {
                        "content": format!("#{}", order),
                    }
                }
            ]
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

    // Handle file uploads (either external URL or uploaded file ID)
    if let Some(upload_id) = file_upload_id.filter(|id| !id.is_empty()) {
        let name = media_name
            .filter(|name| !name.is_empty())
            .unwrap_or("Uploaded file");
        properties.insert(
            ids.f_res_media.clone(),
            json!({
                "files": [
                    {
                        "name": name,
                        "type": "file_upload",
                        "file_upload": { "id": upload_id }
                    }
                ]
            }),
        );
    } else if let Some(url) = media_url.filter(|url| !url.is_empty()) {
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

/// Build a resource page request that includes multiple uploaded files under the media property.
pub fn build_resource_page_request_with_uploads(
    ids: &NotionIds,
    parent_main_page_id: Option<&str>,
    order: i64,
    text: Option<&str>,
    files: &[(String, String)], // (name, file_upload_id)
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
            "title": [ { "text": { "content": format!("#{}", order) } } ]
        }),
    );

    if let Some(text_content) = text.filter(|t| !t.is_empty()) {
        properties.insert(
            ids.f_res_text.clone(),
            json!({
                "rich_text": [ { "text": { "content": text_content } } ]
            }),
        );
    }

    if !files.is_empty() {
        let files_json: Vec<Value> = files
            .iter()
            .map(|(name, id)| {
                json!({
                    "name": if name.is_empty() { "Uploaded file" } else { name },
                    "type": "file_upload",
                    "file_upload": { "id": id }
                })
            })
            .collect();
        properties.insert(ids.f_res_media.clone(), json!({ "files": files_json }));
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

#[derive(Deserialize)]
struct CreateFileUploadResponse {
    id: String,
    upload_url: String,
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
            None,
        );

        assert_eq!(body["parent"]["database_id"], "resource-db");
        assert_eq!(
            body["properties"]["rel-parent"]["relation"][0]["id"],
            "parent-1"
        );
        assert_eq!(
            body["properties"]["res-order"]["title"][0]["text"]["content"],
            "#3"
        );
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
        let body = build_resource_page_request(&ids, None, 7, None, None, None, None);
        assert_eq!(
            body["properties"]["res-order"]["title"][0]["text"]["content"],
            "#7"
        );
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
