#!/usr/bin/env bash
# Bring up the kb-smoke stack and leave it running.
#
# Real Postgres 16 + SurrealDB v2 + S3 (rustfs) + an embedder (stub dim 8 or TEI dim 1024) +
# kb-server, on the ports smoke.sh uses. Idempotent: re-running it while the
# stack is up is a no-op that re-prints where things are.
#
#   rust/scripts/stack-up.sh        # start, or say "already up"
#   source /tmp/kb-stack/stack.env  # DATABASE_URL, S3_*, PORT, KB_SERVER_BIN…
#   rust/scripts/stack-down.sh      # stop everything this script started
#
# Opt-in modes:
#   KB_STACK_EMBEDDINGS=tei         # starts kb-smoke-tei-embed (Qwen3-Embedding-0.6B, dim 1024)
#   KB_STACK_RERANK=tei             # starts kb-smoke-tei-rerank (bge-reranker-v2-m3)
#   KB_STACK_QDRANT=1               # starts kb-smoke-qdrant (Qdrant v1.19.2; HTTP 6333, gRPC 6334)
#
# ONLY the `kb-smoke-*` containers belong to this stack. This machine runs
# other people's containers (ClickHouse, Postgres); `stack-down.sh` never names
# anything else, and neither should any script that sources stack.env.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$WORKSPACE_ROOT/.." && pwd)"

PG_CONTAINER="kb-smoke-pg"
SURREAL_CONTAINER="kb-smoke-surreal"
RUSTFS_CONTAINER="kb-smoke-rustfs"
TEI_EMBED_CONTAINER="kb-smoke-tei-embed"
TEI_RERANK_CONTAINER="kb-smoke-tei-rerank"
QDRANT_CONTAINER="kb-smoke-qdrant"

PG_PORT="${KB_STACK_PG_PORT:-15432}"
SURREAL_PORT="${KB_STACK_SURREAL_PORT:-18000}"
S3_PORT="${KB_STACK_S3_PORT:-19000}"
STUB_PORT="${KB_STACK_STUB_PORT:-19111}"
SERVER_PORT="${KB_STACK_SERVER_PORT:-18080}"
TEI_EMBED_PORT="${KB_STACK_TEI_EMBED_PORT:-18081}"
TEI_RERANK_PORT="${KB_STACK_TEI_RERANK_PORT:-18082}"
QDRANT_HTTP_PORT="${KB_STACK_QDRANT_HTTP_PORT:-16333}"
QDRANT_GRPC_PORT="${KB_STACK_QDRANT_GRPC_PORT:-16334}"

# Pinned. The Qdrant adapter (kb-store-qdrant) targets the Query API with
# `prefetch` + RRF fusion, which needs >= 1.10, and the tiered-multitenancy and
# per-tenant `idf` corpus-filter notes in docs/architecture-target.md §2 want
# 1.16 / 1.19 respectively. 1.19.2 is the current release (2026-10-16).
QDRANT_IMAGE="${KB_STACK_QDRANT_IMAGE:-qdrant/qdrant:v1.19.2}"

KB_STACK_EMBEDDINGS="${KB_STACK_EMBEDDINGS:-stub}"
KB_STACK_RERANK="${KB_STACK_RERANK:-none}"
KB_STACK_QDRANT="${KB_STACK_QDRANT:-0}"
KB_STACK_INGEST_POLL_MS="${KB_STACK_INGEST_POLL_MS:-500}"
KB_MODELS_CACHE="${KB_MODELS_CACHE:-$HOME/.cache/kb-models}"

STACK_DIR="${KB_STACK_DIR:-/tmp/kb-stack}"
STACK_ENV="$STACK_DIR/stack.env"
STUB_PID_FILE="$STACK_DIR/stub.pid"
SERVER_PID_FILE="$STACK_DIR/server.pid"
STUB_LOG="$STACK_DIR/stub.log"
SERVER_LOG="$STACK_DIR/server.log"

