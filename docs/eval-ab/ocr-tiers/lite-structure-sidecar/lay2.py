import glob, time, collections, sys
from rapid_layout import RapidLayout, RapidLayoutInput, ModelType
imgs = sorted(glob.glob('/img/*.png'))
spec = {"doclayout": (ModelType.DOCLAYOUT_DOCSTRUCTBENCH, "/m/doclayout_yolo_docstructbench_imgsz1024.onnx"),
        "yolov8n": (ModelType.YOLOV8N_LAYOUT_GENERAL6, None),
        "ppv2": (ModelType.PP_DOC_LAYOUTV2, "/m/pp_doc_layoutv2.onnx"), "ppv3": (ModelType.PP_DOC_LAYOUTV3, "/m/pp_doc_layoutv3.onnx")}
for name in sys.argv[1:]:
    mt, path = spec[name]
    cfg = {"intra_op_num_threads": 2, "inter_op_num_threads": 1, "enable_cpu_mem_arena": False}
    e = RapidLayout(cfg=RapidLayoutInput(model_type=mt, model_dir_or_path=path, engine_cfg=cfg))
    e(imgs[0]); t = time.time(); c = collections.Counter()
    for f in imgs:
        r = e(f); c.update(r.class_names or [])
    print(name, 's/page', round((time.time()-t)/len(imgs), 3), dict(c.most_common(9)), flush=True)
