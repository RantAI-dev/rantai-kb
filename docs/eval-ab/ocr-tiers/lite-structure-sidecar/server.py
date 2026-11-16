"""CPU OCR engines behind the chat-completions shape kb-server's local OCR adapter sends.

ENGINE: tesseract | onnxtr | rapidocr ; RAPID_VARIANT: see VARIANTS ; OCR_THREADS (default 2).
One page at a time; the answer is the page text, one visual line per line."""
import base64, io, json, os, threading, time, uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import numpy as np
from PIL import Image

ENGINE = os.environ.get("ENGINE", "rapidocr"); THREADS = int(os.environ.get("OCR_THREADS", "2"))
LOCK = threading.Lock()

if ENGINE == "tesseract":
    import pytesseract
    TESS_LANG = os.environ.get("TESS_LANG", "eng"); TESS_PSM = os.environ.get("TESS_PSM", "3")
    os.environ["OMP_THREAD_LIMIT"] = "1"
    def read(img):
        return pytesseract.image_to_string(img, lang=TESS_LANG, config=f"--oem 1 --psm {TESS_PSM}")
elif ENGINE == "onnxtr":
    os.environ.setdefault("OMP_NUM_THREADS", str(THREADS))
    from onnxtr.models import ocr_predictor, EngineConfig
    import onnxruntime as ort
    so = ort.SessionOptions(); so.intra_op_num_threads = THREADS; so.inter_op_num_threads = 1
    cfg = EngineConfig(session_options=so, providers=["CPUExecutionProvider"])
    MODEL = ocr_predictor(det_arch=os.environ.get("DET", "fast_base"), reco_arch=os.environ.get("RECO", "crnn_mobilenet_v3_small"),
                          det_engine_cfg=cfg, reco_engine_cfg=cfg, clf_engine_cfg=cfg, assume_straight_pages=True, detect_orientation=False)
    def read(img):
        doc = MODEL([np.asarray(img)])
        out = []
        for page in doc.pages:
            for block in page.blocks:
                for line in block.lines:
                    out.append(" ".join(w.value for w in line.words))
                out.append("")
        return "\n".join(out)
