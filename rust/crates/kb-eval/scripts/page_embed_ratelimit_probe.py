"""Throughput and 429s of voyage-multimodal-3.5 page embeddings on OpenRouter at several concurrencies.
Each request: 4 pages (text + 96 dpi image), like kb-server's default batch. Paid: ~$0.0023/request."""
import base64, glob, json, os, sys, time, urllib.request, urllib.error
from concurrent.futures import ThreadPoolExecutor
KEY = os.environ["OPENROUTER_API_KEY"]
imgs = sorted(glob.glob(os.path.expanduser("~/.cache/kb-cache/page-image-arm/pages-96dpi/*/p-*.jpg")))
def body(i):
    batch = imgs[(i * 4) % len(imgs):(i * 4) % len(imgs) + 4]
    inp = [{"content": [{"type": "text", "text": f"probe {time.time()} {i} " + "page text " * 150},
                        {"type": "image_url", "image_url": {"url": "data:image/jpeg;base64," + base64.b64encode(open(f, "rb").read()).decode()}}]} for f in batch]
    return json.dumps({"model": "voyageai/voyage-multimodal-3.5", "input": inp, "input_type": "document"}).encode()
def call(i):
    t = time.time()
    try:
        r = json.load(urllib.request.urlopen(urllib.request.Request("https://openrouter.ai/api/v1/embeddings", data=body(i),
            headers={"Authorization": "Bearer " + KEY, "Content-Type": "application/json"}), timeout=120))
        return ("ok", time.time() - t, float(r.get("usage", {}).get("cost", 0) or 0))
    except urllib.error.HTTPError as e:
        return (f"http{e.code}", time.time() - t, 0.0)
    except Exception as e:
        return (type(e).__name__, time.time() - t, 0.0)
offset = 0
for conc in map(int, sys.argv[1:] or ["4", "8", "16"]):
    n = conc * 4
    t0 = time.time()
    with ThreadPoolExecutor(conc) as ex:
        res = list(ex.map(call, range(offset, offset + n)))
    offset += n
    wall = time.time() - t0
    ok = [r for r in res if r[0] == "ok"]
    errs = {}
    for r in res:
        if r[0] != "ok": errs[r[0]] = errs.get(r[0], 0) + 1
    lat = sorted(r[1] for r in ok)
    print(f"conc {conc:2d}: {len(ok)}/{n} ok, errors {errs}, wall {wall:.1f}s, {4*len(ok)/wall:.1f} pages/s, "
          f"latency p50 {lat[len(lat)//2] if lat else 0:.2f}s max {lat[-1] if lat else 0:.2f}s, cost ${sum(r[2] for r in res):.4f}", flush=True)
