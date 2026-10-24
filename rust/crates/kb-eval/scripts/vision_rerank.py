"""Rerank a page run's top pages with a vision LLM that looks at the page images.

One chat call per query: the query plus the top --depth page images (96 dpi
JPEGs), asking for a 0-10 relevance score per image as JSON. The final order
blends the model's order with RUN's order by weighted RRF; weight and k are
chosen by 2-fold CV (even/odd queries), as in page_rerank.py, so the reported
number is held-out. Every answer is cached on disk by sha256(model, prompt
version, query, page ids, image bytes), so reruns are free.

    python -I vision_rerank.py --qrels Q --queries QUERIES.jsonl --run RUN.page.trec \
        --images ~/.cache/kb-cache/page-image-arm/pages-96dpi --model qwen/qwen3.7-flash
"""
import argparse, base64, glob, hashlib, json, math, os, random, re, sys, time, urllib.request
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor

ap = argparse.ArgumentParser()
ap.add_argument("--qrels", required=True)
ap.add_argument("--queries", required=True)
ap.add_argument("--run", required=True)
ap.add_argument("--images", required=True)
ap.add_argument("--model", required=True)
ap.add_argument("--depth", type=int, default=10)
ap.add_argument("--limit", type=int, default=0, help="only the first N queries (smoke test)")
ap.add_argument("--workers", type=int, default=4)
ap.add_argument("--cache", default=os.path.expanduser("~/.cache/kb-cache/vision-rerank"))
ap.add_argument("--url", default="https://openrouter.ai/api/v1/chat/completions")
ap.add_argument("--key-env", default="OPENROUTER_API_KEY")
ap.add_argument("--meta", default="", help="query-meta.jsonl: per content type nDCG@10")
ap.add_argument("--no-reasoning", action="store_true",
                help="send reasoning.enabled=false (thinking models such as qwen3.8-flash spend max_tokens on reasoning)")
a = ap.parse_args()
KEY = os.environ.get(a.key_env, "")
PROMPT_VERSION = "v1"

gold = defaultdict(dict)
for l in open(a.qrels):
    p = l.split()
    if p[0] != "query-id":
        gold[p[0]][p[1]] = int(p[2])
queries = {json.loads(l)["_id"]: json.loads(l)["text"] for l in open(a.queries)}
run = defaultdict(list)
for l in open(a.run):
    q, _, d, *_ = l.split()
    if d not in run[q]:
        run[q].append(d)

img_path = {}
for f in glob.glob(os.path.join(a.images, "*", "p-*.jpg")):
    doc = os.path.basename(os.path.dirname(f))
    n = int(re.search(r"p-(\d+)\.jpg$", f).group(1))
    img_path[f"{doc}#p{n}"] = f

INSTRUCTIONS = (
    "You are ranking document pages for a search query. You are given the query and {n} page "
    "images, numbered 1 to {n} in the order shown. Score each page 0-10 for how well it answers "
    "the query (10 = directly answers it, including from a chart or table; 0 = unrelated). "
    'Reply with JSON only: {{"scores": [s1, s2, ..., s{n}]}}.'
)

spend = {"cost": 0.0, "calls": 0, "cached": 0}
abort = {"failed": 0, "now": False}


