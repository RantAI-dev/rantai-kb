# /// script
# dependencies = ["pyarrow", "httpx"]
# ///
"""Import a ViDoRe v3 dataset as a kb-eval dataset with page-level qrels.

ViDoRe v3 (https://hf.co/vidore, CC-BY-4.0) pairs real enterprise PDFs with
human-written queries and human-labelled relevant pages. This keeps the PDFs
themselves as the corpus, so kb-server ingests them through its own
extraction exactly as it would a customer upload, and the eval scores the
page the engine cites.

Output (BEIR/TREC layout, see kb-eval/README.md):
  corpus/<doc_id>.pdf      the dataset's own PDFs (not committed: re-run this)
  queries.jsonl            {"_id", "text"}, one language
  qrels/test.tsv           query-id  <doc_id>#p<N>  score   (N 1-based)
  query-meta.jsonl         query_types / content_type per query, for breakdowns
  PROVENANCE.txt           dataset, revision, counts

ViDoRe's `page_number_in_doc` is 0-based and covers every page of each PDF
(the corpus has exactly sum(page counts) rows), so page N = page_number_in_doc + 1.

    uv run import_vidore.py --dataset vidore/vidore_v3_hr --language english --out DIR
"""

import argparse
import io
import json
from collections import Counter
from pathlib import Path

import httpx
import pyarrow.parquet as pq

PINNED = {
    "vidore/vidore_v3_hr": "0cdf0979f2c5a0fd3e335e6373b9da48a9fe3bc3",
    "vidore/vidore_v3_computer_science": "d5cc75883d92e294f0c0fc2662551c9708a06ebc",
}


def fetch(client: httpx.Client, repo: str, rev: str, path: str) -> bytes:
    url = f"https://huggingface.co/datasets/{repo}/resolve/{rev}/{path}"
    r = client.get(url, follow_redirects=True, timeout=600)
    r.raise_for_status()
    return r.content


def table(client, repo, rev, config, columns=None):
    files = [
        s["path"]
        for s in client.get(f"https://huggingface.co/api/datasets/{repo}/tree/{rev}/{config}").json()
        if s["path"].endswith(".parquet")
    ]
    rows = []
    for f in sorted(files):
        rows += pq.read_table(io.BytesIO(fetch(client, repo, rev, f)), columns=columns).to_pylist()
    return rows


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dataset", default="vidore/vidore_v3_hr")
    ap.add_argument("--revision", default=None)
    ap.add_argument("--language", default="english")
    ap.add_argument("--out", type=Path, required=True)
    args = ap.parse_args()
    rev = args.revision or PINNED.get(args.dataset)
    if not rev:
        raise SystemExit(f"no pinned revision for {args.dataset}; pass --revision")

    out = args.out
    (out / "corpus").mkdir(parents=True, exist_ok=True)
    (out / "qrels").mkdir(exist_ok=True)
    with httpx.Client() as client:
        docs = table(client, args.dataset, rev, "documents_metadata")
        corpus = table(client, args.dataset, rev, "corpus", ["corpus_id", "doc_id", "page_number_in_doc"])
        queries = table(client, args.dataset, rev, "queries")
        qrels = table(client, args.dataset, rev, "qrels", ["query_id", "corpus_id", "score"])

        pages = Counter(r["doc_id"] for r in corpus)
        for d in docs:
            if pages[d["doc_id"]] != d["page_number"]:
                raise SystemExit(f"{d['doc_id']}: {pages[d['doc_id']]} corpus pages, {d['page_number']} in the PDF")
            target = out / "corpus" / f"{d['doc_id']}.pdf"
            if not target.exists():
                target.write_bytes(fetch(client, args.dataset, rev, f"pdfs/{d['file_name']}"))

    page_of = {r["corpus_id"]: (r["doc_id"], r["page_number_in_doc"] + 1) for r in corpus}
    keep = {q["query_id"]: q for q in queries if q["language"] == args.language}
    with (out / "queries.jsonl").open("w", encoding="utf-8") as f, \
            (out / "query-meta.jsonl").open("w", encoding="utf-8") as m:
        for qid in sorted(keep):
            q = keep[qid]
            f.write(json.dumps({"_id": str(qid), "text": q["query"]}, ensure_ascii=False) + "\n")
            m.write(json.dumps({"_id": str(qid), "query_types": q["query_types"],
                                "content_type": q["content_type"], "query_format": q["query_format"]}) + "\n")
    n_qrels = 0
    with (out / "qrels" / "test.tsv").open("w", encoding="utf-8") as f:
        f.write("query-id\tcorpus-id\tscore\n")
        for r in qrels:
            if r["query_id"] in keep and r["score"] > 0:
                doc, page = page_of[r["corpus_id"]]
                f.write(f"{r['query_id']}\t{doc}#p{page}\t{r['score']}\n")
                n_qrels += 1
    (out / "PROVENANCE.txt").write_text(
        f"{args.dataset} @ {rev} (CC-BY-4.0)\nlanguage: {args.language}\n"
        f"documents: {len(docs)}, pages: {len(corpus)}, queries: {len(keep)}, page qrels: {n_qrels}\n"
        f"built by rust/crates/kb-eval/scripts/import_vidore.py\n")
    print((out / "PROVENANCE.txt").read_text())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
