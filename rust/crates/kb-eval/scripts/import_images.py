# /// script
# dependencies = ["httpx", "python-pptx"]
# ///
"""Build `images-v1`: is a picture found by describing what it shows? (ING-5)

Three public sources, each pairing an image with text a human wrote about it.
The describing model only ever sees the pixels; the human text becomes the query.

  coco-*   COCO (Karpathy test split) photos, mostly no text in the picture.
           Query: the second human caption.             CC BY 4.0 annotations,
           images under their Flickr licences.
  tc-*     TextCaps val photos that contain scene text (signs, books, labels).
           Query: the second human caption, which usually names the text.
  chart-*  ChartQA test charts. Query: a human-written question (human split),
           kept only when it names something specific (a proper noun or number).

Two layouts are written from the same images and the same query ids, so the
same question can be scored against a standalone upload and against the image
inside a document:

  <out>/images-v1/            corpus/<key>.<jpg|png> standalone images
  <out>/images-v1-embedded/   each image inside a .docx, .pptx or .pdf (a third
                              each) whose only text is a generic caption line that
                              is identical across documents, so the text cannot
                              tell them apart; no alt text.

Both get the same distractors: `note-*.md` documents holding human captions of
*other* COCO photos (hard negatives in text space).

    uv run import_images.py --out DIR [--coco 100 --textcaps 50 --charts 50 --notes 150]

PDFs are made with LibreOffice (`soffice` on PATH).
"""

import argparse
import ast
import io
import json
import random
import re
import shutil
import subprocess
import tempfile
import zipfile
from pathlib import Path

import httpx

ROWS = "https://datasets-server.huggingface.co/rows"
SEED = 20261113


def get(client, url, **kw):
    """GET with retries: the datasets server answers 502/429 under load."""
    import time

    for attempt in range(6):
        try:
            r = client.get(url, timeout=60, follow_redirects=True, **kw)
            if r.status_code in (429, 500, 502, 503, 504):
                raise httpx.HTTPStatusError(str(r.status_code), request=r.request, response=r)
            r.raise_for_status()
            return r
        except (httpx.HTTPStatusError, httpx.TransportError):
            if attempt == 5:
                raise
            time.sleep(2 * 2**attempt)


def rows(client, dataset, split, offset, length, config="default"):
    params = {"dataset": dataset, "config": config, "split": split, "offset": offset, "length": length}
    return [x["row"] for x in get(client, ROWS, params=params).json()["rows"]]


def fetch(client, url):
    return get(client, url).content


def as_list(value):
    """The rows API returns some list columns as JSON lists, others as strings."""
    return value if isinstance(value, list) else ast.literal_eval(value)


def ext_of(data):
    return ".png" if data.startswith(b"\x89PNG") else ".jpg"


def specific(question):
    """A ChartQA question a searcher could use to find one chart."""
    words = question.split()
    if len(words) < 7:
        return False
    # A capitalised word that is not a colour, an answer word or a chart part
    # is a name a searcher would type (a country, a brand, a person).
    generic = {"yes", "no", "is", "the", "what", "which", "how", "green", "red", "blue",
               "orange", "grey", "gray", "yellow", "black", "dark", "light", "navy",
               "say", "and", "or", "in", "of", "bar", "bars", "graph", "chart", "a"}
    names = [w.strip(",.?!()'\"") for w in words[1:] if w[:1].isupper()]
    proper = any(n and n.lower() not in generic for n in names)
    return bool(proper or re.search(r"\b(19|20)\d\d\b", question))


DOCX_CT = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Default Extension="jpeg" ContentType="image/jpeg"/>
<Default Extension="png" ContentType="image/png"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"""
DOCX_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"""
DOC_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1{ext}"/>
</Relationships>"""
DOC_XML = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
 xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
 xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"
 xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
 xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture">
<w:body>
<w:p><w:r><w:t>{caption}</w:t></w:r></w:p>
<w:p><w:r><w:drawing><wp:inline><wp:extent cx="4572000" cy="3429000"/><wp:docPr id="1" name="Picture 1"/>
<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic>
<pic:nvPicPr><pic:cNvPr id="0" name="image1{ext}"/><pic:cNvPicPr/></pic:nvPicPr>
<pic:blipFill><a:blip r:embed="rId5"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>
<pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="4572000" cy="3429000"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr>
</pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>
</w:body></w:document>"""

GENERIC_CAPTION = "Lampiran dokumentasi kegiatan."


def write_docx(path, image, ext):
    ext = ".jpeg" if ext == ".jpg" else ext
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml", DOCX_CT)
        z.writestr("_rels/.rels", DOCX_RELS)
        z.writestr("word/_rels/document.xml.rels", DOC_RELS.replace("{ext}", ext))
        z.writestr("word/document.xml", DOC_XML.replace("{ext}", ext).replace("{caption}", GENERIC_CAPTION))
        z.writestr(f"word/media/image1{ext}", image)


def write_pptx(path, image):
    from pptx import Presentation
    from pptx.util import Inches

    prs = Presentation()
    slide = prs.slides.add_slide(prs.slide_layouts[5])
    slide.shapes.title.text = GENERIC_CAPTION
    slide.shapes.add_picture(io.BytesIO(image), Inches(1.5), Inches(1.8), width=Inches(7))
    prs.save(path)


