#!/usr/bin/env python3
"""Score `tables-v1` against a live kb-server (TBL-1).

Three scores, each through the public HTTP API, nothing linked in:

1. **Table retrieval.** `POST /v1/search` with the question; is the gold
   `tableId` among the `tableId`s of the top 5 hits? No model involved.
2. **Execution.** The gold `TableQuery` goes to `POST /v1/tables/{id}/query`;
   its answer must match the gold answer within tolerance. This grades the
   extractor, the manifest (subtotal rows excluded, code columns not summed)
   and DuckDB together. `subtotal_trap` and `code_trap` questions are reported
   separately, because they are the two ways a table pipeline is usually wrong.
3. **End to end** (`--agent`). An OpenAI-compatible chat model answers in two
   arms: (a) from the retrieved text chunks only, (b) from the table summary,
   writing a `TableQuery` the harness executes before the model states the
   answer. The paired difference (b) − (a) is the number that says whether
   structured tables beat flattened text.

The run is resumable: uploads are remembered in `ingest.json` by file hash, and
a re-run reuses a document that is still `ready`. Every failure is counted and
listed — an API error is never dropped as an empty result.

    python3 score_tables.py --dataset ~/kbdata/tables-v1 \
        --base-url http://127.0.0.1:8080 --key "$KB_API_KEY"
    python3 score_tables.py ... --agent --llm-base-url http://127.0.0.1:8080/v1 \
        --llm-model qwen2.5-7b-instruct

`--llm-base-url` must be loopback unless `--allow-paid` is given: a paid run
needs the owner's go-ahead (AGENTS.md rule 7). Responses are cached on disk by
(model, messages), so a re-run costs nothing.

Outputs, in `--out` (default `<dataset>/runs/<timestamp>`):

* `report.md` — the tables, targets and failures;
* `results.json` — the same numbers, machine-readable;
* `per_question.jsonl` — one line per question with both arms' answers;
* `ingest.json`, `llm_cache/` — the resumable state.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import math
import os
import random
import re
import sys
import time
import unicodedata
import urllib.error
import urllib.parse
import urllib.request
import uuid
from pathlib import Path

DEFAULT_TOPK = 5
BOUNDARY = "----kb-eval-" + uuid.uuid4().hex
TARGETS = {
    "retrieval_top5": 0.85,
    "execution_correct": 0.98,
    "subtotal_trap_correct": 0.98,
    "end_to_end_table": 0.80,
    "end_to_end_delta": 0.30,
}


# ---------------------------------------------------------------------------
# Answers: numbers in Indonesian and English, and comparison within tolerance
# ---------------------------------------------------------------------------

_NUMBER = re.compile(
    r"(?<![\w.,])([+-]?(?:\d{1,3}(?:[.,]\d{3})+|\d+)(?:[.,]\d+)?)"
    # A suffix only counts when the word ends there: "5.730 kelahiran" is not
    # 5.73 million, and "1,2 miliar" is.
    r"\s*(%|persen|juta|jt|miliar|milyar|triliun|tn|ribu|rb|k|thousand|million|billion|trillion)?(?![A-Za-z])",
    re.I,
)
_MULTIPLIER = {
    "": 1.0,
    "%": 1.0,
    "persen": 1.0,
    "juta": 1e6,
    "jt": 1e6,
    "miliar": 1e9,
    "milyar": 1e9,
    "triliun": 1e12,
    "tn": 1e12,
    "ribu": 1e3,
    "rb": 1e3,
    "k": 1e3,
    "thousand": 1e3,
    "million": 1e6,
    "billion": 1e9,
    "trillion": 1e12,
}


def parse_number(text) -> float | None:
    """A table cell or a model answer as a number, or None.

    Locale-aware: with both `.` and `,` the rightmost separator is decimal
    (`1.234,5` → 1234.5); a lone separator in groups of three is thousands
    (`1.234.567`); otherwise it is decimal. `%`, `Rp`, `juta`, `miliar`,
    `million` and `billion` are understood, because model answers use them.
    """
    if text is None or isinstance(text, bool):
        return None
    if isinstance(text, (int, float)):
        value = float(text)
        return None if math.isnan(value) or math.isinf(value) else value
    s = unicodedata.normalize("NFKC", str(text)).strip()
    if not s:
        return None
    s = s.replace("\u00a0", " ").replace("\u2212", "-")
    neg = s.startswith("(") and s.endswith(")")
    if neg:
        s = s[1:-1]
    s = re.sub(r"^(?:rp\.?|idr|us\$|usd|\$|€)\s*", "", s.strip(), flags=re.I)
    m = re.fullmatch(
        r"([+-]?(?:\d{1,3}(?:[.,]\d{3})+|\d+)(?:[.,]\d+)?)\s*"
        r"(%|persen|juta|jt|miliar|milyar|triliun|tn|ribu|rb|k|thousand|million|billion|trillion)?",
        s,
        re.I,
    )
    if not m:
        return None
    body, suffix = m.group(1), (m.group(2) or "").lower()
    has_dot, has_comma = "." in body, "," in body
    if has_dot and has_comma:
        body = body.replace(".", "").replace(",", ".") if body.rfind(",") > body.rfind(".") else body.replace(",", "")
    elif has_dot:
        if re.fullmatch(r"[+-]?\d{1,3}(?:\.\d{3})+", body):
            body = body.replace(".", "")
    elif has_comma:
        if re.fullmatch(r"[+-]?\d{1,3}(?:,\d{3})+", body):
            body = body.replace(",", "")
        else:
            body = body.replace(",", ".")
    try:
        value = float(body) * _MULTIPLIER[suffix]
    except (ValueError, KeyError):
        return None
    return -value if neg else value


def numbers_in(text) -> list[float]:
    """Every number in a model answer ("Rp 1.234,5 juta" → [1.2345e9])."""
    if text is None:
        return []
    if isinstance(text, (int, float)) and not isinstance(text, bool):
        return [float(text)]
    out: list[float] = []
    for match in _NUMBER.finditer(unicodedata.normalize("NFKC", str(text)).replace("\u00a0", " ")):
        value = parse_number(match.group(1) + (match.group(2) or ""))
        if value is not None:
            out.append(value)
    return out


def normalize_text(text) -> str:
    s = unicodedata.normalize("NFKC", str(text or "")).casefold()
    s = re.sub(r"\s+", " ", s).strip()
    return s.strip(" .,;:!?\"'()[]")


def _close(gold: float, got: float, tolerance: float) -> bool:
    return abs(gold - got) <= max(1e-9, abs(gold) * max(tolerance, 0.0) + 1e-9)


def answers_match(gold, got, tolerance: float = 1e-6) -> bool:
    """Compare an API answer (or a model answer) with the gold answer.

    Numbers compare within `tolerance` (relative) and at least 1e-9 absolute, so
    a count of 0 and 0 match and a sum of 5730 needs 5730.000005730. Strings
    compare case- and whitespace-insensitively. A `{label, value}` answer may
    arrive as a two-cell row; a top-k list may arrive as rows of `[label,
    value]`. Text with a number in it ("sekitar 1.234,5 juta") matches the
    number.
    """
    if isinstance(gold, dict) and "label" in gold and "value" in gold:
        label = gold["label"]
        if isinstance(got, dict):
            return normalize_text(got.get("label")) == normalize_text(label) and answers_match(
                gold["value"], got.get("value"), tolerance
            )
        if isinstance(got, (list, tuple)) and len(got) == 1 and isinstance(got[0], (list, tuple)):
            return answers_match(gold, got[0], tolerance)
        if isinstance(got, (list, tuple)) and len(got) >= 2 and not isinstance(got[0], (list, tuple)):
            return normalize_text(got[0]) == normalize_text(label) and answers_match(gold["value"], got[1], tolerance)
        return normalize_text(label) in normalize_text(got) and answers_match(gold["value"], got, tolerance)
    if isinstance(gold, list):
        if isinstance(got, str):
            if all(
                isinstance(item, dict) and normalize_text(item.get("label")) in normalize_text(got)
                for item in gold
            ):
                return True
            return False
        if not isinstance(got, (list, tuple)):
            return False
        if len(got) != len(gold):
            return False
        return all(answers_match(g, x, tolerance) for g, x in zip(gold, got))
    if isinstance(gold, (int, float)) and not isinstance(gold, bool):
        if isinstance(got, (int, float)) and not isinstance(got, bool):
            return _close(float(gold), float(got), tolerance)
        if isinstance(got, str):
            direct = parse_number(got)
            if direct is not None and _close(float(gold), direct, tolerance):
                return True
            return any(_close(float(gold), value, tolerance) for value in numbers_in(got))
        return False
    if isinstance(got, (int, float)) and not isinstance(got, bool):
        return normalize_text(gold) == normalize_text(got)
    if isinstance(got, (list, tuple)) and len(got) == 1:
        return answers_match(gold, got[0], tolerance)
    return normalize_text(gold) == normalize_text(got)


def answer_of(result: dict):
    """A `QueryResult` as one answer: a scalar, or the rows."""
    rows = result.get("rows") or []
    columns = result.get("columns") or []
    if len(rows) == 1 and len(columns) == 1:
        return rows[0][0]
    return rows


# ---------------------------------------------------------------------------
# HTTP: one small client, no dependencies
# ---------------------------------------------------------------------------

class Api:
    """The kb-server HTTP API, with retries and a counted failure."""

    def __init__(self, base_url: str, key: str, timeout: float = 120.0):
        self.base = base_url.rstrip("/")
        self.key = key
        self.timeout = timeout

    def _request(self, method: str, path: str, body: bytes | None = None, headers: dict | None = None):
        url = f"{self.base}{path}"
        all_headers = {"x-api-key": self.key, "accept": "application/json"}
        all_headers.update(headers or {})
        for attempt in range(4):
            request = urllib.request.Request(url, data=body, headers=all_headers, method=method)
            try:
                with urllib.request.urlopen(request, timeout=self.timeout) as response:
                    raw = response.read()
                    return response.status, _decode(raw, response.headers.get("content-type", ""))
            except urllib.error.HTTPError as error:
                raw = error.read()
                payload = _decode(raw, error.headers.get("content-type", "") if error.headers else "")
                if error.code in (429, 500, 502, 503, 504) and attempt < 3:
                    time.sleep(2**attempt)
                    continue
                return error.code, payload
            except (urllib.error.URLError, TimeoutError, ConnectionError) as error:
                if attempt < 3:
                    time.sleep(2**attempt)
                    continue
                return 0, {"error": f"{type(error).__name__}: {error}"}
        return 0, {"error": "retries exhausted"}

    def get(self, path: str):
        return self._request("GET", path)

    def post(self, path: str, payload: dict):
        return self._request(
            "POST", path, json.dumps(payload).encode(), {"content-type": "application/json"}
        )

    def upload(self, path: Path, title: str):
        body, content_type = _multipart(path, title)
        return self._request("POST", "/v1/documents", body, {"content-type": content_type})

    def query_table(self, table_id: str, query: dict):
        return self.post(f"/v1/tables/{urllib.parse.quote(table_id, safe='')}/query", query)


def _decode(raw: bytes, content_type: str):
    text = raw.decode("utf-8", errors="replace")
    if "json" in (content_type or "") or text[:1] in "[{":
        try:
            return json.loads(text)
        except json.JSONDecodeError:
            return {"raw": text}
    return {"raw": text}


def _multipart(path: Path, title: str) -> tuple[bytes, str]:
    """A multipart body with `file` and `title`, built by hand (stdlib only)."""
    chunks: list[bytes] = []
    for name, value in (("title", title), ("externalRef", f"tables-v1:{path.name}")):
        chunks.append(f"--{BOUNDARY}\r\n".encode())
        chunks.append(f'Content-Disposition: form-data; name="{name}"\r\n\r\n'.encode())
        chunks.append(str(value).encode() + b"\r\n")
    chunks.append(f"--{BOUNDARY}\r\n".encode())
    chunks.append(
        f'Content-Disposition: form-data; name="file"; filename="{path.name}"\r\n'
        f"Content-Type: {_mime(path)}\r\n\r\n".encode()
    )
    chunks.append(path.read_bytes())
    chunks.append(f"\r\n--{BOUNDARY}--\r\n".encode())
    return b"".join(chunks), f"multipart/form-data; boundary={BOUNDARY}"


def _mime(path: Path) -> str:
    return {
        ".xlsx": "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        ".csv": "text/csv",
        ".docx": "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        ".pdf": "application/pdf",
    }.get(path.suffix.lower(), "application/octet-stream")


# ---------------------------------------------------------------------------
# Ingest and the document → table map
# ---------------------------------------------------------------------------

READY = {"ready", "indexed", "completed", "done"}
FAILED = {"failed", "error", "cancelled", "canceled"}


def file_hash(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def load_ingest(path: Path) -> dict:
    if path.exists():
        try:
            return json.loads(path.read_text())
        except json.JSONDecodeError:
            return {}
    return {}


def save_ingest(path: Path, state: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(state, indent=2) + "\n")
    tmp.replace(path)


def wait_ready(api: Api, document_id: str, timeout: float) -> tuple[str, str]:
    deadline = time.time() + timeout
    last = "unknown"
    while time.time() < deadline:
        status, payload = api.get(f"/v1/documents/{urllib.parse.quote(document_id)}")
        if status == 200:
            document = payload.get("document", payload) if isinstance(payload, dict) else {}
            last = str(document.get("status", "unknown"))
            if last.lower() in READY:
                return "ready", ""
            if last.lower() in FAILED:
                return "failed", last
        elif status == 404:
            return "failed", "document disappeared"
        time.sleep(2)
    return "timeout", last


def ingest(api: Api, dataset: Path, out: Path, timeout: float, reingest_failed: bool) -> dict:
    state = load_ingest(out / "ingest.json")
    sources = [json.loads(line) for line in (dataset / "sources.jsonl").read_text().splitlines() if line.strip()]
    problems = []
    for source in sources:
        name = source["file"]
        path = dataset / "sources" / name
        digest = file_hash(path)
        entry = state.get(name)
        if entry and entry.get("sha256") == digest:
            if entry.get("status") == "ready":
                status, _ = wait_ready(api, entry["documentId"], 30)
                if status == "ready":
                    continue
            elif entry.get("status") in ("failed", "timeout") and not reingest_failed:
                continue
        status, payload = api.upload(path, source.get("doc_title") or path.stem)
        if status not in (200, 201, 202):
            problems.append(f"{name}: upload HTTP {status}: {str(payload)[:200]}")
            state[name] = {"sha256": digest, "status": "upload-failed", "error": str(payload)[:400]}
            save_ingest(out / "ingest.json", state)
            continue
        document_id = payload.get("id") or payload.get("documentId")
        if not document_id:
            problems.append(f"{name}: upload returned no document id: {str(payload)[:200]}")
            continue
        state[name] = {
            "sha256": digest,
            "documentId": document_id,
            "jobId": payload.get("jobId"),
            "status": "processing",
            "uploadedAt": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
        }
        save_ingest(out / "ingest.json", state)
        status, detail = wait_ready(api, document_id, timeout)
        state[name]["status"] = status
        if detail:
            state[name]["detail"] = detail
        save_ingest(out / "ingest.json", state)
        if status != "ready":
            problems.append(f"{name}: ingest {status} ({detail})")
    return {"state": state, "problems": problems}


def list_tables(api: Api, document_id: str) -> tuple[list[dict], str]:
    """The document's tables, accepting the plausible JSON shapes."""
    status, payload = api.get(f"/v1/tables?documentId={urllib.parse.quote(document_id)}")
    if status != 200:
        return [], f"HTTP {status}: {str(payload)[:200]}"
    if isinstance(payload, list):
        return [t for t in payload if isinstance(t, dict)], ""
    if isinstance(payload, dict):
        for key in ("tables", "data", "items", "results"):
            value = payload.get(key)
            if isinstance(value, list):
                return [t for t in value if isinstance(t, dict)], ""
        if "id" in payload:
            return [payload], ""
    return [], "unrecognised /v1/tables shape"


