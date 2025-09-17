use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum BatchState {
    OPEN,
    COMMITTED,
    ROLLED_BACK,
}

impl BatchState {
    pub fn as_str(&self) -> &'static str {
        match self {
            BatchState::OPEN => "OPEN",
            BatchState::COMMITTED => "COMMITTED",
            BatchState::ROLLED_BACK => "ROLLED_BACK",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum OutboxKind {
    PushBatch,
    PushResource,
}

impl OutboxKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            OutboxKind::PushBatch => "push_batch",
            OutboxKind::PushResource => "push_resource",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: i64,
    pub tg_user_id: i64,
    pub username: Option<String>,
    pub full_name: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Batch {
    pub id: i64,
    pub user_id: i64,
    pub state: BatchState,
    pub title: Option<String>,
    pub notion_page_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub committed_at: Option<DateTime<Utc>>,
    pub rolled_back_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resource {
    pub id: i64,
    pub user_id: i64,
    pub batch_id: Option<i64>,
    pub kind: String,
    pub content: String,
    pub tg_message_id: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxTask {
    pub id: i64,
    pub user_id: i64,
    pub kind: OutboxKind,
    pub ref_id: i64,
    pub attempt: i32,
    pub due_at: DateTime<Utc>,
}

