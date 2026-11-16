# EVAL-3 zero-cost `--no-llm` measurements — exact tool output

This file is the raw record behind the summary in `README.md`. Everything below
was produced on 2026-11-04 from the committed `communities.json` files, the
human ViDoRe qrels subsets in this directory, and the existing cloud baseline
runs `docs/eval-ab/rk-cloud-vs-kf/<dataset>/rk-cloud/run.page.trec` (no new live
search was run).

**What "no-LLM" means here.** `community_summarise.py --no-llm` replaces each
community summary with that community's concatenated entity names. The arm then
ranks communities by BM25 over those names and pages by query-entity overlap,
and `global_fuse.py` RRF-fuses it with the baseline (k=60, global weight 0.5).
It is a zero-cost lower bound on "does knowing which entities cluster together
help?", not the global mode EVAL-3 asks about. Do not read it as a verdict on
LLM community summaries.

Arms:

- `global.no-llm` — the global arm alone (no baseline pages in the run).
- `fused.all` — RRF with every query, `--gate none`.
- `fused.gated` — RRF only for queries the lexical gate labels aggregative
  (`--gate aggregative`); the gate has precision 0.323 / recall 0.782 against
  the human labels, so it also fuses 195 non-aggregative queries.
- `fused.oracle` — RRF only for the queries in this set's
  `queries.aggregative.jsonl` (`--gate list`). An oracle **gate**, not an oracle
  arm: the arm itself is unchanged. Its zero delta on the non-aggregative
  subset is a construction property, printed as a plumbing check.

## Arm diagnostics (zero cost)

| corpus | aggregative q | q with ≥1 gold page in selected top-3 communities | gold pages covered by selected communities | oracle best single community | oracle best 3 communities |
|---|---|---|---|---|---|
| vidore3-hr-en | 66 | 33/66 (0.50) | 115/395 (0.29) | 0.62 | 0.91 |
| vidore3-cs-en | 53 | 28/53 (0.53) | 103/310 (0.33) | 0.67 | 0.95 |

The partition can hold the answer (best 3 communities cover 91% / 95% of gold
pages); selecting communities by BM25 over entity names finds them only
29% / 33% of the time. That is where the no-LLM arm loses its recall.

## Exact printed output

````text
### vidore3-hr-en / aggregative / baseline vs global.no-llm
Evaluated 66 queries (vidore3-hr-en aggregative global.no-llm).
| Metric | Off (Baseline) | On (Treatment) | Delta | 95% Bootstrap CI | n |
|---|---|---|---|---|---|
| recall@5 | 0.5129 | 0.1149 | -0.3980 | [-0.4803, -0.3171] | 66 |
| recall@10 | 0.6680 | 0.1756 | -0.4924 | [-0.5795, -0.4050] | 66 |
| recall@20 | 0.7769 | 0.2326 | -0.5443 | [-0.6335, -0.4540] | 66 |
| mrr@10 | 0.7961 | 0.2441 | -0.5519 | [-0.6728, -0.4247] | 66 |
| ndcg@10 | 0.6676 | 0.1663 | -0.5013 | [-0.5820, -0.4197] | 66 |

### vidore3-hr-en / aggregative / baseline vs fused.all
Evaluated 66 queries (vidore3-hr-en aggregative fused.all).
| Metric | Off (Baseline) | On (Treatment) | Delta | 95% Bootstrap CI | n |
|---|---|---|---|---|---|
| recall@5 | 0.5129 | 0.3603 | -0.1526 | [-0.2209, -0.0898] | 66 |
| recall@10 | 0.6680 | 0.5812 | -0.0868 | [-0.1359, -0.0397] | 66 |
| recall@20 | 0.7769 | 0.7783 | +0.0013 | [-0.0252, +0.0286] | 66 |
| mrr@10 | 0.7961 | 0.6199 | -0.1762 | [-0.2810, -0.0706] | 66 |
| ndcg@10 | 0.6676 | 0.5197 | -0.1479 | [-0.2062, -0.0911] | 66 |

