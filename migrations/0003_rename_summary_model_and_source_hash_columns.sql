-- Align the T2-seeded `summaries` columns with the names ticket T7's generation code
-- uses on the wire and in SQL: `model` becomes `model_alias` (the LiteLLM alias each
-- version was generated with) and `note_hash` becomes `source_hash` (the hash of the
-- notes + guide a version was generated from, used for the no-op short-circuit).

ALTER TABLE summaries RENAME COLUMN model TO model_alias;
ALTER TABLE summaries RENAME COLUMN note_hash TO source_hash;
