#!/usr/bin/env bash
# Cold ingest of one PDF against a fake embedder with provider-like latency. Free:
# no paid API is called, and a fresh empty cache makes extraction and embedding cold.
#
#   KB_BENCH_PDF=path/to/big.pdf [LABEL=name] [PAGE_S=2.5] [TEXT_S=1.8] rust/scripts/bench-ingest.sh
#
# PAGE_S / TEXT_S: seconds per request, measured against voyage-multimodal-3.5 on
# OpenRouter (2026-10-24: ~2.5 s per 4 pages, ~1.8 s per 32 chunks). Read the ingest
# time from the server log ("ingest claimed" -> "ingest finished") and request
# overlap from the fake server's log ($OUT/fake-$LABEL.log).
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"
: "${KB_BENCH_PDF:?set KB_BENCH_PDF to a PDF}"
LABEL=${LABEL:-bench}
BIN=${BIN:-$REPO_ROOT/rust/target/release/kb-server}
OUT=${KB_BENCH_OUT:-$HOME/.cache/kb-bench}
PORT=${KB_BENCH_FAKE_PORT:-19300}
mkdir -p "$OUT"
DS=$(mktemp -d); CACHE=$(mktemp -d)
mkdir -p "$DS/corpus" "$DS/qrels"
ln -s "$(realpath "$KB_BENCH_PDF")" "$DS/corpus/"
stem=$(basename "$KB_BENCH_PDF" .pdf)
echo '{"_id": "0", "text": "benchmark query"}' > "$DS/queries.jsonl"
printf 'query-id\tcorpus-id\tscore\n0\t%s#p1\t1\n' "$stem" > "$DS/qrels/test.tsv"
echo "bench-ingest: $KB_BENCH_PDF" > "$DS/PROVENANCE.txt"
PAGE_S=${PAGE_S:-2.5} TEXT_S=${TEXT_S:-1.8} python3 -I rust/crates/kb-eval/scripts/fake_embed.py "$PORT" > "$OUT/fake-$LABEL.log" 2>&1 &
FAKE=$!
trap 'kill $FAKE 2>/dev/null; rm -rf "$DS" "$CACHE"' EXIT
sleep 1
export KB_CHUNK_STORE=qdrant KB_STACK_EMBEDDINGS=remote KB_STACK_REMOTE_EMBED_URL=http://127.0.0.1:$PORT \
  KB_STACK_REMOTE_EMBED_KEY=fake KB_STACK_REMOTE_EMBED_MODEL=fake/bench KB_STACK_REMOTE_EMBED_DIM=1024 \
  KB_STACK_RERANK=none KB_STACK_CACHE_DIR=$CACHE KB_AB_TIMEOUT=3600 KB_AB_DATASET=$DS KB_AB_OUT=$OUT
rm -rf "${OUT:?}/$LABEL"
KB_STACK_SERVER_ENV="KB_PAGE_EMBEDDING_MODEL=fake/page KB_PAGE_EMBEDDING_BASE_URL=http://127.0.0.1:$PORT/v1/embeddings KB_PAGE_EMBEDDING_API_KEY=fake KB_PAGE_EMBEDDING_DIM=1024 KB_PAGE_IMAGE_MAX_PAGES=2000 KB_MAX_FILE_BYTES=104857600 ${EXTRA:-}" \
  rust/scripts/eval-ab.sh "$LABEL=$BIN"
sed 's/\x1b\[[0-9;]*m//g' "${KB_STACK_DIR:-/tmp/kb-stack}/server.log" | grep -E "ingest claimed|ingest finished|page images" || true
