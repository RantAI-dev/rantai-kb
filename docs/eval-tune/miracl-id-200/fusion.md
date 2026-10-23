# Fusion tuning (offline replay)

dataset `miracl-id-200`, 200 queries, tune 100 / test 100 (seed 42), embedder `Qwen/Qwen3-Embedding-0.6B` at `http://192.168.18.92:8081/v1/embeddings`

## Reproduction (all queries; must match the live runs)

| config | nDCG@10 | MRR@10 | Hit@1 | Recall@10 |
|---|---:|---:|---:|---:|
| live old: title, k=2 then k=60 re-sort, pool 20 | 0.796 | 0.818 | 0.715 | 0.941 |
| live fusion-only: title, k=2, pool 20 | 0.780 | 0.791 | 0.685 | 0.945 |
| live new: emb, k=2, pool 20 | 0.780 | 0.791 | 0.685 | 0.945 |

## Top 15 on the tune half (nDCG@10)

| config | tune nDCG@10 | tune MRR@10 |
|---|---:|---:|
| title: convex max a=0.6 pool 100 | 0.819 | 0.831 |
| emb: convex max a=0.6 pool 100 | 0.819 | 0.831 |
| title: convex z a=0.5 pool 100 | 0.816 | 0.834 |
| emb: convex z a=0.5 pool 100 | 0.816 | 0.834 |
| title: convex z a=0.45 pool 100 | 0.815 | 0.829 |
| emb: convex z a=0.45 pool 100 | 0.815 | 0.829 |
| title: convex max a=0.75 pool 100 | 0.815 | 0.834 |
| emb: convex max a=0.75 pool 100 | 0.815 | 0.834 |
| title: convex max a=0.6 pool 50 | 0.815 | 0.831 |
| emb: convex max a=0.6 pool 50 | 0.815 | 0.831 |
| title: convex z a=0.55 pool 100 | 0.814 | 0.829 |
| emb: convex z a=0.55 pool 100 | 0.814 | 0.829 |
| title: convex minmax a=0.45 pool 100 | 0.814 | 0.830 |
| emb: convex minmax a=0.45 pool 100 | 0.814 | 0.830 |
| title: convex minmax a=0.55 pool 100 | 0.813 | 0.839 |

## Held-out test half (100 queries), paired against `live old: title, k=2 then k=60 re-sort, pool 20`

| config | nDCG@10 | Δ nDCG [95% CI] | MRR@10 | Δ MRR [95% CI] | Hit@1 |
|---|---:|---|---:|---|---:|
| live old: title, k=2 then k=60 re-sort, pool 20 | 0.793 | +0.000 [+0.000, +0.000] | 0.814 | +0.000 [+0.000, +0.000] | 0.700 |
| title: convex max a=0.6 pool 100 | 0.806 | +0.013 [-0.011, +0.037] | 0.806 | -0.008 [-0.043, +0.027] | 0.680 |
| live new: emb, k=2, pool 20 | 0.775 | -0.018 [-0.043, +0.006] | 0.771 | -0.043 [-0.078, -0.010] | 0.660 |
| emb: dense only | 0.709 | -0.085 [-0.122, -0.050] | 0.735 | -0.079 [-0.128, -0.031] | 0.620 |
| title: dense only | 0.709 | -0.085 [-0.122, -0.050] | 0.735 | -0.079 [-0.128, -0.031] | 0.620 |
