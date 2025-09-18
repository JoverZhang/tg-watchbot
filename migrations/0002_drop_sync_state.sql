-- Migrate away from duplicate sync_state table.
-- Preserve any existing value by copying into outbox_cursor, then drop sync_state.

-- Copy value if sync_state exists and has the singleton row
UPDATE outbox_cursor
SET last_sent_outbox_id = COALESCE((SELECT last_processed_outbox_id FROM sync_state WHERE id = 1), last_sent_outbox_id),
    updated_at = CURRENT_TIMESTAMP
WHERE id = 1;

-- Drop the deprecated table if present
DROP TABLE IF EXISTS sync_state;

