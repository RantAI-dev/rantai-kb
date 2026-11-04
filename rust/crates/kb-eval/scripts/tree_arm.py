#!/usr/bin/env python3
"""A tree arm: score outline nodes for a query, emit their pages as a TREC run.

EVAL-2 asks whether a document's section hierarchy is worth putting on the
retrieval path. This is the cheap prototype of that arm, with no model call and
no index to build: it reads `outlines.json` (from `outline.py`) and a
`queries.jsonl`, and writes one page-level run in the same shape as
`kb-server eval`'s `run.page.trec`:

    qid Q0 <doc>#p<N> <rank> <score> tree

Scoring, deliberately simple and explainable:

* **Explicit numbers.** A query that says "section 4.2", "§ 4.2", "chapter 5"
  or just "4.2" is matched against the heading's own number. Chapter numbers
  prefer a "Chapter N" node; a chapter number is also a prefix of every "N.x"
  section, so those get a smaller partial score rather than nothing.
* **Title words.** BM25 over node titles (k1 1.2, b 0.75), with the document
  title folded into each node's text. Someone asking "the chapter on labour
  market developments" need not know the number.
* **Which document.** Query tokens are matched against each document's title
  and corpus key. When one document wins clearly the nodes are restricted to
  it; otherwise all documents compete, which is the honest behaviour for a
  question that never named a book.

It emits the best `--nodes` nodes' first `--gold-pages` pages. That cap mirrors
the structural set's gold (five pages per section): a 30-page chapter would
otherwise flood a ten-result evaluation with one heading.

Run it from the repo root:

    python3 rust/crates/kb-eval/scripts/tree_arm.py \
      --outlines rust/crates/kb-eval/sets/structural/outlines.json \
      --queries  rust/crates/kb-eval/sets/structural/queries.jsonl \
      --out      /tmp/eval2/tree.trec --explain
"""

from __future__ import annotations

import argparse
import json
import math
import re
from collections import Counter
from pathlib import Path
from typing import Any

DOTTED = re.compile(r"^(\d+(?:\.\d+)+)\.?\s+(.+)$")
CHAPTER = re.compile(r"^Chapter\s+(\d+)\b", re.I)
CHAPTER_QUERY = re.compile(
    r"\b(?:chapter|chap\.?|part)\s+(\d+(?:\.\d+)*)\b", re.I
)
SECTION_QUERY = re.compile(
    r"(?:\bsection|\bsec\.?|§)\s*(\d+(?:\.\d+)*)\b", re.I
)
BARE_NUMBER = re.compile(r"(?<![\w.])(\d+\.\d+(?:\.\d+)*)(?![\w.])")

STOPWORDS = set(
    """a about above after again against all also an and any are as at be because been
    before being below between both but by can could did do does doing down during each
    few for from further had has have having he her here hers herself him himself his how
    i if in into is it its itself just me more most my myself no nor not now of off on once
    only or other our ours ourselves out over own same she should so some such than that
    the their theirs them themselves then there these they this those through to too under
    until up very was we were what when where which while who whom why will with would you
    your yours yourself yourselves""".split()
)

TOKEN = re.compile(r"[a-z0-9]+")


def tokens(text: str) -> list[str]:
    return [word for word in TOKEN.findall(text.lower()) if word not in STOPWORDS]


def node_number(title: str) -> str | None:
    match = DOTTED.match(title)
    if match:
        return match.group(1).rstrip(".")
    match = CHAPTER.match(title)
    return match.group(1) if match else None


def is_chapter_node(title: str) -> bool:
    return bool(CHAPTER.match(title))


def load_nodes(path: Path) -> list[dict[str, Any]]:
    documents = json.loads(path.read_text(encoding="utf-8"))["documents"]
    nodes: list[dict[str, Any]] = []
    for key in sorted(documents):
        document = documents[key]
        doc_tokens = tokens(document["title"] + " " + key.replace("_", " ").replace("-", " "))
        for node in document["nodes"]:
            nodes.append(
                {
                    "doc": key,
                    "id": node["id"],
                    "title": node["title"],
                    "level": node["level"],
                    "page": node["page"],
                    "end_page": node["end_page"],
                    "number": node_number(node["title"]),
                    "chapter": is_chapter_node(node["title"]),
                    "tokens": tokens(node["title"]) + doc_tokens,
                }
            )
    return nodes


def doc_scores(query_tokens: list[str], nodes: list[dict[str, Any]]) -> dict[str, float]:
    """A crude TF-IDF-like document vote: rare query words count more."""
    docs: dict[str, set[str]] = {}
    for node in nodes:
        docs.setdefault(node["doc"], set()).update(node["tokens"])
    frequency = Counter()
    for vocabulary in docs.values():
        frequency.update(vocabulary)
    total = max(len(docs), 1)
    query_counts = Counter(query_tokens)
    scores: dict[str, float] = {}
    for doc, vocabulary in docs.items():
        score = 0.0
        for token, count in query_counts.items():
            if token in vocabulary:
                score += count * math.log(1 + total / frequency[token])
        scores[doc] = score / math.sqrt(max(len(query_tokens), 1))
    return scores


