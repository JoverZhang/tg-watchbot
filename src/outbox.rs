use crate::db::{self, BatchForOutbox, ResourceForOutbox};
use crate::model::{BatchState, OutboxKind};
use crate::notion::{NotionClient, NotionIds, NotionService};
use anyhow::{anyhow, Result};
use sqlx::SqlitePool;
use tracing::{debug, info, instrument, warn};

#[instrument(skip_all)]
pub async fn process_next_task(
    pool: &SqlitePool,
    notion: &dyn NotionService,
    notion_ids: &NotionIds,
    max_backoff_secs: i64,
) -> Result<bool> {
    if let Some((id, _user_id, kind, ref_id, attempt)) = db::next_due_outbox(pool).await? {
        let kind_enum = match kind.as_str() {
            "push_batch" => OutboxKind::PushBatch,
            _ => OutboxKind::PushResource,
        };
        let res = match kind_enum {
            OutboxKind::PushBatch => push_batch_task(pool, notion, notion_ids, ref_id).await,
            OutboxKind::PushResource => push_resource_task(pool, notion, notion_ids, ref_id).await,
        };
        match res {
            Ok(_) => {
                db::delete_outbox(pool, id).await?;
                info!(id, kind, ref_id, "outbox task succeeded");
            }
            Err(err) => {
                warn!(
                    ?err,
                    id, kind, ref_id, attempt, "outbox task failed; backoff"
                );
                db::backoff_outbox_with_cap(pool, id, attempt, max_backoff_secs).await?;
            }
        }
        return Ok(true);
    }
    Ok(false)
}

async fn push_batch_task(
    pool: &SqlitePool,
    notion: &dyn NotionService,
    notion_ids: &NotionIds,
    batch_id: i64,
) -> Result<()> {
    let batch: BatchForOutbox = db::fetch_batch_for_outbox(pool, batch_id).await?;
    if let Some(existing) = &batch.notion_page_id {
        debug!(batch_id, notion_page_id=%existing, "batch already synced; skipping");
        return Ok(());
    }

    if batch.state != BatchState::Committed {
        return Err(anyhow!(
            "batch {} not committed (state {:?})",
            batch_id,
            batch.state
        ));
    }

    let title = batch
        .title
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or("Untitled");
    info!(batch_id, title, "creating main Notion page");
    let page_id = notion.create_main_page(notion_ids, title).await?;
    db::mark_batch_notion_page_id(pool, batch_id, &page_id).await?;
    Ok(())
}

async fn push_resource_task(
    pool: &SqlitePool,
    notion: &dyn NotionService,
    notion_ids: &NotionIds,
    resource_id: i64,
) -> Result<()> {
    let resource: ResourceForOutbox = db::fetch_resource_for_outbox(pool, resource_id).await?;
    if let Some(existing) = &resource.notion_page_id {
        debug!(resource_id, notion_page_id=%existing, "resource already synced; skipping");
        return Ok(());
    }

    let parent_page_id = if let Some(batch_id) = resource.batch_id {
        let state = resource.batch_state.ok_or_else(|| {
            anyhow!(
                "resource {} has batch {} without state",
                resource_id,
                batch_id
            )
        })?;
        if state != BatchState::Committed {
            return Err(anyhow!(
                "batch {} not ready for resource {} (state {:?})",
                batch_id,
                resource_id,
                state
            ));
        }
        let notion_page = resource.batch_notion_page_id.ok_or_else(|| {
            anyhow!(
                "batch {} missing Notion page id for resource {}; retry after main page",
                batch_id,
                resource_id
            )
        })?;
        Some(notion_page)
    } else {
        None
    };

    let text = resource.text.as_deref();
    let media_url = sanitize_media_url(resource.media_url.as_deref());
    let media_name = resource
        .media_name
        .as_deref()
        .filter(|name| !name.is_empty());

    info!(
        resource_id,
        order = resource.sequence,
        kind = %resource.kind,
        "creating resource Notion page"
    );

    // Prefer external URL if present; otherwise, attempt to upload a local file if available
    let page_id = if media_url.is_some() || resource.kind == "text" {
        notion
            .create_resource_page(
                notion_ids,
                parent_page_id.as_deref(),
                resource.sequence,
                text,
                media_name,
                media_url.as_deref(),
            )
            .await?
    } else {
        // Try to downcast to a concrete NotionClient for file uploads
        if let Some(client) = (notion as &dyn std::any::Any).downcast_ref::<NotionClient>() {
            // Use the DB `content` as a local file path if it exists
            let path = std::path::Path::new(&resource.content);
            if path.exists() {
                // If this is a video, attempt to also attach its generated thumbnail first
                if resource.kind == "video" {
                    let mut files: Vec<(String, String)> = Vec::new();

                    // Derive thumbnail path: data_dir/media/thumbs/{video_stem}.jpg
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        // Try to locate the 'media' directory ancestor to infer data_dir
                        let thumb_path = derive_thumb_path_from_video(path, stem);
                        if let Some(tp) = thumb_path {
                            if tp.exists() {
                                let tname = tp
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("thumb.jpg");
                                let tid = client.upload_file(&tp).await?;
                                files.push((tname.to_string(), tid));
                            }
                        }
                    }

                    // Always upload the video itself second
                    let vname = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("video.bin");
                    let vid = client.upload_file(path).await?;
                    files.push((vname.to_string(), vid));

                    client
                        .create_resource_page_with_file_uploads(
                            notion_ids,
                            parent_page_id.as_deref(),
                            resource.sequence,
                            text,
                            &files,
                        )
                        .await?
                } else {
                    // Non-video: single file upload
                    let file_name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("uploaded.bin");
                    let upload_id = client.upload_file(path).await?;
                    client
                        .create_resource_page_with_file_upload(
                            notion_ids,
                            parent_page_id.as_deref(),
                            resource.sequence,
                            text,
                            Some(file_name),
                            Some(&upload_id),
                        )
                        .await?
                }
            } else {
                // Fallback: create without media
                notion
                    .create_resource_page(
                        notion_ids,
                        parent_page_id.as_deref(),
                        resource.sequence,
                        text,
                        None,
                        None,
                    )
                    .await?
            }
        } else {
            // Fallback for mock implementations without upload support
            notion
                .create_resource_page(
                    notion_ids,
                    parent_page_id.as_deref(),
                    resource.sequence,
                    text,
                    None,
                    None,
                )
                .await?
        }
    };
    db::mark_resource_notion_page_id(pool, resource_id, &page_id).await?;
    Ok(())
}

fn sanitize_media_url(raw: Option<&str>) -> Option<String> {
    let url = raw?.trim();
    if url.is_empty() {
        return None;
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return None;
    }
    if url.contains("api.telegram.org") {
        return None;
    }
    Some(url.to_string())
}

/// Try to derive `{data_dir}/media/thumbs/{stem}.jpg` from a video path like
/// `{data_dir}/media/{user_id}/{stem}.{ext}`.
fn derive_thumb_path_from_video(
    video_path: &std::path::Path,
    stem: &str,
) -> Option<std::path::PathBuf> {
    // Find the "media" directory in the ancestors
    let mut cur = video_path.parent();
    while let Some(p) = cur {
        if p.file_name().and_then(|n| n.to_str()) == Some("media") {
            // data_dir is parent of media
            let data_dir = p.parent()?;
            return Some(
                data_dir
                    .join("media")
                    .join("thumbs")
                    .join(format!("{}.jpg", stem)),
            );
        }
        cur = p.parent();
    }
    None
}
