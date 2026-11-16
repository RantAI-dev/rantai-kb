"""Document OCR on a small CPU behind the chat shape kb-server's local OCR adapter sends.

kb-server (`kb_extract::PageOcrExtractor`, `KB_OCR_RECIPE=paddle`) posts one rendered
page per request to `/v1/chat/completions`: a user message whose content holds an
`image_url` part (base64 data URL) and a text part (the prompt, ignored here). The
answer is the page as markdown.

Two modes (`OCR_MODE`):

* `structure` (default): a layout model finds the page's regions (text, title, table,
  figure, caption, equation, header, footer); PP-OCRv6 tiny reads every text line once;
  each line is assigned to the region it sits in; tables become HTML through SLANet-plus
  (re-using the page's OCR lines, no second OCR pass); figures become a
  `<figure data-bbox="x1,y1,x2,y2">` marker with their text; page headers and footers are
  dropped (as Xiaomi-OCR-0's document prompt does); and the regions are put in reading
  order with a recursive XY-cut, so a multi-column page reads column by column.
* `text`: PP-OCRv6 tiny only, lines in XY-cut reading order, joined into paragraphs.

Measured on two cores of a 2016 Xeon (docs/eval-ab/ocr-tiers): PP-OCRv6 tiny reads a page
in ~1 s per core; the layout model and a table add ~0.3 s and ~0.13 s. Every model here
is Apache-2.0 (PP-OCRv6, PP-Layout CDLA, SLANet-plus). `LAYOUT_MODEL` can name another
RapidLayout model; the YOLOv8 360LayoutAnalysis weights are faster but their licence
requires an application for commercial use, so they are not the default.

Settings (environment):

    OCR_HOST            bind address                         0.0.0.0
    OCR_PORT            port                                 8000
    OCR_MODE            structure | text                     structure
    OCR_WORKERS         pages read at the same time          2
    OCR_THREADS         ONNX threads in total                OCR_WORKERS
    OCR_MAX_SIDE        text-detection resize (longest px)   960
    LAYOUT_SIDE         layout-model resize (longest px)     1024
    LAYOUT_MODEL        RapidLayout model type               pp_layout_cdla
    OCR_DROP_MARGINS    drop page headers and footers        1
    OCR_TEXT_SCORE      drop text boxes below this score     0.5
    OCR_FIGURE_TEXT_LINES  a "figure" with this many lines is text  12
    OCR_MAX_IMAGE_BYTES largest decoded image                33554432
"""

from __future__ import annotations

import base64
import io
import json
import os
import queue
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import numpy as np
from PIL import Image
from rapidocr import ModelType as OcrModel
from rapidocr import OCRVersion, RapidOCR

MODE = os.environ.get("OCR_MODE", "structure")
WORKERS = max(1, int(os.environ.get("OCR_WORKERS", "2")))
THREADS = max(1, int(os.environ.get("OCR_THREADS", str(WORKERS))))
MAX_SIDE = int(os.environ.get("OCR_MAX_SIDE", "960"))
LAYOUT_SIDE = int(os.environ.get("LAYOUT_SIDE", "1024"))
LAYOUT_MODEL = os.environ.get("LAYOUT_MODEL", "pp_layout_cdla")
DROP_MARGINS = os.environ.get("OCR_DROP_MARGINS", "1") not in ("0", "false", "no")
TEXT_SCORE = float(os.environ.get("OCR_TEXT_SCORE", "0.5"))
MAX_IMAGE_BYTES = int(os.environ.get("OCR_MAX_IMAGE_BYTES", str(32 * 1024 * 1024)))
# A figure region with at least this many text lines is read as text.
FIGURE_TEXT_LINES = int(os.environ.get("OCR_FIGURE_TEXT_LINES", "12"))
MODEL_NAME = f"rantai-kb-ocr-cpu/{MODE}"

