"""Score images-v1 runs by slice (ING-5): Recall@1/5/10 and MRR@10, with
95% bootstrap intervals, and paired deltas between two runs.

    python3 score_images.py --dataset DIR RUN_DIR [RUN_DIR ...]

Each RUN_DIR is a `kb-server eval --out` directory (its `run.trec` is read).
The first run is the reference for the paired deltas. Slices come from the
dataset's `query-meta.jsonl` (`photo`, `scene-text`, `chart`) plus `all`.
A query with no hit at all counts as rank infinity (recall 0, RR 0), so a
picture that was never indexed scores as the miss it is.
"""

import argparse
import json
import random
from collections import defaultdict
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument("--dataset", required=True, type=Path)
ap.add_argument("--resamples", type=int, default=2000)
ap.add_argument("runs", nargs="+", type=Path)
a = ap.parse_args()

gold = defaultdict(set)
for line in open(a.dataset / "qrels" / "test.tsv"):
    p = line.split()
    if p and p[0] != "query-id" and int(p[2]) > 0:
        gold[p[0]].add(p[1])
slice_of = {json.loads(l)["_id"]: json.loads(l)["slice"] for l in open(a.dataset / "query-meta.jsonl")}
queries = sorted(gold)


def ranks(run_dir):
    run = defaultdict(list)
    for line in open(run_dir / "run.trec"):
        q, _, d, rank, *_ = line.split()
        run[q].append((int(rank), d))
    out = {}
    for q in queries:
        hits = [d for _, d in sorted(run.get(q, []))]
        out[q] = next((i + 1 for i, d in enumerate(hits) if d in gold[q]), None)
    return out


def metrics(rank):
    return {
        "R@1": 1.0 if rank is not None and rank <= 1 else 0.0,
        "R@5": 1.0 if rank is not None and rank <= 5 else 0.0,
        "R@10": 1.0 if rank is not None and rank <= 10 else 0.0,
        "MRR@10": 1.0 / rank if rank is not None and rank <= 10 else 0.0,
    }


def ci(values, rng):
    n = len(values)
    if n == 0:
        return (0.0, 0.0, 0.0)
    mean = sum(values) / n
    boots = sorted(sum(values[rng.randrange(n)] for _ in range(n)) / n for _ in range(a.resamples))
    return (mean, boots[int(0.025 * a.resamples)], boots[int(0.975 * a.resamples) - 1])


slices = ["all"] + sorted({slice_of[q] for q in queries})
names = ["R@1", "R@5", "R@10", "MRR@10"]
per_run = [(r, ranks(r)) for r in a.runs]
for run_dir, rk in per_run:
    print(f"\n## {run_dir.name}")
    print("| slice | n | " + " | ".join(names) + " | never found |")
    print("|---|---|" + "---|" * len(names) + "---|")
    for sl in slices:
        qs = [q for q in queries if sl == "all" or slice_of[q] == sl]
        rng = random.Random(7)
        cells = []
        for m in names:
            mean, lo, hi = ci([metrics(rk[q])[m] for q in qs], rng)
            cells.append(f"{mean:.3f} [{lo:.3f}, {hi:.3f}]")
        missing = sum(1 for q in qs if rk[q] is None)
        print(f"| {sl} | {len(qs)} | " + " | ".join(cells) + f" | {missing} |")

if len(per_run) > 1:
    ref_dir, ref = per_run[0]
    for run_dir, rk in per_run[1:]:
        print(f"\n## paired delta: {run_dir.name} − {ref_dir.name}")
        print("| slice | n | ΔR@10 | ΔMRR@10 |")
        print("|---|---|---|---|")
        for sl in slices:
            qs = [q for q in queries if sl == "all" or slice_of[q] == sl]
            rng = random.Random(11)
            cells = []
            for m in ("R@10", "MRR@10"):
                d = [metrics(rk[q])[m] - metrics(ref[q])[m] for q in qs]
                mean, lo, hi = ci(d, rng)
                cells.append(f"{mean:+.3f} [{lo:+.3f}, {hi:+.3f}]")
            print(f"| {sl} | {len(qs)} | " + " | ".join(cells) + " |")
