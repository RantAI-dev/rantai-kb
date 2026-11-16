#!/usr/bin/env python3
"""Build the aggregative and non-aggregative qrels subsets from ViDoRe v3 labels.

The set is defined once, before any run is scored, from the dataset's human
`query_types` labels — never from a model's guess:

    aggregative := query_types contains "enumerative" or "multi-hop"
                   AND the query has >= 2 positive gold pages

The `>= 2` condition is what makes "answer-support recall" mean something: on a
one-gold-page question recall@10 is hit@10, so a global summary cannot help by
returning more of the answer. The non-aggregative subset is every evaluated
query not in the aggregative set, and it is the guardrail — a global arm that
lifts the first set while dropping the second has not earned its ingest cost.

Output, per dataset, under `--out`:

    <dataset>/qrels.aggregative.tsv      qrels for the subset
    <dataset>/qrels.nonaggregative.tsv   the guardrail subset
    <dataset>/queries.aggregative.jsonl  query text + types, for inspection
    <dataset>/queries.nonaggregative.jsonl
    counts.json                          exact counts + gate agreement

    python3 aggregative_subsets.py \\
        --dataset /path/vidore3-hr-en --dataset /path/vidore3-cs-en \\
        --out rust/crates/kb-eval/sets/aggregative
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import Counter, defaultdict
from pathlib import Path
from typing import Dict, List, Set

sys.path.insert(0, str(Path(__file__).resolve().parent))

import community_lib as lib  # noqa: E402

AGGREGATIVE_TYPES = {"enumerative", "multi-hop"}
MIN_GOLD = 2


def read_qrels(path: Path) -> Dict[str, Dict[str, int]]:
    gold: Dict[str, Dict[str, int]] = defaultdict(dict)
    with path.open(encoding="utf-8") as fh:
        for line in fh:
            parts = line.split()
            if len(parts) < 3 or parts[0] == "query-id":
                continue
            gold[parts[0]][parts[1]] = int(float(parts[2]))
    return gold


def write_qrels(path: Path, gold: Dict[str, Dict[str, int]], qids: List[str]) -> int:
    path.parent.mkdir(parents=True, exist_ok=True)
    rows = 0
    with path.open("w", encoding="utf-8") as fh:
        fh.write("query-id\tcorpus-id\tscore\n")
        for qid in qids:
            for docid, score in sorted(gold[qid].items()):
                if score > 0:
                    fh.write(f"{qid}\t{docid}\t{score}\n")
                    rows += 1
    return rows


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--dataset", type=Path, action="append", required=True)
    ap.add_argument("--out", type=Path, required=True)
    args = ap.parse_args()

    counts: Dict[str, dict] = {}
    total_gate = Counter()
    for dataset in args.dataset:
        name = dataset.name
        meta = {row["_id"]: row for row in lib.read_jsonl(dataset / "query-meta.jsonl")}
        queries = {row["_id"]: row["text"] for row in lib.read_jsonl(dataset / "queries.jsonl")}
        gold = read_qrels(dataset / "qrels" / "test.tsv")

        type_counts = Counter(t for row in meta.values() for t in row["query_types"])
        candidates = [
            qid for qid, row in meta.items()
            if AGGREGATIVE_TYPES & set(row["query_types"])
        ]
        aggregative = [qid for qid in candidates if len(gold.get(qid, {})) >= MIN_GOLD]
        aggregative.sort(key=lambda q: (len(q), q))
        non = sorted((qid for qid in gold if qid not in set(aggregative)), key=lambda q: (len(q), q))

        outdir = args.out / name
        agg_rows = write_qrels(outdir / "qrels.aggregative.tsv", gold, aggregative)
        non_rows = write_qrels(outdir / "qrels.nonaggregative.tsv", gold, non)
        for suffix, qids in (("aggregative", aggregative), ("nonaggregative", non)):
            with (outdir / f"queries.{suffix}.jsonl").open("w", encoding="utf-8") as fh:
                for qid in qids:
                    fh.write(json.dumps(
                        {"_id": qid, "text": queries[qid], "query_types": meta[qid]["query_types"]},
                        ensure_ascii=False) + "\n")

        # Gate agreement against the human labels. Reported, never used to tune
        # the set: the set comes from the labels alone.
        tp = fp = fn = tn = 0
        for qid, text in queries.items():
            truth = qid in set(aggregative)
            guess = lib.is_aggregative_query(text)
            tp += int(truth and guess)
            fp += int(not truth and guess)
            fn += int(truth and not guess)
            tn += int(not truth and not guess)
        total_gate.update({"tp": tp, "fp": fp, "fn": fn, "tn": tn})

        counts[name] = {
            "queries": len(meta),
            "queries_with_label": len(gold),
            "enumerative": type_counts.get("enumerative", 0),
            "multi_hop": type_counts.get("multi-hop", 0),
            "aggregative_candidates_before_gold_filter": len(candidates),
            "aggregative": len(aggregative),
            "nonaggregative": len(non),
            "aggregative_gold_pages": sum(len(gold[q]) for q in aggregative),
            "aggregative_rows_written": agg_rows,
            "nonaggregative_rows_written": non_rows,
            "gate": {
                "tp": tp, "fp": fp, "fn": fn, "tn": tn,
                "precision": tp / (tp + fp) if tp + fp else 0.0,
                "recall": tp / (tp + fn) if tp + fn else 0.0,
            },
        }
        print(
            f"{name}: {len(aggregative)} aggregative / {len(non)} non-aggregative "
            f"(candidates before >= {MIN_GOLD} gold pages: {len(candidates)}; "
            f"enumerative {type_counts.get('enumerative', 0)}, "
            f"multi-hop {type_counts.get('multi-hop', 0)})"
        )

    tp, fp = total_gate["tp"], total_gate["fp"]
    fn, tn = total_gate["fn"], total_gate["tn"]
    counts["gate_agreement_all"] = {
        "tp": tp, "fp": fp, "fn": fn, "tn": tn,
        "precision": tp / (tp + fp) if tp + fp else 0.0,
        "recall": tp / (tp + fn) if tp + fn else 0.0,
        "accuracy": (tp + tn) / (tp + fp + fn + tn) if (tp + fp + fn + tn) else 0.0,
    }
    args.out.mkdir(parents=True, exist_ok=True)
    (args.out / "counts.json").write_text(json.dumps(counts, indent=2), encoding="utf-8")
    print(
        f"gate agreement over both sets: precision {counts['gate_agreement_all']['precision']:.3f}, "
        f"recall {counts['gate_agreement_all']['recall']:.3f}, "
        f"accuracy {counts['gate_agreement_all']['accuracy']:.3f} "
        f"(tp {tp}, fp {fp}, fn {fn}, tn {tn})"
    )
    print(f"wrote {args.out}/counts.json and per-dataset subsets")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
