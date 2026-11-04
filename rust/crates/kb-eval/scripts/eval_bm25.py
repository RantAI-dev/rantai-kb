# /// script
# dependencies = [
#   "rank-bm25",
#   "ranx",
# ]
# ///
"""Independent BM25 baseline scoring with rank_bm25 and ranx.

Scores any BEIR-layout dataset (corpus/*.md, queries.jsonl, qrels/test.tsv).
Uses rank_bm25 (BM25Okapi) with \\w+ lowercase tokenization.
"""

import argparse
import glob
import json
import re
from pathlib import Path
from rank_bm25 import BM25Okapi
from ranx import Qrels, Run, evaluate


def main():
    parser = argparse.ArgumentParser(description="Run independent BM25 evaluation")
    parser.add_argument(
        "--dataset",
        type=Path,
        required=True,
        help="Path to dataset directory (containing corpus/, queries.jsonl, qrels/test.tsv)",
    )
    args = parser.parse_args()

    dataset = args.dataset
    doc_files = sorted(glob.glob(str(dataset / "corpus" / "*.md")))
    if not doc_files:
        raise ValueError(f"No corpus files found in {dataset / 'corpus'}")

    print(f"Loading {len(doc_files)} corpus files...")
    doc_ids = [Path(f).stem for f in doc_files]
    corpus = [re.findall(r"\w+", Path(f).read_text(encoding="utf-8").lower()) for f in doc_files]

    print("Building BM25Okapi index...")
    bm25 = BM25Okapi(corpus)

    queries = []
    with open(dataset / "queries.jsonl", encoding="utf-8") as f:
        for line in f:
            item = json.loads(line)
            queries.append((item["_id"], item["text"]))
    print(f"Loaded {len(queries)} queries.")

    print("Querying BM25...")
    run_dict = {}
    for qid, qtext in queries:
        q_tokens = re.findall(r"\w+", qtext.lower())
        scores = bm25.get_scores(q_tokens)
        top_indices = sorted(range(len(scores)), key=lambda i: scores[i], reverse=True)[:100]
        run_dict[str(qid)] = {doc_ids[i]: float(scores[i]) for i in top_indices if scores[i] > 0}

    qrels_dict = {}
    with open(dataset / "qrels" / "test.tsv", encoding="utf-8") as f:
        for line in f:
            parts = line.strip().split()
            if len(parts) >= 3 and parts[0] != "query-id":
                qid, docid, score = parts[0], parts[1], int(parts[2])
                qrels_dict.setdefault(qid, {})[docid] = score
    qrels = Qrels(qrels_dict)
    run = Run(run_dict)

    res = evaluate(
        qrels,
        run,
        metrics=["ndcg@10", "recall@10", "hit_rate@1", "mrr@10", "precision@10"],
    )
    formatted = {k: round(float(v), 4) for k, v in res.items()}
    print(f"independent BM25 on {dataset.name}:", formatted)


if __name__ == "__main__":
    main()
