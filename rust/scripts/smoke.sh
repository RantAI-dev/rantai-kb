#!/usr/bin/env bash
# End-to-end smoke test for rantai-kb (Rust service).
# Real Postgres 16 + SurrealDB v2 + S3 (rustfs), stub embeddings (dim 8).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$WORKSPACE_ROOT/.." && pwd)"

PG_CONTAINER="kb-smoke-pg"
SURREAL_CONTAINER="kb-smoke-surreal"
RUSTFS_CONTAINER="kb-smoke-rustfs"

PG_PORT="15432"
SURREAL_PORT="18000"
S3_PORT="19000"
STUB_PORT="19111"
SERVER_PORT="18080"

TMP_DIR="$(mktemp -d -t kb-smoke-XXXXXX)"
EVENTS_LOG="$TMP_DIR/events.log"
SERVER_LOG="$TMP_DIR/server.log"
STUB_LOG="$TMP_DIR/stub.log"

STUB_PID=""
SERVER_PID=""
EVENTS_PID=""

cleanup() {
  echo ""
  echo "--- Server log on exit ---"
  if [ -f "$SERVER_LOG" ]; then
    cat "$SERVER_LOG"
  fi
  echo "--- Cleaning up smoke test resources ---"
  if [ -n "$EVENTS_PID" ] && kill -0 "$EVENTS_PID" 2>/dev/null; then
    kill "$EVENTS_PID" 2>/dev/null || true
  fi
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
  fi
  if [ -n "$STUB_PID" ] && kill -0 "$STUB_PID" 2>/dev/null; then
    kill "$STUB_PID" 2>/dev/null || true
  fi
  docker rm -f "$PG_CONTAINER" "$SURREAL_CONTAINER" "$RUSTFS_CONTAINER" 2>/dev/null || true
  rm -rf "$TMP_DIR"
  echo "--- Cleanup complete ---"
}
trap cleanup EXIT

echo "=== 1. Starting clean containers ==="
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

echo "Waiting for Postgres to be ready..."
for _ in {1..30}; do
  if docker exec "$PG_CONTAINER" pg_isready -U kb >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

echo "Waiting for SurrealDB to be ready..."
for _ in {1..30}; do
  if curl -s "http://127.0.0.1:${SURREAL_PORT}/health" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

echo "Waiting for RustFS (S3) to be ready..."
for _ in {1..30}; do
  if curl -s "http://127.0.0.1:${S3_PORT}/minio/health/ready" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

echo "Creating S3 bucket rantai-kb..."
docker run --rm --network host \
  -e AWS_ACCESS_KEY_ID=kbadmin \
  -e AWS_SECRET_ACCESS_KEY=kbsecret123 \
  amazon/aws-cli:latest --endpoint-url "http://127.0.0.1:${S3_PORT}" s3 mb s3://rantai-kb

echo "=== 2. Starting stub OpenAI embedding server ==="
STUB_BIN="$WORKSPACE_ROOT/target/debug/examples/stub_embeddings"
if [ ! -f "$STUB_BIN" ]; then
  cargo build -p kb-server --example stub_embeddings --manifest-path "$WORKSPACE_ROOT/Cargo.toml"
fi
"$STUB_BIN" "127.0.0.1:${STUB_PORT}" 8 > "$STUB_LOG" 2>&1 &
STUB_PID=$!

for _ in {1..20}; do
  if curl -s "http://127.0.0.1:${STUB_PORT}/health" >/dev/null 2>&1; then
    break
  fi
  sleep 0.5
done
echo "Stub embeddings running on 127.0.0.1:${STUB_PORT} (PID: $STUB_PID)"

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
export KB_EMBEDDING_BASE_URL="http://127.0.0.1:${STUB_PORT}/embeddings"
export KB_EMBEDDING_API_KEY="stub"
export KB_EMBEDDING_MODEL="stub"
export KB_EMBEDDING_DIM="8"
export KB_ENTITY_EXTRACTION_ENABLED="false"
export KB_WORKER_ENABLED="true"
export PORT="${SERVER_PORT}"
export KB_MIGRATIONS_DIR="$REPO_ROOT/prisma/migrations"
export RUST_LOG="info,kb_server=debug,kb_store_blob=debug,kb_engine=debug"

KB_SERVER_BIN="$WORKSPACE_ROOT/target/debug/kb-server"
if [ ! -f "$KB_SERVER_BIN" ]; then
  cargo build -p kb-server --bin kb-server --manifest-path "$WORKSPACE_ROOT/Cargo.toml"
