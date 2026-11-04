"""Rerank depth and rerank/fusion blending, from cached reranker scores.

A cross-encoder scores each (query, passage) pair on its own, so the scores
`rerank_bakeoff.py` cached for the top N candidates also give every depth
below N, with no new calls. This script replays:

- depth d (5, 10, 15, 20): rerank the first d fused candidates, the rest keep
  their fused order after them;
- blending: rank by w / (k + rerank rank) + (1 - w) / (k + fused rank) over
  the reranked head, which keeps some of the fusion's opinion.

Blend weights are chosen on the tune half of `split.json` (written by
fusion_tune.py) and reported on the test half, with paired bootstrap CIs
against no rerank.

    python rerank_blend.py --candidates candidates.jsonl --dataset DIR \
        --cache rerank-cache.jsonl --split split.json --model URL --depth 20
"""

import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
from kf_vplane_ablate import metrics, paired_ci  # noqa: E402
from rerank_bakeoff import RerankCache  # noqa: E402


def docs_of(order, cands):
    seen, out = set(), []
    for i in order:
        d = cands[i]["doc"]
        if d not in seen:
            seen.add(d)
            out.append(d)
    return out[:10]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--candidates", type=Path, required=True)
    ap.add_argument("--dataset", type=Path, required=True)
    ap.add_argument("--cache", type=Path, required=True)
    ap.add_argument("--split", type=Path, required=True)
    ap.add_argument("--model", action="append", required=True, help="the target the scores were cached under")
    ap.add_argument("--depth", type=int, default=20, help="the depth the scores were cached at")
    ap.add_argument("--out", type=Path, default=None)
    args = ap.parse_args()

    qrels: dict[str, set[str]] = defaultdict(set)
    for line in (args.dataset / "qrels" / "test.tsv").open(encoding="utf-8"):
        p = line.split()
        if len(p) >= 3 and p[0] != "query-id" and float(p[-1]) > 0:
            qrels[p[0]].add(p[1])
    cands = {}
    queries = {}
    for line in args.candidates.open(encoding="utf-8"):
        r = json.loads(line)
        cands[r["qid"]], queries[r["qid"]] = r["candidates"], r["query"]
    split = json.loads(args.split.read_text())
    tune, test = sorted(split["tune"]), sorted(split["test"])
    cache = RerankCache(args.cache)

    lines = ["# Rerank depth and blending (from cached scores)", ""]
    for model in args.model:
        # Per query: the reranker's score for each of the first `depth` candidates.
        score = {}
        for qid, cs in cands.items():
            docs = [c["text"] for c in cs[: args.depth]]
            hit = cache.get(RerankCache.key(model, qid, queries[qid], docs))
            if hit is None:
                print(f"{model}: no cached scores for {qid}", file=sys.stderr)
                return 1
            s = np.zeros(len(docs))
            for i, v in zip(hit["order"], hit["scores"], strict=True):
                s[i] = v
            score[qid] = s

        def order_for(qid, depth, w, k):
            n = len(cands[qid])
            head = list(range(min(depth, n)))
            rr = {i: r for r, i in enumerate(sorted(head, key=lambda i: -score[qid][i]))}
            if w >= 1.0:
                ranked = sorted(head, key=lambda i: rr[i])
            else:
                ranked = sorted(head, key=lambda i: -(w / (k + rr[i]) + (1 - w) / (k + i)))
            return ranked + list(range(len(head), n))

        def evaluate(qids, depth, w, k):
            run = {q: docs_of(order_for(q, depth, w, k), cands[q]) for q in qids}
            return metrics(run, {q: qrels[q] for q in qids})

        base_test = evaluate(test, 0, 1.0, 1)
        grid = [(d, w, k) for d in (5, 10, 15, 20) if d <= args.depth
                for w in (1.0, 0.9, 0.8, 0.7, 0.6, 0.5) for k in ((1,) if w >= 1.0 else (1, 5, 20, 60))]
        tuned = sorted(grid, key=lambda g: -evaluate(tune, *g)["ndcg"].mean())
        lines += [f"## {model}", "", "| depth | blend w | k | tune nDCG | test nDCG | Δ vs no rerank [95% CI] | "
                  "test MRR | test Hit@1 |", "|---:|---:|---:|---:|---:|---|---:|---:|"]
        shown = []
        for d in (5, 10, 15, 20):
            if d <= args.depth:
                shown.append((d, 1.0, 1))
                best_d = next(g for g in tuned if g[0] == d)
                if best_d not in shown:
                    shown.append(best_d)
        for g in [(0, 1.0, 1)] + shown:
            t = evaluate(tune, *g)
            m = evaluate(test, *g)
            dl, lo, hi = paired_ci(m["ndcg"], base_test["ndcg"])
            lines.append(f"| {g[0]} | {g[1]} | {g[2]} | {t['ndcg'].mean():.3f} | {m['ndcg'].mean():.3f} | "
                         f"{dl:+.3f} [{lo:+.3f}, {hi:+.3f}] | {m['mrr'].mean():.3f} | {m['hit1'].mean():.3f} |")
        lines += ["", f"tune winner: depth {tuned[0][0]}, w {tuned[0][1]}, k {tuned[0][2]}", ""]
    report = "\n".join(lines) + "\n"
    if args.out:
        args.out.write_text(report)
    print(report)
    return 0


if __name__ == "__main__":
    sys.exit(main())
