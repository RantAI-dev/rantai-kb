#!/usr/bin/env bash
# Search parity test: Bun reference vs Rust implementation
# Compares /v1/search responses across a matrix of requests
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$WORKSPACE_ROOT/.." && pwd)"

PG_CONTAINER="kb-parity-pg"
SURREAL_CONTAINER="kb-parity-surreal"
RUSTFS_CONTAINER="kb-parity-rustfs"

PG_PORT="15434"
SURREAL_PORT="18002"
S3_PORT="19002"
STUB_PORT="19113"
RUST_PORT="18082"
BUN_PORT="18083"

TMP_DIR="$(mktemp -d -t kb-parity-XXXXXX)"
BUN_LOG="$TMP_DIR/bun.log"
RUST_LOG="$TMP_DIR/rust.log"
STUB_LOG="$TMP_DIR/stub.log"

STUB_PID=""
RUST_PID=""
BUN_PID=""

cleanup() {
  echo ""
  echo "--- Cleaning up search-parity resources ---"
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
  echo "--- Search-parity cleanup complete ---"
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

echo "Waiting for containers to be ready..."
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
if [ ! -f "$STUB_BIN" ]; then
  cargo build -p kb-server --example stub_embeddings --manifest-path "$WORKSPACE_ROOT/Cargo.toml"
fi
"$STUB_BIN" "127.0.0.1:${STUB_PORT}" 8 > "$STUB_LOG" 2>&1 &
STUB_PID=$!
for _ in {1..20}; do
  if curl -s "http://127.0.0.1:${STUB_PORT}/health" >/dev/null 2>&1; then break; fi
  sleep 0.5
done

# Shared Environment
export DATABASE_URL="postgresql://kb:kb@127.0.0.1:${PG_PORT}/rantai_kb?schema=public"
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
if [ ! -f "$KB_SERVER_BIN" ]; then
  cargo build -p kb-server --bin kb-server --manifest-path "$WORKSPACE_ROOT/Cargo.toml"
fi

echo "=== 3. Schema & Keys ==="
SURREAL_DB_URL="http://127.0.0.1:${SURREAL_PORT}" "$KB_SERVER_BIN" migrate
SURREAL_DB_URL="http://127.0.0.1:${SURREAL_PORT}" "$KB_SERVER_BIN" apply-schema

KEY_OUTPUT=$("$KB_SERVER_BIN" create-api-key "parity-tenant" "parity-key" --scopes kb:read,kb:write)
API_KEY=$(echo "$KEY_OUTPUT" | grep -o 'rkb_[a-zA-Z0-9_-]*' | head -n 1)
echo "API Key: $API_KEY"

echo "=== 4. Starting Bun (API) and Rust (Worker + API) ==="
PORT="${BUN_PORT}" KB_WORKER_ENABLED="false" SURREAL_DB_URL="ws://127.0.0.1:${SURREAL_PORT}/rpc" bun "$REPO_ROOT/src/server.ts" > "$BUN_LOG" 2>&1 &
BUN_PID=$!

PORT="${RUST_PORT}" KB_WORKER_ENABLED="true" SURREAL_DB_URL="http://127.0.0.1:${SURREAL_PORT}" "$KB_SERVER_BIN" serve > "$RUST_LOG" 2>&1 &
RUST_PID=$!

for _ in {1..30}; do
  if curl -s "http://127.0.0.1:${BUN_PORT}/health" >/dev/null 2>&1 && curl -s "http://127.0.0.1:${RUST_PORT}/health" >/dev/null 2>&1; then
    break
  fi
  sleep 0.5
done
echo "Both Bun (:${BUN_PORT}) and Rust (:${RUST_PORT}) are ready."

echo "=== 5. Setting up Knowledge Base & Documents ==="
KB_RESP=$(curl -s -X POST \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"name": "Parity KB A", "description": "Primary KB for search parity verification"}' \
  "http://127.0.0.1:${RUST_PORT}/v1/knowledge-bases")
KB_ID=$(echo "$KB_RESP" | jq -r '.id // .knowledgeBase.id')
echo "Created Knowledge Base A: $KB_ID"

KB_B_RESP=$(curl -s -X POST \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"name": "Parity KB B", "description": "Secondary KB for leakage divergence verification"}' \
  "http://127.0.0.1:${RUST_PORT}/v1/knowledge-bases")
KB_B_ID=$(echo "$KB_B_RESP" | jq -r '.id // .knowledgeBase.id')
echo "Created Knowledge Base B: $KB_B_ID"

# Doc 1: CSV inventory
CSV_FILE="$TMP_DIR/items.csv"
cat << 'EOF' > "$CSV_FILE"
Item,Count,Location
Alpha,42,Jakarta
Beta,99,Bandung
Gamma,17,Surabaya
EOF

# Doc 2: Markdown guide
MD_FILE="$TMP_DIR/alpha-guide.md"
cat << 'EOF' > "$MD_FILE"
# Alpha Architecture Guide

Alpha serves as the primary gateway cluster in Jakarta handling 42 core partitions.
Secondary failover operations route directly to Beta infrastructure in Bandung.
Gamma monitoring nodes supervise operations across Surabaya.
EOF

# Doc 3: Plain text notes
TXT_FILE="$TMP_DIR/beta-notes.txt"
cat << 'EOF' > "$TXT_FILE"
Beta deployment runbook notes:
Beta 99 instances deployed across Bandung cluster.
Alpha configuration synchronizes state with Bandung every hour.
EOF

# Doc 4: Archive markdown
ARCHIVE_FILE="$TMP_DIR/delta-archive.md"
cat << 'EOF' > "$ARCHIVE_FILE"
# Delta Historic Archive

Delta records document legacy mainframe records from ancient repository.
No active overlap with modern Alpha or Beta production clusters.
EOF

# Doc 5: Secret document in KB B with unique term for leak testing
LEAK_FILE="$TMP_DIR/omega-leak.txt"
cat << 'EOF' > "$LEAK_FILE"
OmegaLeakToken unique secret record strictly contained in Knowledge Base B.
This content has category leaktest and must never be exposed to KB A or guides queries.
EOF

upload_doc() {
  local file="$1"
  local title="$2"
  local type="$3"
  local cat="$4"
  local kb_id="$5"

  local cmd=(curl -s -X POST -H "Authorization: Bearer $API_KEY")
  cmd+=(-F "file=@${file};type=${type}")
  cmd+=(-F "title=${title}")
  if [ -n "$cat" ]; then
    cmd+=(-F "categories=${cat}")
  fi
  if [ -n "$kb_id" ]; then
    cmd+=(-F "knowledgeBaseIds=[\"${kb_id}\"]")
  fi
  cmd+=("http://127.0.0.1:${RUST_PORT}/v1/documents")

  local resp
  resp=$("${cmd[@]}")
  local doc_id
  doc_id=$(echo "$resp" | jq -r '.id')
  local job_id
  job_id=$(echo "$resp" | jq -r '.jobId')
  echo >&2 "Uploaded $title -> doc: $doc_id, job: $job_id"

  # Wait for ingest
  for _ in {1..60}; do
    local st
    st=$(curl -s -H "Authorization: Bearer $API_KEY" "http://127.0.0.1:${RUST_PORT}/v1/jobs/$job_id" | jq -r '.status')
    if [ "$st" = "success" ]; then
      break
    elif [ "$st" = "failed" ]; then
      echo "ERROR: job $job_id failed!"
      exit 1
    fi
    sleep 0.5
  done
  echo "$doc_id"
}

DOC1_ID=$(upload_doc "$CSV_FILE" "Inventory CSV" "text/csv" "inventory" "")
DOC2_ID=$(upload_doc "$MD_FILE" "Alpha Guide" "text/markdown" "guides" "$KB_ID")
DOC3_ID=$(upload_doc "$TXT_FILE" "Beta Notes" "text/plain" "notes" "$KB_ID")
DOC4_ID=$(upload_doc "$ARCHIVE_FILE" "Delta Archive" "text/markdown" "archive" "")
DOC5_ID=$(upload_doc "$LEAK_FILE" "Omega Leak Doc" "text/plain" "leaktest" "$KB_B_ID")

echo "Ingested all 5 documents successfully."
echo "DOC1 (csv): $DOC1_ID"
echo "DOC2 (md, in KB A): $DOC2_ID"
echo "DOC3 (txt, in KB A): $DOC3_ID"
echo "DOC4 (archive): $DOC4_ID"
echo "DOC5 (leak test, in KB B): $DOC5_ID"

echo "=== 6. Running Search Parity Matrix ==="

# We define test cases as JSON payloads
declare -a TEST_CASES=(
  # Queries basic
  '{"query": "Alpha Count"}'
  '{"query": "Beta 99"}'
  '{"query": "Jakarta Bandung"}'
  '{"query": "Gamma"}'
  '{"query": "Delta archive"}'
  '{"query": "nonexistent term qwerty12345"}'

  # With and without hybrid
  '{"query": "Alpha Count", "hybrid": true}'
  '{"query": "Alpha Count", "hybrid": false}'
  '{"query": "Beta 99", "hybrid": true}'
  '{"query": "Beta 99", "hybrid": false}'

  # maxChunks variations (1, 3, 8, 50)
  '{"query": "Beta 99", "maxChunks": 1}'
  '{"query": "Beta 99", "maxChunks": 3}'
  '{"query": "Beta 99", "maxChunks": 8}'
  '{"query": "Beta 99", "maxChunks": 50}'

  # format: "context"
  '{"query": "Alpha Count", "format": "context"}'
  '{"query": "Beta 99", "format": "context", "hybrid": true}'
  '{"query": "Beta 99", "format": "context", "hybrid": false}'

  # documentIds filtering
  "{\"query\": \"Beta 99\", \"documentIds\": [\"$DOC1_ID\"]}"
  "{\"query\": \"Beta 99\", \"documentIds\": [\"$DOC2_ID\", \"$DOC3_ID\"]}"
  "{\"query\": \"Beta 99\", \"documentIds\": [\"$DOC1_ID\"], \"format\": \"context\"}"
  "{\"query\": \"Beta 99\", \"documentIds\": [\"$DOC4_ID\"]}"

  # knowledgeBaseIds filtering (in-scope matches)
  "{\"query\": \"failover operations\", \"knowledgeBaseIds\": [\"$KB_ID\"]}"
  "{\"query\": \"Alpha Guide\", \"knowledgeBaseIds\": [\"$KB_ID\"]}"

  # category filtering (in-scope matches)
  '{"query": "Alpha Count", "category": "inventory"}'
  '{"query": "gateway cluster", "category": "guides"}'
  '{"query": "runbook notes", "category": "notes"}'
  '{"query": "Delta mainframe", "category": "archive"}'
  '{"query": "nonexistent term qwerty12345", "category": "nonexistent"}'

  # Leak term searched unrestricted or within KB B (both Bun and Rust match)
  '{"query": "OmegaLeakToken"}'
  "{\"query\": \"OmegaLeakToken\", \"knowledgeBaseIds\": [\"$KB_B_ID\"]}"
)

# Leak cases with expected divergence (Rust drops out-of-scope BM25 hits; Bun leaks them)
declare -a DIVERGENCE_CASES=(
  "{\"query\": \"OmegaLeakToken\", \"knowledgeBaseIds\": [\"$KB_ID\"]}"
  "{\"query\": \"OmegaLeakToken\", \"category\": \"guides\"}"
  "{\"query\": \"OmegaLeakToken\", \"knowledgeBaseIds\": [\"$KB_ID\"], \"category\": \"guides\"}"
)

TOTAL_CASES=$((${#TEST_CASES[@]} + ${#DIVERGENCE_CASES[@]}))
PASSED_CASES=0
FAILED_CASES=0

# jq normalization filter:
# Sorts keys, rounds numbers to 4 decimal places, and drops `chunks[].retrieval`.
#
# `retrieval` is the additive per-arm provenance block Rust added in S.1c and Bun
# does not produce. This script exists to prove the two runtimes answer the same
# question identically on every field they *share*; a field only one of them emits
# is not a divergence to fix, so it is removed before the diff. Deleting it from
# Bun's answer is a no-op. `docs/rust-cutover.md` records the deliberate
# difference.
NORMALIZE_JQ='walk(if type == "object" and has("totalTimeMs") then (.embeddingTimeMs = 0 | .vectorSearchTimeMs = 0 | .entitySearchTimeMs = 0 | .graphSearchTimeMs = 0 | .fusionTimeMs = 0 | .totalTimeMs = 0) elif type == "number" then ((. * 10000 | round) / 10000) else . end) | del(.chunks[]?.retrieval)'

for idx in "${!TEST_CASES[@]}"; do
  BODY="${TEST_CASES[$idx]}"
  echo "--- Test $((idx + 1))/$TOTAL_CASES: $BODY ---"

  BUN_RESP=$(curl -s -X POST \
    -H "Authorization: Bearer $API_KEY" \
    -H "Content-Type: application/json" \
    -d "$BODY" \
    "http://127.0.0.1:${BUN_PORT}/v1/search")

  RUST_RESP=$(curl -s -X POST \
    -H "Authorization: Bearer $API_KEY" \
    -H "Content-Type: application/json" \
    -d "$BODY" \
    "http://127.0.0.1:${RUST_PORT}/v1/search")

  echo "$BUN_RESP" | jq -S "$NORMALIZE_JQ" > "$TMP_DIR/bun_norm.json" 2>"$TMP_DIR/bun_err.log" || {
    echo "ERROR: Failed to parse Bun JSON response: $BUN_RESP"
    cat "$TMP_DIR/bun_err.log"
    FAILED_CASES=$((FAILED_CASES + 1))
    continue
  }

  echo "$RUST_RESP" | jq -S "$NORMALIZE_JQ" > "$TMP_DIR/rust_norm.json" 2>"$TMP_DIR/rust_err.log" || {
    echo "ERROR: Failed to parse Rust JSON response: $RUST_RESP"
    cat "$TMP_DIR/rust_err.log"
    FAILED_CASES=$((FAILED_CASES + 1))
    continue
  }

  if diff -u "$TMP_DIR/bun_norm.json" "$TMP_DIR/rust_norm.json" > "$TMP_DIR/diff.patch"; then
    echo "✓ Match!"
    PASSED_CASES=$((PASSED_CASES + 1))
  else
    echo "✗ DIFF DETECTED:"
    cat "$TMP_DIR/diff.patch"
    FAILED_CASES=$((FAILED_CASES + 1))
  fi
done

echo ""
echo "=== 7. Running Expected Divergence Cases (Access-Control Leak Matrix) ==="
for div_idx in "${!DIVERGENCE_CASES[@]}"; do
  BODY="${DIVERGENCE_CASES[$div_idx]}"
  case_num=$((${#TEST_CASES[@]} + div_idx + 1))
  echo "--- Test $case_num/$TOTAL_CASES (Expected Divergence): $BODY ---"

  BUN_RESP=$(curl -s -X POST \
    -H "Authorization: Bearer $API_KEY" \
    -H "Content-Type: application/json" \
    -d "$BODY" \
    "http://127.0.0.1:${BUN_PORT}/v1/search")

  RUST_RESP=$(curl -s -X POST \
    -H "Authorization: Bearer $API_KEY" \
    -H "Content-Type: application/json" \
    -d "$BODY" \
    "http://127.0.0.1:${RUST_PORT}/v1/search")

  RUST_LEAK_COUNT=$(echo "$RUST_RESP" | jq -r '(.chunks // []) | map(select(.documentId == "'"$DOC5_ID"'" or (.content | test("OmegaLeakToken")))) | length')
  BUN_LEAK_COUNT=$(echo "$BUN_RESP" | jq -r '(.chunks // []) | map(select(.documentId == "'"$DOC5_ID"'" or (.content | test("OmegaLeakToken")))) | length')

  if [ "$RUST_LEAK_COUNT" -eq 0 ]; then
    echo "✓ Expected divergence verified: Rust returned 0 KB-B chunks (secure); Bun returned $BUN_LEAK_COUNT leaked chunks."
    PASSED_CASES=$((PASSED_CASES + 1))
  else
    echo "✗ SECURITY REGRESSION: Rust leaked $RUST_LEAK_COUNT KB-B chunks!"
    FAILED_CASES=$((FAILED_CASES + 1))
  fi
done

echo ""
echo "=========================================="
echo "Search Parity Results: $PASSED_CASES / $TOTAL_CASES passed, $FAILED_CASES failed (${#DIVERGENCE_CASES[@]} expected divergences verified)"
echo "=========================================="

if [ "$FAILED_CASES" -gt 0 ]; then
  exit 1
fi
