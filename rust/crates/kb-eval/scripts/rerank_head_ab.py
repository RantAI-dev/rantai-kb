#!/usr/bin/env python3
"""What a wider reranked head buys (issue #31).

Before #31 the engine kept only the top `KB_RERANK_FINAL_K` (5) candidates the
reranker had scored: they were reordered and carried a `rerankScore`, and the
rest of the pool came back in the fused order without a score. #31 asks for the
whole scored set to be ordered by the rerank score, with `stats.rerankedCount`
marking the boundary.

This script measures the two behaviours offline, on the same pool and the same
reranker scores, so the only thing that changes between the arms is the rule
that puts the pool in order:

  * `head K + fused tail` - today's rule: the top K by rerank score, then the
    rest of the pool in the order it arrived;
  * `all by score` - the whole scored pool, ordered by the rerank score.

The inputs are the bake-off's own artefacts, so nothing here needs a GPU, a
reranker or a paid API: the pool comes from `make_candidates.py` and the
reranker scores from `rerank_bakeoff.py`'s on-disk cache.

The script verifies the pool against the cache before it scores anything: the
cache key is `sha256(model, qid, query, documents...)`, the same function
`rerank_bakeoff.py` uses, so a pool that does not hash to the stored key is a
different pool and the indices in `order` would point at the wrong documents.
A run that does not verify stops.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import random
import statistics
import sys
from pathlib import Path
from typing import Dict, List, Sequence, Set, Tuple


def cache_key(model: str, qid: str, query: str, documents: Sequence[str]) -> str:
    """The key `rerank_bakeoff.py` stores: the pool is part of the hash."""
    digest = hashlib.sha256()
    digest.update(model.encode())
    digest.update(b"\0")
    digest.update(qid.encode())
    digest.update(b"\0")
    digest.update(query.encode())
    digest.update(b"\0")
    for document in documents:
        digest.update(document.encode())
        digest.update(b"\0")
    return digest.hexdigest()


def load_pool(path: Path) -> Dict[str, List[Dict[str, str]]]:
    """`{"qid": ..., "candidates": [{"doc": ..., "text": ...}]}` -> {qid: [candidate]}."""
    pool: Dict[str, List[Dict[str, str]]] = {}
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            row = json.loads(line)
            pool[str(row["qid"])] = row["candidates"]
    return pool


def load_queries(path: Path) -> Dict[str, str]:
    """`{"_id": ..., "text": ...}` -> {qid: query text}."""
    queries: Dict[str, str] = {}
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            row = json.loads(line)
            queries[str(row["_id"])] = row["text"]
    return queries


def load_cache(path: Path, model: str) -> Dict[str, Dict[str, object]]:
    """The rerank cache -> {qid: {"order": [pool index...], "scores": [...], "k": ...}}.

    The last row for a query wins: the cache appends, so a re-run of the same
    question leaves more than one row and the newest answer is the one the
    bake-off used.
    """
    rows: Dict[str, Dict[str, object]] = {}
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            row = json.loads(line)
            if row.get("model") != model:
                continue
            rows[str(row["qid"])] = {
                "order": list(row["order"]),
                "scores": list(row["scores"]),
                "k": row.get("k", ""),
            }
    return rows


def load_qrels(path: Path) -> Dict[str, Set[str]]:
    """TSV `query-id corpus-id score` -> {qid: {relevant docid}}."""
    qrels: Dict[str, Set[str]] = {}
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            parts = line.split()
            if len(parts) < 3 or parts[0] == "query-id":
                continue
            if float(parts[2]) > 0:
                qrels.setdefault(parts[0], set()).add(parts[1])
    return qrels


def head_k_plus_fused_tail(pool: Sequence[str], order: Sequence[int], k: int) -> List[str]:
    """Today's rule: the top k by rerank score, then the rest in pool order."""
    head = list(order[:k])
    picked = set(head)
    return [pool[i] for i in head] + [pool[i] for i in range(len(pool)) if i not in picked]


def all_by_score(pool: Sequence[str], order: Sequence[int]) -> List[str]:
    """#31's rule: the whole scored pool, ordered by the rerank score."""
    return [pool[i] for i in order]


def fused_only(pool: Sequence[str]) -> List[str]:
    """The reference arm: the pool as it arrived, with no reranker at all."""
    return list(pool)


def dedupe(docs: Sequence[str]) -> List[str]:
    """A document's second chunk does not give it a second place."""
    seen: Set[str] = set()
    out: List[str] = []
    for doc in docs:
        if doc not in seen:
            seen.add(doc)
            out.append(doc)
    return out


def per_query_metrics(ranked: Sequence[str], relevant: Set[str], k: int) -> Tuple[float, float, float, float]:
    """nDCG@k, MRR@k, Hit@1, Recall@k - the arithmetic `rerank_bakeoff.py` uses."""
    docs = list(ranked[:k])
    dcg = sum(1 / math.log2(i + 2) for i, d in enumerate(docs) if d in relevant)
    idcg = sum(1 / math.log2(i + 2) for i in range(min(len(relevant), k)))
    ndcg = dcg / idcg if idcg else 0.0
    rr = next((1 / (i + 1) for i, d in enumerate(docs) if d in relevant), 0.0)
    hit1 = 1.0 if docs and docs[0] in relevant else 0.0
    recall = len(set(docs) & relevant) / len(relevant) if relevant else 0.0
    return ndcg, rr, hit1, recall


