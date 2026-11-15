-- Initial Strands schema (ticket T2, spec §6 "Data model (Postgres)").
--
-- Every table here carries `user_id` and an FK back to `users(id)`, per the
-- closing line of spec §6: "Every table carries user_id and every query
-- filters on it; a single require_user extractor, no raw IDs from the
-- client without ownership check." That line is authoritative over the
-- abbreviated per-table column sketch earlier in §6, which omits `user_id`
-- from a few child tables (threads, summaries, summary_notes,
-- connector_secrets, import_runs) purely for brevity in the diagram.
--
-- IDs are application-supplied UUIDs (strands-core generates them with
-- `Uuid::new_v4()`), so primary keys have no `DEFAULT`.

-- ---------------------------------------------------------------------------
-- Enum types
-- ---------------------------------------------------------------------------

CREATE TYPE note_source AS ENUM ('manual', 'google_photos', 'whatsapp', 'google_maps');
CREATE TYPE note_kind AS ENUM ('text', 'photo', 'visit');
CREATE TYPE summary_state AS ENUM ('proposed', 'approved', 'discarded', 'edited', 'failed');
CREATE TYPE summary_preset AS ENUM ('shorter', 'takeaways', 'detail', 'dates');
CREATE TYPE connector_status AS ENUM ('disconnected', 'needs_sign_in', 'connected', 'error');
CREATE TYPE import_run_state AS ENUM ('running', 'completed', 'failed');
CREATE TYPE job_kind AS ENUM ('import', 'summarize', 'judge_links', 'enrich_places', 'export_kml');
CREATE TYPE job_state AS ENUM ('queued', 'running', 'completed', 'failed');

-- ---------------------------------------------------------------------------
-- users — the identity root (spec §6, D4 Google OAuth multi-user)
-- ---------------------------------------------------------------------------

CREATE TABLE users (
    id          UUID PRIMARY KEY,
    google_sub  TEXT NOT NULL,
    email       TEXT NOT NULL,
    name        TEXT NOT NULL,
    avatar_url  TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT users_google_sub_key UNIQUE (google_sub),
    CONSTRAINT users_email_key UNIQUE (email)
);

-- ---------------------------------------------------------------------------
-- sessions — HttpOnly session cookie backing store (spec §6 auth flow)
-- ---------------------------------------------------------------------------

CREATE TABLE sessions (
    id          UUID PRIMARY KEY,
    user_id     UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    expires_at  TIMESTAMPTZ NOT NULL
);

CREATE INDEX sessions_user_id_idx ON sessions (user_id);

-- ---------------------------------------------------------------------------
-- categories — 5 seeded per user on first login (spec §6)
-- ---------------------------------------------------------------------------

CREATE TABLE categories (
    id        UUID PRIMARY KEY,
    user_id   UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name      TEXT NOT NULL,
    color     TEXT NOT NULL,
    position  INTEGER NOT NULL
);

CREATE INDEX categories_user_id_idx ON categories (user_id);

-- ---------------------------------------------------------------------------
-- tags — the "strand" (spec §6). summary_approved_id is backfilled with its
-- FK below once `summaries` exists (forward reference).
-- ---------------------------------------------------------------------------

CREATE TABLE tags (
    id                  UUID PRIMARY KEY,
    user_id             UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name                TEXT NOT NULL,
    category_id         UUID NOT NULL REFERENCES categories (id) ON DELETE CASCADE,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    summary_approved_id UUID
);

CREATE INDEX tags_user_id_idx ON tags (user_id);

-- ---------------------------------------------------------------------------
-- threads — groups notes within a tag (spec §6)
-- ---------------------------------------------------------------------------

