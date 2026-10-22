"""Why does rantai-kb rank below moco-kf's V-plane? One factor at a time.

Rebuilds both engines' rankings offline, from the same vectors, so each design
difference can be switched on alone:

- unit: the whole MIRACL passage (KF) or rantai-kb's chunks (800/200 smart chunker);
- dense text: what precedes the chunk in the embedding input. rantai-kb embeds
  "{title}\\n\\n{chunk}", and the eval uploaded no title, so the title was the file
  stem (a MIRACL docid such as "1000580_1");
- sparse encoder: fastembed Qdrant/bm25 (KF) or kb_core::sparse (rantai-kb);
  both scored with Qdrant's IDF modifier;
- pool: candidates per arm (KF 50, rantai-kb's KB_RERANK_INITIAL_K default 20);
- fusion: Qdrant's server RRF (k=2, 0-based ranks) or rantai-kb's re-sort of
  Qdrant's fused list with k=60.

The two reproduction rows must match the measured runs (0.813 and 0.796) before
any ablation row is trusted.

Inputs:
  --chunks  JSONL of rantai-kb's own chunks and sparse vectors, produced by calling
            kb_core::chunk::smart_chunk_document and kb_core::sparse (not a
            re-implementation). "tsi/tsv" is the chunk with its article title
            prepended (chunks after the first lose the "# Title" line).
  --kf-cache  the KF arm's embedding cache (whole passages, "passage: " prefix,
              Qdrant/bm25 sparse).
Dense vectors for chunks and queries come from OpenRouter (multilingual-e5-large),
cached on disk by text hash, so no text is paid for twice.
"""

from __future__ import annotations

import argparse
import asyncio
import hashlib
import json
import math
import os
import sys
from collections import defaultdict
from pathlib import Path

import httpx
import numpy as np

MODEL = "intfloat/multilingual-e5-large"


# ---------- embeddings (OpenRouter, disk cache keyed by model + text hash) ----------

class DenseCache:
    def __init__(self, path: Path, key: str) -> None:
        self.path, self.key = path, key
        self.mem: dict[str, list[float]] = {}
        if path.exists():
            for line in path.open(encoding="utf-8"):
                row = json.loads(line)
                self.mem[row["h"]] = row["d"]

    @staticmethod
    def h(text: str) -> str:
        return hashlib.sha256((MODEL + "\0" + text).encode()).hexdigest()

    async def fill(self, texts: list[str], batch: int = 64, conc: int = 4) -> None:
        todo = sorted({t for t in texts if self.h(t) not in self.mem})
        if not todo:
            return
        print(f"embedding {len(todo)} new texts via OpenRouter", file=sys.stderr)
        sem = asyncio.Semaphore(conc)
        out = self.path.open("a", encoding="utf-8")
        async with httpx.AsyncClient(timeout=120) as client:

            async def one(part: list[str]) -> None:
                async with sem:
                    err = ""
                    for attempt in range(6):
                        try:
                            r = await client.post(
                                "https://openrouter.ai/api/v1/embeddings",
                                headers={"Authorization": f"Bearer {self.key}"},
                                json={"model": MODEL, "input": part},
                            )
                            if r.status_code == 200:
                                data = sorted(r.json()["data"], key=lambda d: d["index"])
                                if len(data) == len(part):
                                    for t, d in zip(part, data, strict=True):
                                        self.mem[self.h(t)] = d["embedding"]
                                        out.write(json.dumps({"h": self.h(t), "d": d["embedding"]}) + "\n")
                                    out.flush()
                                    return
                            err = f"{r.status_code} {r.text[:200]}"
                        except (httpx.HTTPError, KeyError, ValueError) as exc:
                            err = repr(exc)
                        await asyncio.sleep(2**attempt)
                    raise RuntimeError(f"OpenRouter failed: {err}")

            await asyncio.gather(*(one(todo[i : i + batch]) for i in range(0, len(todo), batch)))
        out.close()

    def matrix(self, texts: list[str]) -> np.ndarray:
        m = np.array([self.mem[self.h(t)] for t in texts], dtype=np.float32)
        return m / np.linalg.norm(m, axis=1, keepdims=True)


# ---------- sparse scoring with Qdrant's IDF modifier ----------

