#!/usr/bin/env python3
"""Extract each corpus PDF's section hierarchy (its outline) into one JSON file.

The tree index EVAL-2 measures needs two things before it can be built: a
document's sections, and where each one starts and stops. This script produces
both from the PDFs in the corpus, with no model, no network and no third-party
package.

Two extraction methods, one per document, recorded in the output:

  bookmarks  The PDF's own outline. Poppler's `pdftohtml -xml` prints it as a
             nested `<outline>` block, and each `<item page="N">Title</item>`
             is a bookmark whose destination page is the **1-based PDF page**
             (the same page number ViDoRe qrels use: `<doc>#p<N>`). Only page 1
             is rendered — the outline is emitted regardless — so the parse
             costs about a tenth of a second per book even at 900 pages.
  headings   No bookmarks. The fallback reads every page's text with
             `pdftotext -layout` and treats a numbered heading ("3.4 Title",
             "Chapter 3", "2.1.1 Title") as a section start. Table-of-contents
             pages are skipped first, or every heading would be "discovered" on
             the contents page and win the first-occurrence dedupe.
  none       Neither produced anything. The document is reported, not silently
             dropped, so a corpus with missing structure is visible.

Levels. The bookmark tree's nesting is used as the level, but a title that
carries its own numbering ("2.1.1") is trusted over the nesting: Poppler elides
bookmark parents that have no destination, so some reports come out with a
chapter's children at the wrong depth, while the number in the title is exact.
A title without numbering keeps its nesting depth.

End pages. `end_page` is "the page before the next section at the same or a
higher level starts", clamped to the document's last page, exactly the rule a
reader would use to say where a chapter stops. A section that shares its start
page with its first child therefore has a one-page span, which is the honest
answer: its content begins and its first subsection takes over on that page.

Usage:

    python3 outline.py                          # the two ViDoRe corpora
    python3 outline.py --dataset DIR [--dataset DIR ...] --out FILE

Run from the repo root. The output is deterministic (sorted documents, fixed
JSON indentation), so re-running it on the same PDFs produces the same bytes.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import time
import unicodedata
import xml.etree.ElementTree as ET
from pathlib import Path
from typing import Any

DEFAULT_DATASETS = [
    "rust/crates/kb-eval/datasets/vidore3-cs-en",
    "rust/crates/kb-eval/datasets/vidore3-hr-en",
]
DEFAULT_OUT = "rust/crates/kb-eval/sets/structural/outlines.json"

# A heading with a dotted number: "4.2", "2.1.1", "3.4.1.".
DOTTED = re.compile(r"^(?:section\s+|sec\.?\s+)?(\d+(?:\.\d+)+)\.?\s+(.+)$", re.I)
# A top-level numbered heading: "3. Labour market developments".
TOP_NUMBERED = re.compile(r"^(\d+)\.?\s+([A-Z][^.]{2,})$")
# "Chapter 3", "CHAPTER 3: Title", "Part II".
CHAPTER = re.compile(r"^(chapter|part)\s+([0-9]+|[ivxlcdm]+)\b[.:]?\s*(.*)$", re.I)
ANNEX = re.compile(r"^(annex|appendix)\b", re.I)
# A table-of-contents line ends in a page number, often after dot leaders.
TOC_LINE = re.compile(r"(\.{3,}\s*|\s{2,})\d{1,4}\s*$")


def run(cmd: list[str], *, binary: bool = False) -> bytes | str:
    proc = subprocess.run(cmd, capture_output=True)
    if proc.returncode != 0:
        raise RuntimeError(
            f"{' '.join(cmd)} failed ({proc.returncode}): "
            f"{proc.stderr.decode('utf-8', 'replace')[:400]}"
        )
    return proc.stdout if binary else proc.stdout.decode("utf-8", "replace")


def pdf_pages(path: Path) -> int:
    text = run(["pdfinfo", str(path)])
    match = re.search(r"^Pages:\s+(\d+)", text, re.M)
    if not match:
        raise RuntimeError(f"{path}: pdfinfo did not report a page count")
    return int(match.group(1))


def pdf_title(path: Path) -> str:
    text = run(["pdfinfo", str(path)])
    match = re.search(r"^Title:\s*(.+)$", text, re.M)
    title = unicodedata.normalize("NFC", match.group(1).strip()) if match else ""
    if len(title) >= 4 and title.lower() not in {"untitled", "unknown", "subject:"}:
        return title
    # Some PDFs write "Subject:" (or nothing) into /Title; a filename is
    # uglier but it is the only name the document actually carries.
    stem = re.sub(r"-[A-Z]{2,4}\d+ENN$", "", path.stem)
    words = stem.replace("_", " ").strip()
    return words[:1].upper() + words[1:] if words else path.stem


def clean_title(raw: str) -> str:
    text = re.sub(r"\s+", " ", raw).strip()
    # A bookmark title can carry a stray footnote marker ("... (3F )").
    text = re.sub(r"\s*\(\d+[A-Za-z]?\s*\)$", "", text).strip()
    return text


def numbering_level(title: str) -> int | None:
    """The level a title's own numbering implies, or None.

    "Chapter 3" and "3." are top level; "4.2" is level 2; "2.1.1" is level 3.
    """
    if CHAPTER.match(title) or ANNEX.match(title):
        return 1
    match = DOTTED.match(title)
    if match:
        return match.group(1).count(".") + 1
    match = TOP_NUMBERED.match(title)
    if match:
        return 1
    return None


def bookmark_nodes(path: Path) -> list[dict[str, Any]]:
    """Parse `pdftohtml -xml -i -f 1 -l 1` for the PDF's outline tree."""
    proc = subprocess.run(
        ["pdftohtml", "-xml", "-i", "-stdout", "-f", "1", "-l", "1", str(path)],
        capture_output=True,
    )
    if proc.returncode != 0 or not proc.stdout:
        return []
    try:
        root = ET.fromstring(proc.stdout)
    except ET.ParseError:
        return []
    outline = root.find("outline")
    if outline is None:
        return []

    raw: list[tuple[str, int, int]] = []

    def walk(element: ET.Element, depth: int) -> None:
        for child in element:
            if child.tag == "item":
                title = clean_title("".join(child.itertext()))
                try:
                    page = int(child.get("page", "0"))
                except ValueError:
                    continue
                if title and page > 0:
                    raw.append((title, depth, page))
            elif child.tag == "outline":
                walk(child, depth + 1)

    walk(outline, 1)
    return [
        {
            "title": title,
            "level": numbering_level(title) or depth,
            "page": page,
            "source_level": depth,
        }
        for title, depth, page in raw
    ]


