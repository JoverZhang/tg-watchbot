ALTER TABLE resources ADD COLUMN notion_page_id TEXT;
ALTER TABLE resources ADD COLUMN sequence INTEGER;
ALTER TABLE resources ADD COLUMN text TEXT;
ALTER TABLE resources ADD COLUMN media_name TEXT;
ALTER TABLE resources ADD COLUMN media_url TEXT;

UPDATE resources SET sequence = tg_message_id WHERE sequence IS NULL;
UPDATE resources SET text = content WHERE kind = 'text' AND text IS NULL;
