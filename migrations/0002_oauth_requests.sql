-- OAuth authorization-code + PKCE server-side state (ticket T3, spec §6
-- "Auth flow"). `GET /auth/google` writes a row here (the state token plus
-- the PKCE verifier and the exact redirect_uri it sent to Google) before
-- redirecting to Google's consent screen; the callback looks the row up by
-- `state`, deletes it (single use), and uses the verifier to complete the
-- exchange. Never put the PKCE verifier or any Google token in a cookie --
-- this table is the only place a verifier lives, and only for a few minutes.
--
-- No `user_id`: at this point in the flow nobody is authenticated yet, so
-- there is no user to scope the row to. See the NOT_USER_SCOPED allowlist
-- in crates/server/tests/schema_shape.rs.

CREATE TABLE oauth_requests (
    id             UUID PRIMARY KEY,
    state          TEXT NOT NULL,
    pkce_verifier  TEXT NOT NULL,
    redirect_uri   TEXT NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at     TIMESTAMPTZ NOT NULL,
    CONSTRAINT oauth_requests_state_key UNIQUE (state)
);

CREATE INDEX oauth_requests_expires_at_idx ON oauth_requests (expires_at);
