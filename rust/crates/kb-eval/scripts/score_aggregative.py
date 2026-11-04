#!/usr/bin/env python3
"""Answer-support recall with paired bootstrap CIs, on any qrels subset.

`score_deltas.py` is the repo's trusted paired-delta instrument, and this script
does not re-implement its statistics: it imports `parse_qrels`, `parse_trec_run`
and `bootstrap_ci` from it and adds the metric this task needs — **recall@20**,
because a global mode's whole claim is that it surfaces more of a multi-page
answer than a top-10 page list does. recall@10 and nDCG@10 come from the same
loop so all three describe the same query set.

A query present in the qrels but absent from a run is scored as zero (the
instrument's rule: a failed search is not an empty result, and a silently
missing query is not a win). Fixed bootstrap seed (42) so a re-run prints the
same interval.

    python3 score_aggregative.py --qrels .../qrels.aggregative.tsv \\
        --baseline rk-cloud/run.page.trec --treatment runs/fused.page.trec
"""

from __future__ import annotations

import argparse
import math
import sys
from pathlib import Path
from typing import Dict, List

sys.path.insert(0, str(Path(__file__).resolve().parent))

from score_deltas import bootstrap_ci, parse_qrels, parse_trec_run  # noqa: E402


def dcg(ranked: List[str], gold: Dict[str, int], k: int) -> float:
    total = 0.0
    for i, docid in enumerate(ranked[:k]):
        rel = gold.get(docid, 0)
        if rel > 0:
            total += rel / math.log2(i + 2.0)
    return total


def ideal_dcg(gold: Dict[str, int], k: int) -> float:
    positives = sorted((rel for rel in gold.values() if rel > 0), reverse=True)
    return sum(rel / math.log2(i + 2.0) for i, rel in enumerate(positives[:k]))


def metrics(ranked: List[str], gold: Dict[str, int]) -> Dict[str, float]:
    total_rel = sum(1 for rel in gold.values() if rel > 0)
    if total_rel == 0:
        return {}
    idcg = ideal_dcg(gold, 10)
    mrr = 0.0
    for i, docid in enumerate(ranked[:10]):
        if gold.get(docid, 0) > 0:
            mrr = 1.0 / (i + 1)
            break
    return {
        "recall@5": sum(1 for d in ranked[:5] if gold.get(d, 0) > 0) / total_rel,
        "recall@10": sum(1 for d in ranked[:10] if gold.get(d, 0) > 0) / total_rel,
        "recall@20": sum(1 for d in ranked[:20] if gold.get(d, 0) > 0) / total_rel,
        "mrr@10": mrr,
        "ndcg@10": dcg(ranked, gold, 10) / idcg if idcg else 0.0,
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--qrels", type=Path, required=True)
    ap.add_argument("--baseline", type=Path, required=True)
    ap.add_argument("--treatment", type=Path, required=True)
    ap.add_argument("--label", default="")
    ap.add_argument("--resamples", type=int, default=10000)
    ap.add_argument("--seed", type=int, default=42)
    args = ap.parse_args()

    gold = parse_qrels(args.qrels)
    base_run = parse_trec_run(args.baseline)
    treat_run = parse_trec_run(args.treatment)
    qids = sorted(qid for qid, g in gold.items() if any(s > 0 for s in g.values()))

    names = ["recall@5", "recall@10", "recall@20", "mrr@10", "ndcg@10"]
    base_vals: Dict[str, List[float]] = {m: [] for m in names}
    treat_vals: Dict[str, List[float]] = {m: [] for m in names}
    deltas: Dict[str, List[float]] = {m: [] for m in names}
    for qid in qids:
        g = gold[qid]
        b = metrics(base_run.get(qid, []), g)
        t = metrics(treat_run.get(qid, []), g)
        for name in names:
            bv, tv = b.get(name, 0.0), t.get(name, 0.0)
            base_vals[name].append(bv)
            treat_vals[name].append(tv)
            deltas[name].append(tv - bv)

    n = len(qids)
    if n == 0:
        raise SystemExit(f"no evaluable queries in {args.qrels}")
    title = f" ({args.label})" if args.label else ""
    print(f"Evaluated {n} queries{title}.")
    print("| Metric | Off (Baseline) | On (Treatment) | Delta | 95% Bootstrap CI | n |")
    print("|---|---|---|---|---|---|")
    for name in names:
        b_mean = sum(base_vals[name]) / n
        t_mean = sum(treat_vals[name]) / n
        d_mean, low, high = bootstrap_ci(deltas[name], args.resamples, args.seed)
        sign = "+" if d_mean >= 0 else ""
        print(f"| {name} | {b_mean:.4f} | {t_mean:.4f} | {sign}{d_mean:.4f} | [{low:+.4f}, {high:+.4f}] | {n} |")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
