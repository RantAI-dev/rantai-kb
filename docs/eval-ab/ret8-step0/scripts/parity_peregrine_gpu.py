#!/usr/bin/env python3
"""Task 5 parity: cosine between Peregrine Q8 and GPU bf16 vectors for the
same page texts.

Reads page texts from a pages.jsonl, sends `passage: <text>` to Peregrine's
/v1/embeddings (truncate: true) and the raw text to the GPU /v2/embed with
input_type=document (the server adds its own passage prefix), for the first
--n pages with text. Prints per-page cosine and the mean.
"""
import argparse
import json
import os
import sys
import urllib.request

import numpy as np


def post(url, body):
    req = urllib.request.Request(url, data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=1200))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pages", required=True)
    ap.add_argument("--gpu", default="http://127.0.0.1:8210/v2/embed")
    ap.add_argument("--gpu-model", default="nvidia/Nemotron-3.5-Embed-1B-BF16")
    ap.add_argument("--cpu", default="http://127.0.0.1:8311/v1/embeddings")
    ap.add_argument("--n", type=int, default=20)
    a = ap.parse_args()

    rows = [json.loads(l) for l in open(a.pages, encoding="utf-8")]
    rows = [r for r in rows if r.get("text")][:a.n]
    cos = []
    for r in rows:
        t = r["text"]
        g = post(a.gpu, {"model": a.gpu_model, "input_type": "document",
                         "inputs": [{"content": [{"type": "text", "text": t}]}],
                         "embedding_types": ["float"], "truncate": "END"})
        gv = np.asarray(g["embeddings"]["float"][0], dtype=np.float64)
        c = post(a.cpu, {"model": "peregrine", "input": ["passage: " + t], "truncate": True})
        cv = np.asarray(c["data"][0]["embedding"], dtype=np.float64)
        d = float(np.dot(gv, cv) / (np.linalg.norm(gv) * np.linalg.norm(cv)))
        cos.append(d)
        print(f"{r['_id']}\t{len(t)}\t{d:.6f}")
    print(f"mean cosine over {len(cos)} pages: {np.mean(cos):.6f} min {np.min(cos):.6f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
