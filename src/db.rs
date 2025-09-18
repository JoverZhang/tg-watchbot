use crate::model::{BatchState, OutboxKind};
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use sqlx::{sqlite::SqliteRow, Sqlite, SqlitePool};
use sqlx::{Row, Transaction};
use tracing::instrument;

pub type Pool = SqlitePool;

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
    let (had_slashes, path_with_query) = if let Some(r) = rest.strip_prefix("//") {
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
            .bind(BatchState::OPEN.as_str())
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
pub async fn insert_resource(
    pool: &Pool,
    user_id: i64,
    batch_id: Option<i64>,
    kind: &str,
    content: &str,
    tg_message_id: i32,
) -> Result<i64> {
    let mut tx = pool.begin().await?;
    let rec = sqlx::query(
        "INSERT INTO resources (user_id, batch_id, kind, content, tg_message_id) VALUES (?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(user_id)
    .bind(batch_id)
    .bind(kind)
    .bind(content)
    .bind(tg_message_id)
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

/// Compute the next sequence number for a resource within a batch.
/// Currently implemented as the count of existing resources for the batch.
pub async fn next_resource_sequence(pool: &Pool, batch_id: i64) -> Result<i64> {
    let cnt: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM resources WHERE batch_id = ?")
        .bind(batch_id)
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    Ok(cnt)
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
pub async fn next_due_outbox(pool: &Pool) -> Result<Option<(i64, i64, String, i64, i32)>> {
    let row = sqlx::query(
        "SELECT id, user_id, kind, ref_id, attempt FROM outbox WHERE datetime(due_at) <= CURRENT_TIMESTAMP ORDER BY datetime(due_at) ASC LIMIT 1",
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
        let rid = insert_resource(&pool, uid, Some(bid), "text", "hello", 1)
            .await
            .unwrap();
        // standalone should enqueue outbox
        let rid2 = insert_resource(&pool, uid, None, "text", "single", 2)
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