elif ENGINE == "structure":
    import queue
    from rapidocr import RapidOCR, ModelType as OM, OCRVersion
    from rapid_layout import RapidLayout, RapidLayoutInput, ModelType as LM
    from rapid_table import RapidTable, RapidTableInput, ModelType as TM
    WORKERS = int(os.environ.get("OCR_WORKERS", "1")); SIDE = int(os.environ.get("OCR_MAX_SIDE", "960"))
    LAYOUT_SIDE = int(os.environ.get("LAYOUT_SIDE", "1024"))
    th = max(1, THREADS // WORKERS); ecfg = {"intra_op_num_threads": th, "inter_op_num_threads": 1}
    POOL = queue.Queue()
    for _ in range(WORKERS):
        POOL.put((
            RapidOCR(params={"Det.ocr_version": OCRVersion.PPOCRV6, "Det.model_type": OM.TINY, "Rec.ocr_version": OCRVersion.PPOCRV6,
                             "Rec.model_type": OM.TINY, "Global.use_cls": False, "Global.max_side_len": SIDE, "Global.log_level": "error",
                             "EngineConfig.onnxruntime.intra_op_num_threads": th, "EngineConfig.onnxruntime.inter_op_num_threads": 1}),
            RapidLayout(cfg=RapidLayoutInput(model_type=LM.YOLOV8N_LAYOUT_GENERAL6, engine_cfg=ecfg)),
            RapidTable(RapidTableInput(model_type=TM.SLANETPLUS, engine_cfg=ecfg, use_ocr=True, ocr_params={"Det.model_type": OM.TINY, "Rec.model_type": OM.TINY, "Det.ocr_version": OCRVersion.PPOCRV6, "Rec.ocr_version": OCRVersion.PPOCRV6, "Global.log_level": "error"})),
        ))
    LOCK = None
    def read(img):
        e = POOL.get()
        try:
            return structure_page(img, *e)
        finally:
            POOL.put(e)
else:
    from rapidocr import RapidOCR, ModelType, OCRVersion
    v = os.environ.get("RAPID_VARIANT", "v6-small")
    ver = {"v6": OCRVersion.PPOCRV6, "v5": OCRVersion.PPOCRV5}[v.split("-")[0]]
    mt = {"small": ModelType.SMALL, "tiny": ModelType.TINY, "mobile": ModelType.MOBILE, "server": ModelType.SERVER}[v.split("-")[1]]
    import queue
    WORKERS = int(os.environ.get("OCR_WORKERS", "1")); SIDE = int(os.environ.get("OCR_MAX_SIDE", "2000"))
    POOL = queue.Queue()
    for _ in range(WORKERS):
        POOL.put(RapidOCR(params={"Det.ocr_version": ver, "Det.model_type": mt, "Rec.ocr_version": ver, "Rec.model_type": mt, "Global.use_cls": False,
                         "Global.max_side_len": SIDE, "EngineConfig.onnxruntime.intra_op_num_threads": max(1, THREADS // WORKERS),
                         "EngineConfig.onnxruntime.inter_op_num_threads": 1, "Global.log_level": "error"}))
    LOCK = None
    def read(img):
        e = POOL.get()
        try:
            r = e(np.asarray(img)[:, :, ::-1])
        finally:
            POOL.put(e)
        if r.boxes is None or not r.txts:
            return ""
        return xy_cut_text([(b, t) for b, t in zip(r.boxes, r.txts)], img.width, img.height)

def _bbox(b):
    xs = [p[0] for p in b]; ys = [p[1] for p in b]
    return min(xs), min(ys), max(xs), max(ys)

def _gaps(intervals, min_gap):
    """(position, width) of every gap no interval covers, at least min_gap wide."""
    iv = sorted(intervals); out = []; cur = iv[0][1]
    for a, b in iv[1:]:
        if a - cur >= min_gap:
            out.append(((cur + a) / 2, a - cur))
        cur = max(cur, b)
    return out

def _split(items, cuts, lo_idx, hi_idx):
    groups = [[] for _ in range(len(cuts) + 1)]
    for it in items:
        c = (it[0][lo_idx] + it[0][hi_idx]) / 2; groups[sum(1 for x in cuts if c > x)].append(it)
    return [g for g in groups if g]

def _xy_cut(items, depth=0):
    """Recursive XY-cut, widest gap first (relative to the line height)."""
    if len(items) <= 1 or depth > 16:
        return [items]
    hs = sorted(i[0][3] - i[0][1] for i in items); h = hs[len(hs) // 2] or 1
    rows = _gaps([(i[0][1], i[0][3]) for i in items], 1.2 * h)
    cols = _gaps([(i[0][0], i[0][2]) for i in items], 0.25 * h)
    best_r = max((w for _, w in rows), default=0) / (1.2 * h)
    best_c = max((w for _, w in cols), default=0) / (0.25 * h)
    if best_r == 0 and best_c == 0:
        return [items]
    if best_c >= best_r:
        parts = _split(items, [x for x, _ in cols], 0, 2)
    else:
        parts = _split(items, [y for y, _ in rows], 1, 3)
    return [g for part in parts for g in _xy_cut(part, depth + 1)]

def _lines(block):
    """Group boxes of one block into visual lines (same baseline), left to right."""
    block = sorted(block, key=lambda i: (i[0][1] + i[0][3]) / 2)
    lines = []
    for it in block:
        cy = (it[0][1] + it[0][3]) / 2; hh = it[0][3] - it[0][1]
        if lines and abs(cy - lines[-1][0]) < 0.5 * hh:
            lines[-1][1].append(it)
        else:
            lines.append([cy, [it]])
    return [" ".join(t for _, t in sorted(l[1], key=lambda i: i[0][0])) for l in lines]

def xy_cut_text(pairs, w, h):
    items = [(_bbox(b), t.strip()) for b, t in pairs if t.strip()]
    if not items:
        return ""
    paras = []
    for block in _xy_cut(items):
        text = ""
        for line in _lines(block):
            if text.endswith("-") and line[:1].islower():
                text = text[:-1] + line
            else:
                text = (text + " " + line) if text else line
        paras.append(text)
    return "\n\n".join(paras)

def _center_in(c, b, pad=0.0):
    return b[0] - pad <= c[0] <= b[2] + pad and b[1] - pad <= c[1] <= b[3] + pad

def structure_page(img, ocr, layout, table):
    """Layout regions + one OCR pass + table structure, in XY-cut reading order, as markdown."""
    arr = np.asarray(img)[:, :, ::-1]
    small = img.copy(); small.thumbnail((LAYOUT_SIDE, LAYOUT_SIDE)); k = img.width / small.width
    lr = layout(np.asarray(small)[:, :, ::-1])
    regions = []
    if lr.boxes is not None:
        for b, c in zip(lr.boxes, lr.class_names):
            regions.append({"box": tuple(float(v) * k for v in b), "cls": c, "lines": []})
    o = ocr(arr)
    lines = [(_bbox(b), t.strip(), b, sc) for b, t, sc in zip(o.boxes, o.txts, o.scores)] if o.boxes is not None else []
    orphans = []
    for ln in lines:
        if not ln[1]:
            continue
        c = ((ln[0][0] + ln[0][2]) / 2, (ln[0][1] + ln[0][3]) / 2)
        hits = [r for r in regions if _center_in(c, r["box"])]
        if hits:
            min(hits, key=lambda r: (r["box"][2] - r["box"][0]) * (r["box"][3] - r["box"][1]))["lines"].append(ln)
        else:
            orphans.append(ln)
    # Reading order over regions and orphan lines together.
    items = [(r["box"], ("region", i)) for i, r in enumerate(regions)] + [(ln[0], ("line", j)) for j, ln in enumerate(orphans)]
    if not items:
        return ""
    out = []
    for block in _xy_cut(items):
        orphan_lines = []
        for box, (kind, idx) in sorted(block, key=lambda it: (it[0][1], it[0][0])):
            if kind == "line":
                orphan_lines.append((orphans[idx][0], orphans[idx][1])); continue
            if orphan_lines:
                out.append(_para(orphan_lines)); orphan_lines = []
            r = regions[idx]; cls = r["cls"]; x1, y1, x2, y2 = r["box"]
            if cls == "Table" and r["lines"]:
                crop = arr[int(y1):int(y2), int(x1):int(x2)].copy()
                boxes = np.array([[[p[0] - x1, p[1] - y1] for p in ln[2]] for ln in r["lines"]], dtype=np.float32)
                try:
                    html = table(crop, ocr_results=[(boxes, tuple(ln[1] for ln in r["lines"]), tuple(float(ln[3]) for ln in r["lines"]))]).pred_htmls[0]
                    html = html.replace("<html><body>", "").replace("</body></html>", "")
                    out.append(html); continue
                except Exception:  # noqa: BLE001 - fall back to the cell text
                    pass
            if cls == "Figure":
                out.append(f"<figure data-bbox=\"{int(x1)},{int(y1)},{int(x2)},{int(y2)}\"></figure>")
                if r["lines"]:
                    out.append(_para([(ln[0], ln[1]) for ln in r["lines"]]))
                continue
            if not r["lines"]:
                continue
            text = _para([(ln[0], ln[1]) for ln in r["lines"]])
            out.append(("## " + text) if cls == "Title" and len(text) < 200 else text)
        if orphan_lines:
            out.append(_para(orphan_lines))
    return "\n\n".join(x for x in out if x.strip())

def _para(pairs):
    """Join a region's lines (in visual-line order) into one paragraph, de-hyphenating."""
    text = ""
    for line in _lines([(b, t) for b, t in pairs]):
        if text.endswith("-") and line[:1].islower():
            text = text[:-1] + line
        else:
            text = (text + " " + line) if text else line
    return text

def page_image(body):
    for m in body.get("messages", []):
        c = m.get("content")
        if isinstance(c, list):
            for part in c:
                if part.get("type") == "image_url":
                    url = part["image_url"]["url"]; data = base64.b64decode(url.split(",", 1)[1])
                    return Image.open(io.BytesIO(data)).convert("RGB")
    raise ValueError("no image")

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def _send(self, code, obj):
        b = json.dumps(obj).encode(); self.send_response(code); self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        self._send(200, {"status": "ok", "engine": ENGINE})
    def do_POST(self):
        try:
            body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))))
            img = page_image(body)
            t = time.time()
            if LOCK is None:
                text = read(img)
            else:
                with LOCK:
                    text = read(img)
            dt = time.time() - t
        except Exception as e:
            return self._send(400, {"error": {"message": str(e)[:300]}})
        self._send(200, {"id": "chatcmpl-" + uuid.uuid4().hex, "object": "chat.completion", "created": int(time.time()), "model": ENGINE,
                         "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
                         "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0, "ocr_seconds": round(dt, 3)}})

ThreadingHTTPServer(("0.0.0.0", int(os.environ.get("OCR_PORT", "8000"))), H).serve_forever()
