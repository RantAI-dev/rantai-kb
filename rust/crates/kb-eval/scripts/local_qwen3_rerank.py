"""Qwen3-Reranker-0.6B on this box's CPU, as a bake-off candidate.

TEI cannot serve this model: it is a causal LM scored by the "yes"/"no"
logit, not a sequence-classification head, and TEI's router refuses it
("the `--pooling` arg is not set and we could not find a pooling
configuration"). So the model is run here with torch, exactly as its model
card documents it:

    "<Instruct>: {instruction}\\n<Query>: {query}\\n<Document>: {doc}"

wrapped in the chat template's system prompt, and scored by the probability
of the "yes" token against the "no" token at the last position.

The output is written in the bake-off's cache format, so `rerank_bakeoff.py`
scores it with the same metrics, the same paired bootstrap and the same
baseline as every other candidate.

    python local_qwen3_rerank.py --candidates candidates.jsonl --dataset DIR \
        --out cache.jsonl --limit 20
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rerank_bakeoff import RerankCache  # noqa: E402

# The model card's default instruction, and the recommendation to write the
# instruction in English even for a multilingual corpus.
INSTRUCTION = "Given a web search query, retrieve relevant passages that answer the query"

PREFIX = (
    '<|im_start|>system\nJudge whether the Document meets the requirements based on the Query '
    'and the Instruct provided. Note that the answer can only be "yes" or "no".<|im_end|>\n'
    "<|im_start|>user\n"
)
SUFFIX = "<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--candidates", type=Path, required=True)
    ap.add_argument("--dataset", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True, help="the bake-off cache file to write")
    ap.add_argument("--model", default="Qwen/Qwen3-Reranker-0.6B")
    ap.add_argument("--limit", type=int, default=20)
    ap.add_argument("--batch-size", type=int, default=8)
    ap.add_argument("--threads", type=int, default=6)
    args = ap.parse_args()

    import torch
    from transformers import AutoModelForCausalLM, AutoTokenizer

    torch.set_num_threads(args.threads)

    queries = {}
    for line in (args.dataset / "queries.jsonl").open(encoding="utf-8"):
        row = json.loads(line)
        queries[row["_id"]] = row["text"]
    cands = {}
    for line in args.candidates.open(encoding="utf-8"):
        row = json.loads(line)
        cands[row["qid"]] = row["candidates"]
    qids = sorted(cands)[: args.limit]

    print(f"loading {args.model} ...", file=sys.stderr)
    tokenizer = AutoTokenizer.from_pretrained(args.model, padding_side="left")
    model = AutoModelForCausalLM.from_pretrained(args.model, dtype=torch.float32).eval()
    true_id = tokenizer.convert_tokens_to_ids("yes")
    false_id = tokenizer.convert_tokens_to_ids("no")
    prefix_tokens = tokenizer.encode(PREFIX, add_special_tokens=False)
    suffix_tokens = tokenizer.encode(SUFFIX, add_special_tokens=False)

    cache = RerankCache(args.out)
    for n, qid in enumerate(qids, start=1):
        query = queries[qid]
        docs = [c["text"] for c in cands[qid]]
        key = RerankCache.key(args.model, qid, query, docs)
        if cache.get(key) is not None:
            print(f"[{n}/{len(qids)}] {qid}: cached", file=sys.stderr)
            continue
        pairs = [
            f"<Instruct>: {INSTRUCTION}\n<Query>: {query}\n<Document>: {d}"
            for d in docs
        ]
        t0 = time.perf_counter()
        scores = []
        with torch.no_grad():
            for start in range(0, len(pairs), args.batch_size):
                part = pairs[start : start + args.batch_size]
                encoded = tokenizer(
                    part,
                    padding=False,
                    truncation="longest_first",
                    return_attention_mask=False,
                    max_length=8192 - len(prefix_tokens) - len(suffix_tokens),
                )
                input_ids = [prefix_tokens + e + suffix_tokens for e in encoded["input_ids"]]
                batch = tokenizer.pad(
                    {"input_ids": input_ids}, padding=True, return_tensors="pt"
                )
                logits = model(**batch).logits[:, -1, :]
                yes = logits[:, true_id]
                no = logits[:, false_id]
                probs = torch.softmax(torch.stack([no, yes], dim=1), dim=1)[:, 1]
                scores.extend(float(x) for x in probs)
        ms = (time.perf_counter() - t0) * 1000
        order = sorted(range(len(docs)), key=lambda i: scores[i], reverse=True)
        cache.put(key, args.model, qid, order, [scores[i] for i in order], ms, 0.0)
        print(f"[{n}/{len(qids)}] {qid}: {ms:.0f} ms for {len(docs)} documents", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
