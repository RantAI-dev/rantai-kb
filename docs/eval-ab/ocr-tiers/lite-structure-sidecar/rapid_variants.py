# Time RapidOCR variants on the sample pages, one page at a time (2 cores pinned by docker).
import glob, json, sys, time
from rapidocr import RapidOCR, ModelType, OCRVersion, EngineType
imgs = sorted(glob.glob("/img/*.png"))
variants = {
  "v6-small": {},
  "v6-small-nocls": {"Global.use_cls": False},
  "v6-tiny": {"Det.ocr_version": OCRVersion.PPOCRV6, "Det.model_type": ModelType.TINY, "Rec.ocr_version": OCRVersion.PPOCRV6, "Rec.model_type": ModelType.TINY, "Global.use_cls": False},
  "v6-mobile": {"Det.ocr_version": OCRVersion.PPOCRV6, "Det.model_type": ModelType.MOBILE, "Rec.ocr_version": OCRVersion.PPOCRV6, "Rec.model_type": ModelType.MOBILE, "Global.use_cls": False},
  "v5-mobile": {"Det.ocr_version": OCRVersion.PPOCRV5, "Det.model_type": ModelType.MOBILE, "Rec.ocr_version": OCRVersion.PPOCRV5, "Rec.model_type": ModelType.MOBILE, "Global.use_cls": False},
  "v6-small-openvino": {"Det.engine_type": EngineType.OPENVINO, "Rec.engine_type": EngineType.OPENVINO, "Cls.engine_type": EngineType.OPENVINO, "Global.use_cls": False},
}
only = sys.argv[1:] or list(variants)
for name in only:
    p = dict(variants[name]); p.update({"EngineConfig.onnxruntime.intra_op_num_threads": 2, "EngineConfig.onnxruntime.inter_op_num_threads": 1, "EngineConfig.openvino.inference_num_threads": 2, "Global.log_level": "error"})
    try:
        eng = RapidOCR(params=p); eng(imgs[0])
    except Exception as e:
        print(json.dumps({"variant": name, "error": str(e)[:200]})); continue
    t = time.time(); chars = 0
    for f in imgs:
        r = eng(f); chars += sum(len(x) for x in (r.txts or ()))
    dt = time.time() - t
    print(json.dumps({"variant": name, "pages": len(imgs), "s_per_page": round(dt / len(imgs), 2), "pages_per_s": round(len(imgs) / dt, 2), "chars": chars}), flush=True)
