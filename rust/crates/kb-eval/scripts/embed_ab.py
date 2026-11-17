"""A/B two embedding models over the same items, offline, with a disk cache.

The question this exists for: should a tier embed text and page images with one
model (one vector space) or two? It ranks pages (or passages) with each model,
scores nDCG@10 / MRR@10 / Hit@1 / Recall@10 against the dataset's qrels, and
prints the paired bootstrap delta between the two runs. Nothing is tuned here:
the same queries, qrels and items for every model.

    # text items, two models, viDoRe-style qrels
    python3 embed_ab.py --dataset ~/kbdata/bakeoff/datasets/vidore3-hr-en \
        --pages ~/kbdata/bakeoff/datasets/vidore3-hr-en-pages/pages.jsonl \
        --out /tmp/ab --mode text \
        --model qwen06b=http://host:8081/v1/embeddings#openai \
        --model qwenvl2b=http://host:8082/v1/embeddings#chat

    # a page's text and its image in one space (what a one-embedder index does)
    python3 embed_ab.py ... --mode both --model qwenvl2b=...#chat

`--mode text|image|both`: `both` embeds every page's text *and* image with the
model and scores each page by its best hit in the one space (max cosine), which
is what a single collection with one vector per unit gives you.

Embedders are any OpenAI-compatible endpoint:
  openai  POST /v1/embeddings {"model","input":[texts]}
  chat    POST /v1/embeddings {"model","messages":[{"role":"user","content":[...]}]}
          one item per request — vLLM's Chat-Embeddings shape, which does not
          batch pages (`kb-inference`'s PageEmbedFormat::VllmChat).

Every vector is cached by model + sha256(payload) under --cache, so a re-run and
a second experiment cost nothing (AGENTS.md rule 7).

    python3 embed_ab.py --self-test      # no GPU, no network: proves the wiring
"""
import argparse
import base64
import concurrent.futures as cf
import hashlib
import json
import math
import os
import sys
import urllib.error
import urllib.request
from collections import defaultdict

import numpy as np

QRELS_HEADER = ("query-id", "qid", "query_id")


def eprint(*a):
    print(*a, file=sys.stderr)


# ─── Embedders ─────────────────────────────────────────────────────────────


class Embedder:
    """One model at one URL, with a JSONL cache keyed by model + payload hash."""

    # Constructed by from_spec() so the CLI stays one flag per model.
    def __init__(self, name, url, kind, cache_root, workers, batch, api_key=""):
        self.name, self.url, self.kind = name, url, kind
        self.workers, self.batch, self.api_key = workers, batch, api_key
        self.mem = {}
        self.path = os.path.join(cache_root, name.replace("/", "_") + ".jsonl")
        if os.path.exists(self.path):
            for line in open(self.path, encoding="utf-8"):
                row = json.loads(line)
                self.mem[row["h"]] = row["d"]
        self.calls = 0

    @classmethod
    def from_spec(cls, spec, **kw):
        """`name=url#kind`, e.g. `qwenvl2b=http://host:8000/v1/embeddings#chat`."""
        left, _, kind = spec.partition("#")
        name, _, url = left.partition("=")
        if not (name and url):
            raise SystemExit(f"--model wants name=url#kind, got {spec!r}")
        if kind not in ("openai", "chat"):
            raise SystemExit(f"--model {name}: kind must be openai or chat, got {kind!r}")
        return cls(name, url, kind, **kw)

    def h(self, payload):
        return hashlib.sha256(self.name.encode() + b"\0" + payload).hexdigest()

    def _body(self, payloads):
        if self.kind == "openai":
            return [{"model": self.name,
                     "input": [p.decode("utf-8", "replace") for p in payloads]}]
        # chat: one item per request, the shape vLLM answers for a page.
        out = []
        for p in payloads:
            if b"\0" in p[:200] and p.split(b"\0", 1)[0] == b"img":
                content = [{"type": "image_url", "image_url": {"url":
                            "data:image/jpeg;base64," + base64.b64encode(p.split(b"\0", 1)[1]).decode()}}]
            else:
                content = [{"type": "text", "text": p.decode("utf-8", "replace")}]
            out.append({"model": self.name, "messages": [{"role": "user", "content": content}]})
        return out

    def _post(self, body):
        req = urllib.request.Request(
            self.url, data=json.dumps(body).encode(),
            headers={"Content-Type": "application/json",
                     **({"Authorization": "Bearer " + self.api_key} if self.api_key else {})})
        for attempt in range(5):
            try:
                self.calls += 1
                r = json.load(urllib.request.urlopen(req, timeout=600))
                return [d["embedding"] for d in sorted(r["data"], key=lambda d: d.get("index", 0))]
            except urllib.error.HTTPError as e:
                body_txt = e.read()[:200]
                if e.code in (429, 500, 502, 503, 504) and attempt < 4:
                    import time
                    time.sleep(2 ** attempt)
                    continue
                raise SystemExit(f"{self.name}: HTTP {e.code}: {body_txt}")
            except (urllib.error.URLError, TimeoutError):
                if attempt < 4:
                    import time
                    time.sleep(2 ** attempt)
                    continue
                raise
        raise SystemExit(f"{self.name}: gave up")

    def embed(self, items, label):
        """items: [(key, payload_bytes)] -> {key: vector}"""
        out, todo = {}, []
        for k, payload in items:
            p = self.h(payload)
            if p in self.mem:
                out[k] = self.mem[p]
            else:
                todo.append((k, payload, p))
        eprint(f"  {self.name}/{label}: {len(items) - len(todo)} cached, {len(todo)} to embed")
        if not todo:
            return out

        def work(chunk):
            bodies = self._body([p for _, p, _ in chunk])
            if self.kind == "openai":
                vecs = self._post(bodies[0])
            else:
                vecs = []
                for b in bodies:
                    vecs.extend(self._post(b))
            if len(vecs) != len(chunk):
                raise SystemExit(f"{self.name}: {len(vecs)} vectors for {len(chunk)} items")
            for (k, _, p), v in zip(chunk, vecs):
                self.mem[p] = v
                with open(self.path, "a", encoding="utf-8") as f:
                    f.write(json.dumps({"h": p, "d": v}) + "\n")
                out[k] = v

        step = 1 if self.kind == "chat" else self.batch
        chunks = [todo[i:i + step] for i in range(0, len(todo), step)]
        done = 0
        with cf.ThreadPoolExecutor(self.workers) as ex:
            for n in ex.map(work, chunks):
                done += 1
                if done % 25 == 0 or done == len(chunks):
                    eprint(f"    {label}: {min(done * step, len(todo))}/{len(todo)}")
        return out

    def dim(self):
        return len(next(iter(self.mem.values()))) if self.mem else 0


