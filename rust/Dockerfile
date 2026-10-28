# syntax=docker/dockerfile:1
FROM lukemathwalker/cargo-chef:latest-rust-1-bookworm AS chef
WORKDIR /app/rust

FROM chef AS planner
WORKDIR /app/rust
COPY rust/Cargo.toml rust/Cargo.lock ./
COPY rust/crates ./crates
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
WORKDIR /app/rust
# Cargo's job count. Unset (the default) keeps cargo's own default (all cores);
# a shared dev machine sets CARGO_BUILD_JOBS=2 (rust/deploy/compose.yaml does)
# so a release build does not starve everything else running on the host.
ARG CARGO_BUILD_JOBS
COPY --from=planner /app/rust/recipe.json recipe.json
# Build dependencies - this is the caching Docker layer
RUN cargo chef cook --release --recipe-path recipe.json
# Build application
COPY rust/Cargo.toml rust/Cargo.lock ./
COPY rust/crates ./crates
RUN cargo build --release --bin kb-server

# Stage to fetch libpdfium for the target architecture
FROM --platform=$BUILDPLATFORM debian:bookworm-slim AS pdfium
ARG TARGETARCH
WORKDIR /pdfium
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates tar gzip \
    && rm -rf /var/lib/apt/lists/*
COPY rust/scripts/fetch-pdfium.sh ./fetch-pdfium.sh
RUN TARGETARCH=${TARGETARCH} ./fetch-pdfium.sh /pdfium

# Slim runtime stage
FROM debian:bookworm-slim AS runtime
WORKDIR /app

# Surfaced by GET /health so an operator can tell which build is running
ARG KB_VERSION=dev
ENV KB_VERSION=${KB_VERSION}
ENV PDFIUM_DYNAMIC_LIB_PATH=/app/lib/libpdfium.so
ENV KB_MIGRATIONS_DIR=/app/prisma/migrations

# Ingest shells out to nothing, but PDFs and OCR benefit from fonts being present.
# curl is here for the container healthcheck (`GET /health`) and for an operator
# who wants to probe the service from inside the image.
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates fonts-liberation curl \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -u 10001 -m -s /bin/sh appuser

COPY --from=builder /app/rust/target/release/kb-server /usr/local/bin/kb-server
COPY --from=pdfium /pdfium/lib/libpdfium.so /app/lib/libpdfium.so
COPY prisma/migrations /app/prisma/migrations

# KB_CACHE_DIR lives here, and a named volume is mounted over it: creating the
# directory in the image is what gives a fresh volume appuser's ownership
# instead of root's (Docker copies the mountpoint's metadata from the image).
RUN mkdir -p /app/cache && chown -R appuser:appuser /app

USER appuser
EXPOSE 8080

# Migrations + vector schema are idempotent, so running them on boot keeps a
# single-command deploy honest. Set KB_SKIP_MIGRATE=1 to manage them yourself.
CMD ["sh", "-c", "if [ \"$KB_SKIP_MIGRATE\" != \"1\" ]; then kb-server migrate && kb-server apply-schema; fi; exec kb-server serve"]