PER_WORKER = max(1, THREADS // WORKERS)
ONNX = {"intra_op_num_threads": PER_WORKER, "inter_op_num_threads": 1}

# Normalised region kinds, whichever layout model labelled them.
KIND = {
    "text": "text", "plain text": "text", "reference": "text", "list": "text",
    "title": "title", "doc_title": "title", "paragraph_title": "title",
    "table": "table",
    "figure": "figure", "image": "figure", "chart": "figure",
    "figure_caption": "caption", "table_caption": "caption", "caption": "caption",
    "figure_title": "caption", "table_footnote": "caption",
    "equation": "equation", "isolate_formula": "equation", "display_formula": "equation",
    "header": "margin", "footer": "margin", "abandon": "margin", "number": "margin",
}


def _ocr() -> RapidOCR:
    return RapidOCR(params={
        "Det.ocr_version": OCRVersion.PPOCRV6, "Det.model_type": OcrModel.TINY,
        "Rec.ocr_version": OCRVersion.PPOCRV6, "Rec.model_type": OcrModel.TINY,
        "Global.use_cls": False, "Global.max_side_len": MAX_SIDE,
        "Global.text_score": TEXT_SCORE, "Global.log_level": "error",
        "EngineConfig.onnxruntime.intra_op_num_threads": PER_WORKER,
        "EngineConfig.onnxruntime.inter_op_num_threads": 1,
    })


def _engines():
    ocr = _ocr()
    if MODE != "structure":
        return (ocr, None, None)
    from rapid_layout import ModelType as LayoutModel
    from rapid_layout import RapidLayout, RapidLayoutInput
    from rapid_table import ModelType as TableModel
    from rapid_table import RapidTable, RapidTableInput

    layout = RapidLayout(cfg=RapidLayoutInput(model_type=LayoutModel(LAYOUT_MODEL), engine_cfg=dict(ONNX)))
    # use_ocr=True is what makes RapidTable match cells to text; the OCR results are
    # always passed in, so its own OCR engine is loaded but never run.
    table = RapidTable(RapidTableInput(
        model_type=TableModel.SLANETPLUS, engine_cfg=dict(ONNX), use_ocr=True,
        ocr_params={"Det.ocr_version": OCRVersion.PPOCRV6, "Det.model_type": OcrModel.TINY,
                    "Rec.ocr_version": OCRVersion.PPOCRV6, "Rec.model_type": OcrModel.TINY,
                    "Global.log_level": "error"},
    ))
    return (ocr, layout, table)


POOL: "queue.Queue[tuple]" = queue.Queue()
for _ in range(WORKERS):
    POOL.put(_engines())


# ─── geometry and reading order ──────────────────────────────────────────────

def _bbox(points) -> tuple[float, float, float, float]:
    xs = [float(p[0]) for p in points]
    ys = [float(p[1]) for p in points]
    return min(xs), min(ys), max(xs), max(ys)


def _gaps(intervals, min_gap):
    """(position, width) of every gap no interval covers that is at least min_gap wide."""
    spans = sorted(intervals)
    out, end = [], spans[0][1]
    for start, stop in spans[1:]:
        if start - end >= min_gap:
            out.append(((end + start) / 2, start - end))
        end = max(end, stop)
    return out


def _split(items, cuts, lo, hi):
    groups = [[] for _ in range(len(cuts) + 1)]
    for item in items:
        centre = (item[0][lo] + item[0][hi]) / 2
        groups[sum(1 for cut in cuts if centre > cut)].append(item)
    return [group for group in groups if group]


def xy_cut(items, depth=0):
    """Recursive XY-cut over (bbox, payload) items, widest gap first.

    Gaps are measured against the median box height h: a column gutter counts from
    0.25 h (detection boxes are padded, gutters are narrow), a row gap from 1.2 h (so
    ordinary line spacing never splits a paragraph)."""
    if len(items) <= 1 or depth > 16:
        return [items]
    heights = sorted(item[0][3] - item[0][1] for item in items)
    h = heights[len(heights) // 2] or 1.0
    rows = _gaps([(i[0][1], i[0][3]) for i in items], 1.2 * h)
    cols = _gaps([(i[0][0], i[0][2]) for i in items], 0.25 * h)
    best_row = max((w for _, w in rows), default=0.0) / (1.2 * h)
    best_col = max((w for _, w in cols), default=0.0) / (0.25 * h)
    if best_row == 0 and best_col == 0:
        return [items]
    if best_col >= best_row:
        parts = _split(items, [x for x, _ in cols], 0, 2)
    else:
        parts = _split(items, [y for y, _ in rows], 1, 3)
    return [block for part in parts for block in xy_cut(part, depth + 1)]


def visual_lines(pairs):
    """(bbox, text) boxes of one block grouped into visual lines, top to bottom."""
    pairs = sorted(pairs, key=lambda p: (p[0][1] + p[0][3]) / 2)
    lines = []
    for box, text in pairs:
        centre = (box[1] + box[3]) / 2
        if lines and abs(centre - lines[-1][0]) < 0.5 * (box[3] - box[1]):
            lines[-1][1].append((box, text))
        else:
            lines.append([centre, [(box, text)]])
    return [" ".join(t for _, t in sorted(line[1], key=lambda p: p[0][0])) for line in lines]


def paragraph(pairs) -> str:
    """Join a block's lines into one paragraph, mending words hyphenated at a line end."""
    text = ""
    for line in visual_lines(pairs):
        if text.endswith("-") and line[:1].islower():
            text = text[:-1] + line
        else:
            text = f"{text} {line}" if text else line
    return text


# ─── pages ───────────────────────────────────────────────────────────────────

def read_lines(ocr, arr):
    result = ocr(arr)
    if result.boxes is None or not result.txts:
        return []
    return [(_bbox(b), t.strip(), b, float(s))
            for b, t, s in zip(result.boxes, result.txts, result.scores) if t.strip()]


def text_page(ocr, arr) -> str:
    lines = read_lines(ocr, arr)
    if not lines:
        return ""
    blocks = xy_cut([(ln[0], ln[1]) for ln in lines])
    return "\n\n".join(paragraph(block) for block in blocks)


def structure_page(ocr, layout, table, image: Image.Image, arr) -> str:
    small = image.copy()
    small.thumbnail((LAYOUT_SIDE, LAYOUT_SIDE))
    scale = image.width / small.width
    found = layout(np.asarray(small)[:, :, ::-1])
    regions = []
    if found.boxes is not None:
        for box, label in zip(found.boxes, found.class_names):
            regions.append({"box": tuple(float(v) * scale for v in box),
                            "kind": KIND.get(str(label).lower(), "text"), "lines": []})

    orphans = []
    for line in read_lines(ocr, arr):
        cx, cy = (line[0][0] + line[0][2]) / 2, (line[0][1] + line[0][3]) / 2
        hits = [r for r in regions if r["box"][0] <= cx <= r["box"][2] and r["box"][1] <= cy <= r["box"][3]]
        if hits:
            # The smallest region wins: a caption inside a figure box is the caption.
            min(hits, key=lambda r: (r["box"][2] - r["box"][0]) * (r["box"][3] - r["box"][1]))["lines"].append(line)
        else:
            orphans.append(line)

    items = [(r["box"], ("region", i)) for i, r in enumerate(regions)
             if not (DROP_MARGINS and r["kind"] == "margin")]
    items += [(line[0], ("line", j)) for j, line in enumerate(orphans)]
    if not items:
        return ""
    out = []
    for block in xy_cut(items):
        loose = []
        for _, (kind, index) in sorted(block, key=lambda it: (it[0][1], it[0][0])):
            if kind == "line":
                loose.append((orphans[index][0], orphans[index][1]))
                continue
            if loose:
                out.append(paragraph(loose))
                loose = []
            out.append(render_region(regions[index], table, arr))
        if loose:
            out.append(paragraph(loose))
    return "\n\n".join(part for part in out if part and part.strip())


def render_region(region, table, arr) -> str:
    kind, lines = region["kind"], region["lines"]
    x1, y1, x2, y2 = region["box"]
    if kind == "table" and lines:
        crop = arr[int(y1):int(y2), int(x1):int(x2)].copy()
        boxes = np.array([[[p[0] - x1, p[1] - y1] for p in ln[2]] for ln in lines], dtype=np.float32)
        try:
            html = table(crop, ocr_results=[(boxes, tuple(ln[1] for ln in lines),
                                             tuple(ln[3] for ln in lines))]).pred_htmls[0]
            return html.replace("<html><body>", "").replace("</body></html>", "")
        except Exception:  # noqa: BLE001 - an unreadable table keeps its cell text
            pass
    # Reading order inside the region too: a layout model sometimes boxes a whole
    # multi-column page as one region, and joining its lines top to bottom would
    # interleave the columns again.
    pairs = [(ln[0], ln[1]) for ln in lines]
    text = "\n\n".join(paragraph(block) for block in xy_cut(pairs)) if pairs else ""
    if kind == "figure" and len(lines) >= FIGURE_TEXT_LINES:
        # A "figure" that holds this much text is a page of text the layout model
        # mislabelled: say what it is, but do not mark it as a picture.
        kind = "text"
    if kind == "figure":
        marker = f'<figure data-bbox="{int(x1)},{int(y1)},{int(x2)},{int(y2)}"></figure>'
        return f"{marker}\n\n{text}" if text else marker
    if kind == "title" and text and len(text) < 200:
        return f"## {text}"
    return text


def read_page(image: Image.Image) -> str:
    engines = POOL.get()
    try:
        arr = np.asarray(image)[:, :, ::-1].copy()
        ocr, layout, table = engines
        if layout is None:
            return text_page(ocr, arr)
        return structure_page(ocr, layout, table, image, arr)
    finally:
        POOL.put(engines)


# ─── HTTP ────────────────────────────────────────────────────────────────────

class BadRequest(Exception):
    pass


def page_image(body: dict) -> Image.Image:
    """The first image_url part of the last user message, decoded."""
    for message in reversed(body.get("messages") or []):
        if message.get("role") != "user" or not isinstance(message.get("content"), list):
            continue
        for part in message["content"]:
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


def completion(text: str, model: str, seconds: float) -> dict:
    return {
        "id": f"chatcmpl-{uuid.uuid4().hex}",
        "object": "chat.completion",
        "created": int(time.time()),
        "model": model,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0, "ocr_seconds": round(seconds, 3)},
    }


class Handler(BaseHTTPRequestHandler):
    server_version = "rantai-kb-ocr-cpu/1"

    def _send(self, status: int, payload: dict) -> None:
        data = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self) -> None:  # noqa: N802
        if self.path in ("/health", "/v1/models"):
            self._send(200, {"status": "ok", "mode": MODE, "layout": LAYOUT_MODEL if MODE == "structure" else None,
                             "data": [{"id": MODEL_NAME, "object": "model"}]})
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
            started = time.time()
            text = read_page(image)
            self._send(200, completion(text, body.get("model") or MODEL_NAME, time.time() - started))
        except (BadRequest, json.JSONDecodeError, ValueError, OSError) as err:
            self._send(400, {"error": {"message": str(err)}})
        except Exception as err:  # noqa: BLE001 - a 500 the adapter retries
            self._send(500, {"error": {"message": f"ocr failed: {err}"}})

    def log_message(self, fmt: str, *args) -> None:
        print(f"{self.address_string()} {fmt % args}", flush=True)