### vidore3-hr-en / aggregative / baseline vs fused.gated
Evaluated 66 queries (vidore3-hr-en aggregative fused.gated).
| Metric | Off (Baseline) | On (Treatment) | Delta | 95% Bootstrap CI | n |
|---|---|---|---|---|---|
| recall@5 | 0.5129 | 0.4024 | -0.1106 | [-0.1725, -0.0541] | 66 |
| recall@10 | 0.6680 | 0.5968 | -0.0712 | [-0.1135, -0.0341] | 66 |
| recall@20 | 0.7769 | 0.7638 | -0.0132 | [-0.0345, +0.0045] | 66 |
| mrr@10 | 0.7961 | 0.6793 | -0.1167 | [-0.2122, -0.0217] | 66 |
| ndcg@10 | 0.6676 | 0.5556 | -0.1120 | [-0.1613, -0.0629] | 66 |

### vidore3-hr-en / aggregative / baseline vs fused.oracle
Evaluated 66 queries (vidore3-hr-en aggregative fused.oracle).
| Metric | Off (Baseline) | On (Treatment) | Delta | 95% Bootstrap CI | n |
|---|---|---|---|---|---|
| recall@5 | 0.5129 | 0.3603 | -0.1526 | [-0.2209, -0.0898] | 66 |
| recall@10 | 0.6680 | 0.5812 | -0.0868 | [-0.1359, -0.0397] | 66 |
| recall@20 | 0.7769 | 0.7783 | +0.0013 | [-0.0252, +0.0286] | 66 |
| mrr@10 | 0.7961 | 0.6199 | -0.1762 | [-0.2810, -0.0706] | 66 |
| ndcg@10 | 0.6676 | 0.5197 | -0.1479 | [-0.2062, -0.0911] | 66 |

### vidore3-hr-en / nonaggregative / baseline vs global.no-llm
Evaluated 252 queries (vidore3-hr-en nonaggregative global.no-llm).
| Metric | Off (Baseline) | On (Treatment) | Delta | 95% Bootstrap CI | n |
|---|---|---|---|---|---|
| recall@5 | 0.6401 | 0.1136 | -0.5265 | [-0.5752, -0.4778] | 252 |
| recall@10 | 0.7494 | 0.1590 | -0.5904 | [-0.6378, -0.5420] | 252 |
| recall@20 | 0.8303 | 0.1938 | -0.6365 | [-0.6835, -0.5876] | 252 |
| mrr@10 | 0.8133 | 0.1708 | -0.6425 | [-0.6942, -0.5881] | 252 |
| ndcg@10 | 0.7214 | 0.1342 | -0.5873 | [-0.6283, -0.5445] | 252 |

### vidore3-hr-en / nonaggregative / baseline vs fused.all
Evaluated 252 queries (vidore3-hr-en nonaggregative fused.all).
| Metric | Off (Baseline) | On (Treatment) | Delta | 95% Bootstrap CI | n |
|---|---|---|---|---|---|
| recall@5 | 0.6401 | 0.4987 | -0.1414 | [-0.1793, -0.1064] | 252 |
| recall@10 | 0.7494 | 0.6673 | -0.0821 | [-0.1103, -0.0558] | 252 |
| recall@20 | 0.8303 | 0.7941 | -0.0361 | [-0.0538, -0.0206] | 252 |
| mrr@10 | 0.8133 | 0.5860 | -0.2272 | [-0.2729, -0.1828] | 252 |
| ndcg@10 | 0.7214 | 0.5477 | -0.1737 | [-0.2045, -0.1438] | 252 |

### vidore3-hr-en / nonaggregative / baseline vs fused.gated
Evaluated 252 queries (vidore3-hr-en nonaggregative fused.gated).
| Metric | Off (Baseline) | On (Treatment) | Delta | 95% Bootstrap CI | n |
|---|---|---|---|---|---|
| recall@5 | 0.6401 | 0.5614 | -0.0787 | [-0.1077, -0.0529] | 252 |
| recall@10 | 0.7494 | 0.6917 | -0.0577 | [-0.0803, -0.0379] | 252 |
| recall@20 | 0.8303 | 0.8054 | -0.0249 | [-0.0393, -0.0126] | 252 |
| mrr@10 | 0.8133 | 0.6977 | -0.1156 | [-0.1521, -0.0819] | 252 |
| ndcg@10 | 0.7214 | 0.6282 | -0.0932 | [-0.1179, -0.0695] | 252 |

