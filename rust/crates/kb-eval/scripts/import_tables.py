#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["openpyxl>=3.1", "python-docx>=1.1"]
# ///
"""Build `tables-v1`: can a table be found and then answered exactly? (TBL-1)

Public tables in, questions with computed gold answers out. The point is to
grade the Rust extractor, the manifest and DuckDB, so **nothing here shares
code with them**: the grids are read with openpyxl / csv / python-docx, the
header and data region are found by the heuristics below, and every gold answer
is arithmetic over those rows. A broken reader cannot hide behind the code it
grades.

Sources (all public, licences recorded in `sources.jsonl`):

* `wb-*`   World Bank Open Data indicator CSVs (CC BY 4.0): an English table
           with a country code column and one column per year.
* `klaten-*` Datasets from the Satu Data Kabupaten Klaten CKAN portal
           (`opendata.klaten.go.id`, CC BY 4.0): BPS Klaten tables in
           Indonesian, with `Jumlah` subtotal rows, kode wilayah and 18-digit
           NIP columns, multi-level headers and year columns.
* `docx-*`  Five DOCX documents generated from those spreadsheets with
           python-docx (`generated: true`), so DOCX table extraction is in the
           same eval.

Questions: seven templates (cell lookup, filtered sum, count, top-k, group max,
the subtotal trap, the code trap), Indonesian and English, roughly half of them
in both languages. `--self-check` re-reads every source file and runs the gold
`TableQuery` through a small reference executor, so a gold answer that does not
match its own grid can never be committed.

    uv run import_tables.py --out ~/kbdata/tables-v1
    uv run import_tables.py --out ~/kbdata/tables-v1 --self-check
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import io
import json
import math
import os
import random
import re
import shutil
import sys
import unicodedata
import urllib.parse
import urllib.request
import zipfile
from pathlib import Path

SEED = 20261114
UA = {"User-Agent": "rantai-kb-tables-v1/1.0 (eval set; contact: kb@rantai.dev)"}

# ---------------------------------------------------------------------------
# Sources
# ---------------------------------------------------------------------------

# World Bank indicators: (code, Indonesian label, English label, topic).
# A fixed list, not a search, so a rebuild fetches the same eight tables.
WORLD_BANK = [
    ("SP.POP.TOTL", "jumlah penduduk", "population", "Demografi"),
    ("NY.GDP.MKTP.CD", "produk domestik bruto (USD berlaku)", "GDP (current US$)", "Ekonomi"),
    ("SL.UEM.TOTL.ZS", "tingkat pengangguran (% angkatan kerja)", "unemployment (% of labour force)", "Ketenagakerjaan"),
    ("IT.NET.USER.ZS", "pengguna internet (% penduduk)", "internet users (% of population)", "Teknologi"),
    ("AG.LND.FRST.ZS", "luas hutan (% luas daratan)", "forest area (% of land area)", "Lingkungan"),
    ("SP.DYN.LE00.IN", "angka harapan hidup saat lahir", "life expectancy at birth", "Kesehatan"),
    ("FP.CPI.TOTL.ZG", "inflasi harga konsumen (tahunan %)", "consumer price inflation (annual %)", "Ekonomi"),
    ("SP.URB.TOTL.IN.ZS", "penduduk perkotaan (% penduduk)", "urban population (% of population)", "Demografi"),
]

# Klaten datasets, pinned by title + resource URL: the CKAN search index is not
# a stable identifier and a rebuild must fetch the same bytes. Every one is
# CC BY 4.0; `sources.jsonl` records the dataset page.
KLATEN = [
    {
        "key": "klaten-tenaga-kesehatan",
        "title": "Jumlah Tenaga Kesehatan Menurut Kecamatan di Kabupaten Klaten, 2022",
        "dataset": "https://opendata.klaten.go.id/dataset/5c452894-4011-495b-a771-f789b20f368b",
        "url": "https://opendata.klaten.go.id/dataset/5c452894-4011-495b-a771-f789b20f368b/resource/e0490f60-b903-42a0-a5aa-6e243fea8f3a/download/jumlah-tenaga-kesehatan-menurut-kecamatan-di-kabupaten-klaten-2022.xlsx",
    },
    {
        "key": "klaten-kelahiran-hidup",
        "title": "Jumlah Kelahiran Hidup per Kecamatan Tahun 2024",
        "dataset": "https://opendata.klaten.go.id/dataset/ab2bca7d-d057-4d3a-a6ce-4232a8b59d5d",
        "url": "https://opendata.klaten.go.id/dataset/ab2bca7d-d057-4d3a-a6ce-4232a8b59d5d/resource/ff9224bd-ea33-47df-8871-0b1057b3e709/download/jumlah-kelahiran-hidup-per-kecamatan-tahun-2024.xlsx",
    },
    {
        "key": "klaten-penduduk-umur",
        "title": "Jumlah Penduduk Kabupaten Klaten Menurut Kelompok Umur Tahun 2022",
        "dataset": "https://opendata.klaten.go.id/dataset/3f05fb30-3ebc-4aa3-b15f-d6dc264c5cf8",
        "url": "https://opendata.klaten.go.id/dataset/3f05fb30-3ebc-4aa3-b15f-d6dc264c5cf8/resource/c6cef1bc-6d9e-4a42-8385-2ca8a1548874/download/jumlah-penduduk-kabupaten-klaten-menurut-kelompok-umur-tahun-2022.xlsx",
    },
    {
        "key": "klaten-penduduk-usia",
        "title": "Jumlah Penduduk Menurut Usia",
        "dataset": "https://opendata.klaten.go.id/dataset/f852c6b5-2ac4-4f0a-bc6b-e8bc2e57de4a",
        "url": "https://opendata.klaten.go.id/dataset/f852c6b5-2ac4-4f0a-bc6b-e8bc2e57de4a/resource/726c1664-b8c5-44f6-a0ff-4c08f49b4c63/download/jumlah-penduduk-menurut-usia.xlsx",
    },
    {
        "key": "klaten-desa-rw-rt",
        "title": "Jumlah Desa, Kelurahan, RW, RT",
        "dataset": "https://opendata.klaten.go.id/dataset/0b784da0-4585-4083-b17a-dbe4456de6cd",
        "url": "https://opendata.klaten.go.id/dataset/0b784da0-4585-4083-b17a-dbe4456de6cd/resource/5778f4fa-4250-47c4-9fa7-86e28ca13a83/download/jumlah-desa-kelurahan-rw-rt.xlsx",
    },
    {
        "key": "klaten-aparat-desa",
        "title": "Jumlah Aparat Pemerintah Desa",
        "dataset": "https://opendata.klaten.go.id/dataset/cc0b3c7d-0e28-4f33-b797-8b2317f2e1c5",
        "url": "https://opendata.klaten.go.id/dataset/cc0b3c7d-0e28-4f33-b797-8b2317f2e1c5/resource/48fb56e1-da87-47fe-a5db-ff4ec6c6f9e5/download/jumlah-aparat-pemerintah-desa.xlsx",
    },
    {
        "key": "klaten-sekolah-tk",
        "title": "Sekolah Taman Kanak-Kanak",
        "dataset": "https://opendata.klaten.go.id/dataset/b01be655-0741-4ba0-b643-26d34ed22646",
        "url": "https://opendata.klaten.go.id/dataset/b01be655-0741-4ba0-b643-26d34ed22646/resource/d0b3818b-2aac-47bd-bf1c-bc099d1565df/download/sekolah-taman-kanak-kanak.xlsx",
    },
    {
        "key": "klaten-puskesmas",
        "title": "Puskesmas, Pustu, Poliklinik, Posyandu, Pos Kesehatan Desa",
        "dataset": "https://opendata.klaten.go.id/dataset/014025e2-55ef-42e3-893b-9fe093e95a0d",
        "url": "https://opendata.klaten.go.id/dataset/014025e2-55ef-42e3-893b-9fe093e95a0d/resource/577d0cb1-07a2-44be-a3dd-969f044adc5e/download/puskesmaspustupoliklinikposyandupos-kesehatan-desa.xlsx",
    },
    {
        "key": "klaten-kangkung-tomat",
        "title": "Luas Panen, Rata - rata Produksi , Produksi Kangkung dan Tomat Menurut Kecamatan di Kabupaten Klaten Tahun 2022",
        "dataset": "https://opendata.klaten.go.id/dataset/f1901d83-1730-40fc-8b8a-ae08f9fe63f2",
        "url": "https://opendata.klaten.go.id/dataset/f1901d83-1730-40fc-8b8a-ae08f9fe63f2/resource/f72d0a5c-a4f9-4d59-96cd-dde64a2fe634/download/2022_kangkung-dan-tomat.xlsx",
    },
    {
        "key": "klaten-kobis-bayam",
        "title": "Luas Panen, Rata - rata Produksi , Produksi Kobis dan Bayam Di Kabupaten Klaten",
        "dataset": "https://opendata.klaten.go.id/dataset/d8e9187b-1ba8-4f73-8b7e-42eaa82d1d15",
        "url": "https://opendata.klaten.go.id/dataset/d8e9187b-1ba8-4f73-8b7e-42eaa82d1d15/resource/5bf0bb69-2e99-42b1-9f57-f4abc9accffd/download/tabel-25.10.xlsx",
    },
    {
        "key": "klaten-perkebunan-petani",
        "title": "Luas Areal, Produksi, Produktivitas dan Jumlah Petani Perkebunan Rakyat",
        "dataset": "https://opendata.klaten.go.id/dataset/a7971324-1b9d-4501-843b-bae7950a668b",
        "url": "https://opendata.klaten.go.id/dataset/a7971324-1b9d-4501-843b-bae7950a668b/resource/f8dddf98-21da-4b4e-8981-91a3eb12b2ac/download/tabel-25.39.xlsx",
    },
    {
        "key": "klaten-eselon-profil",
        "title": "Profil Eselon III dan IV Kec Wonosari",
        "dataset": "https://opendata.klaten.go.id/dataset/d4dd5374-b29c-400a-9c16-343a9ec2576d",
        "url": "https://opendata.klaten.go.id/dataset/d4dd5374-b29c-400a-9c16-343a9ec2576d/resource/41a4b45e-4a67-4a13-b128-ab60bc1c9899/download/profil-eselon-iii-dan-iv.xlsx",
    },
    {
        "key": "klaten-uji-kendaraan",
        "title": "Data Uji Kendaraan Bermotor di Kabupaten Klaten Tahun 2018-2022",
        "dataset": "https://opendata.klaten.go.id/dataset/7cf01b5a-8ce4-4205-a30c-888820dcbbec",
        "url": "https://opendata.klaten.go.id/dataset/7cf01b5a-8ce4-4205-a30c-888820dcbbec/resource/dfb4ce89-fa76-4c80-8ec6-ad4b32653f20/download/data-uji-kendaraan-bermotor-di-kabupaten-klaten-tahun-2018-2022.xlsx",
    },
    {
        "key": "klaten-kendaraan-bulanan",
        "title": "Kendaraan Bermotor Yang Diuji Menurut Bulan dan jenisnya di Kabupaten Klaten, 2023",
        "dataset": "https://opendata.klaten.go.id/dataset/09703091-f18e-48bc-8097-5efcdd3f206c",
        "url": "https://opendata.klaten.go.id/dataset/09703091-f18e-48bc-8097-5efcdd3f206c/resource/18f9bc21-d904-412f-9b61-2223f9389eb7/download/8.1.6-kendaraan-bermotor-yang-diuji-menurut-bulan-dan-jenisnya-di-kabupaten-klaten-2023.xlsx",
    },
    {
        "key": "klaten-umkm",
        "title": "Jumlah UMKM Menurut Bidang Usaha",
        "dataset": "https://opendata.klaten.go.id/dataset/95e802dd-cee2-4a2f-a753-ea324e1415dc",
        "url": "https://opendata.klaten.go.id/dataset/95e802dd-cee2-4a2f-a753-ea324e1415dc/resource/dd3a73d7-f929-4623-b37a-e7f4a2683c6c/download/jumlah-umkm-menurut-bidang-usaha.xlsx",
    },
    {
        "key": "klaten-rasio-ketergantungan",
        "title": "Rasio Ketergantungan Penduduk Kabupaten Klaten",
        "dataset": "https://opendata.klaten.go.id/dataset/c7ef2012-f46b-44d2-9c82-5c63370f2cce",
        "url": "https://opendata.klaten.go.id/dataset/c7ef2012-f46b-44d2-9c82-5c63370f2cce/resource/57109593-ca1c-46b6-a851-1e74fb787c8b/download/rasio-ketergantungan-penduduk-kabupaten-klaten-tahun-2022.xlsx",
    },
    {
        "key": "klaten-pasar-jenis",
        "title": "Pasar Menurut Jenisnya Th. 2018-2020",
        "dataset": "https://opendata.klaten.go.id/dataset/955a6a95-5edd-423a-9e39-f1c96cfadefd",
        "url": "https://opendata.klaten.go.id/dataset/955a6a95-5edd-423a-9e39-f1c96cfadefd/resource/8ae3b4d5-f9d3-4b79-b963-9593bb12457c/download/pasar-menurut-jenisnya-th.-2018-2020.xlsx",
    },
]

# ---------------------------------------------------------------------------
# Numbers and cells
# ---------------------------------------------------------------------------

_CURRENCY = r"(?:rp\.?|idr|us\$|usd|\$|€|£)"
_SUFFIX = {
    "": 1.0,
    "%": 1.0,
    "persen": 1.0,
    "juta": 1e6,
    "jt": 1e6,
    "miliar": 1e9,
    "milyar": 1e9,
    "m": 1e9,  # DJPK writes "661.665,87 M"
    "miliar rupiah": 1e9,
    "ribu": 1e3,
    "rb": 1e3,
    "k": 1e3,
    "triliun": 1e12,
    "tn": 1e12,
    "miliar us$": 1e9,
}
_NUM_BODY = re.compile(r"^[+-]?\d[\d.,\s]*$")


def parse_number(raw) -> float | None:
    """A cell value as a number, or None.

    Locale-aware the way the spec asks the extractor to be: with both `.` and
    `,`, the rightmost separator is the decimal one (`1.234,5` → 1234.5,
    `1,234.5` → 1234.5); a lone separator repeated in groups of three is a
    thousands separator (`1.234.567`); a lone separator with one or two digits
    after it is a decimal (`49,82` → 49.82, `2.5` → 2.5). Percent, rupiah and
    juta/miliar suffixes are understood because the Klaten and DJPK tables use
    them.
    """
    if raw is None or isinstance(raw, bool):
        return None
    if isinstance(raw, (int, float)):
        value = float(raw)
        return None if math.isnan(value) or math.isinf(value) else value
    s = unicodedata.normalize("NFKC", str(raw)).strip()
    if not s:
        return None
    s = s.replace("\u00a0", " ").replace("\u2212", "-")
    neg = s.startswith("(") and s.endswith(")")
    if neg:
        s = s[1:-1].strip()
    s = re.sub(r"^(?:" + _CURRENCY + r")\s*", "", s, flags=re.I).strip()
    m = re.match(r"^([+-]?\d[\d.,\s]*)\s*([A-Za-z%]{0,14})?$", s)
    if not m:
        return None
    body, suffix = m.group(1).strip(), (m.group(2) or "").strip().lower()
    body = body.replace(" ", "")
    if not _NUM_BODY.match(body):
        return None
    has_dot, has_comma = "." in body, "," in body
    if has_dot and has_comma:
        if body.rfind(",") > body.rfind("."):
            body = body.replace(".", "").replace(",", ".")
        else:
            body = body.replace(",", "")
    elif has_dot:
        if re.fullmatch(r"[+-]?\d{1,3}(?:\.\d{3})+", body):
            body = body.replace(".", "")
    elif has_comma:
        if re.fullmatch(r"[+-]?\d{1,3}(?:,\d{3})+", body):
            body = body.replace(",", "")
        else:
            body = body.replace(",", ".")
    try:
        value = float(body)
    except ValueError:
        return None
    if suffix:
        if suffix not in _SUFFIX:
            return None
        value *= _SUFFIX[suffix]
    if neg:
        value = -value
    return value


def is_year(value) -> bool:
    """A year as a label (a column named 2023, a cell holding 2023)."""
    if isinstance(value, bool) or value is None:
        return False
    if isinstance(value, int):
        return 1900 <= value <= 2100
    if isinstance(value, float):
        return value.is_integer() and 1900 <= value <= 2100
    return bool(re.fullmatch(r"(?:19|20)\d\d", str(value).strip()))


def is_labelish(value) -> bool:
    """Text that can be a header: not a number, except a year."""
    if value is None:
        return False
    if is_year(value):
        return True
    if isinstance(value, bool):
        return False
    if isinstance(value, (int, float)):
        return False
    s = str(value).strip()
    return bool(s) and parse_number(s) is None


def cell(raw):
    """One grid cell: a number, a string, or empty (None)."""
    if raw is None:
        return None
    if isinstance(raw, bool):
        return raw
    if isinstance(raw, (int, float)):
        value = float(raw)
        if math.isnan(value) or math.isinf(value):
            return None
        if float(value).is_integer():
            # An 18-digit NIP arrives as a float from openpyxl; it is a code,
            # never a quantity, so keep every digit as text.
            return int(value) if abs(value) < 1e15 else str(int(value))
        return value
    s = unicodedata.normalize("NFC", str(raw)).strip()
    if not s:
        return None
    # Placeholders for "no value": a dash is missing data, never a label.
    if s.casefold() in {"-", "–", "—", "na", "n/a", "n.a.", "...", "..", ":", "td"}:
        return None
    # Leading-zero identifiers and long identifier-like digit strings (NIP is 18
    # digits, NIK 16) are codes: a float would eat the zero or the low digits.
    if re.fullmatch(r"0\d{5,}", s) or re.fullmatch(r"\d{16,}", s):
        return s
    number = parse_number(s)
    if number is not None:
        return int(number) if float(number).is_integer() and abs(number) < 1e15 else number
    return s


def _clean_name(text: str) -> str:
    text = unicodedata.normalize("NFC", str(text)).replace("\n", " ")
    return re.sub(r"\s+", " ", text).strip()


def column_name(parts: list[str]) -> str:
    """Flatten a multi-level header the way the spec says: `A / B`."""
    seen: list[str] = []
    for part in parts:
        part = _clean_name(part)
        if part and (not seen or seen[-1] != part):
            seen.append(part)
    return " / ".join(seen)


def unique_names(names: list[str]) -> list[str]:
    """Header names unique within a table: duplicates get ` (2)`, ` (3)`."""
    out: list[str] = []
    counts: dict[str, int] = {}
    for name in names:
        base = name or "kolom"
        counts[base] = counts.get(base, 0) + 1
        out.append(base if counts[base] == 1 else f"{base} ({counts[base]})")
    return out


# ---------------------------------------------------------------------------
# Reading a grid: header, data region, derived rows
# ---------------------------------------------------------------------------

FOOTNOTE = re.compile(
    r"^(sumber|source|catatan|keterangan|note|ket\b|diakses|data diolah|angka sementara|"
    r"angka estimasi|keterangan:|sumber/|source/|\*|•)",
    re.I,
)
SUBTOTAL_LABEL = re.compile(
    r"^(?:jumlah(\s+(?:total|keseluruhan|seluruh))?|total|sub\s*total|subtotal|"
    r"grand\s*total|grandtotal|rata-rata|average)$",
    re.I,
)
SERIAL_HEADER = re.compile(r"^(no\.?|nomor|nr\.?|#)$", re.I)


def _row_nonempty(row) -> list:
    return [c for c in row if c is not None and str(c).strip() != ""]


def _numeric(row) -> int:
    return sum(1 for c in row if c is not None and parse_number(c) is not None)


def _texty(row) -> int:
    return sum(1 for c in row if is_labelish(c))


def _numbering_row(row) -> bool:
    """The `1 2 3 4 5` or `(1) (2) (3)` column-number row under a BSE header."""
    values = []
    for raw in row:
        if raw is None or str(raw).strip() == "":
            continue
        text = str(raw).strip()
        m = re.fullmatch(r"\(?(\d{1,3})\)?", text)
        if not m:
            return False
        values.append(int(m.group(1)))
    if len(values) < 3:
        return False
    return values == sorted(values) and len(set(values)) == len(values) and values[-1] >= 4


def find_header(rows: list[list], limit: int = 25) -> tuple[int, int] | None:
    """`(header_row, header_rows)` for the best header in the first rows.

    A header row is mostly labels (a year counts as a label), has at least two
    columns, and within the next four rows there is one with at least two
    numbers. Among candidates the one with the most labelled columns wins, so a
    two-cell title above a wide header loses to the header.
    """
    best: tuple[float, int, int] | None = None
    for h in range(min(limit, len(rows))):
        row = rows[h]
        nonempty = _row_nonempty(row)
        if len(nonempty) < 2 or _numbering_row(row):
            continue
        # A merged title row reads as the same text in every column after the
        # merge fill; it is a title, never the header.
        if len({_clean_name(v).lower() for v in nonempty}) == 1:
            continue
        if _texty(row) < max(2, int(0.6 * len(nonempty))):
            continue
        ahead = rows[h + 1 : h + 5]
        if not any(_numeric(r) >= 2 for r in ahead):
            continue
        levels = 1
        while h + levels < len(rows) and levels < 3:
            nxt = rows[h + levels]
            ne = _row_nonempty(nxt)
            if len(ne) < 1 or _numbering_row(nxt):
                break
            if _texty(nxt) < max(1, int(0.7 * len(ne))):
                break
            # Stop if the row after this one already looks like data: then this
            # row is the last header level, not a new one.
            after = rows[h + levels + 1] if h + levels + 1 < len(rows) else []
            if _numeric(nxt) >= 2 and _numeric(after) >= 2:
                break
            levels += 1
        score = 2.0 * len(nonempty) + _texty(row) - 0.6 * h
        if best is None or score > best[0]:
            best = (score, h, levels)
    if best is None:
        return None
    return best[1], best[2]


def _header_names(rows: list[list], h: int, levels: int, width: int) -> list[str]:
    names: list[str] = []
    for c in range(width):
        parts: list[str] = []
        for k in range(h, h + levels):
            if c < len(rows[k]) and rows[k][c] is not None:
                part = str(rows[k][c]).strip()
                if part and part not in (":", "-", "–"):
                    parts.append(part)
        names.append(column_name(parts) if parts else f"column_{c + 1}")
    return unique_names(names)


def _is_footnote(row: list) -> bool:
    for value in row:
        if value is None or str(value).strip() == "":
            continue
        return bool(FOOTNOTE.match(str(value).strip()))
    return False


def _repeats_header(row: list, header: list[str]) -> bool:
    values = {_clean_name(v).lower() for v in row if v is not None and str(v).strip()}
    names = {_clean_name(v).lower() for v in header if v}
    return len(values & names) >= max(2, len(names) // 2)


def read_rows(path: Path, sheet: str | None) -> tuple[list[list], list[int]]:
    """Raw rows of one sheet plus 0-based rows whose numeric cells are formulas.

    Merged cells are spread across their span (the extractor does the same), so
    a two-column header merge yields one name per column and a title row spans
    its whole width.
    """
    suffix = path.suffix.lower()
    if suffix == ".csv":
        text = path.read_text(encoding="utf-8-sig", errors="replace")
        sample = text[:8192]
        try:
            dialect = csv.Sniffer().sniff(sample, delimiters=",;\t|")
        except csv.Error:
            dialect = csv.excel
        rows = [list(r) for r in csv.reader(io.StringIO(text), dialect)]
        return rows, []
    if suffix == ".docx":
        import docx

        document = docx.Document(str(path))
        tables = list(document.tables)
        index = int(sheet) if sheet is not None and str(sheet).isdigit() else 0
        if index >= len(tables):
            return [], []
        return [[c.text for c in row.cells] for row in tables[index].rows], []
    import openpyxl

    wb = openpyxl.load_workbook(str(path), data_only=True, read_only=False)
    wb_formulas = openpyxl.load_workbook(str(path), data_only=False, read_only=False)
    ws = wb[sheet] if sheet else wb.worksheets[0]
    ws_f = wb_formulas[ws.title]
    if ws.sheet_state != "visible":
        return [], []
    rows: list[list] = [list(r) for r in ws.iter_rows(values_only=True)]
    # Spread merged cells: a merged title over A1:F1 has its text only in A1,
    # and a merged header over C3:D3 only in C3; both must read as if filled.
    for merged in ws.merged_cells.ranges:
        if merged.min_row > len(rows):
            continue
        top = rows[merged.min_row - 1][merged.min_col - 1]
        if top is None or str(top).strip() == "":
            continue
        for r in range(merged.min_row - 1, min(merged.max_row, len(rows))):
            for c in range(merged.min_col - 1, min(merged.max_col, len(rows[r]))):
                if rows[r][c] is None or str(rows[r][c]).strip() == "":
                    rows[r][c] = top
    formula_rows: list[int] = []
    for r, frow in enumerate(ws_f.iter_rows()):
        if any(
            isinstance(c.value, str) and c.value.startswith("=") and re.search(r"SUM|SUBTOTAL|AVERAGE", c.value, re.I)
            for c in frow
        ):
            formula_rows.append(r)
    return rows, formula_rows


def _trim(rows: list[list]) -> list[list]:
    """Drop fully empty leading/trailing rows and trailing empty columns."""
    start, end = 0, len(rows)
    while start < end and not _row_nonempty(rows[start]):
        start += 1
    while end > start and not _row_nonempty(rows[end - 1]):
        end -= 1
    rows = rows[start:end]
    width = 0
    for row in rows:
        for c, value in enumerate(row):
            if value is not None and str(value).strip() != "":
                width = max(width, c + 1)
    return [list(row[:width]) + [None] * (width - len(row)) for row in rows]


def build_grids(path: Path, sheet: str | None = None, hints: dict | None = None) -> list[dict]:
    """Every table region of a sheet as `{title, header, rows, derived_rows, ...}`.

    `hints` (curated, recorded in the manifest) may pin `header_row`,
    `header_rows`, `data_start` or `title` when a file defeats the heuristics.
    """
    hints = hints or {}
    rows, formula_rows = read_rows(path, sheet)
    if not rows:
        return []
    trimmed_from = 0
    original = rows
    rows = _trim(rows)
    while trimmed_from < len(original) and not _row_nonempty(original[trimmed_from]):
        trimmed_from += 1

    found = None
    if "header_row" in hints:
        found = (hints["header_row"], hints.get("header_rows", 1))
    else:
        found = find_header(rows)
    if found is None:
        return []
    h, levels = found
    if h >= len(rows):
        return []
    width = max(len(r) for r in rows)
    header = _header_names(rows, h, levels, width)
    if sum(1 for name in header if name) < 2:
        return []

    start = hints.get("data_start", h + levels)
    # Skip column-numbering rows directly under the header.
    while start < len(rows) and _numbering_row(rows[start]):
        start += 1

    data: list[list] = []
    derived: list[int] = []
    formula_derived: list[int] = []
    notes: list[str] = list(hints.get("notes") or [])
    blank = 0
    for r in range(start, len(rows)):
        row = list(rows[r]) + [None] * (width - len(rows[r]))
        if not _row_nonempty(row):
            blank += 1
            if blank >= 3:
                break
            continue
        blank = 0
        if _is_footnote(row):
            break
        if _repeats_header(row, header):
            continue
        values = [cell(v) for v in row]
        labelled = any(
            isinstance(value, str) and SUBTOTAL_LABEL.match(value.strip().rstrip(":")) for value in values
        )
        by_formula = (r + trimmed_from) in formula_rows
        data.append(values)
        if labelled or by_formula:
            derived.append(len(data) - 1)
            if by_formula:
                formula_derived.append(len(data) - 1)
    # Some BPS sheets stack a second table under the first (a per-year summary
    # below the per-kecamatan one) with no blank row between. When the first
    # text column turns into bare years for the rest of the sheet, that tail is
    # a different table and is dropped with a note.
    if data:
        text_columns = [
            c
            for c in range(len(header))
            if sum(1 for row in data if isinstance(row[c], str)) >= 0.8 * len(data)
        ]
        for c in text_columns:
            year_rows = [i for i, row in enumerate(data) if is_year(row[c])]
            if len(year_rows) >= 3 and year_rows[0] >= 3:
                tail = data[year_rows[0] :]
                if all(row[c] is None or is_year(row[c]) for row in tail):
                    data = data[: year_rows[0]]
                    notes.append("dropped a stacked year block below the table")
                    break
    # A trailing `Kabupaten …` row that sums every numeric column is a subtotal
    # even without a label. It is *marked* but never a trap: the manifest cannot
    # know it, so grading it would grade a guess. Grids that have one are left
    # out of aggregate templates (cell and code questions still run).
    data_sum_row = None
    if data and not derived:
        last = data[-1]
        numbers = [i for i, v in enumerate(last) if isinstance(v, (int, float))]
        if len(numbers) >= 2:
            ok = 0
            for c in numbers:
                column = [row[c] for row in data[:-1] if isinstance(row[c], (int, float))]
                if len(column) >= 2 and abs(sum(column) - last[c]) <= max(0.51, abs(last[c]) * 0.005):
                    ok += 1
            if ok == len(numbers) and len(data) > 3:
                data_sum_row = len(data) - 1
                derived.append(data_sum_row)
    title = hints.get("title")
    if not title:
        title = _title_above(rows, h) or (sheet or path.stem)
    unit = _unit_from_header(header) or _unit_column(data, header)
    profiles = column_profile(header, data)
    years = [
        p["name"]
        for p in profiles
        if p["role"] == "quantity" and re.fullmatch(r"(?:19|20)\d\d", p["name"].strip())
    ]
    preferred = None
    best_cover = 0.0
    for name in years:
        c = header.index(name)
        cover = sum(1 for row in data if isinstance(row[c], (int, float))) / max(1, len(data))
        if cover >= 0.7 and (cover, int(name)) >= (best_cover, int(name)):
            preferred, best_cover = name, cover
    return [
        {
            "file": path.name,
            "source_kind": path.suffix.lower().lstrip(".") or "file",
            "sheet": sheet,
            "title": _clean_name(title),
            "header": header,
            "header_rows": levels,
            "rows": data,
            "derived_rows": derived,
            "formula_rows": formula_derived,
            "data_sum_row": data_sum_row,
            "unit": unit,
            "year_columns": years,
            "preferred_quantity": preferred,
            "notes": notes,
        }
    ]


def _title_above(rows: list[list], h: int) -> str | None:
    """The nearest non-empty row above the header, de-duplicated, if not data."""
    for r in range(h - 1, -1, -1):
        values = []
        for v in rows[r]:
            text = _clean_name(v) if v is not None and str(v).strip() else ""
            if text and text not in values:
                values.append(text)
        if not values:
            continue
        joined = " ".join(values)
        if len(joined) > 160:
            joined = joined[:157] + "…"
        return joined
    return None


def _unit_from_header(header: list[str]) -> str | None:
    for name in header:
        m = re.search(r"\(([^()]{1,24})\)\s*$", name)
        if m and re.search(r"(rp|juta|miliar|ribu|%|persen|ton|kg|orang|jiwa|unit|ha|km|liter)", m.group(1), re.I):
            return m.group(1).strip()
    return None


def _unit_column(data: list[list], header: list[str]) -> str | None:
    """A `Satuan` column says the unit of the row, once."""
    for c, name in enumerate(header):
        if re.fullmatch(r"(satuan|unit|units)", name.strip(), re.I):
            values = [row[c] for row in data if isinstance(row[c], str) and row[c].strip()]
            if values:
                uniq = sorted({v.strip() for v in values})
                if len(uniq) <= 3:
                    return uniq[0]
    return None


# ---------------------------------------------------------------------------
# Column roles, the way the contract defines them, for choosing questions
# ---------------------------------------------------------------------------

CODE_HEADER = re.compile(r"(kode|code|nik|nip|npwp|telepon|telp|phone|hp|no\.?\s*(hp|telp|telpon))", re.I)
SYSTEM_HEADER = re.compile(r"^(no\.?|nomor|nr\.?|#|kode|code)$", re.I)


def _digits(value) -> str | None:
    if isinstance(value, int) and not isinstance(value, bool):
        text = str(value)
    elif isinstance(value, str):
        text = value.strip()
    else:
        return None
    return text if re.fullmatch(r"\d{4,20}", text) else None


def column_profile(header: list[str], rows: list[list]) -> list[dict]:
    profiles = []
    for c, name in enumerate(header):
        values = [row[c] for row in rows if c < len(row)]
        present = [v for v in values if v is not None and not (isinstance(v, str) and not v.strip())]
        numbers = [v for v in present if isinstance(v, (int, float)) and not isinstance(v, bool)]
        digit_strings = [d for d in (_digits(v) for v in present) if d]
        years = [v for v in present if is_year(v)]
        kind = "empty"
        if not present:
            kind = "empty"
        elif len(numbers) >= 0.8 * len(present):
            kind = "number"
        elif len(years) >= 0.8 * len(present):
            kind = "date"
        elif len(digit_strings) >= 0.8 * len(present):
            kind = "text"
        else:
            kind = "text"
        role = "label"
        if CODE_HEADER.search(name) and (digit_strings or (kind == "text" and len(present) >= 3)):
            role = "code"
        elif digit_strings and len(digit_strings) >= 0.8 * len(present) and len(present) >= 3:
            lengths = {len(d) for d in digit_strings}
            if any(d.startswith("0") for d in digit_strings) or (len(lengths) == 1 and min(lengths) >= 6):
                role = "code"
        if role != "code":
            if kind == "date" or (kind == "number" and years and len(years) >= 0.8 * len(present)):
                role = "date"
            elif kind == "number":
                role = "quantity"
            elif kind == "text":
                distinct = len({str(v) for v in present})
                if distinct >= max(2, int(0.7 * len(present))) and not SYSTEM_HEADER.match(name.strip()):
                    role = "identifier"
        if kind == "number" and role == "code":
            kind = "text"
        profiles.append(
            {
                "name": name,
                "kind": kind,
                "role": role,
                "distinct": len({str(v) for v in present}),
                "n": len(present),
                "sample": [str(v) for v in present[:10]],
            }
        )
    return profiles


# ---------------------------------------------------------------------------
# Download
# ---------------------------------------------------------------------------

def get(url: str, timeout: int = 90) -> bytes:
    request = urllib.request.Request(url, headers=UA)
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return response.read()


def fetch_worldbank(out: Path, code: str) -> tuple[Path, str]:
    """The indicator's bulk CSV, from the ZIP the World Bank serves."""
    url = f"https://api.worldbank.org/v2/en/indicator/{code}?downloadformat=csv"
    name = f"wb-{code}.csv"
    target = out / name
    if target.exists() and target.stat().st_size > 1000:
        return target, url
    blob = get(url, timeout=180)
    with zipfile.ZipFile(io.BytesIO(blob)) as archive:
        member = next(
            (m for m in archive.namelist() if m.startswith("API_") and m.endswith(".csv") and "Metadata" not in m),
            None,
        )
        if member is None:
            raise SystemExit(f"World Bank {code}: no API_*.csv in {url}")
        target.write_bytes(archive.read(member))
    return target, url