def write_pdfs(jobs, out_dir):
    """jobs: [(key, image bytes, ext)] → <out_dir>/<key>.pdf through LibreOffice."""
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        htmls = []
        for key, image, ext in jobs:
            (tmp / f"{key}{ext}").write_bytes(image)
            html = tmp / f"{key}.html"
            html.write_text(
                f'<html><body><p>{GENERIC_CAPTION}</p><img src="{key}{ext}" width="600"></body></html>'
            )
            htmls.append(str(html))
        for start in range(0, len(htmls), 25):
            subprocess.run(
                ["soffice", "--headless", "--convert-to", "pdf:writer_web_pdf_Export", "--outdir", str(out_dir)]
                + htmls[start : start + 25],
                check=True,
                capture_output=True,
                timeout=600,
            )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--coco", type=int, default=100)
    ap.add_argument("--textcaps", type=int, default=50)
    ap.add_argument("--charts", type=int, default=50)
    ap.add_argument("--notes", type=int, default=150)
    args = ap.parse_args()
    rng = random.Random(SEED)

    std = args.out / "images-v1"
    emb = args.out / "images-v1-embedded"
    for d in (std, emb):
        shutil.rmtree(d, ignore_errors=True)
        (d / "corpus").mkdir(parents=True)
        (d / "qrels").mkdir()

    client = httpx.Client(headers={"User-Agent": "kb-eval-import"})
    items = []  # (key, query, image bytes, slice)

    coco_rows = []
    for off in range(0, 1000, 100):
        coco_rows += rows(client, "yerevann/coco-karpathy", "test", off, 100)
    rng.shuffle(coco_rows)
    picked, notes = coco_rows[: args.coco], coco_rows[args.coco : args.coco + args.notes]
    for r in picked:
        caps = as_list(r["sentences"])
        items.append((f"coco-{r['cocoid']}", caps[1].strip(), fetch(client, r["url"]), "photo"))

    tc_rows = []
    for off in range(0, 600, 100):
        tc_rows += rows(client, "lmms-lab-encoder/TextCaps", "val", off, 100)
    seen = set()
    rng.shuffle(tc_rows)
    for r in tc_rows:
        if len([k for k in items if k[3] == "scene-text"]) >= args.textcaps:
            break
        if r["image_id"] in seen:
            continue
        seen.add(r["image_id"])
        caps = as_list(r["caption_str"])
        items.append((f"tc-{r['image_id']}", caps[1].strip(), fetch(client, r["image"]["src"]), "scene-text"))

    chart_rows = []
    for off in range(0, 1200, 100):
        chart_rows += rows(client, "HuggingFaceM4/ChartQA", "test", off, 100)
    chart_rows = [r for r in chart_rows if str(r["human_or_machine"]) == "0" and specific(r["query"])]
    rng.shuffle(chart_rows)
    seen_q = set()
    n = 0
    for r in chart_rows:
        if n >= args.charts:
            break
        # One question per chart; the image src differs per row, so key on the question.
        if r["query"] in seen_q:
            continue
        seen_q.add(r["query"])
        n += 1
        items.append((f"chart-{n:03d}", r["query"].strip(), fetch(client, r["image"]["src"]), "chart"))

    queries = [{"_id": f"q-{k}", "text": q} for k, q, _, _ in items]
    meta = [{"_id": f"q-{k}", "slice": s} for k, _, _, s in items]
    for d in (std, emb):
        (d / "queries.jsonl").write_text("".join(json.dumps(q, ensure_ascii=False) + "\n" for q in queries))
        (d / "query-meta.jsonl").write_text("".join(json.dumps(m) + "\n" for m in meta))

    # Distractors: other photos' captions as short notes, in both layouts.
    for i, r in enumerate(notes):
        caps = as_list(r["sentences"])
        body = f"# Catatan lapangan {i + 1}\n\n{caps[0].strip()}\n"
        for d in (std, emb):
            (d / "corpus" / f"note-{i:03d}.md").write_text(body)

    std_qrels, emb_qrels, pdf_jobs = [], [], []
    for i, (key, _, image, _) in enumerate(items):
        ext = ext_of(image)
        (std / "corpus" / f"{key}{ext}").write_bytes(image)
        std_qrels.append(f"q-{key}\t{key}\t1\n")
        kind = ("docx", "pptx", "pdf")[i % 3]
        ekey = f"{kind}-{key}"
        if kind == "docx":
            write_docx(emb / "corpus" / f"{ekey}.docx", image, ext)
        elif kind == "pptx":
            write_pptx(emb / "corpus" / f"{ekey}.pptx", image)
        else:
            pdf_jobs.append((ekey, image, ext))
        emb_qrels.append(f"q-{key}\t{ekey}\t1\n")
    write_pdfs(pdf_jobs, emb / "corpus")

    (std / "qrels" / "test.tsv").write_text("query-id\tcorpus-id\tscore\n" + "".join(std_qrels))
    (emb / "qrels" / "test.tsv").write_text("query-id\tcorpus-id\tscore\n" + "".join(emb_qrels))
    counts = {s: sum(1 for x in items if x[3] == s) for s in ("photo", "scene-text", "chart")}
    for d in (std, emb):
        (d / "PROVENANCE.txt").write_text(
            "images-v1 (ING-5), built by rust/crates/kb-eval/scripts/import_images.py\n"
            f"seed {SEED}; slices {counts}; distractor notes {len(notes)}\n"
            "sources: yerevann/coco-karpathy (test), lmms-lab-encoder/TextCaps (val), "
            "HuggingFaceM4/ChartQA (test, human questions)\n"
        )
    print(json.dumps({"queries": len(items), **counts, "notes": len(notes)}))


if __name__ == "__main__":
    main()
