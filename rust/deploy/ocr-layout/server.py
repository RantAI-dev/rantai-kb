"""PP-DocLayoutV3 behind the layout shape kb-server's Xiaomi-OCR recipe asks for.

The Xiaomi-OCR-0 recipe reads a page whole, and the whole-page answer
sometimes misses a table the page clearly has. When that happens the recipe
asks a layout detector where the page's regions are, and reads the page region
by region instead. The detector is a service like any other OCR provider: it
can be down, and the caller degrades visibly (`metadata.ocr.layoutFailed`)
rather than pretending the page had no regions.

This server is that detector. It answers one page per request:

    POST /v1/layout   {"image": "<base64 png>", "mime": "image/png"}
      -> {"regions": [{"label": "table", "score": 0.94,
                       "bbox_2d": [x1, y1, x2, y2], "task_type": "table"}]}

    GET  /health      -> {"status": "ok", "model": "...", "device": "cuda:0"}

The boxes come back normalised to 0–1000, so they travel with the page's own
resolution: the region a detector saw in a 150 dpi page is the same region in
a 300 dpi page. The regions are in **reading order**: the model predicts an
order for every region, and that order is what the caller reads the page in.

The postprocessing — NMS, the containment rules, the reading order and the
label-to-task mapping — is the vendor's, ported from
`evaluation_pipeline/twostage_img2md.py` of `xiaomi-research/Xiaomi-OCR-0`
(Apache-2.0). See `docs/model-licences.md`.

Standard library HTTP server on purpose: no framework to pin. The model is a
GPU model, so one page at a time under a lock keeps its memory flat.

Settings (environment):

    LAYOUT_MODEL_DIR       PP-DocLayoutV3 directory  /app/model in the image
    LAYOUT_HOST            bind address              0.0.0.0
    LAYOUT_PORT            port                      8193
    LAYOUT_DEVICE          torch device              cpu (keep it off the vLLM GPU)
    LAYOUT_THRESHOLD       detection confidence      0.3
    LAYOUT_MAX_IMAGE_BYTES largest decoded image     33554432 (32 MiB)
"""

from __future__ import annotations

import base64
import io
import json
import os
import threading
import time
import traceback
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import numpy as np
from PIL import Image

MODEL_DIR = os.environ.get("LAYOUT_MODEL_DIR", "").strip()
DEVICE = os.environ.get("LAYOUT_DEVICE", "cpu")
THRESHOLD = float(os.environ.get("LAYOUT_THRESHOLD", "0.3"))
MAX_IMAGE_BYTES = int(os.environ.get("LAYOUT_MAX_IMAGE_BYTES", str(32 * 1024 * 1024)))

# ═══════════════════════════════════════════════════════════════════════════════
# Vendor constants (xiaomi-research/Xiaomi-OCR-0, Apache-2.0)
# ═══════════════════════════════════════════════════════════════════════════════

ID2LABEL = {
    0: "abstract", 1: "algorithm", 2: "aside_text", 3: "chart",
    4: "content", 5: "display_formula", 6: "doc_title", 7: "figure_title",
    8: "footer", 9: "footer_image", 10: "footnote", 11: "formula_number",
    12: "header", 13: "header_image", 14: "image", 15: "inline_formula",
    16: "number", 17: "paragraph_title", 18: "reference",
    19: "reference_content", 20: "seal", 21: "table", 22: "text",
    23: "vertical_text", 24: "vision_footnote",
}

LABEL_TASK_MAPPING = {
    "text": [
        "abstract", "algorithm", "content", "doc_title", "figure_title",
        "paragraph_title", "reference_content", "text", "vertical_text",
        "vision_footnote", "seal", "formula_number", "header", "footer",
        "number", "footnote", "aside_text", "reference",
    ],
    "table": ["table"],
    "formula": ["display_formula", "inline_formula"],
    "skip": ["chart", "image", "footer_image", "header_image"],
    "abandon": [],
}

