# Pictures searchable by what they show: live results (`ING-5`, 2026-11-13)

The measurements behind backlog item `ING-5` ([spec](../../core/specs/ing-5.md)).
Every number below comes from `kb-server eval` against a live server, through the
HTTP API. Each run's `metrics.json`, `report.md` and `run.trec` are in this
directory. Per-slice tables come from
`rust/crates/kb-eval/scripts/score_images.py`. Its overall Recall@10 and MRR@10
are identical to the harness's own `metrics.json` for every run, an independent
check on the scorer.

## The set: `images-v1`

Built by `rust/crates/kb-eval/scripts/import_images.py` (seed 20261113; the images
are not committed). Each query is text a **human** wrote about the picture. The
describing model never sees it.

| Slice | n | Source | Query |
|---|---|---|---|
| photo | 100 | COCO (Karpathy test) | the second human caption |
| scene-text | 50 | TextCaps val (photos with signs, labels, posters) | the second human caption |
| chart | 50 | ChartQA test, human split | a human question that names something (a country, brand, year) |
| distractors | 150 docs | other COCO photos' captions as `note-*.md` | — |

`images-v1-embedded` holds the same 200 pictures inside documents: 67 `.docx`, 67
`.pptx` and 66 `.pdf`, written by hand-made OOXML, python-pptx and LibreOffice. Every
document's only text is the same generic line ("Lampiran dokumentasi kegiatan."),
and there is no alt text, so only the picture can tell one document from another.

Some ChartQA questions are vague ("What is the sum of 2002, 2003 and 2004?"). They
are kept: they cap the chart slice for every configuration alike.

## Setup

- **Stores:** the `rust/scripts/stack-up.sh` stack (Postgres 16, Qdrant 1.19.2,
  RustFS). Ingest concurrency 4, no reranker, every arm at its defaults.
- **Embedder:** Qwen3-Embedding-0.6B (Q8_0 GGUF, llama.cpp `--embedding`), with 4,096
  tokens of context per request.
- **OCR:** Qwen3-VL-2B-Instruct through `KB_OCR_BASE_URL` (prompt `OCR:`), as the
  local tiers do.
- **Image describer:** `KB_IMAGE_DESCRIBE_*` (prompt `v1`, English).
- **Where models ran:** every model runs on a ThinkStation PGX (GB10) over Tailscale,
  under llama.cpp. The 2B describer is the exact file the Lite/Desk tiers ship
  (`Qwen3VL-2B-Instruct-Q4_K_M.gguf` + `mmproj-…-Q8_0.gguf`). Temperature 0, so the
  readings are the CPU tier's readings; only the speed differs.
- **Baseline:** the same stack running the pre-ING-5 build (commit `14b887a1`):
  OCR only, no visual stage.
- **Cost:** no paid API call was made.

## Results (200 queries, document level)

| Run | Recall@1 | Recall@5 | Recall@10 | MRR@10 | never found |
|---|---|---|---|---|---|
| baseline: OCR only (`f-base`) | 0.345 | 0.495 | 0.555 [0.485, 0.620] | 0.413 | 86 |
| ING-5, Qwen3-VL-2B, standalone (`f2-std`) | 0.800 | 0.980 | **0.985** [0.965, 1.000] | 0.877 | 3 |
| ING-5, Qwen3-VL-2B, embedded (`f2-emb`) | 0.800 | 0.970 | **0.980** [0.960, 0.995] | 0.875 | 3 |
| ING-5, Qwen3-VL-8B, standalone (`f8b-std`) | 0.775 | 0.955 | 0.985 [0.965, 1.000] | 0.850 | 2 |

**A bigger model buys nothing here.** Paired against the 2B, the 8B has ΔR@10 =
+0.000 [−0.015, +0.015] and ΔMRR@10 = −0.027 [−0.062, +0.011]; no slice differs
significantly. The 2B stays the default on every local tier. It is the cheapest
model, and the one the CPU tiers can run.

### By slice

