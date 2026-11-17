-- WhatsApp link (ticket T15).
--
-- The chat list exists as its own table because it has to survive the pairing
-- screen: the client renders every chat on the phone, the user tick what may be
-- imported, and only then does anything write notes. Keeping the snapshot in a
-- table (rather than in the process) means a restart mid-pairing still shows the
-- list and keeps the allowlist reviewable.
CREATE TABLE whatsapp_chats (
    user_id          UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    jid              TEXT NOT NULL,
    display_name     TEXT,
    is_group         BOOLEAN NOT NULL DEFAULT FALSE,
    -- Highest message timestamp written for this chat; "Sync now" imports
    -- strictly past it, so a redelivered message cannot become a second note.
    imported_through TIMESTAMPTZ,
    seen_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, jid)
);

-- The allowlist itself is a JSON array on the connector row
-- (`config->'allowlist'`), not a join table: the connector row already carries
-- per-user, per-kind state and cascades with the user.
--
-- The encrypted session blob lives in connector_secrets.ciphertext, keyed by
-- the connector row, so an account's WhatsApp keys follow the same at-rest story
-- as every other connector secret.
CREATE INDEX whatsapp_chats_user_id_seen_at_idx ON whatsapp_chats (user_id, seen_at DESC);
