#!/usr/bin/env bash
# Mixed fleet test for rantai-kb:
# Test 1: Upload via Bun (API only, KB_WORKER_ENABLED=false) -> Ingest via Rust worker (KB_WORKER_ENABLED=true)
# Test 2: Upload via Rust (API only, KB_WORKER_ENABLED=false) -> Ingest via Bun worker (KB_WORKER_ENABLED=true)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$WORKSPACE_ROOT/.." && pwd)"

PG_CONTAINER="kb-mixed-pg"
SURREAL_CONTAINER="kb-mixed-surreal"
RUSTFS_CONTAINER="kb-mixed-rustfs"

PG_PORT="15433"
SURREAL_PORT="18001"
S3_PORT="19001"
STUB_PORT="19112"
RUST_PORT="18080"
BUN_PORT="18081"

TMP_DIR="$(mktemp -d -t kb-mixed-XXXXXX)"
BUN_LOG="$TMP_DIR/bun.log"
RUST_LOG="$TMP_DIR/rust.log"
STUB_LOG="$TMP_DIR/stub.log"

STUB_PID=""
RUST_PID=""
BUN_PID=""

cleanup() {
  echo ""
  echo "--- Cleaning up mixed-fleet resources ---"
  if [ -n "$BUN_PID" ] && kill -0 "$BUN_PID" 2>/dev/null; then
    kill "$BUN_PID" 2>/dev/null || true
  fi
  if [ -n "$RUST_PID" ] && kill -0 "$RUST_PID" 2>/dev/null; then
    kill "$RUST_PID" 2>/dev/null || true
  fi
  if [ -n "$STUB_PID" ] && kill -0 "$STUB_PID" 2>/dev/null; then
    kill "$STUB_PID" 2>/dev/null || true
  fi
  docker rm -f "$PG_CONTAINER" "$SURREAL_CONTAINER" "$RUSTFS_CONTAINER" 2>/dev/null || true
  rm -rf "$TMP_DIR"
  echo "--- Mixed-fleet cleanup complete ---"
}
trap cleanup EXIT

echo "=== 1. Starting containers ==="
docker rm -f "$PG_CONTAINER" "$SURREAL_CONTAINER" "$RUSTFS_CONTAINER" 2>/dev/null || true

docker run -d --name "$PG_CONTAINER" \
  -p "${PG_PORT}:5432" \
  -e POSTGRES_USER=kb \
  -e POSTGRES_PASSWORD=kb \
  -e POSTGRES_DB=rantai_kb \
  postgres:16

docker run -d --name "$SURREAL_CONTAINER" \
  -p "${SURREAL_PORT}:8000" \
  surrealdb/surrealdb:v2 start --user root --pass root memory

docker run -d --name "$RUSTFS_CONTAINER" \
  -p "${S3_PORT}:9000" \
  -e MINIO_ROOT_USER=kbadmin \
  -e MINIO_ROOT_PASSWORD=kbsecret123 \
  rustfs/rustfs:latest

echo "Waiting for services..."
for _ in {1..30}; do
  if docker exec "$PG_CONTAINER" pg_isready -U kb >/dev/null 2>&1; then break; fi
  sleep 1
done
for _ in {1..30}; do
  if curl -s "http://127.0.0.1:${SURREAL_PORT}/health" >/dev/null 2>&1; then break; fi
  sleep 1
done
for _ in {1..30}; do
  if curl -s "http://127.0.0.1:${S3_PORT}/minio/health/ready" >/dev/null 2>&1; then break; fi
  sleep 1
done

docker run --rm --network host \
  -e AWS_ACCESS_KEY_ID=kbadmin \
  -e AWS_SECRET_ACCESS_KEY=kbsecret123 \
  amazon/aws-cli:latest --endpoint-url "http://127.0.0.1:${S3_PORT}" s3 mb s3://rantai-kb

echo "=== 2. Starting stub embeddings ==="
STUB_BIN="$WORKSPACE_ROOT/target/debug/examples/stub_embeddings"
"$STUB_BIN" "127.0.0.1:${STUB_PORT}" 8 > "$STUB_LOG" 2>&1 &
STUB_PID=$!
for _ in {1..20}; do
  if curl -s "http://127.0.0.1:${STUB_PORT}/health" >/dev/null 2>&1; then break; fi
  sleep 0.5