| Slice | Baseline R@10 | ING-5 2B standalone R@10 | Δ R@10 (paired, 95% CI) | ING-5 2B embedded R@10 |
|---|---|---|---|---|
| photo (100) | 0.230 | **0.990** | +0.760 [+0.670, +0.850] | 1.000 |
| scene-text (50) | 0.860 | **0.980** | +0.120 [+0.040, +0.220] | 0.980 |
| chart (50) | 0.900 | **0.980** | +0.080 [+0.020, +0.160] | 0.940 |

| Slice | Baseline MRR@10 | ING-5 2B standalone MRR@10 |
|---|---|---|
| photo | 0.114 | 0.867 |
| scene-text | 0.797 | 0.970 |
| chart | 0.626 | 0.805 |

**Embedded, by container** (`f2-emb`): `.pdf` R@10 1.000, MRR 0.955; `.docx` 0.970 /
0.871; `.pptx` 0.970 / 0.800.

### Against the targets in the spec

| Target | Measured | |
|---|---|---|
| Photos without text: R@10 ≥ 0.85, MRR ≥ 0.60 | 0.990, 0.867 | met |
| Charts: R@10 ≥ 0.80 | 0.980 | met |
| Embedded within 0.05 of standalone | 0.980 vs 0.985 | met |
| Local CPU-tier model: photos R@10 ≥ 0.70 | 0.990 (the CPU tier's model file, run on a GPU) | met for quality; CPU speed below |

## What the baseline shows

Today's code finds photos only when OCR happens to read a sign. 75 of 100 photos are
never retrieved at all, and most of those are `ready` documents with **zero chunks**.
Charts and scene-text photos already did fairly well on OCR text, because the query
names words written in the picture. The describer still lifts them, mostly in rank
(MRR +0.18 on charts).

## API-class check (free cloud model, half sample)

Run earlier on a fixed 100-query half sample with `nvidia/nemotron-3.5-super-vl:free`
(the paid twin is the cloud tier's default). It used CPU TEI embeddings and no
OCR, so it is not directly comparable to the table above.

| Run | R@10 | MRR@10 |
|---|---|---|
| standalone (`h-api-std`) | 0.960 [0.920, 0.990] | 0.862 |
| photo / chart / scene-text | 0.960 / 0.960 / 0.960 | 0.807 / 0.873 / 0.960 |

The embedded half-sample run on that stack is not reported. Its PDFs (one text line
plus a large picture) are classed as scanned, and that stack had no OCR provider, so
those documents failed visibly before the visual stage ran. Every tier ships an OCR
provider; with one, PDFs score 1.000 above.

## Tenant isolation

`kb-server eval leak --docx` against this build, with no image describer (the CI
gate's configuration), seeded 8 documents across 2 tenants and ran 504 probes
(`leak-docx/leak-report.md`). Result: **0 leaks**. The **figure** row moves from
UNTESTED to **PASS** in every scope: 56 probes, positive controls 14/14. Before
`--docx`, no seeded document had a figure, so that row proved nothing.

## Speed and cost

- **Qwen3-VL-2B on the ThinkStation GPU:** about 0.65 s per uncached picture.
- **Qwen3-VL-2B on the dev box CPU** (8 threads shared with builds): 29–67 s per
  picture in the first probes, about 100 s per picture under load. That is a
  background ingest speed for the Lite tier, not an interactive one. It is the
  number to improve, and it was not measured on an idle CPU.
- **Cost:** readings are cached by model, prompt version and image hash, so a
  reindex or a logo repeated across files is read once. The cloud default costs
  about $0.22 per 1,000 pictures at the listed price (~1,000 input and ~150 output
  tokens each). That price is not measured.

## Problems found while measuring (fixed)

- Word alt text was lost when the inner `pic:cNvPr` had none (`kb-extract`, fixed
  with a regression test).
- An embedder context of 1,024 tokens per request rejected long chart OCR chunks.
  Those runs were re-done with 4,096-token slots, so no reported run has an ingest
  failure.
- VLM servers started from an agent session died with that session's cgroup. The
  2B runs above were checked afterwards in Postgres: 214/214 and 203/203 pictures
  read, 0 failures.