def page_texts(path: Path) -> list[str]:
    text = run(["pdftotext", "-layout", str(path), "-"])
    pages = text.split("\f")
    if pages and not pages[-1].strip():
        pages.pop()
    return pages


def is_toc_page(text: str) -> bool:
    lines = [line for line in text.splitlines() if line.strip()]
    toc_lines = sum(1 for line in lines if TOC_LINE.search(line))
    top = " ".join(lines[:4]).lower()
    return toc_lines >= 3 and ("contents" in top or toc_lines * 2 >= len(lines))


def heading_nodes(path: Path) -> list[dict[str, Any]]:
    """Numbered headings found in page text, in document order, first wins."""
    nodes: list[dict[str, Any]] = []
    seen: set[tuple[str, int]] = set()
    for page_number, text in enumerate(page_texts(path), start=1):
        if is_toc_page(text):
            continue
        lines = text.splitlines()
        for index, line in enumerate(lines):
            candidate = line.strip()
            if not candidate or len(candidate) > 120:
                continue
            title = None
            level = None
            chapter = CHAPTER.match(candidate)
            dotted = DOTTED.match(candidate)
            top = TOP_NUMBERED.match(candidate)
            if chapter:
                title = clean_title(candidate)
                # A bare "Chapter 3" often has its title on the next line.
                if not chapter.group(3) and index + 1 < len(lines):
                    following = lines[index + 1].strip()
                    if following and len(following) < 80 and following[:1].isupper():
                        title = f"{title} {following}"
                        title = clean_title(title)
                level = 1
            elif dotted:
                title = clean_title(candidate)
                level = dotted.group(1).count(".") + 1
            elif top and len(top.group(2)) < 90:
                title = clean_title(candidate)
                level = 1
            if not title or level is None:
                continue
            # Drop a heading that is really a sentence or a figure caption.
            words = title.split()
            if len(words) > 16 or title.endswith("."):
                continue
            key = (re.sub(r"[^a-z0-9 ]", "", title.lower()), level)
            if key in seen:
                continue
            seen.add(key)
            nodes.append({"title": title, "level": level, "page": page_number,
                          "source_level": level})
    return nodes


