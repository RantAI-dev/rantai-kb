"""nDCG@10 per query category, from a TREC run, qrels and a query-meta.jsonl.

    python breakdown.py --qrels Q --meta META --field content_type RUN [RUN...]
"""
import argparse, ast, json, math
from collections import defaultdict

ap = argparse.ArgumentParser()
ap.add_argument("--qrels", required=True)
ap.add_argument("--meta", required=True)
ap.add_argument("--field", default="content_type")
ap.add_argument("--map-doc", default="", help="'md' maps <doc>_p<N> to <doc>#p<N>")
ap.add_argument("runs", nargs="+")
a = ap.parse_args()

gold = defaultdict(dict)
for line in open(a.qrels):
    p = line.split()
    if p[0] != "query-id":
        gold[p[0]][p[1]] = int(p[2])
cats = {}
for line in open(a.meta):
    m = json.loads(line)
    v = m[a.field]
    v = ast.literal_eval(v) if isinstance(v, str) and v.startswith("[") else v
    cats[m["_id"]] = v if isinstance(v, list) else [v]

def ndcg(ranked, g):
    dcg = sum(g.get(d, 0) / math.log2(i + 2) for i, d in enumerate(ranked[:10]))
    ideal = sorted(g.values(), reverse=True)[:10]
    idcg = sum(x / math.log2(i + 2) for i, x in enumerate(ideal))
    return dcg / idcg if idcg else 0.0

for path in a.runs:
    run = defaultdict(list)
    for line in open(path):
        q, _, d, *_ = line.split()
        run[q].append(d)
    by = defaultdict(list)
    for q, g in gold.items():
        s = ndcg(run.get(q, []), g)
        by["ALL"].append(s)
        for c in cats.get(q, []):
            by[c].append(s)
    print(path)
    for c, v in sorted(by.items(), key=lambda kv: -len(kv[1])):
        print(f"  {c:12s} n={len(v):3d} nDCG@10 {sum(v)/len(v):.3f}")
