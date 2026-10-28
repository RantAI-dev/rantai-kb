"""Offline test of a page-image retrieval arm on a ViDoRe-style dataset.

Renders every PDF page (pdftoppm), embeds the images and the queries with a
multimodal embedder over OpenRouter, ranks pages by cosine, and writes:

  <out>/image.page.trec          image-only page ranking (top 100)
  <out>/fused-<name>.page.trec   RRF(k) of the image run with each --text run

Every embedding is cached on disk by (model, kind, sha256(content)), so a rerun
costs nothing (AGENTS.md rule 7). Costs are summed from OpenRouter's `usage`.

    python -I page_image_arm.py --dataset DS --out OUT --text name=run.page.trec ...
"""
import argparse, base64, concurrent.futures as cf, hashlib, json, math, os
import subprocess, sys, time, urllib.error, urllib.request
from collections import defaultdict

ap = argparse.ArgumentParser()
ap.add_argument("--dataset", required=True)
ap.add_argument("--out", required=True)
ap.add_argument("--model", default="voyageai/voyage-multimodal-3.5")
ap.add_argument("--dpi", type=int, default=96)
ap.add_argument("--cache", default=os.path.expanduser("~/.cache/kb-cache/page-image-arm"))
ap.add_argument("--env", default=os.path.join(os.path.dirname(__file__), "../../../../.env"))
ap.add_argument("--batch", type=int, default=4)
ap.add_argument("--workers", type=int, default=4)
ap.add_argument("--rrf-k", type=int, default=60)
ap.add_argument("--max-cost", type=float, default=1.0, help="stop if spend passes this (USD)")
ap.add_argument("--text", action="append", default=[], help="name=path to a page-level TREC run")
ap.add_argument("--page-text-dir", default="", help="extract_dump outputs (<doc>.txt); embed each page as its text + image")
ap.add_argument("--page-text-chars", type=int, default=6000)
a = ap.parse_args()

key = next(l.split("=", 1)[1].strip().strip('"') for l in open(a.env) if l.startswith("OPENROUTER_API_KEY="))
os.makedirs(a.out, exist_ok=True)
img_dir = os.path.join(a.cache, f"pages-{a.dpi}dpi")
emb_dir = os.path.join(a.cache, "emb", a.model.replace("/", "_"))
os.makedirs(emb_dir, exist_ok=True)
spent = 0.0

# 1. Render pages.
corpus = os.path.join(a.dataset, "corpus")
pages = []  # (page_id, path)
for pdf in sorted(f for f in os.listdir(corpus) if f.endswith(".pdf")):
    doc = pdf[:-4]
    d = os.path.join(img_dir, doc)
    if not os.path.isdir(d) or not os.listdir(d):
        os.makedirs(d, exist_ok=True)
        subprocess.run(["pdftoppm", "-r", str(a.dpi), "-jpeg", "-jpegopt", "quality=80",
                        os.path.join(corpus, pdf), os.path.join(d, "p")], check=True)
    for f in sorted(os.listdir(d)):
        n = int(f.split("-")[1].split(".")[0])
        pages.append((f"{doc}#p{n}", os.path.join(d, f)))
print(f"pages: {len(pages)}", file=sys.stderr)


def cache_path(kind, payload):
    h = hashlib.sha256(f"{a.model}\0{kind}\0".encode() + payload).hexdigest()
    return os.path.join(emb_dir, kind, h[:2], h + ".json")


def call(inputs, input_type):
    global spent
    body = {"model": a.model, "input": inputs, "input_type": input_type}
    for attempt in range(6):
        req = urllib.request.Request(
            "https://openrouter.ai/api/v1/embeddings", data=json.dumps(body).encode(),
            headers={"Authorization": "Bearer " + key, "Content-Type": "application/json"})
        try:
            r = json.load(urllib.request.urlopen(req, timeout=120))
            spent += float((r.get("usage") or {}).get("cost") or 0)
            return [d["embedding"] for d in sorted(r["data"], key=lambda d: d["index"])]
        except urllib.error.HTTPError as e:
            msg = e.read()[:300]
            if e.code in (429, 500, 502, 503, 504) and attempt < 5:
                time.sleep(2 ** attempt)
                continue
            raise SystemExit(f"HTTP {e.code}: {msg}")
        except (urllib.error.URLError, TimeoutError):
            if attempt < 5:
                time.sleep(2 ** attempt)
                continue
            raise


