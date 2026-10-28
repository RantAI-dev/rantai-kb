#!/usr/bin/env python3
"""Run moco-kf's V-plane, unchanged, over a kb-eval dataset and write a TREC run.

Part of docs/kf-vplane-eval.md. Run it with moco-kf's own interpreter so the code under
test is KF's, not a re-implementation:

    PYTHONPATH=/home/shiro/moco-kf/src /home/shiro/moco-kf/src/.venv/bin/python \
        kf_vplane_arm.py --dataset ../datasets/miracl-id-200 --qdrant http://127.0.0.1:16333 \
        --variant asis --out /tmp/kfv/asis

What is KF's and what is ours:
  * KF's: the models (multilingual-e5-large + Qdrant/bm25), build_point,
    upsert, hybrid_search, and the collection spec in infra/qdrant/collections.
  * Ours: the dense model is called through OpenRouter (same model, no local CPU),
    the loop, the payload values (one eval tenant, one dataset, status validated),
    and the two fixes the variants switch on.

Variants (docs/kf-vplane-eval.md, assumption 1):
  asis    KF as built.
  prefix  e5 "query: " / "passage: " prefixes added (fastembed adds none for e5).
  idf     the sparse vector gets Qdrant's IDF modifier (Qdrant/bm25 needs it for BM25).
  fixed   prefix + idf.

One unit per corpus file; unit id = file stem (the MIRACL docid), so the run scores
per passage, exactly like the qrels.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import statistics
import sys
import time
from pathlib import Path

import httpx

from moco_kf.services.retrieval import provisioning
from moco_kf.services.retrieval.vector_plane import COLLECTION, build_point, hybrid_search, upsert
from moco_kf.shared.embedding import DENSE_MODEL, SPARSE_MODEL, Embedded, SparseVector

TENANT = "TEN-EVAL"
DATASET = "DS-EVAL"


class OpenRouterKfEmbedder:
    """KF's embedder with the dense half served by OpenRouter instead of local fastembed.

    The dense model is KF's own (intfloat/multilingual-e5-large) unless --dense-model says
    otherwise. The sparse half is KF's local fastembed Qdrant/bm25, unchanged: it is a
    tokenizer + stemmer, not a neural model. ``prefix`` adds e5's "query: "/"passage: "
    to the dense input only; the sparse side always sees the raw text.
    """

    def __init__(self, *, model: str, api_key: str, prefix: bool, query_prefix: str | None = None) -> None:
        from fastembed import SparseTextEmbedding

        self._sparse = SparseTextEmbedding(SPARSE_MODEL)
        self._model = model
        self._key = api_key
        self._qp = query_prefix if query_prefix is not None else ("query: " if prefix else "")
        self._pp = "passage: " if prefix and query_prefix is None else ""
        self._http = httpx.AsyncClient(timeout=120)

    async def _dense(self, texts: list[str]) -> list[list[float]]:
        for attempt in range(6):
            try:
                r = await self._http.post(
                    "https://openrouter.ai/api/v1/embeddings",
                    headers={"Authorization": f"Bearer {self._key}"},
                    json={"model": self._model, "input": texts},
                )
                if r.status_code == 200:
                    data = sorted(r.json()["data"], key=lambda d: d["index"])
                    if len(data) == len(texts):
                        return [d["embedding"] for d in data]
                err = f"{r.status_code} {r.text[:200]}"
            except (httpx.HTTPError, KeyError, ValueError) as exc:
                err = repr(exc)
            await asyncio.sleep(2**attempt)
        raise RuntimeError(f"OpenRouter embeddings failed after retries: {err}")

    def _sparse_vecs(self, texts: list[str], query: bool) -> list[SparseVector]:
        it = self._sparse.query_embed(texts) if query else self._sparse.embed(texts)
        return [SparseVector(indices=[int(i) for i in x.indices], values=[float(v) for v in x.values]) for x in it]

    async def embed_documents(self, texts: list[str]) -> list[Embedded]:
        dense = await self._dense([self._pp + t for t in texts])
        sparse = await asyncio.to_thread(self._sparse_vecs, texts, False)
        return [Embedded(dense=d, sparse=sp) for d, sp in zip(dense, sparse, strict=True)]

    async def embed_query(self, text: str) -> Embedded:
        dense = await self._dense([self._qp + text])
        sparse = await asyncio.to_thread(self._sparse_vecs, [text], True)
        return Embedded(dense=dense[0], sparse=sparse[0])

    async def warm(self) -> None:
        await self.embed_query("pemanasan")


def collection_body(idf: bool, dim: int) -> tuple[dict, list]:
    spec = provisioning._load(provisioning._INFRA / "qdrant" / "collections" / "knowledge_units.json")
    body = provisioning._strip_docs(spec["collection"])
    body["vectors"]["dense"]["size"] = dim
    if idf:
        body["sparse_vectors"]["sparse"]["modifier"] = "idf"
    return body, [provisioning._strip_docs(i) for i in spec["payload_indexes"]]


def pct(values: list[float], p: float) -> float:
    s = sorted(values)
    return s[min(len(s) - 1, int(round(p / 100 * (len(s) - 1))))]


async def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dataset", required=True, type=Path)
    ap.add_argument("--qdrant", default="http://127.0.0.1:16333")
    ap.add_argument("--variant", choices=["asis", "prefix", "idf", "fixed"], required=True)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--limit", type=int, default=50)
    ap.add_argument("--batch", type=int, default=64)
    ap.add_argument("--skip-ingest", action="store_true")
    ap.add_argument("--concurrency", type=int, default=8)
    ap.add_argument("--cache", type=Path, help="embedding cache file; variants with the same prefix setting share it")
    ap.add_argument("--warmup", type=int, default=5)
    ap.add_argument("--dense-model", default=DENSE_MODEL)
    ap.add_argument("--dim", type=int, default=1024)
    ap.add_argument("--query-prefix", default=None, help="override the e5 query prefix (e.g. a Qwen3 instruction)")
    args = ap.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)

    prefix = args.variant in ("prefix", "fixed")
    idf = args.variant in ("idf", "fixed")
    key = os.environ.get("OPENROUTER_API_KEY", "")
    if not key:
        print("OPENROUTER_API_KEY is not set", file=sys.stderr)
        return 1
    embedder = OpenRouterKfEmbedder(
        model=args.dense_model, api_key=key, prefix=prefix, query_prefix=args.query_prefix
    )
    t0 = time.perf_counter()
    await embedder.warm()
    warm_s = time.perf_counter() - t0

    files = sorted((args.dataset / "corpus").glob("*"))
    units = [(f.stem, f.read_text(encoding="utf-8")) for f in files]
    queries = [json.loads(line) for line in (args.dataset / "queries.jsonl").open(encoding="utf-8")]

    async with httpx.AsyncClient(timeout=120) as client:
        ingest_s = 0.0
        if not args.skip_ingest:
            await client.delete(f"{args.qdrant}/collections/{COLLECTION}")
            body, indexes = collection_body(idf, args.dim)
            r = await client.put(f"{args.qdrant}/collections/{COLLECTION}", json=body)
            r.raise_for_status()
            for index in indexes:
                r = await client.put(
                    f"{args.qdrant}/collections/{COLLECTION}/index", params={"wait": "true"}, json=index
                )
                r.raise_for_status()
            cache: dict[str, Embedded] = {}
            if args.cache and args.cache.exists():
                with args.cache.open(encoding="utf-8") as fh:
                    for line in fh:
                        row = json.loads(line)
                        cache[row["id"]] = Embedded(
                            dense=row["d"], sparse=SparseVector(indices=row["si"], values=row["sv"])
                        )
                print(f"embedding cache: {len(cache)} units from {args.cache}", file=sys.stderr)
            cache_out = args.cache.open("a", encoding="utf-8") if args.cache else None
            t0 = time.perf_counter()
            for i in range(0, len(units), args.batch):
                chunk = units[i : i + args.batch]
                todo = [(uid, text) for uid, text in chunk if uid not in cache]
                if todo:
                    fresh = await embedder.embed_documents([text for _, text in todo])
                    for (uid, _), e in zip(todo, fresh, strict=True):
                        cache[uid] = e
                        if cache_out:
                            cache_out.write(json.dumps({"id": uid, "d": e.dense, "si": e.sparse.indices, "sv": e.sparse.values}) + "\n")
                    if cache_out:
                        cache_out.flush()
                embedded = [cache[uid] for uid, _ in chunk]
                points = [
                    build_point(
                        ku_id=uid,
                        embedded=e,
                        payload={
                            "ku_id": uid,
                            "tenant_id": TENANT,
                            "dataset_ids": [DATASET],
                            "language": "id",
                            "status": "validated",
                            "permission_scope": "tenant",
                            "version": 1,
                        },
                    )
                    for (uid, _), e in zip(chunk, embedded, strict=True)
                ]
                err = await upsert(client, base_url=args.qdrant, api_key="", points=points)
                if err:
                    print(f"upsert failed: {err}", file=sys.stderr)
                    return 1
                print(f"ingested {i + len(chunk)}/{len(units)}", file=sys.stderr, flush=True)
            ingest_s = time.perf_counter() - t0
            if cache_out:
                cache_out.close()

        async def one(q: dict) -> tuple[str, list, str | None, float, float]:
            t0 = time.perf_counter()
            embedded = await embedder.embed_query(q["text"])
            t1 = time.perf_counter()
            hits, err = await hybrid_search(
                client,
                base_url=args.qdrant,
                api_key="",
                tenant_id=TENANT,
                embedded=embedded,
                limit=args.limit,
                language="id",
                dataset_refs=[DATASET],
            )
            t2 = time.perf_counter()
            return q["_id"], hits, err, (t1 - t0) * 1000, (t2 - t0) * 1000

        # Warm-up: the first ONNX inference pays a one-time cost that is not query latency.
        for q in queries[: args.warmup]:
            await one(q)

        # Concurrency 1: the run that is scored, plus per-query timings.
        results = [await one(q) for q in queries]
        failed = [qid for qid, _, err, _, _ in results if err]
        with (args.out / "run.trec").open("w", encoding="utf-8") as fh:
            for qid, hits, err, _, _ in results:
                for h in hits:
                    fh.write(f"{qid} Q0 {h.ku_id} {h.rank} {h.score:.6f} kf-{args.variant}\n")

        # Concurrency N: throughput and tail under load (not scored).
        sem = asyncio.Semaphore(args.concurrency)

        async def guarded(q: dict):
            async with sem:
                return await one(q)

        t0 = time.perf_counter()
        loaded = await asyncio.gather(*(guarded(q) for q in queries))
        wall = time.perf_counter() - t0

    embed_ms = [r[3] for r in results]
    total_ms = [r[4] for r in results]
    loaded_ms = [r[4] for r in loaded]
    report = {
        "variant": args.variant,
        "dense_model": args.dense_model,
        "dim": args.dim,
        "prefix": prefix,
        "idf": idf,
        "units": len(units),
        "queries": len(queries),
        "failed": failed,
        "limit": args.limit,
        "model_warm_s": round(warm_s, 2),
        "ingest_s": round(ingest_s, 1),
        "c1": {
            "embed_p50": round(statistics.median(embed_ms), 1),
            "embed_p95": round(pct(embed_ms, 95), 1),
            "search_p50": round(statistics.median([t - e for t, e in zip(total_ms, embed_ms)]), 1),
            "search_p95": round(pct([t - e for t, e in zip(total_ms, embed_ms)], 95), 1),
            "plane_p50": round(statistics.median(total_ms), 1),
            "plane_p95": round(pct(total_ms, 95), 1),
            "plane_p99": round(pct(total_ms, 99), 1),
        },
        f"c{args.concurrency}": {
            "plane_p50": round(statistics.median(loaded_ms), 1),
            "plane_p95": round(pct(loaded_ms, 95), 1),
            "req_per_s": round(len(queries) / wall, 2),
        },
        "per_query_ms_c1": {r[0]: round(r[4], 1) for r in results},
    }
    (args.out / "latency.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
    print(json.dumps({k: v for k, v in report.items() if k != "per_query_ms_c1"}, indent=2))
    return 0 if not failed else 2


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
