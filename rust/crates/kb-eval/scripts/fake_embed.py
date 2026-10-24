#!/usr/bin/env python3
"""Fake OpenAI-style embeddings server with provider-like latency, for ingest benchmarks.
Pages (input items that are objects) sleep PAGE_S per request; text sleeps TEXT_S.
Logs one line per request: time, kind, n, concurrency in flight."""
import json, os, random, sys, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
DIM = int(os.environ.get("DIM", "1024")); PAGE_S = float(os.environ.get("PAGE_S", "2.5")); TEXT_S = float(os.environ.get("TEXT_S", "1.8"))
lock = threading.Lock(); inflight = {"n": 0, "max": 0}; t0 = time.time()
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        self.send_response(200); self.end_headers(); self.wfile.write(b"ok")
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["content-length"])))
        inp = body.get("input", [])
        if isinstance(inp, str): inp = [inp]
        page = bool(inp) and isinstance(inp[0], dict)
        with lock:
            inflight["n"] += 1; inflight["max"] = max(inflight["max"], inflight["n"]); cur = inflight["n"]
        time.sleep((PAGE_S if page else TEXT_S) if len(inp) > 1 or page else 0.05)
        with lock: inflight["n"] -= 1
        print(f"{time.time()-t0:8.1f} {'page' if page else 'text'} n={len(inp)} inflight={cur} max={inflight['max']}", flush=True)
        data = [{"object": "embedding", "index": i, "embedding": [random.random() for _ in range(DIM)]} for i in range(len(inp))]
        out = json.dumps({"object": "list", "data": data, "model": body.get("model")}).encode()
        self.send_response(200); self.send_header("content-type", "application/json"); self.send_header("content-length", str(len(out))); self.end_headers(); self.wfile.write(out)
ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