def embed_all(items, kind, to_input, input_type):
    """items: [(id, payload_bytes)] -> {id: vector}"""
    out, todo = {}, []
    for i, payload in items:
        p = cache_path(kind, payload)
        if os.path.exists(p):
            out[i] = json.load(open(p))
        else:
            todo.append((i, payload, p))
    print(f"{kind}: {len(items) - len(todo)} cached, {len(todo)} to embed", file=sys.stderr)
    batches = [todo[j:j + a.batch] for j in range(0, len(todo), a.batch)]

    def run(batch):
        if spent > a.max_cost:
            raise SystemExit(f"stopping: spend ${spent:.3f} passed --max-cost")
        vecs = call([to_input(pl) for _, pl, _ in batch], input_type)
        for (i, _, p), v in zip(batch, vecs):
            os.makedirs(os.path.dirname(p), exist_ok=True)
            json.dump(v, open(p + ".tmp", "w"))
            os.replace(p + ".tmp", p)
            out[i] = v
        return len(batch)

    done = 0
    with cf.ThreadPoolExecutor(a.workers) as ex:
        for n in ex.map(run, batches):
            done += n
            if done % 100 < a.batch:
                print(f"  {kind}: {done}/{len(todo)}  ${spent:.3f}", file=sys.stderr)
    return out


def image_input(b):
    return {"content": [{"type": "image_url",
                         "image_url": {"url": "data:image/jpeg;base64," + base64.b64encode(b).decode()}}]}


def page_texts(d):
    import re
    out = {}
    for f in os.listdir(d):
        if not f.endswith(".txt"):
            continue
        parts = re.split(r"^===== page (\d+) =====\n", open(os.path.join(d, f)).read(), flags=re.M)
        for i in range(1, len(parts), 2):
            out[f"{f[:-4]}#p{int(parts[i])}"] = parts[i + 1].strip()[: a.page_text_chars]
    return out


if a.page_text_dir:
    texts = page_texts(a.page_text_dir)
    print(f"page texts: {len(texts)}", file=sys.stderr)

    def text_image_input(payload):
        pid, img = payload.split(b"\0", 1)
        t = texts.get(pid.decode(), "")
        content = ([{"type": "text", "text": t}] if t else []) + image_input(img)["content"]
        return {"content": content}

    page_vec = embed_all([(pid, pid.encode() + b"\0" + open(p, "rb").read()) for pid, p in pages],
                         "page+text", text_image_input, "document")
else:
    page_vec = embed_all([(pid, open(p, "rb").read()) for pid, p in pages], "page", image_input, "document")
queries = [json.loads(l) for l in open(os.path.join(a.dataset, "queries.jsonl"))]
query_vec = embed_all([(q["_id"], q["text"].encode()) for q in queries], "query",
                      lambda b: b.decode(), "query")
print(f"spend this run: ${spent:.4f}", file=sys.stderr)


def norm(v):
    s = math.sqrt(sum(x * x for x in v)) or 1.0
    return [x / s for x in v]


pids = [pid for pid, _ in pages]
P = [norm(page_vec[pid]) for pid in pids]
image_run = {}
with open(os.path.join(a.out, ("textimage" if a.page_text_dir else "image") + ".page.trec"), "w") as f:
    for q in queries:
        qv = norm(query_vec[q["_id"]])
        scores = sorted(((sum(x * y for x, y in zip(qv, pv)), pid) for pid, pv in zip(pids, P)), reverse=True)[:100]
        image_run[q["_id"]] = [pid for _, pid in scores]
        for r, (s, pid) in enumerate(scores, 1):
            f.write(f"{q['_id']} Q0 {pid} {r} {s:.6f} image\n")


def read_run(path):
    run = defaultdict(list)
    for line in open(path):
        q, _, d, *_ = line.split()
        if d not in run[q]:
            run[q].append(d)
    return run


for spec in a.text:
    name, path = spec.split("=", 1)
    text = read_run(path)
    with open(os.path.join(a.out, f"fused-{name}.page.trec"), "w") as f:
        for q in queries:
            qid = q["_id"]
            s = defaultdict(float)
            for lst in (text.get(qid, []), image_run.get(qid, [])):
                for r, d in enumerate(lst):
                    s[d] += 1.0 / (a.rrf_k + r + 1)
            for r, (d, v) in enumerate(sorted(s.items(), key=lambda kv: -kv[1])[:100], 1):
                f.write(f"{qid} Q0 {d} {r} {v:.6f} fused-{name}\n")
print("done", file=sys.stderr)
