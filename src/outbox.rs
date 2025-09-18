use crate::db::{self, BatchForOutbox, ResourceForOutbox};
use crate::model::{BatchState, OutboxKind};
use crate::notion::{NotionIds, NotionService};
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

    if batch.state != BatchState::COMMITTED {
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
        if state != BatchState::COMMITTED {
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
        "creating resource Notion page"
    );
    let page_id = notion
        .create_resource_page(
            notion_ids,
            parent_page_id.as_deref(),
            resource.sequence,
            text,
            media_name,
            media_url.as_deref(),
        )
        .await?;
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