LAYOUT_NMS = True
LAYOUT_UNCLIP_RATIO = [1.0, 1.0]
LAYOUT_MERGE_BBOXES_MODE = {
    0: "large", 1: "large", 2: "large", 3: "large", 4: "large",
    5: "large", 6: "large", 7: "large", 8: "large", 9: "large",
    10: "large", 11: "large", 12: "large", 13: "large", 14: "large",
    15: "large", 16: "large", 17: "large", 18: "small", 19: "large",
    20: "large", 21: "large", 22: "large", 23: "large", 24: "large",
}


def _iou(box1, box2):
    x1, y1, x2, y2 = box1
    x1_p, y1_p, x2_p, y2_p = box2
    x1_i = max(x1, x1_p)
    y1_i = max(y1, y1_p)
    x2_i = min(x2, x2_p)
    y2_i = min(y2, y2_p)
    inter_area = max(0, x2_i - x1_i + 1) * max(0, y2_i - y1_i + 1)
    box1_area = (x2 - x1 + 1) * (y2 - y1 + 1)
    box2_area = (x2_p - x1_p + 1) * (y2_p - y1_p + 1)
    return inter_area / float(box1_area + box2_area - inter_area)


def _nms(boxes, iou_same=0.6, iou_diff=0.95):
    scores = boxes[:, 1]
    indices = np.argsort(scores)[::-1]
    selected_boxes = []
    while len(indices) > 0:
        current = indices[0]
        current_box = boxes[current]
        current_class = current_box[0]
        current_coords = current_box[2:]
        selected_boxes.append(current)
        indices = indices[1:]
        filtered_indices = []
        for i in indices:
            box = boxes[i]
            box_class = box[0]
            box_coords = box[2:]
            iou_value = _iou(current_coords, box_coords)
            threshold = iou_same if current_class == box_class else iou_diff
            if iou_value < threshold:
                filtered_indices.append(i)
        indices = filtered_indices
    return selected_boxes


def _is_contained(box1, box2):
    _, _, x1, y1, x2, y2 = box1
    _, _, x1_p, y1_p, x2_p, y2_p = box2
    box1_area = (x2 - x1) * (y2 - y1)
    xi1 = max(x1, x1_p)
    yi1 = max(y1, y1_p)
    xi2 = min(x2, x2_p)
    yi2 = min(y2, y2_p)
    inter_width = max(0, xi2 - xi1)
    inter_height = max(0, yi2 - yi1)
    intersect_area = inter_width * inter_height
    iou_val = intersect_area / box1_area if box1_area > 0 else 0
    return iou_val >= 0.8


def _check_containment(boxes, preserve_indices=None, category_index=None, mode=None):
    n = len(boxes)
    contains_other = np.zeros(n, dtype=int)
    contained_by_other = np.zeros(n, dtype=int)
    for i in range(n):
        for j in range(n):
            if i == j:
                continue
            if preserve_indices is not None and boxes[i][0] in preserve_indices:
                continue
            if category_index is not None and mode is not None:
                if mode == "large" and boxes[j][0] == category_index:
                    if _is_contained(boxes[i], boxes[j]):
                        contained_by_other[i] = 1
                        contains_other[j] = 1
                if mode == "small" and boxes[i][0] == category_index:
                    if _is_contained(boxes[i], boxes[j]):
                        contained_by_other[i] = 1
                        contains_other[j] = 1
            else:
                if _is_contained(boxes[i], boxes[j]):
                    contained_by_other[i] = 1
                    contains_other[j] = 1
    return contains_other, contained_by_other


