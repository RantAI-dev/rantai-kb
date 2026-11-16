#!/usr/bin/env python3
"""Regenerate the Xiaomi-OCR-0 golden fixtures for the kb-core parity test.

The golden files in ``rust/crates/kb-core/tests/fixtures/xiaomi_ocr`` are the
vendor's own output: this script runs the postprocessing of
``xiaomi-research/Xiaomi-OCR-0`` over real model answers and writes what it
produced. The Rust port in ``kb-core`` must reproduce those bytes.

The vendor checkout is not part of this repository, so this script is a
one-off tool: it needs a path to a Xiaomi-OCR-0 checkout and to a directory
of raw model answers. It is kept in the tree so the fixtures can be
regenerated, and so the provenance of every committed golden file is clear.

Usage::

    python3 -I rust/scripts/xiaomi-ocr-fixtures.py \
        --vendor /path/to/Xiaomi-OCR-0 \
        --raw /path/to/raw \
        --final /path/to/final \
        --regions /path/to/trigout_results.jsonl \
        --out rust/crates/kb-core/tests/fixtures/xiaomi_ocr

``--raw`` holds the model's answers (``<category>/<name>.md``); ``--final``
holds the vendor's postprocessed answers for the same pages and is used only
to double-check that the vendor code in the checkout still reproduces them.
``--regions`` is a JSONL file of layout regions with the merged markdown the
vendor produced for each.

The vendor's region merge asks ``wordfreq`` whether a word split across two
blocks is a real word. That package is not installed in the deployment, so the
path that runs — and the one the fixtures were made with — is the fallback that
only looks at the shape of the word. Keep it that way when regenerating:
install nothing, or the expectations move.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

E2E_PAGES_PER_CATEGORY = 6
REGION_PAGES = 12


def spread(items: list, wanted: int) -> list:
    """Pick ``wanted`` items spread evenly over ``items`` (already sorted)."""
    if wanted >= len(items):
        return list(items)
    if wanted <= 0:
        return []
    step = len(items) / wanted
    return [items[int(index * step)] for index in range(wanted)]


def load_vendor(vendor: Path):
    sys.path.insert(0, str(vendor))
    from postprocess.document import finalize_document  # noqa: PLC0415
    from postprocess.region import merge_page_to_markdown  # noqa: PLC0415

    return finalize_document, merge_page_to_markdown


def e2e_fixtures(raw: Path, final: Path, out: Path, finalize) -> list[str]:
    written = []
    e2e_dir = out / "e2e"
    e2e_dir.mkdir(parents=True, exist_ok=True)
    for old in e2e_dir.glob("*.md"):
        old.unlink()

    categories = sorted(path for path in raw.iterdir() if path.is_dir())
    for category in categories:
        pages = sorted(category.glob("*.md"))
        chosen = spread(pages, E2E_PAGES_PER_CATEGORY)
        # The longest answer is the one that exercises the repeated-tail
        # cleanup; it is never chosen by the even spread, so add it.
        longest = max(pages, key=lambda path: path.stat().st_size)
        if longest not in chosen:
            chosen.append(longest)
        for page in sorted(chosen):
            slug = f"{category.name}__{page.stem}"
            answer = page.read_text(encoding="utf-8")
            expected = finalize(answer)
            reference = final / category.name / page.name
            if reference.exists():
                if reference.read_text(encoding="utf-8") != expected:
                    raise SystemExit(
                        f"vendor checkout does not reproduce {reference}; "
                        "the checkout and the fixtures are out of step"
                    )
            (e2e_dir / f"{slug}.raw.md").write_text(answer, encoding="utf-8")
            (e2e_dir / f"{slug}.expected.md").write_text(expected, encoding="utf-8")
            written.append(slug)
    return written


def region_fixtures(regions: Path, out: Path, merge) -> list[str]:
    written = []
    region_dir = out / "regions"
    region_dir.mkdir(parents=True, exist_ok=True)
    for old in region_dir.glob("*.json"):
        old.unlink()

    records = [json.loads(line) for line in regions.read_text(encoding="utf-8").splitlines() if line.strip()]
    records.sort(key=lambda record: record["image_path"])
    for record in spread(records, REGION_PAGES):
        slug = Path(record["image_path"]).stem.replace("__", "_")
        expected = merge(record["layout_result"])
        # Only the fields the merge reads are kept: the score and the task type
        # are the detector's business, not the merge's.
        trimmed = [
            {
                "index": region["index"],
                "label": region["label"],
                "bbox_2d": region["bbox_2d"],
                "merged": region.get("merged", ""),
            }
            for region in record["layout_result"]
        ]
        payload = {"regions": trimmed, "expected": expected}
        (region_dir / f"{slug}.json").write_text(
            json.dumps(payload, ensure_ascii=False, indent=1), encoding="utf-8"
        )
        written.append(slug)
    return written


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vendor", required=True, type=Path, help="Xiaomi-OCR-0 checkout")
    parser.add_argument("--raw", required=True, type=Path, help="directory of raw model answers")
    parser.add_argument("--final", required=True, type=Path, help="vendor postprocessed answers")
    parser.add_argument("--regions", required=True, type=Path, help="region JSONL")
    parser.add_argument("--out", required=True, type=Path, help="fixture directory")
    args = parser.parse_args()

    finalize, merge = load_vendor(args.vendor)
    args.out.mkdir(parents=True, exist_ok=True)

    e2e = e2e_fixtures(args.raw, args.final, args.out, finalize)
    regions = region_fixtures(args.regions, args.out, merge)
    print(f"wrote {len(e2e)} e2e fixtures and {len(regions)} region fixtures to {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
