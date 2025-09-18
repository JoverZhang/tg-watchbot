use super::model::{BatchForOutbox, ResourceForOutbox};
use crate::model::{BatchState, OutboxKind};
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use sqlx::{Row, Transaction};
use sqlx::{Sqlite, SqlitePool};
use tracing::instrument;

pub type Pool = SqlitePool;
type OutboxItem = (i64, i64, String, i64, i32);

pub async fn init_pool(database_url: &str) -> Result<Pool> {
    let normalized = prepare_sqlite_url(database_url);
    let pool = SqlitePool::connect(&normalized).await?;
    // Enable WAL and stricter durability.
    sqlx::query("PRAGMA journal_mode=WAL;")
        .execute(&pool)
        .await?;
    sqlx::query("PRAGMA synchronous=FULL;")
        .execute(&pool)
        .await?;
    Ok(pool)
}

/// If using a file-backed SQLite URL, expand a leading `~/` and ensure the parent
/// directory exists. Leaves in-memory URLs untouched. Returns possibly-updated URL.
fn prepare_sqlite_url(url: &str) -> String {
    // Pass through non-sqlite schemes
    if !url.starts_with("sqlite:") {
        return url.to_string();
    }

    // In-memory URLs like sqlite::memory: or sqlite::memory:?cache=shared
    if url.starts_with("sqlite::memory") {
        return url.to_string();
    }

    // Strip prefix and optional //
    let rest = &url["sqlite:".len()..];
    let (_had_slashes, path_with_query) = if let Some(r) = rest.strip_prefix("//") {
        (true, r)
    } else {
        (false, rest)
    };

    // Separate query string if any
    let (path_part, query_part) = match path_with_query.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (path_with_query, None),
    };

    if path_part.is_empty() {
        // nothing to normalize
        return url.to_string();
    }

    // Expand leading ~/ to HOME
    let expanded_path = if let Some(rest) = path_part.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            format!("{}/{}", home.trim_end_matches('/'), rest)
        } else {
            path_part.to_string()
        }
    } else {
        path_part.to_string()
    };

    // Ensure parent directory exists if any
    if let Some(parent) = std::path::Path::new(&expanded_path).parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }

    // Rebuild URL, prefer sqlite:// form
    let mut rebuilt = String::from("sqlite://");
    rebuilt.push_str(&expanded_path);
    if let Some(q) = query_part {
        rebuilt.push('?');
        rebuilt.push_str(q);
    }
    rebuilt
}

pub async fn run_migrations(pool: &Pool) -> Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}

#[instrument(skip_all)]
pub async fn get_or_create_user(
    pool: &Pool,
    tg_user_id: i64,
    username: Option<&str>,
    full_name: Option<&str>,
) -> Result<i64> {
    if let Some(id) = sqlx::query_scalar::<_, i64>("SELECT id FROM users WHERE tg_user_id = ?")
        .bind(tg_user_id)
        .fetch_optional(pool)
        .await?
    {
        return Ok(id);
    }

    let rec = sqlx::query(
        "INSERT INTO users (tg_user_id, username, full_name) VALUES (?, ?, ?) RETURNING id",
    )
    .bind(tg_user_id)
    .bind(username)
    .bind(full_name)
    .fetch_one(pool)
    .await?;
    Ok(rec.get::<i64, _>("id"))
}

#[instrument(skip_all)]
pub async fn current_open_batch_id(pool: &Pool, user_id: i64) -> Result<Option<i64>> {
    let id = sqlx::query_scalar::<_, i64>("SELECT batch_id FROM current_batch WHERE user_id = ?")
        .bind(user_id)
        .fetch_optional(pool)
        .await?;
    Ok(id)
}

