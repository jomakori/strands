-- ---------------------------------------------------------------------------
-- T12 — quick capture: a body a person typed is bounded at 5000 characters.
--
-- The handler validates first and answers 400 with a readable message; this
-- constraint is the durable half, so no other write path can store an
-- over-long manual note. Connector imports keep going through `Note::new`
-- and stay unbounded: their bodies are normalized output, not typing.
-- ---------------------------------------------------------------------------

ALTER TABLE notes
    ADD CONSTRAINT notes_manual_body_length
    CHECK (source <> 'manual' OR char_length(body) <= 5000);
