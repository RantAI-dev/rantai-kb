# /// script
# dependencies = []
# ///
"""Build a smaller single-fact control view of a MIRACL dataset.

Why this exists: the graph arm's claim in `docs/architecture-target.md` §11 has two
halves — it wins on multi-hop and it *loses* on fact lookup — so the no-regression
check needs a fact-lookup set. `miracl-id-50` is the right shape but its corpus is
1,485 paragraphs, and on this box (6 cores, 11 GB, no GPU, CPU embeddings) that is
over an hour of ingest per arm and therefore per configuration. The claim being
tested is "turning the arm on does not hurt single-fact lookup", not "what does
MIRACL score", so the control can be smaller as long as it says so.

What it keeps, and why each matters:

* **every judged passage.** All 154 of them for `miracl-id-50`. Dropping one would
  make a query unanswerable rather than harder.
* **a deterministic distractor sample.** `--distractors` paragraphs sampled with a
  fixed seed from the unjudged ones, so two runs compare like with like and the
  number is reproducible from the file alone.

The result is a normal BEIR directory, so `kb-server eval` runs it unchanged.

Outputs:
  <out_dir>/
    corpus/<docid>.md   (symlinks into the source dataset's corpus)
    queries.jsonl
    qrels/test.tsv
    PROVENANCE.txt      the source, the seed, the sizes, and what was dropped
"""

from __future__ import annotations

import argparse
import json
import random
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True, help="the MIRACL dataset directory")
    parser.add_argument("--out-dir", type=Path, required=True)
    parser.add_argument("--distractors", type=int, default=400)
    parser.add_argument("--seed", type=int, default=42)
    args = parser.parse_args()

    source = args.source
    judged = {
        line.split("\t")[1]
        for line in (source / "qrels" / "test.tsv").read_text().splitlines()[1:]
        if line.split("\t")[2].strip() not in {"", "0"}
    }
    available = {p.stem for p in (source / "corpus").iterdir() if p.is_file()}
    unjudged = sorted(available - judged)

    rng = random.Random(args.seed)
    keep = sorted(judged) + sorted(rng.sample(unjudged, min(args.distractors, len(unjudged))))

    out = args.out_dir
    (out / "corpus").mkdir(parents=True, exist_ok=True)
    (out / "qrels").mkdir(parents=True, exist_ok=True)
    for docid in keep:
        link = out / "corpus" / f"{docid}.md"
        if not link.exists():
            link.symlink_to(source.resolve() / "corpus" / f"{docid}.md")

    for name in ("queries.jsonl", "answers.jsonl"):
        if (source / name).exists():
            (out / name).write_text((source / name).read_text(), encoding="utf-8")

    rows = (source / "qrels" / "test.tsv").read_text().splitlines()
    kept_qrels = [rows[0]] + [r for r in rows[1:] if r.split("\t")[1] in judged]
    (out / "qrels" / "test.tsv").write_text("\n".join(kept_qrels) + "\n", encoding="utf-8")

    n_queries = sum(1 for _ in (out / "queries.jsonl").read_text().splitlines())
    (out / "PROVENANCE.txt").write_text(
        "\n".join(
            [
                f"source:      {source.name}",
                f"source path: {source}",
                "licence:     Apache-2.0 (MIRACL); see the source dataset's importer",
                "",
                f"queries:     {n_queries}",
                f"judged docs: {len(judged)} (all kept)",
                f"distractors: {len(keep) - len(judged)} sampled from {len(unjudged)} unjudged",
                f"seed:        {args.seed}",
                f"corpus:      {len(keep)} of {len(available)} source paragraphs",
                "",
                "This is a REDUCED control, not MIRACL. Its numbers are not comparable",
                "to a published MIRACL score. It answers one question: does turning the",
                "graph retrieval arm on change single-fact lookup on a set where every",
                "judged passage is still present and only the unjudged distractors were",
                "thinned.",
            ]
        )
        + "\n",
        encoding="utf-8",
    )

    print(f"control: {out}")
    print(f"  queries:     {n_queries}")
    print(f"  judged docs: {len(judged)}")
    print(f"  corpus:      {len(keep)} of {len(available)}")
    print(f"  distractors: {len(keep) - len(judged)} (seed {args.seed})")
    _ = json  # the queries file is copied verbatim; its format is not this script's business


if __name__ == "__main__":
    main()