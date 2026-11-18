"""Time page-text embeddings against an OpenAI /v1/embeddings server, one page per request."""
import json, sys, time, urllib.request
url, n, out = sys.argv[1], int(sys.argv[2]), sys.argv[3]
rows = [json.loads(l) for l in open('/home/shiro/kbdata/bakeoff/datasets/vidore3-hr-en-pages/pages.jsonl')]
rows = [r for r in rows if len(r.get('text') or '') >= 50]
step = max(1, len(rows) // n)
sample = rows[::step][:n]
res = []
for r in sample:
    body = json.dumps({"model": "peregrine", "input": ["passage: " + r["text"]], "truncate": True}).encode()
    t = time.perf_counter()
    resp = json.load(urllib.request.urlopen(urllib.request.Request(url, body, {"Content-Type": "application/json"}), timeout=1800))
    dt = time.perf_counter() - t
    toks = (resp.get("usage") or {}).get("prompt_tokens")
    res.append({"id": r["_id"], "chars": len(r["text"]), "tokens": toks, "s": dt, "dim": len(resp["data"][0]["embedding"])})
    print(json.dumps(res[-1]), flush=True)
tot = sum(x["s"] for x in res); tk = sum(x["tokens"] or 0 for x in res)
summ = {"pages": len(res), "seconds": round(tot, 2), "pages_per_s": round(len(res)/tot, 4), "s_per_page_mean": round(tot/len(res), 2),
        "tokens": tk, "tok_per_s": round(tk/tot, 1) if tk else None}
print(json.dumps(summ)); json.dump({"summary": summ, "rows": res}, open(out, "w"), indent=1)
