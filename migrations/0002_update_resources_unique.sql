-- Allow multiple resource rows per Telegram message by differentiating on kind/content
PRAGMA foreign_keys=OFF;

CREATE TABLE resources_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    batch_id INTEGER REFERENCES batches(id) ON DELETE SET NULL,
    kind TEXT NOT NULL,
    content TEXT NOT NULL,
    tg_message_id INTEGER NOT NULL,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(user_id, tg_message_id, kind, content)
);

INSERT INTO resources_new (id, user_id, batch_id, kind, content, tg_message_id, created_at)
SELECT id, user_id, batch_id, kind, content, tg_message_id, created_at FROM resources;

DROP TABLE resources;
ALTER TABLE resources_new RENAME TO resources;

PRAGMA foreign_keys=ON;
