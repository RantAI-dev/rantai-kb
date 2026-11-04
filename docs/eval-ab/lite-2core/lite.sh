#!/usr/bin/env bash
# Lite tier on 2 cores: CPU embedder (TEI) pinned to cores 0-1, kb-server pinned to 0-1, no reranker, no page arm.
# MODEL / QPREFIX / PPREFIX / LABEL / DS / RERANK (none|tei)
set -euo pipefail
cd /home/shiro/rantai-kb
MODEL=${MODEL:-Qwen/Qwen3-Embedding-0.6B}; LABEL=${LABEL:-lite-q06}; PORT=19400
docker rm -f lite-tei >/dev/null 2>&1 || true
docker run -d --name lite-tei --cpuset-cpus=0,1 ${TEI_THREADS:+-e OMP_NUM_THREADS=$TEI_THREADS -e RAYON_NUM_THREADS=$TEI_THREADS -e MKL_NUM_THREADS=$TEI_THREADS} -p $PORT:80 -v $HOME/.cache/kb-models:/data \
  ghcr.io/huggingface/text-embeddings-inference:cpu-latest --model-id "$MODEL" --max-batch-tokens 4096 --max-client-batch-size 32 >/dev/null
for _ in $(seq 1 600); do curl -sf http://127.0.0.1:$PORT/health >/dev/null && break; sleep 2; done
CACHE=$(mktemp -d /tmp/lite-cache.XXXX); trap 'docker rm -f lite-tei >/dev/null 2>&1; rm -rf "$CACHE"' EXIT
export KB_CHUNK_STORE=qdrant KB_STACK_EMBEDDINGS=remote KB_STACK_REMOTE_EMBED_URL=http://127.0.0.1:$PORT KB_STACK_REMOTE_EMBED_KEY=none \
  KB_STACK_REMOTE_EMBED_MODEL="$MODEL" KB_STACK_REMOTE_EMBED_DIM=${DIM:-1024} KB_STACK_RERANK=${RERANK:-none} \
  KB_STACK_CACHE_DIR=$CACHE KB_AB_TIMEOUT=7200 KB_AB_MAX_CHUNKS=20 \
  KB_AB_DATASET=$PWD/rust/crates/kb-eval/datasets/${DS:-vidore3-hr-en} KB_AB_OUT=$HOME/kbtools/lite
rm -rf "$HOME/kbtools/lite/$LABEL"
KB_STACK_SERVER_ENV="KB_TIER=lite ${PPREFIX:+KB_EMBEDDING_PASSAGE_PREFIX=$PPREFIX} ${EXTRA:-}" \
  rust/scripts/eval-ab.sh "$LABEL=$HOME/kbtools/kb-server-2core"
sed 's/\x1b\[[0-9;]*m//g' /tmp/kb-stack/server.log | grep -E "ingest claimed|ingest finished" | head -40 > "$HOME/kbtools/lite/$LABEL.ingest.log" || true
