#!/usr/bin/env bash
# Cold ingest of one PDF with BOTH text chunks and page images embedded by a real
# Nemotron-3.5-Embed-1B vLLM, using the Studio one-embedder wire shapes. Free: no
# paid API is called; a counting proxy records requests in flight on the way to
# the already-running model server.
#
#   KB_BENCH_PDF=path/to/big.pdf rust/scripts/bench-real.sh
#
# Optional: LABEL, KB_BENCH_EMBED_URL (default http://127.0.0.1:8210),
# KB_BENCH_EMBED_MODEL, KB_BENCH_OUT, BIN, KB_BENCH_PROXY_PORT.
# The server log ("ingest claimed" -> "ingest finished") and the proxy log are
# kept under $KB_BENCH_OUT. Only `dep7-*` containers are started/stopped.
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"
: "${KB_BENCH_PDF:?set KB_BENCH_PDF to a PDF}"
LABEL=${LABEL:-real}
OUT=${KB_BENCH_OUT:-$HOME/dep7/results}
BIN=${BIN:-$HOME/kb-target-dep7/release/kb-server}
CLIENT=${KB_AB_CLIENT_BIN:-$BIN}
EMBED_URL=${KB_BENCH_EMBED_URL:-http://127.0.0.1:8210}
MODEL=${KB_BENCH_EMBED_MODEL:-nvidia/Nemotron-3.5-Embed-1B-BF16}
PROXY_PORT=${KB_BENCH_PROXY_PORT:-19210}
STACK_DIR="$OUT/stack-$LABEL"

mkdir -p "$OUT" "$STACK_DIR"
DS=$(mktemp -d "$OUT/dataset-$LABEL.XXXXXX")
CACHE=$(mktemp -d "$OUT/cache-$LABEL.XXXXXX")
mkdir -p "$DS/corpus" "$DS/qrels"
ln -s "$(realpath "$KB_BENCH_PDF")" "$DS/corpus/"
stem=$(basename "$KB_BENCH_PDF" .pdf)
echo '{"_id": "0", "text": "what is a diffusion model?"}' > "$DS/queries.jsonl"
printf 'query-id\tcorpus-id\tscore\n0\t%s#p1\t1\n' "$stem" > "$DS/qrels/test.tsv"
echo "bench-real: $KB_BENCH_PDF" > "$DS/PROVENANCE.txt"

PROXY_LOG="$OUT/proxy-$LABEL.log"
python3 -u "$REPO_ROOT/rust/scripts/embed_inflight_proxy.py" \
  --listen "127.0.0.1:$PROXY_PORT" --target "$EMBED_URL" \
  --log "$PROXY_LOG" --summary "$OUT/proxy-$LABEL.summary" \
  > "$OUT/proxy-$LABEL.stdout" 2>&1 &
PROXY=$!
trap 'kill "$PROXY" 2>/dev/null || true; rm -rf "$DS" "$CACHE"' EXIT
for _ in {1..40}; do
  curl -sf "http://127.0.0.1:$PROXY_PORT/health" >/dev/null 2>&1 && break
  sleep 0.25
done

# Studio preset shapes: /v2/embed, input_type query/document, `inputs` as
# message content, `embeddings.float` back, both roles at 2048-d.
export KB_CHUNK_STORE=qdrant KB_STACK_EMBEDDINGS=remote \
  KB_STACK_REMOTE_EMBED_URL="http://127.0.0.1:$PROXY_PORT" \
  KB_STACK_REMOTE_EMBED_KEY=fake KB_STACK_REMOTE_EMBED_MODEL="$MODEL" \
  KB_STACK_REMOTE_EMBED_DIM=2048 KB_STACK_REMOTE_EMBED_TIMEOUT_MS=600000 \
  KB_STACK_RERANK=none KB_STACK_CACHE_DIR="$CACHE" \
  KB_AB_TIMEOUT=3600 KB_AB_DATASET="$DS" KB_AB_OUT="$OUT" \
  KB_AB_CLIENT_BIN="$CLIENT" \
  KB_STACK_NAME=dep7 KB_STACK_DIR="$STACK_DIR" \
  KB_STACK_PG_PORT=25432 KB_STACK_S3_PORT=29000 KB_STACK_STUB_PORT=29111 \
  KB_STACK_SERVER_PORT=28080 KB_STACK_QDRANT_HTTP_PORT=26333 \
  KB_STACK_QDRANT_GRPC_PORT=26334

export KB_STACK_SERVER_ENV="KB_TIER=studio KB_EMBED_CONCURRENCY=4 KB_EMBEDDING_FORMAT=omniembed KB_EMBEDDING_BASE_URL=http://127.0.0.1:$PROXY_PORT/v2/embed KB_EMBEDDING_API_KEY=fake KB_EMBEDDING_MODEL=$MODEL KB_EMBEDDING_DIM=2048 KB_EMBEDDING_QUERY_PREFIX= KB_EMBEDDING_PASSAGE_PREFIX= KB_EMBEDDING_TIMEOUT_MS=600000 KB_PAGE_EMBEDDING_FORMAT=omniembed KB_PAGE_EMBEDDING_BASE_URL=http://127.0.0.1:$PROXY_PORT/v2/embed KB_PAGE_EMBEDDING_API_KEY=fake KB_PAGE_EMBEDDING_MODEL=$MODEL KB_PAGE_EMBEDDING_DIM=2048 KB_PAGE_EMBEDDING_CONCURRENCY=4 KB_PAGE_EMBEDDING_BATCH_SIZE=4 KB_PAGE_IMAGE_MAX_PAGES=2000 KB_MAX_FILE_BYTES=104857600 KB_OCR_MODE=off KB_RERANK_ENABLED=false"

echo "=== bench-real ($LABEL): $KB_BENCH_PDF -> $EMBED_URL ($MODEL) ==="
"$REPO_ROOT/rust/scripts/eval-ab.sh" "$LABEL=$BIN"

echo "=== server log: ingest markers ==="
sed 's/\x1b\[[0-9;]*m//g' "$STACK_DIR/server.log" \
  | grep -E 'ingest claimed|ingest finished|page images|failed' || true
echo "=== proxy summary ==="
cat "$OUT/proxy-$LABEL.summary" 2>/dev/null || true
