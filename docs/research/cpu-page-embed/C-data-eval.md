# Research report C: data & evaluation for an ID/EN page-image embedding model

Date: 2026-11-18. Web research only; no datasets were downloaded. All numbers cite a source URL; unverifiable claims are marked **UNVERIFIED**.

## 1. Evaluation sets for page retrieval (with Indonesian coverage)

| Benchmark | Size | Languages | Licence | URL |
|---|---|---|---|---|
| **MIRACL-VISION** | 7,898 queries / 338,734 images total; **Indonesian split: 603 queries / 23,842 pages** | 18 (incl. id, en) | CC BY-SA 4.0 (+ Apache-2.0 additional) | https://huggingface.co/datasets/nvidia/miracl-vision ; paper: https://arxiv.org/abs/2505.11651 |
| **ViDoRe v1** | ~500 queries × 500 docs per task; 10 tasks (8 EN, 2 FR) | EN, FR | mixed per-source | https://huggingface.co/datasets/vidore/colpali_train_set (context); https://arxiv.org/abs/2407.01449 |
| **ViDoRe v2** | 9 tasks; e.g. Economics 58 queries/452 pages, Biomedical 160/1,016 | EN + queries translated to FR/DE/ES | per-dataset | https://arxiv.org/pdf/2505.17166 ; https://huggingface.co/blog/manu/vidore-v2 |
| **ViDoRe v3** | 26,000 pages / 3,099 queries × 6 languages (EN/FR/ES/DE/IT/PT); 8 public + 2 private corpora; 12,000 human hours | docs EN+FR only | "commercially permissive" (paper) | https://huggingface.co/blog/QuentinJG/introducing-vidore-v3 ; https://aclanthology.org/2026.acl-long.755/ |
| **JinaVDR** | 95 tasks / 43 in MTEB; ≤1,000 examples per task | 20 languages incl. **id** (github-readme-multilingual, wikimedia-commons-documents-ml), ms/my | mixed per-subset (e.g. cc-by-sa-4.0 for CharXiv subset) | https://jina.ai/news/jinavdr-new-visual-document-retrieval-benchmark-with-95-tasks-in-20-languages/ ; https://github.com/jina-ai/jina-vdr |
| **MMDocIR (eval)** | 1,658 expert questions / 313 docs / 20,395 pages (65.1 pg/doc); page- + layout-level | EN mostly | **CC BY-NC 4.0 (research-only)** | https://mmdocrag.github.io/MMDocIR/ ; https://github.com/MMDocRAG/MMDocIR (licence note) |
| **VisR-Bench** | 35,571 QA / 1,286 docs (373 EN + 913 multilingual, ~18 pg avg); also 210k QA finetuning set | 16 (no id) | CC BY 4.0 (paper); queries GPT-4o-generated | https://arxiv.org/abs/2508.07493 |
| **M4DocBench** | 158 expert questions / 304 docs; multi-hop, multi-doc; EN+ZH | EN, ZH | TBD (paper 2025-10) | https://arxiv.org/abs/2510.21603 |
| **SEA-Vision** (CVPR 2026) | 15,234 parsing pages + 7,496 TEC-VQA pairs | 11 SEA languages incl. **id, ms** | repo: https://github.com/Shopee-MUG/SEA-Vision | https://openaccess.thecvf.com/content/CVPR2026/html/Yue_SEA-Vision_A_Multilingual_Benchmark_for_Comprehensive_Document_and_Scene_Text_CVPR_2026_paper.html |
| **INDOTABVQA** (ACL 2026 Findings) | 1,593 table images / 6,372 QA (1,593 per lang: id/en/hi/ar) | Bahasa docs + 4 query langs | HF public | https://huggingface.co/datasets/NusaBharat/INDOTABVQA ; https://aclanthology.org/2026.findings-acl.1105/ |
| **IMOQA** (used by IndoColSmol) | used to eval Indonesian page retrieval (Recall@5 0.888–0.890) | id | **UNVERIFIED: no public HF page found; access via authors (Telkom Univ.)** | https://doi.org/10.1109/icodsa67155.2025.11157156 |

