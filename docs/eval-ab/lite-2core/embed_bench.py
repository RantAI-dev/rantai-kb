"""Throughput of an OpenAI-style embeddings server on ~HR-sized chunks.
usage: embed_bench.py URL MODEL N BATCH CONC [PREFIX]"""
import json, sys, time, urllib.request
from concurrent.futures import ThreadPoolExecutor
url, model, n, batch, conc = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5])
prefix = sys.argv[6] if len(sys.argv) > 6 else ""
base = ("Employment in the EU rose to 75.3% in 2023, while long-term unemployment fell in most Member States. "
        "Labour shortages persisted in health, ICT and construction, and real wages recovered only partly after the inflation shock. ")
texts = [f"{prefix}{i} " + (base * 6)[:900] for i in range(n)]  # ~900 chars ≈ HR chunk (avg ~860 chars)
batches = [texts[i:i + batch] for i in range(0, n, batch)]
def post(b):
    r = urllib.request.Request(url, data=json.dumps({"model": model, "input": b}).encode(), headers={"Content-Type": "application/json"})
    return len(json.load(urllib.request.urlopen(r, timeout=600))["data"])
post(texts[:2])  # warm
t = time.time()
with ThreadPoolExecutor(conc) as ex:
    done = sum(ex.map(post, batches))
dt = time.time() - t
print(f"batch {batch} conc {conc}: {done} chunks in {dt:.1f}s = {done/dt:.1f} chunks/s -> {done/dt/4.44:.2f} pages/s (HR: 4.44 chunks/page)")