def fetch_klaten(out: Path, entry: dict) -> tuple[Path, str]:
    name = "klaten-" + entry["key"].removeprefix("klaten-") + ".xlsx"
    target = out / name
    url = entry["url"]
    if target.exists() and target.stat().st_size > 1000:
        return target, url
    target.write_bytes(get(url))
    return target, url


# ---------------------------------------------------------------------------
# Self-check: a reference executor over the grid
# ---------------------------------------------------------------------------


def execute(grid: dict, query: dict) -> dict:
    """Run a `TableQuery` over a grid the way the contract says it should run.

    Deliberately small and independent: filters, derived-row exclusion, group
    by, aggregates, order, limit. Used only by `--self-check`, never to compute
    a gold answer, so the check is a second path to the same number.
    """
    header = grid["header"]
    index = {name: i for i, name in enumerate(header)}
    derived = set(grid["derived_rows"])
    rows = [row for i, row in enumerate(grid["rows"]) if query.get("include_derived") or i not in derived]
    matched = len(rows)

    def value_of(row, column):
        return row[index[column]] if column in index and index[column] < len(row) else None

    def keep(row, f):
        column = f["column"]
        left = value_of(row, column)
        op, right = f["op"], f["value"]
        if op == "contains":
            return left is not None and str(right).lower() in str(left).lower()
        if isinstance(right, list) and op == "in":
            return any(_equals(left, v) for v in right)
        if op == "between":
            return len(right) == 2 and _compare(left, right[0]) >= 0 and _compare(left, right[1]) <= 0
        a, b = _compare(left, right), 0
        return {
            "=": a == 0,
            "!=": a != 0,
            ">": a > 0,
            ">=": a >= 0,
            "<": a < 0,
            "<=": a <= 0,
        }.get(op, False)

    for f in query.get("filter", []):
        rows = [row for row in rows if keep(row, f)]

    def aggregate(item, group):
        agg = item["agg"]
        column = item.get("column")
        if agg == "count":
            return len(group) if column is None else sum(1 for r in group if value_of(r, column) is not None)
        if agg == "count_distinct":
            return len({str(value_of(r, column)) for r in group if value_of(r, column) is not None})
        values = [value_of(r, column) for r in group]
        values = [v for v in values if isinstance(v, (int, float)) and not isinstance(v, bool)]
        if not values:
            return None
        if agg == "sum":
            return sum(values)
        if agg == "avg":
            return sum(values) / len(values)
        if agg == "min":
            return min(values)
        if agg == "max":
            return max(values)
        raise ValueError(agg)

    group_by = query.get("group_by", [])
    groups: list[tuple[list, list]] = []
    if group_by:
        buckets: dict[tuple, list] = {}
        order: list[tuple] = []
        for row in rows:
            key = tuple(value_of(row, c) for c in group_by)
            if key not in buckets:
                buckets[key] = []
                order.append(key)
            buckets[key].append(row)
        groups = [(list(key), buckets[key]) for key in order]
    elif any("agg" not in item for item in query.get("select", [])):
        # Plain columns select rows, not one aggregate: every matched row is
        # its own output row (this is the top-k and cell-lookup shape).
        groups = [([], [row]) for row in rows]
    else:
        groups = [([], rows)]

    columns: list[str] = []
    out: list[list] = []
    for item in query.get("select", []):
        if "agg" in item:
            alias = item.get("as") or (f"{item['agg']}_{item.get('column', 'rows')}")
        else:
            alias = item.get("as") or item["column"]
        if alias not in columns:
            columns.append(alias)
    for key, group in groups:
        record = []
        for item in query.get("select", []):
            if "agg" in item:
                record.append(aggregate(item, group))
            elif item["column"] in group_by:
                record.append(key[group_by.index(item["column"])])
            else:
                record.append(value_of(group[0], item["column"]) if group else None)
        out.append(record)

    for order in reversed(query.get("order_by", [])):
        if order["key"] not in columns:
            raise ValueError(f"order key {order['key']} is not selected")
        j = columns.index(order["key"])
        present = [r for r in out if r[j] is not None]
        missing = [r for r in out if r[j] is None]
        present.sort(key=lambda r: _sort_key(r[j]), reverse=order.get("dir", "asc") == "desc")
        out = present + missing
    limit = query.get("limit")
    truncated = bool(limit and len(out) > limit)
    if limit:
        out = out[:limit]
    return {
        "columns": columns,
        "rows": out,
        "rows_matched": matched,
        "excluded_derived_rows": 0 if query.get("include_derived") else sum(1 for i in range(len(grid["rows"])) if i in derived),
        "truncated": truncated,
    }


