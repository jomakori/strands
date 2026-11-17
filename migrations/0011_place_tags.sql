-- Location as a tagging axis (ticket T21, decision D19).
--
-- Three things live here: a durable reverse-geocode cache so re-importing the
-- same export spends no API calls, a join that lets a note carry a second tag
-- (a note keeps one primary tag for thread grouping), and the staging table
-- that holds Nearby Search results until the user accepts or dismisses them.

-- ---------------------------------------------------------------------------
-- geocode_cache — reverse-geocode results keyed on the rounded coordinate
-- ---------------------------------------------------------------------------
--
-- The key is the coordinate rounded to four decimal places (~11 m), which is
-- the granularity the backfill looks up with: two GPS fixes a few metres apart
-- hit the same row instead of spending two API calls. `found` records what the
-- provider said, so "asked, nothing administratively useful there" is cached
-- like any other answer and open water is not re-asked forever.
--
-- The primary key is the pair, not `user_id` alone: one user has one row per
-- coordinate, and a cache that could only hold one row per user would silently
-- re-query every other place.

CREATE TABLE geocode_cache (
    user_id     UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    coord_key   TEXT NOT NULL,
    lat         DOUBLE PRECISION NOT NULL,
    lng         DOUBLE PRECISION NOT NULL,
    found       BOOLEAN NOT NULL,
    locality    TEXT,
    county      TEXT,
    state       TEXT,
    country     TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, coord_key)
);

-- ---------------------------------------------------------------------------
-- note_tags — the tags a note is filed under beyond its primary one
-- ---------------------------------------------------------------------------
--
-- `notes.tag_id` decides the note's thread and stays the primary tag. Location
-- is a second axis: a visit is filed under "Cafe" *and* "Metro: Lisbon", and
-- the tag screen has to find it under both. The trigger keeps the join honest
-- for every writer, including the ones that never heard of this table.

CREATE TABLE note_tags (
    note_id     UUID NOT NULL REFERENCES notes (id) ON DELETE CASCADE,
    tag_id      UUID NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
    user_id     UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    PRIMARY KEY (note_id, tag_id)
);

CREATE INDEX note_tags_by_tag ON note_tags (user_id, tag_id);

INSERT INTO note_tags (note_id, tag_id, user_id)
SELECT id, tag_id, user_id FROM notes WHERE tag_id IS NOT NULL;

CREATE FUNCTION note_tags_track_primary_tag() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND OLD.tag_id IS DISTINCT FROM NEW.tag_id THEN
        DELETE FROM note_tags
        WHERE note_id = NEW.id AND tag_id = OLD.tag_id;
    END IF;
    INSERT INTO note_tags (note_id, tag_id, user_id)
    VALUES (NEW.id, NEW.tag_id, NEW.user_id)
    ON CONFLICT DO NOTHING;
    RETURN NULL;
END;
$$;

CREATE TRIGGER notes_track_primary_tag
    AFTER INSERT OR UPDATE OF tag_id ON notes
    FOR EACH ROW
    WHEN (NEW.tag_id IS NOT NULL)
    EXECUTE FUNCTION note_tags_track_primary_tag();

-- ---------------------------------------------------------------------------
-- place_candidates — Nearby Search results, staged for the user to act on
-- ---------------------------------------------------------------------------
--
-- Tapping "Nearby" never creates notes: it fills this table, and a candidate
-- becomes a note only if the user taps it. `candidate_key` is the dedupe
-- identity (Google's place id, or the rounded coordinate plus the normalized
-- name) so tapping Nearby twice updates the staging rows instead of stacking
-- duplicate cards. `note_id` remembers the note an accepted candidate made, so
-- a double tap is idempotent rather than a second note.

CREATE TABLE place_candidates (
    id            UUID PRIMARY KEY,
    user_id       UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    candidate_key TEXT NOT NULL,
    name          TEXT NOT NULL,
    address       TEXT,
    lat           DOUBLE PRECISION NOT NULL,
    lng           DOUBLE PRECISION NOT NULL,
    place_id      TEXT,
    primary_type  TEXT,
    types         JSONB NOT NULL DEFAULT '[]'::jsonb,
    poi_tag       TEXT NOT NULL,
    metro         TEXT,
    state         TEXT NOT NULL DEFAULT 'pending'
                  CHECK (state IN ('pending', 'accepted', 'dismissed')),
    note_id       UUID REFERENCES notes (id) ON DELETE SET NULL,
    found_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT place_candidates_user_key UNIQUE (user_id, candidate_key)
);

CREATE INDEX place_candidates_pending ON place_candidates (user_id, state, found_at DESC);
