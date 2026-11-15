#!/usr/bin/env python3
"""Reference generator for the Xiaomi-OCR-0 markdown table format.

The vendor's ``postprocess/otsl.py`` renders an OTSL table as HTML and nothing
else: it has no markdown renderer, because the pipeline it belongs to writes
HTML. ``KB_OCR_TABLE_FORMAT=markdown`` is this repository's own addition — the
table parser downstream reads markdown pipe tables — so its correctness cannot
be checked against the vendor's output.

It can be checked against *this* script: an independent implementation of the
same OTSL grid rules (origin cells, continuation tags, span measurement) that
writes the table as a markdown pipe table. The rules are the ones documented in
``kb-core/src/xiaomi_ocr/otsl.rs``:

* every row is padded to one width — the cheapest width that keeps every row's
  declared content, trading truncation against padding;
* a cell that covers several positions is repeated in each of them, because a
  markdown table has no spans;
* the first grid row becomes the header, followed by a separator row.

The script reuses the vendor's own math-delimiter and repeated-tail handling
(``postprocess.document``); only the table rendering is its own. It is a
reference, not a port: it is written from the rules, so that a disagreement
with the Rust port means one of the two is wrong.

Usage::

    python3 -I rust/scripts/xiaomi-ocr-markdown-ref.py \
        --vendor /path/to/Xiaomi-OCR-0 \
        --raw /path/to/raw \
        --out /path/to/markdown-reference

``--raw`` holds the model's answers (``<category>/<name>.md``); ``--out``
receives the same tree with the markdown table format applied.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

ROW_BREAK = "<nl>"
CELL = "<fcel>"
EMPTY = "<ecel>"
LEFT = "<lcel>"
UP = "<ucel>"
BOTH = "<xcel>"
ALL_TAGS = (CELL, EMPTY, LEFT, UP, BOTH, ROW_BREAK)
TAG_RE = re.compile("(" + "|".join(re.escape(tag) for tag in ALL_TAGS) + ")")
ORIGIN_TAGS = frozenset({CELL, EMPTY})
RIGHT_CONTINUATION = frozenset({LEFT, BOTH})
DOWN_CONTINUATION = frozenset({UP, BOTH})

OTSL_TAG_RE = re.compile(r"<(?:fcel|ecel|nl|lcel|ucel|xcel)>")


def split_chunks(line: str) -> list[str]:
    """Split a row into chunks, each holding one tag plus the text after it."""
    starts = [match.start() for match in TAG_RE.finditer(line)]
    ends = starts[1:] + [len(line)]
    return [line[start:end] for start, end in zip(starts, ends)]


def tag_of(chunk: str) -> str:
    match = TAG_RE.match(chunk)
    return match.group(0) if match else ""


def required_width(chunks: list[str]) -> int:
    width = 0
    for position, chunk in enumerate(chunks, start=1):
        if tag_of(chunk) == CELL:
            width = position
    return width


def target_width(rows: list[list[str]]) -> int:
    lower = max(required_width(row) for row in rows)
    upper = max(len(row) for row in rows)
    best_width = upper
    best_cost = None
    for width in range(lower, upper + 1):
        cost = sum(abs(len(row) - width) for row in rows)
        if best_cost is None or cost < best_cost:
            best_width = width
            best_cost = cost
    return best_width


def pad_rows(otsl: str) -> list[list[str]]:
    text = otsl.strip()
    if ROW_BREAK not in text:
        rows = [split_chunks(text)] if split_chunks(text) else []
    else:
        rows = [split_chunks(line) for line in text.split(ROW_BREAK)]
        rows = [row for row in rows if row]
    if not rows:
        return []
    width = target_width(rows)
    return [
        list(row[:width]) + [EMPTY] * (width - len(row)) for row in rows
    ]


def resolve_cells(rows: list[list[str]]) -> list[dict]:
    """Turn origin tags into cells, measuring spans from the continuation tags."""
    height = len(rows)
    cells = []
    for row_index, tags in enumerate(rows):
        for column, chunk in enumerate(tags):
            tag = tag_of(chunk)
            if tag not in ORIGIN_TAGS:
                continue
            colspan = 1
            while column + colspan < len(tags) and tag_of(tags[column + colspan]) in RIGHT_CONTINUATION:
                colspan += 1
            rowspan = 1
            while (
                row_index + rowspan < height
                and column < len(rows[row_index + rowspan])
                and tag_of(rows[row_index + rowspan][column]) in DOWN_CONTINUATION
            ):
                rowspan += 1
            text = ""
            if tag == CELL:
                text = chunk[len(tag):].strip()
            cells.append(
                {"text": text, "row": row_index, "column": column,
                 "rowspan": rowspan, "colspan": colspan}
            )
    return cells


def markdown_cell(text: str) -> str:
    """Make a cell safe inside a markdown pipe table."""
    return text.replace("|", "\\|").replace("\r", " ").replace("\n", " ")


def otsl_to_markdown(otsl: str) -> str:
    """Render an OTSL table as a markdown pipe table."""
    rows = pad_rows(otsl)
    if not rows:
        return ""
    cells = resolve_cells(rows)
    num_rows = len(rows)
    num_cols = max(len(row) for row in rows)

    grid: list[list[dict | None]] = [[None] * num_cols for _ in range(num_rows)]
    for cell in cells:
        for row in range(cell["row"], min(cell["row"] + cell["rowspan"], num_rows)):
            for column in range(cell["column"], min(cell["column"] + cell["colspan"], num_cols)):
                grid[row][column] = cell

    lines = []
    for row in range(num_rows):
        values = []
        for column in range(num_cols):
            cell = grid[row][column]
            values.append(markdown_cell(cell["text"]) if cell else "")
        lines.append("| " + " | ".join(values) + " |")
        if row == 0:
            lines.append("| " + " | ".join(["---"] * num_cols) + " |")
    return "\n".join(lines)


def split_markdown_blocks(markdown: str):
    parts = re.split(r"(\n\s*\n)", markdown)
    blocks = []
    for index in range(0, len(parts), 2):
        text = parts[index]
        separator = parts[index + 1] if index + 1 < len(parts) else ""
        blocks.append((text, separator))
    return blocks


def postprocess_otsl_tables_markdown(markdown: str) -> str:
    """Convert OTSL-looking markdown blocks to markdown pipe tables."""
    if not markdown or not OTSL_TAG_RE.search(markdown):
        return markdown
    blocks = split_markdown_blocks(markdown)
    in_fence = False
    converted = []
    for text, separator in blocks:
        stripped = text.strip()
        fence_count = stripped.count("```")
        should_skip = in_fence
        if fence_count % 2 == 1:
            in_fence = not in_fence
            should_skip = True
        if not should_skip and OTSL_TAG_RE.search(stripped):
            table = otsl_to_markdown(stripped)
            if table:
                text = table
        converted.append(text + separator)
    return "".join(converted)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vendor", required=True, type=Path, help="Xiaomi-OCR-0 checkout")
    parser.add_argument("--raw", required=True, type=Path, help="directory of raw model answers")
    parser.add_argument("--out", required=True, type=Path, help="output directory")
    args = parser.parse_args()

    sys.path.insert(0, str(args.vendor))
    from postprocess.document import (  # noqa: PLC0415
        _clean_truncated_repeats,
        postprocess_math_delimiters,
    )

    written = 0
    for raw_path in sorted(args.raw.rglob("*.md")):
        answer = raw_path.read_text(encoding="utf-8")
        text = postprocess_math_delimiters(_clean_truncated_repeats(answer))
        text = postprocess_otsl_tables_markdown(text)
        out_path = args.out / raw_path.relative_to(args.raw)
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(text, encoding="utf-8")
        written += 1
    print(f"wrote {written} markdown-format pages to {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
