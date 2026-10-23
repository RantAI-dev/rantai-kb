"""Which fusion of the dense and keyword arms puts the right passage first?

Replays rantai-kb's two retrieval arms offline, from the engine's own chunks and
sparse vectors (the `rkdump` helper: kb_core's chunker, kb_core::sparse,
kb_core::title::embedding_text) and dense vectors from a live embedder, then
scores fusion variants against the qrels.

Protocol, so the winner is not tuned on its own test:
- the queries are split once, by a fixed seed, into a tune half and a test half;
- variants are ranked on the tune half only;
- the report shows the tune winner, and the reference configs, on the test half,
  with paired bootstrap CIs against the live engine's config.

The two reproduction rows must match the live runs in docs/eval-ab/miracl-id-200
(old 0.800, new 0.782 nDCG@10 on all 200 queries) before any other row is
trusted.

Dense vectors are cached on disk keyed by model + text hash, so no text is
embedded twice. The embedder is any OpenAI-compatible /v1/embeddings server
(default: the LAN RX 590's llama.cpp, Qwen3-Embedding-0.6B).

    python fusion_tune.py --dataset DIR --chunks ~/rkdump/chunks.jsonl \
        --cache ~/.cache/kb-eval/dense.jsonl --out DIR
"""

import argparse
import asyncio
import hashlib
import itertools
import json
import math
import os
import random
import sys
from collections import defaultdict
from pathlib import Path

import httpx
import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
from kf_vplane_ablate import SparseIndex, metrics, paired_ci  # noqa: E402

QWEN3_QUERY = "Instruct: Given a question, retrieve passages that answer it\nQuery: "


