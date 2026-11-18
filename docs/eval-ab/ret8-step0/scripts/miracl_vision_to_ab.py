#!/usr/bin/env python3
"""Convert nvidia/miracl-vision <lang>/ into embed_ab.py's dataset format.

Reads corpus.parquet, queries.parquet, qrels.parquet for a language and writes
an output dir with:
  queries.jsonl   {"_id": ..., "text": ...}          (read_queries)
  qrels/test.tsv  "query-id corpus-id score"          (read_qrels: whitespace,
                  header line starting with query-id is skipped)
  pages.jsonl     {"_id": ..., "text": ...}           (page_items; optional
                  "image" is not used — the images live in images.parquet as
                  page-id -> jpeg bytes and are materialized on demand by
                  miracl_vision_images.py, which fills in the "image" field
                  with a path relative to pages.jsonl)

Corpus ids are kept verbatim so the qrels match. No subsampling: every corpus
page is written.
"""
import argparse
import json
import os
import sys

import pyarrow.parquet as pq


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--src", required=True, help="dir with corpus/queries/qrels parquet for one language")
    ap.add_argument("--out", required=True, help="output dataset dir")
    a = ap.parse_args()
    os.makedirs(os.path.join(a.out, "qrels"), exist_ok=True)

    cor = pq.read_table(os.path.join(a.src, "corpus.parquet"))
    print("corpus schema:", cor.schema.names, "rows:", cor.num_rows)
    qry = pq.read_table(os.path.join(a.src, "queries.parquet"))
    print("queries schema:", qry.schema.names, "rows:", qry.num_rows)
    qr = pq.read_table(os.path.join(a.src, "qrels.parquet"))
    print("qrels schema:", qr.schema.names, "rows:", qr.num_rows)

    # figure out column names
    ccols = cor.schema.names
    cid_col = next(c for c in ("docid", "doc_id", "_id", "id", "page_id") if c in ccols)
    text_col = next(c for c in ("text", "passage", "content", "body") if c in ccols)
    qcols = qry.schema.names
    qid_col = next(c for c in ("query_id", "qid", "_id", "id") if c in qcols)
    qtext_col = next(c for c in ("query", "text", "question") if c in qcols)
    rcols = qr.schema.names
    rq_col = next(c for c in ("query_id", "qid", "query-id") if c in rcols)
    rd_col = next(c for c in ("doc_id", "docid", "corpus-id", "corpus_id", "positive_docid") if c in rcols)
    rs_col = next((c for c in ("score", "rel", "relevance") if c in rcols), None)
    print(f"columns: corpus {cid_col}/{text_col}, queries {qid_col}/{qtext_col}, qrels {rq_col}/{rd_col}/{rs_col}")

    # ids are int64 in qrels and string in corpus/queries: normalize to string
    qids_in_qrels = {str(x) for x in qr.column(rq_col).to_pylist()}

    with open(os.path.join(a.out, "queries.jsonl"), "w", encoding="utf-8") as qf, \
         open(os.path.join(a.out, "qrels", "test.tsv"), "w", encoding="utf-8") as rf:
        rf.write("query-id\tcorpus-id\tscore\n")
        nq = 0
        for q in qry.to_pylist():
            if str(q[qid_col]) not in qids_in_qrels:
                continue
            qf.write(json.dumps({"_id": str(q[qid_col]), "text": q[qtext_col]}, ensure_ascii=False) + "\n")
            nq += 1
        for r in qr.to_pylist():
            s = r[rs_col] if rs_col else 1
            rf.write(f"{r[rq_col]}\t{r[rd_col]}\t{s}\n")
    print(f"wrote {nq} queries (of {qry.num_rows}), {qr.num_rows} qrels")

    with open(os.path.join(a.out, "pages.jsonl"), "w", encoding="utf-8") as pf:
        np = 0
        for row in cor.to_pylist():
            pf.write(json.dumps({"_id": str(row[cid_col]), "text": row[text_col] or ""}, ensure_ascii=False) + "\n")
            np += 1
    print(f"wrote {np} pages")
    return 0


if __name__ == "__main__":
    sys.exit(main())
