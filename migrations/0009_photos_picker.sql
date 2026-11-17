-- Google Photos Picker connector (ticket T14): picker sessions, stored thumbnails.
--
-- Photos is Picker-only: the user picks in Google's UI, and the items' `baseUrl`s
-- are live for 60 minutes from issue. Two things therefore have to be durable
-- that a request-scoped connector could otherwise keep in memory:
--
--   * the session row, because polling happens in a later request than creation
--     and the state (`pending` -> `imported`, or `expired`) must outlive it;
--   * the thumbnail bytes, because a lapsed `baseUrl` cannot be re-read. There is
--     no object store here, so the bytes live in Postgres next to the note and
--     still render after a restart.
--
-- `notes.meta.note_thumbnails` holds no path: the row is the storage. The byte
-- cap is a constraint rather than a code path, so no caller can store more than
-- a preview regardless of what the remote advertises.

CREATE TABLE photo_picker_sessions (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- Google's session id: what poll and list are addressed with.
    session_id TEXT NOT NULL,
    -- The URL the user opens to pick; shown once, never logged.
    picker_uri TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    -- `pending` until the user finishes picking, then `imported` or `expired`.
    state TEXT NOT NULL DEFAULT 'pending'
        CONSTRAINT photo_picker_sessions_state_check
        CHECK (state IN ('pending', 'imported', 'expired')),
    notes_created INTEGER NOT NULL DEFAULT 0,
    notes_skipped INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, session_id)
);

-- Polling walks the newest sessions first.
CREATE INDEX photo_picker_sessions_user_created_idx
    ON photo_picker_sessions (user_id, created_at DESC);

CREATE TABLE note_thumbnails (
    note_id UUID PRIMARY KEY REFERENCES notes (id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    content_type TEXT NOT NULL,
    bytes BYTEA NOT NULL
        CONSTRAINT note_thumbnails_size_check
        CHECK (octet_length(bytes) > 0 AND octet_length(bytes) <= 524288),
    width INTEGER NOT NULL DEFAULT 0,
    height INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX note_thumbnails_user_id_idx ON note_thumbnails (user_id);