def _sort_key(value):
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return (0, float(value))
    return (1, str(value))


def _compare(left, right) -> int:
    if isinstance(left, (int, float)) and isinstance(right, (int, float)):
        return (left > right) - (left < right)
    ln, rn = parse_number(left), parse_number(right)
    if ln is not None and rn is not None:
        return (ln > rn) - (ln < rn)
    ls, rs = str(left).strip().lower(), str(right).strip().lower()
    return (ls > rs) - (ls < rs)


def _equals(left, right) -> bool:
    if isinstance(right, (int, float)) and not isinstance(right, bool):
        number = parse_number(left)
        return number is not None and abs(number - right) <= 1e-9
    return str(left).strip().lower() == str(right).strip().lower()


# ---------------------------------------------------------------------------
# Question generation
# ---------------------------------------------------------------------------

TEMPLATES = [
    "cell_lookup",
    "sum_filter",
    "count",
    "top_k",
    "group_max",
    "subtotal_trap",
    "code_trap",
]


def _plain(value) -> str:
    """A value for a generated DOCX: dot decimal, no thousands separator."""
    if isinstance(value, float) and value.is_integer():
        value = int(value)
    if isinstance(value, float):
        return f"{value:.6f}".rstrip("0").rstrip(".")
    return str(value)


def _fmt(value, lang: str = "id") -> str:
    """A number as a question would write it: `1.234,5` / `1,234.5`."""
    thousand, decimal = (".", ",") if lang == "id" else (",", ".")
    if isinstance(value, float) and value.is_integer():
        value = int(value)
    if isinstance(value, int):
        return f"{value:,}".replace(",", thousand)
    number = parse_number(value)
    if number is not None:
        text = f"{number:.6f}".rstrip("0").rstrip(".")
        return text.replace(",", "X").replace(".", thousand).replace("X", decimal)
    return str(value)


