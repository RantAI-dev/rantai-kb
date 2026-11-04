#!/usr/bin/env python3
"""Write one summary per community, cached on disk, with a hard dollar ceiling.

This is the only step in the EVAL-3 prototype that can spend money, so every
rule from AGENTS.md #7 is enforced here rather than left to the caller:

* **Cache first.** A response is keyed by `sha256(model + prompt)` under
  `$KB_CACHE_DIR/community-summaries/<model>/<hash>.json` (default
  `~/.cache/kb-cache`). A second run over the same communities costs nothing;
  the run prints how many were served from cache.
* **Estimate before calling.** Tokens are estimated as `chars / 4` (prompt) plus
  `--max-tokens` (completion), priced from the OpenRouter table below, which was
  read from <https://openrouter.ai/api/v1/models> on 2026-11-04. `--dry-run`
  prints the table and writes nothing; `--max-usd` is a ceiling that aborts the
  whole run *before* the first request if the estimate exceeds it.
* **Paid runs need an explicit approval flag.** A model id not ending in
  `:free` refuses to run without `--i-have-approval`, and more than 20
  communities needs it too. The planner, not this script, decides to spend.

`--no-llm` is the zero-cost lower baseline: each "summary" is the community's
concatenated entity names. It exercises the whole arm without a model, and it is
a real retrieval baseline in its own right (it asks "does knowing which entities
cluster together help?" separately from "does a prose summary help?").

    # free: what would it cost?
    python3 community_summarise.py --communities communities.json --dry-run \\
        --model deepseek/deepseek-v4-flash

    # zero cost, no model at all
    python3 community_summarise.py --communities communities.json \\
        --no-llm --out summaries.json
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Dict, Iterable, List, Tuple

BASE_URL = "https://openrouter.ai/api/v1"
CHAT_URL = f"{BASE_URL}/chat/completions"

# USD per 1,000,000 tokens. Source: GET https://openrouter.ai/api/v1/models,
# fetched 2026-11-04. Keys are OpenRouter model ids; ":free" variants are $0.
# This table only feeds the *estimate*; the provider bills the real amount.
PRICES: Dict[str, Tuple[float, float]] = {
    "deepseek/deepseek-v4-flash": (0.091, 0.182),
    "qwen/qwen3.7-flash": (0.03, 0.13),
    "google/gemini-3.8-flash": (0.825, 4.125),
    # older flash-class ids kept because they are what a 2026-10 eval used
    "qwen/qwen3.5-flash-02-23": (0.065, 0.26),
    "google/gemini-2.5-flash": (0.30, 2.50),
}


def default_cache_dir() -> Path:
    env = os.environ.get("KB_CACHE_DIR") or os.environ.get("KB_STACK_CACHE_DIR")
    return Path(env).expanduser() if env else Path.home() / ".cache" / "kb-cache"


def price_for(model: str) -> Tuple[float, float]:
    if model.endswith(":free"):
        return (0.0, 0.0)
    if model in PRICES:
        return PRICES[model]
    raise SystemExit(
        f"no price for {model!r}; add it to PRICES from "
        "https://openrouter.ai/api/v1/models (or pass a :free model id)"
    )


def estimate_tokens(text: str) -> int:
    """chars/4, stated in the README as an estimate, never as a measurement."""
    return max(1, len(text) // 4)


def summarise_prompt(community: dict, texts: Dict[str, str], chars_per_page: int) -> str:
    """One prompt per community. Deterministic, so the cache key is stable."""
    entities = ", ".join(e["name"] for e in community["entities"][:60])
    pages = community["pages"][:12]
    excerpts = []
    for page in pages:
        text = texts.get(page, "")
        collapsed = " ".join(text.split())[:chars_per_page]
        excerpts.append(f"[{page}] {collapsed}")
    body = "\n".join(excerpts) if excerpts else "(no page text available)"
    return (
        "You are summarising one community of a document collection so that a "
        "search engine can answer broad questions (\"what are the main themes\", "
        "\"list all the ...\") from the summary instead of from single pages.\n\n"
        f"Community entities ({len(community['entities'])}): {entities}\n\n"
        f"Pages in this community ({len(community['pages'])}): "
        f"{', '.join(community['pages'][:40])}\n\n"
        f"Page excerpts:\n{body}\n\n"
        "Write 120-180 words of plain prose. Say what this community is about, "
        "its main themes, the entities and numbers it centres on, and which of "
        "the listed pages carry which theme. Use only the material above; do "
        "not invent facts, names or figures."
    )


def cache_path(cache_dir: Path, model: str, prompt: str) -> Path:
    safe_model = model.replace("/", "__")
    digest = hashlib.sha256((model + "\x00" + prompt).encode("utf-8")).hexdigest()
    return cache_dir / "community-summaries" / safe_model / f"{digest}.json"


def chat_cached(
    model: str,
    prompt: str,
    max_tokens: int,
    cache_dir: Path,
    api_key: str,
    timeout: int = 120,
) -> Tuple[str, dict, bool]:
    """Return `(content, usage, from_cache)`. Network runs are retried twice."""
    path = cache_path(cache_dir, model, prompt)
    if path.is_file():
        record = json.loads(path.read_text(encoding="utf-8"))
        # An empty answer is not a summary: a reasoning model that spent its
        # whole budget thinking returns `content: null`. Never serve one.
        if record.get("content"):
            return record["content"], record.get("usage", {}), True

    payload = json.dumps(
        {
            "model": model,
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": max_tokens,
            "temperature": 0.0,
            # Reasoning tokens count against max_tokens; with them on, deepseek
            # v4 flash spent the whole budget thinking on 95 of 159 communities
            # and returned no text. A summary needs no chain of thought.
            "reasoning": {"enabled": False},
        }
    ).encode("utf-8")
    headers = {
        "Authorization": f"Bearer {api_key}",
        "Content-Type": "application/json",
        "HTTP-Referer": "https://github.com/rantai-kb",
        "X-Title": "rantai-kb eval community summaries",
    }
    last_error: Exception | None = None
    for attempt in range(3):
        try:
            request = urllib.request.Request(CHAT_URL, data=payload, headers=headers)
            with urllib.request.urlopen(request, timeout=timeout) as response:
                body = json.loads(response.read().decode("utf-8"))
            content = body["choices"][0]["message"]["content"]
            usage = body.get("usage", {})
            if not content:
                raise KeyError(f"empty content (usage {usage})")
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(
                json.dumps(
                    {"model": model, "prompt_sha256": hashlib.sha256(prompt.encode()).hexdigest(),
                     "content": content, "usage": usage},
                    ensure_ascii=False,
                ),
                encoding="utf-8",
            )
            return content, usage, False
        except (urllib.error.URLError, KeyError, json.JSONDecodeError, TimeoutError) as exc:
            last_error = exc
            time.sleep(2 * (attempt + 1))
    raise SystemExit(f"OpenRouter call failed after 3 attempts: {last_error}")


def load_texts(dataset: Path, pages: Iterable[str], cache_dir: Path) -> Dict[str, str]:
    """Page text from the shared page cache; read-only here (community_build fills it)."""
    dataset_name = dataset.name
    out: Dict[str, str] = {}
    missing = 0
    for page in pages:
        doc, _, num = page.rpartition("#p")
        path = cache_dir / "pages" / dataset_name / f"{doc}#p{num}.txt"
        if path.is_file():
            out[page] = path.read_text(encoding="utf-8", errors="replace")
        else:
            missing += 1
            out[page] = ""
    if missing:
        print(
            f"warning: {missing} pages had no cached text under "
            f"{cache_dir / 'pages' / dataset_name}; run community_build.py first",
            file=sys.stderr,
        )
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--communities", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True, help="summaries.json to write")
    ap.add_argument("--dataset", type=Path, help="dataset dir (for cached page text)")
    ap.add_argument("--model", default="deepseek/deepseek-v4-flash")
    ap.add_argument("--max-usd", type=float, default=0.0, help="hard ceiling; 0 refuses any paid run")
    ap.add_argument("--max-tokens", type=int, default=320, help="completion budget per community")
    ap.add_argument("--chars-per-page", type=int, default=1200)
    ap.add_argument("--limit", type=int, default=0, help="only the first N communities (smoke test)")
    ap.add_argument("--min-pages", type=int, default=1,
                    help="skip communities with fewer pages (1-page communities dominate cost)")
    ap.add_argument("--dry-run", action="store_true", help="print the estimate, write nothing")
    ap.add_argument("--no-llm", action="store_true", help="summaries = concatenated entity names")
    ap.add_argument("--cache-dir", type=Path, default=default_cache_dir())
    ap.add_argument("--i-have-approval", action="store_true",
                    help="required for a paid model or more than 20 communities")
    args = ap.parse_args()

    data = json.loads(args.communities.read_text(encoding="utf-8"))
    communities: List[dict] = data["communities"]
    communities = [c for c in communities if len(c["pages"]) >= args.min_pages]
    if args.limit:
        communities = communities[: args.limit]

    if args.no_llm:
        summaries = [
            {
                "community_id": c["id"],
                "pages": c["pages"],
                "text": ", ".join(e["name"] for e in c["entities"]),
                "entities": [e["key"] for e in c["entities"]],
                "n_entities": len(c["entities"]),
                "n_pages": len(c["pages"]),
                "source": "entity-names",
            }
            for c in communities
        ]
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(
            json.dumps(
                {"dataset": data.get("dataset"), "model": "none (entity names)",
                 "mode": "no-llm", "summaries": summaries},
                ensure_ascii=False,
            ),
            encoding="utf-8",
        )
        print(f"wrote {len(summaries)} entity-name summaries to {args.out}")
        return 0

    prompt_price, completion_price = price_for(args.model)
    texts: Dict[str, str] = {}
    if args.dataset:
        all_pages = [p for c in communities for p in c["pages"]]
        texts = load_texts(args.dataset, all_pages, args.cache_dir)

    rows = []
    total_prompt_chars = 0
    total_completion_tokens = 0
    cached = 0
    for community in communities:
        prompt = summarise_prompt(community, texts, args.chars_per_page)
        prompt_tokens = estimate_tokens(prompt)
        total_prompt_chars += len(prompt)
        total_completion_tokens += args.max_tokens
        is_cached = cache_path(args.cache_dir, args.model, prompt).is_file()
        cached += int(is_cached)
        cost = (
            0.0
            if is_cached
            else prompt_tokens / 1_000_000 * prompt_price
            + args.max_tokens / 1_000_000 * completion_price
        )
        rows.append((community["id"], prompt_tokens, len(community["pages"]), is_cached, cost))

    total = sum(row[4] for row in rows)
    print(f"model: {args.model}   communities: {len(rows)}   cached: {cached}")
    print("| community | pages | est. prompt tokens | cached | est. USD (uncached) |")
    print("|---|---|---|---|---|")
    for community_id, tokens, pages, is_cached, cost in rows:
        print(f"| {community_id} | {pages} | {tokens} | {'yes' if is_cached else 'no'} | ${cost:.6f} |")
    print(
        f"\ntotal estimated prompt tokens (chars/4): {total_prompt_chars // 4}; "
        f"completion budget tokens: {total_completion_tokens}; "
        f"estimated cost of the uncached remainder: ${total:.4f}"
    )
    print("token counts are ESTIMATES (chars/4), prices are LOOKED UP 2026-11-04; "
          "the provider invoice is the only measured number")

    if args.dry_run:
        print("dry run: nothing written, no request made")
        return 0

    api_key = os.environ.get("OPENROUTER_API_KEY", "")
    if not api_key:
        raise SystemExit("OPENROUTER_API_KEY is not set; refusing to call a provider")
    if not args.model.endswith(":free") and not args.i_have_approval:
        raise SystemExit(
            f"{args.model} is a paid model; a paid ingest batch needs the planner's "
            "explicit approval (pass --i-have-approval once you have it)"
        )
    if len(communities) > 20 and not args.i_have_approval:
        raise SystemExit(
            f"{len(communities)} communities is a batch, not a smoke test; "
            "pass --i-have-approval to run it"
        )
    if total > args.max_usd + 1e-9:
        raise SystemExit(
            f"estimated ${total:.4f} exceeds --max-usd ${args.max_usd:.4f}; aborting before any call"
        )

    summaries = []
    spent_rows = 0
    for community in communities:
        prompt = summarise_prompt(community, texts, args.chars_per_page)
        content, usage, from_cache = chat_cached(
            args.model, prompt, args.max_tokens, args.cache_dir, api_key
        )
        spent_rows += int(not from_cache)
        summaries.append(
            {
                "community_id": community["id"],
                "pages": community["pages"],
                "text": content,
                "entities": [e["key"] for e in community["entities"]],
                "n_entities": len(community["entities"]),
                "n_pages": len(community["pages"]),
                "source": args.model,
                "usage": usage,
            }
        )

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(
        json.dumps(
            {"dataset": data.get("dataset"), "model": args.model,
             "mode": "llm", "summaries": summaries},
            ensure_ascii=False,
        ),
        encoding="utf-8",
    )
    print(f"wrote {len(summaries)} summaries to {args.out} ({spent_rows} fresh calls)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
