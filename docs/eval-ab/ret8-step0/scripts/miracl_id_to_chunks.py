#!/usr/bin/env python3
"""Convert ~/ret8/data/miracl-id-200 into embed_ab.py --chunks format.

chunk_items() reads a jsonl with {"doc", "content", "title"} per chunk; the
retrieval unit is the chunk and scoring maps to documents via `doc`. Here the
natural unit is the corpus .md file: doc = the file name without extension,
content = the file text. qrels reference "<doc>.md", so we also rewrite the
qrels to bare doc ids (to_documents maps unit->doc, and the qrels' corpus ids
must equal doc ids).

Outputs <out>/chunks.jsonl and <out>/qrels/test.tsv (queries.jsonl is copied).
"""
import argparse
import json
import os
import sys

ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
ap.add_argument("--src", default=os.path.expanduser("~/ret8/data/miracl-id-200"))
ap.add_argument("--out", required=True)
a = ap.parse_args()
os.makedirs(os.path.join(a.out, "qrels"), exist_ok=True)

n = 0
with open(os.path.join(a.out, "chunks.jsonl"), "w", encoding="utf-8") as f:
    for name in sorted(os.listdir(os.path.join(a.src, "corpus"))):
        if not name.endswith(".md"):
            continue
        doc = name[:-3]
        text = open(os.path.join(a.src, "corpus", name), encoding="utf-8").read()
        f.write(json.dumps({"doc": doc, "content": text.strip(), "title": ""}, ensure_ascii=False) + "\n")
        n += 1

with open(os.path.join(a.src, "qrels", "test.tsv"), encoding="utf-8") as f, \
     open(os.path.join(a.out, "qrels", "test.tsv"), "w", encoding="utf-8") as g:
    g.write("query-id\tcorpus-id\tscore\n")
    for line in f:
        p = line.split()
        if len(p) < 3 or p[0] == "query-id":
            continue
        g.write(f"{p[0]}\t{p[1].removesuffix('.md')}\t{p[2]}\n")

import shutil
shutil.copy(os.path.join(a.src, "queries.jsonl"), os.path.join(a.out, "queries.jsonl"))
print(f"wrote {n} chunks, qrels and queries to {a.out}")