def _header_names(record: dict) -> list[str]:
    header = record.get("header") or record.get("columns")
    if isinstance(header, list):
        names = []
        for item in header:
            if isinstance(item, str):
                names.append(item)
            elif isinstance(item, dict) and isinstance(item.get("name"), str):
                names.append(item["name"])
        return names
    manifest = record.get("manifest") or {}
    columns = manifest.get("columns") if isinstance(manifest, dict) else None
    if isinstance(columns, list):
        return [c.get("name", "") for c in columns if isinstance(c, dict)]
    return []


def _norm_header(names: list[str]) -> set[str]:
    return {re.sub(r"\s+", " ", unicodedata.normalize("NFKC", n)).strip().casefold() for n in names if n.strip()}


def map_tables(api: Api, questions: list[dict], state: dict) -> tuple[dict, dict, list[str]]:
    """`(file, grid_index) → tableId` by sheet/index, then header overlap."""
    by_file: dict[str, list[dict]] = {}
    problems: list[str] = []
    for name, entry in state.items():
        if entry.get("status") != "ready":
            continue
        records, error = list_tables(api, entry["documentId"])
        if error:
            problems.append(f"{name}: {error}")
            continue
        by_file[name] = records
    mapping: dict[tuple, str] = {}
    details: dict[tuple, dict] = {}
    for question in questions:
        key = (question["file"], question.get("grid_index", 0))
        if key in mapping:
            continue
        records = by_file.get(question["file"], [])
        if not records:
            continue
        want_sheet = question.get("sheet_or_index")
        candidates = records
        if isinstance(want_sheet, str):
            matching = [
                r for r in records if str(r.get("sheet") or "").strip().casefold() == want_sheet.strip().casefold()
            ]
            if matching:
                candidates = matching
        elif isinstance(want_sheet, int) and want_sheet > 0:
            candidates = records
        best = None
        best_score = 0.0
        gold_header = _norm_header(question.get("gold_header") or [])
        for record in candidates:
            score = 0.0
            if question.get("sheet_or_index") not in (None, ""):
                if str(record.get("sheet") or "") == str(question.get("sheet_or_index")):
                    score += 1.0
            if record.get("index") == question.get("grid_index"):
                score += 0.5
            if gold_header:
                overlap = len(gold_header & _norm_header(_header_names(record))) / len(gold_header)
                score += overlap
            if best is None or score > best_score:
                best, best_score = record, score
        if best is None:
            continue
        overlap = 0.0
        if gold_header:
            overlap = len(gold_header & _norm_header(_header_names(best))) / len(gold_header)
        table_id = best.get("id") or best.get("tableId")
        if table_id:
            mapping[key] = table_id
            details[key] = {
                "tableId": table_id,
                "headerOverlap": round(overlap, 3),
                "sheet": best.get("sheet"),
                "index": best.get("index"),
                "title": best.get("title"),
            }
    return mapping, details, problems