def _unclip_boxes(boxes, unclip_ratio=None):
    if unclip_ratio is None:
        return boxes
    if isinstance(unclip_ratio, dict):
        expanded_boxes = []
        for box in boxes:
            class_id, score, x1, y1, x2, y2 = box[:6]
            if class_id in unclip_ratio:
                width_ratio, height_ratio = unclip_ratio[class_id]
                width = x2 - x1
                height = y2 - y1
                new_w = width * width_ratio
                new_h = height * height_ratio
                center_x = x1 + width / 2
                center_y = y1 + height / 2
                new_x1 = center_x - new_w / 2
                new_y1 = center_y - new_h / 2
                new_x2 = center_x + new_w / 2
                new_y2 = center_y + new_h / 2
                expanded_box = [class_id, score, new_x1, new_y1, new_x2, new_y2]
                if len(box) > 6:
                    expanded_box.extend(box[6:])
                expanded_boxes.append(expanded_box)
            else:
                expanded_boxes.append(box)
        return np.array(expanded_boxes)
    widths = boxes[:, 4] - boxes[:, 2]
    heights = boxes[:, 5] - boxes[:, 3]
    new_w = widths * unclip_ratio[0]
    new_h = heights * unclip_ratio[1]
    center_x = boxes[:, 2] + widths / 2
    center_y = boxes[:, 3] + heights / 2
    new_x1 = center_x - new_w / 2
    new_y1 = center_y - new_h / 2
    new_x2 = center_x + new_w / 2
    new_y2 = center_y + new_h / 2
    expanded_boxes = np.column_stack(
        (boxes[:, 0], boxes[:, 1], new_x1, new_y1, new_x2, new_y2)
    )
    if boxes.shape[1] > 6:
        expanded_boxes = np.column_stack((expanded_boxes, boxes[:, 6:]))
    return expanded_boxes


def apply_layout_postprocess(
    raw_results, id2label, img_sizes,
    layout_nms=True, layout_unclip_ratio=None, layout_merge_bboxes_mode=None,
):
    """The vendor's box cleanup: NMS, containment rules, reading order."""
    all_labels = list(id2label.values())
    paddle_format_results = []
    for img_idx, result in enumerate(raw_results):
        scores = result["scores"].cpu().numpy()
        labels = result["labels"].cpu().numpy()
        boxes = result["boxes"].cpu().numpy()
        order_seq = result["order_seq"].cpu().numpy()
        img_size = img_sizes[img_idx]
        boxes_with_order = []
        for i in range(len(scores)):
            cls_id = int(labels[i])
            score = float(scores[i])
            x1, y1, x2, y2 = boxes[i]
            order = int(order_seq[i])
            boxes_with_order.append([cls_id, score, x1, y1, x2, y2, order])
        if len(boxes_with_order) == 0:
            paddle_format_results.append([])
            continue
        boxes_array = np.array(boxes_with_order)
        if layout_nms:
            selected_indices = _nms(boxes_array[:, :6], iou_same=0.6, iou_diff=0.98)
            boxes_array = boxes_array[selected_indices]
        filter_large_image = True
        if filter_large_image and len(boxes_array) > 1:
            area_thres = 0.82 if img_size[0] > img_size[1] else 0.93
            image_index = all_labels.index("image") if "image" in all_labels else None
            img_area = img_size[0] * img_size[1]
            filtered_boxes = []
            for box in boxes_array:
                label_index, score_val, xmin, ymin, xmax, ymax = box[:6]
                if label_index == image_index:
                    xmin = max(0, xmin)
                    ymin = max(0, ymin)
                    xmax = min(img_size[0], xmax)
                    ymax = min(img_size[1], ymax)
                    box_area = (xmax - xmin) * (ymax - ymin)
                    if box_area <= area_thres * img_area:
                        filtered_boxes.append(box)
                else:
                    filtered_boxes.append(box)
            if len(filtered_boxes) > 0:
                boxes_array = np.array(filtered_boxes)
        if layout_merge_bboxes_mode:
            preserve_labels = ["image", "seal", "chart"]
            preserve_indices = set()
            for label in preserve_labels:
                if label in all_labels:
                    preserve_indices.add(all_labels.index(label))
            if isinstance(layout_merge_bboxes_mode, str):
                if layout_merge_bboxes_mode == "union":
                    pass
                else:
                    contains_other, contained_by_other = _check_containment(
                        boxes_array[:, :6], preserve_indices
                    )
                    if layout_merge_bboxes_mode == "large":
                        boxes_array = boxes_array[contained_by_other == 0]
                    elif layout_merge_bboxes_mode == "small":
                        boxes_array = boxes_array[
                            (contains_other == 0) | (contained_by_other == 1)
                        ]
            elif isinstance(layout_merge_bboxes_mode, dict):
                keep_mask = np.ones(len(boxes_array), dtype=bool)
                for category_index, layout_mode in layout_merge_bboxes_mode.items():
                    if layout_mode == "union":
                        pass
                    else:
                        if layout_mode == "large":
                            co, cbo = _check_containment(
                                boxes_array[:, :6], preserve_indices,
                                category_index, mode=layout_mode,
                            )
                            keep_mask &= cbo == 0
                        elif layout_mode == "small":
                            co, cbo = _check_containment(
                                boxes_array[:, :6], preserve_indices,
                                category_index, mode=layout_mode,
                            )
                            keep_mask &= (co == 0) | (cbo == 1)
                boxes_array = boxes_array[keep_mask]
        if len(boxes_array) == 0:
            paddle_format_results.append([])
            continue
        sorted_idx = np.argsort(boxes_array[:, 6])
        boxes_array = boxes_array[sorted_idx]
        if layout_unclip_ratio:
            if isinstance(layout_unclip_ratio, float):
                layout_unclip_ratio = (layout_unclip_ratio, layout_unclip_ratio)
            boxes_array = _unclip_boxes(boxes_array, layout_unclip_ratio)
        img_width, img_height = img_size
        image_results = []
        for box_data in boxes_array:
            cls_id = int(box_data[0])
            score_val = float(box_data[1])
            x1, y1, x2, y2 = box_data[2:6]
            order = int(box_data[6]) if box_data[6] > 0 else None
            label_name = id2label.get(cls_id, f"class_{cls_id}")
            x1 = max(0, min(float(x1), img_width))
            y1 = max(0, min(float(y1), img_height))
            x2 = max(0, min(float(x2), img_width))
            y2 = max(0, min(float(y2), img_height))
            if x1 >= x2 or y1 >= y2:
                continue
            image_results.append({
                "cls_id": cls_id, "label": label_name, "score": score_val,
                "coordinate": [int(x1), int(y1), int(x2), int(y2)],
                "order": order,
            })
        paddle_format_results.append(image_results)
    return paddle_format_results


