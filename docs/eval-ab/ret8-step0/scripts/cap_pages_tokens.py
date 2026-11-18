#!/usr/bin/env python3
"""Write a pages variant with text cut to a token limit for a given model.

Used for multilingual-e5-small (context 511): embed_ab.py's openai kind does
not send "truncate", so texts longer than the limit are cut here with the
model's own tokenizer (decoded back to text). Queries are short and never cut.
"""
import argparse
import json
import os
import sys

from transformers import AutoTokenizer


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pages", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--tokenizer", required=True)
    ap.add_argument("--max-tokens", type=int, required=True)
    ap.add_argument("--prefix", default="", help="role prefix the harness adds (e.g. 'passage: ')")
    a = ap.parse_args()
    tok = AutoTokenizer.from_pretrained(a.tokenizer)
    n_cut = n = 0
    with open(a.pages, encoding="utf-8") as f, open(a.out, "w", encoding="utf-8") as g:
        for line in f:
            r = json.loads(line)
            n += 1
            t = r.get("text") or ""
            # cap prefix + text together, then strip the prefix back off
            joined = a.prefix + t
            ids = tok(joined, add_special_tokens=False)["input_ids"]
            if len(ids) > a.max_tokens - 4:  # leave room for specials + margin
                cut = tok.decode(ids[:a.max_tokens - 4])
                if cut.startswith(a.prefix):
                    cut = cut[len(a.prefix):]
                t = cut
                n_cut += 1
            r["text"] = t
            g.write(json.dumps(r, ensure_ascii=False) + "\n")
    print(f"{n} pages, {n_cut} cut to {a.max_tokens} tokens")
    return 0


if __name__ == "__main__":
    sys.exit(main())
