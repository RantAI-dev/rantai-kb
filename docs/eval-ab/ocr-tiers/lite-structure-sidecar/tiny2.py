import glob, json, sys, time
from multiprocessing import Pool
imgs = sorted(glob.glob("/img/*.png"))
side, batch, procs = int(sys.argv[1]), int(sys.argv[2]), int(sys.argv[3])
E = None
def init():
    global E
    from rapidocr import RapidOCR, ModelType, OCRVersion
    th = 2 // procs
    E = RapidOCR(params={"Det.ocr_version": OCRVersion.PPOCRV6, "Det.model_type": ModelType.TINY, "Rec.ocr_version": OCRVersion.PPOCRV6, "Rec.model_type": ModelType.TINY,
        "Global.use_cls": False, "Global.max_side_len": side, "Rec.rec_batch_num": batch, "Global.log_level": "error",
        "EngineConfig.onnxruntime.intra_op_num_threads": th, "EngineConfig.onnxruntime.inter_op_num_threads": 1})
    E(imgs[0])
def one(f): return len("\n".join(E(f).txts or ()))
with Pool(procs, initializer=init) as p:
    p.map(one, imgs[:procs]); t = time.time(); chars = sum(p.map(one, imgs, chunksize=1)); dt = time.time() - t
print(json.dumps({"side": side, "rec_batch": batch, "procs": procs, "s_per_page": round(dt/len(imgs), 2), "pages_per_s": round(len(imgs)/dt, 2), "chars": chars}))