done

# Shared Environment
export DATABASE_URL="postgresql://kb:kb@127.0.0.1:${PG_PORT}/rantai_kb?schema=public"
export SURREAL_DB_URL="ws://127.0.0.1:${SURREAL_PORT}/rpc"
export SURREAL_DB_USER="root"
export SURREAL_DB_PASS="root"
export SURREAL_DB_NAMESPACE="rantai"
export SURREAL_DB_DATABASE="knowledge"
export S3_ENDPOINT="http://127.0.0.1:${S3_PORT}"
export S3_ACCESS_KEY_ID="kbadmin"
export S3_SECRET_ACCESS_KEY="kbsecret123"
export S3_BUCKET="rantai-kb"
export S3_ENABLE_PATH_STYLE="1"
export KB_EMBEDDING_BASE_URL="http://127.0.0.1:${STUB_PORT}/embeddings"
export KB_EMBEDDING_API_KEY="stub"
export KB_EMBEDDING_MODEL="stub"
export KB_EMBEDDING_DIM="8"
export KB_ENTITY_EXTRACTION_ENABLED="false"
export KB_MIGRATIONS_DIR="$REPO_ROOT/prisma/migrations"

KB_SERVER_BIN="$WORKSPACE_ROOT/target/debug/kb-server"

echo "=== 3. Schema & Keys ==="

# Bun's migration path runs FIRST, on its own, against the fresh database. This is
# the assertion that the Postgres schema stays additive and Bun-safe: Prisma's own
# engine has to accept every migration file, including the entity-graph tables
# (20261017120000_graph_tables), from a database that has never been migrated.
# A migration that touched an existing table, or that Prisma could not apply, fails
# here rather than in production with Bun half-upgraded.
echo "--- Bun: prisma migrate deploy (from empty)"
# The repo has a .env with its own DATABASE_URL, and this must not deploy anything
# anywhere but the throwaway container above. System environment variables take
# precedence over .env in the Prisma CLI, but "relies on that" is not a guard: if
# the exported URL is not the container's, stop before Prisma connects.
case "$DATABASE_URL" in
  *"127.0.0.1:${PG_PORT}"*) ;;
  *) echo "FAIL: DATABASE_URL is not the mixed-fleet container (port ${PG_PORT}). Refusing to run prisma."; exit 1 ;;
esac
PRISMA_OUT=$(cd "$REPO_ROOT" && bunx prisma migrate deploy 2>&1) || {
  echo "FAIL: prisma migrate deploy failed:"; echo "$PRISMA_OUT"; exit 1;
}
echo "$PRISMA_OUT" | grep -E "Applying migration|migrations found" | sed 's/^/    /'
echo "$PRISMA_OUT" | grep -q "20261017120000_graph_tables" || {
  echo "FAIL: Prisma did not apply the entity-graph migration."; echo "$PRISMA_OUT"; exit 1;
}

