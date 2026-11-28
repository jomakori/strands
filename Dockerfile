# syntax=docker/dockerfile:1.7

ARG RUST_VERSION=1.98
ARG DIOXUS_CLI_VERSION=0.7.10

# allow: dx links against glibc 2.39, so builder and runtime stay on trixie; bookworm fails with `GLIBC_2.39 not found`
FROM rust:${RUST_VERSION}-trixie AS build

SHELL ["/bin/bash", "-c"]

RUN rustup target add wasm32-unknown-unknown

# allow: a global ARG is not visible inside a stage, so it is re-declared before the RUN that reads it
ARG TARGETARCH
ARG DIOXUS_CLI_VERSION
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

# allow: .git is outside the build context, so the commit is passed in and reported by /healthz
ARG GIT_COMMIT=unknown
ENV STRANDS_GIT_COMMIT=${GIT_COMMIT}

RUN cargo build --release --locked --package strands-server

RUN dx build --package strands-web --platform web --release \
 && PUBLIC_DIR=$(find target/dx -type d -path '*/release/web/public' | head -n1) \
 && test -n "${PUBLIC_DIR}" \
 && test -f "${PUBLIC_DIR}/index.html" \
 && mkdir -p /src/dist \
 && cp -R "${PUBLIC_DIR}/." /src/dist/

FROM debian:trixie-slim AS runtime

RUN apt-get update \
 && apt-get install --yes --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*

RUN useradd --uid 10001 --user-group --home-dir /app --shell /usr/sbin/nologin strands

WORKDIR /app

COPY --from=build /src/target/release/strands-server /usr/local/bin/strands-server
COPY --from=build --chown=strands:strands /src/dist /app/dist

ENV STRANDS_ASSETS_DIR=/app/dist

EXPOSE 8080
USER 10001:10001

ENTRYPOINT ["/usr/local/bin/strands-server"]