class DenseCache:
    """Embeddings from an OpenAI-compatible server, cached by model + text hash."""

    def __init__(self, path: Path, url: str, key: str, model: str) -> None:
        self.path, self.url, self.key, self.model = path, url, key, model
        self.mem: dict[str, list[float]] = {}
        if path.exists():
            for line in path.open(encoding="utf-8"):
                row = json.loads(line)
                self.mem[row["h"]] = row["d"]

    def h(self, text: str) -> str:
        return hashlib.sha256((self.model + "\0" + text).encode()).hexdigest()

    async def fill(self, texts: list[str], batch: int = 32) -> None:
        todo = sorted({t for t in texts if self.h(t) not in self.mem})
        if not todo:
            return
        print(f"embedding {len(todo)} new texts via {self.url}", file=sys.stderr)
        self.path.parent.mkdir(parents=True, exist_ok=True)
        out = self.path.open("a", encoding="utf-8")
        async with httpx.AsyncClient(timeout=600) as client:
            for i in range(0, len(todo), batch):
                part = todo[i : i + batch]
                r = await client.post(
                    self.url,
                    headers={"Authorization": f"Bearer {self.key}"},
                    json={"model": self.model, "input": part},
                )
                r.raise_for_status()
                data = sorted(r.json()["data"], key=lambda d: d["index"])
                for t, d in zip(part, data, strict=True):
                    self.mem[self.h(t)] = d["embedding"]
                    out.write(json.dumps({"h": self.h(t), "d": d["embedding"]}) + "\n")
                out.flush()
                if (i // batch) % 20 == 0:
                    print(f"  {i + len(part)}/{len(todo)}", file=sys.stderr)
        out.close()

    def matrix(self, texts: list[str]) -> np.ndarray:
        m = np.array([self.mem[self.h(t)] for t in texts], dtype=np.float32)
        return m / np.linalg.norm(m, axis=1, keepdims=True)


def top(scores: np.ndarray, p: int, positive: bool) -> list[int]:
    order = np.argsort(-scores, kind="stable")[:p]
    return [int(i) for i in order if not positive or scores[i] > 0]


# ---------- fusion variants: each maps (dense list, sparse list, scores) -> fused chunk order ----------

def rrf(dense, sparse, k, wd=1.0, ws=1.0, one_based=False):
    off = 1 if one_based else 0
    q: dict[int, float] = defaultdict(float)
    for w, lst in ((wd, dense), (ws, sparse)):
        for r, u in enumerate(lst):
            q[u] += w / (k + r + off)
    return q


def qdrant_then_k60(dense, sparse, p):
    """What the engine did before f8c0639: Qdrant's k=2 list, limited to p, re-sorted by k=60."""
    q = rrf(dense, sparse, 2)
    fused = sorted(q, key=lambda u: -q[u])[:p]
    q60 = rrf(dense, sparse, 60, one_based=True)
    return sorted(fused, key=lambda u: -q60[u])


def norm(vals: dict[int, float], how: str) -> dict[int, float]:
    if not vals:
        return {}
    a = np.array(list(vals.values()), dtype=np.float64)
    if how == "minmax":
        lo, hi = a.min(), a.max()
        return {u: (v - lo) / (hi - lo) if hi > lo else 1.0 for u, v in vals.items()}
    if how == "z":
        mu, sd = a.mean(), a.std()
        return {u: (v - mu) / sd if sd > 0 else 0.0 for u, v in vals.items()}
    if how == "max":
        hi = a.max()
        return {u: v / hi if hi > 0 else 0.0 for u, v in vals.items()}
    raise ValueError(how)


def convex(dense, sparse, dsc, ssc, alpha, how):
    """alpha * norm(dense score) + (1 - alpha) * norm(sparse score), over each arm's own list.
    A chunk missing from an arm gets that arm's minimum (0 after min-max)."""
    dn = norm({u: dsc[u] for u in dense}, how)
    sn = norm({u: ssc[u] for u in sparse}, how)
    dmin = min(dn.values()) if dn else 0.0
    smin = min(sn.values()) if sn else 0.0
    out = {}
    for u in set(dense) | set(sparse):
        out[u] = alpha * dn.get(u, dmin) + (1 - alpha) * sn.get(u, smin)
    return out


def to_docs(order: list[int], chunk_doc: list[str]) -> list[str]:
    seen, docs = set(), []
    for u in order:
        d = chunk_doc[u]
        if d not in seen:
            seen.add(d)
            docs.append(d)
    return docs


async def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dataset", type=Path, required=True)
    ap.add_argument("--chunks", type=Path, required=True)
    ap.add_argument("--cache", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--embed-url", default="http://192.168.18.92:8081/v1/embeddings")
    ap.add_argument("--embed-key-env", default="LLAMA_GPU_API_KEY")
    ap.add_argument("--model", default="Qwen/Qwen3-Embedding-0.6B")
    ap.add_argument("--split-seed", type=int, default=42)
    args = ap.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)

    key = os.environ.get(args.embed_key_env, "")
    queries = [json.loads(l) for l in (args.dataset / "queries.jsonl").open(encoding="utf-8")]
    qrels: dict[str, set[str]] = defaultdict(set)
    for f in (args.dataset / "qrels").glob("*"):
        for line in f.open(encoding="utf-8"):
            parts = line.split()
            if len(parts) >= 3 and parts[0] != "query-id" and float(parts[-1]) > 0:
                qrels[parts[0]].add(parts[1])
    rk_q = {json.loads(l)["id"]: json.loads(l) for l in Path(f"{args.chunks}.queries").open(encoding="utf-8")}
    chunks = [json.loads(l) for l in args.chunks.open(encoding="utf-8")]
    chunk_doc = [c["doc"] for c in chunks]

    # The text each engine version embeds. "title": before 61a076a, the
    # document title and the chunk ("{title}\n\n{content}"). "emb": after it,
    # kb_core::title::embedding_text, straight from the dump.
    texts = {
        "title": [f"{c['title']}\n\n{c['content']}" for c in chunks],
        "emb": [c["emb"] for c in chunks],
    }
    q_texts = [QWEN3_QUERY + q["text"] for q in queries]
    dc = DenseCache(args.cache, args.embed_url, key, args.model)
    await dc.fill(q_texts + texts["emb"] + texts["title"])
    qm = dc.matrix(q_texts)
    cm = {v: dc.matrix(t) for v, t in texts.items()}

    sp = SparseIndex([(c["si"], c["sv"]) for c in chunks])

    ids = [q["_id"] for q in queries if q["_id"] in qrels]
    rng = random.Random(args.split_seed)
    shuffled = ids[:]
    rng.shuffle(shuffled)
    tune, test = set(shuffled[: len(ids) // 2]), set(shuffled[len(ids) // 2 :])
    (args.out / "split.json").write_text(json.dumps({"tune": sorted(tune), "test": sorted(test)}))

    # Per query, per dense text: both arms' full ranked lists (top 200) and scores.
    arms = {}
    for v in cm:
        dense_all = qm @ cm[v].T
        for qi, q in enumerate(queries):
            if q["_id"] not in qrels:
                continue
            ds = dense_all[qi]
            sq = rk_q[q["_id"]]
            ss = sp.scores(sq["si"], sq["sv"])
            arms[(v, q["_id"])] = (top(ds, 200, False), top(ss, 200, True), ds, ss)

    def run(variant: str, fn, qids) -> dict[str, list[str]]:
        out = {}
        for qid in qids:
            d, s, ds, ss = arms[(variant, qid)]
            out[qid] = to_docs(fn(d, s, ds, ss), chunk_doc)[:10]
        return out

    def ordered(score: dict[int, float]) -> list[int]:
        return sorted(score, key=lambda u: -score[u])

    configs: dict[str, tuple[str, object]] = {}
    # Reproductions of the three live arms (pool 20 per arm, the engine's default).
    configs["live old: title, k=2 then k=60 re-sort, pool 20"] = (
        "title", lambda d, s, ds, ss: qdrant_then_k60(d[:20], s[:20], 20))
    configs["live fusion-only: title, k=2, pool 20"] = (
        "title", lambda d, s, ds, ss: ordered(rrf(d[:20], s[:20], 2))[:20])
    configs["live new: emb, k=2, pool 20"] = (
        "emb", lambda d, s, ds, ss: ordered(rrf(d[:20], s[:20], 2))[:20])
    for v in ("title", "emb"):
        configs[f"{v}: dense only"] = (v, lambda d, s, ds, ss: d[:50])
        configs[f"{v}: sparse only"] = (v, lambda d, s, ds, ss: s[:50])
        for pool, k, one, wd in itertools.product((20, 50, 100), (1, 2, 5, 10, 20, 40, 60, 100),
                                                    (False,), (0.5, 0.75, 1.0, 1.5, 2.0, 3.0)):
            configs[f"{v}: rrf k={k} wd={wd} pool {pool}"] = (
                v, lambda d, s, ds, ss, pool=pool, k=k, wd=wd: ordered(rrf(d[:pool], s[:pool], k, wd))[:pool])
        for pool, how, a in itertools.product((20, 50, 100), ("minmax", "z", "max"),
                                              [round(x * 0.05, 2) for x in range(21)]):
            configs[f"{v}: convex {how} a={a} pool {pool}"] = (
                v, lambda d, s, ds, ss, pool=pool, how=how, a=a:
                ordered(convex(d[:pool], s[:pool], ds, ss, a, how))[:pool])

    all_q = sorted(tune | test)
    ref_name = "live old: title, k=2 then k=60 re-sort, pool 20"
    rows = []
    for name, (v, fn) in configs.items():
        m_all = metrics(run(v, fn, all_q), {q: qrels[q] for q in all_q})
        m_tune = metrics(run(v, fn, sorted(tune)), {q: qrels[q] for q in sorted(tune)})
        rows.append((name, m_all, m_tune))
    rows_by = {r[0]: r for r in rows}

    def mean(m, key):
        return float(m[key].mean())

    lines = ["# Fusion tuning (offline replay)", "",
             f"dataset `{args.dataset.name}`, {len(all_q)} queries, tune {len(tune)} / test {len(test)} "
             f"(seed {args.split_seed}), embedder `{args.model}` at `{args.embed_url}`", "",
             "## Reproduction (all queries; must match the live runs)", "",
             "| config | nDCG@10 | MRR@10 | Hit@1 | Recall@10 |", "|---|---:|---:|---:|---:|"]
    for name in [n for n in configs if n.startswith("live")]:
        m = rows_by[name][1]
        lines.append(f"| {name} | {mean(m,'ndcg'):.3f} | {mean(m,'mrr'):.3f} | {mean(m,'hit1'):.3f} | {mean(m,'recall'):.3f} |")

    ranked = sorted(rows, key=lambda r: (-mean(r[2], "ndcg"), -mean(r[2], "mrr")))
    lines += ["", "## Top 15 on the tune half (nDCG@10)", "",
              "| config | tune nDCG@10 | tune MRR@10 |", "|---|---:|---:|"]
    for name, _, mt in ranked[:15]:
        lines.append(f"| {name} | {mean(mt,'ndcg'):.3f} | {mean(mt,'mrr'):.3f} |")

    test_q = sorted(test)
    tq = {q: qrels[q] for q in test_q}
    ref_v, ref_fn = configs[ref_name]
    ref_test = metrics(run(ref_v, ref_fn, test_q), tq)
    picks = [ranked[0][0], "live new: emb, k=2, pool 20", "emb: dense only", "title: dense only"]
    lines += ["", f"## Held-out test half ({len(test_q)} queries), paired against `{ref_name}`", "",
              "| config | nDCG@10 | Δ nDCG [95% CI] | MRR@10 | Δ MRR [95% CI] | Hit@1 |",
              "|---|---:|---|---:|---|---:|"]
    for name in [ref_name] + picks:
        v, fn = configs[name]
        mt = metrics(run(v, fn, test_q), tq)
        dn = paired_ci(mt["ndcg"], ref_test["ndcg"])
        dm = paired_ci(mt["mrr"], ref_test["mrr"])
        lines.append(f"| {name} | {mean(mt,'ndcg'):.3f} | {dn[0]:+.3f} [{dn[1]:+.3f}, {dn[2]:+.3f}] | "
                     f"{mean(mt,'mrr'):.3f} | {dm[0]:+.3f} [{dm[1]:+.3f}, {dm[2]:+.3f}] | {mean(mt,'hit1'):.3f} |")

    # The winner's fused candidate lists (chunk level, top 50), for the reranker stage.
    win_v, win_fn = configs[ranked[0][0]]
    with (args.out / "candidates.jsonl").open("w", encoding="utf-8") as f:
        for q in queries:
            if q["_id"] not in qrels:
                continue
            d, s, ds, ss = arms[(win_v, q["_id"])]
            order = win_fn(d, s, ds, ss)[:50]
            f.write(json.dumps({"qid": q["_id"], "query": q["text"], "config": ranked[0][0],
                                "candidates": [{"doc": chunk_doc[u], "i": chunks[u]["i"],
                                                "text": chunks[u]["content"], "title": chunks[u]["title"]}
                                               for u in order]}) + "\n")
    test_rows = {}
    for name, (v, fn) in configs.items():
        mt = metrics(run(v, fn, test_q), tq)
        test_rows[name] = {"test_ndcg": mean(mt, "ndcg"), "test_mrr": mean(mt, "mrr")}
    with (args.out / "rows.json").open("w", encoding="utf-8") as f:
        json.dump({name: {"all_ndcg": mean(m, "ndcg"), "all_mrr": mean(m, "mrr"),
                          "tune_ndcg": mean(mt_, "ndcg"), "tune_mrr": mean(mt_, "mrr"), **test_rows[name]}
                   for name, m, mt_ in rows}, f, indent=1)
    report = "\n".join(lines) + "\n"
    (args.out / "fusion.md").write_text(report)
    print(report)
    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
