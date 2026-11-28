# Google Photos connector

How the connector is wired, as built: the OAuth client Google needs, the one
redirect URI to register for it, the scope the code asks for, and the paths that
refuse on purpose.

## Connection model: Picker only

Photos arrive **only through a pick**. There is no library listing, and no
scope that would grant one — the connector cannot enumerate anything the user
did not choose in Google's Picker UI.

- The scope list says so in one line: "the Picker API is the only photos path
  (no library listing)" (`crates/server/src/connectors/photos.rs:30-33`).
- The Picker client restates it: "There is no way to read a library directly"
  (`crates/connector-photos/src/picker.rs:1-8`).
- The Pick is three requests: create a session, the user picks in Google's UI,
  poll until the items are ready, then read what was picked
  (`crates/server/src/photos/mod.rs:1-20`).

The full round trip:

1. `GET /api/v1/connectors/google_photos/connect` mints a state row (single use,
   carrying the PKCE verifier) and answers `302` to Google's consent screen
   (`crates/server/src/connectors/photos_connect.rs:277-305`).
2. `GET /api/v1/connectors/google_photos/callback` claims the state, swaps the
   code (with the verifier) at Google's token endpoint, seals the access token
   under the account's key, and marks the connector row connected
   (`crates/server/src/connectors/photos_connect.rs:307-384`).
3. `POST /api/v1/connectors/photos/sessions` opens a pick with that token;
   `GET /api/v1/connectors/photos/sessions/{session_id}` reports where the pick
   stands and imports the picked items as notes when they are ready
   (`crates/server/src/photos/mod.rs:62-66`, `:84-92`).
4. The web client only ever calls those two session routes — it has no other
   photos path (`crates/web/src/api.rs:262-278`).

Tokens live in `connector_secrets`: `ciphertext` holds the sealed access token,
and the same per-account key seals `refresh_token_encrypted`, with `expires_at`
recording when the access token dies (`migrations/0014_connector_oauth_states.sql:30-47`).

## Redirect URI to register

Register this exact string on the Google OAuth client — byte for byte, no
trailing slash:

```
https://strands.maklab.net/api/v1/connectors/google_photos/callback
```

It is not a constant. The callback path is fixed in code —
`{PUBLIC_BASE_URL}/api/v1/connectors/google_photos/callback`
(`crates/server/src/connectors/photos_connect.rs:87-104`) — and
`PUBLIC_BASE_URL` is the deploy's public origin, no trailing slash
(`crates/server/src/config.rs:71-72`). Production's host is `strands`
(`gke_GitOps` `apps/helm/values.yaml:264`) under the cluster domain `maklab.net`
(`gke_GitOps` `apps/helm/templates/_helpers.tpl:22`), so the string above is what
the production deploy sends.

`GOOGLE_OAUTH_REDIRECT_URI` overrides the derived value
(`crates/server/src/connectors/photos_connect.rs:37-38`, `:90`). If it is set,
it must be the same string: the consent redirect and the token exchange both use
the single value the process holds, and Google rejects the exchange when the two
differ (`crates/server/src/connectors/photos_connect.rs:110-140`).

Staging (`staging-strands.maklab.net`) derives the same path but runs with no
Google client, since staging has no accounts.

## Scope the code requests

Exactly one:

```
https://www.googleapis.com/auth/photospicker.mediaitems.readonly
```

The single source is the connector's `SCOPES` (`crates/server/src/connectors/photos.rs:30-33`);
the Picker crate carries the same value as `PHOTOS_SCOPE`
(`crates/connector-photos/src/picker.rs:14-15`). The consent URL joins that list
with a space into `scope=` and sends it with `response_type=code`,
`code_challenge_method=S256`, `access_type=offline` and `prompt=consent`
(`crates/server/src/connectors/photos_connect.rs:110-125`). The same list is
written to `connectors.scopes` on a successful connect, so the row states what
the token allows (`crates/server/src/connectors/photos_connect.rs:368-384`).

## Preview and import answer 503 by design

`GET /api/v1/connectors/{id}/preview` and `POST /api/v1/connectors/{id}/import`
are the generic connector path, and for Photos both refuse:

- `preview()` returns `ConnectorError("Google Photos only imports through a pick;
  there is nothing to preview")` (`crates/server/src/connectors/photos.rs:127-140`).
- `import()` returns the same class of error — "nothing to sync"
  (`crates/server/src/connectors/photos.rs:142-150`).

A `ConnectorError` maps to `AppError::Unavailable` (`crates/server/src/error.rs:91-100`),
which is `503 Service Unavailable` (`crates/server/src/error.rs:131`). The
refusal is deliberate: the pick *is* the preview and the import, in one round
trip, so the generic path has nothing truthful to return. A `503` here is not a
missing token, not a misconfiguration, and not something a reconnect fixes.

The UI does not take that path: the Photos row starts a pick and polls the
session routes instead (`crates/web/src/photos_pick.rs:11-20`), so the `503` is
reachable only by calling the generic endpoints directly.

## Credentials the deploy supplies

- `GOOGLE_CLIENT_ID` and `GOOGLE_CLIENT_SECRET`
  (`crates/server/src/connectors/photos_connect.rs:33-39`). This is a
  confidential client: the secret is sent to Google's token endpoint beside the
  PKCE verifier (`:130-140`).
- No client id means no connect route: `from_env` returns `None` and the route
  reports that instead of redirecting to a client that does not exist
  (`crates/server/src/connectors/photos_connect.rs:84-104`, `:356-366`).

## Refresh and disconnect

- Every Photos request reads its token through one function; an expired token is
  refreshed once and re-sealed before use, and a revoked grant is reported as
  "reconnect required" rather than `503`
  (`crates/server/src/connectors/photos_refresh.rs:1-46`).
- Disconnect revokes the grant, deletes the sealed token row and marks the
  connector disconnected in one transaction; a second call is a no-op
  (`crates/server/src/connectors/photos.rs:152-193`).