def finish(nodes: list[dict[str, Any]], pages: int) -> list[dict[str, Any]]:
    """Add ids and end pages, and flag bookmark order that is not monotonic."""
    for index, node in enumerate(nodes):
        node["id"] = f"n{index:04d}"
        node["end_page"] = node["page"]
    for index, node in enumerate(nodes):
        end = pages
        for later in nodes[index + 1:]:
            if later["level"] <= node["level"]:
                end = later["page"] - 1
                break
        node["end_page"] = max(node["page"], min(end, pages))
    # A report whose bookmarks are all flat (one level) gives a chapter a
    # one-page span, because the very next bookmark is a subsection on the same
    # page. A chapter's real boundary is the next chapter heading, whatever the
    # bookmark depth claims, so use that when the title numbers itself.
    for index, node in enumerate(nodes):
        if not CHAPTER.match(node["title"]):
            continue
        for later in nodes[index + 1:]:
            if CHAPTER.match(later["title"]) and later["page"] > node["page"]:
                node["end_page"] = later["page"] - 1
                break
    for node in nodes:
        node["out_of_order"] = False
    previous = 0
    for node in nodes:
        if node["page"] < previous:
            node["out_of_order"] = True
        previous = max(previous, node["page"])
    return nodes


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--dataset",
        action="append",
        default=None,
        help="dataset directory with corpus/*.pdf (repeatable; default: both ViDoRe corpora)",
    )
    parser.add_argument("--out", type=Path, default=Path(DEFAULT_OUT))
    args = parser.parse_args()

    datasets = [Path(path) for path in (args.dataset or DEFAULT_DATASETS)]
    documents: dict[str, dict[str, Any]] = {}
    started = time.perf_counter()
    per_document: list[tuple[str, float, str, int]] = []

    for dataset in datasets:
        corpus = dataset / "corpus"
        if not corpus.is_dir():
            print(f"skip {corpus}: not a directory", file=sys.stderr)
            continue
        for pdf in sorted(corpus.glob("*.pdf")):
            key = pdf.stem
            doc_started = time.perf_counter()
            pages = pdf_pages(pdf)
            nodes = bookmark_nodes(pdf)
            method = "bookmarks"
            if not nodes:
                nodes = heading_nodes(pdf)
                method = "headings" if nodes else "none"
            nodes = finish(nodes, pages)
            elapsed = time.perf_counter() - doc_started
            per_document.append((key, elapsed, method, len(nodes)))
            documents[key] = {
                "path": f"{dataset.name}/corpus/{pdf.name}",
                "dataset": dataset.name,
                "title": pdf_title(pdf),
                "pages": pages,
                "method": method,
                "nodes": nodes,
                "out_of_order_nodes": sum(1 for n in nodes if n["out_of_order"]),
            }

    output = {
        "generator": "rust/crates/kb-eval/scripts/outline.py",
        "page_numbers": "1-based PDF page; matches ViDoRe <doc>#p<N> qrels",
        "documents": dict(sorted(documents.items())),
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(
        json.dumps(output, ensure_ascii=False, indent=2, sort_keys=False) + "\n",
        encoding="utf-8",
    )

    total = time.perf_counter() - started
    print(f"wrote {args.out} in {total:.2f}s")
    for key, elapsed, method, count in per_document:
        print(f"  {key}: {method}, {count} nodes, {elapsed * 1000:.0f} ms")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