# ─── Data ──────────────────────────────────────────────────────────────────


def read_qrels(dataset):
    gold = defaultdict(dict)
    d = os.path.join(dataset, "qrels")
    files = [d] if os.path.isfile(d) else [os.path.join(d, f) for f in sorted(os.listdir(d))]
    for path in files:
        for line in open(path, encoding="utf-8"):
            p = line.split()
            if len(p) < 3 or p[0] in QRELS_HEADER:
                continue
            gold[p[0]][p[1]] = int(float(p[2]))
    return gold


def read_queries(dataset):
    return [json.loads(l) for l in open(os.path.join(dataset, "queries.jsonl"), encoding="utf-8")]


def page_items(dataset, pages_jsonl):
    """[(page_id, text_payload, image_bytes_or_None)]"""
    base = os.path.dirname(os.path.abspath(pages_jsonl))
    rows = [json.loads(l) for l in open(pages_jsonl, encoding="utf-8")]
    items = []
    for r in rows:
        img = None
        if r.get("image"):
            p = os.path.join(base, r["image"]) if not os.path.isabs(r["image"]) else r["image"]
            if os.path.exists(p):
                img = open(p, "rb").read()
        items.append((r["_id"], r.get("text") or "", img))
    return items


def chunk_items(chunks_jsonl):
    """[(doc_id, embedding_text)] from an `rkdump` dump (the engine's own chunks)."""
    out = {}
    for line in open(chunks_jsonl, encoding="utf-8"):
        c = json.loads(line)
        text = f"{c.get('title') or ''}\n\n{c['content']}".strip()
        doc = c["doc"]
        # One vector per document: the engine's best-scoring chunk represents it.
        if doc not in out or len(text) > len(out[doc]):
            out[doc] = text
    return [(doc, text) for doc, text in out.items()]


# ─── Scoring ───────────────────────────────────────────────────────────────


def cosine_map(queries, query_vec, item_vec, depth=100):
    """{qid: {item id: cosine}} over the item set."""
    ids = sorted(item_vec)
    M = np.array([np.asarray(item_vec[i], dtype=np.float32) for i in ids])
    M /= np.linalg.norm(M, axis=1, keepdims=True) + 1e-12
    out = {}
    for q in queries:
        qv = np.asarray(query_vec[q["_id"]], dtype=np.float32)
        qv /= np.linalg.norm(qv) + 1e-12
        s = M @ qv
        top = np.argsort(-s)[:depth]
        out[q["_id"]] = {ids[i]: float(s[i]) for i in top}
    return out


def rank_from_scores(scores, depth=100):
    """{qid: {id: score}} -> {qid: [ids]}"""
    return {qid: [d for d, _ in sorted(v.items(), key=lambda kv: -kv[1])][:depth]
            for qid, v in scores.items()}


