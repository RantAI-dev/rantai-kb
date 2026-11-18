#!/usr/bin/env python3
"""Throughput of a /v2/embed endpoint over ViDoRe HR pages (task 2).

Usage: bench_teacher.py --pages <pages.jsonl> --dir <dir with page files> \
    --url http://127.0.0.1:8210/v2/embed --model nvidia/Nemotron-3.5-Embed-1B-BF16 \
    --arm image|text --concurrency 1|8|32 --n 200 [--batch B]

Warm-up: 8 items first (not timed). Then N items, ThreadPool of C workers,
each request carries `--batch` inputs. Reports wall time, pages/s and reads
nvidia-smi at the end. One JSON line per run on stdout.
"""
import argparse
import base64
import concurrent.futures as cf
import json
import os
import subprocess
import sys
import time
import urllib.request

import numpy as np


def build_payloads(args, rows):
    base = os.path.dirname(os.path.abspath(args.pages))
    out = []
    for r in rows:
        if args.arm == "image":
            p = r["image"]
            p = os.path.join(base, p) if not os.path.isabs(p) else p
            with open(p, "rb") as f:
                img = f.read()
            content = [{"type": "image_url", "image_url": {
                "url": "data:image/jpeg;base64," + base64.b64encode(img).decode()}}]
        else:
            content = [{"type": "text", "text": r.get("text") or ""}]
        out.append({"content": content})
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pages", required=True)
    ap.add_argument("--url", required=True)
    ap.add_argument("--model", required=True)
    ap.add_argument("--arm", choices=("image", "text"), required=True)
    ap.add_argument("--concurrency", type=int, required=True)
    ap.add_argument("--n", type=int, default=200)
    ap.add_argument("--batch", type=int, default=1)
    a = ap.parse_args()

    rows = [json.loads(l) for l in open(a.pages, encoding="utf-8")]
    payloads = build_payloads(a, rows)
    # warm-up on items beyond the measured window when possible, else the tail
    warm = payloads[a.n:a.n + 8] or payloads[:8]

    def call(items):
        body = {"model": a.model, "input_type": "document", "inputs": items,
                "embedding_types": ["float"], "truncate": "END"}
        req = urllib.request.Request(a.url, data=json.dumps(body).encode(),
                                     headers={"Content-Type": "application/json"})
        r = json.load(urllib.request.urlopen(req, timeout=1200))
        v = r["embeddings"]["float"]
        assert len(v) == len(items), (len(v), len(items))
        return v

    for i in range(0, len(warm), a.batch):
        call(warm[i:i + a.batch])

    items = payloads[:a.n]
    chunks = [items[i:i + a.batch] for i in range(0, len(items), a.batch)]
    t0 = time.time()
    dims = 0
    with cf.ThreadPoolExecutor(a.concurrency) as ex:
        for v in ex.map(call, chunks):
            dims = len(v[0])
    dt = time.time() - t0
    mem = subprocess.run(["nvidia-smi", "--query-gpu=memory.used", "--format=csv,noheader,nounits"],
                         capture_output=True, text=True).stdout.strip()
    print(json.dumps({"arm": a.arm, "concurrency": a.concurrency, "batch": a.batch,
                      "n": a.n, "wall_s": round(dt, 2), "pages_per_s": round(a.n / dt, 3),
                      "dim": dims, "gpu_mem_mib": mem}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