def _q(query: dict) -> dict:
    """A TableQuery with no empty optional members (the contract's defaults)."""
    out: dict = {}
    for key in ("select", "filter", "group_by", "order_by"):
        if query.get(key):
            out[key] = query[key]
    if query.get("limit"):
        out["limit"] = query["limit"]
    if query.get("include_derived"):
        out["include_derived"] = True
    return out


def _select_column(column: str) -> dict:
    return {"column": column}


def _select_agg(agg: str, column: str | None = None, alias: str | None = None) -> dict:
    item: dict = {"agg": agg}
    if column is not None:
        item["column"] = column
    if alias:
        item["as"] = alias
    return item


def _non_derived(grid: dict) -> list[list]:
    derived = set(grid["derived_rows"])
    return [row for i, row in enumerate(grid["rows"]) if i not in derived]


def _identifier(profiles: list[dict]) -> dict | None:
    """The row's key: a text column that is unique (or nearly) on every row.

    A section column that repeats ("a. Jumlah Usaha Mikro" over and over) is not
    an identifier; using it would ask "which row called a. Jumlah ..." and make
    a top-k meaningless.
    """
    hint = re.compile(
        r"(nama|name|kecamatan|kabupaten|kota|provinsi|wilayah|desa|kelurahan|uraian|jenis|"
        r"kategori|komoditas|data|bulan|tahun|country|region|indicator|keterangan)",
        re.I,
    )
    candidates = [
        p
        for p in profiles
        if p["role"] in ("identifier", "label")
        and p["distinct"] >= 3
        and p["n"] >= 3
        and p["distinct"] >= max(3, int(0.9 * p["n"]))
    ]

    def rank(p: dict):
        names = p.get("sample") or []
        average = sum(len(str(v)) for v in names) / len(names) if names else 0
        return (2 if hint.search(p["name"]) else 0, average, p["distinct"])

    candidates.sort(key=rank, reverse=True)
    return candidates[0] if candidates else None