Key facts on MIRACL-VISION-id: built from MIRACL's human-written Indonesian queries (960 in original MIRACL), first-paragraph Wikipedia pages rendered at 980×980, corpus reduced by keeping only positives + top-50 hard negatives per query with multilingual-e5 (https://arxiv.org/html/2505.11651). Reference scores on the id split (nDCG@10): bge-m3 (text) 0.7049; colqwen2-v1.0 0.5320; gme-Qwen2-VL-2B 0.5416 (same paper); nemotron-colembed-vl-8b-v2 0.668 (https://www.sota2.com/research/sota/visual-document-retrieval-on-miracl-vision). No Indonesian page-retrieval benchmark covers born-digital government/finance PDFs or scans; MIRACL-id is Wikipedia-only — hence the need for our own held-out set.

## 2. Training data for visual document retrieval

| Dataset | Size (pairs) | Languages | Licence | URL |
|---|---|---|---|---|
| ColPali train set | 118,695 query-page pairs (DocVQA 39,463; InfoVQA 10,074; TAT-DQA 13,251; arXivQA 10,000; scraped PDFs 45,940) | EN only | academic sources under original licences; synthetic part "no usage restrictions" | https://huggingface.co/datasets/vidore/colpali_train_set |
| Docmatix | 2.4M images / 9.5M QA from 1.27M PDFs (PDFA) | EN | MIT | https://huggingface.co/datasets/HuggingFaceM4/Docmatix ; https://huggingface.co/blog/docmatix |
| **Docmatix-IR** | 5.61M query→positive+20 hard negatives (DSE top-100 consistency filter; ~4M queries removed) | EN | inherits Docmatix (MIT) | https://huggingface.co/datasets/Tevatron/docmatix-ir |
| VisRAG train | 362,110 Q-D pairs (123k in-domain from MP-DocVQA/ArXivQA/ChartQA/InfoVQA/PlotQA/SlideVQA + 239,358 synthetic GPT-4o over textbooks/ICML/NeurIPS/manualslib) | EN | code Apache-2.0; model MiniCPM licence; data on HF | https://github.com/OpenBMB/VisRAG ; https://arxiv.org/html/2410.10594 |
| vdr-multilingual-train | 280,679 filtered queries / 496,167 pages (5 langs: en/es/it/de/fr; 40% filtered out) | 5 EU langs, **no id** | Apache-2.0 | https://huggingface.co/datasets/llamaindex/vdr-multilingual-train ; https://huggingface.co/blog/vdr-2b-multilingual |
| MMDocIR train | 73,843 QA over 6,878 docs (from 7 DocVQA datasets; total bootstrapped pool 173k) | EN | **CC BY-NC 4.0** | https://mmdocrag.github.io/MMDocIR/ |
| Wiki-SS-NQ (DSE) | corpus ~1M-10M screenshots (360 GB); NQ query pairs (~10-100k) | EN | research (HF) | https://huggingface.co/datasets/Tevatron/wiki-ss-corpus ; https://huggingface.co/datasets/Tevatron/wiki-ss-nq |
| PixMo-Docs | 255k synthetic chart/table/diagram/doc images + 2.3M QA | EN | ODC-BY 1.0 (code Apache-2.0) | https://huggingface.co/datasets/allenai/pixmo-docs ; successor CoSyn-400k same org |
| VisR-Bench finetuning | 210k QA / 39.5k docs | 15 + EN | generated with GPT-4o → **OpenAI terms restrict commercial training use** | https://arxiv.org/abs/2508.07493 |
| Nemotron VLM dataset v1 (Eagle2 post-training) | 3M samples (67% VQA, 28.4% OCR, 4.6% captioning) — the style of permissive re-annotation NVIDIA used | EN+ZH | "ready for commercial use" | https://huggingface.co/blog/nvidia/nvidia-vlm-dataset-v1 |
| ColEmbed v2 training mix (NVIDIA) | HotpotQA, MIRACL, NQ, DocMatix-IR, VDR, ColPali train, VisRAG synthetic+in-domain, Wiki-SS-NQ; ~500k image samples fine-tune | multilingual synthetic queries added | model research-only (3B/4B); datasets as listed | https://huggingface.co/nvidia/llama-nemotron-colembed-vl-3b-v2/blob/main/README.md |
| Jina v4 mixtures | text+text-image pairs from >300 sources incl. Wiki-SS, VDR-multilingual; mined hard negatives | 20+ | described in paper only | https://arxiv.org/html/2506.18902 |
| **Nemotron-3.5-Embed-1B (our teacher)** | distilled from Nemotron-3.5-Embed-8B with COS+MSE losses on multilingual text+image retrieval data; evaluated for text in 34 langs (incl. id, ms) and image in 18 (incl. id) | id covered | commercial-use model card | https://huggingface.co/nvidia/Nemotron-3.5-Embed-1B-BF16 |

