-- T19: archiving a thread hides it from its tag's list without deleting the notes it holds.
-- Nullable on purpose: every existing row is live, and "archived at" is the only fact the
-- list query needs, so no second boolean can disagree with the timestamp.
ALTER TABLE threads ADD COLUMN archived_at timestamptz;
