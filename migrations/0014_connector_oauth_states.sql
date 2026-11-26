-- Connector OAuth connect state (B1, spec §"Connecting a connector").
--
-- The login flow already has `oauth_requests` for its own half-finished state,
-- but that table is deliberately unscoped: nobody is authenticated yet. A
-- connector connect starts from a signed-in user and the grant has to belong to
-- that user, so it gets a user-scoped row instead.
--
-- `state` is the value round-tripped through the provider. `pkce_verifier` is
-- the plain verifier the challenge was derived from — it lives here and nowhere
-- else, exactly like `oauth_requests.pkce_verifier`, and only for minutes.
--
-- Rows are single use: the claim is `DELETE ... RETURNING`, so a replayed
-- callback finds nothing. Abandoned rows are swept on the next issue, the same
-- cheap sweep `auth::store::store_oauth_request` performs.

CREATE TABLE connector_oauth_states (
    state         TEXT        PRIMARY KEY,
    user_id       UUID        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind          TEXT        NOT NULL,
    pkce_verifier TEXT        NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at    TIMESTAMPTZ NOT NULL
);

CREATE INDEX connector_oauth_states_user_id_idx ON connector_oauth_states (user_id);
CREATE INDEX connector_oauth_states_expires_at_idx ON connector_oauth_states (expires_at);

COMMENT ON TABLE connector_oauth_states IS
    'Single-use OAuth connect state, scoped to the signed-in user and the connector kind';

-- Token columns a provider grant needs beyond a single bearer.
--
-- `ciphertext` still holds the access token, sealed the same way, so C2's
-- per-account derivation and lazy `key_version` upgrade keep working untouched.
-- These two carry what Google hands back alongside it:
--
--   * `refresh_token_encrypted` — the refresh token, sealed with the same
--     per-user key (never stored in the clear, never logged);
--   * `expires_at` — when the access token stops being valid, so an expired one
--     is refreshed instead of surfacing as a provider 401.
--
-- Both are nullable and default to NULL, which is the shape every existing row
-- already has: the WhatsApp session blob has neither, and it keeps reading back
-- as `None`. `ADD COLUMN` with no default is metadata-only on Postgres 11+.
ALTER TABLE connector_secrets
    ADD COLUMN refresh_token_encrypted BYTEA,
    ADD COLUMN expires_at TIMESTAMPTZ;

COMMENT ON COLUMN connector_secrets.refresh_token_encrypted IS
    'Refresh token sealed under the same per-account key as ciphertext; NULL when the grant has none';
COMMENT ON COLUMN connector_secrets.expires_at IS
    'Expiry of the access token in ciphertext; NULL when it does not expire';