fi

echo "=== 3. Running migrations ==="
"$KB_SERVER_BIN" migrate

echo "=== 4. Applying vector schema ==="
"$KB_SERVER_BIN" apply-schema

echo "=== 5. Minting API keys ==="
KEY_OUTPUT=$("$KB_SERVER_BIN" create-api-key "smoke-tenant" "smoke-key" --scopes kb:read,kb:write)
echo "$KEY_OUTPUT"
API_KEY=$(echo "$KEY_OUTPUT" | grep -o 'rkb_[a-zA-Z0-9_-]*' | head -n 1)
test -n "$API_KEY"

KEY2_OUTPUT=$("$KB_SERVER_BIN" create-api-key "other-tenant" "other-key" --scopes kb:read,kb:write)
OTHER_KEY=$(echo "$KEY2_OUTPUT" | grep -o 'rkb_[a-zA-Z0-9_-]*' | head -n 1)
test -n "$OTHER_KEY"

echo "=== 6. Starting kb-server ==="
"$KB_SERVER_BIN" serve > "$SERVER_LOG" 2>&1 &
SERVER_PID=$!

for _ in {1..30}; do
  if curl -s "http://127.0.0.1:${SERVER_PORT}/health" >/dev/null 2>&1; then
    break
  fi
  sleep 0.5
done
echo "kb-server listening on port ${SERVER_PORT} (PID: $SERVER_PID)"

echo "=== 7. Health & Auth checks ==="
HEALTH_RESP=$(curl -s "http://127.0.0.1:${SERVER_PORT}/health")
echo "Health: $HEALTH_RESP"
test "$(echo "$HEALTH_RESP" | jq -r '.ok')" = "true"
test "$(echo "$HEALTH_RESP" | jq -r '.checks.postgres')" = "true"
test "$(echo "$HEALTH_RESP" | jq -r '.checks.vectors')" = "true"

UNAUTH_STATUS=$(curl -s -o /dev/null -w "%{http_code}" -X POST "http://127.0.0.1:${SERVER_PORT}/v1/search")
test "$UNAUTH_STATUS" = "401"
echo "Unauthenticated request correctly returned 401"

echo "=== 8. Connecting SSE events listener ==="
curl -s -N -H "Authorization: Bearer $API_KEY" "http://127.0.0.1:${SERVER_PORT}/v1/events" > "$EVENTS_LOG" 2>&1 &
EVENTS_PID=$!
sleep 1

echo "=== 9. Uploading document ==="
CSV_FILE="$TMP_DIR/pengunjung.csv"
cat << 'EOF' > "$CSV_FILE"
Kota,Pengunjung,Bulan
Kota0,1000,2026-01
Kota1,1037,2026-02
Kota2,1074,2026-03
Kota3,1111,2026-04
EOF

UPLOAD_RESP=$(curl -s -w "\n%{http_code}" -X POST \
  -H "Authorization: Bearer $API_KEY" \
  -F "file=@$CSV_FILE;type=text/csv" \
  -F "title=Data Pengunjung" \
  "http://127.0.0.1:${SERVER_PORT}/v1/documents")

HTTP_STATUS=$(echo "$UPLOAD_RESP" | tail -n 1)
UPLOAD_BODY=$(echo "$UPLOAD_RESP" | head -n -1)
echo "Upload status: $HTTP_STATUS"
echo "Upload body: $UPLOAD_BODY"

if [ "$HTTP_STATUS" != "202" ]; then
  echo "ERROR: upload returned $HTTP_STATUS"
  exit 1
fi

DOC_ID=$(echo "$UPLOAD_BODY" | jq -r '.id')
JOB_ID=$(echo "$UPLOAD_BODY" | jq -r '.jobId')
test -n "$DOC_ID"
test -n "$JOB_ID"
echo "Uploaded doc: $DOC_ID, jobId: $JOB_ID"

echo "=== 10. Waiting for ingest job completion ==="
JOB_SUCCESS=false
for i in {1..60}; do
  JOB_RESP=$(curl -s -H "Authorization: Bearer $API_KEY" "http://127.0.0.1:${SERVER_PORT}/v1/jobs/$JOB_ID")
  STATUS=$(echo "$JOB_RESP" | jq -r '.status')
  if [ "$STATUS" = "success" ]; then
    echo "Job succeeded in iteration $i: $JOB_RESP"
    JOB_SUCCESS=true
    break
  elif [ "$STATUS" = "failed" ]; then
    echo "Job failed! $JOB_RESP"
    cat "$SERVER_LOG"
    exit 1
  fi
  sleep 0.5