def one_space_runs(per_label_scores):
    """One collection: a page is fetched by whichever of its vectors is closest."""
    merged = defaultdict(dict)
    for scores in per_label_scores.values():
        for qid, per_item in scores.items():
            for pid, v in per_item.items():
                if v > merged[qid].get(pid, -1e9):
                    merged[qid][pid] = v
    return rank_from_scores(merged)


def score(run, qrels, k=10):
    nd, mrr, hit1, rec = [], [], [], []
    for qid, rel in qrels.items():
        docs = run.get(qid, [])
        top = docs[:k]
        dcg = sum(1 / math.log2(i + 2) for i, d in enumerate(top) if d in rel)
        idcg = sum(1 / math.log2(i + 2) for i in range(min(len(rel), k)))
        nd.append(dcg / idcg if idcg else 0.0)
        mrr.append(next((1 / (i + 1) for i, d in enumerate(docs) if d in rel), 0.0))
        hit1.append(1.0 if docs and docs[0] in rel else 0.0)
        rec.append(len([d for d in top if d in rel]) / max(len(rel), 1))
    return {m: np.array(v, dtype=np.float64) for m, v in
            (("nDCG@10", nd), ("MRR@10", mrr), ("Hit@1", hit1), ("Recall@10", rec))}


def paired(a, b, n=10000, seed=42):
    rng = np.random.default_rng(seed)
    d = a - b
    idx = rng.integers(0, len(d), size=(n, len(d)))
    boots = d[idx].mean(axis=1)
    return float(d.mean()), float(np.percentile(boots, 2.5)), float(np.percentile(boots, 97.5))


def report(name, m, nqueries):
    eprint(f"  {name:24s} n={nqueries:3d} nDCG@10 {m['nDCG@10'].mean():.3f}  "
           f"MRR@10 {m['MRR@10'].mean():.3f}  Hit@1 {m['Hit@1'].mean():.3f}  "
           f"Recall@10 {m['Recall@10'].mean():.3f}")


# ─── Self-test: a stub in both shapes, no GPU, no network ──────────────────


