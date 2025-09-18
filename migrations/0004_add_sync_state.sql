-- Sync state tracking table
CREATE TABLE IF NOT EXISTS sync_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    last_processed_outbox_id INTEGER NOT NULL DEFAULT 0,
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Insert initial row
INSERT OR IGNORE INTO sync_state (id, last_processed_outbox_id) VALUES (1, 0);