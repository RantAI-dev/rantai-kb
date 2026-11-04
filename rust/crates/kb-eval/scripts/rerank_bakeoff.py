"""Reranker bake-off: does a reranker lift "the right passage first"?

Takes the candidate list of the best measured retrieval config (Qdrant's own
RRF, the engine's embedding text, 50 candidates per arm — dumped by
`kf_vplane_ablate.py` as `candidates.jsonl`), reranks it with several
rerankers, and scores the result against the same qrels the retrieval runs
use.

What it measures, per reranker:
- nDCG@10, MRR@10, Hit@1, Recall@10 at the **document** level, the level the
  qrels are written at;
- the paired bootstrap delta against **no rerank** on the *same* candidate
  pool, so the delta is the reranker's doing and nothing else;
- the per-query latency of the rerank call itself.

The baseline is the candidate list in fused order, deduplicated to documents
exactly as the engine's own run file is: a document is judged by its best
chunk, and a document's second chunk does not give it a second place in the
ranking.

**Spend.** OpenRouter's rerank router prices per token for most models and
per search unit for Cohere's. Every candidate's cost is estimated from a
probe before the full run and printed; the run stops rather than exceeding
the budget. Every response is cached on disk, keyed by model + query + the
hash of the documents, so a re-run costs nothing and a changed candidate set
is re-reranked rather than silently reused.

    python rerank_bakeoff.py --candidates candidates.jsonl --dataset DIR \
        --out DIR --model openrouter:qwen/qwen3-reranker-8b --limit 10
"""

from __future__ import annotations

import argparse
import asyncio
import hashlib
import json
import math
import os
import statistics
import sys
import time
from collections import defaultdict
from pathlib import Path

import httpx

# Tokens per character, for the cost estimate. Deliberately conservative
# (Indonesian averages ~3.5 chars/token on e5's tokenizer, ~2.5 on a byte
# BPE): the estimate must be an upper bound, or it is not an estimate.
CHARS_PER_TOKEN = 2.5


# ---------- metrics ----------


def metrics(run: dict[str, list[str]], qrels: dict[str, set[str]], k: int = 10):
    """Per-query nDCG@k, MRR@k, Hit@1, Recall@k, the way the eval scores them.

    Binary gain (a relevant document contributes 1/log2(rank+1)), the same
    arithmetic `kb-eval score` and `ranx` use for the "linear" gain.
    """
    nd, mrr, hit1, rec = [], [], [], []
    for qid, rel in qrels.items():
        docs = run.get(qid, [])[:k]
        dcg = sum(1 / math.log2(i + 2) for i, d in enumerate(docs) if d in rel)
        idcg = sum(1 / math.log2(i + 2) for i in range(min(len(rel), k)))
        nd.append(dcg / idcg if idcg else 0.0)
        rr = next((1 / (i + 1) for i, d in enumerate(docs) if d in rel), 0.0)
        mrr.append(rr)
        hit1.append(1.0 if docs and docs[0] in rel else 0.0)
        rec.append(len(set(docs) & rel) / len(rel) if rel else 0.0)
    return {
        "ndcg": nd,
        "mrr": mrr,
        "hit1": hit1,
        "recall": rec,
    }


def paired_ci(a: list[float], b: list[float], n: int = 10000, seed: int = 42):
    """Paired bootstrap over queries: mean(a - b) and its 95% CI."""
    import numpy as np

    rng = np.random.default_rng(seed)
    d = np.asarray(a, dtype=float) - np.asarray(b, dtype=float)
    idx = rng.integers(0, len(d), size=(n, len(d)))
    boots = d[idx].mean(axis=1)
    return float(d.mean()), float(np.percentile(boots, 2.5)), float(np.percentile(boots, 97.5))


# ---------- the disk cache ----------