## 3. Indonesian PDF / page sources at scale

| Source | Scale | Language risk / licence | URL |
|---|---|---|---|
| **JDIH network** (peraturan.go.id, jdihn.go.id, peraturan.bpk.go.id) | 1,233 integrated member institutions; laws (UU/PP/Perpres/Perda), monographs, court decisions as free PDFs | Indonesian Copyright Law excludes official state documents from copyright (UU 28/2014 Art. 42 — **UNVERIFIED, check with counsel**); site T&Cs may claim commercial-use restrictions (e.g. https://jdih.komdigi.go.id/faq) | https://jdihn.go.id/ ; https://peraturan.bpk.go.id/ |
| **BPS statistics publications** | hundreds of "…Dalam Angka" and thematic publications, free PDF downloads | government open-data policy; site terms apply — **UNVERIFIED for redistribution** | https://www.bps.go.id/ (example use: https://journal.irpi.or.id/index.php/malcom/article/view/2632) |
| **OJK / Bank Indonesia reports** | thousands of regulation + annual/sectoral report PDFs, free | institutional copyright; fine for internal training/eval, risky to redistribute | https://www.ojk.go.id ; https://www.bi.go.id (**UNVERIFIED page counts**) |
| **IDX annual reports** | all listed companies file ARs; thousands of PDFs | corporate copyright — internal eval use only | https://www.idx.co.id (**UNVERIFIED bulk-access legality**) |
| **Garuda / Neliti / SINTA repositories** | Garuda aggregates Indonesian journals (mostly OA PDFs); Neliti mixed OA/closed | per-journal licence varies; check CC marks per article | https://garuda.kemdikbud.go.id (**UNVERIFIED licence terms**) |
| **Indonesian Wikipedia rendered pages** | ~654k id-Wikipedia articles (indo_wiki dump) | CC BY-SA 4.0 — safest large source; MIRACL-VISION already renders 23,842 id pages we can reuse | https://github.com/LazarusNLP/NusaBERT (table citing indo_wiki 654,287 articles) |
| **FinePDFs (CommonCrawl PDFs)** | **id_Latn subset: 2,323,354 docs / 20.34B tokens / 19.65 GB**; zsm (Malay) 693,830 docs / 3.41B tokens; ODC-By 1.0 | text+URLs only — original PDFs must be fetched from CommonCrawl; ~54% of re-fetched links were dead (LinkedIn post by author) so budget fetch time | https://huggingface.co/datasets/HuggingFaceFW/finepdfs |
| ccPDF (used by VisR-Bench) | CommonCrawl PDF corpus, multilingual (id present but subset size **UNVERIFIED**) | check per-doc | https://arxiv.org/abs/2508.07493 (§3.1) |
| CulturaX-id / FineWeb2-id | CulturaX id: 23,251,368 docs / 12.06B tokens (text only, licence unstated → research-use); FineWeb2 ind_Latn: 100,238,529 docs / 60.26B words / 141.7 GB, ODC-By 1.0 (text only, not PDFs) | useful for corpus text/teacher distillation side-channels, not page images | https://huggingface.co/datasets/uonlp/CulturaX ; https://huggingface.co/datasets/HuggingFaceFW/fineweb-2 |
| SEA-LION SEA-Pile v2 | has an `id` subset (used by IndMix) | text only | via https://huggingface.co/datasets/AdaMLLab/IndMix sources list |

## 4. Synthetic query generation with local VLMs — recipes from the papers

Common pipeline (all five groups converge on it):
1. **Prompt on the page image** (ColPali's prompt, reused verbatim by Jina v4): "given an image from a pdf page… generate questions that could be asked by a user… not too specific or too general… answer found in the page… max THREE per page" — https://manuelfay.github.io/web-thesis/chapters/colpali-appendix/ ; https://arxiv.org/html/2506.18902 (App. A.5).
2. **Generate from text, not the image, when OCR is reliable**: Docmatix generated QA from PDFA transcriptions with Phi-3-small, 5 prompt variants, ~4 QA/page; filtered ~15% via regex (code, "unanswerable") — https://huggingface.co/blog/docmatix.
3. **Force a specific-vs-general distinction**: LlamaIndex had gemini-1.5-pro/Qwen2-VL-72B write one specific + one general question per page, kept only the specific; cleaning rules (single question, no markdown, no grounding phrases like "according to Figure 1") — https://huggingface.co/blog/vdr-2b-multilingual.
4. **Consistency filtering with a retriever**: (a) LlamaIndex: index broad queries with voyage-3; keep a specific query only if its broad query ranks in top-100 → removes ~40% as low-entropy/duplicates; hard negatives = pages with similarity > 0.75 threshold (positive-aware NV-Retriever mining made negatives *too easy*) — same blog. (b) Docmatix-IR: encode corpus with DSE; drop queries whose positive is not in top-100; other top-100 hits become hard negatives — https://huggingface.co/datasets/Tevatron/docmatix-ir. (c) VisRAG: GPT-4o classifier with human-annotated demos removes *context-dependent* queries ("Where was the conference held?"); this cut eval sets to 4–26% of original — https://arxiv.org/html/2410.10594 (App. A.2). (d) Jina: LLM judge rejects vague queries and queries that "depend on specific documents" — https://arxiv.org/html/2506.18902 (Fig. prompts).
5. **Blind contextual querying for eval realism** (ViDoRe v2/v3): generate queries from section summaries/metadata only, never from the target page; then human review (v2) or 12,000 annotator-hours with a VLM pre-filter funnel (v3, Qwen2.5-32B recall filter → human ranking + Qwen2.5-VL-32B answer verification) — https://arxiv.org/pdf/2505.17166 ; https://huggingface.co/blog/QuentinJG/introducing-vidore-v3.

**Indonesian queries — generate directly vs translate:** both are proven. VisR-Bench prompted GPT-4o to emit each question *in both English and the target language simultaneously* (https://arxiv.org/abs/2508.07493 §3.3). INDOTABVQA wrote questions natively in Bahasa, then auto-translated to en/hi/ar with human validation (https://arxiv.org/html/2604.11970v1 §2.4). Recommendation: generate directly in Bahasa Indonesia with a multilingual local VLM (Qwen3-VL family is the strongest open option; Gemma-3 / InternVL / MiniCPM-V fallback — **UNVERIFIED which is best for id query style; test 200 samples each**), run an id-language-ID check plus the same consistency filter, and use Malay (zsm) pages as bonus near-target data. Always write the generation prompt itself in Indonesian, not English, to avoid anglicised phrasing.

## 5. Proposed evaluation protocol

**External anchors (run unchanged, report nDCG@10 / Recall@5):**
- MIRACL-VISION `id` (603 q / 23,842 docs) and `en` (447 q / 42,971 docs) — CC BY-SA; eval-only, never train on MIRACL-id queries/pages (no official train split exists yet; authors say one is planned — https://arxiv.org/html/2505.11651 §5.0.1).
- ViDoRe v3 public subsets (8 corpora) for enterprise-style EN; MMDocIR page-level eval (note NC licence — eval only) for long-doc robustness.

**Own held-out set "IndoDocEval-v0" (build once, ~2 person-weeks):**
- Corpus: 8,000–12,000 pages from 200–300 documents, mixing: JDIH regulations (digital), BPS publications (tables/charts), OJK/IDX financial reports (tables), university theses/papers via Garuda, 1–2k id-Wikipedia rendered pages, and ≥10% true scans (fax/photocopied Perda or old BPS scans). Split **by document (PDF), never by page**.
- Queries: 500–800 total; ~60% Bahasa, ~40% English; short questions (5–15 words) in the style of MIRACL/ViDoRe; half VLM-generated-then-human-verified, half human-written; tag each query with doc type, digital-vs-scan, and evidence modality (text/table/chart) as ViDoRe v3 does (https://huggingface.co/blog/QuentinJG/introducing-vidore-v3).
- Metrics: nDCG@10 (primary), Recall@5/10; per-slice breakdowns; report BM25-on-OCR and the Nemotron-3.5-Embed-1B teacher as baselines. A small model that beats BM25-OCR on the scan slice is the real milestone.
- **Leakage controls**: (1) document-level split + URL/file-hash blocklist so no document appears in both synthetic-train and eval; (2) MinHash dedup (like IndMix, threshold ~0.8 — https://huggingface.co/datasets/AdaMLLab/IndMix) between eval pages and all training pages; (3) eval queries generated by a *different* pipeline (ideally humans, or a different VLM) than training queries; (4) keep a frozen 20% private slice never used for model selection; (5) never tune on MIRACL-id — report it cold at the end.

## 6. Recommended data mix for a first training run

Unlabelled pages for teacher distillation (Nemotron-3.5-Embed-1B, 2048-d, local on the GB10 box; zero API cost):
- ~150k pages rendered from FinePDFs `ind_Latn` (fetch via CommonCrawl `file_path`/offset; expect dead links) + ~30k pages from JDIH/BPS/OJK + ~20k id-Wikipedia renders → ~200k ID pages.
- ~100k EN pages from Docmatix/VisRAG-synthetic page pool (English anchor) → total **~300k distillation pages**.

Contrastive query–page pairs:
- ColPali train set 118,695 (EN) — full.
- Docmatix-IR 5.61M — subsample ~500k (it ships with 20 hard negatives each).
- VisRAG train 362,110 (EN, in-domain+synthetic).
- vdr-multilingual-train 280,679 (en/es/it/de/fr) — full, for multilingual transfer.
- PixMo-Docs 255k images / 2.3M QA — subsample ~50k chart/table pairs.
- **Own Indonesian synthetic: ~60k pages × 2 queries ≈ 120k ID pairs**, generated with a local Qwen3-VL-class VLM using the ColPali prompt (written in Bahasa), cleaned per LlamaIndex rules, consistency-filtered (drop if positive not in teacher's top-100; keep ~60%), hard negatives = top-100 non-positives per Docmatix-IR recipe.
- Optional: MMDocIR train 73,843 (EN, NC-licence — only if we accept research-only mix).
- Total ≈ **1.4–1.5M query–page pairs, ~8% Indonesian**, plus ~300k teacher-distilled unlabelled pages. All zero paid-API cost; generation runs on the local box.

## 7. Open questions

1. Can we legally *redistribute* (vs internally train on) JDIH/BPS/OJK page images and the resulting model weights? Indonesian UU 28/2014 Art. 42 exemption — needs verification by a lawyer.
2. How much of FinePDFs `ind_Latn` is actually retrievable from CommonCrawl (dead-link rate ~54% per the authors' refetch) and how many pages survive per doc?
3. Is IMOQA publicly obtainable (Telkom University) to compare with IndoColSmol's 0.89 Recall@5, or must we rebuild an equivalent?
4. Which local VLM generates the most natural Indonesian queries (Qwen3-VL vs Gemma-3 vs InternVL vs MiniCPM-V)? Unmeasured — needs a 200-sample human check.
5. MIRACL-VISION id queries are text-modality-heavy (Wikipedia first paragraphs); does our model's id score on it generalise to tables/charts in BPS/OJK pages? Only IndoDocEval-v0 can answer.
6. Malay (zsm) pages (~693k docs in FinePDFs) — helpful near-target augmentation or noise? Ablate.
7. Nemotron-3.5-Embed-1B teacher quality on Indonesian page images is only indirectly evidenced (18-language image eval incl. id, but public per-language page-retrieval numbers are for the ColEmbed 8B line at 0.668 nDCG@10 on MIRACL-id); measure teacher accuracy on our own slice before trusting distillation targets.
