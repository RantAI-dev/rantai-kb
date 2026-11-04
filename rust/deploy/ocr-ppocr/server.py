"""PP-OCRv6 on CPU behind the chat shape kb-server's local OCR adapter sends.

kb-server (`kb_extract::PageOcrExtractor`, `KB_OCR_*`) posts one page per
request to `/v1/chat/completions`: a user message whose content holds an
`image_url` part with a base64 data URL and a text part with the task prefix
(`OCR:`). This server runs RapidOCR (PP-OCRv6 det + rec, ONNX Runtime, CPU) on
that image and answers with an OpenAI-shaped completion whose message content
is the page text, one visual line per line.

The prompt is ignored: PP-OCRv6 only reads text. Tables come back as their
cell text in reading order, not as a table.

Standard library HTTP server on purpose: one page at a time per worker thread,
no framework to pin. Settings (environment):

    OCR_HOST              bind address            0.0.0.0
    OCR_PORT              port                    8000
    OCR_THREADS           ONNX intra-op threads   2
    OCR_MAX_IMAGE_BYTES   largest decoded image   33554432 (32 MiB)
    OCR_TEXT_SCORE        drop boxes below this   0.5
"""

from __future__ import annotations

import base64
import io
import json
import os
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import numpy as np
from PIL import Image

from rapidocr import RapidOCR

MODEL_NAME = "PP-OCRv6"
THREADS = int(os.environ.get("OCR_THREADS", "2"))
MAX_IMAGE_BYTES = int(os.environ.get("OCR_MAX_IMAGE_BYTES", str(32 * 1024 * 1024)))
TEXT_SCORE = float(os.environ.get("OCR_TEXT_SCORE", "0.5"))

ENGINE = RapidOCR(
    params={
        "Global.text_score": TEXT_SCORE,
        "EngineConfig.onnxruntime.intra_op_num_threads": THREADS,
        "EngineConfig.onnxruntime.inter_op_num_threads": 1,
    }
)
# ONNX Runtime sessions are not documented as safe for concurrent `run` calls
# with one shared engine; one page at a time keeps the CPU budget predictable.
ENGINE_LOCK = threading.Lock()


class BadRequest(Exception):
    pass


def page_image(body: dict) -> Image.Image:
    """The first image_url part of the last user message, decoded."""
    messages = body.get("messages") or []
    for message in reversed(messages):
        if message.get("role") != "user":
            continue
        content = message.get("content")
        if not isinstance(content, list):
            continue
        for part in content:
            if part.get("type") != "image_url":
                continue
            url = (part.get("image_url") or {}).get("url", "")
            if not url.startswith("data:") or "," not in url:
                raise BadRequest("image_url must be a base64 data URL")
            raw = base64.b64decode(url.split(",", 1)[1], validate=False)
            if len(raw) > MAX_IMAGE_BYTES:
                raise BadRequest("image too large")
            return Image.open(io.BytesIO(raw)).convert("RGB")
    raise BadRequest("no image_url part in the user message")


def lines_from(result) -> str:
    """Group word boxes into visual lines, top to bottom, left to right."""
    boxes = getattr(result, "boxes", None)
    txts = getattr(result, "txts", None)
    if boxes is None or txts is None or len(txts) == 0:
        return ""
    items = []
    for box, text in zip(boxes, txts):
        pts = np.asarray(box, dtype=float)
        top, bottom = pts[:, 1].min(), pts[:, 1].max()
        items.append((top, bottom, pts[:, 0].min(), str(text)))
    items.sort(key=lambda item: (item[0], item[2]))

    lines: list[list[tuple[float, float, float, str]]] = []
    for item in items:
        top, bottom = item[0], item[1]
        if lines:
            last = lines[-1]
            l_top = min(i[0] for i in last)
            l_bottom = max(i[1] for i in last)
            overlap = min(bottom, l_bottom) - max(top, l_top)
            if overlap > 0.5 * min(bottom - top, l_bottom - l_top):
                last.append(item)
                continue
        lines.append([item])
    return "\n".join(" ".join(i[3] for i in sorted(line, key=lambda i: i[2])) for line in lines)


def completion(text: str, model: str) -> dict:
    return {
        "id": f"chatcmpl-{uuid.uuid4().hex}",
        "object": "chat.completion",
        "created": int(time.time()),
        "model": model,
        "choices": [
            {
                "index": 0,
                "message": {"role": "assistant", "content": text},
                "finish_reason": "stop",
            }
        ],
        "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0},
    }


class Handler(BaseHTTPRequestHandler):
    server_version = "kb-ocr-ppocr/1"

    def _send(self, status: int, payload: dict) -> None:
        data = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self) -> None:  # noqa: N802
        if self.path in ("/health", "/v1/models"):
            self._send(200, {"status": "ok", "data": [{"id": MODEL_NAME, "object": "model"}]})
        else:
            self._send(404, {"error": {"message": "not found"}})

    def do_POST(self) -> None:  # noqa: N802
        if self.path.rstrip("/") not in ("/v1/chat/completions", "/chat/completions"):
            self._send(404, {"error": {"message": "not found"}})
            return
        try:
            length = int(self.headers.get("content-length") or 0)
            if length <= 0 or length > MAX_IMAGE_BYTES * 2:
                raise BadRequest("missing or oversized body")
            body = json.loads(self.rfile.read(length))
            image = page_image(body)
            with ENGINE_LOCK:
                result = ENGINE(np.asarray(image))
            self._send(200, completion(lines_from(result), body.get("model") or MODEL_NAME))
        except (BadRequest, json.JSONDecodeError, ValueError, OSError) as err:
            self._send(400, {"error": {"message": str(err)}})
        except Exception as err:  # noqa: BLE001 - a 500 the adapter retries
            self._send(500, {"error": {"message": f"ocr failed: {err}"}})

    def log_message(self, fmt: str, *args) -> None:
        print(f"{self.address_string()} {fmt % args}", flush=True)


def main() -> None:
    host = os.environ.get("OCR_HOST", "0.0.0.0")
    port = int(os.environ.get("OCR_PORT", "8000"))
    print(f"PP-OCRv6 (RapidOCR, {THREADS} threads) on {host}:{port}", flush=True)
    ThreadingHTTPServer((host, port), Handler).serve_forever()


if __name__ == "__main__":
    main()
