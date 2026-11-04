#!/usr/bin/env python3
"""Spot-check ten structural gold entries against the PDF text they claim.

The structural set's gold comes from PDF bookmarks, and a bookmark can be wrong:
it can point at a table-of-contents page, at a chapter opener with no title
text, or at the wrong page after an editorial reflow. This script takes ten
queries at random (fixed seed), reads the grade-2 gold page with
`pdftotext -layout`, and asks whether the section's own heading is actually
there.

A check passes when the heading's number (for numbered headings) or at least
60% of its content words appear on the page. The verdict, the page and the
matching text are printed and written to `spot-check.txt` next to the set, so
the check is evidence a reader can re-run rather than a claim in a report.

Usage (from the repo root):

    python3 rust/crates/kb-eval/scripts/spot_check_structural.py
"""

from __future__ import annotations

import argparse
import json
import random
import re
import subprocess
from pathlib import Path

SEED = 42
SAMPLES = 10
STOPWORDS = set(
    """a an and are as at be by for from has have in into is it its of on or that the
    their this to was were what when where which who will with""".split()
)
DOTTED = re.compile(r"^(\d+(?:\.\d+)+)\.?\s+")
CHAPTER = re.compile(r"^Chapter\s+(\d+)\b", re.I)


def words(text: str) -> list[str]:
    return [word for word in re.findall(r"[a-z0-9]+", text.lower())
            if word not in STOPWORDS and len(word) > 1]


def page_text(pdf: Path, page: int) -> str:
    proc = subprocess.run(
        ["pdftotext", "-layout", "-f", str(page), "-l", str(page), str(pdf), "-"],
        capture_output=True, text=True)
    return proc.stdout if proc.returncode == 0 else ""


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--set", type=Path,
                        default=Path("rust/crates/kb-eval/sets/structural"))
    args = parser.parse_args()

    meta = [json.loads(line) for line in
            (args.set / "query-meta.jsonl").read_text(encoding="utf-8").splitlines()
            if line.strip()]
    meta.sort(key=lambda item: item["_id"])
    rng = random.Random(SEED)
    sample = rng.sample(meta, min(SAMPLES, len(meta)))

    lines = ["# Structural set spot-check", "",
             f"Seed {SEED}, {len(sample)} of {len(meta)} queries, grade-2 (heading) page.",
             "PASS = the heading number is on the page, or >=60% of its content words are.", ""]
    passed = 0
    for item in sample:
        pdf = args.set / "corpus" / f"{item['document']}.pdf"
        text = page_text(pdf, item["page"])
        flat = " ".join(text.split())
        lower = flat.lower()
        title = item["title"].strip()
        dotted = DOTTED.match(title)
        chapter = CHAPTER.match(title)
        title_words = words(re.sub(r"^\d+(?:\.\d+)*\.?\s+", "", title))
        matched = [word for word in title_words if word in lower]
        ratio = len(matched) / len(title_words) if title_words else 0.0
        number_ok = False
        if dotted:
            number_ok = f" {dotted.group(1)} " in f" {flat} " or f"{dotted.group(1)} " in flat
        elif chapter:
            number_ok = f"chapter {chapter.group(1)}" in lower
        ok = number_ok or ratio >= 0.6
        passed += bool(ok)
        # Evidence: the line with the most matched words, or the page opening.
        best = flat[:220]
        best_score = 0
        for line in text.splitlines():
            line = line.strip()
            if not line:
                continue
            score = sum(1 for word in title_words if word in line.lower())
            if score > best_score:
                best, best_score = line, score
        lines.append(
            f"{'PASS' if ok else 'FAIL'} {item['_id']} ({item['template']}, "
            f"{item['document']}#p{item['page']}) number={number_ok} "
            f"words={len(matched)}/{len(title_words)}")
        lines.append(f"  heading: {title}")
        lines.append(f"  page:    {best[:200]}")
        lines.append("")

    lines.append(f"**{passed}/{len(sample)} passed.**")
    output = "\n".join(lines) + "\n"
    (args.set / "spot-check.txt").write_text(output, encoding="utf-8")
    print(output)
    return 0 if passed == len(sample) else 1


if __name__ == "__main__":
    raise SystemExit(main())
