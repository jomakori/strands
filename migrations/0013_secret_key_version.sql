-- Per-account connector encryption (C2).
--
-- `connector_secrets.ciphertext` used to be sealed under one process-wide key,
-- so a blob could be opened in any account's context. C2 derives a key per user
-- with HKDF-SHA256 and records which derivation sealed each row:
--
--   1 = the old global key (`STRANDS_SECRET_KEY` used directly)
--   2 = HKDF-SHA256(master, info = user_id)
--
-- The default is 1 because every row that already exists was sealed that way.
-- Reads dispatch on this column, so old rows keep opening after the deploy, and
-- each one is re-sealed as 2 the first time it is read and written back. There
-- is no bulk rewrite and no maintenance window.
--
-- `ADD COLUMN ... DEFAULT` is metadata-only on Postgres 11+; the table is not
-- rewritten.
ALTER TABLE connector_secrets
    ADD COLUMN key_version SMALLINT NOT NULL DEFAULT 1;

COMMENT ON COLUMN connector_secrets.key_version IS
    'Which derivation sealed ciphertext: 1 = global key, 2 = HKDF-SHA256 per user';
