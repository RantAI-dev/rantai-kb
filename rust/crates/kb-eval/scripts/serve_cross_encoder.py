"""Serve any Hugging Face cross-encoder as the `/v1/rerank` shape.

`rerank_bakeoff.py` speaks llama.cpp's `/v1/rerank`, and some rerankers only
exist as HF checkpoints (or as GGUFs whose architecture the local llama.cpp
build does not know — `gte-multilingual-reranker-base` is one). This is the
same endpoint over `transformers` with the standard library only, so it needs
no web framework:

    python3 serve_cross_encoder.py --model Alibaba-NLP/gte-multilingual-reranker-base \\
        --port 8125 --api-key secret

    POST /v1/rerank {"model": "x", "query": "...", "documents": ["...", "..."]}
    -> {"results": [{"index": i, "relevance_score": s}, ...]}  (best first)
"""
import argparse
import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

import torch
from transformers import AutoModelForSequenceClassification, AutoTokenizer

ap = argparse.ArgumentParser()
ap.add_argument("--model", required=True)
ap.add_argument("--port", type=int, default=8125)
ap.add_argument("--api-key", default="")
ap.add_argument("--max-length", type=int, default=2048)
a = ap.parse_args()

tokenizer = AutoTokenizer.from_pretrained(a.model, trust_remote_code=True)
model = AutoModelForSequenceClassification.from_pretrained(
    a.model, trust_remote_code=True, torch_dtype=torch.bfloat16).eval().to("cuda")
lock = threading.Lock()


class Handler(BaseHTTPRequestHandler):
    def _send(self, code, payload):
        body = json.dumps(payload).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):  # noqa: N802
        if self.path == "/health":
            self._send(200, {"status": "ok", "model": a.model})
        else:
            self._send(404, {"error": "not found"})

    def do_POST(self):  # noqa: N802
        if self.path != "/v1/rerank":
            self._send(404, {"error": "not found"})
            return
        if a.api_key and self.headers.get("Authorization") != f"Bearer {a.api_key}":
            self._send(401, {"error": "unauthorized"})
            return
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))))
        query = body.get("query", "")
        docs = body.get("documents") or []
        if not docs:
            self._send(200, {"results": []})
            return
        scores = []
        with lock, torch.inference_mode():
            for d in docs:
                inputs = tokenizer(query, d, truncation=True, max_length=a.max_length,
                                   return_tensors="pt").to("cuda")
                logits = model(**inputs).logits.view(-1)
                # Single-logit rerankers are already a score; Qwen3-style ones
                # emit (no, yes) and the answer is the second column.
                scores.append(float(logits[-1] if logits.numel() > 1 else logits[0]))
        order = sorted(range(len(docs)), key=lambda i: -scores[i])
        self._send(200, {"results": [{"index": i, "relevance_score": scores[i]} for i in order]})

    def log_message(self, fmt, *args):
        pass


if __name__ == "__main__":
    print(f"serving {a.model} on :{a.port}", file=sys.stderr, flush=True)
    HTTPServer(("0.0.0.0", a.port), Handler).serve_forever()
