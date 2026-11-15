-- Full-text search over thread titles (ticket T11, spec §7.1 search).
--
-- Notes already carry a generated `body_tsv` column + GIN index (migration
-- 0001), so a note's own content is already indexed. This migration adds
-- the matching generated column on `threads` so a search can also rank a
-- thread by its own title, not only by the notes filed under it.

ALTER TABLE threads
    ADD COLUMN title_tsv TSVECTOR GENERATED ALWAYS AS (to_tsvector('english', title)) STORED;

CREATE INDEX threads_title_tsv_idx ON threads USING GIN (title_tsv);
