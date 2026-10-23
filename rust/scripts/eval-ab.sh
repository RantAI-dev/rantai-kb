#!/usr/bin/env bash
# A/B two kb-server binaries on one dataset, each on a fresh kb-smoke stack.
#
#   rust/scripts/eval-ab.sh <label>=<kb-server binary> [<label>=<binary> ...]
#
# Every arm gets a clean stack (stack-down, then stack-up), its own API key, a full
# ingest and a full query run, written to $KB_AB_OUT/<label>/. The eval client is
# always $KB_AB_CLIENT_BIN (default: this workspace's release kb-server), so only
# the server under test changes between arms.
#
# Embeddings come from KB_STACK_EMBEDDINGS (default "remote": the LAN GPU box, with
# KB_STACK_REMOTE_EMBED_URL / KB_STACK_REMOTE_EMBED_KEY; the key defaults to
# LLAMA_GPU_API_KEY from the repo's .env). Chunk store defaults to Qdrant.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$WORKSPACE_ROOT/.." && pwd)"

DATASET="${KB_AB_DATASET:-$WORKSPACE_ROOT/crates/kb-eval/datasets/miracl-id-200}"
OUT="${KB_AB_OUT:-$REPO_ROOT/docs/eval-ab/$(basename "$DATASET")}"
CLIENT_BIN="${KB_AB_CLIENT_BIN:-$WORKSPACE_ROOT/target/release/kb-server}"
STACK_DIR="${KB_STACK_DIR:-/tmp/kb-stack}"

export KB_CHUNK_STORE="${KB_CHUNK_STORE:-qdrant}"
export KB_STACK_EMBEDDINGS="${KB_STACK_EMBEDDINGS:-remote}"
if [ "$KB_STACK_EMBEDDINGS" = "remote" ]; then
  export KB_STACK_REMOTE_EMBED_URL="${KB_STACK_REMOTE_EMBED_URL:-http://192.168.18.92:8081}"
  if [ -z "${KB_STACK_REMOTE_EMBED_KEY:-}" ] && [ -f "$REPO_ROOT/.env" ]; then
    KB_STACK_REMOTE_EMBED_KEY="$(grep '^LLAMA_GPU_API_KEY=' "$REPO_ROOT/.env" | tail -n 1 | cut -d= -f2- | tr -d '"\r ')"
    export KB_STACK_REMOTE_EMBED_KEY
  fi
fi
if [ -z "${PDFIUM_DYNAMIC_LIB_PATH:-}" ] && [ -f "$WORKSPACE_ROOT/.vendor/pdfium/lib/libpdfium.so" ]; then
  export PDFIUM_DYNAMIC_LIB_PATH="$WORKSPACE_ROOT/.vendor/pdfium/lib/libpdfium.so"
fi

[ "$#" -ge 1 ] || { echo "usage: $0 <label>=<kb-server binary> ..." >&2; exit 2; }
[ -x "$CLIENT_BIN" ] || { echo "eval client $CLIENT_BIN not found" >&2; exit 1; }
mkdir -p "$OUT"

for arm in "$@"; do
  label="${arm%%=*}"
  bin="${arm#*=}"
  [ -x "$bin" ] || { echo "$label: $bin is not an executable" >&2; exit 1; }
  echo "=== arm $label ($bin) ==="
  "$SCRIPT_DIR/stack-down.sh" >/dev/null 2>&1 || true
  KB_SERVER_BIN="$bin" "$SCRIPT_DIR/stack-up.sh"
  # shellcheck disable=SC1090
  source "$STACK_DIR/stack.env"
  key="$("$bin" create-api-key "ab-$label" "ab-$label" --scopes kb:read,kb:write | grep -oE 'rkb_[A-Za-z0-9_-]+' | head -n 1)"
  test -n "$key"
  rm -rf "${OUT:?}/$label"
  start=$(date +%s)
  "$CLIENT_BIN" eval \
    --base-url "$KB_STACK_BASE_URL" \
    --api-key "$key" \
    --dataset "$DATASET" \
    --out "$OUT/$label" \
    --label "$label"
  echo "$label: $(( $(date +%s) - start )) s wall (ingest + queries)" | tee -a "$OUT/timings.txt"
done

"$SCRIPT_DIR/stack-down.sh" >/dev/null 2>&1 || true
echo "results in $OUT"
