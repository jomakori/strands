-- ---------------------------------------------------------------------------
-- T17 — account screen names the sign-in method.
--
-- Until now the identity provider was implicit: `google_sub` was the only
-- subject column, so "google" lived in code. The account screen has to tell a
-- person which sign-in their rows belong to, and a display string belongs in
-- the row that owns it, not in a literal at the call site. A second provider
-- later becomes a value here plus its own subject column, not a rewrite of the
-- profile query.
--
-- Existing accounts were all created by the Google flow, so 'google' is the
-- backfill and the default.
-- ---------------------------------------------------------------------------

ALTER TABLE users
    ADD COLUMN provider TEXT NOT NULL DEFAULT 'google';