### vidore3-hr-en / nonaggregative / baseline vs fused.oracle
Evaluated 252 queries (vidore3-hr-en nonaggregative fused.oracle).
| Metric | Off (Baseline) | On (Treatment) | Delta | 95% Bootstrap CI | n |
|---|---|---|---|---|---|
| recall@5 | 0.6401 | 0.6401 | +0.0000 | [+0.0000, +0.0000] | 252 |
| recall@10 | 0.7494 | 0.7494 | +0.0000 | [+0.0000, +0.0000] | 252 |
| recall@20 | 0.8303 | 0.8303 | +0.0000 | [+0.0000, +0.0000] | 252 |
| mrr@10 | 0.8133 | 0.8133 | +0.0000 | [+0.0000, +0.0000] | 252 |
| ndcg@10 | 0.7214 | 0.7214 | +0.0000 | [+0.0000, +0.0000] | 252 |

### vidore3-cs-en / aggregative / baseline vs global.no-llm
Evaluated 53 queries (vidore3-cs-en aggregative global.no-llm).
| Metric | Off (Baseline) | On (Treatment) | Delta | 95% Bootstrap CI | n |
|---|---|---|---|---|---|
| recall@5 | 0.6041 | 0.1466 | -0.4576 | [-0.5511, -0.3644] | 53 |
| recall@10 | 0.7512 | 0.1904 | -0.5608 | [-0.6448, -0.4771] | 53 |
| recall@20 | 0.8507 | 0.2306 | -0.6200 | [-0.7055, -0.5318] | 53 |
| mrr@10 | 0.8984 | 0.3760 | -0.5225 | [-0.6438, -0.3993] | 53 |
| ndcg@10 | 0.7588 | 0.2161 | -0.5427 | [-0.6235, -0.4625] | 53 |

### vidore3-cs-en / aggregative / baseline vs fused.all
Evaluated 53 queries (vidore3-cs-en aggregative fused.all).
| Metric | Off (Baseline) | On (Treatment) | Delta | 95% Bootstrap CI | n |
|---|---|---|---|---|---|
| recall@5 | 0.6041 | 0.4574 | -0.1468 | [-0.2235, -0.0778] | 53 |
| recall@10 | 0.7512 | 0.6974 | -0.0538 | [-0.0982, -0.0125] | 53 |
| recall@20 | 0.8507 | 0.8137 | -0.0370 | [-0.0724, -0.0043] | 53 |
| mrr@10 | 0.8984 | 0.7009 | -0.1975 | [-0.2866, -0.1168] | 53 |
| ndcg@10 | 0.7588 | 0.6238 | -0.1351 | [-0.1897, -0.0843] | 53 |

### vidore3-cs-en / aggregative / baseline vs fused.gated
Evaluated 53 queries (vidore3-cs-en aggregative fused.gated).
| Metric | Off (Baseline) | On (Treatment) | Delta | 95% Bootstrap CI | n |
|---|---|---|---|---|---|
| recall@5 | 0.6041 | 0.5107 | -0.0935 | [-0.1476, -0.0427] | 53 |
| recall@10 | 0.7512 | 0.7006 | -0.0506 | [-0.0884, -0.0160] | 53 |
| recall@20 | 0.8507 | 0.8294 | -0.0212 | [-0.0490, +0.0046] | 53 |
| mrr@10 | 0.8984 | 0.7527 | -0.1457 | [-0.2254, -0.0748] | 53 |
| ndcg@10 | 0.7588 | 0.6594 | -0.0994 | [-0.1446, -0.0579] | 53 |

