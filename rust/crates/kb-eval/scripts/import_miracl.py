# /// script
# dependencies = [
#   "huggingface_hub",
# ]
# ///
"""Import a reproducible MIRACL Indonesian benchmark subset for kb-eval.

Dataset: MIRACL (Multilingual Information Retrieval Across a Continuum of Languages)
Language: Indonesian (id), dev split.
Licence: Apache-2.0

Outputs a BEIR-layout dataset:
  <out_dir>/
    corpus/<docid>.md
    queries.jsonl
    qrels/test.tsv
"""

import argparse
import gzip
import json
import os
import random
import sys
from collections import defaultdict
from pathlib import Path
from huggingface_hub import hf_hub_download

# Fixed Hugging Face revision hashes for strict reproducibility
MIRACL_REPO = "miracl/miracl"
MIRACL_REVISION = "5be20db9509754dadad47689368639fcec739c00"

CORPUS_REPO = "miracl/miracl-corpus"
CORPUS_REVISION = "d921ec7e349ce0d28daf30b2da9da5ee698bef0d"

CORPUS_FILES = [
    "miracl-corpus-v1.0-id/docs-0.jsonl.gz",
    "miracl-corpus-v1.0-id/docs-1.jsonl.gz",
    "miracl-corpus-v1.0-id/docs-2.jsonl.gz",
]


def sanitize_docid(docid: str) -> str:
    """Sanitize MIRACL docid (e.g. '115796#6') for filesystem and BEIR compatibility."""
    return docid.replace("#", "_")