def task_type_of(label: str) -> str | None:
    """What to do with a region of this label, or `None` to drop it."""
    for task_item, labels in LABEL_TASK_MAPPING.items():
        if isinstance(labels, list) and label in labels:
            return task_item
    return None


# ═══════════════════════════════════════════════════════════════════════════════
# The model
# ═══════════════════════════════════════════════════════════════════════════════

PROCESSOR = None
MODEL = None
MODEL_LOCK = threading.Lock()


def load_model():
    """Load the detector. Called once, at startup."""
    global PROCESSOR, MODEL
    if not MODEL_DIR:
        raise SystemExit(
            "LAYOUT_MODEL_DIR is required: the directory of a local "
            "PP-DocLayoutV3 checkpoint"
        )
    import torch
    from transformers import PPDocLayoutV3ForObjectDetection, PPDocLayoutV3ImageProcessor

    print(f"loading PP-DocLayoutV3 from {MODEL_DIR} on {DEVICE} ...", flush=True)
    processor = PPDocLayoutV3ImageProcessor.from_pretrained(MODEL_DIR)
    model = PPDocLayoutV3ForObjectDetection.from_pretrained(MODEL_DIR)
    model.eval()

    if DEVICE.startswith("cuda"):
        if not torch.cuda.is_available():
            print("no CUDA here; falling back to the CPU", flush=True)
            model = model.to("cpu")
        else:
            # The card can be busy when this starts — another model on the
            # same GPU may hold the memory for a moment. A detector that
            # crashes on start is worse than one that waits.
            for attempt in range(1, 6):
                try:
                    model = model.to(DEVICE)
                    break
                except Exception as err:  # noqa: BLE001 - torch raises many kinds
                    print(f"the GPU is busy ({err}); retry {attempt}/5", flush=True)
                    time.sleep(10)
            else:
                print("no GPU memory after 5 tries; falling back to the CPU", flush=True)
                model = model.to("cpu")

    PROCESSOR = processor
    MODEL = model
    print(f"layout model ready on {MODEL.device}", flush=True)