KB_SERVER_BIN="$WORKSPACE_ROOT/target/debug/kb-server"
STUB_BIN="$WORKSPACE_ROOT/target/debug/examples/stub_embeddings"

alive() { [ -n "${1:-}" ] && kill -0 "$1" 2>/dev/null; }

read_pid() { [ -f "$1" ] && cat "$1" || true; }

if alive "$(read_pid "$SERVER_PID_FILE")"; then
  echo "kb-smoke stack already up: kb-server on http://127.0.0.1:${SERVER_PORT} (pid $(read_pid "$SERVER_PID_FILE"))"
  echo "source $STACK_ENV   # then rust/scripts/stack-down.sh to stop it"
  exit 0
fi

mkdir -p "$STACK_DIR"
rm -f "$STUB_PID_FILE" "$SERVER_PID_FILE"

echo "Host memory status:"
free -h

echo "=== 1. Starting kb-smoke base containers ==="
# Only kb-smoke-* names. Nothing else on this host is ours.
docker rm -f "$PG_CONTAINER" "$SURREAL_CONTAINER" "$RUSTFS_CONTAINER" "$TEI_EMBED_CONTAINER" "$TEI_RERANK_CONTAINER" "$QDRANT_CONTAINER" >/dev/null 2>&1 || true

docker run -d --name "$PG_CONTAINER" \
  -p "${PG_PORT}:5432" \
  -e POSTGRES_USER=kb \
  -e POSTGRES_PASSWORD=kb \
  -e POSTGRES_DB=rantai_kb \
  postgres:16 >/dev/null

docker run -d --name "$SURREAL_CONTAINER" \
  -p "${SURREAL_PORT}:8000" \
  surrealdb/surrealdb:v2 start --user root --pass root memory >/dev/null

docker run -d --name "$RUSTFS_CONTAINER" \
  -p "${S3_PORT}:9000" \
  -e MINIO_ROOT_USER=kbadmin \
  -e MINIO_ROOT_PASSWORD=kbsecret123 \
  rustfs/rustfs:latest >/dev/null

echo "Waiting for Postgres to be ready..."
for _ in {1..60}; do
  if docker exec "$PG_CONTAINER" pg_isready -U kb >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

echo "Waiting for SurrealDB to be ready..."
for _ in {1..60}; do
  if curl -s "http://127.0.0.1:${SURREAL_PORT}/health" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

echo "Waiting for RustFS (S3) to be ready..."
for _ in {1..60}; do
  if curl -s "http://127.0.0.1:${S3_PORT}/minio/health/ready" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

echo "Creating S3 bucket rantai-kb..."
docker run --rm --network host \
  -e AWS_ACCESS_KEY_ID=kbadmin \
  -e AWS_SECRET_ACCESS_KEY=kbsecret123 \
  amazon/aws-cli:latest --endpoint-url "http://127.0.0.1:${S3_PORT}" s3 mb s3://rantai-kb >/dev/null

if [ "$KB_STACK_QDRANT" = "1" ]; then
  # Opt-in (docs/plan-best-multimodal-kb.md step L.2): Qdrant is the store the
  # engine will move to, but nothing is switched over to it in this round, so CI
  # never starts it and the default stack is unchanged.
  echo "Starting Qdrant (${QDRANT_IMAGE}) http ${QDRANT_HTTP_PORT} / grpc ${QDRANT_GRPC_PORT}..."
  docker run -d --name "$QDRANT_CONTAINER" \
    -p "${QDRANT_HTTP_PORT}:6333" \
    -p "${QDRANT_GRPC_PORT}:6334" \
    "$QDRANT_IMAGE" >/dev/null

  for _ in {1..60}; do
    if curl -s "http://127.0.0.1:${QDRANT_HTTP_PORT}/readyz" >/dev/null 2>&1; then
      break
    fi
    sleep 1
  done

  if ! curl -sf "http://127.0.0.1:${QDRANT_HTTP_PORT}/readyz" >/dev/null 2>&1; then
    echo "Qdrant did not come up ready; container logs:" >&2
    docker logs "$QDRANT_CONTAINER" | tail -n 30 >&2 || true
    exit 1
  fi
  echo "Qdrant ready on 127.0.0.1:${QDRANT_HTTP_PORT}"