def _quantities(grid: dict, profiles: list[dict]) -> list[dict]:
    """Summable columns: numeric, not a code, not a year, at least 3 values."""
    out = []
    for p in profiles:
        if p["role"] != "quantity" or SYSTEM_HEADER.match(p["name"].strip()):
            continue
        c = grid["header"].index(p["name"])
        values = [row[c] for row in _non_derived(grid) if isinstance(row[c], (int, float))]
        if len(values) < 3:
            continue
        if all(is_year(v) for v in values):
            continue
        if not re.fullmatch(r"(?:19|20)\d\d", p["name"].strip()) and all(
            float(v).is_integer() and len(str(int(v))) >= 9 for v in values
        ):
            # A long unitless integer column with no year header looks like a
            # code; summing it is a coin flip, so it is not a question target.
            continue
        out.append(p)
    out.sort(
        key=lambda p: (
            0 if p["name"] == grid.get("preferred_quantity") else 1,
            p["name"].lower().endswith("jumlah"),
            -p["distinct"],
            p["name"],
        )
    )
    return out


def _matches(grid: dict, column: str, needle: str) -> list[list]:
    c = grid["header"].index(column)
    return [row for row in _non_derived(grid) if row[c] is not None and needle.lower() in str(row[c]).lower()]


def _unique_value(grid: dict, column: str) -> str | None:
    values = _unique_values(grid, column, 1)
    return values[0] if values else None


def _unique_values(grid: dict, column: str, limit: int = 3) -> list[str]:
    c = grid["header"].index(column)
    counts: dict[str, int] = {}
    for row in _non_derived(grid):
        if isinstance(row[c], str) and row[c].strip():
            counts[row[c]] = counts.get(row[c], 0) + 1
    return [value for value, n in counts.items() if n == 1 and len(value) >= 3][:limit]


def _substring(grid: dict, column: str) -> tuple[str, int] | None:
    found = _substrings(grid, column, 1)
    return found[0] if found else None


def _substrings(grid: dict, column: str, limit: int = 2) -> list[tuple[str, int]]:
    """Substrings of an identifier column that select 2-8 data rows each."""
    c = grid["header"].index(column)
    words: dict[str, int] = {}
    for row in _non_derived(grid):
        value = row[c]
        if not isinstance(value, str) or len(value) < 4:
            continue
        for word in re.split(r"[\s,./()\-]+", value):
            word = word.strip()
            if len(word) >= 4 and not parse_number(word):
                words[word] = words.get(word, 0) + 1
    ranked = sorted(words.items(), key=lambda kv: (-kv[1], kv[0]))
    return [(word, n) for word, n in ranked if 2 <= n <= 8][:limit]


def _question_texts(template: str, lang: str, ctx: dict) -> str:
    title = ctx["title"]
    column = ctx.get("column", "")
    identifier = ctx["identifier"]
    value = ctx.get("value")
    k = ctx.get("k")
    unit = ctx.get("unit")
    threshold = ctx.get("threshold")
    in_unit = f" (dalam {unit})" if unit and lang == "id" else (f" (in {unit})" if unit else "")
    year = column if column and re.fullmatch(r"(?:19|20)\d\d", column.strip()) else None
    col_id = f'tahun {year}' if year and lang == "id" else (f"the year {year}" if year else f'kolom "{column}"')
    col_en = f"the year {year}" if year else f'"{column}"'
    col_id_q = f'"{column}"'
    if template == "cell_lookup":
        if lang == "id":
            return f'Pada tabel "{title}", berapa nilai {col_id} untuk {identifier} "{value}"?'
        return f'In "{title}", what is the value of {col_en} for {identifier} "{value}"?'
    if template == "sum_filter":
        if threshold is not None:
            if lang == "id":
                return (
                    f'Pada tabel "{title}", berapa jumlah {col_id} untuk semua baris yang '
                    f'nilainya lebih dari {_fmt(threshold, lang)}{in_unit}?'
                )
            return (
                f'In "{title}", what is the sum of {col_en} over rows whose value is greater '
                f'than {_fmt(threshold, lang)}{in_unit}?'
            )
        if lang == "id":
            return (
                f'Pada tabel "{title}", berapa jumlah {col_id_q} untuk baris yang '
                f'{identifier}-nya mengandung "{value}"{in_unit}?'
            )
        return (
            f'In "{title}", what is the sum of {col_en} for rows whose {identifier} '
            f'contains "{value}"{in_unit}?'
        )
    if template == "count":
        if threshold is not None:
            if lang == "id":
                return (
                    f'Pada tabel "{title}", berapa banyak baris yang nilai {col_id_q}-nya '
                    f'lebih dari {_fmt(threshold, lang)}?'
                )
            return (
                f'In "{title}", how many rows have a {col_en} value greater than '
                f'{_fmt(threshold, lang)}?'
            )
        if lang == "id":
            return (
                f'Pada tabel "{title}", berapa banyak baris data yang {identifier}-nya '
                f'mengandung "{value}"?'
            )
        return f'In "{title}", how many data rows have "{value}" in {identifier}?'
    if template == "top_k":
        if lang == "id":
            return (
                f'Pada tabel "{title}", {k} {identifier} mana yang memiliki nilai {col_id_q} '
                f'tertinggi? Sebutkan {identifier} dan nilainya{in_unit}.'
            )
        return (
            f'In "{title}", which {k} {identifier} have the highest value for {col_en}? '
            f'Give the {identifier} and the value{in_unit}.'
        )
    if template == "group_max":
        if lang == "id":
            return (
                f'Pada tabel "{title}", {identifier} mana yang memiliki nilai {col_id_q} '
                f'tertinggi, dan berapa nilainya{in_unit}?'
            )
        return (
            f'In "{title}", which {identifier} has the highest value for {col_en}, and what '
            f'is that value{in_unit}?'
        )
    if template == "subtotal_trap":
        if lang == "id":
            return (
                f'Pada tabel "{title}", berapa total {col_id_q} dari seluruh baris data?'
            )
        return f'In "{title}", what is the total of {col_en} across all data rows?'
    if template == "code_trap":
        if lang == "id":
            return (
                f'Pada tabel "{title}", berapa banyak nilai {col_id_q} yang berbeda '
                f'(jangan dijumlahkan)?'
            )
        return f'In "{title}", how many distinct values of {col_en} are there (do not sum them)?'
    raise ValueError(template)