class RerankCache:
    """Every rerank response, on disk, keyed by model + query + documents.

    The key includes the hash of the documents themselves, so a different
    candidate list is a different entry: the cache never turns "the corpus
    changed" into "the old answer still looks right".
    """

    def __init__(self, path: Path) -> None:
        self.path = path
        self.mem: dict[str, dict] = {}
        if path.exists():
            for line in path.open(encoding="utf-8"):
                row = json.loads(line)
                self.mem[row["k"]] = row

    @staticmethod
    def key(model: str, qid: str, query: str, documents: list[str]) -> str:
        h = hashlib.sha256()
        h.update(model.encode())
        h.update(b"\0")
        h.update(qid.encode())
        h.update(b"\0")
        h.update(query.encode())
        h.update(b"\0")
        for d in documents:
            h.update(d.encode())
            h.update(b"\0")
        return h.hexdigest()

    def get(self, key: str) -> dict | None:
        return self.mem.get(key)

    def put(self, key: str, model: str, qid: str, order: list[int], scores: list[float], ms: float, cost: float):
        row = {"k": key, "model": model, "qid": qid, "order": order, "scores": scores, "ms": ms, "cost": cost}
        self.mem[key] = row
        with self.path.open("a", encoding="utf-8") as fh:
            fh.write(json.dumps(row) + "\n")
            fh.flush()


# ---------- reranker backends ----------


class RateLimiter:
    """Spaces the calls out. OpenRouter's free tier allows 20 requests a
    minute per key, and a 429 costs the whole run's time, so the free models
    are called one every 3.5 s."""

    def __init__(self, interval: float) -> None:
        self.interval = interval
        self.lock = asyncio.Lock()
        self.next_at = 0.0

    async def wait(self) -> None:
        if self.interval <= 0:
            return
        async with self.lock:
            now = time.monotonic()
            delay = self.next_at - now
            if delay > 0:
                await asyncio.sleep(delay)
            self.next_at = time.monotonic() + self.interval


async def rerank_openrouter(client: httpx.AsyncClient, model: str, query: str, documents: list[str]):
    """POST https://openrouter.ai/api/v1/rerank — the endpoint OpenRouter
    documents at /docs/client-sdks/python/sdks/rerank.

    A 429 is waited out, not given up on: the free tier is 20 requests a
    minute, and a run that dies on the 21st has measured nothing.
    """
    body = {"model": model, "query": query, "documents": documents}
    last = ""
    for attempt in range(8):
        resp = await client.post(
            "https://openrouter.ai/api/v1/rerank",
            headers={"Authorization": f"Bearer {os.environ['OPENROUTER_API_KEY']}"},
            json=body,
        )
        if resp.status_code == 200:
            out = resp.json()
            order = [r["index"] for r in out["results"]]
            scores = [r["relevance_score"] for r in out["results"]]
            usage = out.get("usage") or {}
            return order, scores, float(usage.get("cost") or 0.0), usage.get("total_tokens")
        last = f"{resp.status_code} {resp.text[:200]}"
        if resp.status_code in (429, 500, 502, 503, 529):
            reset = resp.headers.get("x-ratelimit-reset")
            delay = 15.0
            if reset and reset.isdigit():
                delay = max(5.0, min(120.0, int(reset) / 1000 - time.time()))
            print(f"  {model}: {resp.status_code}, waiting {delay:.0f}s", file=sys.stderr)
            await asyncio.sleep(delay)
            continue
        break
    raise RuntimeError(f"openrouter rerank failed: {last}")


async def rerank_tei(client: httpx.AsyncClient, model: str, query: str, documents: list[str], batch: int = 16):
    """POST {base}/rerank — Text Embeddings Inference's rerank endpoint.

    TEI caps a request's batch size (16 with the flags `stack-up.sh` uses), so
    a 30-candidate list goes out in two requests and the scores are merged.
    """
    results: list[dict] = []
    for start in range(0, len(documents), batch):
        part = documents[start : start + batch]
        resp = await client.post(model, json={"query": query, "texts": part})
        if resp.status_code != 200:
            raise RuntimeError(f"tei rerank failed: {resp.status_code} {resp.text[:200]}")
        body = resp.json()
        rows = body if isinstance(body, list) else (body.get("rankings") or body.get("results") or [])
        for r in rows:
            results.append({"index": int(r["index"]) + start, "score": float(r.get("score", 0.0))})
    results.sort(key=lambda r: r["score"], reverse=True)
    order = [r["index"] for r in results]
    scores = [r["score"] for r in results]
    return order, scores, 0.0, None


