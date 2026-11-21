-- Connector import dedupe ledger (ticket T33b).
--
-- `connector_items` is the connector-agnostic record of what `import`
-- already wrote: one row per `(user_id, connector, item_key)` ever imported.
-- It is deliberately separate from `notes.dedupe_key` — each connector's
-- dedupe key format is its own business, but `preview` (T33a) needs one
-- generic place to ask "did an earlier import already land this item_key?"
-- without knowing any connector's note-writing details.
--
-- The row and the strand (note/thread) it protects are written in the same
-- transaction by the caller (see `crates/server/src/connectors/ledger.rs`),
-- so a crash cannot leave one without the other, and a repeat import with
-- the same item_key writes nothing thanks to the unique constraint below.

CREATE TABLE connector_items (
    id          UUID PRIMARY KEY,
    user_id     UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    connector   TEXT NOT NULL,
    item_key    TEXT NOT NULL,
    source_hash TEXT NOT NULL,
    thread_id   UUID REFERENCES threads (id) ON DELETE SET NULL,
    imported_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT connector_items_user_id_connector_item_key_key
        UNIQUE (user_id, connector, item_key)
);

CREATE INDEX connector_items_user_id_connector_imported_at_idx
    ON connector_items (user_id, connector, imported_at DESC);