fi

echo "=== 2. Building binaries (before memory-heavy models start) ==="
if [ "$KB_STACK_EMBEDDINGS" != "tei" ]; then
  [ -f "$STUB_BIN" ] || cargo build -p kb-server --example stub_embeddings --manifest-path "$WORKSPACE_ROOT/Cargo.toml"
fi
[ -f "$KB_SERVER_BIN" ] || cargo build -p kb-server --bin kb-server --manifest-path "$WORKSPACE_ROOT/Cargo.toml"

echo "=== 3. Starting models ==="
if [ "$KB_STACK_EMBEDDINGS" = "tei" ]; then
  echo "Starting TEI embeddings (Qwen/Qwen3-Embedding-0.6B) on port ${TEI_EMBED_PORT}..."
  docker run -d --name "$TEI_EMBED_CONTAINER" \
    -p "${TEI_EMBED_PORT}:80" \
    -v "${KB_MODELS_CACHE}:/data" \
    ghcr.io/huggingface/text-embeddings-inference:cpu-latest \
    --model-id Qwen/Qwen3-Embedding-0.6B --max-batch-tokens 4096 --max-client-batch-size 32 >/dev/null

  echo "Waiting for TEI embedding server to be ready (CPU warmup may take ~2 minutes)..."
  for _ in {1..300}; do
    if curl -s "http://127.0.0.1:${TEI_EMBED_PORT}/health" >/dev/null 2>&1; then
      break
    fi
    sleep 1
  done

  if ! curl -s "http://127.0.0.1:${TEI_EMBED_PORT}/health" >/dev/null 2>&1; then
    echo "TEI embedding server did not come up healthy; container logs:" >&2
    docker logs "$TEI_EMBED_CONTAINER" | tail -n 30 >&2 || true
    exit 1
  fi
  echo "TEI embedding server ready on 127.0.0.1:${TEI_EMBED_PORT}"
else
  echo "Starting stub embedding server on port ${STUB_PORT}..."
  if command -v tmux >/dev/null 2>&1; then
    tmux kill-session -t kb-smoke-stub 2>/dev/null || true
    tmux new-session -d -s kb-smoke-stub "bash -c 'echo \$\$ > \"$STUB_PID_FILE\" && exec \"$STUB_BIN\" \"127.0.0.1:${STUB_PORT}\" 8 > \"$STUB_LOG\" 2>&1'"
  else
    nohup "$STUB_BIN" "127.0.0.1:${STUB_PORT}" 8 > "$STUB_LOG" 2>&1 &
    echo $! > "$STUB_PID_FILE"
  fi

  for _ in {1..30}; do
    if curl -s "http://127.0.0.1:${STUB_PORT}/health" >/dev/null 2>&1; then
      break
    fi
    sleep 0.5
  done
  echo "stub embeddings on 127.0.0.1:${STUB_PORT} (pid $(cat "$STUB_PID_FILE"))"
fi