# ---------------------------------------------------------------------------
# Search hit → table ids, tolerant to the response shape
# ---------------------------------------------------------------------------

def table_ids_of(hits: dict) -> list[str]:
    """Every tableId a search response carries, in rank order, deduplicated."""
    out: list[str] = []

    def push(value):
        if isinstance(value, str) and value and value not in out:
            out.append(value)

    if not isinstance(hits, dict):
        return out
    for table in hits.get("tables") or []:
        if isinstance(table, dict):
            push(table.get("tableId") or table.get("id"))
        elif isinstance(table, str):
            push(table)
    for chunk in hits.get("chunks") or []:
        if not isinstance(chunk, dict):
            continue
        push(chunk.get("tableId") or chunk.get("table_id"))
        metadata = chunk.get("metadata")
        if isinstance(metadata, dict):
            push(metadata.get("tableId") or metadata.get("table_id"))
    for source in hits.get("sources") or []:
        if isinstance(source, dict):
            push(source.get("tableId"))
    return out


def hit_texts(hits: dict, limit: int = 5) -> list[str]:
    texts = []
    for chunk in (hits.get("chunks") or [])[:limit]:
        if isinstance(chunk, dict):
            text = chunk.get("content") or chunk.get("text") or ""
            if text:
                header = chunk.get("documentTitle") or chunk.get("document_title") or ""
                texts.append(f"[{header}] {text}")
    if not texts and isinstance(hits.get("context"), str):
        texts.append(hits["context"][:6000])
    return texts


