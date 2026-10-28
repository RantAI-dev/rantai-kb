"""Write a weighted-RRF fusion of a text page run and an image page run.

    python -I fuse_write.py TEXT.page.trec IMAGE.page.trec OUT.page.trec W K
"""
import sys
from collections import defaultdict
text_p, img_p, out, w, k = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4]), int(sys.argv[5])
def run(p):
    r = defaultdict(list)
    for l in open(p):
        q, _, d, *_ = l.split()
        if d not in r[q]: r[q].append(d)
    return r
T, I = run(text_p), run(img_p)
with open(out, "w") as f:
    for q in sorted(set(T) | set(I), key=int):
        s = defaultdict(float); order = []
        for lst, wt in ((T.get(q, []), 1 - w), (I.get(q, []), w)):
            for r, d in enumerate(lst):
                if d not in s: order.append(d)
                s[d] += wt / (k + r + 1)
        ranked = sorted(order, key=lambda d: (-s[d], order.index(d)))[:100]
        for r, d in enumerate(ranked, 1):
            f.write(f"{q} Q0 {d} {r} {s[d]:.6f} fused\n")