def score_query(qid):
    cands = [d for d in run.get(qid, [])[: a.depth] if d in img_path]
    if not cands:
        return qid, [], []
    blobs = [open(img_path[d], "rb").read() for d in cands]
    h = hashlib.sha256()
    # The flag joins the key only when set, so existing cache entries stay valid.
    for part in [a.model, PROMPT_VERSION, *(["no-reasoning"] if a.no_reasoning else []), queries[qid], *cands]:
        h.update(part.encode() + b"\0")
    for b in blobs:
        h.update(hashlib.sha256(b).digest())
    key = h.hexdigest()
    path = os.path.join(a.cache, key[:2], key + ".json")
    if os.path.exists(path):
        spend["cached"] += 1
        return qid, cands, json.load(open(path))["scores"]
    content = [{"type": "text", "text": INSTRUCTIONS.format(n=len(cands)) + "\n\nQuery: " + queries[qid]}]
    for i, b in enumerate(blobs, 1):
        content.append({"type": "text", "text": f"Page {i}:"})
        content.append({"type": "image_url", "image_url": {"url": "data:image/jpeg;base64," + base64.b64encode(b).decode()}})
    body = {"model": a.model, "messages": [{"role": "user", "content": content}],
            "temperature": 0, "max_tokens": 300, "response_format": {"type": "json_object"},
            "usage": {"include": True}}
    if a.no_reasoning:
        body["reasoning"] = {"enabled": False}
    req = urllib.request.Request(a.url, data=json.dumps(body).encode(),
                                 headers={"Authorization": "Bearer " + KEY, "Content-Type": "application/json"})
    if abort["now"]:
        return qid, cands, None
    for attempt in range(6):
        try:
            r = json.load(urllib.request.urlopen(req, timeout=180))
            text = r["choices"][0]["message"]["content"] or ""
            m = re.search(r"\{.*\}", text, re.S)
            scores = [float(x) for x in json.loads(m.group(0))["scores"]]
            if len(scores) != len(cands):
                raise ValueError(f"{len(scores)} scores for {len(cands)} pages")
            break
        except Exception as e:
            if attempt == 5:
                print(f"  q{qid}: giving up ({e})", file=sys.stderr)
                abort["failed"] += 1
                if abort["failed"] >= 8 and spend["calls"] == 0:
                    abort["now"] = True  # the model never answers: stop paying for retries
                return qid, cands, None
            time.sleep(min(30, 2 ** attempt))
    spend["calls"] += 1
    spend["cost"] += float(r.get("usage", {}).get("cost", 0) or 0)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    json.dump({"scores": scores, "usage": r.get("usage")}, open(path, "w"))
    return qid, cands, scores


qids = sorted(gold, key=int)
if a.limit:
    qids = qids[: a.limit]
scored = {}
with ThreadPoolExecutor(a.workers) as ex:
    for n, (qid, cands, s) in enumerate(ex.map(score_query, qids), 1):
        scored[qid] = (cands, s)
        if n % 50 == 0:
            print(f"  scored {n}/{len(qids)}  ${spend['cost']:.3f}", file=sys.stderr)
failed = [q for q in qids if scored[q][1] is None]
if abort["now"]:
    sys.exit(f"aborted: the first {abort['failed']} queries failed and none succeeded (${spend['cost']:.4f})")
print(f"calls {spend['calls']}, cached {spend['cached']}, failed {len(failed)}, cost ${spend['cost']:.4f}")


def ndcg(rk, g):
    dcg = sum(g.get(d, 0) / math.log2(i + 2) for i, d in enumerate(rk[:10]))
    idl = sorted(g.values(), reverse=True)[:10]
    idcg = sum(x / math.log2(i + 2) for i, x in enumerate(idl))
    return dcg / idcg if idcg else 0


def final(qid, w, k):
    cands, s = scored[qid]
    if not s:  # failed or no images: keep the run's order
        return run.get(qid, [])
    # Stable sort: ties keep the run's order.
    by_v = [d for _, _, d in sorted(zip(s, range(len(cands)), cands), key=lambda x: (-x[0], x[1]))]
    sc = defaultdict(float)
    for r, d in enumerate(cands):
        sc[d] += (1 - w) / (k + r + 1)
    for r, d in enumerate(by_v):
        sc[d] += w / (k + r + 1)
    head = [d for d, _ in sorted(sc.items(), key=lambda kv: -kv[1])]
    return head + [d for d in run.get(qid, []) if d not in sc]


grid = [(w / 10, k) for w in range(11) for k in [1, 2, 5, 10, 20, 60]]
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


print(f"run {sum(base)/len(base):.3f} | vision only {sum(pure)/len(pure):.3f} | CV blend {sum(cv)/len(cv):.3f}")
print("delta vision-only vs run: %+.3f [%+.3f, %+.3f]" % ci(pure, base))
print("delta CV blend vs run:    %+.3f [%+.3f, %+.3f]" % ci(cv, base))

if a.meta:
    import ast
    types = defaultdict(list)
    for l in open(a.meta):
        m = json.loads(l)
        v = m.get("content_type")
        vals = ast.literal_eval(v) if isinstance(v, str) and v.startswith("[") else (v if isinstance(v, list) else [v])
        for t in vals:
            types[t].append(str(m.get("query_id", m.get("_id"))))
    for t in ("Table", "Chart", "Text"):
        qs = [q for q in types.get(t, []) if q in per_q]
        if qs:
            b = sum(ndcg(run.get(q, []), gold[q]) for q in qs) / len(qs)
            c = sum(per_q[q] for q in qs) / len(qs)
            print(f"  {t:6s} n={len(qs):3d} run {b:.3f} -> CV blend {c:.3f} ({c-b:+.3f})")
