#!/usr/bin/env python3
"""Save the raw `POST /v1/search` response for every query, one file per query.

The input for `floor_calibrate.py` (RET-2) and any offline analysis that needs
more than the eval runner's TREC run (scores, provenance, stats).

    dump_responses.py --base-url http://127.0.0.1:3000 --api-key-file KEYFILE \
        --queries datasets/miracl-id-200/queries.jsonl --out OUT_DIR \
        [--max-chunks 10] [--extra '{"minScore": 0.3}']

`--queries` takes a BEIR `queries.jsonl` (`_id`, `text`) or a `qid<TAB>query`
TSV. A query that fails (HTTP error, timeout) is written as
`{"error": "..."}` and counted, so a failed search is never mistaken for an
empty result. Stdlib only.
"""

import argparse
import json
import sys
import urllib.error
import urllib.request
from pathlib import Path


def read_queries(path: Path):
    out = []
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            line = line.rstrip("\n")
            if not line.strip():
                continue
            if path.suffix == ".jsonl":
                row = json.loads(line)
                out.append((str(row["_id"]), row["text"]))
            else:
                qid, _, query = line.partition("\t")
                out.append((qid, query))
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--api-key-file", required=True, type=Path)
    parser.add_argument("--queries", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--max-chunks", type=int, default=10)
    parser.add_argument("--extra", default="{}", help="JSON object merged into every request body")
    parser.add_argument("--timeout", type=float, default=120.0)
    args = parser.parse_args()

    key = args.api_key_file.read_text(encoding="utf-8").strip()
    extra = json.loads(args.extra)
    args.out.mkdir(parents=True, exist_ok=True)
    failed = 0
    queries = read_queries(args.queries)
    for qid, query in queries:
        body = {"query": query, "maxChunks": args.max_chunks, **extra}
        request = urllib.request.Request(
            args.base_url.rstrip("/") + "/v1/search",
            data=json.dumps(body).encode("utf-8"),
            headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json"},
            method="POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=args.timeout) as response:
                payload = json.loads(response.read().decode("utf-8"))
        except (urllib.error.URLError, TimeoutError, ValueError) as exc:
            payload = {"error": str(exc)}
            failed += 1
        (args.out / f"{qid}.json").write_text(json.dumps(payload, ensure_ascii=False), encoding="utf-8")
    print(f"{len(queries)} queries, {failed} failed -> {args.out}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
