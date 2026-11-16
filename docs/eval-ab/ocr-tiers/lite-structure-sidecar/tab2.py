import glob, time, numpy as np
from PIL import Image
from rapid_layout import RapidLayout, RapidLayoutInput, ModelType
from rapid_table import RapidTable, RapidTableInput, ModelType as TM
from rapidocr import RapidOCR, ModelType as OM, OCRVersion
cfg = {"intra_op_num_threads": 2, "inter_op_num_threads": 1}
imgs = [Image.open(f).convert("RGB") for f in sorted(glob.glob('/img/*.png'))]
lay = RapidLayout(cfg=RapidLayoutInput(model_type=ModelType.YOLOV8N_LAYOUT_GENERAL6, engine_cfg=cfg))
ocr = RapidOCR(params={"Det.ocr_version": OCRVersion.PPOCRV6, "Det.model_type": OM.TINY, "Rec.ocr_version": OCRVersion.PPOCRV6, "Rec.model_type": OM.TINY,
    "Global.use_cls": False, "Global.max_side_len": 960, "Global.log_level": "error", "EngineConfig.onnxruntime.intra_op_num_threads": 2, "EngineConfig.onnxruntime.inter_op_num_threads": 1})
tables = []
for im in imgs:
    s = im.copy(); s.thumbnail((1024, 1024)); r = lay(np.asarray(s)[:, :, ::-1]); k = im.width / s.width
    for b, c in zip(r.boxes, r.class_names):
        if c == "Table": tables.append(np.asarray(im.crop(tuple(int(v * k) for v in b)))[:, :, ::-1].copy())
tb = RapidTable(RapidTableInput(model_type=TM.SLANETPLUS, engine_cfg=cfg, use_ocr=False))
t_ocr = t_tab = 0
for x in tables:
    t = time.time(); o = ocr(x); t_ocr += time.time() - t
    res = (np.array(o.boxes), tuple(o.txts), tuple(o.scores)) if o.boxes is not None else (np.zeros((0, 4, 2)), (), ())
    t = time.time(); out = tb(x, ocr_results=[res]); t_tab += time.time() - t
n = len(tables)
print(f"tables {n} | tiny OCR on crop {t_ocr/n:.2f} s | SLANet-plus structure {t_tab/n:.2f} s per table")
print(out.pred_htmls[0][:400])