class SparseIndex:
    """Dot product of query and document values, each term weighted by Qdrant's
    IDF: ln(1 + (N - n + 0.5) / (n + 0.5)), N = points, n = points with the term."""

    def __init__(self, docs: list[tuple[list[int], list[float]]]) -> None:
        self.n = len(docs)
        self.post: dict[int, list[tuple[int, float]]] = defaultdict(list)
        for di, (idx, val) in enumerate(docs):
            for t, v in zip(idx, val, strict=True):
                self.post[t].append((di, v))
        self.idf = {
            t: math.log(1 + (self.n - len(p) + 0.5) / (len(p) + 0.5)) for t, p in self.post.items()
        }

    def scores(self, q_idx: list[int], q_val: list[float]) -> np.ndarray:
        s = np.zeros(self.n, dtype=np.float64)
        for t, qv in zip(q_idx, q_val, strict=True):
            if t in self.post:
                w = qv * self.idf[t]
                for di, dv in self.post[t]:
                    s[di] += w * dv
        return s


def top(scores: np.ndarray, p: int, require_positive: bool) -> list[int]:
    order = np.argsort(-scores, kind="stable")[:p]
    return [int(i) for i in order if not require_positive or scores[i] > 0]


def fuse(
    dense: list[int],
    sparse: list[int],
    p: int,
    mode: str,
    tie: str = "order",
    tiekey=None,
) -> list[int]:
    """Qdrant RRF (k=2, 0-based) over the two prefetches, limited to p; with
    mode "rk60" the fused list is then re-sorted by k=60 (1-based, f32), as
    kb-store-qdrant does.

    With two arms RRF ties are structural: ranks (0, 1) and (1, 0) sum to the
    same score. `tie` picks the tie-break: "order" keeps the dense-first
    insertion order, "key" sorts ties by `tiekey(u)` ascending (the live engine
    uses document UUID, then chunk index; "dense" passes minus the cosine).
    """
    q: dict[int, float] = defaultdict(float)
    for lst in (dense, sparse):
        for r, u in enumerate(lst):
            q[u] += 1.0 / (2 + r)

    def key(score):
        return (lambda u: (-score(u), tiekey(u))) if tie == "key" else (lambda u: -score(u))

    fused = sorted(q, key=key(lambda u: q[u]))[:p]
    if mode == "qdrant":
        return fused
    dr = {u: r for r, u in enumerate(dense)}
    sr = {u: r for r, u in enumerate(sparse)}

    def k60(u: int) -> float:
        acc = np.float32(0.0)
        for m in (dr, sr):
            if u in m:
                acc = np.float32(acc + np.float32(1.0) / np.float32(60 + m[u] + 1))
        return float(acc)

    return sorted(fused, key=key(k60))


# ---------- metrics ----------

def metrics(run: dict[str, list[str]], qrels: dict[str, set[str]], k: int = 10) -> dict[str, np.ndarray]:
    nd, mrr, hit1, rec = [], [], [], []
    for qid, rel in qrels.items():
        docs = run.get(qid, [])[:k]
        dcg = sum(1 / math.log2(i + 2) for i, d in enumerate(docs) if d in rel)
        idcg = sum(1 / math.log2(i + 2) for i in range(min(len(rel), k)))
        nd.append(dcg / idcg)
        rr = next((1 / (i + 1) for i, d in enumerate(docs) if d in rel), 0.0)
        mrr.append(rr)
        hit1.append(1.0 if docs and docs[0] in rel else 0.0)
        rec.append(len(set(docs) & rel) / len(rel))
    return {"ndcg": np.array(nd), "mrr": np.array(mrr), "hit1": np.array(hit1), "recall": np.array(rec)}


def paired_ci(a: np.ndarray, b: np.ndarray, n: int = 10000, seed: int = 42) -> tuple[float, float, float]:
    rng = np.random.default_rng(seed)
    d = a - b
    idx = rng.integers(0, len(d), size=(n, len(d)))
    boots = d[idx].mean(axis=1)
    return float(d.mean()), float(np.percentile(boots, 2.5)), float(np.percentile(boots, 97.5))


