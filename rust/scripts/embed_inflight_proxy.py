#!/usr/bin/env python3
"""Counting reverse proxy for vLLM `/v2/embed`, used by bench-real.sh (DEP-7).

It forwards requests byte-for-byte to a real embedder and logs, for every
request, its kind (page when the body carries an image_url, else text), the
number of inputs, how many requests were in flight when it arrived, its
latency and its HTTP status. Nothing is cached and no model is started.

    python3 rust/scripts/embed_inflight_proxy.py \
        --listen 127.0.0.1:19210 --target http://127.0.0.1:8210 --log out/proxy.log
"""

from __future__ import annotations

import argparse
import json
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

STATE = {
    "lock": threading.Lock(),
    "inflight": 0,
    "max_inflight": 0,
    "requests": 0,
    "by_kind": {"text": 0, "page": 0},
    "failures": 0,
}


class Handler(BaseHTTPRequestHandler):
    target: str = "http://127.0.0.1:8210"
    log_path: str = "proxy.log"
    log_lock: threading.Lock = threading.Lock()

    protocol_version = "HTTP/1.1"

    def _log(self, line: str) -> None:
        with self.log_lock:
            with open(self.log_path, "a", encoding="utf-8") as fh:
                fh.write(line + "\n")

    def _forward(self, method: str) -> None:
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length) if length else b""
        counted = method == "POST" and bool(body)
        kind = "page" if b"image_url" in body else "text"
        n_inputs = 0
        if body:
            try:
                n_inputs = len(json.loads(body).get("inputs", []))
            except (ValueError, AttributeError):
                pass

        with STATE["lock"]:
            if counted:
                STATE["inflight"] += 1
                inflight = STATE["inflight"]
                STATE["max_inflight"] = max(STATE["max_inflight"], inflight)
                STATE["requests"] += 1
                STATE["by_kind"][kind] += 1
            else:
                inflight = STATE["inflight"]

        started = time.time()
        status = 502
        data = b""
        try:
            req = urllib.request.Request(
                self.target + self.path,
                data=body if method == "POST" else None,
                headers={"Content-Type": "application/json"},
                method=method,
            )
            with urllib.request.urlopen(req, timeout=600) as resp:
                status = resp.status
                data = resp.read()
        except urllib.error.HTTPError as exc:
            status = exc.code
            data = exc.read()
        except Exception as exc:  # noqa: BLE001 - the proxy must stay up
            data = json.dumps({"error": str(exc)}).encode()
        elapsed = time.time() - started
        if status >= 400 and counted:
            with STATE["lock"]:
                STATE["failures"] += 1

        if counted:
            self._log(
                f"{time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(started))} "
                f"kind={kind} inputs={n_inputs} inflight={inflight} "
                f"status={status} seconds={elapsed:.3f}"
            )
        try:
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            if method != "HEAD":
                self.wfile.write(data)
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            if counted:
                with STATE["lock"]:
                    STATE["inflight"] -= 1

    def do_POST(self) -> None:  # noqa: N802 - http.server API
        self._forward("POST")

    def do_GET(self) -> None:  # noqa: N802 - http.server API
        self._forward("GET")

    def do_HEAD(self) -> None:  # noqa: N802 - http.server API
        self._forward("HEAD")

    def log_message(self, fmt: str, *args: object) -> None:
        pass


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--listen", default="127.0.0.1:19210")
    parser.add_argument("--target", default="http://127.0.0.1:8210")
    parser.add_argument("--log", dest="log_path", default="proxy.log")
    parser.add_argument("--summary", default="")
    args = parser.parse_args()

    host, port = args.listen.rsplit(":", 1)
    Handler.target = args.target.rstrip("/")
    Handler.log_path = args.log_path
    server = ThreadingHTTPServer((host, int(port)), Handler)
    print(f"proxy {args.listen} -> {Handler.target}; log {args.log_path}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        summary = (
            f"requests={STATE['requests']} text={STATE['by_kind']['text']} "
            f"page={STATE['by_kind']['page']} max_inflight={STATE['max_inflight']} "
            f"failures={STATE['failures']}"
        )
        print(summary, flush=True)
        if args.summary:
            with open(args.summary, "a", encoding="utf-8") as fh:
                fh.write(summary + "\n")


if __name__ == "__main__":
    main()
