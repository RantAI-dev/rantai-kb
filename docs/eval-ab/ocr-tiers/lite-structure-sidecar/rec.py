import glob, time, numpy as np
from PIL import Image
from rapidocr import RapidOCR, ModelType as OM, OCRVersion, EngineType, LangRec
imgs = [Image.open(f).convert("RGB") for f in sorted(glob.glob('/img/*.png'))]
imgs = [np.asarray(i.resize((int(i.width * 110 / 150), int(i.height * 110 / 150))))[:, :, ::-1] for i in imgs]
base = {"Det.ocr_version": OCRVersion.PPOCRV6, "Det.model_type": OM.TINY, "Global.use_cls": False, "Global.max_side_len": 960, "Global.log_level": "error",
        "EngineConfig.onnxruntime.intra_op_num_threads": 1, "EngineConfig.onnxruntime.inter_op_num_threads": 1, "EngineConfig.openvino.inference_num_threads": 1}
V = {
 "v6tiny-ort-b6": {"Rec.ocr_version": OCRVersion.PPOCRV6, "Rec.model_type": OM.TINY},
 "v6tiny-ort-b24": {"Rec.ocr_version": OCRVersion.PPOCRV6, "Rec.model_type": OM.TINY, "Rec.rec_batch_num": 24},
 "v6tiny-ov": {"Rec.ocr_version": OCRVersion.PPOCRV6, "Rec.model_type": OM.TINY, "Rec.engine_type": EngineType.OPENVINO},
 "v4mobile-en": {"Rec.ocr_version": OCRVersion.PPOCRV4, "Rec.model_type": OM.MOBILE, "Rec.lang_type": LangRec.EN},
 "v5mobile-en": {"Rec.ocr_version": OCRVersion.PPOCRV5, "Rec.model_type": OM.MOBILE, "Rec.lang_type": LangRec.EN},
}
for name, extra in V.items():
    try:
        p = dict(base); p.update(extra); o = RapidOCR(params=p); o(imgs[0])
    except Exception as e:
        print(name, "ERR", str(e)[:120]); continue
    tr = 0; chars = 0
    for a in imgs:
        r = o(a); tr += (r.elapse_list[-1] or 0); chars += sum(len(t) for t in (r.txts or ()))
    print(f"{name}: rec {tr/len(imgs):.2f}s/page (1 thread), chars {chars}", flush=True)
