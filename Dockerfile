# syntax=docker/dockerfile:1.7

# The whole deployable is this image (spec §6): one process answers `/api` and
# serves the wasm bundle at the site root. There is no second web tier, so the
# bundle and the API can never disagree about which release is live.
#
# Two build stages in one builder: `dx` produces the browser bundle, `cargo`
# produces the host binary, and both share the registry cache. The runtime
# stage carries only the binary, the bundle, and a trust store.

ARG RUST_VERSION=1.98
ARG DIOXUS_CLI_VERSION=0.7.10

# ---- build -----------------------------------------------------------------
FROM rust:${RUST_VERSION}-bookworm AS build

# The web crate only compiles for the browser target.
RUN rustup target add wasm32-unknown-unknown

# `dx` is taken as a prebuilt release binary rather than `cargo install`ed:
# compiling it costs minutes per architecture (much more under QEMU for the
# arm64 half) and would need OpenSSL headers in the builder for nothing. The
# release tarball is the same binary, pinned to the version lint-test.yml uses.
ARG TARGETARCH
RUN <<EOF
set -euo pipefail
case "${TARGETARCH}" in
  amd64) TRIPLE=x86_64-unknown-linux-gnu ;;
  arm64) TRIPLE=aarch64-unknown-linux-gnu ;;
  *) echo "no dx release for TARGETARCH=${TARGETARCH}" >&2; exit 1 ;;
esac
curl -fsSL \
  "https://github.com/DioxusLabs/dioxus/releases/download/v${DIOXUS_CLI_VERSION}/dx-${TRIPLE}.tar.gz" \
  | tar -xz -C /usr/local/bin dx
dx --version
EOF

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY migrations ./migrations

# The commit this binary claims to be. It comes in as a build argument because
# `.git` is deliberately not part of the context (see .dockerignore), and
# `/healthz` reports it so a rollout can be checked against the tag deployed.
ARG GIT_COMMIT=unknown
ENV STRANDS_GIT_COMMIT=${GIT_COMMIT}

RUN cargo build --release --locked --package strands-server

# `dx` writes the bundle under a path it derives from the package name, so it
# is located rather than assumed — the same way lint-test.yml finds it.
RUN dx build --package strands-web --platform web --release \
 && PUBLIC_DIR=$(find target/dx -type d -path '*/release/web/public' | head -n1) \
 && test -n "${PUBLIC_DIR}" \
 && test -f "${PUBLIC_DIR}/index.html" \
 && mkdir -p /src/dist \
 && cp -R "${PUBLIC_DIR}/." /src/dist/

# ---- runtime ---------------------------------------------------------------
# Bookworm slim rather than distroless: the binary is glibc-linked and the only
# thing it needs at runtime is a CA store for outbound HTTPS (rustls, so no
# OpenSSL). A shell is kept for `kubectl exec`, which is worth more than the
# handful of megabytes distroless would save.
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
 && apt-get install --yes --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*

# A fixed, non-root uid: the cluster's securityContext can assert it, and the
# process cannot write to its own code.
RUN useradd --uid 10001 --user-group --home-dir /app --shell /usr/sbin/nologin strands

WORKDIR /app

# Migrations are embedded in the binary (`sqlx::migrate!("../../migrations")` in
# db.rs), so the schema travels inside the executable and the runtime image
# ships no `migrations/` directory.
COPY --from=build /src/target/release/strands-server /usr/local/bin/strands-server
COPY --from=build --chown=strands:strands /src/dist /app/dist

ENV STRANDS_ASSETS_DIR=/app/dist

EXPOSE 8080
USER 10001:10001

ENTRYPOINT ["/usr/local/bin/strands-server"]
