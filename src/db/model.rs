//! Database entity and view models used by repositories.
//!
//! Keep these structs focused on the data returned by queries. Business logic
//! should live in higher layers.

use crate::model::BatchState;

/// Batch slice used by the outbox worker to decide how to sync a batch.
#[derive(Debug, Clone)]
pub struct BatchForOutbox {
    pub state: BatchState,
    pub title: Option<String>,
    pub notion_page_id: Option<String>,
}

/// Resource slice used by the outbox worker when pushing an item.
#[derive(Debug, Clone)]
pub struct ResourceForOutbox {
    pub batch_id: Option<i64>,
    pub sequence: i64,
    pub text: Option<String>,
    pub media_name: Option<String>,
    pub media_url: Option<String>,
    pub notion_page_id: Option<String>,
    pub batch_state: Option<BatchState>,
    pub batch_notion_page_id: Option<String>,
}