# ---------------------------------------------------------------------------
# The optional end-to-end agent
# ---------------------------------------------------------------------------

class Llm:
    """An OpenAI-compatible chat client with a disk cache and a spend guard."""

    def __init__(self, base_url: str, model: str, key: str, cache: Path, allow_paid: bool):
        self.base = base_url.rstrip("/")
        self.model = model
        self.key = key
        self.cache = cache
        self.allow_paid = allow_paid
        host = urllib.parse.urlparse(self.base).hostname or ""
        local = host in ("localhost", "127.0.0.1", "::1") or host.startswith("127.")
        if not local and not allow_paid:
            raise SystemExit(
                f"--llm-base-url {self.base} is not loopback. A paid run needs an explicit "
                "--allow-paid after the owner's go-ahead (AGENTS.md rule 7)."
            )
        cache.mkdir(parents=True, exist_ok=True)

    def complete(self, messages: list[dict], temperature: float = 0.0) -> tuple[str, str]:
        """`(text, source)` where source is `cache` or `api`."""
        key = hashlib.sha256(
            (self.model + "\0" + json.dumps(messages, ensure_ascii=False, sort_keys=True)).encode()
        ).hexdigest()
        path = self.cache / key[:2] / f"{key}.json"
        if path.exists():
            return json.loads(path.read_text()).get("text", ""), "cache"
        payload = {"model": self.model, "messages": messages, "temperature": temperature, "max_tokens": 800}
        request = urllib.request.Request(
            f"{self.base}/chat/completions",
            data=json.dumps(payload).encode(),
            headers={"Content-Type": "application/json", "Authorization": f"Bearer {self.key}"},
            method="POST",
        )
        with urllib.request.urlopen(request, timeout=180) as response:
            data = json.loads(response.read().decode())
        text = data["choices"][0]["message"].get("content") or ""
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps({"model": self.model, "messages": messages, "text": text}, ensure_ascii=False))
        return text, "api"


def json_block(text: str) -> dict | None:
    """The first JSON object in a model answer, fences and prose tolerated."""
    if not text:
        return None
    fence = re.search(r"```(?:json)?\s*(.*?)```", text, re.S)
    candidates = [fence.group(1)] if fence else []
    start = text.find("{")
    while start != -1:
        depth = 0
        for i in range(start, len(text)):
            if text[i] == "{":
                depth += 1
            elif text[i] == "}":
                depth -= 1
                if depth == 0:
                    candidates.append(text[start : i + 1])
                    break
        break
    for candidate in candidates:
        try:
            value = json.loads(candidate)
            if isinstance(value, dict):
                return value
        except json.JSONDecodeError:
            continue
    return None


def table_brief(api: Api, table_id: str) -> str:
    status, payload = api.get(f"/v1/tables/{urllib.parse.quote(table_id, safe='')}")
    if status != 200 or not isinstance(payload, dict):
        return f"- {table_id}"
    record = payload.get("table", payload)
    header = _header_names(record)[:25]
    manifest = record.get("manifest") or {}
    derived = manifest.get("derived_rows") if isinstance(manifest, dict) else None
    return (
        f"- tableId: {table_id}\n"
        f"  title: {record.get('title') or record.get('docTitle') or ''}\n"
        f"  sheet: {record.get('sheet')}\n"
        f"  columns: {', '.join(header)}\n"
        f"  derived rows: {derived if derived is not None else 'unknown'}"
    )