if [ "$KB_STACK_RERANK" = "tei" ]; then
  echo "Starting TEI reranker (BAAI/bge-reranker-v2-m3) on port ${TEI_RERANK_PORT}..."
  docker run -d --name "$TEI_RERANK_CONTAINER" \
    -p "${TEI_RERANK_PORT}:80" \
    -v "${KB_MODELS_CACHE}:/data" \
    ghcr.io/huggingface/text-embeddings-inference:cpu-latest \
    --model-id BAAI/bge-reranker-v2-m3 --max-batch-tokens 2048 --max-client-batch-size 64 --auto-truncate >/dev/null

  echo "Waiting for TEI reranker server to be ready (CPU warmup may take ~2 minutes)..."
  for _ in {1..300}; do
    if curl -s "http://127.0.0.1:${TEI_RERANK_PORT}/health" >/dev/null 2>&1; then
      break
    fi
    sleep 1
  done

  if ! curl -s "http://127.0.0.1:${TEI_RERANK_PORT}/health" >/dev/null 2>&1; then
    echo "TEI reranker server did not come up healthy; container logs:" >&2
    docker logs "$TEI_RERANK_CONTAINER" | tail -n 30 >&2 || true
    exit 1
  fi
  echo "TEI reranker server ready on 127.0.0.1:${TEI_RERANK_PORT}"
fi

# Every connection string the stack uses, in one file other scripts can source.
cat > "$STACK_ENV" <<EOF
# Generated by rust/scripts/stack-up.sh — source this, do not edit.
export DATABASE_URL="postgresql://kb:kb@127.0.0.1:${PG_PORT}/rantai_kb?schema=public"
export SURREAL_DB_URL="http://127.0.0.1:${SURREAL_PORT}"
export SURREAL_DB_USER="root"
export SURREAL_DB_PASS="root"
export SURREAL_DB_NAMESPACE="rantai"
export SURREAL_DB_DATABASE="knowledge"
export S3_ENDPOINT="http://127.0.0.1:${S3_PORT}"
export S3_ACCESS_KEY_ID="kbadmin"
export S3_SECRET_ACCESS_KEY="kbsecret123"
export S3_BUCKET="rantai-kb"
export S3_ENABLE_PATH_STYLE="1"
export KB_ENTITY_EXTRACTION_ENABLED="false"
export KB_WORKER_ENABLED="true"
export KB_INGEST_POLL_MS="${KB_STACK_INGEST_POLL_MS}"
export PORT="${SERVER_PORT}"
export KB_MIGRATIONS_DIR="$REPO_ROOT/prisma/migrations"
export RUST_LOG="info,kb_server=debug,kb_store_blob=debug,kb_engine=debug"
export KB_STACK_DIR="$STACK_DIR"
export KB_STACK_BASE_URL="http://127.0.0.1:${SERVER_PORT}"
export KB_SERVER_BIN="$KB_SERVER_BIN"
export KB_STACK_PG_CONTAINER="$PG_CONTAINER"
export KB_STACK_SURREAL_CONTAINER="$SURREAL_CONTAINER"
export KB_STACK_RUSTFS_CONTAINER="$RUSTFS_CONTAINER"
export KB_STACK_TEI_EMBED_CONTAINER="$TEI_EMBED_CONTAINER"
export KB_STACK_TEI_RERANK_CONTAINER="$TEI_RERANK_CONTAINER"
EOF

# QDRANT_URL is exported only when Qdrant was actually started, so a script that
# sources stack.env cannot be handed a URL that answers nothing.
if [ "$KB_STACK_QDRANT" = "1" ]; then
  cat >> "$STACK_ENV" <<EOF
export QDRANT_URL="http://127.0.0.1:${QDRANT_HTTP_PORT}"
export QDRANT_GRPC_URL="http://127.0.0.1:${QDRANT_GRPC_PORT}"
export KB_STACK_QDRANT_CONTAINER="$QDRANT_CONTAINER"
EOF
fi

if [ "$KB_STACK_EMBEDDINGS" = "tei" ]; then
  cat >> "$STACK_ENV" <<EOF
export KB_EMBEDDING_BASE_URL="http://127.0.0.1:${TEI_EMBED_PORT}/v1/embeddings"
export KB_EMBEDDING_API_KEY="local-tei"
export KB_EMBEDDING_MODEL="Qwen/Qwen3-Embedding-0.6B"
export KB_EMBEDDING_DIM="1024"
export KB_EMBEDDING_QUERY_PREFIX="Instruct: Given a question, retrieve passages that answer it
Query: "
export KB_EMBEDDING_PASSAGE_PREFIX=""
EOF
else
  cat >> "$STACK_ENV" <<EOF
