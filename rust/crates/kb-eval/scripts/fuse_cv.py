"""2-fold CV of weighted RRF (text, image): tune weight w and k on one half, test on the other.

    python -I fuse_cv.py QRELS TEXT.page.trec IMAGE.page.trec
"""
import sys, math, random
from collections import defaultdict
qrels, text_p, img_p = sys.argv[1:4]
gold=defaultdict(dict)
for l in open(qrels):
    p=l.split()
    if p[0]!='query-id': gold[p[0]][p[1]]=int(p[2])
def run(path):
    r=defaultdict(list)
    for l in open(path):
        q,_,d,*_=l.split()
        if d not in r[q]: r[q].append(d)
    return r
T,I=run(text_p),run(img_p)
def ndcg(rk,g):
    dcg=sum(g.get(d,0)/math.log2(i+2) for i,d in enumerate(rk[:10]))
    idl=sorted(g.values(),reverse=True)[:10]; idcg=sum(x/math.log2(i+2) for i,x in enumerate(idl))
    return dcg/idcg if idcg else 0
def fuse(q,w,k):
    s=defaultdict(float)
    for r,d in enumerate(T.get(q,[])): s[d]+=(1-w)/(k+r+1)
    for r,d in enumerate(I.get(q,[])): s[d]+=w/(k+r+1)
    return [d for d,_ in sorted(s.items(),key=lambda kv:-kv[1])]
qs=sorted(gold, key=int)
grid=[(w,k) for w in [i/20 for i in range(0,21)] for k in [5,10,20,40,60,100]]
def mean(qset,w,k): return sum(ndcg(fuse(q,w,k),gold[q]) for q in qset)/len(qset)
folds=[qs[0::2],qs[1::2]]
per_q={}
for i in (0,1):
    tr,te=folds[i],folds[1-i]
    w,k=max(grid,key=lambda p:mean(tr,*p))
    print(f"fold {i}: best on train w={w} k={k} train={mean(tr,w,k):.3f} test={mean(te,w,k):.3f}  (equal w=0.5,k=60 test={mean(te,0.5,60):.3f})")
    for q in te: per_q[q]=ndcg(fuse(q,w,k),gold[q])
cv=sum(per_q.values())/len(per_q)
base=[ndcg(T.get(q,[]),gold[q]) for q in qs]
eq=[ndcg(fuse(q,0.5,60),gold[q]) for q in qs]
cvl=[per_q[q] for q in qs]
def ci(a,b,n=5000):
    random.seed(0); d=[x-y for x,y in zip(a,b)]; m=sum(d)/len(d)
    bs=sorted(sum(random.choice(d) for _ in d)/len(d) for _ in range(n))
    return m,bs[int(.025*n)],bs[int(.975*n)]
print(f"text only {sum(base)/len(base):.3f} | equal RRF {sum(eq)/len(eq):.3f} | CV-tuned {cv:.3f}")
print("delta equal-RRF vs text: %+.3f [%+.3f, %+.3f]"%ci(eq,base))
print("delta CV-tuned vs text:  %+.3f [%+.3f, %+.3f]"%ci(cvl,base))
