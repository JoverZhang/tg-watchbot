use anyhow::Result;
use std::path::Path;

use tg_watchbot::config;
use tg_watchbot::notion::{NotionClient, NotionFacade};

#[tokio::test]
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
        .create_resource_media_from_file(
            Some(&main_page_id),
            2,
            "tests/media/test_picture.jpg",
        )
        .await?;
    assert!(!photo_page.trim().is_empty());
    println!(
        "Created photo page: https://www.notion.so/{}",
        photo_page.replace('-', "")
    );

    // Create a video resource (upload local file)
    let video_page = notion
        .create_resource_media_from_file(
            Some(&main_page_id),
            3,
            "tests/media/video.mp4",
        )
        .await?;
    assert!(!video_page.trim().is_empty());
    println!(
        "Created video page: https://www.notion.so/{}",
        video_page.replace('-', "")
    );

    Ok(())
}

