"""Rank-based vs score-based blending of a page reranker with the fused page run.

Reads reranker scores only from page_rerank.py's disk cache (never calls an API:
a cache miss is an error), so it is free to run after page_rerank.py has run.

For the top --depth pages of RUN, three blends are tuned by 2-fold CV (even/odd
queries) and scored on the held-out half:

  rrf     weighted RRF of the rerank order and the run order (what kb-server does)
  minmax  a * minmax(rerank score) + (1 - a) * minmax(run score)
  z       a * z(rerank score) + (1 - a) * z(run score)

    python -I page_rerank_scoreblend.py --qrels Q --queries QUERIES.jsonl \
        --page-text-dir DUMP --run fused.page.trec --model voyageai/rerank-2.5
"""
import argparse, hashlib, json, math, os, random, re, statistics
from collections import defaultdict

ap = argparse.ArgumentParser()
ap.add_argument("--qrels", required=True)
ap.add_argument("--queries", required=True)
ap.add_argument("--page-text-dir", required=True)
ap.add_argument("--run", required=True)
ap.add_argument("--model", required=True)
ap.add_argument("--depth", type=int, default=30)
ap.add_argument("--chars", type=int, default=3000)
ap.add_argument("--cache", default=os.path.expanduser("~/.cache/kb-cache/page-rerank"))
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
run, run_score = defaultdict(list), defaultdict(dict)
for l in open(a.run):
    q, _, d, _, s, *_ = l.split()
    if d not in run[q]:
        run[q].append(d)
        run_score[q][d] = float(s)


def cached(q, t):
    h = hashlib.sha256(f"{a.model}\0{q}\0{t}".encode()).hexdigest()
    p = os.path.join(a.cache, h[:2], h + ".json")
    if not os.path.exists(p):
        raise SystemExit(f"cache miss for model {a.model}: run page_rerank.py first")
    return json.load(open(p))


qids = sorted(gold, key=int)
cand = {q: run.get(q, [])[: a.depth] for q in qids}
rr = {q: [cached(queries[q], texts.get(d, "")) for d in cand[q]] for q in qids}


def ndcg(rk, g):
    dcg = sum(g.get(d, 0) / math.log2(i + 2) for i, d in enumerate(rk[:10]))
    idl = sorted(g.values(), reverse=True)[:10]
    idcg = sum(x / math.log2(i + 2) for i, x in enumerate(idl))
    return dcg / idcg if idcg else 0


def norm(xs, kind):
    if not xs:
        return xs
    if kind == "minmax":
        lo, hi = min(xs), max(xs)
        return [(x - lo) / (hi - lo) if hi > lo else 0.0 for x in xs]
    mu, sd = statistics.fmean(xs), statistics.pstdev(xs)
    return [(x - mu) / sd if sd else 0.0 for x in xs]


def final(q, method, w, k):
    c, s = cand[q], rr[q]
    sc = defaultdict(float)
    if method == "rrf":
        by_rr = [d for _, d in sorted(zip(s, c), key=lambda x: -x[0])]
        for r, d in enumerate(c):
            sc[d] += (1 - w) / (k + r + 1)
        for r, d in enumerate(by_rr):
            sc[d] += w / (k + r + 1)
    else:
        a_ = norm(s, method)
        b_ = norm([run_score[q][d] for d in c], method)
        for d, x, y in zip(c, a_, b_):
            sc[d] = w * x + (1 - w) * y
    head = [d for d, _ in sorted(sc.items(), key=lambda kv: -kv[1])]
    return head + [d for d in run.get(q, []) if d not in sc]


grids = {
    "rrf": [(w / 10, k) for w in range(11) for k in [1, 2, 5, 10, 20, 60]],
    "minmax": [(w / 20, 0) for w in range(21)],
    "z": [(w / 20, 0) for w in range(21)],
}
mean = lambda qs, m, w, k: sum(ndcg(final(q, m, w, k), gold[q]) for q in qs) / len(qs)
folds = [qids[0::2], qids[1::2]]
per = {}
for m, grid in grids.items():
    per[m] = {}
    for i in (0, 1):
        tr, te = folds[i], folds[1 - i]
        w, k = max(grid, key=lambda p: mean(tr, m, *p))
        print(f"{m:6s} fold {i}: w={w} k={k} test={mean(te, m, w, k):.3f}")
        for q in te:
            per[m][q] = ndcg(final(q, m, w, k), gold[q])


def ci(x, y, n=5000):
    random.seed(0)
    d = [p - q for p, q in zip(x, y)]
    bs = sorted(sum(random.choice(d) for _ in d) / len(d) for _ in range(n))
    return sum(d) / len(d), bs[int(.025 * n)], bs[int(.975 * n)]


base = [ndcg(run.get(q, []), gold[q]) for q in qids]
vec = {m: [per[m][q] for q in qids] for m in per}
print(f"run {sum(base)/len(base):.3f} | " + " | ".join(f"{m} {sum(v)/len(v):.3f}" for m, v in vec.items()))
for m in ("minmax", "z"):
    print(f"delta {m} vs rrf: %+.3f [%+.3f, %+.3f]" % ci(vec[m], vec["rrf"]))
