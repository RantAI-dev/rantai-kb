#!/usr/bin/env bash
# Continuous integration retrieval quality and scope isolation gate.
#
# Runs the live smoke stack, runs kb-eval on fixtures/tiny, compares metrics
# against the committed baseline (tiny-stub.metrics.json), runs the cross-scope
# leak matrix, and verifies all artifacts are generated.
#
# Deterministic plumbing numbers: the smoke stack uses stub_embeddings (dim 8),
# so results are reproducible and have zero external dependencies or API costs.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$WORKSPACE_ROOT/.." && pwd)"

STACK_DIR="${KB_STACK_DIR:-/tmp/kb-stack}"
EVAL_OUT_DIR="${KB_EVAL_OUT_DIR:-$REPO_ROOT/eval-out}"
BASELINE_FILE="${KB_EVAL_BASELINE:-$WORKSPACE_ROOT/crates/kb-eval/baselines/tiny-stub.metrics.json}"
MAX_DROP="${KB_EVAL_MAX_DROP:-0.02}"
METRICS="${KB_EVAL_METRICS:-ndcg,hit1,recall}"

cleanup() {
  echo ""
  echo "--- Cleaning up eval gate stack ---"
  "$SCRIPT_DIR/stack-down.sh" || true
}
trap cleanup EXIT

echo "=== 1. Checking libpdfium ==="
if [ -z "${PDFIUM_DYNAMIC_LIB_PATH:-}" ]; then
  if [ ! -f "$WORKSPACE_ROOT/.vendor/pdfium/lib/libpdfium.so" ]; then
    echo "libpdfium not found; fetching..."
    "$SCRIPT_DIR/fetch-pdfium.sh" "$WORKSPACE_ROOT/.vendor/pdfium"
  fi
  export PDFIUM_DYNAMIC_LIB_PATH="$WORKSPACE_ROOT/.vendor/pdfium/lib/libpdfium.so"
fi
echo "Using PDFIUM_DYNAMIC_LIB_PATH=$PDFIUM_DYNAMIC_LIB_PATH"

echo "=== 2. Starting kb-smoke stack ==="
"$SCRIPT_DIR/stack-up.sh"
# shellcheck disable=SC1090
source "$STACK_DIR/stack.env"

echo "=== 3. Minting API keys ==="
# Dedicated tenant for quality evaluation
KEY_EVAL_OUTPUT=$("$KB_SERVER_BIN" create-api-key "eval-tenant" "eval-key" --scopes kb:read,kb:write)
KEY_EVAL=$(echo "$KEY_EVAL_OUTPUT" | grep -oE 'rkb_[A-Za-z0-9_-]+' | head -n 1)
test -n "$KEY_EVAL"

# Dedicated tenants for leak matrix red-team probes
KEY_A_OUTPUT=$("$KB_SERVER_BIN" create-api-key "leak-a" "leak-a-key" --scopes kb:read,kb:write)
KEY_A=$(echo "$KEY_A_OUTPUT" | grep -oE 'rkb_[A-Za-z0-9_-]+' | head -n 1)
test -n "$KEY_A"

KEY_B_OUTPUT=$("$KB_SERVER_BIN" create-api-key "leak-b" "leak-b-key" --scopes kb:read,kb:write)
KEY_B=$(echo "$KEY_B_OUTPUT" | grep -oE 'rkb_[A-Za-z0-9_-]+' | head -n 1)
test -n "$KEY_B"
echo "API keys minted successfully."

echo "=== 4. Running retrieval evaluation on fixtures/tiny ==="
rm -rf "$EVAL_OUT_DIR"
mkdir -p "$EVAL_OUT_DIR/tiny"
"$KB_SERVER_BIN" eval \
  --base-url "$KB_STACK_BASE_URL" \
  --api-key "$KEY_EVAL" \
  --dataset "$WORKSPACE_ROOT/crates/kb-eval/fixtures/tiny" \
  --out "$EVAL_OUT_DIR/tiny" \
  --label ci-gate

echo "=== 5. Comparing results against baseline ==="
echo "Baseline: $BASELINE_FILE"
test -f "$BASELINE_FILE" || { echo "ERROR: Baseline file $BASELINE_FILE does not exist!" >&2; exit 1; }

"$KB_SERVER_BIN" eval compare \
  --baseline "$BASELINE_FILE" \
  --current "$EVAL_OUT_DIR/tiny/metrics.json" \
  --max-drop "$MAX_DROP" \
  --metric "$METRICS"

echo "=== 6. Running cross-scope leak matrix ==="
mkdir -p "$EVAL_OUT_DIR/leak"
"$KB_SERVER_BIN" eval leak \
  --base-url "$KB_STACK_BASE_URL" \
  --api-key "$KEY_A" \
  --peer-key "$KEY_B" \
  --out "$EVAL_OUT_DIR/leak"

echo "=== 7. Verifying artifacts ==="
test -f "$EVAL_OUT_DIR/tiny/report.md"
test -f "$EVAL_OUT_DIR/tiny/metrics.json"
test -f "$EVAL_OUT_DIR/leak/leak-report.md"
echo "All eval gate artifacts verified."
echo "Evaluation gate PASSED."