def _row_value(grid: dict, column: str, row: list):
    return row[grid["header"].index(column)] if column in grid["header"] else None


def _base_record(qid: str, lang: str, template: str, question: str, grid: dict, query: dict, answer, tolerance: float) -> dict:
    return {
        "id": qid,
        "lang": lang,
        "question": question,
        "file": grid["file"],
        "sheet_or_index": grid["sheet"] if grid["sheet"] is not None else 0,
        "grid_index": grid.get("index", 0),
        "gold_query": _q(query),
        "gold_answer": answer,
        "tolerance": tolerance,
        "template": template,
    }


def _intents(grids: list[dict], seed: int = SEED) -> list[dict]:
    """Every (template, grid, context) intent before language expansion."""
    rng = random.Random(seed)
    out: list[dict] = []
    for grid in grids:
        profiles = column_profile(grid["header"], grid["rows"])
        derived = set(grid["derived_rows"])
        data_rows = [r for i, r in enumerate(grid["rows"]) if i not in derived]
        if len(data_rows) < 3:
            continue
        identifier = _identifier(profiles)
        quantities = _quantities(grid, profiles)
        if identifier is None:
            continue
        ambiguous = grid.get("data_sum_row") is not None
        title = grid.get("doc_title") or grid["title"]
        base = {"title": title, "identifier": identifier["name"], "unit": grid.get("unit")}

        if not quantities:
            # A directory table (a code column, names, no quantity): a cell
            # lookup on text plus the code trap still grade it.
            targets = [
                p
                for p in profiles
                if p["role"] in ("label",)
                and p["name"] != identifier["name"]
                and p["distinct"] >= 2
            ]
            label = _unique_value(grid, identifier["name"])
            if targets and label:
                target = targets[0]
                row = next((r for r in data_rows if _row_value(grid, identifier["name"], r) == label), None)
                value = _row_value(grid, target["name"], row) if row else None
                if isinstance(value, str) and value.strip():
                    query = {
                        "select": [_select_column(target["name"])],
                        "filter": [{"column": identifier["name"], "op": "=", "value": label}],
                    }
                    add(
                        "cell_lookup",
                        {"title": base["title"], "identifier": identifier["name"], "column": target["name"], "value": label},
                        query,
                        value,
                        0.0,
                    )
            for code in [p for p in profiles if p["role"] == "code"][:2]:
                ccode = grid["header"].index(code["name"])
                distinct = len({str(r[ccode]) for r in data_rows if r[ccode] is not None})
                if distinct >= 3:
                    add(
                        "code_trap",
                        {"title": base["title"], "identifier": identifier["name"], "column": code["name"]},
                        {"select": [_select_agg("count_distinct", code["name"], "n")]},
                        distinct,
                        0.0,
                        trap="code",
                    )
            continue

        docx_budget = 0

        def add(template, ctx, query, answer, tolerance, **extra):
            nonlocal docx_budget
            if grid.get("family") == "docx":
                if docx_budget >= 8:
                    return
                docx_budget += 1
            out.append(
                {
                    "template": template,
                    "ctx": ctx,
                    "grid": grid,
                    "query": query,
                    "answer": answer,
                    "tolerance": tolerance,
                    "extra": extra,
                }
            )

        # 1. Cell lookup: a unique identifier value and a populated quantity.
        emitted = 0
        for quantity in quantities[:2]:
            for label in _unique_values(grid, identifier["name"], 2):
                row = next((r for r in data_rows if _row_value(grid, identifier["name"], r) == label), None)
                value = _row_value(grid, quantity["name"], row) if row else None
                if not isinstance(value, (int, float)):
                    continue
                query = {
                    "select": [_select_column(quantity["name"])],
                    "filter": [{"column": identifier["name"], "op": "=", "value": label}],
                }
                add("cell_lookup", {**base, "column": quantity["name"], "value": label}, query, value, 1e-6)
                emitted += 1
                if emitted >= 3:
                    break
            if emitted >= 3:
                break

        # 2. Sum with a filter: a substring of the identifier when it selects a
        # few rows, else a numeric threshold on the summed column (the natural
        # filter for a country/year table).
        for quantity in quantities[:2]:
            if ambiguous:
                break
            column = quantity["name"]
            picks = _substrings(grid, identifier["name"], 2)
            if picks:
                for needle, _ in picks:
                    rows = _matches(grid, identifier["name"], needle)
                    values = [v for v in (_row_value(grid, column, r) for r in rows) if isinstance(v, (int, float))]
                    if not values:
                        continue
                    total = sum(values)
                    query = {
                        "select": [_select_agg("sum", column, "total")],
                        "filter": [{"column": identifier["name"], "op": "contains", "value": needle}],
                    }
                    add(
                        "sum_filter",
                        {**base, "column": column, "value": needle, "filter_op": "contains"},
                        query,
                        total,
                        1e-6,
                    )
                break
            values = [r[grid["header"].index(column)] for r in data_rows]
            values = [v for v in values if isinstance(v, (int, float))]
            if len(values) >= 8:
                threshold = sorted(values)[int(0.8 * len(values))]
                selected = [v for v in values if v > threshold]
                if threshold > 0 and 2 <= len(selected) <= 40:
                    query = {
                        "select": [_select_agg("sum", column, "total")],
                        "filter": [{"column": column, "op": ">", "value": threshold}],
                    }
                    add(
                        "sum_filter",
                        {**base, "column": column, "threshold": threshold, "filter_op": ">"},
                        query,
                        sum(selected),
                        1e-6,
                    )
                    break

        # 3. Count rows matching a substring, or above a numeric threshold.
        if not ambiguous:
            picked = _substring(grid, identifier["name"])
            column = quantities[0]["name"]
            values = [r[grid["header"].index(column)] for r in data_rows]
            values = [v for v in values if isinstance(v, (int, float))]
            if picked:
                picks = _substrings(grid, identifier["name"], 2)
                for needle, _ in picks:
                    query = {
                        "select": [_select_agg("count", None, "n")],
                        "filter": [{"column": identifier["name"], "op": "contains", "value": needle}],
                    }
                    add("count", {**base, "value": needle, "filter_op": "contains"}, query, len(_matches(grid, identifier["name"], needle)), 0.0)
            elif len(values) >= 8:
                threshold = sorted(values)[int(0.8 * len(values))]
                selected = [v for v in values if v > threshold]
                if threshold > 0 and 2 <= len(selected) <= 40:
                    query = {
                        "select": [_select_agg("count", None, "n")],
                        "filter": [{"column": column, "op": ">", "value": threshold}],
                    }
                    add(
                        "count",
                        {**base, "column": column, "threshold": threshold, "filter_op": ">"},
                        query,
                        len(selected),
                        0.0,
                    )

        # 4. Top-k: identifier + quantity, ordered, limited.
        quantity = quantities[0]
        c = grid["header"].index(quantity["name"])
        ci = grid["header"].index(identifier["name"])
        candidates = [
            r for r in data_rows if isinstance(r[c], (int, float)) and r[ci] is not None
        ]
        if len(candidates) >= 5 and not ambiguous:
            for k in (3, 5):
                if k > len(candidates):
                    continue
                ranked = sorted(candidates, key=lambda r: r[c], reverse=True)[:k]
                answer = [
                    {"label": _row_value(grid, identifier["name"], r), "value": _row_value(grid, quantity["name"], r)}
                    for r in ranked
                ]
                query = {
                    "select": [_select_column(identifier["name"]), _select_column(quantity["name"])],
                    "order_by": [{"key": quantity["name"], "dir": "desc"}],
                    "limit": k,
                }
                add("top_k", {**base, "column": quantity["name"], "k": k}, query, answer, 1e-6)

        # 5. Group max: a repeated label is a true group; otherwise each
        # identifier is its own group and the maximum row wins.
        if candidates and not ambiguous:
            group_column = _repeated_group(profiles, identifier, grid)
            if group_column:
                cg = grid["header"].index(group_column)
                groups: dict[str, list[float]] = {}
                for row in data_rows:
                    label, value = row[cg], row[c]
                    if isinstance(value, (int, float)) and isinstance(label, str) and label.strip():
                        groups.setdefault(label, []).append(value)
                multi = {key: max(v) for key, v in groups.items() if len(v) >= 2}
                if multi:
                    top_label = max(multi, key=lambda key: multi[key])
                    query = {
                        "select": [_select_column(group_column), _select_agg("max", quantity["name"], "max_value")],
                        "group_by": [group_column],
                        "order_by": [{"key": "max_value", "dir": "desc"}],
                        "limit": 1,
                    }
                    add(
                        "group_max",
                        {**base, "column": quantity["name"], "identifier": group_column},
                        query,
                        {"label": top_label, "value": multi[top_label]},
                        1e-6,
                    )
            if not any(i["template"] == "group_max" and i["grid"] is grid for i in out):
                best = max(candidates, key=lambda r: r[c])
                query = {
                    "select": [_select_column(identifier["name"]), _select_agg("max", quantity["name"], "max_value")],
                    "group_by": [identifier["name"]],
                    "order_by": [{"key": "max_value", "dir": "desc"}],
                    "limit": 1,
                }
                add(
                    "group_max",
                    {**base, "column": quantity["name"]},
                    query,
                    {"label": _row_value(grid, identifier["name"], best), "value": _row_value(grid, quantity["name"], best)},
                    1e-6,
                )

        # 6. The subtotal trap: the sum over data rows only, where adding the
        # `Jumlah` row would give a different (wrong) answer.
        labelled = [i for i in grid["derived_rows"] if i != grid.get("data_sum_row")]
        if labelled and not ambiguous:
            for quantity in quantities[:2]:
                column = quantity["name"]
                total = sum(
                    v for v in (_row_value(grid, column, r) for r in data_rows) if isinstance(v, (int, float))
                )
                with_derived = sum(
                    v
                    for r in grid["rows"]
                    for v in [_row_value(grid, column, r)]
                    if isinstance(v, (int, float))
                )
                if abs(with_derived - total) <= max(1e-9, abs(total) * 1e-9):
                    continue
                add(
                    "subtotal_trap",
                    {**base, "column": column},
                    {"select": [_select_agg("sum", column, "total")]},
                    total,
                    1e-6,
                    trap="subtotal",
                    wrong_if_derived_included=with_derived,
                )

        # 7. The code trap: distinct count over a code column, never a sum.
        for code in [p for p in profiles if p["role"] == "code"][:2]:
            ccode = grid["header"].index(code["name"])
            distinct = len({str(r[ccode]) for r in data_rows if r[ccode] is not None})
            if distinct >= 3:
                add(
                    "code_trap",
                    {**base, "column": code["name"]},
                    {"select": [_select_agg("count_distinct", code["name"], "n")]},
                    distinct,
                    0.0,
                    trap="code",
                )
    return out


