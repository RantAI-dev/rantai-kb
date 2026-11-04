#!/usr/bin/env bash
# Query-only variants on ONE ingest: the corpus is ingested once, then
# kb-server is restarted with each variant's environment and the queries are
# re-run with --skip-ingest. For search-side settings (reranker, pool size,
# limits): anything that does not change what was indexed.
#
#   rust/scripts/eval-variants.sh <base-label> <variant-label>='KEY=VAL KEY=VAL' ...
#
# The stack must already be up (stack-up.sh) and <base-label> must already be
# ingested into $KB_AB_OUT/<base-label> (eval-ab.sh or a plain `kb-server eval`).
# Each variant's KEY=VAL pairs replace those keys in stack.env; every
# KB_RERANK_* key the variant does not set is reset to off first, so variants
# do not inherit each other's reranker.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$WORKSPACE_ROOT/.." && pwd)"
DATASET="${KB_AB_DATASET:-$WORKSPACE_ROOT/crates/kb-eval/datasets/miracl-id-200}"
OUT="${KB_AB_OUT:-$REPO_ROOT/docs/eval-ab/$(basename "$DATASET")}"
CLIENT_BIN="${KB_AB_CLIENT_BIN:-$WORKSPACE_ROOT/target/release/kb-server}"
STACK_DIR="${KB_STACK_DIR:-/tmp/kb-stack}"
STACK_ENV="$STACK_DIR/stack.env"
MAX_CHUNKS="${KB_AB_MAX_CHUNKS:-10}"

base="$1"
shift
[ -f "$OUT/$base/ingest-map.json" ] || { echo "no ingest at $OUT/$base" >&2; exit 1; }
[ -f "$STACK_ENV" ] || { echo "no stack at $STACK_ENV" >&2; exit 1; }
cp "$STACK_ENV" "$STACK_DIR/stack.env.orig"

for spec in "$@"; do
  label="${spec%%=*}"
  vars="${spec#*=}"
  [ "$label" = "$spec" ] && vars=""
  echo "=== variant $label: ${vars:-(base settings)} ==="
  grep -v -E '^export KB_RERANK_' "$STACK_DIR/stack.env.orig" > "$STACK_ENV"
  echo 'export KB_RERANK_ENABLED="false"' >> "$STACK_ENV"
  for kv in $vars; do
    key="${kv%%=*}"
    grep -v -E "^export ${key}=" "$STACK_ENV" > "$STACK_ENV.new" || true
    mv "$STACK_ENV.new" "$STACK_ENV"
    echo "export ${key}=\"${kv#*=}\"" >> "$STACK_ENV"
  done
  chmod 600 "$STACK_ENV"
  # shellcheck disable=SC1090
  source "$STACK_ENV"
  KB_STACK_RESTART_SERVER=1 "$SCRIPT_DIR/stack-up.sh" >/dev/null
  # The ingest belongs to the base arm's tenant: query as that tenant.
  key="$(cat "$STACK_DIR/ab-key-$base")"
  rm -rf "${OUT:?}/$label"
  mkdir -p "$OUT/$label"
  cp "$OUT/$base/ingest-map.json" "$OUT/$label/"
  start=$(date +%s)
  "$CLIENT_BIN" eval --base-url "$KB_STACK_BASE_URL" --api-key "$key" --dataset "$DATASET" \
    --out "$OUT/$label" --label "$label" --skip-ingest --max-chunks "$MAX_CHUNKS"
  echo "$label: $(( $(date +%s) - start )) s for $(wc -l < "$DATASET/queries.jsonl") queries" | tee -a "$OUT/timings.txt"
done
cp "$STACK_DIR/stack.env.orig" "$STACK_ENV"