def bm25_scores(query_tokens: list[str], candidates: list[dict[str, Any]],
                k1: float = 1.2, b: float = 0.75) -> dict[int, float]:
    if not candidates or not query_tokens:
        return {index: 0.0 for index in range(len(candidates))}
    lengths = [len(node["tokens"]) for node in candidates]
    average = sum(lengths) / len(lengths) or 1.0
    document_frequency = Counter()
    for node in candidates:
        document_frequency.update(set(node["tokens"]))
    total = len(candidates)
    scores: dict[int, float] = {}
    for index, node in enumerate(candidates):
        counts = Counter(node["tokens"])
        score = 0.0
        for token in set(query_tokens):
            frequency = counts.get(token, 0)
            if not frequency:
                continue
            idf = math.log(1 + (total - document_frequency[token] + 0.5)
                           / (document_frequency[token] + 0.5))
            denominator = frequency + k1 * (1 - b + b * lengths[index] / average)
            score += idf * frequency * (k1 + 1) / denominator
        scores[index] = score
    return scores


def query_numbers(text: str) -> list[tuple[str, str]]:
    """(number, kind) pairs; kind is "chapter" or "section"."""
    found: list[tuple[str, str]] = []
    for match in CHAPTER_QUERY.finditer(text):
        found.append((match.group(1).rstrip("."), "chapter"))
    for match in SECTION_QUERY.finditer(text):
        found.append((match.group(1).rstrip("."), "section"))
    if not found:
        for match in BARE_NUMBER.finditer(text):
            found.append((match.group(1).rstrip("."), "section"))
    return found


def number_bonus(number: str, kind: str, node: dict[str, Any]) -> float:
    if not node["number"]:
        return 0.0
    if node["number"] == number:
        return 12.0 if (kind == "chapter" and node["chapter"]) else 8.0
    # "chapter 5" is a prefix of sections 5.1, 5.2 ...; the chapter node still wins.
    if node["number"].startswith(number + "."):
        return 2.0 if kind == "chapter" else 1.0
    if number.startswith(node["number"] + "."):
        return 1.5
    return 0.0


def score_query(text: str, nodes: list[dict[str, Any]]) -> list[dict[str, Any]]:
    query_tokens = tokens(text)
    votes = doc_scores(query_tokens, nodes)
    ranked_docs = sorted(votes.items(), key=lambda pair: pair[1], reverse=True)
    best_doc, best_score = ranked_docs[0] if ranked_docs else (None, 0.0)
    second_score = ranked_docs[1][1] if len(ranked_docs) > 1 else 0.0
    if best_doc and best_score >= 1.0 and best_score > 1.5 * max(second_score, 0.0):
        candidates = [node for node in nodes if node["doc"] == best_doc]
    else:
        candidates = nodes

    lexical = bm25_scores(query_tokens, candidates)
    numbers = query_numbers(text)
    scored: list[dict[str, Any]] = []
    for index, node in enumerate(candidates):
        bonus = sum(number_bonus(number, kind, node) for number, kind in numbers)
        score = lexical.get(index, 0.0) + bonus
        if score > 0:
            scored.append({**node, "score": score})
    scored.sort(key=lambda node: (-node["score"], node["doc"], node["page"]))
    return scored


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--outlines", type=Path,
        default=Path("rust/crates/kb-eval/sets/structural/outlines.json"))
    parser.add_argument(
        "--queries", type=Path,
        default=Path("rust/crates/kb-eval/sets/structural/queries.jsonl"))
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--nodes", type=int, default=5, help="heading nodes per query")
    parser.add_argument("--gold-pages", type=int, default=5,
                        help="pages taken from each node, heading page first")
    parser.add_argument("--max-pages", type=int, default=50)
    parser.add_argument("--tag", default="tree")
    parser.add_argument("--explain", action="store_true",
                        help="print the chosen headings to stderr")
    args = parser.parse_args()

    nodes = load_nodes(args.outlines)
    queries = [json.loads(line) for line in args.queries.read_text(encoding="utf-8").splitlines()
               if line.strip()]
    args.out.parent.mkdir(parents=True, exist_ok=True)
    total = 0
    with args.out.open("w", encoding="utf-8") as handle:
        for query in queries:
            scored = score_query(query["text"], nodes)[: args.nodes]
            pages: list[tuple[str, float]] = []
            seen: set[str] = set()
            for node in scored:
                if len(pages) >= args.max_pages:
                    break
                last = min(node["end_page"], node["page"] + args.gold_pages - 1)
                for page in range(node["page"], last + 1):
                    page_id = f"{node['doc']}#p{page}"
                    if page_id in seen:
                        continue
                    seen.add(page_id)
                    pages.append((page_id, node["score"]))
            pages = pages[: args.max_pages]
            for rank, (page_id, score) in enumerate(pages, start=1):
                handle.write(f"{query['_id']} Q0 {page_id} {rank} {score:.6f} {args.tag}\n")
            total += len(pages)
            if args.explain:
                chosen = ", ".join(
                    f"{node['doc']}/{node['id']}:{node['number'] or '-'}"
                    f"@p{node['page']} ({node['score']:.2f})"
                    for node in scored) or "none"
                print(f"{query['_id']} {chosen}", file=__import__("sys").stderr)
    print(f"wrote {total} page rows for {len(queries)} queries to {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