def _materialise(intents: list[dict], seed: int = SEED) -> list[dict]:
    """Expand intents into records: about half of them in both languages."""
    rng = random.Random(seed + 1)
    records: list[dict] = []
    alternate = 0
    for intent in intents:
        bilingual = rng.random() < 0.5
        if bilingual:
            langs = ["id", "en"]
        else:
            alternate += 1
            langs = ["id" if alternate % 2 else "en"]
        for lang in langs:
            record = {
                "lang": lang,
                "question": _question_texts(intent["template"], lang, intent["ctx"]),
                "file": intent["grid"]["file"],
                "sheet_or_index": intent["grid"]["sheet"] if intent["grid"]["sheet"] is not None else 0,
                "grid_index": intent["grid"].get("index", 0),
                "gold_query": _q(intent["query"]),
                "gold_answer": intent["answer"],
                "tolerance": intent["tolerance"],
                "template": intent["template"],
            }
            record.update(intent["extra"])
            record["_pair"] = intent["ctx"].get("intent_key", "")
            records.append(record)
    records.sort(key=lambda r: (r["file"], r["template"], r["lang"], r["question"]))
    for i, record in enumerate(records, 1):
        record["id"] = f"q-{i:04d}"
        record.pop("_pair", None)
    return records


def _fingerprint(grid: dict) -> str:
    """A grid's content hash, to find the same table in two files.

    A DOCX generated from a spreadsheet holds the same header and rows: a
    question about that table is answered by either, so retrieval must accept
    both and execution still tests the file the question names.
    """
    payload = json.dumps([grid["header"], grid["rows"]], ensure_ascii=False, sort_keys=True, default=str)
    return hashlib.sha256(payload.encode()).hexdigest()


def generate_questions(grids: list[dict], seed: int = SEED) -> list[dict]:
    """All questions for all grids, in both languages for about half."""
    for grid in grids:
        grid.setdefault("index", 0)
    by_content: dict[str, list[str]] = {}
    for grid in grids:
        by_content.setdefault(_fingerprint(grid), []).append(grid["file"])
    questions = _materialise(_intents(grids, seed), seed)
    for question in questions:
        grid = next(
            g
            for g in grids
            if g["file"] == question["file"] and g.get("index", 0) == question["grid_index"]
        )
        alternatives = [f for f in by_content.get(_fingerprint(grid), []) if f != question["file"]]
        if alternatives:
            question["alt_files"] = alternatives
    return questions


def _repeated_group(profiles: list[dict], identifier: dict, grid: dict) -> str | None:
    """A text column with 2-8 distinct values and at least two rows each."""
    for p in profiles:
        if p["role"] not in ("label", "identifier") or p["name"] == identifier["name"]:
            continue
        if 2 <= p["distinct"] <= 8 and p["n"] >= 4:
            c = grid["header"].index(p["name"])
            counts: dict[str, int] = {}
            for row in _non_derived(grid):
                if isinstance(row[c], str) and row[c].strip():
                    counts[row[c]] = counts.get(row[c], 0) + 1
            if sum(1 for n in counts.values() if n >= 2) >= 2:
                return p["name"]
    return None


# ---------------------------------------------------------------------------
# DOCX tables generated from the spreadsheets
# ---------------------------------------------------------------------------

DOCX_FROM = [
    "klaten-kelahiran-hidup",
    "klaten-penduduk-usia",
    "klaten-penduduk-umur",
    "klaten-tenaga-kesehatan",
    "klaten-sekolah-tk",
]


def write_docx(path: Path, title: str, header: list[str], rows: list[list], note: str) -> None:
    """A DOCX whose table is the spreadsheet's grid, under its title."""
    import docx

    document = docx.Document()
    document.add_heading(title, level=1)
    document.add_paragraph(note)
    table = document.add_table(rows=1, cols=len(header))
    table.style = "Table Grid"
    for i, name in enumerate(header):
        table.rows[0].cells[i].text = str(name)
    for row in rows:
        cells = table.add_row().cells
        for i, value in enumerate(row[: len(header)]):
            if value is not None:
                cells[i].text = _plain(value)
    document.save(str(path))


def docx_sources(out: Path, grids: list[dict]) -> list[dict]:
    """Five DOCX files generated from five spreadsheets, marked generated."""
    by_file = {g["file"]: g for g in grids}
    chosen = [by_file[f"{key}.xlsx"] for key in DOCX_FROM if f"{key}.xlsx" in by_file]
    if len(chosen) < 5:
        extra = [
            g
            for g in grids
            if g.get("family") == "klaten"
            and g["file"] not in {c["file"] for c in chosen}
            and len(g["rows"]) >= 6
        ]
        chosen.extend(extra[: 5 - len(chosen)])
    sources = []
    for grid in chosen[:5]:
        key = grid["file"].removesuffix(".xlsx")
        path = out / "sources" / f"docx-{key.removeprefix('klaten-')}.docx"
        write_docx(
            path,
            grid.get("doc_title") or grid["title"],
            grid["header"],
            grid["rows"],
            f"Tabel ini disalin dari lembar \"{grid['sheet']}\" pada {grid['file']}.",
        )
        sources.append(
            {
                "key": path.stem,
                "file": path.name,
                "format": "docx",
                "url": f"{grid.get('doc_url', '')}#generated-from-{grid['file']}",
                "licence": "CC BY 4.0 (generated from a CC BY 4.0 dataset)",
                "publisher": "generated with python-docx from opendata.klaten.go.id",
                "lang": "id",
                "generated": True,
                "parent": grid["file"],
                "needs_ocr": False,
                "doc_title": grid.get("doc_title") or grid["title"],
                "topic": "POS / OPD Klaten",
                "hints": {},
            }
        )
    return sources


# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------


def _source_record(entry: dict, path: Path) -> dict:
    return {
        "key": entry["key"],
        "file": path.name,
        "format": path.suffix.lower().lstrip("."),
        "url": entry["url"],
        "licence": entry["licence"],
        "publisher": entry["publisher"],
        "lang": entry["lang"],
        "generated": entry.get("generated", False),
        "parent": entry.get("parent"),
        "needs_ocr": entry.get("needs_ocr", False),
        "doc_title": entry["doc_title"],
        "topic": entry.get("topic"),
        "hints": entry.get("hints") or {},
    }


def collect_sources(out: Path, download: bool = True) -> list[dict]:
    src = out / "sources"
    src.mkdir(parents=True, exist_ok=True)
    sources: list[dict] = []
    for code, id_label, en_label, topic in WORLD_BANK:
        if download:
            path, url = fetch_worldbank(src, code)
        else:
            path, url = src / f"wb-{code}.csv", f"https://api.worldbank.org/v2/en/indicator/{code}?downloadformat=csv"
            if not path.exists():
                raise SystemExit(f"missing {path} and --no-download was given")
        sources.append(
            {
                "key": f"wb-{code}",
                "file": path.name,
                "format": "csv",
                "url": url,
                "licence": "CC BY 4.0",
                "publisher": "World Bank Open Data",
                "lang": "en",
                "generated": False,
                "needs_ocr": False,
                "doc_title": f"{en_label.capitalize()} — Indonesia (World Bank, {code})",
                "indicator_id": id_label,
                "indicator_en": en_label,
                "topic": topic,
                "hints": {},
            }
        )
    for entry in KLATEN:
        path, url = (fetch_klaten(src, entry) if download else (src / f"{entry['key']}.xlsx", entry["url"]))
        if not path.exists():
            raise SystemExit(f"missing {path} and --no-download was given")
        sources.append(
            {
                "key": entry["key"],
                "file": path.name,
                "format": "xlsx",
                "url": entry["url"],
                "licence": "CC BY 4.0",
                "publisher": "Satu Data Kabupaten Klaten (opendata.klaten.go.id)",
                "lang": "id",
                "generated": False,
                "needs_ocr": False,
                "doc_title": entry["title"],
                "dataset": entry["dataset"],
                "topic": "BPS Kabupaten Klaten",
                "hints": entry.get("hints") or {},
            }
        )
    return sources