def agent_answer(
    llm: Llm,
    api: Api,
    question: dict,
    hits: dict,
    candidate_tables: list[str],
) -> dict:
    """The two arms for one question."""
    q = question["question"]
    text_context = "\n\n".join(hit_texts(hits, 5))[:9000]
    text_messages = [
        {
            "role": "system",
            "content": (
                "You answer data questions from retrieved document text. If the text holds a "
                "table, add the numbers carefully and do not count a total row twice. Reply with "
                "the answer only: a number, or the asked-for name, in one short line."
            ),
        },
        {"role": "user", "content": f"Question: {q}\n\nRetrieved text:\n{text_context or '(nothing)'}"},
    ]
    try:
        text_answer, text_source = llm.complete(text_messages)
    except Exception as error:  # noqa: BLE001 - an LLM failure is data
        text_answer, text_source = "", f"error: {type(error).__name__}: {error}"

    briefs = "\n".join(table_brief(api, table_id) for table_id in candidate_tables[:3])
    query_messages = [
        {
            "role": "system",
            "content": (
                "You turn a question about a table into a typed query. Reply with JSON only: "
                '{"tableId": "...", "query": {"select": [...], "filter": [...], "group_by": [...], '
                '"order_by": [...], "limit": 10}}. select items are {"column": name} or '
                '{"agg": "sum|avg|min|max|count|count_distinct", "column": name, "as": alias}. '
                "filter items are {column, op: =|!=|>|>=|<|<=|contains|in|between, value}. "
                "Use only the listed column names. Subtotal rows are excluded by default; do not "
                "add them. Never sum a code column. For a total, leave filter empty."
            ),
        },
        {"role": "user", "content": f"Question: {q}\n\nTables:\n{briefs or '(none)'}"},
    ]
    arm = {"a": text_answer, "aSource": text_source, "b": "", "bQuery": None, "bTable": None, "bErrors": []}
    try:
        plan_text, _ = llm.complete(query_messages)
        plan = json_block(plan_text) or {}
        table_id = plan.get("tableId") or (candidate_tables[0] if candidate_tables else None)
        query = plan.get("query")
        if not table_id or not isinstance(query, dict):
            arm["bErrors"].append("no tableId/query in the model's JSON")
            return arm
        arm["bTable"] = table_id
        arm["bQuery"] = query
        status, result = api.query_table(table_id, query)
        if status != 200:
            arm["bErrors"].append(f"query HTTP {status}: {str(result)[:200]}")
            return arm
        answer_messages = [
            {
                "role": "system",
                "content": "State the answer to the question from the query result. One short line; a number only.",
            },
            {
                "role": "user",
                "content": (
                    f"Question: {q}\n\nThe query returned:\n"
                    + json.dumps(result, ensure_ascii=False)[:4000]
                ),
            },
        ]
        arm["b"], _ = llm.complete(answer_messages)
    except Exception as error:  # noqa: BLE001
        arm["bErrors"].append(f"{type(error).__name__}: {error}")
    return arm


def text_matches(gold, text: str, tolerance: float) -> bool:
    """Does a model's prose answer match the gold answer?"""
    if not text:
        return False
    if isinstance(gold, list):
        return all(
            normalize_text(item.get("label")) in normalize_text(text)
            for item in gold
            if isinstance(item, dict) and "label" in item
        ) and answers_match(gold, text, tolerance)
    if isinstance(gold, dict) and "label" in gold:
        label_ok = normalize_text(gold["label"]) in normalize_text(text)
        value_ok = any(_close(float(gold["value"]), v, tolerance) for v in numbers_in(text))
        return label_ok and value_ok
    if isinstance(gold, (int, float)) and not isinstance(gold, bool):
        if any(_close(float(gold), v, tolerance) for v in numbers_in(text)):
            return True
        return normalize_text(gold) in normalize_text(text)
    return normalize_text(gold) in normalize_text(text)


# ---------------------------------------------------------------------------
# Statistics
# ---------------------------------------------------------------------------


def mean_ci(values: list[float], resamples: int, seed: int = 7) -> tuple[float, float, float, int]:
    n = len(values)
    if n == 0:
        return (0.0, 0.0, 0.0, 0)
    mean = sum(values) / n
    rng = random.Random(seed)
    boots = sorted(sum(values[rng.randrange(n)] for _ in range(n)) / n for _ in range(resamples))
    return mean, boots[int(0.025 * resamples)], boots[max(0, int(0.975 * resamples) - 1)], n


def slice_rows(questions: list[dict]) -> list[tuple[str, list[str]]]:
    """The slices every score is reported over."""
    templates = sorted({q["template"] for q in questions})
    langs = sorted({q["lang"] for q in questions})
    traps = sorted({q["trap"] for q in questions if q.get("trap")})
    slices = [("all", [q["id"] for q in questions])]
    slices += [(f"template:{t}", [q["id"] for q in questions if q["template"] == t]) for t in templates]
    slices += [(f"lang:{lang}", [q["id"] for q in questions if q["lang"] == lang]) for lang in langs]
    slices += [(f"trap:{trap}", [q["id"] for q in questions if q.get("trap") == trap]) for trap in traps]
    return slices


def summarise(questions: list[dict], per_question: dict[str, dict], key: str, resamples: int) -> dict:
    """Overall and per-slice accuracy with 95% bootstrap CIs."""
    def value(qid: str) -> float:
        row = per_question.get(qid, {})
        return 1.0 if row.get(f"{key}_ok") else 0.0

    out = {}
    for name, ids in slice_rows(questions):
        if not ids:
            continue
        mean, lo, hi, n = mean_ci([value(qid) for qid in ids], resamples)
        out[name] = {"n": n, "mean": mean, "ci_low": lo, "ci_high": hi}
    return out


def paired_delta(per_question: dict[str, dict], ids: list[str], resamples: int) -> dict:
    diffs = []
    for qid in ids:
        row = per_question.get(qid, {})
        if row.get("agent_a_ok") is None or row.get("agent_b_ok") is None:
            continue
        diffs.append((1.0 if row["agent_b_ok"] else 0.0) - (1.0 if row["agent_a_ok"] else 0.0))
    mean, lo, hi, n = mean_ci(diffs, resamples, seed=11)
    return {"n": n, "mean": mean, "ci_low": lo, "ci_high": hi}


