"""OpenAI-style /v1/embeddings over a model2vec static model (for the Lite experiment)."""
import json, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from model2vec import StaticModel
model = StaticModel.from_pretrained(sys.argv[2])
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        self.send_response(200); self.end_headers(); self.wfile.write(b"ok")
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["content-length"])))
        inp = body["input"] if isinstance(body["input"], list) else [body["input"]]
        vecs = model.encode(inp, normalize=True)
        out = json.dumps({"object": "list", "data": [{"object": "embedding", "index": i, "embedding": v.tolist()} for i, v in enumerate(vecs)]}).encode()
        self.send_response(200); self.send_header("content-type", "application/json"); self.send_header("content-length", str(len(out))); self.end_headers(); self.wfile.write(out)
ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
