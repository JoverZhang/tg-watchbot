-- Users
CREATE TABLE IF NOT EXISTS users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    tg_user_id INTEGER NOT NULL UNIQUE,
    username TEXT,
    full_name TEXT,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Batches
CREATE TABLE IF NOT EXISTS batches (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    state TEXT NOT NULL,
    title TEXT,
    notion_page_id TEXT,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    committed_at DATETIME,
    rolled_back_at DATETIME
);

-- Resources (final schema after all migrations)
CREATE TABLE IF NOT EXISTS resources (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    batch_id INTEGER REFERENCES batches(id) ON DELETE SET NULL,
    kind TEXT NOT NULL,
    content TEXT NOT NULL,
    tg_message_id INTEGER NOT NULL,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    notion_page_id TEXT,
    sequence INTEGER,
    text TEXT,
    media_name TEXT,
    media_url TEXT,
    UNIQUE(user_id, tg_message_id, kind, content)
);

-- Outbox
CREATE TABLE IF NOT EXISTS outbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    ref_id INTEGER NOT NULL,
    attempt INTEGER NOT NULL DEFAULT 0,
    due_at DATETIME NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_outbox_due_at ON outbox(due_at);

-- Current batch pointer
CREATE TABLE IF NOT EXISTS current_batch (
    user_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    batch_id INTEGER REFERENCES batches(id) ON DELETE SET NULL
);

-- Outbox cursor (single row)
CREATE TABLE IF NOT EXISTS outbox_cursor (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    last_sent_outbox_id INTEGER NOT NULL DEFAULT 0,
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);
INSERT OR IGNORE INTO outbox_cursor (id, last_sent_outbox_id) VALUES (1, 0);
