#!/usr/bin/env python3
"""Shared, dependency-free helpers for the EVAL-3 community-summary prototype.

Three things live here, and each of them mirrors something the Rust service
already does rather than inventing a second definition:

1. **The heuristic entity extractor.** `rust/crates/kb-inference/src/heuristic_extractor.rs`
   selects `kb_core::intelligence::heuristic` when `KB_ENTITY_EXTRACTOR=heuristic`;
   that module is the zero-spend option the graph arm is measured with. `extract_entities`
   below is a port of `heuristic::extract`'s surface passes (title-case runs,
   acronyms, quoted spans, number-with-unit quantities) and of `key_of`, the
   dedupe key. The Rust stopword list is *read from the Rust source at runtime*
   so the two cannot drift: there is no second copy to forget to update.
2. **The aggregative query gate.** `rust/crates/kb-core/src/query_class.rs` is the
   only query classifier in the service today, and it answers a different
   question: Natural vs Exact (identifier / quoted phrase). It has **no**
   aggregative class — that is RET-5, still `Idea`. `is_aggregative_query` below
   is therefore a stand-in, and `aggregative_subsets.py` measures how well it
   agrees with ViDoRe's human `query_types` labels instead of trusting it.
3. **TREC I/O and BM25** so the arm and the fuser rank pages the same way.
"""

from __future__ import annotations

import json
import math
import re
import sys
import unicodedata
from pathlib import Path
from typing import Dict, Iterable, List, Sequence, Tuple

SCRIPT_DIR = Path(__file__).resolve().parent
CRATES_DIR = SCRIPT_DIR.parents[1]  # rust/crates
HEURISTIC_RS = CRATES_DIR / "kb-core" / "src" / "intelligence" / "heuristic.rs"
QUERY_CLASS_RS = CRATES_DIR / "kb-core" / "src" / "query_class.rs"

MAX_RUN_WORDS = 6
MAX_WORD_LEN = 40
MIN_KEY_LEN = 3
MAX_ENTITIES_PER_PAGE = 512
MAX_QUERY_NGRAM = 2

# Typographic letters the Rust `push_folded` expands to two ASCII letters.
_EXPAND = {"æ": "ae", "Æ": "ae", "œ": "oe", "Œ": "oe", "ß": "ss"}

_stopwords_cache: set[str] | None = None


def load_stopwords() -> set[str]:
    """The Rust `STOPWORDS` list, parsed out of the Rust source.

    Parsing instead of copying is deliberate: a hand-copied list is a second
    source of truth that silently diverges. If the file moves, fail loudly.
    """
    global _stopwords_cache
    if _stopwords_cache is not None:
        return _stopwords_cache
    if not HEURISTIC_RS.is_file():
        raise SystemExit(f"cannot find the Rust heuristic extractor at {HEURISTIC_RS}")
    text = HEURISTIC_RS.read_text(encoding="utf-8")
    start = text.index("const STOPWORDS: &[&str] = &[")
    end = text.index("];", start)
    words = set(re.findall(r'"([^"]*)"', text[start:end]))
    if len(words) < 100:
        raise SystemExit(f"only {len(words)} stopwords parsed from {HEURISTIC_RS}")
    _stopwords_cache = words
    return words


UNITS = {
    "acre", "bab", "billion", "cm", "day", "days", "decade", "feet", "foot", "gr",
    "gram", "grams", "ha", "hectare", "hour", "hours", "inch", "juta", "kg", "km",
    "km/h", "kwh", "l", "mile", "miliar", "mm", "month", "months", "mph", "page",
    "percent", "ribu", "rupiah", "ton", "tonnes", "triliun", "year", "years", "yard",
}
CURRENCY_MARKERS = {"rp", "rp.", "usd", "idr", "$"}
CONNECTORS = {"de", "da", "del", "for", "of", "the", "van", "von"}


def key_of(name: str) -> str | None:
    """Port of `heuristic::key_of`: case, whitespace, punctuation, diacritics folded."""
    out: list[str] = []
    pending_space = False
    for ch in name:
        if ch.isalnum():
            if pending_space and out:
                out.append(" ")
                pending_space = False
            if ch in _EXPAND:
                out.append(_EXPAND[ch])
                continue
            folded = unicodedata.normalize("NFKD", ch)
            base = "".join(c for c in folded if not unicodedata.combining(c))
            out.append(base.lower())
        elif out:
            pending_space = True
    key = "".join(out).rstrip()
    if len(key) < 2:
        return None
    return key


def _is_generic(key: str) -> bool:
    return (
        len(key) < 3
        or key.isnumeric()
        or key in load_stopwords()
    )


class Token:
    __slots__ = ("text", "start", "length", "title", "acronym")

    def __init__(self, text: str, start: int) -> None:
        self.text = text
        self.start = start
        self.length = len(text)
        alpha = [c for c in text if c.isalpha()]
        self.title = bool(alpha) and alpha[0].isupper()
        self.acronym = (
            len(alpha) >= 2
            and all(c.isupper() for c in alpha)
            and not any(c.isdigit() for c in text)
            and 2 <= len(alpha) <= 8
        )


