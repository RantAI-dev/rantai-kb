#!/usr/bin/env python3
"""Where the relevant chunk sits in the *fused* (pre-rerank) list.

The reranker scores every candidate the engine sends it, but before issue #31
the engine kept only `KB_RERANK_FINAL_K` (5) of those scores and returned the
rest in the fused order, unscored. This script measures the headroom that
decision left on the table: for every query, the rank the best relevant chunk
held in the fused list.

    fused_rank_histogram.py --qrels QRELS --run RUN [--run LABEL=RUN ...]

It reads a TREC run file (`qid Q0 docid rank score tag`) and a qrels file
(TSV: `query-id corpus-id score`) and prints, for each run:

  * recall@k of the fused list for k = 5, 10, 20, 30 — the fraction of
    queries whose best relevant chunk is within the first k fused candidates;
  * the histogram of that rank, banded 1-5, 6-10, 11-20, 21-30, 31+.

The band 6..N is the pool the old `final_k=5` never scored: a relevant chunk
there was returned without a `rerankScore`, so a caller that decides grounding
on the score had to treat it as 0.

Stdlib only.
"""

import argparse
import sys
from pathlib import Path
from typing import Dict, List, Optional, Tuple

BANDS = [(1, 5), (6, 10), (11, 20), (21, 30), (31, 10**9)]
DEFAULT_KS = (5, 10, 20, 30)


def parse_qrels(path: Path) -> Dict[str, Dict[str, int]]:
    """Parse a qrels file (TSV or whitespace: qid, docid, score)."""
    gold: Dict[str, Dict[str, int]] = {}
    with open(path, "r", encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split()
            if len(parts) < 3 or parts[0] == "query-id":
                continue
            qid, docid, score_str = parts[0], parts[1], parts[2]
            try:
                score = int(float(score_str))
            except ValueError:
                continue
            gold.setdefault(qid, {})[docid] = score
    return gold


def parse_trec_run(path: Path) -> Dict[str, List[str]]:
    """Parse a TREC run file into {qid: [docid, ...]} in rank order."""
    raw: Dict[str, List[Tuple[int, str]]] = {}
    with open(path, "r", encoding="utf-8") as handle:
        for line in handle:
            parts = line.strip().split()
            if len(parts) < 6:
                continue
            qid, docid, rank_str = parts[0], parts[2], parts[3]
            try:
                rank = int(rank_str)
            except ValueError:
                continue
            raw.setdefault(qid, []).append((rank, docid))
    runs: Dict[str, List[str]] = {}
    for qid, items in raw.items():
        items.sort(key=lambda item: item[0])
        seen = set()
        ordered = []
        for _, docid in items:
            if docid in seen:
                continue
            seen.add(docid)
            ordered.append(docid)
        runs[qid] = ordered
    return runs


def best_relevant_rank(run: List[str], gold: Dict[str, int]) -> Optional[int]:
    """The 1-based rank of the first relevant document in the run, if any."""
    for index, docid in enumerate(run, start=1):
        if gold.get(docid, 0) > 0:
            return index
    return None


def band_of(rank: int) -> str:
    for low, high in BANDS:
        if low <= rank <= high:
            return f"{low}-{high}" if high < 10**9 else f"{low}+"
    return "none"


def report(label: str, path: Path, qrels: Dict[str, Dict[str, int]], ks) -> int:
    run = parse_trec_run(path)
    queries = sorted(qrels)
    if not queries:
        print(f"{label}: no qrels for {path}", file=sys.stderr)
        return 1
    missing = [qid for qid in queries if qid not in run]
    ranks: Dict[str, int] = {}
    for qid in queries:
        if qid not in run:
            continue
        gold = {docid: score for docid, score in qrels[qid].items() if score > 0}
        rank = best_relevant_rank(run[qid], gold)
        if rank is not None:
            ranks[qid] = rank

    depth = max((len(docs) for docs in run.values()), default=0)
    answered = len(run)
    print(f"== {label}")
    print(f"   run: {path}")
    print(f"   queries with qrels: {len(queries)}  answered: {answered}  "
          f"no answer: {len(missing)}  max depth: {depth}")
    print(f"   queries whose best relevant chunk was found: {len(ranks)} "
          f"({len(ranks) / len(queries):.3f} of the queries with qrels)")
    for k in ks:
        if k > depth:
            print(f"   recall@{k:<3} n/a  (the run is only {depth} deep)")
            continue
        hit = sum(1 for rank in ranks.values() if rank <= k)
        print(f"   recall@{k:<3} {hit / len(queries):.3f}  ({hit}/{len(queries)})")
    counts: Dict[str, int] = {}
    for rank in ranks.values():
        counts[band_of(rank)] = counts.get(band_of(rank), 0) + 1
    for low, high in BANDS:
        name = f"{low}-{high}" if high < 10**9 else f"{low}+"
        count = counts.get(name, 0)
        print(f"   rank {name:>6}: {count:>4} ({count / len(queries):.3f})")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--qrels", required=True, type=Path,
                        help="qrels file (TSV: query-id, corpus-id, score)")
    parser.add_argument("--run", required=True, action="append",
                        help="TREC run file; repeat with LABEL=PATH to label it")
    parser.add_argument("--ks", default=",".join(str(k) for k in DEFAULT_KS),
                        help="comma-separated cut-offs for recall@k")
    args = parser.parse_args()

    ks = [int(k) for k in args.ks.split(",") if k.strip()]
    qrels = parse_qrels(args.qrels)
    status = 0
    for spec in args.run:
        label, _, raw_path = spec.partition("=")
        if not raw_path:
            label, raw_path = Path(label).stem, label
        status |= report(label, Path(raw_path), qrels, ks)
    return status


if __name__ == "__main__":
    sys.exit(main())
