import sys, time, numpy as np, onnxruntime as ort
from tokenizers import Tokenizer
model, threads, batch = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
maxlen = int(sys.argv[4]) if len(sys.argv) > 4 else 512
tok = Tokenizer.from_file(sys.argv[5] if len(sys.argv) > 5 else __import__('os').path.expanduser('~/.cache/kb-models/local/e5-small-int8/tokenizer.json'))
tok.enable_truncation(maxlen); tok.enable_padding()
so = ort.SessionOptions(); so.intra_op_num_threads = threads; so.inter_op_num_threads = 1
s = ort.InferenceSession(model, so, providers=["CPUExecutionProvider"])
names = [i.name for i in s.get_inputs()]
base = ("Employment in the EU rose to 75.3% in 2023, while long-term unemployment fell in most Member States. "
        "Labour shortages persisted in health, ICT and construction, and real wages recovered only partly after the inflation shock. ")
texts = [f"passage: {i} " + (base * 6)[:900] for i in range(160)]
def run(b):
    enc = tok.encode_batch(b)
    ids = np.array([e.ids for e in enc], dtype=np.int64); am = np.array([e.attention_mask for e in enc], dtype=np.int64)
    feed = {"input_ids": ids, "attention_mask": am}
    if "token_type_ids" in names: feed["token_type_ids"] = np.zeros_like(ids)
    return s.run(None, feed)[0], ids.shape[1]
run(texts[:2])
t = time.time(); L = 0
for i in range(0, len(texts), batch): _, L = run(texts[i:i+batch])
dt = time.time() - t
print(f"{model.split('/')[-1]} threads {threads} batch {batch} maxlen {maxlen} seq {L}: {len(texts)/dt:.1f} chunks/s")
