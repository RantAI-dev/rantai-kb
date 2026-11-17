"""Does a page (VL) embedder's text vector hold up as the text arm?

Embeds every page's text with the same model, uncut and cut to N characters, and
scores both against the qrels. It exists because the first run of the one-embedder
experiment (`README.md`) showed a near-random text arm on whole pages and a healthy
one on a truncated slice; this isolates length as the cause.

    python3 probe_text_length.py --pages ~/kbdata/bakeoff/datasets/vidore3-hr-en-pages/pages.jsonl \\
        --dataset ~/kbdata/bakeoff/datasets/vidore3-hr-en \\
        --url http://host:8202/v1/embeddings --api-key ... --queries 20 \\
        --caps 0,600,1200,2400 --out probe.json

`--caps 0` means the full text. Candidates are the pages the 20 queries' qrels name,
so the probe is a small, fair ranking problem rather than a random sample.
"""
import argparse, json, math, os, random, urllib.request

ap = argparse.ArgumentParser()
ap.add_argument("--dataset", required=True)
ap.add_argument("--pages", required=True)
ap.add_argument("--url", required=True, help="vLLM chat-embeddings endpoint")
ap.add_argument("--model", default="qwen3-vl-embed-2b")
ap.add_argument("--api-key", default="")
ap.add_argument("--queries", type=int, default=20)
ap.add_argument("--caps", default="0,600,1200,2400")
ap.add_argument("--out", default="")
a = ap.parse_args()


def embed(text):
    body = {"model": a.model, "messages": [{"role": "user", "content": [{"type": "text", "text": text}]}]}
    req = urllib.request.Request(a.url, data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json",
                                          **({"Authorization": "Bearer " + a.api_key} if a.api_key else {})})
    return json.load(urllib.request.urlopen(req, timeout=300))["data"][0]["embedding"]


gold = {}
qdir = os.path.join(a.dataset, "qrels")
files = [qdir] if os.path.isfile(qdir) else [os.path.join(qdir, f) for f in sorted(os.listdir(qdir))]
for path in files:
    for line in open(path):
        p = line.split()
        if len(p) >= 3 and p[0] != "query-id":
            gold.setdefault(p[0], set()).add(p[1])
queries = [q for q in map(json.loads, open(os.path.join(a.dataset, "queries.jsonl"))) if q["_id"] in gold][:a.queries]
pages = [p for p in map(json.loads, open(a.pages)) if p["_id"] in {d for q in queries for d in gold[q["_id"]]}]
random.seed(1)
pages = pages[:150]


def ndcg(ranked, rel, k=10):
    dcg = sum(1 / math.log2(i + 2) for i, d in enumerate(ranked[:k]) if d in rel)
    ideal = sum(1 / math.log2(i + 2) for i in range(min(len(rel), k)))
    return dcg / ideal if ideal else 0.0


result = {"queries": len(queries), "candidates": len(pages), "caps": {}}
for cap in [int(c) for c in a.caps.split(",")]:
    vecs = [embed(p["text"][:cap] if cap else p["text"]) for p in pages]
    scores = []
    for q in queries:
        qv = embed(q["text"])
        ranked = [p["_id"] for p, _ in sorted(zip(pages, vecs),
                                              key=lambda t: -sum(x * y for x, y in zip(qv, t[1])))]
        scores.append(ndcg(ranked, gold[q["_id"]]))
    mean = sum(scores) / len(scores)
    result["caps"][str(cap)] = mean
    print(f"cap {cap or 'full':>5}: nDCG@10 {mean:.3f} over {len(queries)} queries, {len(pages)} candidates")
if a.out:
    json.dump(result, open(a.out, "w"), indent=2)
