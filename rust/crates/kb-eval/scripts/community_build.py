#!/usr/bin/env python3
"""Build page/entity communities for the EVAL-3 "global mode" prototype.

What it does, in order:

1. **Page text.** `pdftotext -f N -l N` per page, cached under
   `$KB_CACHE_DIR/pages/<dataset>/<doc>#p<N>.txt`. Per page rather than whole
   document so a re-run after a failed page does not redo the book, and so the
   page ids match the qrels (`<doc>#p<N>`, N 1-based).
2. **Entities per page.** The default extractor is a Python port of
   `kb_core::intelligence::heuristic` — the same zero-spend extractor
   `KB_ENTITY_EXTRACTOR=heuristic` selects in the service. `--extractor llm`
   is the optional paid path (OpenRouter, cached, budget-gated); it exists so
   the choice can be measured, not because it is the default.
3. **Page–entity graph → entity communities.** Weighted co-occurrence between
   entities that share a page, clustered with a deterministic Louvain
   (`--resolution` 1.0). Every node is visited in sorted order and ties go to
   the lowest community id, so the same input gives the same communities on
   every machine.
4. **Pages assigned to communities.** A page joins the community holding most
   of its entities (ties: lowest community id). Pages whose text yields no
   entity are reported, not silently dropped.

Output `communities.json`:

```json
{"dataset": "...", "extractor": "heuristic", "n_pages": 1110,
 "n_pages_with_entities": 1104, "n_entities": 5312, "n_edges": 40211,
 "n_communities": 37,
 "communities": [{"id": 0, "entities": [{"key": "...", "name": "..."}],
                  "pages": ["doc#p12", ...]}],
 "page_entities": {"doc#p12": ["key", ...]}}
```

`page_entities` is kept because the arm orders a community's pages by how many
of the query's entities each page actually mentions; without it the arm could
only rank whole communities.

    python3 community_build.py --dataset .../vidore3-hr-en \\
        --out ../sets/aggregative/vidore3-hr-en/communities.json
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path
from typing import Dict, List, Tuple

sys.path.insert(0, str(Path(__file__).resolve().parent))

import community_lib as lib  # noqa: E402


# --- page text ---------------------------------------------------------------


def pdf_pages(pdf: Path) -> int:
    out = subprocess.run(
        ["pdfinfo", str(pdf)], capture_output=True, text=True, check=True
    ).stdout
    for line in out.splitlines():
        if line.startswith("Pages:"):
            return int(line.split(":", 1)[1].strip())
    raise SystemExit(f"pdfinfo did not report a page count for {pdf}")


def page_text(pdf: Path, page: int, cache: Path) -> str:
    if cache.is_file():
        return cache.read_text(encoding="utf-8", errors="replace")
    result = subprocess.run(
        ["pdftotext", "-f", str(page), "-l", str(page), "-layout", str(pdf), "-"],
        capture_output=True,
        text=True,
    )
    text = result.stdout if result.returncode == 0 else ""
    cache.parent.mkdir(parents=True, exist_ok=True)
    cache.write_text(text, encoding="utf-8")
    return text


def load_page_texts(
    dataset: Path, cache_dir: Path, max_pages: int
) -> Tuple[Dict[str, str], Dict[str, int]]:
    texts: Dict[str, str] = {}
    stats = {"pdfs": 0, "pages": 0, "from_cache": 0}
    for pdf in sorted((dataset / "corpus").glob("*.pdf")):
        doc = pdf.stem
        pages = pdf_pages(pdf)
        stats["pdfs"] += 1
        stop = pages if not max_pages else min(pages, max_pages)
        for page in range(1, stop + 1):
            key = f"{doc}#p{page}"
            cache = cache_dir / "pages" / dataset.name / f"{doc}#p{page}.txt"
            existed = cache.is_file()
            texts[key] = page_text(pdf, page, cache)
            stats["pages"] += 1
            stats["from_cache"] += int(existed)
    return texts, stats


# --- deterministic Louvain ---------------------------------------------------


def _one_level(
    graph: Dict[int, Dict[int, float]], resolution: float
) -> Dict[int, int]:
    nodes = sorted(graph)
    degree = {n: sum(graph[n].values()) for n in nodes}
    m2 = sum(degree.values()) or 1.0
    community = {n: n for n in nodes}
    total = {n: degree[n] for n in nodes}
    moved = True
    while moved:
        moved = False
        for node in nodes:
            current = community[node]
            k = degree[node]
            total[current] -= k
            weights: Dict[int, float] = {}
            for other, weight in graph[node].items():
                if other == node:
                    continue
                weights[community[other]] = weights.get(community[other], 0.0) + weight
            best = current
            best_gain = weights.get(current, 0.0) - resolution * total[current] * k / m2
            for candidate in sorted(weights):
                if candidate == current:
                    continue
                gain = weights[candidate] - resolution * total[candidate] * k / m2
                if gain > best_gain + 1e-12:
                    best, best_gain = candidate, gain
            community[node] = best
            total[best] = total.get(best, 0.0) + k
            if best != current:
                moved = True
    return community


def _aggregate(
    graph: Dict[int, Dict[int, float]], community: Dict[int, int]
) -> Tuple[Dict[int, Dict[int, float]], Dict[int, int]]:
    groups = sorted(set(community.values()))
    renumber = {group: index for index, group in enumerate(groups)}
    new_graph: Dict[int, Dict[int, float]] = {index: {} for index in range(len(groups))}
    for node in sorted(graph):
        a = renumber[community[node]]
        for other, weight in graph[node].items():
            b = renumber[community[other]]
            # Internal edges become a self-loop. Dropping them (the obvious
            # shortcut) loses the community's cohesion, and the next level then
            # merges everything into one community at modularity 0 — a bug that
            # looks like "the corpus has no structure".
            new_graph[a][a] = new_graph[a].get(a, 0.0) + weight
    return new_graph, renumber


def louvain(
    nodes: List[str], edges: Dict[Tuple[str, str], float], resolution: float = 1.0
) -> Dict[str, int]:
    """Weighted Louvain with deterministic order. Returns node -> community id."""
    index = {node: i for i, node in enumerate(sorted(nodes))}
    graph: Dict[int, Dict[int, float]] = {i: {} for i in range(len(index))}
    for (left, right), weight in edges.items():
        if left == right or left not in index or right not in index:
            continue
        a, b = index[left], index[right]
        graph[a][b] = graph[a].get(b, 0.0) + weight
        graph[b][a] = graph[b].get(a, 0.0) + weight
    membership = {i: i for i in range(len(index))}
    for _ in range(20):
        community = _one_level(graph, resolution)
        if len(set(community.values())) == len(graph):
            break
        graph, renumber = _aggregate(graph, community)
        membership = {n: renumber[community[membership[n]]] for n in membership}
    names = sorted(index)
    # Renumber communities by size (desc) then first member, for stable readable ids.
    groups: Dict[int, List[str]] = {}
    for node in names:
        groups.setdefault(membership[index[node]], []).append(node)
    ordered = sorted(groups.values(), key=lambda members: (-len(members), members[0]))
    label_of: Dict[str, int] = {}
    for label, members in enumerate(ordered):
        for name in members:
            label_of[name] = label
    return label_of


# --- LLM extraction (optional, paid, cached) ---------------------------------


def llm_entities(
    texts: Dict[str, str],
    model: str,
    cache_dir: Path,
    max_usd: float,
    dry_run: bool,
) -> Dict[str, List[Tuple[str, str, int]]]:
    here = Path(__file__).resolve().parent
    sys.path.insert(0, str(here))
    import community_summarise as cs  # noqa: E402

    prompt_price, completion_price = cs.price_for(model)
    prompts = {
        page: (
            "List the named entities in this page: organisations, places, people, "
            "products, policies, and number-with-unit quantities. One per line, "
            "no commentary, no duplicates. Use the page's own wording.\n\n"
            + " ".join(text.split())[:6000]
        )
        for page, text in texts.items()
    }
    total = sum(
        cs.estimate_tokens(prompt) / 1_000_000 * prompt_price + 120 / 1_000_000 * completion_price
        for prompt in prompts.values()
    )
    print(f"LLM entity extraction: {len(prompts)} pages, estimated ${total:.4f} "
          f"({model}); prices looked up 2026-11-04, tokens chars/4")
    if dry_run:
        return {}
    if total > max_usd + 1e-9:
        raise SystemExit(f"estimated ${total:.4f} exceeds --max-usd ${max_usd:.4f}")
    api_key = os.environ.get("OPENROUTER_API_KEY", "")
    if not api_key:
        raise SystemExit("OPENROUTER_API_KEY is not set")
    out: Dict[str, List[Tuple[str, str, int]]] = {}
    for i, (page, prompt) in enumerate(sorted(prompts.items()), start=1):
        content, _, _ = cs.chat_cached(model, prompt, 120, cache_dir, api_key)
        counts: Dict[str, int] = {}
        for line in content.splitlines():
            key = lib.key_of(line.strip().lstrip("-*0123456789. "))
            if key and not lib._is_generic(key):
                counts[key] = counts.get(key, 0) + 1
        out[page] = [
            (key, key, n)
            for key, n in sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))[:64]
        ]
        if i % 50 == 0:
            print(f"  {i}/{len(prompts)} pages", file=sys.stderr)
    return out


# --- selftest ----------------------------------------------------------------


def selftest() -> int:
    sample = (
        "Scott Derrickson (born July 16, 1966) is an American director, "
        "screenwriter and producer. He lives in Los Angeles, California. He is "
        'best known for directing horror films such as "Sinister", "The Exorcism '
        'of Emily Rose", and "Deliver Us From Evil"'
    )
    found = {key for key, _, _ in lib.extract_entities(sample)}
    assert "scott derrickson" in found, sorted(found)
    assert "los angeles" in found, sorted(found)
    print(f"heuristic port: {len(found)} entities, sample keys {sorted(found)[:6]}")

    edges: Dict[Tuple[str, str], float] = {}
    for clique in (["a1", "a2", "a3", "a4", "a5"], ["b1", "b2", "b3", "b4", "b5"]):
        for i, left in enumerate(clique):
            for right in clique[i + 1:]:
                edges[(left, right)] = 1.0
    nodes = sorted({node for pair in edges for node in pair})
    labels = louvain(nodes, edges)
    groups: Dict[int, set] = {}
    for node, label in labels.items():
        groups.setdefault(label, set()).add(node)
    assert len(groups) == 2, groups
    assert any(members == {"a1", "a2", "a3", "a4", "a5"} for members in groups.values())
    assert labels == louvain(nodes, edges), "not deterministic"
    print(f"Louvain: two cliques -> {len(groups)} communities, deterministic")
    print("selftest passed")
    return 0


# --- main --------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--dataset", type=Path, required=False)
    ap.add_argument("--out", type=Path, required=False)
    ap.add_argument("--extractor", choices=["heuristic", "llm"], default="heuristic")
    ap.add_argument("--model", default="deepseek/deepseek-v4-flash", help="model for --extractor llm")
    ap.add_argument("--max-usd", type=float, default=0.0)
    ap.add_argument("--dry-run", action="store_true", help="estimate the extraction cost only")
    ap.add_argument("--max-pages", type=int, default=0, help="cap pages per PDF (smoke tests)")
    ap.add_argument("--resolution", type=float, default=1.0)
    ap.add_argument("--max-df-frac", type=float, default=0.05,
                    help="drop entities on more than this fraction of pages from the graph "
                         "(hub stop-entities; 0 disables)")
    ap.add_argument("--cache-dir", type=Path, default=None)
    ap.add_argument("--selftest", action="store_true")
    args = ap.parse_args()

    if args.selftest:
        return selftest()
    if not args.dataset or not args.out:
        ap.error("--dataset and --out are required (or use --selftest)")

    cache_dir = args.cache_dir or (
        Path(os.environ.get("KB_CACHE_DIR") or os.environ.get("KB_STACK_CACHE_DIR"))
        if os.environ.get("KB_CACHE_DIR") or os.environ.get("KB_STACK_CACHE_DIR")
        else Path.home() / ".cache" / "kb-cache"
    )
    started = time.time()
    texts, stats = load_page_texts(args.dataset, cache_dir, args.max_pages)
    print(
        f"page text: {stats['pages']} pages from {stats['pdfs']} PDFs "
        f"({stats['from_cache']} cached) in {time.time() - started:.1f}s"
    )

    per_page: Dict[str, List[Tuple[str, str, int]]] = {}
    if args.extractor == "llm":
        per_page = llm_entities(texts, args.model, cache_dir, args.max_usd, args.dry_run)
        if args.dry_run:
            return 0
    else:
        for page, text in texts.items():
            per_page[page] = lib.extract_entities(text)

    page_entities: Dict[str, List[str]] = {
        page: [key for key, _, _ in entities]
        for page, entities in per_page.items()
        if entities
    }
    display: Dict[str, str] = {}
    for entities in per_page.values():
        for key, name, _ in entities:
            display.setdefault(key, name)
    empty_pages = sorted(set(texts) - set(page_entities))
    if empty_pages:
        print(f"warning: {len(empty_pages)} pages yielded no entity (not in any community)")

    # Hub stop-entities. A surface extractor finds "Member States" and "access"
    # on hundreds of pages; leaving them in the graph joins every topic into one
    # community whose summary says nothing. They are reported, not hidden.
    raw_pages_with_entities = len(page_entities)
    df: Dict[str, int] = {}
    for keys in page_entities.values():
        for key in set(keys):
            df[key] = df.get(key, 0) + 1
    hubs: List[str] = []
    if args.max_df_frac > 0:
        limit = max(2, int(args.max_df_frac * raw_pages_with_entities))
        hubs = sorted((k for k, n in df.items() if n > limit), key=lambda k: (-df[k], k))
    hub_set = set(hubs)
    if hubs:
        page_entities = {
            page: [key for key in keys if key not in hub_set]
            for page, keys in page_entities.items()
        }
        page_entities = {page: keys for page, keys in page_entities.items() if keys}
        print(
            f"hub stop-entities: dropped {len(hubs)} entities seen on more than "
            f"{int(args.max_df_frac * 100)}% of pages ({len(page_entities)} pages left); "
            f"worst: {', '.join(hubs[:5])}"
        )

    cooccur: Dict[Tuple[str, str], float] = {}
    for keys in page_entities.values():
        unique = sorted(set(keys))
        for i, left in enumerate(unique):
            for right in unique[i + 1:]:
                pair = (left, right) if left < right else (right, left)
                cooccur[pair] = cooccur.get(pair, 0.0) + 1.0

    entities = sorted({key for keys in page_entities.values() for key in keys})
    labels = louvain(entities, cooccur, args.resolution)

    # Page -> community by majority of its entities' communities.
    members: Dict[int, Dict[str, list]] = {}
    for page, keys in page_entities.items():
        votes: Dict[int, int] = {}
        for key in keys:
            label = labels[key]
            votes[label] = votes.get(label, 0) + 1
        best = min(votes.items(), key=lambda kv: (-kv[1], kv[0]))[0]
        members.setdefault(best, {"entities": [], "pages": []})["pages"].append(page)
    for key in entities:
        members.setdefault(labels[key], {"entities": [], "pages": []})["entities"].append(key)

    communities = []
    for label, group in members.items():
        if not group["pages"]:
            continue
        communities.append(
            {
                "id": label,
                "entities": [{"key": key, "name": display[key]} for key in sorted(group["entities"])],
                "pages": sorted(group["pages"], key=lib.page_sort_key),
            }
        )
    communities.sort(key=lambda c: (-len(c["pages"]), -len(c["entities"]), c["id"]))
    for new_id, community in enumerate(communities):
        community["id"] = new_id

    payload = {
        "dataset": args.dataset.name,
        "extractor": args.extractor,
        "model": args.model if args.extractor == "llm" else None,
        "resolution": args.resolution,
        "max_df_frac": args.max_df_frac,
        "n_pages": len(texts),
        "n_pages_with_entities_raw": raw_pages_with_entities,
        "n_pages_with_entities": len(page_entities),
        "n_hub_entities_dropped": len(hubs),
        "hub_entities": [{"key": key, "name": display.get(key, key), "pages": df[key]} for key in hubs[:200]],
        "n_entities": len(entities),
        "n_edges": len(cooccur),
        "n_communities": len(communities),
        "n_communities_with_2plus_pages": sum(1 for c in communities if len(c["pages"]) >= 2),
        "communities": communities,
        "page_entities": {page: keys for page, keys in sorted(page_entities.items(), key=lambda kv: lib.page_sort_key(kv[0]))},
    }
    sizes = sorted((len(c["pages"]) for c in communities), reverse=True)
    print(
        f"graph: {len(entities)} entities, {len(cooccur)} co-occurrence edges, "
        f"{len(communities)} communities; page sizes: largest {sizes[:5]}, "
        f"median {sizes[len(sizes) // 2] if sizes else 0}"
    )
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(payload, ensure_ascii=False, separators=(",", ":")), encoding="utf-8")
    size_mb = args.out.stat().st_size / (1024 * 1024)
    print(f"wrote {args.out} ({size_mb:.2f} MB) in {time.time() - started:.1f}s")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
