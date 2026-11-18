#!/usr/bin/env python3
"""Materialize miracl-vision page images from images.parquet next to pages.jsonl.

images.parquet: file_name (== corpus.image_id, e.g. "i0") -> {bytes, path}
(jpeg bytes). The corpus's _id is the page id used by pages.jsonl/qrels, so we
map image_id -> _id from corpus.parquet and write <out>/pages/<_id>.jpg at
native resolution unless the long side is > 1568 px, then downscale to 1568
(quality 90). Rewrites pages.jsonl with the "image" field (relative path).
"""
import argparse
import io
import json
import os
import sys

import pyarrow.parquet as pq
from PIL import Image

MAX_SIDE = 1568


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--src", required=True, help="dir with corpus.parquet and images.parquet")
    ap.add_argument("--out", required=True, help="dataset dir holding pages.jsonl")
    ap.add_argument("--limit", type=int, default=0, help="only write the first N images (debug)")
    a = ap.parse_args()

    cor = pq.read_table(os.path.join(a.src, "corpus.parquet"), columns=["_id", "image_id"])
    imgid_to_pid = {img: pid for pid, img in
                    zip(cor.column("_id").to_pylist(), cor.column("image_id").to_pylist())}
    print(f"corpus: {len(imgid_to_pid)} pages with image ids")

    pf = pq.ParquetFile(os.path.join(a.src, "images.parquet"))
    print("images schema:", pf.schema_arrow.names, "rows:", pf.metadata.num_rows)

    pages_dir = os.path.join(a.out, "pages")
    os.makedirs(pages_dir, exist_ok=True)
    n = scaled = missing = 0
    for rg in range(pf.num_row_groups):
        t = pf.read_row_group(rg, columns=["file_name", "image"])
        for row in t.to_pylist():
            pid = imgid_to_pid.get(row["file_name"])
            if pid is None:
                missing += 1
                continue
            if a.limit and n >= a.limit:
                continue
            blob = row["image"]["bytes"] if isinstance(row["image"], dict) else row["image"]
            im = Image.open(io.BytesIO(blob))
            w, h = im.size
            if max(w, h) > MAX_SIDE:
                r = MAX_SIDE / max(w, h)
                im = im.resize((round(w * r), round(h * r)), Image.LANCZOS)
                scaled += 1
            out = os.path.join(pages_dir, pid + ".jpg")
            if not os.path.exists(out):
                im.convert("RGB").save(out, "JPEG", quality=90)
            n += 1
            if n % 2000 == 0:
                print(f"  {n} images written ({scaled} downscaled)", flush=True)
    print(f"wrote {n} images, {scaled} downscaled, {missing} image rows with no corpus page")

    pj = os.path.join(a.out, "pages.jsonl")
    tmp = pj + ".tmp"
    m = 0
    with open(pj, encoding="utf-8") as f, open(tmp, "w", encoding="utf-8") as g:
        for line in f:
            r = json.loads(line)
            p = os.path.join(pages_dir, r["_id"] + ".jpg")
            if os.path.exists(p):
                r["image"] = f"pages/{r['_id']}.jpg"
                m += 1
            g.write(json.dumps(r, ensure_ascii=False) + "\n")
    os.replace(tmp, pj)
    print(f"pages.jsonl: {m} rows now carry an image path")
    return 0


if __name__ == "__main__":
    sys.exit(main())