done
test "$JOB_SUCCESS" = "true"

echo "=== 11. Verifying SSE events delivered ==="
kill "$EVENTS_PID" 2>/dev/null || true
EVENTS_PID=""
echo "Captured SSE events:"
cat "$EVENTS_LOG"
grep -q ": connected" "$EVENTS_LOG"
echo "SSE connection verified!"

echo "=== 12. Searching and retrieving chunk ==="
SEARCH_RESP=$(curl -s -X POST \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"query": "Kota pengunjung", "maxChunks": 5}' \
  "http://127.0.0.1:${SERVER_PORT}/v1/search")
echo "Search result: $SEARCH_RESP"
CHUNK_COUNT=$(echo "$SEARCH_RESP" | jq '.chunks | length')
test "$CHUNK_COUNT" -gt 0
FIRST_CONTENT=$(echo "$SEARCH_RESP" | jq -r '.chunks[0].content')
echo "First chunk content preview: ${FIRST_CONTENT:0:80}"
echo "$FIRST_CONTENT" | grep -q "Kota"

echo "=== 13. List, Detail, Raw ==="
LIST_RESP=$(curl -s -H "Authorization: Bearer $API_KEY" "http://127.0.0.1:${SERVER_PORT}/v1/documents")
test "$(echo "$LIST_RESP" | jq -r '.documents[0].id')" = "$DOC_ID"

DETAIL_RESP=$(curl -s -H "Authorization: Bearer $API_KEY" "http://127.0.0.1:${SERVER_PORT}/v1/documents/$DOC_ID")
test "$(echo "$DETAIL_RESP" | jq -r '.document.id')" = "$DOC_ID"
test "$(echo "$DETAIL_RESP" | jq '.chunks | length')" -gt 0

RAW_RESP=$(curl -s -H "Authorization: Bearer $API_KEY" "http://127.0.0.1:${SERVER_PORT}/v1/documents/$DOC_ID/raw")
diff -u "$CSV_FILE" <(echo "$RAW_RESP")
echo "Raw bytes match original upload exactly!"

echo "=== 14. Retry non-failed job -> 409 Conflict ==="
RETRY_STATUS=$(curl -s -o /dev/null -w "%{http_code}" -X POST \
  -H "Authorization: Bearer $API_KEY" \
  "http://127.0.0.1:${SERVER_PORT}/v1/jobs/$JOB_ID/retry")
echo "Retry status: $RETRY_STATUS"
test "$RETRY_STATUS" = "409"
echo "Retry correctly returned 409 Conflict for non-failed job"

echo "=== 15. Cross-tenant isolation ==="
OTHER_LIST=$(curl -s -H "Authorization: Bearer $OTHER_KEY" "http://127.0.0.1:${SERVER_PORT}/v1/documents")
test "$(echo "$OTHER_LIST" | jq '.documents | length')" = "0"

OTHER_SEARCH=$(curl -s -X POST \
  -H "Authorization: Bearer $OTHER_KEY" \
  -H "Content-Type: application/json" \
  -d '{"query": "Kota pengunjung", "maxChunks": 5}' \
  "http://127.0.0.1:${SERVER_PORT}/v1/search")
test "$(echo "$OTHER_SEARCH" | jq '.chunks | length')" = "0"
echo "Tenant isolation verified: other tenant sees 0 docs and 0 chunks"

echo "=== 16. Deleting document ==="
DEL_STATUS=$(curl -s -o /dev/null -w "%{http_code}" -X DELETE \
  -H "Authorization: Bearer $API_KEY" \
  "http://127.0.0.1:${SERVER_PORT}/v1/documents/$DOC_ID")
test "$DEL_STATUS" = "200"

AFTER_DEL_STATUS=$(curl -s -o /dev/null -w "%{http_code}" \
  -H "Authorization: Bearer $API_KEY" \
  "http://127.0.0.1:${SERVER_PORT}/v1/documents/$DOC_ID")
test "$AFTER_DEL_STATUS" = "404"
echo "Document successfully deleted and confirmed 404!"

echo ""
echo "=========================================="
echo "  LIVE END-TO-END SMOKE TEST PASSED!     "
echo "=========================================="
