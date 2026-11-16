import glob, time, numpy as np
from PIL import Image
from rapid_layout import RapidLayout, RapidLayoutInput, ModelType
from rapid_table import RapidTable, RapidTableInput, ModelType as TM
imgs = [Image.open(f).convert("RGB") for f in sorted(glob.glob('/img/*.png'))]
def small(im, m=1024):
    im = im.copy(); im.thumbnail((m, m)); return np.asarray(im)[:, :, ::-1]
cfg = {"intra_op_num_threads": 2, "inter_op_num_threads": 1}
e = RapidLayout(cfg=RapidLayoutInput(model_type=ModelType.YOLOV8N_LAYOUT_GENERAL6, engine_cfg=cfg))
arr = [small(i) for i in imgs]; e(arr[0])
t = time.time(); tables = []
for a, im in zip(arr, imgs):
    r = e(a)
    for b, c in zip(r.boxes, r.class_names):
        if c == "Table":
            s = im.width / a.shape[1]; x1, y1, x2, y2 = [int(v * s) for v in b]; tables.append(np.asarray(im.crop((x1, y1, x2, y2)))[:, :, ::-1])
print("yolov8n@1024 s/page", round((time.time()-t)/len(arr), 3), "tables", len(tables), flush=True)
tb = RapidTable(RapidTableInput(model_type=TM.SLANETPLUS, engine_cfg=cfg))
if tables:
    tb(tables[0]); t = time.time()
    for x in tables: out = tb(x)
    print("slanet_plus s/table (incl. its own OCR)", round((time.time()-t)/len(tables), 3), flush=True)
    print(str(out.pred_htmls[0])[:300] if hasattr(out, "pred_htmls") else str(out)[:300])
