-- Cursor table tracking last successfully sent outbox item.
CREATE TABLE IF NOT EXISTS outbox_cursor (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    last_sent_outbox_id INTEGER NOT NULL DEFAULT 0,
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Seed single-row cursor if missing.
INSERT OR IGNORE INTO outbox_cursor (id, last_sent_outbox_id) VALUES (1, 0);

