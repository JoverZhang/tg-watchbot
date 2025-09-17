-- Users table
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

-- Resources
CREATE TABLE IF NOT EXISTS resources (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    batch_id INTEGER REFERENCES batches(id) ON DELETE SET NULL,
    kind TEXT NOT NULL,
    content TEXT NOT NULL,
    tg_message_id INTEGER NOT NULL,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(user_id, tg_message_id)
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

