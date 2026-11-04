#!/usr/bin/env python3
"""The "global mode" retrieval arm: query -> communities -> pages.

No embeddings here, deliberately. The production shape would embed each
community summary once at ingest and run the caller's query through the same
embedding model as the chunk index; the prototype scores summaries with BM25 so
it can run on a laptop with no model call and no key. That substitution is
stated in the README next to every number it produces.

Ranking, per query:

1. BM25 over community summary text. The top `--top-communities` communities
   with a positive score are selected; if no summary shares a term with the
   query the query gets **no** global rows (the fuser then leaves it alone),
   which is visible in `--stats` rather than papered over.
2. Every page in a selected community is scored
   `sum(community_bm25 * (1 + |query entities ∩ page entities|))` and ranked
   descending, ties by page id. The community score carries the page into the
   run; the entity overlap orders pages inside and across communities, which is
   the part that lets a summary about "labour mobility" put the page that
   mentions the asked-about country first instead of the community's first page.

Output is a page TREC run (`qid Q0 <doc>#p<N> rank score tag`), so `score_deltas.py`
and `score_aggregative.py` can score it unchanged.

    python3 global_arm.py --communities communities.json \\
        --summaries summaries.json --queries .../queries.jsonl \\
        --out runs/global.page.trec --stats runs/global.stats.json
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Dict, List, Tuple

sys.path.insert(0, str(Path(__file__).resolve().parent))

import community_lib as lib  # noqa: E402


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--communities", type=Path, required=True)
    ap.add_argument("--summaries", type=Path, required=True)
    ap.add_argument("--queries", type=Path, required=True, help="queries.jsonl from the dataset")
    ap.add_argument("--out", type=Path, required=True, help="page TREC run")
    ap.add_argument("--stats", type=Path, help="per-query selection diagnostics (JSON)")
    ap.add_argument("--top-communities", type=int, default=3)
    ap.add_argument("--max-pages", type=int, default=200, help="pages emitted per query")
    ap.add_argument("--tag", default="global-communities")
    args = ap.parse_args()

    communities = json.loads(args.communities.read_text(encoding="utf-8"))
    summaries = json.loads(args.summaries.read_text(encoding="utf-8"))
    queries = lib.read_jsonl(args.queries)

    page_entities: Dict[str, List[str]] = communities.get("page_entities", {})
    page_community: Dict[str, int] = {}
    community_entities: Dict[int, set] = {}
    for community in communities["communities"]:
        community_entities[community["id"]] = {e["key"] for e in community["entities"]}
        for page in community["pages"]:
            page_community[page] = community["id"]

    docs = {str(s["community_id"]): s["text"] for s in summaries["summaries"]}
    if not docs:
        raise SystemExit("no summaries; run community_summarise.py first")

    runs: Dict[str, List[Tuple[str, float]]] = {}
    stats: Dict[str, dict] = {}
    empty = 0
    for row in queries:
        qid, text = str(row["_id"]), row["text"]
        scores = lib.bm25_scores(docs, text)
        ranked = sorted(scores.items(), key=lambda kv: (-kv[1], int(kv[0])))
        selected = [(cid, score) for cid, score in ranked[: args.top_communities] if score > 0]
        terms = set(lib.query_terms(text, limit=16)) | set(lib.bm25_tokens(text))
        page_scores: Dict[str, float] = {}
        for cid, score in selected:
            for page, community_id in page_community.items():
                if community_id != int(cid):
                    continue
                overlap = len(terms & set(page_entities.get(page, [])))
                page_scores[page] = page_scores.get(page, 0.0) + score * (1.0 + overlap)
        ordered = sorted(page_scores.items(), key=lambda kv: (-kv[1], lib.page_sort_key(kv[0])))
        runs[qid] = ordered[: args.max_pages]
        stats[qid] = {
            "communities": [{"id": int(cid), "score": round(score, 6)} for cid, score in selected],
            "n_pages": len(ordered),
            "query_entities": sorted(terms)[:20],
        }
        if not ordered:
            empty += 1

    lib.write_trec(args.out, runs, args.tag)
    print(
        f"wrote {args.out}: {len(runs)} queries, {sum(len(v) for v in runs.values())} rows, "
        f"{empty} queries with no matching community"
    )
    if args.stats:
        args.stats.parent.mkdir(parents=True, exist_ok=True)
        args.stats.write_text(json.dumps(stats, indent=1), encoding="utf-8")
        print(f"wrote {args.stats}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