def main():
    parser = argparse.ArgumentParser(description="Import MIRACL Indonesian dev subset for kb-eval")
    parser.add_argument("--seed", type=int, default=42, help="Random seed for sampling (default: 42)")
    parser.add_argument("--num-queries", type=int, default=200, help="Number of dev queries to sample (default: 200)")
    parser.add_argument("--num-distractors", type=int, default=1000, help="Number of unjudged distractor passages (default: 1000)")
    parser.add_argument(
        "--force-qids",
        type=str,
        default="",
        help="Comma-separated query IDs to force into the sampled topics",
    )
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=Path(__file__).resolve().parent.parent / "datasets" / "miracl-id-200",
        help="Target dataset directory",
    )
    args = parser.parse_args()

    print(f"Importing MIRACL id dev subset:")
    print(f"  Seed: {args.seed}")
    print(f"  Num queries: {args.num_queries}")
    print(f"  Num distractors: {args.num_distractors}")
    if args.force_qids:
        print(f"  Forced query IDs: {args.force_qids}")
    print(f"  Output directory: {args.out_dir}")

    # 1. Download dev topics and qrels from miracl/miracl
    print("\nDownloading topics and qrels from Hugging Face...")
    topics_path = hf_hub_download(
        repo_id=MIRACL_REPO,
        filename="miracl-v1.0-id/topics/topics.miracl-v1.0-id-dev.tsv",
        revision=MIRACL_REVISION,
        repo_type="dataset",
    )
    qrels_path = hf_hub_download(
        repo_id=MIRACL_REPO,
        filename="miracl-v1.0-id/qrels/qrels.miracl-v1.0-id-dev.tsv",
        revision=MIRACL_REVISION,
        repo_type="dataset",
    )

    # 2. Parse topics
    topics = []
    with open(topics_path, "r", encoding="utf-8") as f:
        for line in f:
            parts = line.strip().split("\t", 1)
            if len(parts) == 2:
                topics.append((parts[0], parts[1]))
    # Sort deterministically by integer ID
    topics.sort(key=lambda t: int(t[0]))
    print(f"Total dev topics available: {len(topics)}")

    # 3. Sample queries
    rng = random.Random(args.seed)
    forced_qids = set(q.strip() for q in args.force_qids.split(",") if q.strip())
    forced_topics = [t for t in topics if t[0] in forced_qids]
    remaining_topics = [t for t in topics if t[0] not in forced_qids]
    needed = max(0, args.num_queries - len(forced_topics))
    sampled_remaining = rng.sample(remaining_topics, needed)
    sampled_topics = forced_topics + sampled_remaining
    sampled_topics.sort(key=lambda t: int(t[0]))
    sampled_qids = set(t[0] for t in sampled_topics)
    print(f"Sampled {len(sampled_topics)} queries with seed {args.seed}")

    # 4. Parse qrels
    all_dev_judged_docids = set()
    sample_qrels = defaultdict(list)
    with open(qrels_path, "r", encoding="utf-8") as f:
        for line in f:
            parts = line.strip().split("\t")
            if len(parts) == 4:
                qid, _, docid, score_str = parts
            elif len(parts) == 3:
                qid, docid, score_str = parts
            else:
                continue
            score = int(score_str)
            all_dev_judged_docids.add(docid)
            if qid in sampled_qids:
                sample_qrels[qid].append((docid, score))

    judged_docids_needed = set()
    relevant_qrels = []  # (qid, safe_docid, 1)
    for qid, _ in sampled_topics:
        for docid, score in sample_qrels[qid]:
            judged_docids_needed.add(docid)
            if score > 0:
                relevant_qrels.append((qid, sanitize_docid(docid), 1))

    print(f"Judged passages for sampled queries: {len(judged_docids_needed)} (relevant + non-relevant hard distractors)")
    print(f"Relevant judgements (score > 0): {len(relevant_qrels)}")

    # 5. Download and iterate over miracl-corpus to extract needed passages and sample unjudged distractors
    print("\nScanning corpus to extract judged passages and sample unjudged distractors...")
    judged_corpus = {}  # docid -> (title, text)
    distractor_pool = []  # list of (docid, title, text)
    distractor_count = 0
    distractor_rng = random.Random(args.seed)

    for cfile in CORPUS_FILES:
        print(f"  Reading {cfile}...")
        path = hf_hub_download(
            repo_id=CORPUS_REPO,
            filename=cfile,
            revision=CORPUS_REVISION,
            repo_type="dataset",
        )
        with gzip.open(path, "rt", encoding="utf-8") as f:
            for line in f:
                item = json.loads(line)
                docid = item["docid"]
                title = item.get("title", "")
                text = item.get("text", "")

                if docid in judged_docids_needed:
                    judged_corpus[docid] = (title, text)
                elif docid not in all_dev_judged_docids:
                    # Reservoir sampling for distractors
                    if len(distractor_pool) < args.num_distractors:
                        distractor_pool.append((docid, title, text))
                    else:
                        j = distractor_rng.randint(0, distractor_count)
                        if j < args.num_distractors:
                            distractor_pool[j] = (docid, title, text)
                    distractor_count += 1

    missing = judged_docids_needed - set(judged_corpus.keys())
    if missing:
        print(f"ERROR: {len(missing)} judged passages were not found in corpus!", file=sys.stderr)
        sys.exit(1)

    print(f"Found all {len(judged_corpus)} judged passages.")
    print(f"Sampled {len(distractor_pool)} random unjudged distractors (from pool of {distractor_count}).")

    # Combine corpus
    final_corpus = {}
    for docid, (title, text) in judged_corpus.items():
        final_corpus[sanitize_docid(docid)] = (title, text)
    for docid, title, text in distractor_pool:
        final_corpus[sanitize_docid(docid)] = (title, text)

    print(f"\nTotal corpus size: {len(final_corpus)} documents.")

    # 6. Write output files in BEIR layout
    corpus_dir = args.out_dir / "corpus"
    qrels_dir = args.out_dir / "qrels"
    corpus_dir.mkdir(parents=True, exist_ok=True)
    qrels_dir.mkdir(parents=True, exist_ok=True)

    print(f"Writing corpus files to {corpus_dir}...")
    for safe_id, (title, text) in final_corpus.items():
        file_path = corpus_dir / f"{safe_id}.md"
        content = f"# {title}\n\n{text}\n" if title else f"{text}\n"
        with open(file_path, "w", encoding="utf-8") as f:
            f.write(content)

    print(f"Writing queries to {args.out_dir / 'queries.jsonl'}...")
    with open(args.out_dir / "queries.jsonl", "w", encoding="utf-8") as f:
        for qid, text in sampled_topics:
            f.write(json.dumps({"_id": qid, "text": text}, ensure_ascii=False) + "\n")

    print(f"Writing qrels to {qrels_dir / 'test.tsv'}...")
    with open(qrels_dir / "test.tsv", "w", encoding="utf-8") as f:
        f.write("query-id\tcorpus-id\tscore\n")
        # Deterministic sorting
        relevant_qrels.sort(key=lambda r: (int(r[0]), r[1]))
        for qid, safe_id, score in relevant_qrels:
            f.write(f"{qid}\t{safe_id}\t{score}\n")

    print(f"\nDone! Dataset written to {args.out_dir}")
    print(f"  Corpus documents: {len(final_corpus)}")
    print(f"  Queries: {len(sampled_topics)}")
    print(f"  Relevant judgements: {len(relevant_qrels)}")


if __name__ == "__main__":
    main()