# The four graph tables exist, and they are the only thing the new migration added.
GRAPH_TABLES=$(docker exec "$PG_CONTAINER" psql -U kb -d rantai_kb -t -A -c \
  "SELECT count(*) FROM information_schema.tables
    WHERE table_schema='public'
      AND table_name IN ('KbEntity','KbMention','KbRelation','KbGraphVersion')")
[ "$GRAPH_TABLES" = "4" ] || { echo "FAIL: expected 4 graph tables, found $GRAPH_TABLES"; exit 1; }
echo "    graph tables: 4/4 (KbEntity, KbMention, KbRelation, KbGraphVersion)"

# Rust's migrator now runs against the database Prisma just migrated. It has to read
# Prisma's rows in _prisma_migrations and skip every one of them: one ledger, two
# runtimes. If it applies anything, the two engines disagree about what is pending,
# which is exactly the state where a rolling deploy diverges.
echo "--- Rust: kb-server migrate (against Prisma's ledger)"
MIGRATE_OUT=$(SURREAL_DB_URL="http://127.0.0.1:${SURREAL_PORT}" "$KB_SERVER_BIN" migrate 2>&1) || {
  echo "FAIL: kb-server migrate failed:"; echo "$MIGRATE_OUT"; exit 1;
}
echo "$MIGRATE_OUT" | tail -n 1 | sed 's/^/    /'
if echo "$MIGRATE_OUT" | grep -q "^applied:"; then
  echo "FAIL: the Rust migrator applied a migration Prisma had already applied:"
  echo "$MIGRATE_OUT" | grep "^applied:" | sed 's/^/    /'
  exit 1
fi
echo "    Rust applied 0 migrations: Prisma's ledger is authoritative and shared."

SURREAL_DB_URL="http://127.0.0.1:${SURREAL_PORT}" "$KB_SERVER_BIN" apply-schema

KEY_OUTPUT=$("$KB_SERVER_BIN" create-api-key "mixed-tenant" "mixed-key" --scopes kb:read,kb:write)
API_KEY=$(echo "$KEY_OUTPUT" | grep -o 'rkb_[a-zA-Z0-9_-]*' | head -n 1)
echo "API Key: $API_KEY"

CSV_FILE="$TMP_DIR/sample.csv"
cat << 'EOF' > "$CSV_FILE"
Item,Count
Alpha,42
Beta,99
EOF

echo "================================================================="
echo "=== TEST 1: Upload via Bun (API only) -> Ingest via Rust worker =="
echo "================================================================="

# Start Bun on port 18081, worker disabled
PORT="${BUN_PORT}" KB_WORKER_ENABLED="false" bun "$REPO_ROOT/src/server.ts" > "$BUN_LOG" 2>&1 &
BUN_PID=$!

# Start Rust on port 18080, worker enabled
PORT="${RUST_PORT}" KB_WORKER_ENABLED="true" SURREAL_DB_URL="http://127.0.0.1:${SURREAL_PORT}" "$KB_SERVER_BIN" serve > "$RUST_LOG" 2>&1 &
RUST_PID=$!

for _ in {1..30}; do
  if curl -s "http://127.0.0.1:${BUN_PORT}/health" >/dev/null 2>&1 && curl -s "http://127.0.0.1:${RUST_PORT}/health" >/dev/null 2>&1; then
    break
  fi
  sleep 0.5
done
echo "Both Bun (API, :${BUN_PORT}) and Rust (Worker, :${RUST_PORT}) are healthy"

# Upload to Bun
UPLOAD1_RESP=$(curl -s -X POST \
  -H "Authorization: Bearer $API_KEY" \
  -F "file=@$CSV_FILE;type=text/csv" \
  -F "title=Bun Upload Doc" \
  "http://127.0.0.1:${BUN_PORT}/v1/documents")
echo "Bun upload response: $UPLOAD1_RESP"
JOB1_ID=$(echo "$UPLOAD1_RESP" | jq -r '.jobId')
DOC1_ID=$(echo "$UPLOAD1_RESP" | jq -r '.id')

# Wait for Rust worker to process it
echo "Waiting for Rust worker to ingest job $JOB1_ID..."
for i in {1..60}; do
  STATUS_RESP=$(curl -s -H "Authorization: Bearer $API_KEY" "http://127.0.0.1:${RUST_PORT}/v1/jobs/$JOB1_ID")
  STATUS=$(echo "$STATUS_RESP" | jq -r '.status')
  if [ "$STATUS" = "success" ]; then
    echo "Job completed successfully by Rust worker in iteration $i!"
    break
  fi
  sleep 0.5
done
test "$STATUS" = "success"

# Verify search works on both services
SEARCH_BUN=$(curl -s -X POST -H "Authorization: Bearer $API_KEY" -H "Content-Type: application/json" \
  -d '{"query": "Alpha Count"}' "http://127.0.0.1:${BUN_PORT}/v1/search")
echo "Search on Bun found chunks: $(echo "$SEARCH_BUN" | jq '.chunks | length')"
test "$(echo "$SEARCH_BUN" | jq '.chunks | length')" -gt 0

SEARCH_RUST=$(curl -s -X POST -H "Authorization: Bearer $API_KEY" -H "Content-Type: application/json" \
  -d '{"query": "Alpha Count"}' "http://127.0.0.1:${RUST_PORT}/v1/search")
echo "Search on Rust found chunks: $(echo "$SEARCH_RUST" | jq '.chunks | length')"
test "$(echo "$SEARCH_RUST" | jq '.chunks | length')" -gt 0
echo "✓ Test 1 Passed: Bun upload -> Rust ingest -> Search on both works!"

# Stop instances
kill "$BUN_PID" "$RUST_PID" 2>/dev/null || true
wait "$BUN_PID" "$RUST_PID" 2>/dev/null || true
BUN_PID=""
RUST_PID=""
sleep 1

echo "================================================================="
echo "=== TEST 2: Upload via Rust (API only) -> Ingest via Bun worker =="
echo "================================================================="

# Start Rust on port 18080, worker disabled
PORT="${RUST_PORT}" KB_WORKER_ENABLED="false" SURREAL_DB_URL="http://127.0.0.1:${SURREAL_PORT}" "$KB_SERVER_BIN" serve > "$RUST_LOG" 2>&1 &
RUST_PID=$!

# Start Bun on port 18081, worker enabled
PORT="${BUN_PORT}" KB_WORKER_ENABLED="true" bun "$REPO_ROOT/src/server.ts" > "$BUN_LOG" 2>&1 &
BUN_PID=$!

for _ in {1..30}; do
  if curl -s "http://127.0.0.1:${BUN_PORT}/health" >/dev/null 2>&1 && curl -s "http://127.0.0.1:${RUST_PORT}/health" >/dev/null 2>&1; then
    break
  fi
  sleep 0.5
done
echo "Both Rust (API, :${RUST_PORT}) and Bun (Worker, :${BUN_PORT}) are healthy"

# Upload to Rust
UPLOAD2_RESP=$(curl -s -X POST \
  -H "Authorization: Bearer $API_KEY" \
  -F "file=@$CSV_FILE;type=text/csv" \
  -F "title=Rust Upload Doc" \
  "http://127.0.0.1:${RUST_PORT}/v1/documents")
echo "Rust upload response: $UPLOAD2_RESP"
JOB2_ID=$(echo "$UPLOAD2_RESP" | jq -r '.jobId')
DOC2_ID=$(echo "$UPLOAD2_RESP" | jq -r '.id')

# Wait for Bun worker to process it
echo "Waiting for Bun worker to ingest job $JOB2_ID..."
for i in {1..60}; do
  STATUS_RESP=$(curl -s -H "Authorization: Bearer $API_KEY" "http://127.0.0.1:${BUN_PORT}/v1/jobs/$JOB2_ID")
  STATUS=$(echo "$STATUS_RESP" | jq -r '.status')
  if [ "$STATUS" = "success" ]; then
    echo "Job completed successfully by Bun worker in iteration $i!"
    break
  fi
  sleep 0.5
done
test "$STATUS" = "success"

# Verify search works on both services
SEARCH_RUST2=$(curl -s -X POST -H "Authorization: Bearer $API_KEY" -H "Content-Type: application/json" \
  -d '{"query": "Beta 99"}' "http://127.0.0.1:${RUST_PORT}/v1/search")
echo "Search on Rust found chunks: $(echo "$SEARCH_RUST2" | jq '.chunks | length')"
test "$(echo "$SEARCH_RUST2" | jq '.chunks | length')" -gt 0

SEARCH_BUN2=$(curl -s -X POST -H "Authorization: Bearer $API_KEY" -H "Content-Type: application/json" \
  -d '{"query": "Beta 99"}' "http://127.0.0.1:${BUN_PORT}/v1/search")
echo "Search on Bun found chunks: $(echo "$SEARCH_BUN2" | jq '.chunks | length')"
test "$(echo "$SEARCH_BUN2" | jq '.chunks | length')" -gt 0
echo "✓ Test 2 Passed: Rust upload -> Bun ingest -> Search on both works!"

echo ""
echo "=========================================="
echo "  MIXED FLEET BIDIRECTIONAL TEST PASSED!  "
echo "=========================================="
