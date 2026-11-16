import glob, time, numpy as np
from PIL import Image
from rapidocr import RapidOCR, ModelType as OM, OCRVersion
from rapid_layout import RapidLayout, RapidLayoutInput, ModelType as LM
imgs = [Image.open(f).convert("RGB") for f in sorted(glob.glob('/img/*.png'))]
# what kb-server sends at 110 dpi: scale the 150-dpi samples down
imgs = [i.resize((int(i.width * 110 / 150), int(i.height * 110 / 150))) for i in imgs]
cfg = {"intra_op_num_threads": 1, "inter_op_num_threads": 1}
lay = RapidLayout(cfg=RapidLayoutInput(model_type=LM.YOLOV8N_LAYOUT_GENERAL6, engine_cfg=cfg))
for side in (960, 736):
    o = RapidOCR(params={"Det.ocr_version": OCRVersion.PPOCRV6, "Det.model_type": OM.TINY, "Rec.ocr_version": OCRVersion.PPOCRV6, "Rec.model_type": OM.TINY,
        "Global.use_cls": False, "Global.max_side_len": side, "Global.log_level": "error", "EngineConfig.onnxruntime.intra_op_num_threads": 1, "EngineConfig.onnxruntime.inter_op_num_threads": 1})
    o(np.asarray(imgs[0])); tl = td = tr = 0; n = 0
    for im in imgs:
        a = np.asarray(im)[:, :, ::-1]; s = im.copy(); s.thumbnail((1024, 1024))
        t = time.time(); lay(np.asarray(s)[:, :, ::-1]); tl += time.time() - t
        r = o(a); el = r.elapse_list if hasattr(r, "elapse_list") else None
        if el: td += el[0] or 0; tr += el[-1] or 0
        n += len(r.txts or ())
    k = len(imgs)
    print(f"side {side}: layout {tl/k:.2f}s det {td/k:.2f}s rec {tr/k:.2f}s per page (1 thread), lines/page {n/k:.0f}")
