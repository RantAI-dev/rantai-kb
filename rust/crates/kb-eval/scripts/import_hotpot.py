# /// script
# dependencies = [
#   "huggingface_hub",
#   "pyarrow",
# ]
# ///
"""Import a reproducible HotpotQA distractor dev subset for kb-eval.

Why this dataset and not MIRACL again: §11's claim about graph RAG is that it
wins on *multi-hop* and loses on *fact lookup*, and proving it needs two sets.
MIRACL Indonesian (already in `datasets/miracl-id-200`) is the fact-lookup
control; this is the treatment. HotpotQA distractor dev is the standard
multi-hop set whose supporting facts are published with the questions, so a qrels
judgement is a citation rather than a guess.

Dataset: HotpotQA (Yang et al., ICLR 2018), distractor config, validation split.
Licence: CC BY-SA 4.0 (see the dataset card; confirmed on the pinned revision
below, not assumed from the paper).

Outputs a BEIR-layout dataset:
  <out_dir>/
    corpus/<slug>.md
    corpus-index.tsv        slug -> original Wikipedia title
    queries.jsonl
    qrels/test.tsv
    answers.jsonl
    split.tsv               qid -> tune|test, the 100/100 split the eval reports on
"""

import argparse
import hashlib
import json
import random
import re
import unicodedata
from pathlib import Path

from huggingface_hub import hf_hub_download
import pyarrow.parquet as pq

# Fixed Hugging Face revision for strict reproducibility. Verify with:
#   curl -s https://huggingface.co/api/datasets/hotpotqa/hotpot_qa | jq -r .sha
HOTPOT_REPO = "hotpotqa/hotpot_qa"
HOTPOT_REVISION = "1908d6afbbead072334abe2965f91bd2709910ab"
HOTPOT_FILE = "distractor/validation-00000-of-00001.parquet"

SEED = 20261017
N_QUESTIONS = 200


def slugify(title: str) -> str:
    """A filesystem- and BEIR-safe corpus key for a Wikipedia title.

    HotpotQA titles are Wikipedia article names: they contain slashes, quotes,
    parentheses and non-ASCII letters. The corpus key has to survive a filename
    and a TSV cell, so it is an ASCII slug plus a stable short hash of the
    original title — the hash, not an incrementing counter, so the key does not
    depend on iteration order and stays stable when a question is added.
    """
    normalised = unicodedata.normalize("NFKD", title)
    ascii_only = normalised.encode("ascii", "ignore").decode("ascii")
    slug = re.sub(r"[^a-z0-9]+", "-", ascii_only.lower()).strip("-")
    if not slug:
        slug = "t"
    # `hashlib`, not `hash()`: CPython randomises `str.__hash__` per process, so
    # the corpus keys would differ on every re-import.
    digest = hashlib.sha256(title.encode("utf-8")).hexdigest()[:8]
    return f"{slug[:64]}-{digest}"


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Import HotpotQA distractor dev subset for kb-eval"
    )
    parser.add_argument("--seed", type=int, default=SEED, help="Sampling seed (default: 20261017)")
    parser.add_argument(
        "--num-questions", type=int, default=N_QUESTIONS, help="Questions to sample (default: 200)"
    )
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=Path(__file__).resolve().parent.parent / "datasets" / "hotpot-200",
        help="Target dataset directory",
    )
    args = parser.parse_args()

    print("Importing HotpotQA distractor dev subset:")
    print(f"  repo:     {HOTPOT_REPO}")
    print(f"  revision: {HOTPOT_REVISION}")
    print(f"  file:     {HOTPOT_FILE}")
    print(f"  seed:     {args.seed}")
    print(f"  queries:  {args.num_questions}")

    path = hf_hub_download(
        repo_id=HOTPOT_REPO,
        filename=HOTPOT_FILE,
        repo_type="dataset",
        revision=HOTPOT_REVISION,
    )
    print(f"  cached:   {path}")

    rows = pq.read_table(path).to_pylist()
    print(f"  rows:     {len(rows)}")

    # Deterministic selection: sort by id, then sample with a fixed seed. Sorting
    # first means the sample depends on the seed and the revision only, not on the
    # order the parquet reader happens to return.
    rows.sort(key=lambda r: r["id"])
    rng = random.Random(args.seed)
    sampled = sorted(rng.sample(rows, args.num_questions), key=lambda r: r["id"])

    corpus_dir = args.out_dir / "corpus"
    qrels_dir = args.out_dir / "qrels"
    corpus_dir.mkdir(parents=True, exist_ok=True)
    qrels_dir.mkdir(parents=True, exist_ok=True)

    slug_by_title: dict[str, str] = {}
    title_by_slug: dict[str, str] = {}
    qrels: list[tuple[str, str, int]] = []
    queries: list[tuple[str, str]] = []
    answers: list[tuple[str, str]] = []

    for row in sampled:
        titles = row["context"]["title"]
        sentences = row["context"]["sentences"]
        for title, sents in zip(titles, sentences):
            if title in slug_by_title:
                continue
            slug = slugify(title)
            slug_by_title[title] = slug
            title_by_slug[slug] = title
            body = " ".join(s.strip() for s in sents if s and s.strip())
            (corpus_dir / f"{slug}.md").write_text(f"# {title}\n\n{body}\n", encoding="utf-8")

        qid = row["id"]
        queries.append((qid, row["question"]))
        answers.append((qid, row["answer"]))
        for gold in dict.fromkeys(row["supporting_facts"]["title"]):
            slug = slug_by_title[gold]
            qrels.append((qid, slug, 1))

    print(f"  corpus documents: {len(slug_by_title)}")

    with open(args.out_dir / "queries.jsonl", "w", encoding="utf-8") as f:
        for qid, text in queries:
            f.write(json.dumps({"_id": qid, "text": text}, ensure_ascii=False) + "\n")

    with open(args.out_dir / "answers.jsonl", "w", encoding="utf-8") as f:
        for qid, text in answers:
            f.write(json.dumps({"_id": qid, "answer": text}, ensure_ascii=False) + "\n")

    with open(qrels_dir / "test.tsv", "w", encoding="utf-8") as f:
        f.write("query-id\tcorpus-id\tscore\n")
        for qid, slug, score in sorted(qrels, key=lambda r: (r[0], r[1])):
            f.write(f"{qid}\t{slug}\t{score}\n")

    with open(args.out_dir / "corpus-index.tsv", "w", encoding="utf-8") as f:
        f.write("corpus-id\ttitle\n")
        for slug, title in sorted(title_by_slug.items()):
            f.write(f"{slug}\t{title}\n")

    # The 100/100 tune/test split. Position in the *sorted* question list, not a
    # hash of the id: half the questions are tuning material for the arm's
    # parameters and half are the numbers that get reported, and the split has to
    # be reproducible from the file alone.
    half = len(queries) // 2
    with open(args.out_dir / "split.tsv", "w", encoding="utf-8") as f:
        f.write("query-id\tsplit\n")
        for i, (qid, _) in enumerate(queries):
            f.write(f"{qid}\t{'tune' if i < half else 'test'}\n")

    print(f"  queries:          {len(queries)} (tune {half}, test {len(queries) - half})")
    print(f"  relevant qrels:   {len(qrels)}")
    print(f"  dataset written to {args.out_dir}")


if __name__ == "__main__":
    main()