def tokenize(text: str) -> List[Token]:
    tokens: List[Token] = []
    buf: list[str] = []
    start = 0
    for offset, ch in enumerate(text):
        if ch.isalnum() or ch in "'\u2019":
            if not buf:
                start = offset
            buf.append(ch)
        elif buf:
            tokens.append(Token("".join(buf), start))
            buf = []
    if buf:
        tokens.append(Token("".join(buf), start))
    return tokens


def _is_word(token: Token) -> bool:
    return token.title and token.length <= MAX_WORD_LEN and token.text.lower() not in load_stopwords()


def _run_from(tokens: Sequence[Token], start: int) -> Tuple[List[int], int]:
    parts = [start]
    words = 1
    cursor = start + 1
    while cursor < len(tokens) and words < MAX_RUN_WORDS:
        prev = tokens[cursor - 1]
        gap = tokens[cursor].start - (prev.start + prev.length)
        if gap > 1:
            break
        if _is_word(tokens[cursor]):
            parts.append(cursor)
            words += 1
            cursor += 1
            continue
        lower = tokens[cursor].text.lower()
        nxt = tokens[cursor + 1] if cursor + 1 < len(tokens) else None
        next_gap = (
            nxt.start - (tokens[cursor].start + tokens[cursor].length) if nxt is not None else 99
        )
        if lower in CONNECTORS and nxt is not None and next_gap <= 1 and _is_word(nxt):
            parts.append(cursor)
            cursor += 1
            continue
        break
    return parts, words


def _push_surface(out: List[str], parts: Sequence[int], tokens: Sequence[Token]) -> None:
    surface = " ".join(tokens[i].text for i in parts)
    _push_one(out, surface)


def _push_one(out: List[str], surface: str) -> None:
    key = key_of(surface)
    if key is not None and len(key) >= MIN_KEY_LEN and not _is_generic(key):
        out.append(surface)


def _quoted(text: str) -> List[str]:
    out: List[str] = []
    i = 0
    while i < len(text):
        ch = text[i]
        close = {'"': '"', "\u201c": "\u201d", "\u00ab": "\u00bb"}.get(ch)
        if close is None:
            i += 1
            continue
        j = i + 1
        buf: list[str] = []
        closed = False
        while j < len(text):
            if text[j] == close:
                closed = True
                break
            if text[j] == "\n":
                break
            buf.append(text[j])
            j += 1
        i = j + 1
        if not closed or len(buf) < 3:
            continue
        trimmed = "".join(buf).strip()
        first, sep, rest = trimmed.partition(" ")
        body = rest if sep and first.lower() in load_stopwords() else trimmed
        if body:
            out.append(body)
    return out


def _quantities(tokens: Sequence[Token]) -> List[str]:
    out: List[str] = []
    for i, token in enumerate(tokens):
        nxt = tokens[i + 1] if i + 1 < len(tokens) else None
        starts_digit = token.text[:1].isdigit()
        if starts_digit and nxt is not None and nxt.text.lower() in UNITS:
            out.append(f"{token.text} {nxt.text}")
            continue
        if (
            not starts_digit
            and token.text.lower() in CURRENCY_MARKERS
            and nxt is not None
            and nxt.text[:1].isdigit()
        ):
            out.append(f"{token.text} {nxt.text}")
    return out


def _candidates(text: str) -> List[str]:
    tokens = tokenize(text)
    out: List[str] = []
    i = 0
    while i < len(tokens):
        if not _is_word(tokens[i]):
            i += 1
            continue
        parts, words = _run_from(tokens, i)
        end = parts[-1] if parts else i
        if words >= 2 or (words == 1 and not tokens[i].acronym):
            _push_surface(out, parts, tokens)
        i = end + 1
    for token in tokens:
        if token.acronym:
            _push_one(out, token.text)
    out.extend(_quantities(tokens))
    out.extend(_quoted(text))
    return out


def extract_entities(text: str) -> List[Tuple[str, str, int]]:
    """`(key, display, count)` per entity, ordered as `heuristic::extract` orders them."""
    counts: Dict[str, int] = {}
    display: Dict[str, str] = {}
    for surface in _candidates(text):
        key = key_of(surface)
        if key is None or _is_generic(key):
            continue
        counts[key] = counts.get(key, 0) + 1
        if key not in display:
            display[key] = " ".join(surface.split())
    ranked = sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))
    return [(key, display[key], n) for key, n in ranked[:MAX_ENTITIES_PER_PAGE]]


def query_terms(query: str, limit: int = 12) -> List[str]:
    """Port of `heuristic::query_terms`: longest phrase first, stopwords dropped."""
    tokens = [key_of(t.text) or "" for t in tokenize(query)]
    out: List[str] = []
    max_n = min(len(tokens), MAX_QUERY_NGRAM)
    for n in range(max_n, 0, -1):
        for i in range(0, len(tokens) - n + 1):
            window = tokens[i:i + n]
            phrase = key_of(" ".join(window))
            if phrase is None or len(phrase) < 3 or _is_generic(phrase):
                continue
            if all(w in load_stopwords() for w in window):
                continue
            if phrase not in out:
                out.append(phrase)
    return out[:limit]


