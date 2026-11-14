#!/usr/bin/env python3
"""Fake OpenAI-style chat-completions OCR server for the ING-6 ingest benchmark.

Every request is one page image. It answers after PAGE_S seconds (+/- JITTER),
the shape of a remote OCR service, with deterministic text derived from the
image bytes so chunks differ between pages. MAX_INFLIGHT caps concurrent pages
like a real server's batch slots (extra requests queue). One log line per
request: elapsed, in-flight count, max seen. A failing page can be forced with
FAIL_EVERY=N (every Nth request answers 500), for the failed-page checks.
"""
import hashlib, json, os, random, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PAGE_S = float(os.environ.get("PAGE_S", "1.5"))
JITTER = float(os.environ.get("JITTER", "0.3"))
MAX_INFLIGHT = int(os.environ.get("MAX_INFLIGHT", "16"))
FAIL_EVERY = int(os.environ.get("FAIL_EVERY", "0"))
PORT = int(os.environ.get("PORT", "39500"))
WORDS = ("laporan tahunan kinerja pendapatan regional produk anggaran statistik penduduk "
         "pemerintah undang pasal ayat revenue quarter growth segment operating income "
         "outlook margin cash flow employees policy region market share forecast").split()

slots = threading.BoundedSemaphore(MAX_INFLIGHT)
lock = threading.Lock()
state = {"n": 0, "max": 0, "req": 0}
t0 = time.time()


def page_text(digest: bytes) -> str:
    rng = random.Random(digest)
    paras = []
    for _ in range(4):
        paras.append(" ".join(rng.choice(WORDS) for _ in range(rng.randint(60, 90))) + ".")
    return "\n\n".join(paras)


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def _send(self, code, obj):
        out = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)

    def do_GET(self):
        self._send(200, {"object": "list", "data": [{"id": "fake-ocr", "object": "model"}]})

    def do_POST(self):
        raw = self.rfile.read(int(self.headers.get("content-length", "0")))
        digest = hashlib.sha256(raw).digest()
        with lock:
            state["req"] += 1
            req = state["req"]
        with slots:
            with lock:
                state["n"] += 1
                state["max"] = max(state["max"], state["n"])
                cur = state["n"]
            time.sleep(max(0.05, PAGE_S + random.uniform(-JITTER, JITTER)))
            with lock:
                state["n"] -= 1
        print(f"{time.time()-t0:8.1f} inflight={cur} max={state['max']} req={req}", flush=True)
        if FAIL_EVERY and req % FAIL_EVERY == 0:
            self._send(500, {"error": {"message": "forced failure"}})
            return
        self._send(200, {
            "id": f"ocr-{req}", "object": "chat.completion", "model": "fake-ocr",
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": page_text(digest)}}],
            "usage": {"prompt_tokens": 1000, "completion_tokens": 300},
        })


if __name__ == "__main__":
    srv = ThreadingHTTPServer(("127.0.0.1", PORT), H)
    srv.daemon_threads = True
    print(f"fake OCR on :{PORT} PAGE_S={PAGE_S} MAX_INFLIGHT={MAX_INFLIGHT}", flush=True)
    srv.serve_forever()
