use anyhow::{anyhow, Result};
use std::path::Path;

use tg_watchbot::config::{self, Config};
use tg_watchbot::notion::{NotionClient, NotionIds};

#[tokio::test]
#[ignore]
async fn notion_it_creates_main_and_resources() -> Result<()> {
    // Load local config (exact schema as example.config.yaml)
    let cfg = config::load(Some(Path::new("./config.yaml")))?;

    // Resolve property IDs from Notion and build a façade
    let client = NotionClient::new(cfg.notion.token.clone(), cfg.notion.version.clone());
    let notion = NotionFacade::new(client, &cfg).await?;

    let title = format!(
        "tg-watchbot IT {}",
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")
    );

    // Create a main page
    let main_page_id = notion.create_main_page(&title).await?;
    assert!(!main_page_id.trim().is_empty());
    println!(
        "Created main page: https://www.notion.so/{}",
        main_page_id.replace('-', "")
    );

    // Create a text resource
    let text_page = notion
        .create_resource_text(Some(&main_page_id), 1, "hello from integration test")
        .await?;
    assert!(!text_page.trim().is_empty());
    println!(
        "Created text page: https://www.notion.so/{}",
        text_page.replace('-', "")
    );

    // Create a photo resource (upload local file)
    let photo_page = notion
        .create_resource_media_from_file(Some(&main_page_id), 2, "tests/media/test_picture.jpg")
        .await?;
    assert!(!photo_page.trim().is_empty());
    println!(
        "Created photo page: https://www.notion.so/{}",
        photo_page.replace('-', "")
    );

    // Create a video resource (upload local file)
    let video_page = notion
        .create_resource_media_from_file(Some(&main_page_id), 3, "tests/media/video.mp4")
        .await?;
    assert!(!video_page.trim().is_empty());
    println!(
        "Created video page: https://www.notion.so/{}",
        video_page.replace('-', "")
    );

    Ok(())
}

/// Thin façade that binds a `NotionClient` to resolved property IDs. It exposes
/// small convenience helpers that align with repository needs and the
/// integration test.
#[derive(Clone)]
pub struct NotionFacade {
    client: NotionClient,
    ids: NotionIds,
}

impl NotionFacade {
    /// Construct a façade by resolving property IDs for the given config.
    pub async fn new(client: NotionClient, cfg: &Config) -> Result<Self> {
        let ids = client.resolve_property_ids(cfg).await?;
        Ok(Self { client, ids })
    }

    /// Return the resolved IDs (database and property IDs).
    pub fn ids(&self) -> &NotionIds {
        &self.ids
    }

    /// Create a main page and return its Notion page ID.
    pub async fn create_main_page(&self, title: &str) -> Result<String> {
        self.client.create_main_page(&self.ids, title).await
    }

    /// Create a text resource under the optional main page.
    pub async fn create_resource_text(
        &self,
        main_page_id: Option<&str>,
        order: i64,
        content: &str,
    ) -> Result<String> {
        self.client
            .create_resource_page(&self.ids, main_page_id, order, Some(content), None, None)
            .await
    }

    /// Create a media resource (external URL only) under the optional main page.
    pub async fn create_resource_media(
        &self,
        main_page_id: Option<&str>,
        order: i64,
        name: &str,
        external_url: &str,
    ) -> Result<String> {
        self.client
            .create_resource_page(
                &self.ids,
                main_page_id,
                order,
                None,
                Some(name),
                Some(external_url),
            )
            .await
    }

    /// Upload a local file and create a media resource under the optional main page.
    pub async fn create_resource_media_from_file<P: AsRef<Path>>(
        &self,
        main_page_id: Option<&str>,
        order: i64,
        file_path: P,
    ) -> Result<String> {
        let file_path = file_path.as_ref();
        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow!("invalid file name"))?;

        // Upload the file first to get the file upload ID
        let file_upload_id = self.client.upload_file(file_path).await?;

        // Then create the resource page with the uploaded file ID
        self.client
            .create_resource_page_with_file_upload(
                &self.ids,
                main_page_id,
                order,
                None,
                Some(file_name),
                Some(&file_upload_id),
            )
            .await
    }
}