async def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dataset", type=Path, required=True)
    ap.add_argument("--chunks", type=Path, required=True)
    ap.add_argument("--kf-cache", type=Path, required=True)
    ap.add_argument("--cache", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument(
        "--ingest-map", type=Path, required=True,
        help="docid -> document UUID from the live run, for the live tie-break",
    )
    args = ap.parse_args()
    key = os.environ.get("OPENROUTER_API_KEY", "")
    if not key:
        print("OPENROUTER_API_KEY is not set", file=sys.stderr)
        return 1
    args.out.mkdir(parents=True, exist_ok=True)

    from fastembed import SparseTextEmbedding

    bm25 = SparseTextEmbedding("Qdrant/bm25")

    queries = [json.loads(l) for l in (args.dataset / "queries.jsonl").open(encoding="utf-8")]
    qrels: dict[str, set[str]] = defaultdict(set)
    for f in (args.dataset / "qrels").glob("*"):
        for line in f.open(encoding="utf-8"):
            parts = line.split()
            if len(parts) >= 3 and parts[0] != "query-id":
                qid, did, rel = parts[0], parts[-2], parts[-1]
                if float(rel) > 0:
                    qrels[qid].add(did)
    rk_q = {json.loads(l)["id"]: json.loads(l) for l in Path(f"{args.chunks}.queries").open(encoding="utf-8")}

    passages = {p.stem: p.read_text(encoding="utf-8") for p in sorted((args.dataset / "corpus").glob("*"))}
    titles = {}
    for stem, text in passages.items():
        first = text.lstrip().splitlines()[0] if text.strip() else ""
        titles[stem] = first.lstrip("#").strip() if first.startswith("#") else ""
    kf = {}
    for line in args.kf_cache.open(encoding="utf-8"):
        row = json.loads(line)
        kf[row["id"]] = row
    chunks = [json.loads(l) for l in args.chunks.open(encoding="utf-8")]

    dc = DenseCache(args.cache, key)
    q_texts = ["query: " + q["text"] for q in queries]
    dense_variants = {
        "stem": [f"passage: {c['doc']}\n\n{c['content']}" for c in chunks],
        "none": [f"passage: {c['content']}" for c in chunks],
        "title": [f"passage: {titles[c['doc']]}\n\n{c['content']}" for c in chunks],
    }
    whole_stem = [f"passage: {s}\n\n{t}" for s, t in passages.items()]
    await dc.fill(q_texts + [t for v in dense_variants.values() for t in v] + whole_stem)
    Q = dc.matrix(q_texts)

    # Units: whole passages or rantai-kb chunks, each with dense and sparse options.
    p_ids = list(passages)
    units = {
        "passage": {
            "doc": p_ids,
            "dense": {
                "none": (lambda m: m / np.linalg.norm(m, axis=1, keepdims=True))(
                    np.array([kf[i]["d"] for i in p_ids], dtype=np.float32)
                ),
                "stem": dc.matrix(whole_stem),
            },
            "sparse": {
                "bm25": SparseIndex([(kf[i]["si"], kf[i]["sv"]) for i in p_ids]),
                "rk": SparseIndex([(c["fsi"], c["fsv"]) for c in chunks if c["i"] == 0]),
            },
        },
        "chunk": {
            "doc": [c["doc"] for c in chunks],
            "dense": {k: dc.matrix(v) for k, v in dense_variants.items()},
            "sparse": {
                "rk": SparseIndex([(c["si"], c["sv"]) for c in chunks]),
                "bm25": SparseIndex(
                    [
                        ([int(i) for i in e.indices], [float(v) for v in e.values])
                        for e in bm25.embed([c["content"] for c in chunks])
                    ]
                ),
                "rk+title": SparseIndex([(c["tsi"], c["tsv"]) for c in chunks]),
            },
        },
    }
    # Guard: the passage-level rk sparse list must be in passage order.
    assert [c["doc"] for c in chunks if c["i"] == 0] == p_ids
    bm25_q = {
        q["_id"]: ([int(i) for i in e.indices], [float(v) for v in e.values])
        for q, e in zip(queries, bm25.query_embed([q["text"] for q in queries]), strict=True)
    }

    doc_uuid = json.loads(args.ingest_map.read_text(encoding="utf-8"))
    chunk_idx = {"passage": [0] * len(p_ids), "chunk": [c["i"] for c in chunks]}

    def run(
        unit: str, dense: str, sparse: str, pool: int, fusion: str, tie: str = "order"
    ) -> dict[str, list[str]]:
        u = units[unit]
        D = u["dense"][dense]
        S = u["sparse"][sparse]
        out: dict[str, list[str]] = {}
        for qi, q in enumerate(queries):
            qid = q["_id"]
            ds = D @ Q[qi]
            if sparse.startswith("rk"):
                qs = S.scores(rk_q[qid]["si"], rk_q[qid]["sv"])
            else:
                qs = S.scores(*bm25_q[qid])
            if tie == "uuid":
                tk = lambda ui: (doc_uuid[u["doc"][ui]], chunk_idx[unit][ui])  # noqa: E731
            elif tie == "dense":
                tk = lambda ui: -float(ds[ui])  # noqa: E731
            else:
                tk = None
            fused = fuse(
                top(ds, pool, False), top(qs, pool, True), pool, fusion,
                "key" if tk else "order", tk,
            )
            seen: list[str] = []
            for ui in fused:
                d = u["doc"][ui]
                if d not in seen:
                    seen.append(d)
            out[qid] = seen
        return out

    configs = [
        # name, unit, dense text, sparse, pool, fusion
        ("KF fixed (repro)", "passage", "none", "bm25", 50, "qdrant"),
        ("rantai-kb (repro)", "chunk", "stem", "rk", 20, "rk60"),
        ("rantai-kb (repro, live tie-break)", "chunk", "stem", "rk", 20, "rk60", "uuid"),
        ("rk: ties by dense score", "chunk", "stem", "rk", 20, "rk60", "dense"),
        ("rk: fusion k=2", "chunk", "stem", "rk", 20, "qdrant"),
        ("rk: fusion k=2, ties by dense", "chunk", "stem", "rk", 20, "qdrant", "dense"),
        ("KF: ties by dense", "passage", "none", "bm25", 50, "qdrant", "dense"),
        ("rk: pool 50", "chunk", "stem", "rk", 50, "rk60"),
        ("rk: no stem in dense", "chunk", "none", "rk", 20, "rk60"),
        ("rk: real title in dense", "chunk", "title", "rk", 20, "rk60"),
        ("rk: bm25 sparse", "chunk", "stem", "bm25", 20, "rk60"),
        ("rk: whole passages", "passage", "stem", "rk", 20, "rk60"),
        ("KF: rk sparse", "passage", "none", "rk", 50, "qdrant"),
        ("KF: stem in dense", "passage", "stem", "bm25", 50, "qdrant"),
        ("KF: pool 20", "passage", "none", "bm25", 20, "qdrant"),
        ("KF: k=60 re-sort", "passage", "none", "bm25", 50, "rk60"),
        ("KF: rk chunks", "chunk", "none", "bm25", 50, "qdrant"),
        ("rk: k=2 + real title", "chunk", "title", "rk", 20, "qdrant"),
        ("rk: k=2 + real title + pool 50", "chunk", "title", "rk", 50, "qdrant"),
        ("rk: k=2 + real title + pool 50, ties by dense", "chunk", "title", "rk", 50, "qdrant", "dense"),
        ("rk: k=2 + title (dense+sparse) + pool 50", "chunk", "title", "rk+title", 50, "qdrant"),
        ("rk: k=2 + pool 50 + whole passages, no stem", "passage", "none", "rk", 50, "qdrant"),
    ]
    results = {}
    for name, *cfg in configs:
        r = run(*cfg)
        results[name] = (metrics(r, qrels), r)
        tag = name.replace(" ", "_").replace(":", "").replace("(", "").replace(")", "").replace("+", "p").replace(",", "").replace("=", "")
        with (args.out / f"{tag}.trec").open("w", encoding="utf-8") as fh:
            for qid, docs in r.items():
                for i, d in enumerate(docs[:50]):
                    fh.write(f"{qid} Q0 {d} {i + 1} {1 / (i + 1):.6f} {tag}\n")

    base = results["KF fixed (repro)"][0]
    rkb = results["rantai-kb (repro, live tie-break)"][0]
    lines = ["| config | nDCG@10 | MRR@10 | Hit@1 | Recall@10 | ΔnDCG vs rk live-tie repro [95% CI] | ΔnDCG vs KF fixed [95% CI] |",
             "|---|---:|---:|---:|---:|---|---|"]
    for name, (m, _) in results.items():
        d1 = paired_ci(m["ndcg"], rkb["ndcg"])
        d2 = paired_ci(m["ndcg"], base["ndcg"])
        lines.append(
            f"| {name} | {m['ndcg'].mean():.3f} | {m['mrr'].mean():.3f} | {m['hit1'].mean():.3f} | "
            f"{m['recall'].mean():.3f} | {d1[0]:+.3f} [{d1[1]:+.3f}, {d1[2]:+.3f}] | "
            f"{d2[0]:+.3f} [{d2[1]:+.3f}, {d2[2]:+.3f}] |"
        )
    table = "\n".join(lines)
    (args.out / "ablation.md").write_text(table + "\n", encoding="utf-8")
    print(table)
    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
