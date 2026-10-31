#!/usr/bin/env bash
# rantai-kb cloud tier (preset values written out, since stack-up sets some
# roles explicitly) on MIRACL-id-200, ViDoRe HR and CS, then text-only variants.
set -uo pipefail
REPO=~/rantai-kb
OUTROOT=$REPO/docs/eval-ab/rk-cloud-vs-kf
LIMIT_USD=${LIMIT_USD:-5}
OPENROUTER_API_KEY=$(grep -E '^OPENROUTER_API_KEY2=' $REPO/.env | head -1 | cut -d= -f2- | tr -d "\"' \r")
export OPENROUTER_API_KEY
usage() { curl -s --max-time 15 -H "Authorization: Bearer $OPENROUTER_API_KEY" https://openrouter.ai/api/v1/key | python3 -c 'import sys,json
try: print(json.load(sys.stdin)["data"]["usage"])
except Exception: sys.exit(1)' 2>/dev/null; }
check() { u=$(usage) || u=0; echo "[spend] $(date +%T) usage=\$$u (limit \$$LIMIT_USD) $1"; python3 -c "import sys;sys.exit(0 if float('$u')<$LIMIT_USD else 1)" || { echo "[spend] LIMIT HIT, stopping"; $REPO/rust/scripts/stack-down.sh >/dev/null 2>&1; exit 3; }; }

# watchdog: hard stop even mid-arm
( while sleep 60; do u=$(usage) || continue; python3 -c "import sys;sys.exit(0 if float('$u')<$LIMIT_USD else 1)" || { echo "[watchdog] usage \$$u >= \$$LIMIT_USD, stack-down"; $REPO/rust/scripts/stack-down.sh >/dev/null 2>&1; pkill -f 'kb-server eval' ; exit; }; done ) &
WATCH=$!
trap 'kill $WATCH 2>/dev/null' EXIT

export KB_SERVER_BIN=~/kb-target-alt/release/kb-server KB_AB_CLIENT_BIN=~/kb-target-alt/release/kb-server
export KB_STACK_EMBEDDINGS=openrouter KB_STACK_OR_MODEL=voyageai/voyage-multimodal-3.5 KB_STACK_OR_DIM=1024
export KB_STACK_CACHE_DIR=~/.cache/kb-cache KB_AB_TIMEOUT=3600 KB_AB_KEEP=1
export KB_STACK_SERVER_ENV="KB_TIER=cloud KB_EMBEDDING_INPUT_TYPE=true KB_RERANK_ENABLED=true KB_RERANK_PROVIDER=cohere KB_RERANK_BASE_URL=https://openrouter.ai/api/v1/rerank KB_RERANK_MODEL=voyageai/rerank-2.5 KB_RERANK_CANDIDATE_LIMIT=20 KB_PAGE_EMBEDDING_MODEL=voyageai/voyage-multimodal-3.5 KB_PAGE_EMBEDDING_DIM=1024 KB_PAGE_RERANK=true KB_MAX_FILE_BYTES=104857600"
TEXT_ONLY="KB_PAGE_EMBEDDING_MODEL= KB_RERANK_ENABLED=true KB_RERANK_PROVIDER=cohere KB_RERANK_BASE_URL=https://openrouter.ai/api/v1/rerank KB_RERANK_MODEL=voyageai/rerank-2.5 KB_RERANK_CANDIDATE_LIMIT=20"

check "start"
for ds in ${DATASETS:-miracl-id-200 vidore3-hr-en vidore3-cs-en}; do
  export KB_AB_DATASET=$REPO/rust/crates/kb-eval/datasets/$ds KB_AB_OUT=$OUTROOT/$ds
  echo "=== $ds cloud $(date +%T)"
  nice -n 10 $REPO/rust/scripts/eval-ab.sh rk-cloud=$KB_SERVER_BIN || echo "$ds cloud FAILED"
  check "after $ds cloud"
  case $ds in vidore*)
    echo "=== $ds text-only variant $(date +%T)"
    KB_AB_MAX_CHUNKS=20 nice -n 10 $REPO/rust/scripts/eval-variants.sh rk-cloud "rk-cloud-text=$TEXT_ONLY" || echo "$ds text FAILED"
    check "after $ds text";;
  esac
  $REPO/rust/scripts/stack-down.sh >/dev/null 2>&1 || true
done
echo "=== done $(date +%T)"