# --- aggregative gate (stand-in for a class query_class.rs does not have) ----

# Why these cues: ViDoRe's own definition of the two types we target is
# "enumerative" (list/count every instance) and "multi-hop" (combine facts from
# several places). A production classifier belongs in kb-core next to
# `classify_query`; until RET-5 lands, this is a lexical proxy whose agreement
# with the human labels `aggregative_subsets.py` reports.
ENUMERATIVE_CUES = (
    "list", "enumerate", "name all", "name every", "name the", "which countries",
    "which sectors", "which member", "what are the", "what are all", "all the",
    "all of the", "every ", "how many", "how much", "main themes", "key themes",
    "main topics", "summary of", "summarise", "summarize", "overview of",
    "across the document", "across these", "compare", "comparison", "differences between",
    "trends in", "developments in",
)
MULTIHOP_CUES = (
    " and ", " after ", " before ", " while ", " whereas ", " combined with",
    " compared with", " compared to", " relationship between", "impact of",
    "effect of", "why did", "how did", "what impact", "contributed to",
)


def is_aggregative_query(query: str) -> bool:
    """Lexical stand-in for an aggregative label.

    `query_class.rs` (read 2026-11-04) defines only `QueryClass::Natural` and
    `QueryClass::Exact`; the module docs say the exact class exists for the
    relevance floor. There is nothing there to mirror for "broad question", so
    this function is a new rule set, not a port, and the README says so.
    """
    q = " " + " ".join(query.lower().split()) + " "
    if any(cue in q for cue in ENUMERATIVE_CUES):
        return True
    return sum(1 for cue in MULTIHOP_CUES if cue in q) >= 1 and q.count("?") + q.count(" ") > 12


# --- TREC and text helpers ---------------------------------------------------


def read_jsonl(path: Path) -> List[dict]:
    rows = []
    with path.open(encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def read_trec(path: Path) -> Dict[str, List[Tuple[int, str, float]]]:
    """`{qid: [(rank, docid, score)]}`, ranks coerced to int where possible."""
    runs: Dict[str, List[Tuple[int, str, float]]] = {}
    with path.open(encoding="utf-8") as fh:
        for line in fh:
            parts = line.split()
            if len(parts) < 4:
                continue
            qid, docid = parts[0], parts[2]
            try:
                rank = int(parts[3])
            except ValueError:
                rank = len(runs.get(qid, [])) + 1
            try:
                score = float(parts[4]) if len(parts) > 4 else 0.0
            except ValueError:
                score = 0.0
            runs.setdefault(qid, []).append((rank, docid, score))
    for qid in runs:
        runs[qid].sort(key=lambda row: row[0])
    return runs


def write_trec(path: Path, runs: Dict[str, List[Tuple[str, float]]], tag: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as fh:
        for qid in sorted(runs, key=lambda q: (len(q), q)):
            for rank, (docid, score) in enumerate(runs[qid], start=1):
                fh.write(f"{qid} Q0 {docid} {rank} {score:.6f} {tag}\n")


_STOP_FOR_BM25 = None


def bm25_tokens(text: str) -> List[str]:
    global _STOP_FOR_BM25
    if _STOP_FOR_BM25 is None:
        _STOP_FOR_BM25 = load_stopwords()
    return [
        t for t in re.findall(r"[a-z0-9]+", text.lower())
        if len(t) >= 2 and t not in _STOP_FOR_BM25
    ]


def bm25_scores(docs: Dict[str, str], query: str, k1: float = 1.2, b: float = 0.75) -> Dict[str, float]:
    """Okapi BM25 over short community summaries. Ties are broken by caller order."""
    qterms = bm25_tokens(query)
    if not qterms:
        return {doc_id: 0.0 for doc_id in docs}
    doc_tokens = {doc_id: bm25_tokens(text) for doc_id, text in docs.items()}
    n = len(doc_tokens)
    avgdl = sum(len(v) for v in doc_tokens.values()) / n if n else 0.0
    df: Dict[str, int] = {}
    for tokens in doc_tokens.values():
        for term in set(tokens) & set(qterms):
            df[term] = df.get(term, 0) + 1
    scores: Dict[str, float] = {}
    for doc_id, tokens in doc_tokens.items():
        tf: Dict[str, int] = {}
        for term in tokens:
            if term in qterms:
                tf[term] = tf.get(term, 0) + 1
        dl = len(tokens)
        score = 0.0
        for term, freq in tf.items():
            idf = math.log(1.0 + (n - df.get(term, 0) + 0.5) / (df.get(term, 0) + 0.5))
            denom = freq + k1 * (1.0 - b + b * dl / avgdl if avgdl else 1.0)
            score += idf * freq * (k1 + 1.0) / denom
        scores[doc_id] = score
    return scores


def warn(message: str) -> None:
    print(message, file=sys.stderr)


def page_sort_key(page: str) -> Tuple[str, int]:
    doc, _, num = page.rpartition("#p")
    try:
        return doc, int(num)
    except ValueError:
        return page, 0
