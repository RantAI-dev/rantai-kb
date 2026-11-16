#!/usr/bin/env python3
"""Fuse a baseline page run with the tree arm's page run by reciprocal-rank fusion.

EVAL-2's comparison is tree-arm-on vs tree-arm-off, holding the baseline fixed.
The treatment run is therefore the baseline plus the tree arm's ranking, fused
with RRF:

    score(page) = w_base / (k + rank_base) + w_tree / (k + rank_tree)

`k` (60 by default) is the constant from Cormack et al.'s RRF paper; the tree
weight is configurable because a prototype made from PDF bookmarks should not
start with the same authority as the production retriever.

The gate. A tree arm can only help a question that refers to document
structure; on "two factor authentication vs single factor" it has nothing to
navigate. `--gate` fuses only queries whose wording looks structural and copies
the baseline rows unchanged for the rest, so a gated treatment can never hurt
an ordinary query by construction. The cue list is:

  * an explicit structural noun: chapter, section, subsection, part, appendix,
    annex, "table of contents", §;
  * a dotted number (4.2, 2.1.1) or "<noun> N" (chapter 5, section 3.4);
  * the report-scaffolding vocabulary that kb-core's heuristic entity
    extractor already treats as structural rather than content
    (`rust/crates/kb-core/src/intelligence/heuristic.rs`, the "Report
    scaffolding and titles" block: abstract, appendix, bibliography, chapter,
    conclusion, contents, overview, introduction, section, summary, ...).

`rust/crates/kb-core/src/query_class.rs` is the nearest Rust precedent for
putting a query classifier in kb-core, and this script copies its token
normalisation (lower-case, strip surrounding punctuation). It has **no
structural branch** — it only separates exact/identifier queries from natural
ones — so the cue list above is taken from `heuristic.rs` rather than invented
here, and this is stated again in the README.

Usage:

    python3 tree_fuse.py --baseline run.page.trec --tree tree.trec \
        --out fused.trec --gate --queries queries.jsonl --tree-weight 0.5
"""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path

CUE = re.compile(
    r"\b(chapter|chap|section|sec|subsection|sub-section|part|paragraph|"
    r"appendix|annex|contents|overview|introduction|conclusion|summary|"
    r"preface|glossary|bibliography|index|abstract)\b",
    re.I,
)
NUMBERED = re.compile(
    r"\b(?:chapter|chap\.?|section|sec\.?|part|§)\s*\d+(?:\.\d+)*\b"
    r"|\b\d+\.\d+(?:\.\d+)*\b",
    re.I,
)


def is_structural(text: str) -> bool:
    return bool(CUE.search(text) or NUMBERED.search(text))


def parse_run(path: Path) -> dict[str, list[tuple[str, float]]]:
    runs: dict[str, list[tuple[str, float]]] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        fields = line.split()
        if len(fields) < 6:
            continue
        qid, _, page_id, rank, score = fields[0], fields[1], fields[2], fields[3], fields[4]
        try:
            runs.setdefault(qid, []).append((page_id, float(score)))
        except ValueError:
            continue
    for qid, rows in runs.items():
        # Keep the run's own score order; the rank column is authoritative in
        # a TREC file, and a run writer may legitimately repeat scores.
        rows.sort(key=lambda row: -row[1])
    return runs


def natural_key(qid: str) -> tuple:
    parts = re.split(r"(\d+)", qid)
    return tuple((0, int(part)) if part.isdigit() else (1, part) for part in parts)


def rrf(base: list[tuple[str, float]], tree: list[tuple[str, float]],
        k: int, base_weight: float, tree_weight: float) -> list[tuple[str, float]]:
    scores: dict[str, float] = {}
    for rank, (page_id, _) in enumerate(base, start=1):
        scores[page_id] = scores.get(page_id, 0.0) + base_weight / (k + rank)
    for rank, (page_id, _) in enumerate(tree, start=1):
        scores[page_id] = scores.get(page_id, 0.0) + tree_weight / (k + rank)
    return sorted(scores.items(), key=lambda row: (-row[1], row[0]))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--tree", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--queries", type=Path,
                        help="queries.jsonl (needed by --gate)")
    parser.add_argument("--k", type=int, default=60)
    parser.add_argument("--tree-weight", type=float, default=1.0)
    parser.add_argument("--baseline-weight", type=float, default=1.0)
    parser.add_argument("--gate", action="store_true",
                        help="fuse only queries that look structural")
    parser.add_argument("--classes", type=Path,
                        help="output of `kb-server eval classify --queries F --per-query`; "
                             "with --gate, fuse only queries the production classifier "
                             "labels `structural` instead of using this script's cue list")
    parser.add_argument("--tag", default="tree-fused")
    args = parser.parse_args()

    base_run = parse_run(args.baseline)
    tree_run = parse_run(args.tree)
    text_of: dict[str, str] = {}
    if args.queries:
        for line in args.queries.read_text(encoding="utf-8").splitlines():
            if line.strip():
                query = json.loads(line)
                text_of[query["_id"]] = query.get("text", "")
    class_of: dict[str, str] = {}
    if args.classes:
        for line in args.classes.read_text(encoding="utf-8").splitlines():
            qid, _, label = line.partition("\t")
            if label:
                class_of[qid] = label.strip()
    if args.gate and not text_of and not class_of:
        raise SystemExit("--gate needs --queries so a query can be classified")

    fused_count = gated_count = 0
    args.out.parent.mkdir(parents=True, exist_ok=True)
    with args.out.open("w", encoding="utf-8") as handle:
        for qid in sorted(set(base_run) | set(tree_run), key=natural_key):
            if not args.gate:
                fuse = True
            elif class_of:
                fuse = class_of.get(qid) == "structural"
            else:
                fuse = is_structural(text_of.get(qid, ""))
            if fuse:
                rows = rrf(base_run.get(qid, []), tree_run.get(qid, []),
                           args.k, args.baseline_weight, args.tree_weight)
                fused_count += 1
            else:
                rows = base_run.get(qid, [])
                gated_count += 1
            for rank, (page_id, score) in enumerate(rows, start=1):
                handle.write(f"{qid} Q0 {page_id} {rank} {score:.6f} {args.tag}\n")

    print(f"wrote {args.out}: fused {fused_count} queries, gated off {gated_count}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
