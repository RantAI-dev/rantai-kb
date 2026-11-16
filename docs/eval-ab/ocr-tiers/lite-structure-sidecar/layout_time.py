import glob, json, sys, time, collections
from rapid_layout import RapidLayout, RapidLayoutInput, ModelType
imgs = sorted(glob.glob("/img/*.png"))
for name in sys.argv[1:]:
    try:
        cfg = RapidLayoutInput(model_type=ModelType(name), engine_cfg={"intra_op_num_threads": 2, "inter_op_num_threads": 1})
    except TypeError:
        cfg = RapidLayoutInput(model_type=ModelType(name))
    eng = RapidLayout(cfg=cfg); eng(imgs[0])
    t = time.time(); cls = collections.Counter()
    for f in imgs:
        r = eng(f); cls.update(r.class_names or [])
    dt = time.time() - t
    print(json.dumps({"layout": name, "s_per_page": round(dt/len(imgs), 3), "classes": dict(cls.most_common(8))}), flush=True)