CREATE TABLE threads (
    id            UUID PRIMARY KEY,
    user_id       UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    tag_id        UUID NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
    title         TEXT NOT NULL,
    source_count  INTEGER NOT NULL DEFAULT 0,
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX threads_user_id_idx ON threads (user_id);

-- ---------------------------------------------------------------------------
-- notes — manual capture or normalized connector output (spec §6, §7.5
-- search needs a generated tsvector + GIN index over the body)
-- ---------------------------------------------------------------------------

CREATE TABLE notes (
    id           UUID PRIMARY KEY,
    user_id      UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    tag_id       UUID NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
    thread_id    UUID REFERENCES threads (id) ON DELETE SET NULL,
    kind         note_kind NOT NULL,
    body         TEXT NOT NULL,
    occurred_at  TIMESTAMPTZ NOT NULL,
    source       note_source NOT NULL,
    external_id  TEXT,
    dedupe_key   TEXT NOT NULL,
    meta         JSONB NOT NULL DEFAULT '{}',
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    body_tsv     TSVECTOR GENERATED ALWAYS AS (to_tsvector('english', body)) STORED,
    CONSTRAINT notes_user_id_dedupe_key_key UNIQUE (user_id, dedupe_key)
);

CREATE INDEX notes_user_id_idx ON notes (user_id);
CREATE INDEX notes_body_tsv_idx ON notes USING GIN (body_tsv);

-- ---------------------------------------------------------------------------
-- summaries — version history of AI-written tag summaries (spec §6, D8)
-- ---------------------------------------------------------------------------

CREATE TABLE summaries (
    id          UUID PRIMARY KEY,
    user_id     UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    tag_id      UUID NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
    version     INTEGER NOT NULL,
    body        TEXT NOT NULL,
    state       summary_state NOT NULL DEFAULT 'proposed',
    model       TEXT NOT NULL,
    guide       TEXT,
    preset      summary_preset,
    note_hash   TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT summaries_tag_id_version_key UNIQUE (tag_id, version)
);

CREATE INDEX summaries_user_id_idx ON summaries (user_id);

-- Now that `summaries` exists, back-fill the forward reference from `tags`.
ALTER TABLE tags
    ADD CONSTRAINT tags_summary_approved_id_fkey
    FOREIGN KEY (summary_approved_id) REFERENCES summaries (id) ON DELETE SET NULL;

-- ---------------------------------------------------------------------------
-- summary_notes — join table: which notes fed a summary version (spec §6)
-- ---------------------------------------------------------------------------

CREATE TABLE summary_notes (
    summary_id  UUID NOT NULL REFERENCES summaries (id) ON DELETE CASCADE,
    note_id     UUID NOT NULL REFERENCES notes (id) ON DELETE CASCADE,
    user_id     UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    PRIMARY KEY (summary_id, note_id)
);

CREATE INDEX summary_notes_user_id_idx ON summary_notes (user_id);

-- ---------------------------------------------------------------------------
-- links — AI-judged connections between tags (spec §6, D7; undirected,
-- unique pair enforced via a canonical a_tag_id < b_tag_id ordering)
-- ---------------------------------------------------------------------------

CREATE TABLE links (
    id         UUID PRIMARY KEY,
    user_id    UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    a_tag_id   UUID NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
    b_tag_id   UUID NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
    score      INTEGER NOT NULL,
    rationale  TEXT NOT NULL,
    judged_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT links_canonical_pair_order CHECK (a_tag_id < b_tag_id),
    CONSTRAINT links_a_tag_id_b_tag_id_key UNIQUE (a_tag_id, b_tag_id)
);

CREATE INDEX links_user_id_idx ON links (user_id);

-- ---------------------------------------------------------------------------
-- connectors — one row per connector kind per user (spec §6, D10/D12)
-- ---------------------------------------------------------------------------

CREATE TABLE connectors (
    id            UUID PRIMARY KEY,
    user_id       UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind          TEXT NOT NULL,
    status        connector_status NOT NULL DEFAULT 'disconnected',
    config        JSONB NOT NULL DEFAULT '{}',
    scopes        TEXT[] NOT NULL DEFAULT '{}',
    last_sync_at  TIMESTAMPTZ,
    last_error    TEXT,
    CONSTRAINT connectors_user_id_kind_key UNIQUE (user_id, kind)
);

CREATE INDEX connectors_user_id_idx ON connectors (user_id);

-- ---------------------------------------------------------------------------
-- connector_secrets — envelope-encrypted OAuth/session secrets (spec §6)
-- ---------------------------------------------------------------------------

CREATE TABLE connector_secrets (
    connector_id  UUID PRIMARY KEY REFERENCES connectors (id) ON DELETE CASCADE,
    user_id       UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    ciphertext    BYTEA NOT NULL
);

CREATE INDEX connector_secrets_user_id_idx ON connector_secrets (user_id);

-- ---------------------------------------------------------------------------
-- lists — place lists, first-class Strands objects (spec §6, D11)
-- ---------------------------------------------------------------------------

CREATE TABLE lists (
    id             UUID PRIMARY KEY,
    user_id        UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name           TEXT NOT NULL,
    source         TEXT NOT NULL,
    import_run_id  UUID
);

CREATE INDEX lists_user_id_idx ON lists (user_id);

-- ---------------------------------------------------------------------------
-- places — rows within a list, enriched via Places API (spec §6, §7.12)
-- ---------------------------------------------------------------------------

CREATE TABLE places (
    id            UUID PRIMARY KEY,
    user_id       UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    list_id       UUID REFERENCES lists (id) ON DELETE SET NULL,
    tag_id        UUID REFERENCES tags (id) ON DELETE SET NULL,
    name          TEXT NOT NULL,
    address       TEXT,
    lat           DOUBLE PRECISION,
    lng           DOUBLE PRECISION,
    place_id      TEXT,
    primary_type  TEXT,
    types         TEXT[] NOT NULL DEFAULT '{}',
    metro         TEXT,
    locality      TEXT,
    country       TEXT,
    notes         TEXT,
    source        TEXT NOT NULL,
    enriched_at   TIMESTAMPTZ
);

CREATE INDEX places_user_id_idx ON places (user_id);

-- ---------------------------------------------------------------------------
-- import_runs — one row per connector sync attempt (spec §6)
-- ---------------------------------------------------------------------------

CREATE TABLE import_runs (
    id            UUID PRIMARY KEY,
    user_id       UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    connector_id  UUID NOT NULL REFERENCES connectors (id) ON DELETE CASCADE,
    range         TEXT NOT NULL,
    state         import_run_state NOT NULL DEFAULT 'running',
    items_seen    INTEGER NOT NULL DEFAULT 0,
    items_kept    INTEGER NOT NULL DEFAULT 0,
    skipped       INTEGER NOT NULL DEFAULT 0,
    error         TEXT
);

CREATE INDEX import_runs_user_id_idx ON import_runs (user_id);

-- Now that `import_runs` exists, back-fill the forward reference from `lists`.
ALTER TABLE lists
    ADD CONSTRAINT lists_import_run_id_fkey
    FOREIGN KEY (import_run_id) REFERENCES import_runs (id) ON DELETE SET NULL;

-- ---------------------------------------------------------------------------
-- jobs — in-process job worker queue (spec §6, D13). The claim query is
-- `UPDATE ... WHERE state = 'queued' ORDER BY run_after LIMIT 1
-- FOR UPDATE SKIP LOCKED`, hence the (state, run_after) index.
-- ---------------------------------------------------------------------------

CREATE TABLE jobs (
    id          UUID PRIMARY KEY,
    user_id     UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind        job_kind NOT NULL,
    payload     JSONB NOT NULL DEFAULT '{}',
    state       job_state NOT NULL DEFAULT 'queued',
    attempts    INTEGER NOT NULL DEFAULT 0,
    run_after   TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_error  TEXT
);

CREATE INDEX jobs_user_id_idx ON jobs (user_id);
CREATE INDEX jobs_claim_idx ON jobs (state, run_after);
