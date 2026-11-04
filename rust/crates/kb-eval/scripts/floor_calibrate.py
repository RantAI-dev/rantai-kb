#!/usr/bin/env python3
"""Sweep relevance-floor thresholds over saved /v1/search responses (RET-2).

Inputs are directories of per-query JSON files, one file per query, each the raw
body of a `POST /v1/search` response. The file stem is the query id:

    responses/
      q001.json    {"chunks": [...], "stats": {...}, "note": "...", ...}
      q002.json
      ...

The same server build must have produced both directories. For the in-domain
set, qrels give the relevance labels; for the off-topic set, every query is
expected to be unanswerable, so the only label is "this should come back empty".

Which score is swept
--------------------
The floor compares a score that is comparable across queries, and so must this
script:

* the reranker score (`chunks[].retrieval.rerankScore`) when the response says
  the floor kind was `rerank`, or when rerank scores are present;
* otherwise the dense cosine (`chunks[].similarity`).

It never uses the fused RRF score. The response carries the kind it used in
`stats.floorScoreKind` when a floor was applied; with no floor configured the
script infers `rerank` from the presence of `rerankScore` and falls back to
`dense`. A hit with no score of the chosen kind fails every threshold (it has no
evidence, which is exactly how the server treats an unscored hit).

Dense and reranker scores live on different scales. Calibrate a reranker tier
and a no-reranker tier separately (`--score rerank` / `--score dense`); the
default `auto` follows each response, which is only meaningful if every query in
both directories ran the same way.

Usage
-----
    floor_calibrate.py \
        --in-domain eval-out/miracl-id-200 \
        --qrels      rust/crates/kb-eval/datasets/miracl-id-200/qrels/test.tsv \
        --off-topic  eval-out/off-topic/miracl-id \
        --score      dense

The in-domain directory may contain `ingest-map.json` (the eval runner's
`corpus-key -> server document id` map). Without it the script assumes qrels
`corpus-id` values are the response `documentId` values. `--level auto` scores
pages when the qrels ids look like `doc#p3` (ViDoRe) and documents otherwise
(MIRACL); pass `--level document` or `--level page` to force one.

Empty in-domain results count as nDCG@10 = 0: a floor that silently deletes an
answer must not look free.

Stdlib only. This is a calibration instrument, not an assertion: it prints a
table, it does not fail a build.
"""

import argparse
import json
import math
import sys
from pathlib import Path
from typing import Dict, Iterable, List, Optional, Tuple


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--in-domain", required=True, type=Path, help="directory of in-domain response JSON files")
    parser.add_argument("--off-topic", required=True, type=Path, help="directory of off-topic response JSON files")
    parser.add_argument("--qrels", required=True, type=Path, help="BEIR/TREC qrels (query-id corpus-id score)")
    parser.add_argument(
        "--score",
        choices=["auto", "dense", "rerank"],
        default="auto",
        help="which score to sweep (default: follow stats.floorScoreKind, else infer)",
    )
    parser.add_argument("--k", type=int, default=10, help="cutoff for nDCG (default: 10)")
    parser.add_argument(
        "--level",
        choices=["auto", "document", "page"],
        default="auto",
        help="judge documents or pages; auto uses page when qrels ids contain '#' (default: auto)",
    )
    parser.add_argument(
        "--thresholds",
        help="comma-separated thresholds, e.g. 0.30,0.35,0.40; default: 21 values across the observed range",
    )
    parser.add_argument("--ingest-map", type=Path, help="corpus-key -> server document id map (default: <in-domain>/ingest-map.json)")
    parser.add_argument(
        "--target-empty",
        type=float,
        default=0.90,
        help="off-topic empty rate the recommendation must reach (default: 0.90)",
    )
    return parser.parse_args()


def read_json(path: Path) -> dict:
    with open(path, "r", encoding="utf-8") as handle:
        return json.load(handle)


def response_files(directory: Path) -> List[Path]:
    if not directory.is_dir():
        raise SystemExit(f"not a directory: {directory}")
    files = sorted(directory.glob("*.json"))
    if not files:
        raise SystemExit(f"no *.json responses in {directory}")
    return files