### vidore3-cs-en / aggregative / baseline vs fused.oracle
Evaluated 53 queries (vidore3-cs-en aggregative fused.oracle).
| Metric | Off (Baseline) | On (Treatment) | Delta | 95% Bootstrap CI | n |
|---|---|---|---|---|---|
| recall@5 | 0.6041 | 0.4574 | -0.1468 | [-0.2235, -0.0778] | 53 |
| recall@10 | 0.7512 | 0.6974 | -0.0538 | [-0.0982, -0.0125] | 53 |
| recall@20 | 0.8507 | 0.8137 | -0.0370 | [-0.0724, -0.0043] | 53 |
| mrr@10 | 0.8984 | 0.7009 | -0.1975 | [-0.2866, -0.1168] | 53 |
| ndcg@10 | 0.7588 | 0.6238 | -0.1351 | [-0.1897, -0.0843] | 53 |

### vidore3-cs-en / nonaggregative / baseline vs global.no-llm
Evaluated 162 queries (vidore3-cs-en nonaggregative global.no-llm).
| Metric | Off (Baseline) | On (Treatment) | Delta | 95% Bootstrap CI | n |
|---|---|---|---|---|---|
| recall@5 | 0.7570 | 0.1695 | -0.5875 | [-0.6419, -0.5337] | 162 |
| recall@10 | 0.8671 | 0.2106 | -0.6566 | [-0.7061, -0.6057] | 162 |
| recall@20 | 0.9220 | 0.2806 | -0.6414 | [-0.6973, -0.5850] | 162 |
| mrr@10 | 0.9273 | 0.3015 | -0.6258 | [-0.6906, -0.5599] | 162 |
| ndcg@10 | 0.8432 | 0.1966 | -0.6466 | [-0.6942, -0.5983] | 162 |

### vidore3-cs-en / nonaggregative / baseline vs fused.all
Evaluated 162 queries (vidore3-cs-en nonaggregative fused.all).
| Metric | Off (Baseline) | On (Treatment) | Delta | 95% Bootstrap CI | n |
|---|---|---|---|---|---|
| recall@5 | 0.7570 | 0.6117 | -0.1453 | [-0.1837, -0.1095] | 162 |
| recall@10 | 0.8671 | 0.7640 | -0.1032 | [-0.1317, -0.0761] | 162 |
| recall@20 | 0.9220 | 0.8760 | -0.0460 | [-0.0667, -0.0276] | 162 |
| mrr@10 | 0.9273 | 0.7047 | -0.2226 | [-0.2741, -0.1729] | 162 |
| ndcg@10 | 0.8432 | 0.6432 | -0.2000 | [-0.2354, -0.1657] | 162 |

### vidore3-cs-en / nonaggregative / baseline vs fused.gated
Evaluated 162 queries (vidore3-cs-en nonaggregative fused.gated).
| Metric | Off (Baseline) | On (Treatment) | Delta | 95% Bootstrap CI | n |
|---|---|---|---|---|---|
| recall@5 | 0.7570 | 0.6952 | -0.0618 | [-0.0889, -0.0378] | 162 |
| recall@10 | 0.8671 | 0.8219 | -0.0452 | [-0.0655, -0.0272] | 162 |
| recall@20 | 0.9220 | 0.8941 | -0.0279 | [-0.0449, -0.0129] | 162 |
| mrr@10 | 0.9273 | 0.8259 | -0.1014 | [-0.1409, -0.0645] | 162 |
| ndcg@10 | 0.8432 | 0.7537 | -0.0895 | [-0.1186, -0.0633] | 162 |

### vidore3-cs-en / nonaggregative / baseline vs fused.oracle
Evaluated 162 queries (vidore3-cs-en nonaggregative fused.oracle).
| Metric | Off (Baseline) | On (Treatment) | Delta | 95% Bootstrap CI | n |
|---|---|---|---|---|---|
| recall@5 | 0.7570 | 0.7570 | +0.0000 | [+0.0000, +0.0000] | 162 |
| recall@10 | 0.8671 | 0.8671 | +0.0000 | [+0.0000, +0.0000] | 162 |
| recall@20 | 0.9220 | 0.9220 | +0.0000 | [+0.0000, +0.0000] | 162 |
| mrr@10 | 0.9273 | 0.9273 | +0.0000 | [+0.0000, +0.0000] | 162 |
| ndcg@10 | 0.8432 | 0.8432 | +0.0000 | [+0.0000, +0.0000] | 162 |

````