def bootstrap_ci(deltas: List[float], n_resamples: int = 10000, seed: int = 42) -> Tuple[float, float, float]:
    """Paired bootstrap over queries: mean(a - b) and its 95% CI."""
    if not deltas:
        return 0.0, 0.0, 0.0
    n = len(deltas)
    mean = sum(deltas) / float(n)
    rng = random.Random(seed)
    means = []
    for _ in range(n_resamples):
        sample = [deltas[rng.randrange(n)] for _ in range(n)]
        means.append(sum(sample) / float(n))
    means.sort()
    low = means[int(math.floor(0.025 * (n_resamples - 1)))]
    high = means[int(math.ceil(0.975 * (n_resamples - 1)))]
    return mean, low, high


def mean(values: Sequence[float]) -> float:
    return statistics.mean(values) if values else 0.0


def main() -> int:
    parser = argparse.ArgumentParser(description="What a wider reranked head buys")
    parser.add_argument("--candidates", required=True, type=Path,
                        help="the candidate pool (make_candidates.py)")
    parser.add_argument("--cache", required=True, type=Path,
                        help="the rerank cache (rerank_bakeoff.py)")
    parser.add_argument("--queries", required=True, type=Path,
                        help="the dataset queries.jsonl (the cache key hashes it)")
    parser.add_argument("--model", required=True,
                        help="the model whose scores the cache holds")
    parser.add_argument("--qrels", required=True, type=Path)
    parser.add_argument("--heads", default="5,10,20",
                        help="comma-separated head widths to compare")
    parser.add_argument("--k", type=int, default=10, help="the cut-off to score at")
    parser.add_argument("--resamples", type=int, default=10000)
    parser.add_argument("--seed", type=int, default=42)
    args = parser.parse_args()

    pool = load_pool(args.candidates)
    queries = load_queries(args.queries)
    cache = load_cache(args.cache, args.model)
    qrels = load_qrels(args.qrels)
    qids = sorted(q for q in cache if q in pool and q in qrels and q in queries)
    if not qids:
        print("no query has both a cached rerank and a qrel", file=sys.stderr)
        return 1

    # The pool must be the one the cache was made from, or `order` points at
    # the wrong documents and every number below would be fiction.
    verified = 0
    for qid in qids:
        depth = len(cache[qid]["order"])
        documents = [c["text"] for c in pool[qid][:depth]]
        if cache_key(args.model, qid, queries[qid], documents) == cache[qid]["k"]:
            verified += 1
    print(f"model: {args.model}")
    print(f"pool: {args.candidates}")
    print(f"cache: {args.cache}")
    print(f"queries: {len(qids)}   pool vs cache keys verified: {verified}/{len(qids)}")
    if verified != len(qids):
        print("STOP: the pool does not hash to the cached keys; this pool is not "
              "the one the reranker scored", file=sys.stderr)
        return 2

    heads = [int(h) for h in args.heads.split(",") if h.strip()]
    depths = sorted({len(cache[q]["order"]) for q in qids})
    print(f"pool depth: {min(len(pool[q]) for q in qids)}-{max(len(pool[q]) for q in qids)}   "
          f"scored depth: {depths}   k: {args.k}")
    print()

    for depth in depths:
        usable = [q for q in qids if len(cache[q]["order"]) == depth]
        if not usable:
            continue
        # The sent pool is the first `depth` candidates: the bake-off sent
        # `cands[qid][:depth]`, the rest kept their fused order behind them.
        sent = {q: pool[q][:depth] for q in usable}
        print(f"=== scored pool: {depth} candidates per query ({len(usable)} queries)")
        print(f"    {'arm':<28} nDCG@{args.k}  MRR@{args.k}  Hit@1  Recall@{args.k}")

        arms: Dict[str, List[Tuple[float, float, float, float]]] = {}
        for head in heads:
            rows = []
            for q in usable:
                docs = dedupe(head_k_plus_fused_tail([c["doc"] for c in sent[q]],
                                                     cache[q]["order"], head))
                rows.append(per_query_metrics(docs, qrels[q], args.k))
            arms[f"head {head} + fused tail"] = rows
        rows = []
        for q in usable:
            docs = dedupe(all_by_score([c["doc"] for c in sent[q]], cache[q]["order"]))
            rows.append(per_query_metrics(docs, qrels[q], args.k))
        arms[f"all {depth} by score"] = rows
        rows = []
        for q in usable:
            docs = dedupe(fused_only([c["doc"] for c in sent[q]]))
            rows.append(per_query_metrics(docs, qrels[q], args.k))
        arms[f"fused only (no rerank)"] = rows

        for label, rows in arms.items():
            ndcg = [r[0] for r in rows]
            mrr = [r[1] for r in rows]
            hit1 = [r[2] for r in rows]
            recall = [r[3] for r in rows]
            print(f"    {label:<28} {mean(ndcg):9.3f}  {mean(mrr):8.3f}  "
                  f"{mean(hit1):6.3f}  {mean(recall):9.3f}")

        baseline_label = f"head {heads[0]} + fused tail"
        baseline = arms[baseline_label]
        for label, rows in arms.items():
            if label == baseline_label:
                continue
            print(f"    delta vs {baseline_label}: {label}")
            for index, name in enumerate(("nDCG", "MRR", "Hit@1", "Recall")):
                deltas = [b[index] - a[index] for a, b in zip(baseline, rows, strict=False)]
                delta, low, high = bootstrap_ci(deltas, args.resamples, args.seed)
                print(f"      {name:<7} {delta:+.4f} [{low:+.4f}, {high:+.4f}]")
        print()

    return 0


if __name__ == "__main__":
    sys.exit(main())