def parse_qrels(path: Path) -> Dict[str, Dict[str, int]]:
    gold: Dict[str, Dict[str, int]] = {}
    with open(path, "r", encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split()
            if len(parts) < 3 or parts[0] == "query-id":
                continue
            qid, docid = parts[0], parts[1]
            try:
                score = int(float(parts[2]))
            except ValueError:
                continue
            gold.setdefault(qid, {})[docid] = score
    return gold


def load_ingest_map(directory: Path, explicit: Optional[Path]) -> Dict[str, str]:
    path = explicit or (directory / "ingest-map.json")
    if not path.is_file():
        return {}
    raw = read_json(path)
    if not isinstance(raw, dict):
        raise SystemExit(f"{path} is not a JSON object")
    return {str(k): str(v) for k, v in raw.items()}


def map_document_id(corpus_id: str, ingest_map: Dict[str, str]) -> str:
    document_key = corpus_id.split("#", 1)[0]
    return ingest_map.get(document_key, ingest_map.get(corpus_id, document_key))


def map_judged_id(corpus_id: str, ingest_map: Dict[str, str], level: str) -> Optional[str]:
    """Map a qrels id onto the id a response reports at the chosen level.

    Document level drops the `#pN` suffix. Page level keeps it, because a
    ViDoRe judgement is about the page, not the whole PDF.
    """
    if level == "page":
        document_key, separator, page = corpus_id.partition("#")
        if not separator:
            return None
        return f"{map_document_id(document_key, ingest_map)}#{page}"
    return map_document_id(corpus_id, ingest_map)


def chunks_of(response: dict) -> List[dict]:
    chunks = response.get("chunks")
    return chunks if isinstance(chunks, list) else []


def chunk_score(chunk: dict, kind: str) -> Optional[float]:
    if kind == "rerank":
        retrieval = chunk.get("retrieval")
        if isinstance(retrieval, dict):
            value = retrieval.get("rerankScore")
            if isinstance(value, (int, float)):
                return float(value)
        return None
    value = chunk.get("similarity")
    if isinstance(value, (int, float)):
        return float(value)
    return None


def score_kind_for(response: dict, requested: str) -> str:
    if requested != "auto":
        return requested
    stats = response.get("stats")
    if isinstance(stats, dict):
        kind = stats.get("floorScoreKind")
        if kind in ("rerank", "dense"):
            return kind
    for chunk in chunks_of(response):
        retrieval = chunk.get("retrieval")
        if isinstance(retrieval, dict) and retrieval.get("rerankScore") is not None:
            return "rerank"
    return "dense"


def filter_chunks(response: dict, kind: str, threshold: float) -> List[dict]:
    kept = []
    for chunk in chunks_of(response):
        score = chunk_score(chunk, kind)
        if score is not None and score >= threshold:
            kept.append(chunk)
    return kept


def ranked_units(chunks: Iterable[dict], level: str) -> List[str]:
    seen = set()
    ranked = []
    for chunk in chunks:
        doc = chunk.get("documentId")
        if not isinstance(doc, str):
            continue
        if level == "page":
            page = chunk.get("page")
            if not isinstance(page, int):
                continue
            unit = f"{doc}#p{page}"
        else:
            unit = doc
        if unit in seen:
            continue
        seen.add(unit)
        ranked.append(unit)
    return ranked


def dcg(ranked: List[str], gold: Dict[str, int], k: int) -> float:
    total = 0.0
    for index, doc in enumerate(ranked[:k]):
        gain = gold.get(doc, 0)
        if gain > 0:
            total += float(gain) / math.log2(index + 2.0)
    return total


def ideal_dcg(gold: Dict[str, int], k: int) -> float:
    positives = sorted((gain for gain in gold.values() if gain > 0), reverse=True)
    return sum(float(gain) / math.log2(i + 2.0) for i, gain in enumerate(positives[:k]))


def ndcg(ranked: List[str], gold: Dict[str, int], k: int) -> float:
    ideal = ideal_dcg(gold, k)
    return dcg(ranked, gold, k) / ideal if ideal > 0 else 0.0


def observed_scores(responses: Dict[str, dict], kind: str) -> List[float]:
    scores = []
    for response in responses.values():
        for chunk in chunks_of(response):
            score = chunk_score(chunk, kind)
            if score is not None:
                scores.append(score)
    return scores


def default_thresholds(scores: List[float]) -> List[float]:
    if not scores:
        return [0.0]
    low, high = min(scores), max(scores)
    if math.isclose(low, high):
        return [round(low, 6)]
    return [round(low + (high - low) * i / 20.0, 6) for i in range(21)]


def main() -> int:
    args = parse_args()
    if args.k < 1:
        raise SystemExit("--k must be >= 1")

    in_domain = {path.stem: read_json(path) for path in response_files(args.in_domain)}
    off_topic = {path.stem: read_json(path) for path in response_files(args.off_topic)}
    qrels = parse_qrels(args.qrels)
    ingest_map = load_ingest_map(args.in_domain, args.ingest_map)
    level = args.level
    if level == "auto":
        level = "page" if any("#" in corpus_id for gold in qrels.values() for corpus_id in gold) else "document"
    if level == "page" and not any("#" in corpus_id for gold in qrels.values() for corpus_id in gold):
        raise SystemExit("--level page needs page-level qrels ids like `doc#p3`")

    in_kinds: Dict[str, int] = {}
    off_kinds: Dict[str, int] = {}
    for responses, kinds in ((in_domain, in_kinds), (off_topic, off_kinds)):
        for response in responses.values():
            kind = score_kind_for(response, args.score)
            kinds[kind] = kinds.get(kind, 0) + 1

    all_scores: List[float] = []
    for responses in (in_domain, off_topic):
        for response in responses.values():
            kind = score_kind_for(response, args.score)
            all_scores.extend(
                score for score in (chunk_score(c, kind) for c in chunks_of(response)) if score is not None
            )
    if args.thresholds:
        thresholds = [float(part) for part in args.thresholds.split(",") if part.strip()]
    else:
        thresholds = default_thresholds(all_scores)
    thresholds = sorted(set(thresholds))

    # Only queries with both a response and positive qrels are evaluable. A
    # missing response is reported, not silently treated as a good result.
    evaluable = [qid for qid in in_domain if qid in qrels and ideal_dcg(qrels[qid], args.k) > 0]
    missing = sorted(qid for qid in qrels if qid not in in_domain)
    unevaluable = sorted(qid for qid in in_domain if qid not in qrels or ideal_dcg(qrels[qid], args.k) == 0)

    print(f"level: {level}")
    print(f"in-domain responses: {len(in_domain)} ({in_kinds or 'none'})")
    print(f"off-topic responses: {len(off_topic)} ({off_kinds or 'none'})")
    print(f"evaluable in-domain queries: {len(evaluable)}; qrels without a response: {len(missing)}; responses without usable qrels: {len(unevaluable)}")
    if missing:
        print(f"  missing qids: {', '.join(missing[:10])}{' …' if len(missing) > 10 else ''}")
    if not evaluable:
        raise SystemExit("no evaluable in-domain queries; check --qrels and the response ids")
    if not off_topic:
        raise SystemExit("no off-topic responses")

    print()
    print(f"{'minScore':>10}  {'off-topic empty':>16}  {'nDCG@' + str(args.k):>10}  {'in-domain empty':>16}")
    print("-" * 60)

    best: Optional[Tuple[float, float, float]] = None
    for threshold in thresholds:
        empty_off = 0
        for response in off_topic.values():
            kind = score_kind_for(response, args.score)
            if not filter_chunks(response, kind, threshold):
                empty_off += 1
        off_rate = empty_off / len(off_topic)

        ndcgs = []
        empty_in = 0
        for qid in evaluable:
            response = in_domain[qid]
            kind = score_kind_for(response, args.score)
            kept = filter_chunks(response, kind, threshold)
            if not kept:
                empty_in += 1
            ranked = ranked_units(kept, level)
            gold = {}
            for corpus_id, score in qrels[qid].items():
                if score <= 0:
                    continue
                judged = map_judged_id(corpus_id, ingest_map, level)
                if judged is not None:
                    gold[judged] = score
            ndcgs.append(ndcg(ranked, gold, args.k))
        mean_ndcg = sum(ndcgs) / len(ndcgs)
        in_rate = empty_in / len(evaluable)

        print(f"{threshold:>10.4f}  {off_rate:>15.1%}  {mean_ndcg:>10.4f}  {in_rate:>15.1%}")

        # The recommendation is the highest in-domain nDCG among thresholds
        # that meet the off-topic target; ties go to the lower threshold (less
        # aggressive, so more robust to a miscalibrated score).
        if off_rate >= args.target_empty:
            if best is None or mean_ndcg > best[1] + 1e-12:
                best = (threshold, mean_ndcg, off_rate)

    print()
    if best is None:
        print(
            f"No threshold reached off-topic empty >= {args.target_empty:.0%}. "
            "The causal fix is a better score or a wider off-topic set, not a higher floor."
        )
        return 0
    threshold, mean_ndcg, off_rate = best
    print(
        f"Recommendation at off-topic empty >= {args.target_empty:.0%}: "
        f"minScore={threshold:.4f} (off-topic empty {off_rate:.1%}, nDCG@{args.k} {mean_ndcg:.4f}). "
        "Check the in-domain drop against the no-floor row (the lowest threshold)."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