# ---------------------------------------------------------------------------
# The run
# ---------------------------------------------------------------------------


def run(args) -> int:
    dataset: Path = args.dataset
    out: Path = args.out or dataset / "runs" / dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    out.mkdir(parents=True, exist_ok=True)
    questions = [json.loads(line) for line in (dataset / "queries.jsonl").read_text().splitlines() if line.strip()]
    sources = {
        json.loads(line)["file"]: json.loads(line)
        for line in (dataset / "sources.jsonl").read_text().splitlines()
        if line.strip()
    }
    manifest = json.loads((dataset / "manifest.json").read_text()) if (dataset / "manifest.json").exists() else {}
    tables_by_file = {t["file"]: t for t in manifest.get("tables", [])}
    for question in questions:
        table = tables_by_file.get(question["file"])
        if table:
            question.setdefault("gold_header", table["header"])
            question.setdefault("gold_title", table.get("doc_title") or table.get("title"))
    if args.limit:
        questions = questions[: args.limit]

    api = Api(args.base_url, args.key, args.timeout)
    health, _ = api.get("/health")
    if health != 200:
        print(f"error: {args.base_url}/health answered HTTP {health}", file=sys.stderr)
        return 2

    print(f"ingesting {len(sources)} files into {args.base_url} …")
    ingest_result = ingest(api, dataset, out, args.ingest_timeout, args.reingest_failed)
    state = ingest_result["state"]
    mapping, map_details, map_problems = map_tables(api, questions, state)

    per: dict[str, dict] = {}
    failures: dict[str, int] = {}
    failure_examples: dict[str, list[str]] = {}
    search_cache: dict[str, dict] = {}
    ids_by_file: dict[str, set[str]] = {}
    for (file, _index), table_id in mapping.items():
        ids_by_file.setdefault(file, set()).add(table_id)

    def fail(kind: str, example: str):
        failures[kind] = failures.get(kind, 0) + 1
        failure_examples.setdefault(kind, [])
        if len(failure_examples[kind]) < 5:
            failure_examples[kind].append(example)

    for index, question in enumerate(questions, 1):
        qid = question["id"]
        key = (question["file"], question.get("grid_index", 0))
        row = {
            "id": qid,
            "file": question["file"],
            "lang": question["lang"],
            "template": question["template"],
            "trap": question.get("trap"),
            "question": question["question"],
            "tableId": mapping.get(key),
            "retrieval_ok": False,
            "execution_ok": False,
            "execution_remapped_ok": False,
        }
        gold_table = mapping.get(key)
        if not gold_table:
            fail("unmatched_table", f"{qid}: {question['file']} has no table in the API")
            per[qid] = row
            continue

        hits = search_cache.get(qid)
        if hits is None:
            status, hits = api.post(
                "/v1/search", {"query": question["question"], "maxChunks": args.max_chunks}
            )
            if status != 200:
                fail("search_error", f"{qid}: HTTP {status}: {str(hits)[:160]}")
                hits = {}
            search_cache[qid] = hits
        ids = table_ids_of(hits)
        row["retrieved_tables"] = ids[: args.max_chunks]
        # A generated DOCX and the spreadsheet it came from hold the same table:
        # either is the right table for retrieval (execution below still runs on
        # this question's own table, so the DOCX is graded as a DOCX).
        acceptable = set(ids_by_file.get(question["file"], set()))
        for alternative in question.get("alt_files", []):
            acceptable |= ids_by_file.get(alternative, set())
        row["gold_tables"] = sorted(acceptable)
        row["retrieval_ok"] = bool(acceptable & set(ids[: args.top_k]))
        if not ids:
            fail("search_no_tables", f"{qid}: /v1/search returned no tableId")

        status, result = api.query_table(gold_table, question["gold_query"])
        if status == 200:
            got = answer_of(result)
            row["execution_answer"] = got
            row["execution_ok"] = answers_match(question["gold_answer"], got, question["tolerance"])
            if not row["execution_ok"]:
                fail("execution_mismatch", f"{qid}: gold {question['gold_answer']!r} got {got!r}")
        else:
            error = result.get("error") if isinstance(result, dict) else result
            row["execution_error"] = error
            if status == 400:
                fail("query_rejected", f"{qid}: {str(error)[:160]}")
            else:
                fail("query_api_error", f"{qid}: HTTP {status}: {str(error)[:160]}")
            # Retry once with gold column names remapped onto the API's header,
            # so a naming difference is reported, not mistaken for a wrong answer.
            if isinstance(error, dict) and "unknown_column" in json.dumps(error):
                details = map_details.get(key) or {}
                record_status, record = api.get(f"/v1/tables/{urllib.parse.quote(gold_table, safe='')}")
                if record_status == 200 and isinstance(record, dict):
                    names = _header_names(record.get("table", record))
                    remapped = _remap_query(question["gold_query"], names)
                    if remapped is not None:
                        status2, result2 = api.query_table(gold_table, remapped)
                        if status2 == 200:
                            got2 = answer_of(result2)
                            row["execution_remapped_ok"] = answers_match(
                                question["gold_answer"], got2, question["tolerance"]
                            )
                            row["execution_remapped_query"] = remapped
                            row["execution_remapped_answer"] = got2
                            if row["execution_remapped_ok"]:
                                fail("column_name_drift", f"{qid}: gold columns differ from the manifest")
        per[qid] = row

        if index % 25 == 0:
            print(f"  {index}/{len(questions)} questions", flush=True)

    if args.agent and args.llm_model:
        llm = Llm(args.llm_base_url, args.llm_model, args.llm_key, out / "llm_cache", args.allow_paid)
        for position, question in enumerate(questions, 1):
            qid = question["id"]
            row = per[qid]
            hits = search_cache.get(qid) or {}
            ids = table_ids_of(hits)[: args.top_k]
            if not ids and row.get("tableId"):
                ids = [row["tableId"]]
            attempt = agent_answer(llm, api, question, hits, ids)
            row["agent_a"] = attempt["a"]
            row["agent_a_source"] = attempt["aSource"]
            row["agent_a_ok"] = text_matches(question["gold_answer"], attempt["a"], question["tolerance"])
            row["agent_b"] = attempt["b"]
            row["agent_b_query"] = attempt["bQuery"]
            row["agent_b_table"] = attempt["bTable"]
            row["agent_b_errors"] = attempt["bErrors"]
            row["agent_b_ok"] = text_matches(question["gold_answer"], attempt["b"], question["tolerance"])
            if attempt["bErrors"]:
                fail("agent_error", f"{qid}: {attempt['bErrors'][0]}")
            if position % 10 == 0:
                print(f"  agent {position}/{len(questions)}", flush=True)

    retrieval = summarise(questions, per, "retrieval", args.resamples)
    execution = summarise(questions, per, "execution", args.resamples)
    remapped = summarise(questions, per, "execution_remapped", args.resamples)
    agent_a = summarise(questions, per, "agent_a", args.resamples) if args.agent else {}
    agent_b = summarise(questions, per, "agent_b", args.resamples) if args.agent else {}
    all_ids = [q["id"] for q in questions]
    delta = paired_delta(per, all_ids, args.resamples) if args.agent else {}
    delta_by_slice = (
        {name: paired_delta(per, ids, args.resamples) for name, ids in slice_rows(questions) if len(ids) >= 5}
        if args.agent
        else {}
    )

    ingest_failed = [name for name, entry in state.items() if entry.get("status") != "ready"]
    results = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
        "dataset": str(dataset),
        "base_url": args.base_url,
        "config": {
            "top_k": args.top_k,
            "max_chunks": args.max_chunks,
            "resamples": args.resamples,
            "agent": bool(args.agent),
            "llm_model": args.llm_model if args.agent else None,
            "llm_base_url": args.llm_base_url if args.agent else None,
        },
        "ingest": {
            "files": len(sources),
            "ready": sum(1 for entry in state.values() if entry.get("status") == "ready"),
            "failed": ingest_failed,
            "problems": ingest_result["problems"],
        },
        "tables": {
            "matched": len(mapping),
            "documents_with_tables": len({v["tableId"] for v in map_details.values()}),
            "problems": map_problems,
        },
        "retrieval": retrieval,
        "execution": execution,
        "execution_remapped": remapped,
        "agent": {
            "text": agent_a,
            "table": agent_b,
            "delta": delta,
            "delta_by_slice": delta_by_slice,
        }
        if args.agent
        else None,
        "failures": failures,
        "failure_examples": failure_examples,
        "targets": TARGETS,
    }
    (out / "results.json").write_text(json.dumps(results, ensure_ascii=False, indent=2) + "\n")
    (out / "per_question.jsonl").write_text(
        "".join(json.dumps(per[q["id"]], ensure_ascii=False) + "\n" for q in questions)
    )
    (out / "report.md").write_text(report(results, per, questions))
    print(f"\nwrote {out}/report.md, results.json, per_question.jsonl")
    return 0


