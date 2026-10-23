"""Rerank a fused page run with a text cross-encoder over each page's full text.

For each query, the top --depth pages of RUN are scored by the reranker on
(query, page text); the final order blends the reranker's ranking with RUN's
ranking by weighted RRF (rerank weight a, constant k). a and k are chosen by
2-fold CV (even/odd queries), exactly like fuse_cv.py, so the reported number
is held-out. Reranker scores are cached on disk by sha256(model, query, text).

    python -I page_rerank.py --qrels Q --queries QUERIES.jsonl --page-text-dir DUMP \
        --run fused.page.trec --url http://host:8082/v1/rerank --key-env LLAMA_GPU_API_KEY
"""
import argparse, hashlib, json, math, os, random, re, sys, urllib.request
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor

ap = argparse.ArgumentParser()
ap.add_argument("--qrels", required=True)
ap.add_argument("--queries", required=True)
ap.add_argument("--page-text-dir", required=True)
ap.add_argument("--run", required=True)
ap.add_argument("--url", required=True)
ap.add_argument("--key", default="")
ap.add_argument("--model", default="bge-reranker-v2-m3")
ap.add_argument("--depth", type=int, default=30)
ap.add_argument("--chars", type=int, default=3000)
ap.add_argument("--batch", type=int, default=10)
ap.add_argument("--workers", type=int, default=2)
ap.add_argument("--cache", default=os.path.expanduser("~/.cache/kb-cache/page-rerank"))
ap.add_argument("--out", default="")
a = ap.parse_args()

gold = defaultdict(dict)
for l in open(a.qrels):
    p = l.split()
    if p[0] != "query-id":
        gold[p[0]][p[1]] = int(p[2])
queries = {json.loads(l)["_id"]: json.loads(l)["text"] for l in open(a.queries)}
texts = {}
for f in os.listdir(a.page_text_dir):
    if f.endswith(".txt"):
        parts = re.split(r"^===== page (\d+) =====\n", open(os.path.join(a.page_text_dir, f)).read(), flags=re.M)
        for i in range(1, len(parts), 2):
            texts[f"{f[:-4]}#p{int(parts[i])}"] = parts[i + 1].strip()[: a.chars]
run = defaultdict(list)
for l in open(a.run):
    q, _, d, *_ = l.split()
    if d not in run[q]:
        run[q].append(d)


def ckey(q, t):
    return hashlib.sha256(f"{a.model}\0{q}\0{t}".encode()).hexdigest()


def cpath(h):
    return os.path.join(a.cache, h[:2], h + ".json")


def rerank(q, docs):
    out = [None] * len(docs)
    todo = []
    for i, t in enumerate(docs):
        p = cpath(ckey(q, t))
        if os.path.exists(p):
            out[i] = json.load(open(p))
        else:
            todo.append(i)
    for j in range(0, len(todo), a.batch):
        idx = todo[j:j + a.batch]
        body = {"model": a.model, "query": q, "documents": [docs[i] or " " for i in idx]}
        req = urllib.request.Request(a.url, data=json.dumps(body).encode(),
                                     headers={"Content-Type": "application/json", "Authorization": "Bearer " + a.key})
        r = json.load(urllib.request.urlopen(req, timeout=300))
        for res in r["results"]:
            i = idx[res["index"]]
            out[i] = res["relevance_score"]
            p = cpath(ckey(q, docs[i]))
            os.makedirs(os.path.dirname(p), exist_ok=True)
            json.dump(out[i], open(p, "w"))
    return out


qids = sorted(gold, key=int)


def score_query(qid):
    cands = run.get(qid, [])[: a.depth]
    s = rerank(queries[qid], [texts.get(d, "") for d in cands])
    return qid, cands, s


scored = {}
with ThreadPoolExecutor(a.workers) as ex:
    for n, (qid, cands, s) in enumerate(ex.map(score_query, qids), 1):
        scored[qid] = (cands, s)
        if n % 50 == 0:
            print(f"  scored {n}/{len(qids)}", file=sys.stderr)


def ndcg(rk, g):
    dcg = sum(g.get(d, 0) / math.log2(i + 2) for i, d in enumerate(rk[:10]))
    idl = sorted(g.values(), reverse=True)[:10]
    idcg = sum(x / math.log2(i + 2) for i, x in enumerate(idl))
    return dcg / idcg if idcg else 0


def final(qid, w, k):
    cands, s = scored[qid]
    by_rr = [d for _, d in sorted(zip(s, cands), key=lambda x: -x[0])]
    sc = defaultdict(float)
    for r, d in enumerate(cands):
        sc[d] += (1 - w) / (k + r + 1)
    for r, d in enumerate(by_rr):
        sc[d] += w / (k + r + 1)
    head = [d for d, _ in sorted(sc.items(), key=lambda kv: -kv[1])]
    return head + [d for d in run.get(qid, []) if d not in sc]


grid = [(w, k) for w in [i / 10 for i in range(11)] for k in [1, 2, 5, 10, 20, 60]]
mean = lambda qs, w, k: sum(ndcg(final(q, w, k), gold[q]) for q in qs) / len(qs)
folds = [qids[0::2], qids[1::2]]
per_q = {}
for i in (0, 1):
    tr, te = folds[i], folds[1 - i]
    w, k = max(grid, key=lambda p: mean(tr, *p))
    print(f"fold {i}: w={w} k={k} train={mean(tr, w, k):.3f} test={mean(te, w, k):.3f}")
    for q in te:
        per_q[q] = ndcg(final(q, w, k), gold[q])
base = [ndcg(run.get(q, []), gold[q]) for q in qids]
pure = [ndcg(final(q, 1.0, 60), gold[q]) for q in qids]
cv = [per_q[q] for q in qids]


def ci(x, y, n=5000):
    random.seed(0)
    d = [p - q for p, q in zip(x, y)]
    bs = sorted(sum(random.choice(d) for _ in d) / len(d) for _ in range(n))
    return sum(d) / len(d), bs[int(.025 * n)], bs[int(.975 * n)]


print(f"run {sum(base)/len(base):.3f} | rerank only {sum(pure)/len(pure):.3f} | CV blend {sum(cv)/len(cv):.3f}")
print("delta rerank-only vs run: %+.3f [%+.3f, %+.3f]" % ci(pure, base))
print("delta CV blend vs run:    %+.3f [%+.3f, %+.3f]" % ci(cv, base))
if a.out:
    w, k = max(grid, key=lambda p: mean(qids, *p))
    with open(a.out, "w") as f:
        for q in qids:
            for r, d in enumerate(final(q, w, k)[:100], 1):
                f.write(f"{q} Q0 {d} {r} {1.0/r:.6f} page-rerank\n")
    print(f"wrote {a.out} with full-data w={w} k={k} (not held-out)")
