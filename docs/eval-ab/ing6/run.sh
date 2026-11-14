#!/usr/bin/env bash
# run.sh <label> <kb-server bin> <scenario: throughput|hol> [extra server env "K=V K=V"]
# One ING-6 benchmark run on the kb-ing6 stack: fresh fake OCR + fake embedder,
# a fresh cache dir, kb-server restarted on <bin>, then bench.py.
set -euo pipefail
label=$1; bin=$2; scenario=$3; extra=${4:-}
D=~/kbdata/ing6; R=$D/runs/$label; mkdir -p "$R"
pkill -f "$D/fake_ocr.py" 2>/dev/null || true
pkill -f "fake_embed.py" 2>/dev/null || true
sleep 1
PAGE_S=${PAGE_S:-1.5} MAX_INFLIGHT=${MAX_INFLIGHT:-16} PORT=39500 FAIL_EVERY=${FAIL_EVERY:-0} \
  nohup python3 "$D/fake_ocr.py" > "$R/ocr.log" 2>&1 &
DIM=1024 TEXT_S=${TEXT_S:-0.5} nohup python3 /home/shiro/rantai-kb-formats/rust/crates/kb-eval/scripts/fake_embed.py 39501 > "$R/embed.log" 2>&1 &
sleep 1
rm -rf "$D/cache-$label"
cd /home/shiro/rantai-kb-formats
export KB_STACK_NAME=kb-ing6 KB_STACK_DIR=/tmp/kb-ing6 KB_SERVER_BIN=$bin \
  KB_STACK_PG_PORT=45432 KB_STACK_S3_PORT=49000 KB_STACK_STUB_PORT=49111 KB_STACK_SERVER_PORT=48080 \
  KB_STACK_TEI_EMBED_PORT=48081 KB_STACK_TEI_RERANK_PORT=48082 KB_STACK_QDRANT_HTTP_PORT=46333 KB_STACK_QDRANT_GRPC_PORT=46334 \
  KB_STACK_EMBEDDINGS=remote KB_STACK_REMOTE_EMBED_URL=http://127.0.0.1:39501 KB_STACK_REMOTE_EMBED_KEY=x \
  KB_STACK_CACHE_DIR=$D/cache-$label KB_STACK_RESTART_SERVER=1 \
  KB_STACK_SERVER_ENV="KB_OCR_BASE_URL=http://127.0.0.1:39500/v1/chat/completions KB_OCR_MODEL=fake-ocr KB_OCR_CONCURRENCY=${OCR_CONC:-8} KB_OCR_TIMEOUT_MS=120000 KB_OCR_MAX_TOKENS=1024 $extra"
# First run brings the stack up; afterwards the restart path keeps the old
# stack.env, so the run-specific lines are rewritten here before the restart.
if [ -f /tmp/kb-ing6/stack.env ]; then
  env=/tmp/kb-ing6/stack.env
  grep -v -E '^export (KB_CACHE_DIR|KB_OCR_|KB_JOB_WORKERS|KB_SEATS|KB_INGEST_CONCURRENCY)' "$env" > "$env.new"
  echo "export KB_CACHE_DIR=\"$D/cache-$label\"" >> "$env.new"
  for kv in KB_OCR_BASE_URL=http://127.0.0.1:39500/v1/chat/completions KB_OCR_MODEL=fake-ocr \
            KB_OCR_CONCURRENCY=${OCR_CONC:-8} KB_OCR_TIMEOUT_MS=120000 KB_OCR_MAX_TOKENS=1024 $extra; do
    echo "export ${kv%%=*}=\"${kv#*=}\"" >> "$env.new"
  done
  mv "$env.new" "$env"; chmod 600 "$env"
fi
rust/scripts/stack-up.sh > "$R/stack.log" 2>&1
sleep 3
grep -q "KB_CACHE_DIR=\"$D/cache-$label\"" /tmp/kb-ing6/stack.env || { echo "stack.env not rewritten for $label" >&2; exit 1; }
pid=$(cat /tmp/kb-ing6/server.pid 2>/dev/null || pgrep -f "$bin serve" | head -1)
# the server process may be a child of the tmux bash; take the kb-server itself
pid=$(pgrep -f "^$bin serve" | head -1)
source /tmp/kb-ing6/stack.env
key=$("$bin" create-api-key "ing6-$label" "bench" --scopes kb:read,kb:write | grep -o 'rkb_[a-zA-Z0-9_-]*' | head -1)
echo "run $label bin=$bin pid=$pid scenario=$scenario extra=[$extra]" | tee "$R/meta.txt"
grep -E "^export KB_(OCR|JOB|SEATS|INGEST)" /tmp/kb-ing6/stack.env | tee -a "$R/meta.txt"
case $scenario in
  throughput) files=$(ls $D/corpus/scan50-*.pdf) ; stagger=0 ;;
  hol)        files="$D/corpus/big300.pdf $D/corpus/small1.pdf" ; stagger=2 ;;
esac
python3 "$D/bench.py" --label "$label" --key "$key" --port 48080 --pid "$pid" \
  --files $files --stagger $stagger --out "$D/results.jsonl" | tee "$R/bench.log"
tail -1 "$R/ocr.log" | tee -a "$R/meta.txt"