def _remap_query(query: dict, names: list[str]) -> dict | None:
    """Rewrite a query's column names onto the manifest's, when they differ only
    in whitespace, case or separator."""
    by_norm = {re.sub(r"[^a-z0-9]+", " ", n.casefold()).strip(): n for n in names}
    changed = False

    def fix(column):
        nonlocal changed
        if column in names:
            return column
        norm = re.sub(r"[^a-z0-9]+", " ", str(column).casefold()).strip()
        if norm in by_norm:
            changed = True
            return by_norm[norm]
        return column

    out = json.loads(json.dumps(query))
    for item in out.get("select", []):
        if item.get("column"):
            item["column"] = fix(item["column"])
    for item in out.get("filter", []):
        item["column"] = fix(item["column"])
    out["group_by"] = [fix(c) for c in out.get("group_by", [])]
    for item in out.get("order_by", []):
        item["key"] = fix(item["key"])
    return out if changed else None


# ---------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------


def pct(entry: dict | None) -> str:
    if not entry:
        return "—"
    return f"{entry['mean']:.3f} [{entry['ci_low']:.3f}, {entry['ci_high']:.3f}] (n={entry['n']})"


def _table_block(title: str, scores: dict) -> str:
    """A markdown table of overall + per-template + per-language + per-trap rows."""
    lines = [f"### {title}", "", "| slice | accuracy [95% CI] |", "|---|---|"]
    order = ["all"] + [k for k in scores if k != "all"]
    for name in order:
        lines.append(f"| {name} | {pct(scores.get(name))} |")
    return "\n".join(lines)