def self_test():
    import http.server
    import threading

    seen = []

    class Stub(http.server.BaseHTTPRequestHandler):
        def do_POST(self):  # noqa: N802
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            seen.append(body)
            if "messages" in body:
                parts = body["messages"][0]["content"]
                text = next((p.get("text", "") for p in parts if p.get("type") == "text"), "")
                image = any(p.get("type") == "image_url" for p in parts)
                seed = text + ("|img" if image else "")
                vecs = [[float(len(seed) % 7), float(image), 1.0]]
            else:
                vecs = [[float(len(t) % 7), 0.0, 1.0] for t in body["input"]]
            data = [{"index": i, "embedding": v} for i, v in enumerate(vecs)]
            payload = json.dumps({"data": data}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def log_message(self, *a):
            pass

    srv = http.server.HTTPServer(("127.0.0.1", 0), Stub)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{srv.server_address[1]}/v1/embeddings"
    cache = "/tmp/opencode/embed-ab-selftest"
    os.makedirs(cache, exist_ok=True)
    for f in os.listdir(cache):
        os.remove(os.path.join(cache, f))

    items = [("a#p1", "alpha"), ("b#p1", "beta"), ("c#p1", "gamma")]
    chat = Embedder.from_spec(f"stub-chat={url}#chat", cache_root=cache, workers=2, batch=8)
    openai = Embedder.from_spec(f"stub-openai={url}#openai", cache_root=cache, workers=2, batch=8)
    v1 = chat.embed([(k, p.encode()) for k, p in items], "text")
    assert set(v1) == {k for k, _ in items}, v1
    n_after_first = len(seen)
    v2 = chat.embed([(k, p.encode()) for k, p in items], "text")
    assert len(seen) == n_after_first, "cache miss on the second pass"
    assert v1 == v2
    v3 = openai.embed([(k, p.encode()) for k, p in items], "text")
    assert len(v3) == len(items)
    assert len(seen) == n_after_first + 1, "openai batch should be one request"
    img = b"img\0\xff\xd8\xff\xe0fakejpeg"
    v4 = chat.embed([("a#p1", img)], "image")
    assert len(v4) == 1
    qrels = defaultdict(dict)
    run = {"q1": ["a#p1", "b#p1"], "q2": ["c#p1", "a#p1"]}
    qrels["q1"]["a#p1"] = 1
    qrels["q2"]["c#p1"] = 1
    m = score(run, qrels)
    assert abs(m["nDCG@10"].mean() - 1.0) < 1e-9, m["nDCG@10"]
    d, lo, hi = paired(np.array([1.0, 0.0]), np.array([0.0, 0.0]))
    assert abs(d - 0.5) < 1e-9 and lo <= d <= hi
    srv.shutdown()
    print("self-test ok: chat+openai shapes, cache reuse, image payload, scoring, paired CI")
    return 0


# ─── Main ──────────────────────────────────────────────────────────────────


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--dataset", help="dir with queries.jsonl and qrels/")
    ap.add_argument("--pages", help="pages.jsonl (pre-rendered pages with text)")
    ap.add_argument("--chunks", help="rkdump chunks.jsonl (the engine's own chunks)")
    ap.add_argument("--mode", choices=("text", "image", "both"), default="text")
    ap.add_argument("--model", action="append", default=[], help="name=url#openai|chat")
    ap.add_argument("--out", default="")
    ap.add_argument("--cache", default=os.path.expanduser("~/.cache/kb-eval/embed-ab"))
    ap.add_argument("--batch", type=int, default=32)
    ap.add_argument("--workers", type=int, default=8)
    ap.add_argument("--api-key", default=os.environ.get("KB_EMBEDDING_API_KEY", ""))
    ap.add_argument("--self-test", action="store_true")
    a = ap.parse_args()

    if a.self_test:
        return self_test()
    if not a.dataset or not a.model:
        raise SystemExit("--dataset and at least one --model are required")
    if a.mode in ("text", "both") and not (a.pages or a.chunks):
        raise SystemExit("--mode text needs --pages or --chunks")
    if a.mode in ("image", "both") and not a.pages:
        raise SystemExit("--mode image/both needs --pages")
    out = a.out or os.path.join("/tmp", "embed-ab")
    os.makedirs(out, exist_ok=True)
    os.makedirs(a.cache, exist_ok=True)

    qrels = read_qrels(a.dataset)
    queries = [q for q in read_queries(a.dataset) if q["_id"] in qrels]
    eprint(f"dataset {os.path.basename(a.dataset.rstrip('/'))}: {len(queries)} queries, "
           f"{sum(len(v) for v in qrels.values())} qrels")

    if a.pages:
        pages = page_items(a.dataset, a.pages)
        text_items = [(pid, t.encode()) for pid, t, _ in pages if t]
        image_items = [(pid, b"img\0" + img) for pid, _, img in pages if img]
        eprint(f"pages: {len(pages)} ({len(text_items)} with text, {len(image_items)} with image)")
    else:
        chunks = chunk_items(a.chunks)
        text_items = [(doc, t.encode()) for doc, t in chunks]
        image_items = []
        eprint(f"documents: {len(chunks)}")

    summary = {"dataset": os.path.basename(a.dataset.rstrip("/")), "mode": a.mode,
               "queries": len(queries), "models": []}
    results = {}
    for spec in a.model:
        emb = Embedder.from_spec(spec, cache_root=a.cache, workers=a.workers,
                                 batch=a.batch, api_key=a.api_key)
        eprint(f"model {emb.name} ({emb.kind}) at {emb.url}")
        qvec = emb.embed([(q["_id"], q["text"].encode()) for q in queries], "query")
        vecs = {}
        if a.mode in ("text", "both"):
            vecs["text"] = emb.embed(text_items, "text")
        if a.mode in ("image", "both"):
            vecs["image"] = emb.embed(image_items, "image")
        per_label = {label: cosine_map(queries, qvec, v) for label, v in vecs.items()}
        runs = {label: rank_from_scores(scores) for label, scores in per_label.items()}
        if len(per_label) > 1:
            runs["one-space"] = one_space_runs(per_label)
        entry = {"model": emb.name, "dim": emb.dim(), "runs": {}}
        for label, run in runs.items():
            path = os.path.join(out, f"{emb.name.replace('/', '_')}.{label}.trec")
            with open(path, "w", encoding="utf-8") as f:
                for qid, ids in run.items():
                    for r, pid in enumerate(ids, 1):
                        f.write(f"{qid} Q0 {pid} {r} {1.0 / r:.6f} {emb.name}-{label}\n")
            m = score(run, qrels)
            report(f"{emb.name}/{label}", m, len(queries))
            entry["runs"][label] = {"metrics": {k: float(v.mean()) for k, v in m.items()},
                                    "trec": path}
            results[f"{emb.name}/{label}"] = m
        summary["models"].append(entry)

    names = list(results)
    for i in range(len(names)):
        for j in range(i + 1, len(names)):
            d, lo, hi = paired(results[names[i]]["nDCG@10"], results[names[j]]["nDCG@10"])
            eprint(f"  Δ nDCG@10 {names[i]} − {names[j]}: {d:+.3f} [{lo:+.3f}, {hi:+.3f}]")
            summary.setdefault("deltas", []).append(
                {"a": names[i], "b": names[j], "dnDCG10": d, "lo": lo, "hi": hi})
    with open(os.path.join(out, "summary.json"), "w", encoding="utf-8") as f:
        json.dump(summary, f, indent=2)
    eprint(f"wrote {out}/summary.json")
    return 0


if __name__ == "__main__":
    sys.exit(main())
