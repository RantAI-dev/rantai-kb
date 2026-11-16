import glob, json, os, sys, time
import numpy as np
from PIL import Image
imgs = sorted(glob.glob("/img/*.png"))
def run(name, fn):
    fn(imgs[0]); t = time.time(); chars = 0
    for f in imgs: chars += len(fn(f))
    dt = time.time() - t
    print(json.dumps({"variant": name, "s_per_page": round(dt/len(imgs), 2), "pages_per_s": round(len(imgs)/dt, 2), "chars": chars}), flush=True)
which = sys.argv[1]
if which.startswith("rapid"):
    from rapidocr import RapidOCR, ModelType, OCRVersion, EngineType
    _, eng, side = which.split(":")
    p = {"Det.ocr_version": OCRVersion.PPOCRV6, "Det.model_type": ModelType.TINY, "Rec.ocr_version": OCRVersion.PPOCRV6, "Rec.model_type": ModelType.TINY,
         "Global.use_cls": False, "Global.max_side_len": int(side), "Global.log_level": "error",
         "EngineConfig.onnxruntime.intra_op_num_threads": 2, "EngineConfig.onnxruntime.inter_op_num_threads": 1, "EngineConfig.openvino.inference_num_threads": 2}
    if eng == "ov": p.update({"Det.engine_type": EngineType.OPENVINO, "Rec.engine_type": EngineType.OPENVINO})
    E = RapidOCR(params=p)
    run(which, lambda f: "\n".join(E(f).txts or ()))
elif which.startswith("tess"):
    import pytesseract
    from concurrent.futures import ThreadPoolExecutor
    os.environ["OMP_THREAD_LIMIT"] = "1"
    cfg = "--oem 1 --psm 3"
    imgs_ = imgs
    ex = ThreadPoolExecutor(2)
    pytesseract.image_to_string(Image.open(imgs_[0]), config=cfg)
    t = time.time(); outs = list(ex.map(lambda f: pytesseract.image_to_string(Image.open(f), config=cfg), imgs_)); dt = time.time() - t
    print(json.dumps({"variant": "tesseract-2proc", "s_per_page": round(dt/len(imgs_), 2), "pages_per_s": round(len(imgs_)/dt, 2), "chars": sum(map(len, outs))}))
else:
    import onnxruntime as ort
    from onnxtr.models import ocr_predictor, EngineConfig
    _, det, reco = which.split(":")
    so = ort.SessionOptions(); so.intra_op_num_threads = 2; so.inter_op_num_threads = 1
    cfg = EngineConfig(session_options=so, providers=["CPUExecutionProvider"])
    M = ocr_predictor(det_arch=det, reco_arch=reco, det_engine_cfg=cfg, reco_engine_cfg=cfg, clf_engine_cfg=cfg, assume_straight_pages=True, detect_orientation=False)
    def fn(f):
        d = M([np.asarray(Image.open(f).convert("RGB"))]); return d.render()
    run(which, fn)
