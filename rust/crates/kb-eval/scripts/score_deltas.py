#!/usr/bin/env python3
"""Compute retrieval metrics (Recall@5, Recall@10, MRR@10, nDCG@10) from TREC runs
and report paired per-query deltas with 95% bootstrap confidence intervals.
"""

import argparse
import math
import random
import sys
from pathlib import Path
from typing import Dict, List, Tuple


def parse_qrels(path: Path) -> Dict[str, Dict[str, int]]:
    """Parse qrels file (TSV or whitespace-separated: qid, docid, score)."""
    gold = {}
    with open(path, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split()
            if len(parts) >= 3:
                if parts[0] == "query-id":
                    continue
                qid, docid, score_str = parts[0], parts[1], parts[2]
                try:
                    score = int(float(score_str))
                except ValueError:
                    continue
                if qid not in gold:
                    gold[qid] = {}
                gold[qid][docid] = score
    return gold


def parse_trec_run(path: Path) -> Dict[str, List[str]]:
    """Parse TREC run file (qid Q0 docid rank score tag).
    Returns {qid: [docid, ...] in rank order}.
    """
    runs = {}
    with open(path, "r", encoding="utf-8") as f:
        for line in f:
            parts = line.strip().split()
            if len(parts) >= 6:
                qid, docid, rank_str = parts[0], parts[2], parts[3]
                try:
                    rank = int(rank_str)
                except ValueError:
                    continue
                if qid not in runs:
                    runs[qid] = []
                runs[qid].append((rank, docid))

    # Sort by rank and deduplicate
    clean_runs = {}
    for qid, items in runs.items():
        items.sort(key=lambda x: x[0])
        seen = set()
        deduped = []
        for _, docid in items:
            if docid not in seen:
                seen.add(docid)
                deduped.append(docid)
        clean_runs[qid] = deduped
    return clean_runs


def dcg(ranked: List[str], gold: Dict[str, int], k: int = 10) -> float:
    score = 0.0
    for i, docid in enumerate(ranked[:k]):
        rel = gold.get(docid, 0)
        if rel > 0:
            score += float(rel) / math.log2(i + 2.0)
    return score


def ideal_dcg(gold: Dict[str, int], k: int = 10) -> float:
    positives = sorted([rel for rel in gold.values() if rel > 0], reverse=True)
    score = 0.0
    for i, rel in enumerate(positives[:k]):
        score += float(rel) / math.log2(i + 2.0)
    return score


def eval_query_metrics(ranked: List[str], gold: Dict[str, int]) -> Dict[str, float]:
    rel_total = sum(1 for s in gold.values() if s > 0)
    if rel_total == 0:
        return {}

    idcg10 = ideal_dcg(gold, 10)
    ndcg10 = dcg(ranked, gold, 10) / idcg10 if idcg10 > 0 else 0.0

    top5 = ranked[:5]
    rel5 = sum(1 for d in top5 if gold.get(d, 0) > 0)
    recall5 = rel5 / float(rel_total)

    top10 = ranked[:10]
    rel10 = sum(1 for d in top10 if gold.get(d, 0) > 0)
    recall10 = rel10 / float(rel_total)

    mrr10 = 0.0
    for i, d in enumerate(top10):
        if gold.get(d, 0) > 0:
            mrr10 = 1.0 / (i + 1.0)
            break

    return {
        "recall@5": recall5,
        "recall@10": recall10,
        "mrr@10": mrr10,
        "ndcg@10": ndcg10,
    }


def bootstrap_ci(deltas: List[float], n_resamples: int = 10000, seed: int = 42) -> Tuple[float, float, float]:
    """Calculate mean and 95% bootstrap CI (2.5%, 97.5%)."""
    if not deltas:
        return 0.0, 0.0, 0.0
    n = len(deltas)
    mean = sum(deltas) / float(n)
    if n == 1:
        return mean, mean, mean

    rng = random.Random(seed)
    means = []
    for _ in range(n_resamples):
        sample = [deltas[rng.randrange(n)] for _ in range(n)]
        means.append(sum(sample) / float(n))
    means.sort()

    low_idx = int(math.floor(0.025 * (n_resamples - 1)))
    high_idx = int(math.ceil(0.975 * (n_resamples - 1)))
    return mean, means[low_idx], means[high_idx]


def main():
    parser = argparse.ArgumentParser(description="Compare two runs with paired deltas and bootstrap CI")
    parser.add_argument("--qrels", type=Path, required=True, help="Path to qrels file")
    parser.add_argument("--baseline", type=Path, required=True, help="Path to baseline run.trec (off)")
    parser.add_argument("--treatment", type=Path, required=True, help="Path to treatment run.trec (on)")
    parser.add_argument("--resamples", type=int, default=10000, help="Number of bootstrap resamples")
    parser.add_argument("--seed", type=int, default=42, help="PRNG seed")
    args = parser.parse_args()

    gold = parse_qrels(args.qrels)
    base_run = parse_trec_run(args.baseline)
    treat_run = parse_trec_run(args.treatment)

    eval_qids = [qid for qid, g in gold.items() if any(s > 0 for s in g.values())]
    eval_qids.sort()

    metrics_list = ["recall@5", "recall@10", "mrr@10", "ndcg@10"]
    base_vals = {m: [] for m in metrics_list}
    treat_vals = {m: [] for m in metrics_list}
    deltas = {m: [] for m in metrics_list}

    for qid in eval_qids:
        g = gold[qid]
        b_metrics = eval_query_metrics(base_run.get(qid, []), g)
        t_metrics = eval_query_metrics(treat_run.get(qid, []), g)

        for m in metrics_list:
            bv = b_metrics.get(m, 0.0)
            tv = t_metrics.get(m, 0.0)
            base_vals[m].append(bv)
            treat_vals[m].append(tv)
            deltas[m].append(tv - bv)

    n = len(eval_qids)
    print(f"Evaluated {n} queries.")
    print("| Metric | Off (Baseline) | On (Treatment) | Delta | 95% Bootstrap CI | n |")
    print("|---|---|---|---|---|---|")
    for m in metrics_list:
        b_mean = sum(base_vals[m]) / float(n) if n > 0 else 0.0
        t_mean = sum(treat_vals[m]) / float(n) if n > 0 else 0.0
        d_mean, ci_low, ci_high = bootstrap_ci(deltas[m], args.resamples, args.seed)
        sign = "+" if d_mean >= 0 else ""
        print(f"| {m} | {b_mean:.4f} | {t_mean:.4f} | {sign}{d_mean:.4f} | [{ci_low:+.4f}, {ci_high:+.4f}] | {n} |")


if __name__ == "__main__":
    main()