def report(results: dict, per: dict, questions: list[dict]) -> str:
    counts: dict[str, int] = {}
    for question in questions:
        counts[question["template"]] = counts.get(question["template"], 0) + 1
    lines = [
        "# tables-v1 — live results",
        "",
        f"Run {results['generated_at']} against `{results['base_url']}`, dataset "
        f"`{results['dataset']}`.",
        "",
        "| | |",
        "|---|---|",
        f"| questions | {len(questions)} |",
        f"| files ingested, ready | {results['ingest']['ready']} / {results['ingest']['files']} |",
        f"| grids mapped to a tableId | {results['tables']['matched']} |",
        f"| max retrieval candidates | {results['config']['max_chunks']} |",
        "",
        "## Targets",
        "",
        "| what | target | measured |",
        "|---|---:|---|",
    ]
    ret = results["retrieval"].get("all")
    exe = results["execution"].get("all")
    trap = results["execution"].get("trap:subtotal")
    lines += [
        f"| table retrieval, top {results['config']['top_k']} | {TARGETS['retrieval_top5']:.2f} | "
        f"{ret['mean']:.3f} |" if ret else "| table retrieval | — | — |",
        f"| execution, all | {TARGETS['execution_correct']:.2f} | {exe['mean']:.3f} |"
        if exe
        else "| execution | — | — |",
        f"| execution, subtotal traps | {TARGETS['subtotal_trap_correct']:.2f} | {trap['mean']:.3f} |"
        if trap
        else "| execution, subtotal traps | — | — |",
    ]
    if results["agent"]:
        arm_b = results["agent"]["table"].get("all")
        delta = results["agent"]["delta"]
        lines += [
            f"| end to end, table arm | {TARGETS['end_to_end_table']:.2f} | {arm_b['mean']:.3f} |"
            if arm_b
            else "| end to end, table arm | — | — |",
            f"| end to end, (b) − (a) | +{TARGETS['end_to_end_delta']:.2f} | "
            f"{delta['mean']:+.3f} [{delta['ci_low']:+.3f}, {delta['ci_high']:+.3f}] (n={delta['n']}) |"
            if delta
            else "| end to end, (b) − (a) | — | — |",
        ]
    lines += ["", "## Questions", "", "| template | n |", "|---|---:|"]
    for name in sorted(counts):
        lines.append(f"| {name} | {counts[name]} |")
    lines += ["", "## Score 1 — table retrieval", "", _table_block("Gold table in the top 5", results["retrieval"])]
    lines += ["", "## Score 2 — execution", "", _table_block("Gold query through the API", results["execution"])]
    if results["execution_remapped"]:
        lines += ["", "Execution after remapping column names onto the manifest's (naming drift only):", ""]
        lines += [f"- {name}: {pct(entry)}" for name, entry in results["execution_remapped"].items() if name in ("all", "trap:subtotal")]
    if results["agent"]:
        lines += ["", "## Score 3 — end to end", ""]
        lines += [_table_block("(a) answer from retrieved text", results["agent"]["text"]), ""]
        lines += [_table_block("(b) table summary → TableQuery → API", results["agent"]["table"]), ""]
        delta_rows = results["agent"].get("delta_by_slice") or {}
        lines += [
            "Paired delta (b) − (a), per slice (n ≥ 5):",
            "",
            "| slice | Δ [95% CI] |",
            "|---|---|",
        ]
        for name, entry in delta_rows.items():
            lines.append(
                f"| {name} | {entry['mean']:+.3f} [{entry['ci_low']:+.3f}, {entry['ci_high']:+.3f}] "
                f"(n={entry['n']}) |"
            )
    lines += ["", "## Failures, counted", ""]
    if results["failures"]:
        lines += ["| kind | n | examples |", "|---|---:|---|"]
        for kind, count in sorted(results["failures"].items()):
            examples = "<br>".join(results["failure_examples"].get(kind, [])[:3]).replace("|", "\\|")
            lines.append(f"| {kind} | {count} | {examples} |")
    else:
        lines.append("None. (A failed search or query would be counted here, never shown as an empty result.)")
    if results["ingest"]["failed"]:
        lines += ["", "Ingest failures:", ""] + [f"- {name}" for name in results["ingest"]["failed"]]
    lines += [
        "",
        "## How to read this",
        "",
        "Every number is a mean over questions with a 95% bootstrap interval (2000 resamples, seed 7).",
        "`execution` counts a question correct only when the query returns within the gold tolerance; a",
        "400, a 5xx or an unmatched table is a failure, counted in **Failures** and never dropped.",
        "Subtotal traps are the questions where adding the `Jumlah` row would give a different total;",
        "code traps ask for distinct codes and fail loudly if a pipeline sums them.",
        "",
    ]
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--dataset", type=Path, default=Path.home() / "kbdata" / "tables-v1",
                        help="dataset directory built by import_tables.py")
    parser.add_argument("--base-url", default="http://127.0.0.1:8080", help="kb-server base URL")
    parser.add_argument("--key", default=os.environ.get("KB_API_KEY", ""), help="API key (or KB_API_KEY)")
    parser.add_argument("--out", type=Path, default=None, help="output directory (default: <dataset>/runs/<timestamp>)")
    parser.add_argument("--limit", type=int, default=0, help="only the first N questions (smoke test)")
    parser.add_argument("--top-k", type=int, default=DEFAULT_TOPK, help="retrieval cut-off (default 5)")
    parser.add_argument("--max-chunks", type=int, default=DEFAULT_TOPK,
                        help="chunks requested from /v1/search (default 5)")
    parser.add_argument("--timeout", type=float, default=120.0, help="per-request timeout, seconds")
    parser.add_argument("--ingest-timeout", type=float, default=900.0, help="per-document ingest timeout, seconds")
    parser.add_argument("--reingest-failed", action="store_true", help="re-upload files whose ingest failed")
    parser.add_argument("--resamples", type=int, default=2000, help="bootstrap resamples")
    parser.add_argument("--agent", action="store_true", help="run the end-to-end model arms")
    parser.add_argument("--llm-base-url", default="http://127.0.0.1:8080/v1",
                        help="OpenAI-compatible base URL (loopback unless --allow-paid)")
    parser.add_argument("--llm-model", default="", help="chat model name (no agent run without it)")
    parser.add_argument("--llm-key", default=os.environ.get("KB_LLM_KEY", "local"), help="LLM API key")
    parser.add_argument("--allow-paid", action="store_true",
                        help="permit a non-loopback LLM endpoint (needs the owner's go-ahead)")
    parser.add_argument("--self-check", action="store_true",
                        help="run import_tables.py --self-check on the dataset and exit (needs "
                             "openpyxl and python-docx: uv run --with openpyxl --with python-docx)")
    args = parser.parse_args()
    if args.self_check:
        sys.path.insert(0, str(Path(__file__).resolve().parent))
        import import_tables

        return import_tables.self_check(args.dataset)
    if not args.key:
        print("error: --key or KB_API_KEY is required", file=sys.stderr)
        return 2
    if args.agent and not args.llm_model:
        print(
            "error: --agent needs --llm-model (a local OpenAI-compatible model, e.g. "
            "qwen2.5-7b-instruct on llama.cpp). No paid model without --allow-paid.",
            file=sys.stderr,
        )
        return 2
    return run(args)


if __name__ == "__main__":
    sys.exit(main())