def detect_regions(image: Image.Image) -> list[dict]:
    """The page's regions, in reading order, with boxes normalised to 0–1000."""
    import torch

    width, height = image.size
    inputs = PROCESSOR(images=image, return_tensors="pt")
    inputs = {k: v.to(MODEL.device) for k, v in inputs.items()}
    with torch.no_grad():
        outputs = MODEL(**inputs)
    target_sizes = torch.tensor([image.size[::-1]], device=MODEL.device)
    raw_results = PROCESSOR.post_process_object_detection(
        outputs, threshold=THRESHOLD, target_sizes=target_sizes,
    )
    paddle_results = apply_layout_postprocess(
        raw_results=raw_results, id2label=ID2LABEL, img_sizes=[image.size],
        layout_nms=LAYOUT_NMS, layout_unclip_ratio=LAYOUT_UNCLIP_RATIO,
        layout_merge_bboxes_mode=LAYOUT_MERGE_BBOXES_MODE,
    )

    regions: list[dict] = []
    for det in paddle_results[0]:
        label = det["label"]
        task_type = task_type_of(label)
        if task_type is None or task_type == "abandon":
            continue
        x1, y1, x2, y2 = det["coordinate"]
        regions.append({
            "label": label,
            "score": float(det["score"]),
            "bbox_2d": [
                int(float(x1) / width * 1000),
                int(float(y1) / height * 1000),
                int(float(x2) / width * 1000),
                int(float(y2) / height * 1000),
            ],
            "task_type": task_type,
        })
    return regions


# ═══════════════════════════════════════════════════════════════════════════════
# HTTP
# ═══════════════════════════════════════════════════════════════════════════════


class BadRequest(Exception):
    pass


def page_image(body: dict) -> Image.Image:
    """The base64 page image of the request, decoded."""
    raw = body.get("image")
    if not isinstance(raw, str) or not raw:
        raise BadRequest("the request has no image")
    if raw.startswith("data:"):
        _, _, raw = raw.partition(",")
    try:
        data = base64.b64decode(raw, validate=False)
    except (ValueError, TypeError) as err:
        raise BadRequest(f"the image is not base64: {err}") from err
    if len(data) > MAX_IMAGE_BYTES:
        raise BadRequest("image too large")
    try:
        return Image.open(io.BytesIO(data)).convert("RGB")
    except OSError as err:
        raise BadRequest(f"the image does not decode: {err}") from err


class Handler(BaseHTTPRequestHandler):
    server_version = "kb-ocr-layout/1"

    def _send(self, status: int, payload: dict) -> None:
        data = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self) -> None:  # noqa: N802
        if self.path.rstrip("/") in ("/health", ""):
            self._send(200, {
                "status": "ok",
                "model": os.path.basename(MODEL_DIR.rstrip("/")) if MODEL_DIR else "",
                "device": str(MODEL.device) if MODEL is not None else DEVICE,
                "threshold": THRESHOLD,
            })
        else:
            self._send(404, {"error": {"message": "not found"}})

    def do_POST(self) -> None:  # noqa: N802
        if self.path.rstrip("/") not in ("/v1/layout", "/layout"):
            self._send(404, {"error": {"message": "not found"}})
            return
        try:
            length = int(self.headers.get("content-length") or 0)
            if length <= 0 or length > MAX_IMAGE_BYTES * 2:
                raise BadRequest("missing or oversized body")
            body = json.loads(self.rfile.read(length))
            image = page_image(body)
            with MODEL_LOCK:
                regions = detect_regions(image)
            self._send(200, {"regions": regions})
        except (BadRequest, json.JSONDecodeError, ValueError, OSError) as err:
            self._send(400, {"error": {"message": str(err)}})
        except Exception as err:  # noqa: BLE001 - a 500 the caller degrades on
            traceback.print_exc()
            self._send(500, {"error": {"message": f"layout failed: {err}"}})

    def log_message(self, fmt: str, *args) -> None:
        print(f"{self.address_string()} {fmt % args}", flush=True)


def main() -> None:
    load_model()
    host = os.environ.get("LAYOUT_HOST", "0.0.0.0")
    port = int(os.environ.get("LAYOUT_PORT", "8193"))
    print(f"PP-DocLayoutV3 on {host}:{port}", flush=True)
    ThreadingHTTPServer((host, port), Handler).serve_forever()


if __name__ == "__main__":
    main()