def warm_up() -> None:
    """Run every engine once on a drawn page: the OCR, layout and table models
    load (and, at build time, download) lazily on their first inference."""
    from PIL import ImageDraw

    page = Image.new("RGB", (800, 600), "white")
    draw = ImageDraw.Draw(page)
    draw.text((40, 40), "rantai-kb warm-up page", fill="black")
    for row in range(4):
        for col in range(3):
            x, y = 40 + col * 200, 200 + row * 50
            draw.rectangle((x, y, x + 200, y + 50), outline="black")
            draw.text((x + 10, y + 15), f"cell {row}.{col}", fill="black")
    engines = []
    while not POOL.empty():
        engines.append(POOL.get())
    try:
        for ocr, layout, table in engines:
            arr = np.asarray(page)[:, :, ::-1].copy()
            lines = read_lines(ocr, arr)
            if layout is not None:
                layout(arr)
                if lines:
                    boxes = np.array([ln[2] for ln in lines], dtype=np.float32)
                    table(arr, ocr_results=[(boxes, tuple(ln[1] for ln in lines), tuple(ln[3] for ln in lines))])
    finally:
        for engine in engines:
            POOL.put(engine)


def main() -> None:
    host = os.environ.get("OCR_HOST", "0.0.0.0")
    port = int(os.environ.get("OCR_PORT", "8000"))
    print(f"rantai-kb OCR on CPU: mode={MODE} layout={LAYOUT_MODEL} workers={WORKERS} "
          f"threads={THREADS} on {host}:{port}", flush=True)
    warm_up()
    ThreadingHTTPServer((host, port), Handler).serve_forever()


if __name__ == "__main__":
    main()