export KB_EMBEDDING_BASE_URL="http://127.0.0.1:${STUB_PORT}/embeddings"
export KB_EMBEDDING_API_KEY="stub"
export KB_EMBEDDING_MODEL="stub"
export KB_EMBEDDING_DIM="8"
export KB_EMBEDDING_QUERY_PREFIX=""
export KB_EMBEDDING_PASSAGE_PREFIX=""
EOF
fi

if [ "$KB_STACK_RERANK" = "tei" ]; then
  cat >> "$STACK_ENV" <<EOF
export KB_RERANK_ENABLED="true"
export KB_RERANK_PROVIDER="tei"
export KB_RERANK_BASE_URL="http://127.0.0.1:${TEI_RERANK_PORT}"
export KB_RERANK_MODEL="BAAI/bge-reranker-v2-m3"
export KB_RERANK_API_KEY="unused-for-tei"
EOF
else
  cat >> "$STACK_ENV" <<EOF
export KB_RERANK_ENABLED="false"
EOF
fi

# shellcheck disable=SC1090
source "$STACK_ENV"

echo "=== 4. Migrations and vector schema ==="
"$KB_SERVER_BIN" migrate | tail -n 1
"$KB_SERVER_BIN" apply-schema | tail -n 1

echo "=== 5. Starting kb-server ==="
if command -v tmux >/dev/null 2>&1; then
  tmux kill-session -t kb-smoke-server 2>/dev/null || true
  tmux new-session -d -s kb-smoke-server "bash -c 'source \"$STACK_ENV\" && echo \$\$ > \"$SERVER_PID_FILE\" && exec \"$KB_SERVER_BIN\" serve > \"$SERVER_LOG\" 2>&1'"
else
  nohup "$KB_SERVER_BIN" serve > "$SERVER_LOG" 2>&1 &
  echo $! > "$SERVER_PID_FILE"
fi

for _ in {1..60}; do
  if curl -s "http://127.0.0.1:${SERVER_PORT}/health" >/dev/null 2>&1; then
    break
  fi
  sleep 0.5
done

if ! curl -s "http://127.0.0.1:${SERVER_PORT}/health" | grep -q '"ok":true'; then
  echo "kb-server did not come up healthy; last lines of $SERVER_LOG:" >&2
  tail -n 40 "$SERVER_LOG" >&2 || true
  exit 1
fi

echo ""
echo "kb-smoke stack is up."
echo "  kb-server   http://127.0.0.1:${SERVER_PORT}  (pid $(cat "$SERVER_PID_FILE"), log $SERVER_LOG)"
echo "  postgres    127.0.0.1:${PG_PORT}   surreal 127.0.0.1:${SURREAL_PORT}   s3 127.0.0.1:${S3_PORT}"
if [ "$KB_STACK_EMBEDDINGS" = "tei" ]; then
  echo "  tei-embed   http://127.0.0.1:${TEI_EMBED_PORT} (dim 1024, Qwen3-Embedding-0.6B)"
else
  echo "  stub-embed  http://127.0.0.1:${STUB_PORT} (dim 8)"
fi
if [ "$KB_STACK_RERANK" = "tei" ]; then
  echo "  tei-rerank  http://127.0.0.1:${TEI_RERANK_PORT} (bge-reranker-v2-m3)"
fi
if [ "$KB_STACK_QDRANT" = "1" ]; then
  echo "  qdrant      http://127.0.0.1:${QDRANT_HTTP_PORT} (grpc 127.0.0.1:${QDRANT_GRPC_PORT}, ${QDRANT_IMAGE})"
fi
echo "  source $STACK_ENV"
echo "  $SCRIPT_DIR/stack-down.sh   # stop"
