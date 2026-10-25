#!/usr/bin/env bash
# End-to-end smoke test for rantai-kb (Rust service).
# Real Postgres 16 + Qdrant + S3 (rustfs), stub embeddings (dim 8).
#
# The stack itself lives in `stack-up.sh` / `stack-down.sh`, so the same live
# server can be measured by `kb-server eval` or `kb-server eval leak` while it
# is up; this script is the behavioural walk-through and tears the stack down on
# the way out.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$WORKSPACE_ROOT/.." && pwd)"

STACK_DIR="${KB_STACK_DIR:-/tmp/kb-stack}"
SERVER_PORT="${KB_STACK_SERVER_PORT:-18080}"

TMP_DIR="$(mktemp -d -t kb-smoke-XXXXXX)"
EVENTS_LOG="$TMP_DIR/events.log"
SERVER_LOG="$STACK_DIR/server.log"
EVENTS_PID=""

cleanup() {
  echo ""
  echo "--- Server log on exit ---"
  if [ -f "$SERVER_LOG" ]; then
    cat "$SERVER_LOG"
  fi
  echo "--- Cleaning up smoke test resources ---"
  "$SCRIPT_DIR/stack-down.sh"
  rm -rf "$TMP_DIR"
  echo "--- Cleanup complete ---"
}
trap cleanup EXIT

echo "=== 0. Bringing up the kb-smoke stack ==="
"$SCRIPT_DIR/stack-up.sh"
# shellcheck disable=SC1091
source "$STACK_DIR/stack.env"

# Containers, S3 bucket, stub embedder, migrations, schema and kb-server were
# all started by stack-up.sh above; the API keys are minted here because they are
# part of what this script asserts, not part of the infrastructure.
echo "=== 5. Minting API keys ==="
KEY_OUTPUT=$("$KB_SERVER_BIN" create-api-key "smoke-tenant" "smoke-key" --scopes kb:read,kb:write)
echo "$KEY_OUTPUT"
API_KEY=$(echo "$KEY_OUTPUT" | grep -o 'rkb_[a-zA-Z0-9_-]*' | head -n 1)
test -n "$API_KEY"

KEY2_OUTPUT=$("$KB_SERVER_BIN" create-api-key "other-tenant" "other-key" --scopes kb:read,kb:write)
OTHER_KEY=$(echo "$KEY2_OUTPUT" | grep -o 'rkb_[a-zA-Z0-9_-]*' | head -n 1)
test -n "$OTHER_KEY"

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
