#!/usr/bin/env python3
"""Reciprocal-rank fusion of a page baseline with the global (community) run.

`score = sum(weight_arm / (k + rank_arm))`, k=60, rank 1-based — the same shape
as the service's own RRF. The global arm carries `--weight` (default 0.5): a
summary-based run is a coarser signal than a page reranker, and a full-weight
fuse would displace correct baseline pages on the first try.

`--gate aggregative` fuses **only** queries the gate labels broad. Everything
else is copied through from the baseline unchanged, because global mode must not
be allowed to hurt the extractive questions it was never meant for. The gate is
`community_lib.is_aggregative_query`, a lexical stand-in; `query_class.rs` has
no aggregative class yet (RET-5), and `aggregative_subsets.py` reports how well
the stand-in agrees with ViDoRe's human labels.

`--gate list --query-list <jsonl>` fuses exactly the listed query ids. Point it
at `queries.aggregative.jsonl` to measure the arm with an **oracle gate**: it
separates "is the arm good?" from "is the gate good?", which is the question a
DEC-5 decision actually needs. A zero delta on the non-aggregative subset under
`--gate list` is a property of the construction, not a result.

    python3 global_fuse.py --baseline rk-cloud/run.page.trec \\
        --global runs/global.page.trec --queries .../queries.jsonl \\
        --gate aggregative --weight 0.5 --out runs/fused.page.trec
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Dict, List, Tuple

sys.path.insert(0, str(Path(__file__).resolve().parent))

import community_lib as lib  # noqa: E402


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--baseline", type=Path, required=True)
    ap.add_argument("--global", dest="global_run", type=Path, required=True)
    ap.add_argument("--queries", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--k", type=int, default=60)
    ap.add_argument("--weight", type=float, default=0.5, help="RRF weight of the global arm")
    ap.add_argument("--gate", choices=["none", "aggregative", "list"], default="none")
    ap.add_argument("--query-list", type=Path, help="jsonl with _id, for --gate list")
    ap.add_argument("--max-pages", type=int, default=1000)
    ap.add_argument("--tag", default="global-fused")
    args = ap.parse_args()

    baseline = lib.read_trec(args.baseline)
    global_run = lib.read_trec(args.global_run)
    queries = {str(row["_id"]): row["text"] for row in lib.read_jsonl(args.queries)}
    listed = set()
    if args.gate == "list":
        if not args.query_list:
            raise SystemExit("--gate list needs --query-list")
        listed = {str(row["_id"]) for row in lib.read_jsonl(args.query_list)}

    fused: Dict[str, List[Tuple[str, float]]] = {}
    gated = 0
    for qid in sorted(set(baseline) | set(global_run), key=lambda q: (len(q), q)):
        if args.gate == "none":
            use_global = True
        elif args.gate == "list":
            use_global = qid in listed
        else:
            use_global = lib.is_aggregative_query(queries.get(qid, ""))
        scores: Dict[str, float] = {}
        for rank, docid, _ in baseline.get(qid, []):
            scores[docid] = scores.get(docid, 0.0) + 1.0 / (args.k + rank)
        if use_global and args.weight > 0.0:
            gated += int(qid in global_run)
            for rank, docid, _ in global_run.get(qid, []):
                scores[docid] = scores.get(docid, 0.0) + args.weight / (args.k + rank)
        ordered = sorted(scores.items(), key=lambda kv: (-kv[1], lib.page_sort_key(kv[0])))
        fused[qid] = ordered[: args.max_pages]

    lib.write_trec(args.out, fused, args.tag)
    print(
        f"wrote {args.out}: {len(fused)} queries, global arm used on {gated} "
        f"(gate={args.gate}, weight={args.weight}, k={args.k})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
