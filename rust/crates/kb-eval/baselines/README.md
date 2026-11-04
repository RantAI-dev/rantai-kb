# Eval Baselines

- `tiny-stub.metrics.json`: Baseline metrics for the `fixtures/tiny` dataset using the stub embedder (`stub_embeddings`). With stub embeddings these numbers are deterministic plumbing numbers.
- `tiny-stub-qdrant.metrics.json`: the baseline `rust/scripts/ci-eval-gate.sh` compares against. It is the `fixtures/tiny` run on the Qdrant-only stack with the stub embedder (dim 8). Only plumbing is measured: the stub maps the first bytes of a chunk to a vector, so it exercises ingest, fusion, scope and ranking with no model, network or spend.

## Determinism, and the 2026-10-28 hit@1 flake

The stub's 8 dimensions make *exact* dense-score ties common, so the order in
which equal scores come out is part of what this gate measures. It must be a
function of the corpus, never of an ingest.

It was not. Document ids are UUIDs minted per ingest (`prisma/schema.prisma`),
and `sort_candidates` / the Qdrant adapter's `ranked_arm` broke equal fused
scores on `document_id` and `unit_id` (`{document_id}_{chunk_index}`). Those keys
change on every re-ingest, so two chunks tied at rank 1 — for `q4`, the coffee
chunk found by BM25 and the mission chunk found by the dense arm, both at
`1/(60+1)` — could come out in either order. That is the CI failure of
2026-10-28 (run 40703570765): document Hit@1 0.500 → 0.417 while page nDCG@10
rose, which looked like a retrieval regression but was one coin flip.

The fix keeps the baseline: `kb-ports::sort_candidates` and
`kb-store-qdrant::ranked_arm` now break ties on the chunk's text, then page, then
chunk index — properties of the corpus that do not move when the document is
re-ingested. Five fresh stacks (new containers, new ingest, new document UUIDs)
give the same document nDCG@10 0.776955, Hit@1 0.5 and page nDCG@10 1.0; before
the fix the same loop produced both 0.757/0.417 and 0.788/0.5. The baseline is
unchanged because Hit@1 and recall match it exactly and every other metric is
higher (`+0.022` document nDCG, `+0.500` page nDCG/recall).

Residual caveat: Qdrant still chooses which points fill the last slot of an
arm's `per_arm_k` when scores tie at that boundary, so the relative order of
chunks that do not affect a metric can still vary between ingests. The measured
metrics above are stable across runs.
