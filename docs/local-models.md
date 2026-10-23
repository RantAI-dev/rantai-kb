# Local model profile (CPU, zero cost)

Measured 2026-10-16 on the dev box: 6 cores, 11 GB RAM, no GPU. Hugging Face
Text Embeddings Inference (TEI), image `ghcr.io/huggingface/text-embeddings-inference:cpu-latest`.
Weights are cached in `~/.cache/kb-models` (3.3 GB for both models), so they download once.

kb-server needs **no code change** for either model. `kb-inference` already speaks OpenAI
`/v1/embeddings` and TEI `/rerank`.

## Embeddings: Qwen3-Embedding-0.6B

```sh
docker run -d --name kb-smoke-tei-embed -p 18081:80 -v ~/.cache/kb-models:/data \
  ghcr.io/huggingface/text-embeddings-inference:cpu-latest \
  --model-id Qwen/Qwen3-Embedding-0.6B --max-batch-tokens 4096 --max-client-batch-size 32
```

```sh
KB_EMBEDDING_BASE_URL=http://127.0.0.1:18081/v1/embeddings
KB_EMBEDDING_MODEL=Qwen/Qwen3-Embedding-0.6B
KB_EMBEDDING_DIM=1024
KB_EMBEDDING_QUERY_PREFIX="Instruct: Given a question, retrieve passages that answer it\nQuery: "
```

| Measure | Value |
|---|---|
| Dimension | 1024 |
| RAM | 2.5 GB |
| One short query, warm | 0.33–0.35 s (3 runs) |

`--max-batch-tokens` is required. With the default, warm-up tries to allocate 16 GB and
aborts.

## Reranker: bge-reranker-v2-m3

```sh
docker run -d --name kb-smoke-tei-rerank -p 18082:80 -v ~/.cache/kb-models:/data \
  ghcr.io/huggingface/text-embeddings-inference:cpu-latest \
  --model-id BAAI/bge-reranker-v2-m3 --max-batch-tokens 2048 --max-client-batch-size 64 --auto-truncate
```

```sh
KB_RERANK_ENABLED=true
KB_RERANK_PROVIDER=tei
KB_RERANK_BASE_URL=http://127.0.0.1:18082
KB_RERANK_MODEL=BAAI/bge-reranker-v2-m3
```

| Measure | Value |
|---|---|
| RAM | 5.6 GB |
| 30 short Indonesian candidates, warm | 3.9–4.2 s (3 runs) |
| Right passage ranked first | yes (score 0.995) |

With `--max-batch-tokens 8192` the container was OOM-killed (exit 137) during warm-up.

## What this means for the plan

- **Quality evaluation is unblocked at zero cost.** A few hundred queries at about 4 s each is
  fine offline.
- **Latency targets are not reachable with this CPU setup.** The plan asks for search p95
  < 80 ms without rerank and < 300 ms with it. Query embedding alone costs about 340 ms, and
  rerank costs about 4 s. The embedding cost is per query, so the query cache
  (`kb-inference/src/query_cache.rs`) only helps repeats.
- **Options to measure before choosing:**
  - an ONNX/int8 or GGUF (llama.cpp) build of the same models;
  - a smaller multilingual reranker;
  - fewer rerank candidates;
  - a GPU box or a cloud profile for production.

  Record each option's quality delta and latency with `kb-eval`. Do not pick one by
  intuition.
- **Both models together use about 8 GB.** On this 11 GB box they cannot run alongside the
  full stack plus a build. Run them one at a time, or embed the corpus first and rerank after.

## GPU profile: llama.cpp on a LAN box (measured 2026-10-23)

The recommended local profile. Both models run on a Windows PC with an AMD RX 590 (8 GB,
Vulkan), served by `llama server`. kb-server reaches them over the LAN. Results are in
`docs/eval-ab/README.md`.

```powershell
# Windows box, one window each; $key is the access key
llama server -hf Qwen/Qwen3-Embedding-0.6B-GGUF:Q8_0 --embedding --pooling last --device Vulkan0 -ngl 99 -c 8192 -b 8192 -ub 8192 --host 0.0.0.0 --port 8081 --api-key $key
llama server -hf gpustack/bge-reranker-v2-m3-GGUF:Q8_0 --reranking --device Vulkan0 -ngl 99 -c 8192 -b 8192 -ub 8192 --host 0.0.0.0 --port 8082 --api-key $key
```

```sh
KB_EMBEDDING_BASE_URL=http://<gpu-host>:8081/v1/embeddings
KB_EMBEDDING_API_KEY=<key>
KB_EMBEDDING_MODEL=Qwen/Qwen3-Embedding-0.6B
KB_EMBEDDING_DIM=1024
KB_EMBEDDING_QUERY_PREFIX="Instruct: Given a question, retrieve passages that answer it\nQuery: "
KB_RERANK_ENABLED=true
KB_RERANK_PROVIDER=cohere          # llama.cpp's /v1/rerank takes the Cohere request shape
KB_RERANK_BASE_URL=http://<gpu-host>:8082/v1/rerank
KB_RERANK_API_KEY=<key>
KB_RERANK_MODEL=bge-reranker-v2-m3
KB_RERANK_CANDIDATE_LIMIT=20       # 10 for ~0.8 s/query at a small quality cost
```

| Measure (RX 590) | Value |
|---|---|
| Embedding, Q8 vs TEI fp32 | cosine 0.9997 on the same texts |
| Embedding throughput | ~1,450 tokens/s; ingest of 2,933 passages ~9.5 min |
| One query embedding | ~70 ms |
| Search with rerank, top 20 / top 10 | ~1.4 s / ~0.8 s per query |
| nDCG@10, rerank top 20 vs none | miracl-id-200 0.830 vs 0.793; hotpot-200-test 0.928 vs 0.820 |

The RX 590 has no fp16 or int-dot support (llama bench pp512: 1,407 tokens/s on the GPU,
217 on its CPU). A newer card cuts the reranker time several-fold. Qwen3-Reranker-0.6B
(`ggml-org/Qwen3-Reranker-0.6B-Q8_0-GGUF`) is better on Indonesian and worse on English
than bge, and 2.4× slower, so it is not the default.
