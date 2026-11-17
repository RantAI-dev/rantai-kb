# Reranking bake-off

- datasets: hotpot-200-test, miracl-id-200
- candidate pool: top-50 of `qwen3-embed-0.6b`, documents truncated to 2000 chars
- no rerank (hotpot-200-test): nDCG@10 0.8565 MRR@10 0.9240 Hit@1 0.8800
- no rerank (miracl-id-200): nDCG@10 0.7910 MRR@10 0.8206 Hit@1 0.7350

## Overall

| model | nDCG@10 | MRR@10 | Hit@1 | p50 ms | p95 ms | Δ nDCG vs no rerank | Δ nDCG vs baseline reranker | licence |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| `qwen3-rerank-0.6b` | 0.8792 | 0.9014 | 0.8367 | - | - | +0.0664 [+0.0491, +0.0842] | +0.0011 [-0.0131, +0.0157] | apache-2.0 |
| `bge-reranker-v2-m3` | 0.8781 | 0.8967 | 0.8333 | - | - | +0.0653 [+0.0475, +0.0827] | - | apache-2.0 |
| `nemotron-rerank-1b-v2` | 0.8769 | 0.8966 | 0.8433 | - | - | +0.0641 [+0.0474, +0.0809] | -0.0012 [-0.0127, +0.0104] | other (NVIDIA Open Model License) |

## Per dataset

### hotpot-200-test

| model | nDCG@10 | MRR@10 | Hit@1 | Recall@10 | Δ nDCG vs no rerank | Δ nDCG vs baseline reranker |
|---|---:|---:|---:|---:|---:|---:|
| `nemotron-rerank-1b-v2` | 0.9577 | 0.9850 | 0.9700 | 0.9950 | +0.1012 [+0.0724, +0.1313] | +0.0047 [-0.0109, +0.0210] |
| `bge-reranker-v2-m3` | 0.9530 | 0.9900 | 0.9800 | 0.9850 | +0.0965 [+0.0672, +0.1277] | - |
| `qwen3-rerank-0.6b` | 0.9062 | 0.9492 | 0.9100 | 0.9800 | +0.0497 [+0.0212, +0.0783] | -0.0468 [-0.0708, -0.0251] |

### miracl-id-200

| model | nDCG@10 | MRR@10 | Hit@1 | Recall@10 | Δ nDCG vs no rerank | Δ nDCG vs baseline reranker |
|---|---:|---:|---:|---:|---:|---:|
| `qwen3-rerank-0.6b` | 0.8657 | 0.8776 | 0.8000 | 0.9791 | +0.0747 [+0.0531, +0.0974] | +0.0250 [+0.0079, +0.0434] |
| `bge-reranker-v2-m3` | 0.8407 | 0.8500 | 0.7600 | 0.9641 | +0.0497 [+0.0284, +0.0714] | - |
| `nemotron-rerank-1b-v2` | 0.8366 | 0.8524 | 0.7800 | 0.9602 | +0.0456 [+0.0253, +0.0659] | -0.0041 [-0.0194, +0.0121] |

## FAILED

None: every model ran.
