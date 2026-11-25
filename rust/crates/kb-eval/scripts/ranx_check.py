"""Cross-check a TREC run with ranx, an independent scorer.

`kb-eval score` and this script must agree: two implementations of nDCG@10,
MRR@10, Hit@1 and Recall@10 that read the same run file and the same qrels
and do not agree mean one of them is wrong, and the number is not evidence
until that is settled.

    uvx --offline --with ranx python ranx_check.py --qrels qrels.trec \
        --run run.trec [--run other.trec]
"""

from __future__ import annotations

import argparse
from pathlib import Path

from ranx import Qrels, Run, evaluate


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--qrels", type=Path, required=True, help="TREC qrels: qid 0 docid rel")
    ap.add_argument("--run", type=Path, action="append", required=True, help="a TREC run file")
    args = ap.parse_args()

    qrels = Qrels.from_file(str(args.qrels))
    for run_path in args.run:
        run = Run.from_file(str(run_path))
        result = evaluate(qrels, run, ["ndcg@10", "mrr@10", "recall@10", "precision@1"], threads=2)
        print(f"== {run_path}")
        for metric in ("ndcg@10", "mrr@10", "recall@10", "precision@1"):
            value = result[metric]
            print(f"   {metric:<10} {value:.4f}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