#[instrument(skip_all)]
pub async fn current_batch_state(pool: &Pool, user_id: i64) -> Result<Option<BatchState>> {
    let state: Option<String> = sqlx::query_scalar(
        "SELECT b.state FROM batches b JOIN current_batch c ON c.batch_id = b.id WHERE c.user_id = ?",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(state.and_then(|s| BatchState::parse_state(&s)))
}

#[instrument(skip_all)]
pub async fn open_batch(pool: &Pool, user_id: i64) -> Result<i64> {
    let mut tx = pool.begin().await?;
    let existing =
        sqlx::query_scalar::<_, i64>("SELECT batch_id FROM current_batch WHERE user_id = ?")
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?;
    if existing.is_some() {
        return Err(anyhow!("batch already open"));
    }
    let batch_id: i64 =
        sqlx::query("INSERT INTO batches (user_id, state) VALUES (?, ?) RETURNING id")
            .bind(user_id)
            .bind(BatchState::Open.as_str())
            .fetch_one(&mut *tx)
            .await?
            .get("id");
    sqlx::query("INSERT INTO current_batch (user_id, batch_id) VALUES (?, ?)")
        .bind(user_id)
        .bind(batch_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(batch_id)
}

#[instrument(skip_all)]
pub async fn rollback_batch(pool: &Pool, user_id: i64) -> Result<()> {
    let mut tx = pool.begin().await?;
    let batch_id =
        sqlx::query_scalar::<_, i64>("SELECT batch_id FROM current_batch WHERE user_id = ?")
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(batch_id) = batch_id else {
        return Err(anyhow!("no open batch"));
    };
    sqlx::query(
        "UPDATE batches SET state = 'ROLLED_BACK', rolled_back_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(batch_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM current_batch WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[instrument(skip_all)]
pub async fn commit_batch(pool: &Pool, user_id: i64, title: Option<&str>) -> Result<i64> {
    let mut tx = pool.begin().await?;
    let batch_id =
        sqlx::query_scalar::<_, i64>("SELECT batch_id FROM current_batch WHERE user_id = ?")
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(batch_id) = batch_id else {
        return Err(anyhow!("no open batch"));
    };
    sqlx::query("UPDATE batches SET state = 'COMMITTED', committed_at = CURRENT_TIMESTAMP, title = COALESCE(?, title) WHERE id = ?")
        .bind(title)
        .bind(batch_id)
        .execute(&mut *tx)
        .await?;
    // enqueue push for batch
    enqueue_outbox_tx(
        &mut tx,
        user_id,
        OutboxKind::PushBatch,
        batch_id,
        Utc::now(),
    )
    .await?;

    // enqueue all resources in batch
    let res_ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM resources WHERE batch_id = ?")
        .bind(batch_id)
        .fetch_all(&mut *tx)
        .await?;
    for rid in res_ids {
        enqueue_outbox_tx(&mut tx, user_id, OutboxKind::PushResource, rid, Utc::now()).await?;
    }

    sqlx::query("DELETE FROM current_batch WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(batch_id)
}

#[instrument(skip_all)]
pub async fn mark_current_batch_waiting_title(pool: &Pool, user_id: i64) -> Result<()> {
    let mut tx = pool.begin().await?;
    let batch_id =
        sqlx::query_scalar::<_, i64>("SELECT batch_id FROM current_batch WHERE user_id = ?")
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(batch_id) = batch_id else {
        return Err(anyhow!("no open batch"));
    };
    sqlx::query("UPDATE batches SET state = 'WAITING_TITLE' WHERE id = ?")
        .bind(batch_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[instrument(skip_all)]
pub async fn insert_resource(
    pool: &Pool,
    user_id: i64,
    batch_id: Option<i64>,
    kind: &str,
    content: &str,
    tg_message_id: i32,
) -> Result<i64> {
    let mut tx = pool.begin().await?;

    // Calculate sequence for items in a batch (1..N). Standalone items use 1.
    let sequence_opt: Option<i64> = if let Some(batch_id) = batch_id {
        let max_seq: Option<i64> =
            sqlx::query_scalar("SELECT MAX(sequence) FROM resources WHERE batch_id = ?")
                .bind(batch_id)
                .fetch_optional(&mut *tx)
                .await?;
        Some(max_seq.unwrap_or(0) + 1)
    } else {
        Some(1)
    };
    let text_value = if kind == "text" {
        Some(content.to_string())
    } else {
        None
    };
    let rec = sqlx::query(
        "INSERT INTO resources (user_id, batch_id, kind, content, tg_message_id, sequence, text, media_name, media_url) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(user_id)
    .bind(batch_id)
    .bind(kind)
    .bind(content)
    .bind(tg_message_id)
    .bind(sequence_opt)
    .bind(text_value)
    .bind::<Option<String>>(None)
    .bind::<Option<String>>(None)
    .fetch_one(&mut *tx)
    .await?;
    let id: i64 = rec.get("id");

    // If standalone (no batch), enqueue push task
    if batch_id.is_none() {
        enqueue_outbox_tx(&mut tx, user_id, OutboxKind::PushResource, id, Utc::now()).await?;
    }

    tx.commit().await?;
    Ok(id)
}

// View models are declared in `model.rs` to keep repository focused on SQL.

pub async fn fetch_batch_for_outbox(pool: &Pool, batch_id: i64) -> Result<BatchForOutbox> {
    let row =
        sqlx::query("SELECT id, user_id, state, title, notion_page_id FROM batches WHERE id = ?")
            .bind(batch_id)
            .fetch_optional(pool)
            .await?;

    let Some(row) = row else {
        return Err(anyhow!("batch {} not found", batch_id));
    };

    let state_str: String = row.get("state");
    let state = BatchState::parse_state(&state_str)
        .ok_or_else(|| anyhow!("batch {} has unknown state {}", batch_id, state_str))?;

    Ok(BatchForOutbox {
        state,
        title: row.try_get("title").ok(),
        notion_page_id: row
            .try_get::<String, _>("notion_page_id")
            .ok()
            .filter(|s| !s.trim().is_empty()),
    })
}

pub async fn fetch_resource_for_outbox(pool: &Pool, resource_id: i64) -> Result<ResourceForOutbox> {
    let row = sqlx::query(
        "SELECT r.id, r.user_id, r.batch_id, r.sequence, r.text, r.media_name, r.media_url, \
                r.notion_page_id, r.kind, r.content, r.tg_message_id, \
                b.state AS batch_state, b.notion_page_id AS batch_notion_page_id \
         FROM resources r \
         LEFT JOIN batches b ON r.batch_id = b.id \
         WHERE r.id = ?",
    )
    .bind(resource_id)
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        return Err(anyhow!("resource {} not found", resource_id));
    };

    // Use stored sequence for Notion ordering: within batch it's 1..N; standalone defaults to 1.
    let batch_id_opt = row.try_get::<Option<i64>, _>("batch_id").ok().flatten();
    let sequence = row
        .try_get::<Option<i64>, _>("sequence")
        .ok()
        .flatten()
        .unwrap_or(1);

    let kind: String = row.get("kind");
    let content: String = row.get("content");
    let text: Option<String> = row
        .try_get::<Option<String>, _>("text")
        .ok()
        .flatten()
        .filter(|s| !s.is_empty());
    let text = text.or_else(|| {
        if kind == "text" {
            Some(content.clone())
        } else {
            None
        }
    });

    let batch_state = row
        .try_get::<Option<String>, _>("batch_state")
        .ok()
        .flatten()
        .and_then(|s| BatchState::parse_state(&s));

    Ok(ResourceForOutbox {
        batch_id: batch_id_opt,
        sequence,
        kind,
        content,
        text,
        media_name: row
            .try_get::<Option<String>, _>("media_name")
            .ok()
            .flatten(),
        media_url: row.try_get::<Option<String>, _>("media_url").ok().flatten(),
        notion_page_id: row
            .try_get::<Option<String>, _>("notion_page_id")
            .ok()
            .flatten(),
        batch_state,
        batch_notion_page_id: row
            .try_get::<Option<String>, _>("batch_notion_page_id")
            .ok()
            .flatten(),
    })
}

pub async fn mark_batch_notion_page_id(pool: &Pool, batch_id: i64, page_id: &str) -> Result<()> {
    sqlx::query("UPDATE batches SET notion_page_id = ? WHERE id = ?")
        .bind(page_id)
        .bind(batch_id)
        .execute(pool)
        .await
        .context("failed to persist batch notion page")?;
    Ok(())
}

pub async fn mark_resource_notion_page_id(
    pool: &Pool,
    resource_id: i64,
    page_id: &str,
) -> Result<()> {
    sqlx::query("UPDATE resources SET notion_page_id = ? WHERE id = ?")
        .bind(page_id)
        .bind(resource_id)
        .execute(pool)
        .await
        .context("failed to persist resource notion page")?;
    Ok(())
}

#[instrument(skip_all)]
pub async fn enqueue_outbox(
    pool: &Pool,
    user_id: i64,
    kind: OutboxKind,
    ref_id: i64,
    due_at: DateTime<Utc>,
) -> Result<i64> {
    let mut tx = pool.begin().await?;
    let id = enqueue_outbox_tx(&mut tx, user_id, kind, ref_id, due_at).await?;
    tx.commit().await?;
    Ok(id)
}

async fn enqueue_outbox_tx(
    tx: &mut Transaction<'_, Sqlite>,
    user_id: i64,
    kind: OutboxKind,
    ref_id: i64,
    due_at: DateTime<Utc>,
) -> Result<i64> {
    let rec = sqlx::query(
        "INSERT INTO outbox (user_id, kind, ref_id, attempt, due_at) VALUES (?, ?, ?, 0, ?) RETURNING id",
    )
    .bind(user_id)
    .bind(kind.as_str())
    .bind(ref_id)
    .bind(due_at)
    .fetch_one(&mut **tx)
    .await?;
    Ok(rec.get("id"))
}

#[instrument(skip_all)]
pub async fn next_due_outbox(pool: &Pool) -> Result<Option<OutboxItem>> {
    let row = sqlx::query(
        "SELECT id, user_id, kind, ref_id, attempt FROM outbox WHERE datetime(due_at) <= CURRENT_TIMESTAMP ORDER BY (CASE WHEN kind = 'push_batch' THEN 0 ELSE 1 END), datetime(due_at) ASC LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;
    if let Some(row) = row {
        let id: i64 = row.get("id");
        let user_id: i64 = row.get("user_id");
        let kind: String = row.get("kind");
        let ref_id: i64 = row.get("ref_id");
        let attempt: i32 = row.get("attempt");
        Ok(Some((id, user_id, kind, ref_id, attempt)))
    } else {
        Ok(None)
    }
}

pub async fn list_due_outbox(pool: &Pool) -> Result<Vec<(i64, String, i64)>> {
    let rows = sqlx::query(
        "SELECT id, kind, ref_id FROM outbox WHERE datetime(due_at) <= CURRENT_TIMESTAMP ORDER BY datetime(due_at) ASC",
    )
    .fetch_all(pool)
    .await?;

    let tasks = rows
        .into_iter()
        .map(|row| {
            let id: i64 = row.get("id");
            let kind: String = row.get("kind");
            let ref_id: i64 = row.get("ref_id");
            (id, kind, ref_id)
        })
        .collect();
    Ok(tasks)
}

#[instrument(skip_all)]
pub async fn delete_outbox(pool: &Pool, id: i64) -> Result<()> {
    sqlx::query("DELETE FROM outbox WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

#[instrument(skip_all)]
pub async fn backoff_outbox(pool: &Pool, id: i64, attempt: i32) -> Result<()> {
    // Exponential backoff: 5s * 2^attempt, capped at 3600s
    let secs = (5_i64) * (1_i64 << attempt.min(10));
    let secs = secs.min(3600);
    sqlx::query(
        "UPDATE outbox SET attempt = ?, due_at = datetime('now', ? || ' seconds') WHERE id = ?",
    )
    .bind(attempt + 1)
    .bind(secs)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

#[instrument(skip_all)]
pub async fn backoff_outbox_with_cap(
    pool: &Pool,
    id: i64,
    attempt: i32,
    max_cap_secs: i64,
) -> Result<()> {
    let secs = (5_i64) * (1_i64 << attempt.min(10));
    let cap = if max_cap_secs <= 0 {
        secs
    } else {
        max_cap_secs
    };
    let secs = secs.min(cap);
    sqlx::query(
        "UPDATE outbox SET attempt = ?, due_at = datetime('now', ? || ' seconds') WHERE id = ?",
    )
    .bind(attempt + 1)
    .bind(secs)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

#[instrument(skip_all)]
pub async fn get_last_processed_outbox_id(pool: &Pool) -> Result<i64> {
    let id: i64 = sqlx::query_scalar("SELECT last_sent_outbox_id FROM outbox_cursor WHERE id = 1")
        .fetch_one(pool)
        .await?;
    Ok(id)
}

#[instrument(skip_all)]
pub async fn update_last_processed_outbox_id(pool: &Pool, outbox_id: i64) -> Result<()> {
    sqlx::query(
        "UPDATE outbox_cursor SET last_sent_outbox_id = ?, updated_at = CURRENT_TIMESTAMP WHERE id = 1",
    )
    .bind(outbox_id)
    .execute(pool)
    .await?;
    Ok(())
}

#[instrument(skip_all)]
pub async fn count_remaining_outbox_tasks(pool: &Pool) -> Result<i64> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM outbox")
        .fetch_one(pool)
        .await?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup_pool() -> Pool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("PRAGMA journal_mode=WAL;")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn test_open_commit_rollback() {
        let pool = setup_pool().await;
        let uid = get_or_create_user(&pool, 123, Some("alice"), Some("Alice"))
            .await
            .unwrap();

        assert!(current_open_batch_id(&pool, uid).await.unwrap().is_none());
        let bid = open_batch(&pool, uid).await.unwrap();
        assert_eq!(current_open_batch_id(&pool, uid).await.unwrap(), Some(bid));

        // insert resource in batch
        let _rid = insert_resource(&pool, uid, Some(bid), "text", "hello", 1)
            .await
            .unwrap();
        // standalone should enqueue outbox
        let _rid2 = insert_resource(&pool, uid, None, "text", "single", 2)
            .await
            .unwrap();

        // commit should enqueue tasks
        commit_batch(&pool, uid, Some("My Title")).await.unwrap();
        assert!(current_open_batch_id(&pool, uid).await.unwrap().is_none());

        // Expect at least 2 tasks: one batch, one resource (rid) plus standalone already queued
        let cnt: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM outbox")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(cnt >= 2);

        // Backoff and delete flow
        if let Some((oid, _u, _k, _r, attempt)) = next_due_outbox(&pool).await.unwrap() {
            backoff_outbox(&pool, oid, attempt).await.unwrap();
        }
    }
}
