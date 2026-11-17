"""Build a rerank candidate pool: the top N documents per query from a dense run.

`rerank_bakeoff.py` needs `candidates.jsonl`, and the file that produced the
November rerank numbers was never committed — so comparing a new reranker to
those numbers needs the pool rebuilt. This does that from the engine's own
chunks (`rkdump`) and any OpenAI-compatible embedder, writing the exact shape
the bake-off reads:

    {"qid": "17", "candidates": [{"doc": "1000580_1", "text": "..."}, ...]}

The pool is dense-only on purpose: the bake-off measures what the *reranker*
does to a fixed pool, and a pool built the same way for every reranker is what
makes the deltas comparable. Documents are truncated to `--chars`, as an
embedder would see them.

    python3 make_candidates.py --chunks ~/rkdump/chunks.jsonl \\
        --dataset ~/kbdata/bakeoff/datasets/miracl-id-200 \\
        --url http://127.0.0.1:8100/v1/embeddings --api-key ... \\
        --model qwen3-embed-0.6b --depth 50 --out /tmp/candidates.jsonl
"""
import argparse
import json
import math
import os
import sys
import urllib.request

ap = argparse.ArgumentParser()
ap.add_argument("--chunks", required=True)
ap.add_argument("--dataset", required=True, help="dir with queries.jsonl and qrels/")
ap.add_argument("--url", required=True)
ap.add_argument("--model", default="qwen3-embed-0.6b")
ap.add_argument("--api-key", default="")
ap.add_argument("--depth", type=int, default=50)
ap.add_argument("--chars", type=int, default=2000, help="document text kept in the pool")
ap.add_argument("--query-prefix", default="")
ap.add_argument("--out", required=True)


def embed(texts):
    body = {"model": a.model, "input": texts}
    req = urllib.request.Request(
        a.url, data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json",
                 **({"Authorization": "Bearer " + a.api_key} if a.api_key else {})})
    return json.load(urllib.request.urlopen(req, timeout=600))["data"]


def norm(v):
    s = math.sqrt(sum(x * x for x in v)) or 1.0
    return [x / s for x in v]


a = ap.parse_args()

# The engine's chunks, one entry per chunk, as the retrieval unit.
items, owner = {}, {}
for n, line in enumerate(open(a.chunks, encoding="utf-8")):
    c = json.loads(line)
    text = f"{c.get('title') or ''}\n\n{c['content']}".strip()
    uid = f"{c['doc']}#c{n}"
    items[uid] = text
    owner[uid] = c["doc"]
print(f"chunks: {len(items)} over {len(set(owner.values()))} documents", file=sys.stderr)

qrels = set()
qdir = os.path.join(a.dataset, "qrels")
files = [qdir] if os.path.isfile(qdir) else [os.path.join(qdir, f) for f in sorted(os.listdir(qdir))]
for path in files:
    for line in open(path, encoding="utf-8"):
        p = line.split()
        if len(p) >= 3 and p[0] != "query-id":
            qrels.add(p[0])
queries = [q for q in map(json.loads, open(os.path.join(a.dataset, "queries.jsonl"), encoding="utf-8"))
           if q["_id"] in qrels]
print(f"queries: {len({q['_id'] for q in queries})}", file=sys.stderr)

uids = list(items)
vecs = {}
for i in range(0, len(uids), 16):
    part = [items[u] for u in uids[i:i + 16]]
    for u, d in zip(uids[i:i + 16], embed(part)):
        vecs[u] = norm(d["embedding"])
    if i % 512 == 0:
        print(f"  embedded {i}/{len(uids)}", file=sys.stderr)

with open(a.out, "w", encoding="utf-8") as out:
    for q in queries:
        qv = norm(embed([a.query_prefix + q["text"]])[0]["embedding"])
        scored = sorted(((sum(x * y for x, y in zip(qv, v)), u) for u, v in vecs.items()), reverse=True)
        seen, cands = set(), []
        for _, u in scored:
            doc = owner[u]
            if doc in seen:
                continue
            seen.add(doc)
            cands.append({"doc": doc, "text": items[u][:a.chars]})
            if len(cands) >= a.depth:
                break
        out.write(json.dumps({"qid": q["_id"], "candidates": cands}) + "\n")
print(f"wrote {a.out}: {len(queries)} queries x {a.depth} candidates", file=sys.stderr)
