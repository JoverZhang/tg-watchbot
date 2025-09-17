use crate::db;
use crate::model::OutboxKind;
use crate::notion::NotionClient;
use anyhow::Result;
use sqlx::SqlitePool;
use tracing::{info, instrument, warn};

#[instrument(skip_all)]
pub async fn process_next_task(
    pool: &SqlitePool,
    notion: &dyn NotionClient,
    max_backoff_secs: i64,
) -> Result<bool> {
    if let Some((id, _user_id, kind, ref_id, attempt)) = db::next_due_outbox(pool).await? {
        let kind_enum = match kind.as_str() {
            "push_batch" => OutboxKind::PushBatch,
            _ => OutboxKind::PushResource,
        };
        let res = match kind_enum {
            OutboxKind::PushBatch => notion.push_batch(pool, ref_id).await,
            OutboxKind::PushResource => notion.push_resource(pool, ref_id).await,
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