def build(args) -> dict:
    out: Path = args.out
    out.mkdir(parents=True, exist_ok=True)
    sources = collect_sources(out, download=not args.no_download)

    tables: list[dict] = []
    parsed: list[dict] = []
    kept: list[dict] = []
    skipped: list[dict] = []
    for source in sources:
        path = out / "sources" / source["file"]
        try:
            grids = build_grids(path, None, source["hints"])
        except Exception as error:  # a broken download must be visible, not fatal
            skipped.append({"file": source["file"], "reason": f"unreadable: {error}"})
            continue
        if not grids:
            skipped.append({"file": source["file"], "reason": "no table region found"})
            continue
        grid = grids[0]
        grid["index"] = 0
        grid["doc_title"] = source["doc_title"]
        grid["doc_url"] = source["url"]
        grid["family"] = "worldbank" if source["key"].startswith("wb-") else "klaten"
        profiles = column_profile(grid["header"], grid["rows"])
        grid["columns"] = profiles
        if len(grid["rows"]) < 2 or not (
            _quantities(grid, profiles) or any(p["role"] == "code" for p in profiles)
        ):
            skipped.append({"file": source["file"], "reason": "no summable or code column"})
            continue
        tables.append(grid)
        parsed.append(source)
        kept.append(source)

    generated = docx_sources(out, tables)
    for source in generated:
        path = out / "sources" / source["file"]
        try:
            grids = build_grids(path, None, source["hints"])
        except Exception as error:
            skipped.append({"file": source["file"], "reason": f"unreadable: {error}"})
            continue
        if not grids:
            skipped.append({"file": source["file"], "reason": "no table region found"})
            continue
        grid = grids[0]
        grid["index"] = 0
        grid["doc_title"] = source["doc_title"]
        grid["doc_url"] = source["url"]
        grid["family"] = "docx"
        grid["columns"] = column_profile(grid["header"], grid["rows"])
        if not (_quantities(grid, grid["columns"]) or any(p["role"] == "code" for p in grid["columns"])):
            tables.append(grid)  # generated from a kept grid, so it parses the same way
            kept.append(source)
            continue
        tables.append(grid)
        kept.append(source)

    questions = generate_questions(tables, seed=args.seed)
    if len(questions) < args.min_questions:
        counts: dict[str, int] = {}
        for question in questions:
            counts[question["template"]] = counts.get(question["template"], 0) + 1
        raise SystemExit(
            f"only {len(questions)} questions (< {args.min_questions}); per template: {counts}"
        )

    # Write the set.
    (out / "sources.jsonl").write_text(
        "".join(json.dumps(source, ensure_ascii=False) + "\n" for source in kept)
    )
    (out / "queries.jsonl").write_text(
        "".join(json.dumps(question, ensure_ascii=False) + "\n" for question in questions)
    )
    by_template: dict[str, int] = {}
    by_lang: dict[str, int] = {}
    by_trap: dict[str, int] = {}
    for question in questions:
        by_template[question["template"]] = by_template.get(question["template"], 0) + 1
        by_lang[question["lang"]] = by_lang.get(question["lang"], 0) + 1
        if question.get("trap"):
            by_trap[question["trap"]] = by_trap.get(question["trap"], 0) + 1
    bilingual = sum(1 for q in questions if q["lang"] == "en")
    manifest = {
        "name": "tables-v1",
        "version": 1,
        "seed": args.seed,
        "built_by": "rust/crates/kb-eval/scripts/import_tables.py",
        "purpose": "TBL-1: table retrieval, table execution and end-to-end table QA",
        "files": len(kept),
        "xlsx_csv_files": sum(1 for s in kept if s["format"] in ("xlsx", "csv")),
        "docx_files": sum(1 for s in kept if s["format"] == "docx"),
        "questions": len(questions),
        "counts": {
            "by_template": by_template,
            "by_lang": by_lang,
            "by_trap": by_trap,
            "bilingual_english_records": bilingual,
        },
        "targets": {
            "retrieval_top5": 0.85,
            "execution_correct": 0.98,
            "subtotal_trap_correct": 0.98,
            "end_to_end_table": 0.80,
            "end_to_end_delta": 0.30,
        },
        "sources": kept,
        "skipped": skipped,
        "tables": [
            {
                "file": grid["file"],
                "doc_title": grid.get("doc_title"),
                "family": grid.get("family"),
                "sheet": grid["sheet"],
                "title": grid["title"],
                "header_rows": grid["header_rows"],
                "header": grid["header"],
                "n_rows": len(grid["rows"]),
                "derived_rows": grid["derived_rows"],
                "formula_rows": grid["formula_rows"],
                "data_sum_row": grid["data_sum_row"],
                "unit": grid["unit"],
                "year_columns": grid.get("year_columns", []),
                "columns": grid["columns"],
            }
            for grid in tables
        ],
    }
    (out / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n")
    (out / "README.md").write_text(dataset_readme(manifest))
    return {
        "out": str(out),
        "files": len(kept),
        "xlsx_csv_files": manifest["xlsx_csv_files"],
        "docx_files": sum(1 for s in kept if s["format"] == "docx"),
        "questions": len(questions),
        "by_template": by_template,
        "by_lang": by_lang,
        "by_trap": by_trap,
        "skipped": [s["file"] for s in skipped],
    }


def dataset_readme(manifest: dict) -> str:
    counts = manifest["counts"]
    template_rows = "\n".join(
        f"| {name} | {counts['by_template'].get(name, 0)} |" for name in TEMPLATES
    )
    source_rows = "\n".join(
        f"| `{s['file']}` | {s['format']} | {s['lang']} | {s['publisher']} | {s['licence']} |"
        for s in manifest["sources"]
    )
    return f"""# tables-v1

{table_intro()}

Built by `rust/crates/kb-eval/scripts/import_tables.py` with seed {manifest['seed']}.

## What is in it

* {manifest['files']} sources ({manifest['xlsx_csv_files']} xlsx/csv, {manifest['docx_files']} DOCX
  generated from five of the spreadsheets and marked `generated`).
* {manifest['questions']} questions, {counts['by_lang'].get('id', 0)} Indonesian and
  {counts['by_lang'].get('en', 0)} English (about half the intents appear in both languages).
* `queries.jsonl` — one record per question: the question, the file, the gold `TableQuery`,
  the gold answer, the tolerance and the template.
* `sources.jsonl` — where each file came from and under which licence.
* `manifest.json` — the grids this script read (header, derived rows, column roles), so the
  reading is auditable without re-running the script.

## Questions per template

| template | questions |
|---|---:|
{template_rows}

Subtotal traps: {counts['by_trap'].get('subtotal', 0)}. Code traps: {counts['by_trap'].get('code', 0)}.

## Sources

| file | format | language | publisher | licence |
|---|---|---|---|---|
{source_rows}

## How the gold answers are made

Nothing here imports the Rust extractor, the manifest builder or DuckDB. A source is read with
openpyxl / csv / python-docx; merged cells are spread, the title and header are found, a
multi-level header is flattened with ` / `, duplicates get ` (2)`, and the data region stops at
a footnote. `Jumlah`/`Total` rows are marked derived from the label (or a `SUM` formula in the
file), and a trailing row that equals the sum of the ones above it is marked too (and the grid
is then kept out of aggregate templates, because the manifest cannot know that row).

Each gold answer is arithmetic over those rows in the template's own code. `--self-check`
then runs each gold `TableQuery` through a separate reference executor over the same grid and
compares the result to the gold answer, so a gold answer that does not agree with its own grid
fails the build. The executor is a second path, not the source of the number.

    uv run rust/crates/kb-eval/scripts/import_tables.py --out ~/kbdata/tables-v1 --self-check

## Grading

`rust/crates/kb-eval/scripts/score_tables.py` ingests `sources/` through `POST /v1/documents`,
waits for `ready`, maps each file to its `tableId` via `GET /v1/tables?documentId=`, and then
scores table retrieval (top 5), execution (the gold `TableQuery` through
`POST /v1/tables/{{id}}/query`) and, with `--agent`, an end-to-end model arm. Unmatched files
are extraction failures, reported, never dropped.
"""


def table_intro() -> str:
    return (
        "Public tables in Indonesian and English with questions a calling app's agent would "
        "ask: a cell, a filtered sum, a count, a top-k, a group maximum, a total that must not "
        "include the `Jumlah` row, and a distinct count over a code column that must never be "
        "summed."
    )


# ---------------------------------------------------------------------------
# Self-check
# ---------------------------------------------------------------------------


def answer_of(result: dict):
    rows, columns = result["rows"], result["columns"]
    if len(rows) == 1 and len(columns) == 1:
        return rows[0][0]
    return rows


def self_check(out: Path) -> int:
    """Run every gold query through the reference executor and compare."""
    manifest = json.loads((out / "manifest.json").read_text())
    questions = [json.loads(line) for line in (out / "queries.jsonl").read_text().splitlines() if line.strip()]
    source_of = {s["file"]: s for s in manifest["sources"]}
    cache: dict[tuple, dict] = {}
    try:
        from score_tables import answers_match
    except ImportError:
        answers_match = _fallback_match

    failures = []
    for question in questions:
        source = source_of.get(question["file"])
        if source is None:
            failures.append((question["id"], "no source record"))
            continue
        key = (question["file"], question["grid_index"])
        if key not in cache:
            grids = build_grids(out / "sources" / question["file"], None, source.get("hints") or {})
            if len(grids) <= question["grid_index"]:
                failures.append((question["id"], "grid not found"))
                continue
            grid = grids[question["grid_index"]]
            grid["index"] = question["grid_index"]
            cache[key] = grid
        result = execute(cache[key], question["gold_query"])
        got = answer_of(result)
        if not answers_match(question["gold_answer"], got, question["tolerance"]):
            failures.append((question["id"], f"gold {question['gold_answer']!r} != {got!r}"))
    print(f"self-check: {len(questions) - len(failures)}/{len(questions)} gold queries match their grids")
    for qid, reason in failures[:15]:
        print(f"  FAIL {qid}: {reason}")
    if failures:
        print(f"  {len(failures)} failures")
        return 1
    return 0


def _fallback_match(gold, got, tolerance: float) -> bool:
    if isinstance(gold, (int, float)) and isinstance(got, (int, float)):
        return abs(gold - got) <= max(1e-9, abs(gold) * max(tolerance, 1e-9))
    return gold == got


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--out", type=Path, default=Path.home() / "kbdata" / "tables-v1",
                        help="dataset directory (default: ~/kbdata/tables-v1)")
    parser.add_argument("--seed", type=int, default=SEED, help="random seed for language pairing")
    parser.add_argument("--min-questions", type=int, default=300, help="fail if fewer questions are generated")
    parser.add_argument("--no-download", action="store_true",
                        help="use the files already in <out>/sources instead of downloading")
    parser.add_argument("--self-check", action="store_true",
                        help="re-read every source and check each gold query with the reference executor")
    args = parser.parse_args()
    if args.self_check:
        return self_check(args.out)
    summary = build(args)
    print(json.dumps(summary, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
