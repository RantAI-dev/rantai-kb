#!/usr/bin/env python3
"""ING-6 ingest benchmark: upload files to a live kb-server and time them.

    python3 bench.py --label NAME --key rkb_... --port 48080 --pid PID \
        --files a.pdf b.pdf ... [--stagger 2.0] [--out results.jsonl]

Uploads every file (in the given order, `--stagger` seconds apart), then polls
each document until it is `ready` or `failed`. Reports wall time, first/last
ready, per-file seconds, failures, and kb-server's VmHWM (peak resident set)
read from /proc/PID/status when the run ends. Stdlib only.
"""
import argparse, json, mimetypes, os, time, urllib.request, uuid

ap = argparse.ArgumentParser()
ap.add_argument("--label", required=True)
ap.add_argument("--key", required=True)
ap.add_argument("--port", type=int, required=True)
ap.add_argument("--pid", type=int, required=True)
ap.add_argument("--files", nargs="+", required=True)
ap.add_argument("--stagger", type=float, default=0.0)
ap.add_argument("--timeout", type=float, default=7200)
ap.add_argument("--out", default="results.jsonl")
a = ap.parse_args()
base = f"http://127.0.0.1:{a.port}"
H = {"Authorization": f"Bearer {a.key}"}


def upload(path):
    boundary = uuid.uuid4().hex
    name = os.path.basename(path)
    data = open(path, "rb").read()
    mime = mimetypes.guess_type(name)[0] or "application/octet-stream"
    body = (f"--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n"
            f"Content-Type: {mime}\r\n\r\n").encode() + data + f"\r\n--{boundary}--\r\n".encode()
    req = urllib.request.Request(base + "/v1/documents", data=body, method="POST",
                                 headers={**H, "Content-Type": f"multipart/form-data; boundary={boundary}"})
    with urllib.request.urlopen(req, timeout=120) as r:
        return json.loads(r.read())


def doc(doc_id):
    req = urllib.request.Request(f"{base}/v1/documents/{doc_id}", headers=H)
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read())


def vm_hwm(pid):
    try:
        for line in open(f"/proc/{pid}/status"):
            if line.startswith("VmHWM:"):
                return int(line.split()[1]) // 1024  # MiB
    except OSError:
        return None


t0 = time.time()
docs = []
for i, f in enumerate(a.files):
    if i and a.stagger:
        time.sleep(a.stagger)
    r = upload(f)
    d = r.get("document") or r
    did = d.get("id") or r.get("documentId") or r.get("document_id")
    docs.append({"file": os.path.basename(f), "id": did, "uploaded": time.time() - t0})
print(f"[{a.label}] uploaded {len(docs)} in {time.time()-t0:.1f}s", flush=True)

pending = {d["id"]: d for d in docs}
peak_seen = 0
while pending and time.time() - t0 < a.timeout:
    for did in list(pending):
        try:
            body = doc(did)
        except Exception:
            continue
        rec = body.get("document") or body
        status = rec.get("status")
        if status in ("ready", "failed"):
            d = pending.pop(did)
            d["status"] = status
            d["done"] = time.time() - t0
            d["seconds"] = d["done"] - d["uploaded"]
            d["chunks"] = rec.get("chunkCount", rec.get("chunk_count"))
            ocr = (rec.get("metadata") or {}).get("ocr")
            if ocr:
                d["ocr"] = {k: ocr.get(k) for k in ("mode", "pages", "ocrPages", "cachedPages", "localPages") if k in ocr}
                d["failedPages"] = ocr.get("failedPages")
            print(f"  {d['file']:<18} {status:<6} {d['seconds']:7.1f}s after upload, at {d['done']:7.1f}s", flush=True)
    hwm = vm_hwm(a.pid) or 0
    peak_seen = max(peak_seen, hwm)
    time.sleep(0.5)

wall = time.time() - t0
done = [d for d in docs if "done" in d]
summary = {
    "label": a.label,
    "files": len(docs),
    "ready": sum(d.get("status") == "ready" for d in docs),
    "failed": sum(d.get("status") == "failed" for d in docs),
    "unfinished": len(pending),
    "wall_s": round(wall, 1),
    "first_ready_s": round(min((d["done"] for d in done), default=0), 1),
    "last_ready_s": round(max((d["done"] for d in done), default=0), 1),
    "vm_hwm_mib": vm_hwm(a.pid) or peak_seen,
    "docs": docs,
}
with open(a.out, "a") as fh:
    fh.write(json.dumps(summary) + "\n")
print(json.dumps({k: v for k, v in summary.items() if k != "docs"}), flush=True)