async def rerank_llama(client: httpx.AsyncClient, base: str, query: str, documents: list[str]):
    """POST {base}/v1/rerank: llama.cpp's server (`--reranking`), e.g. the LAN
    RX 590. The key is LLAMA_GPU_API_KEY. One request per query, every
    candidate in it, as the engine would send them.
    """
    resp = await client.post(
        base.rstrip("/") + "/v1/rerank",
        headers={"Authorization": f"Bearer {os.environ.get('LLAMA_GPU_API_KEY', '')}"},
        json={"model": "x", "query": query, "documents": documents},
    )
    if resp.status_code != 200:
        raise RuntimeError(f"llama rerank failed: {resp.status_code} {resp.text[:200]}")
    rows = sorted(resp.json()["results"], key=lambda r: r["relevance_score"], reverse=True)
    return [r["index"] for r in rows], [r["relevance_score"] for r in rows], 0.0, None


# ---------- the run ----------


async def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--candidates", type=Path, required=True, help="candidates.jsonl from kf_vplane_ablate.py")
    ap.add_argument("--dataset", type=Path, required=True, help="dataset dir (for queries.jsonl and qrels/test.tsv)")
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--model", action="append", required=True,
                    help="backend:target, e.g. openrouter:qwen/qwen3-reranker-8b, tei:http://127.0.0.1:18082/rerank "
                         "or llama:http://192.168.18.92:8082")
    ap.add_argument("--name", action="append", default=[], help="a name for the model (defaults to the target)")
    ap.add_argument("--limit", type=int, default=0, help="only the first N queries (a probe)")
    ap.add_argument("--depth", type=int, default=0,
                    help="rerank only the first N candidates; the rest keep their fused order "
                         "after them, as in the engine (0 = all)")
    ap.add_argument("--budget", type=float, default=0.50, help="hard cap on the paid spend of this run, in USD")
    ap.add_argument("--concurrency", type=int, default=4)
    ap.add_argument("--cache", type=Path, default=None, help="cache file (default <out>/rerank-cache.jsonl)")
    args = ap.parse_args()

    if not os.environ.get("OPENROUTER_API_KEY") and any(m.startswith("openrouter:") for m in args.model):
        print("OPENROUTER_API_KEY is not set", file=sys.stderr)
        return 1

    args.out.mkdir(parents=True, exist_ok=True)
    cache_path = args.cache or (args.out / "rerank-cache.jsonl")
    cache = RerankCache(cache_path)

    # --- inputs ---
    queries = {}
    for line in (args.dataset / "queries.jsonl").open(encoding="utf-8"):
        row = json.loads(line)
        queries[row["_id"]] = row["text"]
    qrels: dict[str, set[str]] = defaultdict(set)
    qrel_path = args.dataset / "qrels" / "test.tsv"
    for line in qrel_path.open(encoding="utf-8"):
        parts = line.split()
        if len(parts) >= 3 and parts[0] != "query-id":
            if float(parts[-1]) > 0:
                qrels[parts[0]].add(parts[-2])

    cands: dict[str, list[dict]] = {}
    for line in args.candidates.open(encoding="utf-8"):
        row = json.loads(line)
        cands[row["qid"]] = row["candidates"]
    qids = sorted(cands)
    if args.limit:
        qids = qids[: args.limit]
    print(f"queries: {len(qids)} (of {len(cands)}), candidates per query: "
          f"{min(len(cands[q]) for q in qids)}-{max(len(cands[q]) for q in qids)}", file=sys.stderr)

    # --- the baseline: the same pool, in fused order ---
    def doc_run(order_by_qid: dict[str, list[int]]) -> dict[str, list[str]]:
        out = {}
        for qid, order in order_by_qid.items():
            seen, docs = set(), []
            for i in order:
                d = cands[qid][i]["doc"]
                if d not in seen:
                    seen.add(d)
                    docs.append(d)
            out[qid] = docs
        return out

    base_order = {qid: list(range(len(cands[qid]))) for qid in qids}
    base_run = doc_run(base_order)
    # Only the queries this run answers are scored: a probe over 10 of 200
    # queries must not be averaged against 190 queries it never saw.
    qrels_eval = {q: qrels[q] for q in qids if q in qrels}
    base_metrics = metrics(base_run, qrels_eval)
    print(f"baseline (no rerank): nDCG@10 {statistics.mean(base_metrics['ndcg']):.3f} "
          f"MRR@10 {statistics.mean(base_metrics['mrr']):.3f} "
          f"Hit@1 {statistics.mean(base_metrics['hit1']):.3f} "
          f"Recall@10 {statistics.mean(base_metrics['recall']):.3f}", file=sys.stderr)

    results = {
        "candidates_file": str(args.candidates),
        "n_queries": len(qids),
        "limit": args.limit or None,
        "baseline": {
            "ndcg": statistics.mean(base_metrics["ndcg"]),
            "mrr": statistics.mean(base_metrics["mrr"]),
            "hit1": statistics.mean(base_metrics["hit1"]),
            "recall": statistics.mean(base_metrics["recall"]),
        },
        "models": [],
    }

    names = args.name + [None] * (len(args.model) - len(args.name))
    spent = 0.0

    for spec, name in zip(args.model, names, strict=False):
        backend, _, target = spec.partition(":")
        label = name or target
        if backend == "openrouter":
            # The free tier is 20 requests a minute per key.
            limiter = RateLimiter(3.5 if target.endswith(":free") else 0.0)

            async def call(client, query, documents, target=target, limiter=limiter):
                await limiter.wait()
                return await rerank_openrouter(client, target, query, documents)
        elif backend == "tei":
            async def call(client, query, documents, target=target):
                return await rerank_tei(client, target, query, documents)
        elif backend == "llama":
            async def call(client, query, documents, target=target):
                return await rerank_llama(client, target, query, documents)
        elif backend == "cache":
            # A model that was run by another script and left in the cache
            # (the local Qwen3 reranker, which TEI cannot serve). It is never
            # called: every entry is already in the cache, and a miss here is
            # an error rather than a silent zero.
            async def call(client, query, documents, target=target):
                raise RuntimeError(
                    f"{target} is not in the cache; run the model first "
                    f"(local_qwen3_rerank.py)"
                )
        else:
            print(f"unknown backend: {backend}", file=sys.stderr)
            return 1

        # The documents the reranker sees: the chunk's own text, full, not
        # truncated — the same text the engine hands its own reranker.
        depth = args.depth or None
        documents_by_qid = {qid: [c["text"] for c in cands[qid][:depth]] for qid in qids}
        total_chars = sum(len(t) for docs in documents_by_qid.values() for t in docs)
        est_tokens = total_chars / CHARS_PER_TOKEN
        print(f"\n=== {label} ({backend}:{target}) ===", file=sys.stderr)
        print(f"documents: {sum(len(d) for d in documents_by_qid.values())}, "
              f"{total_chars} chars, ~{est_tokens:.0f} tokens", file=sys.stderr)

        # --- the run, with every response cached ---
        orders: dict[str, list[int]] = {}
        latencies: list[float] = []
        spend = 0.0
        tokens_seen = 0
        sem = asyncio.Semaphore(args.concurrency)

        # The price of this model, measured before the run: the first query is
        # reranked on its own, and its cost per character projects the rest.
        # A model whose projection does not fit the remaining budget is not
        # run at all — the spend rule is enforced here, not by good intentions.
        avg_chars = total_chars / max(1, len(qids))
        probe_qid = qids[0]
        probe_key = RerankCache.key(target, probe_qid, queries[probe_qid], documents_by_qid[probe_qid])
        probe_hit = cache.get(probe_key)
        if probe_hit is not None:
            probe_cost = probe_hit.get("cost", 0.0)
            probe_chars = sum(len(t) for t in documents_by_qid[probe_qid])
        else:
            async with httpx.AsyncClient(timeout=300) as probe_client:
                t0 = time.perf_counter()
                p_order, p_scores, probe_cost, p_tokens = await call(
                    probe_client, queries[probe_qid], documents_by_qid[probe_qid]
                )
                probe_ms = (time.perf_counter() - t0) * 1000
            probe_chars = sum(len(t) for t in documents_by_qid[probe_qid])
            tokens_seen += int(p_tokens or 0)
            spend += probe_cost
            cache.put(probe_key, target, probe_qid, p_order, p_scores, probe_ms, probe_cost)
            orders[probe_qid] = p_order
            latencies.append(probe_ms)
        per_query = probe_cost / max(1, probe_chars)
        projected = per_query * avg_chars * len(qids)
        results.setdefault("estimates", []).append({
            "model": target,
            "probe_cost_usd": probe_cost,
            "probe_chars": probe_chars,
            "usd_per_1k_chars": per_query * 1000,
            "projected_full_run_usd": projected,
        })
        print(f"price probe: ${probe_cost:.6f} for {probe_chars} chars -> "
              f"${per_query * 1000:.6f} per 1k chars -> ${projected:.4f} for {len(qids)} queries",
              file=sys.stderr)
        if spent + projected > args.budget:
            print(f"SKIP {label}: the projection ${projected:.4f} plus the ${spent:.4f} already spent "
                  f"does not fit the ${args.budget:.2f} budget. Not run.", file=sys.stderr)
            results["skipped"] = results.get("skipped", []) + [{"model": target, "projected_usd": projected}]
            continue

        async with httpx.AsyncClient(timeout=300) as client:
            async def one(qid: str):
                nonlocal spend, tokens_seen
                query = queries[qid]
                docs = documents_by_qid[qid]
                key = RerankCache.key(target, qid, query, docs)
                hit = cache.get(key)
                if hit is not None:
                    orders[qid] = hit["order"]
                    latencies.append(hit["ms"])
                    spend += hit.get("cost", 0.0)
                    return
                async with sem:
                    t0 = time.perf_counter()
                    order, scores, cost, tokens = await call(client, query, docs)
                    ms = (time.perf_counter() - t0) * 1000
                tokens_seen += int(tokens or 0)
                spend += cost
                orders[qid] = order
                latencies.append(ms)
                cache.put(key, target, qid, order, scores, ms, cost)

            await asyncio.gather(*(one(q) for q in qids if q != probe_qid))

        spent += spend
        print(f"spend for this model: ${spend:.6f} over {len(qids)} queries "
              f"({tokens_seen} provider tokens); total so far ${spent:.6f}", file=sys.stderr)
        if spent > args.budget:
            print(f"STOP: the total spend ${spent:.4f} is over the ${args.budget} budget", file=sys.stderr)
            return 2

        # The candidates past --depth were not reranked: they follow, in fused order.
        for qid in orders:
            n = len(documents_by_qid[qid])
            orders[qid] = list(orders[qid]) + list(range(n, len(cands[qid])))
        run = doc_run(orders)
        m = metrics(run, qrels_eval)
        row = {
            "model": target,
            "backend": backend,
            "name": label,
            "n_queries": len(qids),
            "cost_usd": spend,
            "provider_tokens": tokens_seen,
            "latency_ms": {
                "mean": statistics.mean(latencies),
                "p50": statistics.median(latencies),
                "p95": sorted(latencies)[int(0.95 * (len(latencies) - 1))],
                "max": max(latencies),
            },
            "metrics": {},
            "delta_vs_no_rerank": {},
        }
        for metric in ("ndcg", "mrr", "hit1", "recall"):
            d, lo, hi = paired_ci(m[metric], base_metrics[metric])
            row["metrics"][metric] = statistics.mean(m[metric])
            row["delta_vs_no_rerank"][metric] = {"delta": d, "ci_low": lo, "ci_high": hi}
        results["models"].append(row)
        print(f"{label}: nDCG@10 {row['metrics']['ndcg']:.3f} ({row['delta_vs_no_rerank']['ndcg']['delta']:+.3f} "
              f"[{row['delta_vs_no_rerank']['ndcg']['ci_low']:+.3f}, {row['delta_vs_no_rerank']['ndcg']['ci_high']:+.3f}]) "
              f"MRR@10 {row['metrics']['mrr']:.3f} Hit@1 {row['metrics']['hit1']:.3f} "
              f"Recall@10 {row['metrics']['recall']:.3f} | {row['latency_ms']['p50']:.0f} ms p50",
              file=sys.stderr)

        # The run file, so another scorer can check it.
        tag = label.replace("/", "_").replace(":", "_").replace(" ", "_")
        with (args.out / f"run-{tag}.trec").open("w", encoding="utf-8") as fh:
            for qid, docs in run.items():
                for i, d in enumerate(docs[:50]):
                    fh.write(f"{qid} Q0 {d} {i + 1} {1 / (i + 1):.6f} {tag}\n")

    total = sum(r["cost_usd"] for r in results["models"])
    results["total_cost_usd"] = total
    results["budget_usd"] = args.budget
    (args.out / "bakeoff.json").write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
    print(f"\ntotal spend: ${total:.6f} (budget ${args.budget:.2f})", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
