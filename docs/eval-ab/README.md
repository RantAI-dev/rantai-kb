# Live retrieval results, local models (2026-10-23)

**Best measured setup:** k=60 fusion (`4c01a7d`), then **bge-reranker-v2-m3 on the top 20
fused candidates**, everything served by llama.cpp on the LAN RX 590. Against the same
engine with no reranker:

- MIRACL-id: nDCG@10 0.793 → 0.830.
- hotpot: nDCG@10 0.820 → 0.928.

No query failed or degraded in any run.

All runs are live: `kb-server` (release) on Qdrant, ingest through the HTTP API, scored by
`kb-server eval`. Embeddings come from Qwen3-Embedding-0.6B Q8_0 on llama.cpp (Vulkan,
RX 590), which matches TEI fp32 at cosine 0.9997. Each dataset is ingested once
(`eval-ab.sh`), then queried under each reranker setting (`eval-variants.sh`, which restarts
the server with new settings on the same ingest). Every run requests 10 chunks per query
(`--max-chunks 10`), because the engine only reranks when it holds more candidates than the
caller asked for.

## MIRACL-id (200 Indonesian queries, 2,933 passages)

| Setup | nDCG@10 | MRR@10 | Hit@1 | Recall@10 | s/query |
|---|---:|---:|---:|---:|---:|
| k60, no reranker | 0.793 | 0.824 | 0.720 | 0.927 | – |
| + bge top 10 | 0.824 | 0.858 | 0.775 | 0.927 | 0.8 |
| + bge top 20 | 0.830 | 0.857 | 0.770 | 0.939 | 1.4 |
| + Qwen3-Reranker-0.6B top 20 | **0.849** | **0.879** | **0.805** | **0.946** | 3.4 |

Paired deltas against k60 with no reranker (10,000 bootstrap resamples):

| Setup | nDCG@10 Δ [95% CI] |
|---|---|
| bge top 10 | +0.031 [+0.010, +0.053] |
| bge top 20 | +0.037 [+0.014, +0.061] |
| Qwen3 top 20 | +0.056 [+0.030, +0.082] |

Qwen3 against bge, both top 20: +0.019 [+0.003, +0.036].

## hotpot-200-test (100 English multi-hop queries, 995 paragraphs)

| Setup | nDCG@10 | MRR@10 | Hit@1 | Recall@10 | s/query |
|---|---:|---:|---:|---:|---:|
| k60, no reranker | 0.820 | 0.908 | 0.850 | 0.905 | – |
| + bge top 10 | 0.900 | 0.990 | 0.980 | 0.905 | 0.8 |
| + bge top 20 | **0.928** | **0.990** | **0.980** | **0.945** | 1.5 |
| + Qwen3-Reranker-0.6B top 20 | 0.889 | 0.955 | 0.920 | 0.940 | 2.7 |

Paired deltas against k60 with no reranker:

| Setup | nDCG@10 Δ [95% CI] |
|---|---|
| bge top 10 | +0.080 [+0.057, +0.105] |
| bge top 20 | +0.108 [+0.076, +0.142] |
| Qwen3 top 20 | +0.070 [+0.040, +0.101] |

Qwen3 against bge, both top 20: **−0.038** [−0.059, −0.020].

## Decision

- **Reranker: bge-reranker-v2-m3, top 20.** It is significantly better than no reranker on
  both datasets. Qwen3-Reranker-0.6B is better on Indonesian (+0.019) and significantly worse
  on English (−0.038), at 2.4× the latency, so it is not the default.
- **Settings for the local profile:**

  ```
  KB_RERANK_ENABLED=true
  KB_RERANK_PROVIDER=cohere   # llama.cpp /v1/rerank takes the Cohere shape
  KB_RERANK_BASE_URL=http://<gpu-host>:8082/v1/rerank
  KB_RERANK_API_KEY=<llama key>
  KB_RERANK_CANDIDATE_LIMIT=20
  ```

  For lower latency use `KB_RERANK_CANDIDATE_LIMIT=10` (0.8 s/query on the RX 590), which
  gives up 0.006 nDCG on MIRACL and 0.028 on hotpot.
- **Fusion: k=60.** `f8c0639`'s k=2 lost 0.021 nDCG@10 live (`miracl-id-200/README.md`) and
  is reverted. The offline sweep (`docs/eval-tune/miracl-id-200`) puts every RRF k from 20 to
  60 within 0.002, and the best score blend within noise of k=60 on the held-out half.

## Caveats

- **Latency is the RX 590's.** It has no fp16 or int-dot support; pp512 is 1,407 tok/s. A
  modern GPU would cut the reranker time several-fold. The time is per query, sequential,
  through the whole `/v1/search`.
- **MIRACL was used for tuning.** The fusion sweep and the reranker choice were tuned on the
  tune half of the 200 queries. hotpot-200-test was not used for tuning and confirms the
  reranker choice.
- **Different chunk count from the A/B runs.** The k=2 A/B in `miracl-id-200/README.md`
  requested 50 chunks per query; these runs request 10. Compare rows within one table only.
