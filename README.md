<div align="center">
  <img src="docs/media/banner.svg" alt="Strands — notes by tag" width="880">

  <p align="center">
    <img src="https://img.shields.io/github/actions/workflow/status/jomakori/strands/lint-test.yml?logo=githubactions&logoColor=white&label=CI" alt="CI">
    <img src="https://img.shields.io/github/license/jomakori/strands?logo=opensourceinitiative&logoColor=white&label=License" alt="License">
    <img src="https://img.shields.io/github/stars/jomakori/strands?logo=github&logoColor=white&label=Stars" alt="Stars">
    <img src="https://img.shields.io/github/last-commit/jomakori/strands?logo=git&logoColor=white&label=Last%20commit" alt="Last commit">
  </p>
</div>

## Quick Links

- [What is it?](#what-is-it) · [Features](#features) · [Layout](#layout) · [Quickstart](#quickstart) ·
  [Development](#development) · [Contributing](#contributing)

## What is it?

Strands is a notes-by-tag app in pure Rust: a Dioxus 0.7 WASM SPA on mobile and
desktop, talking to one self-hosted axum service that owns the store. Tags are the
navigation — a note is anything captured with a source attached.

## Features

- **Notes first** — capture text, links, photos, places; the store is the source of truth
- **Tag grid** — home is the tag surface, not a feed
- **Capture with sources** — every note records where it came from
- **Connectors** — plugin seam for new sources (Google Photos, WhatsApp, Maps in v1)
- **Server-owned** — one Rust service, Postgres via sqlx, no client-side database

## Layout

- `crates/core` — domain types (`Note`, `Tag`, `Source`), no IO, no framework deps
- `crates/server` — axum binary, serves the API and the SPA bundle (`GET /healthz`, Google OAuth login, session-backed `/api/v1/*`)
- `crates/web` — Dioxus 0.7 web-only SPA (no SSR), built to WASM with `dx build`

## Quickstart

```sh
cargo run -p strands-server
curl http://localhost:8080/healthz   # => {"status":"ok"}
```

The SPA needs the dioxus CLI:

```sh
cargo install dioxus-cli --version 0.7.10 --locked
dx build --package strands-web --platform web --release
```

## Configuration

The server refuses to boot without its auth credentials rather than starting up
half-configured: `main` exits with a clear message instead of failing on the
first login.

| Variable | Required | Default | Meaning |
|---|---|---|---|
| `DATABASE_URL` | yes | — | Postgres URL; migrations run on boot |
| `GOOGLE_CLIENT_ID` | yes | — | OAuth client id from the Google Cloud console |
| `GOOGLE_CLIENT_SECRET` | yes | — | OAuth client secret. **Never** commit it; supply it from the environment or the secret store (Doppler → ExternalSecret → env) |
| `PUBLIC_BASE_URL` | no | `http://localhost:8080` | Public origin, no trailing slash. The redirect URI is `${PUBLIC_BASE_URL}/auth/google/callback` and must be registered byte for byte in the Google console |
| `SESSION_COOKIE_SECURE` | no | `true` | `Secure` on the session cookie. Set `false` only for a local plain-HTTP run |
| `SESSION_TTL_DAYS` | no | `30` | Session lifetime, matching spec §6 |
| `GOOGLE_AUTHORIZATION_ENDPOINT` / `GOOGLE_TOKEN_ENDPOINT` | no | Google's | Overrides, for a fake provider or a test double (endpoints are strings, so nothing here needs Google at build time) |
| `STRANDS_TEST_REQUIRE_DB` | no | unset | Test-only. When `1`, an unreachable `DATABASE_URL` fails a test instead of skipping; the CI `schema` job sets it so a broken service container cannot pass as "skipped" |

```sh
export DATABASE_URL=postgres://strands:strands@localhost/strands
export GOOGLE_CLIENT_ID=...apps.googleusercontent.com
export GOOGLE_CLIENT_SECRET=...            # from the secret store, not a file in the repo
export PUBLIC_BASE_URL=http://localhost:8080
export SESSION_COOKIE_SECURE=false         # local HTTP only
cargo run -p strands-server
```

### Auth flow

```
GET  /auth/google           → 303 to Google's consent screen (state + PKCE S256, stored server-side)
GET  /auth/google/callback  → exchange the code, upsert the user, seed 5 categories,
                              set the session cookie, 303 to /
POST /auth/logout           → 204, session row deleted, cookie cleared
```

The cookie (`strands_session`) carries only the opaque `sessions.id` — no access
token, no email, nothing a client could replay at Google. It is `HttpOnly;
SameSite=Lax` (the provider redirect is cross-site, so `Strict` would drop it on
the way back) and `Secure` unless explicitly disabled.

Handlers never parse a user id out of the request: they take the `CurrentUser`
extractor (`crates/server/src/auth/extractor.rs`), which resolves the cookie to a
live session row and rejects everything else with `401`. Every query takes that
id as a bound parameter, so another user's object id is simply a row that does
not exist — `404`, not `403`.

## Development

### Summary review

A generated summary is a *proposal*: it is committed or dropped, never silently
applied. The approved version is the tag's summary, and it is the only thing the
card shows as such.

```
POST /api/v1/tags/{id}/summary             → { "version": 1, "state": "proposed", ... }
GET  /api/v1/tags/{id}/summary/history     → { "versions": [...], "approved_version_id": ... }
POST /api/v1/tags/{id}/summary/{v}/approve → 200 with the new history (commits {v})
POST /api/v1/tags/{id}/summary/{v}/discard → 200 with the new history (drops {v})
POST /api/v1/tags/{id}/summary/{v}/edit    → { "body": "..." }, records a new `edited` version
```

An edit adds a row; it never rewrites the generated one, so the history always
shows what the model said and what a person changed it to. The card binds
`A` (approve), `D` (discard) and `H` (history) while it is open, and ignores
them whenever a text field has focus.

The toolchain is pinned in `rust-toolchain.toml` (stable plus the
`wasm32-unknown-unknown` target); `rustup` installs it on first use in this directory.

```sh
cargo fmt --all --check
cargo clippy --workspace --exclude strands-web --all-targets -- -D warnings
cargo test --workspace --exclude strands-web
```

`strands-web` has no native target, so the `lint-test` workflow builds and gates it:
the `wasm-build` job reports the release bundle size as an artifact and fails above
5 MiB.

`pre-commit install` wires the same fmt/clippy gate into git.

## Contributing

Pull requests follow `.github/pull_request_template.md` and conventional commit
messages. CI must be green before merge; UI changes carry visuals in the PR body.
