# Model and tool licences (checked 2026-10-23)

**How this was checked.**

- For each model, the `license` field on the Hugging Face model card, and the card's
  `base_model`.
- Wherever the licence is custom, the licence file itself, searched for: non-commercial,
  revenue or user thresholds, competitor clauses, attribution, territory.
- For code, the GitHub SPDX id.

This is an engineering triage, not legal advice. Re-check before shipping, since licences
change; MinerU's did.

**Verdicts.**

- 🟢 Permissive (Apache-2.0 / MIT), with no extra terms found.
- 🟡 Commercial use allowed, with conditions.
- 🔴 Non-commercial, or blocked for us.

## Text embeddings

| Model | Size | Licence | Verdict |
|---|---|---|---|
| Qwen/Qwen3-Embedding-0.6B / 4B / 8B | 0.6–8B | Apache-2.0 | 🟢 |
| intfloat/multilingual-e5-small / base / large-instruct | 118M–560M | MIT | 🟢 |
| ibm-granite/granite-embedding-97m-multilingual-r2, -311m-multilingual-r2, -107m-multilingual | 97–311M | Apache-2.0 | 🟢 |
| BAAI/bge-m3 | 568M | MIT | 🟢 |
| Alibaba-NLP/gte-multilingual-base | 305M | Apache-2.0 | 🟢 |
| Snowflake/snowflake-arctic-embed-m-v2.0 | 305M | Apache-2.0 | 🟢 |
| nomic-ai/nomic-embed-text-v2-moe | 475M | Apache-2.0 | 🟢 |
| mixedbread-ai/mxbai-edge-colbert-v0-17m / 32m | 17–32M | Apache-2.0 | 🟢 (likely English-only; check) |
| lightonai/GTE-ModernColBERT-v1 | 150M | Apache-2.0 | 🟢 |
| google/embeddinggemma-300m | 300M | Gemma terms, gated | 🟡 Gemma use policy |
| LiquidAI/LFM2-ColBERT-350M, LFM2.5-ColBERT-350M | 350M | LFM 1.0 | 🟡 commercial use only below **$10M annual revenue** |
| jinaai/jina-embeddings-v3, jina-colbert-v2 | — | CC-BY-NC-4.0 | 🔴 |

## Text rerankers

| Model | Size | Licence | Verdict |
|---|---|---|---|
| BAAI/bge-reranker-v2-m3 | 568M | Apache-2.0 | 🟢 |
| Qwen/Qwen3-Reranker-0.6B / 4B / 8B | 0.6–8B | Apache-2.0 | 🟢 |
| zeroentropy/zerank-2-reranker, zerank-1-small-reranker | 4B / small | Apache-2.0 (base Qwen3-4B, Apache) | 🟢 |
| mixedbread-ai/mxbai-rerank-xsmall-v1, mxbai-rerank-base-v2 | 70M / 0.5B | Apache-2.0 | 🟢 |
| Alibaba-NLP/gte-multilingual-reranker-base | 306M | Apache-2.0 | 🟢 |
| ibm-granite/granite-embedding-reranker-english-r2 | — | Apache-2.0 | 🟢 (English) |
| BAAI/bge-reranker-v2.5-gemma2-lightweight | — | Gemma terms | 🟡 |
| jinaai/jina-reranker-v2-base-multilingual, v3, v3.5 | — | CC-BY-NC-4.0 | 🔴 |

## Page-image (multimodal) embeddings

| Model | Size | Licence | ViDoRe v3 HR (en, Table 9) | Verdict |
|---|---|---|---:|---|
| Qwen/Qwen3-VL-Embedding-2B / 8B | 2B / 8B | Apache-2.0 | not in table | 🟢 |
| vidore/colqwen2-v1.0 | 2B | Apache-2.0 (base Qwen2-VL-2B, Apache) | 0.547 | 🟢 |
| vidore/colSmol-256M / 500M | 0.25 / 0.5B | MIT (base SmolVLM, Apache) | 0.460 (256M) | 🟢 |
| ModernVBERT/colmodernvbert | 0.25B | MIT | 0.470 | 🟢 |
| nomic-ai/colnomic-embed-multimodal-7b | 7B | Apache-2.0 (base Qwen2.5-VL-7B, Apache) | 0.629 | 🟢 |
| ibm-granite/granite-vision-3.3-2b-embedding | 2B | Apache-2.0 | not in table | 🟢 |
| vidore/colpali-v1.3 | 3B | MIT adapter, base PaliGemma (Gemma terms) | 0.533 | 🟡 |
| vidore/colqwen2.5-v0.2 | 3B | MIT adapter, **base Qwen2.5-VL-3B = Qwen Research licence (non-commercial)** | 0.592 | 🔴 |
| nomic-ai/colnomic-embed-multimodal-3b, nomic-embed-multimodal-3b | 3B | Qwen Research licence (non-commercial) | 0.626 / 0.519 | 🔴 |
| jinaai/jina-embeddings-v4 | 3B | Qwen Research licence (non-commercial) | 0.646 | 🔴 |
| nvidia/llama-nemoretriever-colembed-1b-v1, -3b-v1, llama-nemotron-colembed-vl-3b-v2 | 1–3B | NVIDIA customised NSCL: **non-commercial research only** | 0.645 / 0.649 / 0.654 | 🔴 |
| nvidia/nemotron-colembed-vl-4b-v2 / 8b-v2 | 4 / 8B | CC-BY-NC-4.0 | — | 🔴 |

## Multimodal rerankers

| Model | Licence | Verdict |
|---|---|---|
| Qwen/Qwen3-VL-Reranker-2B / 8B | Apache-2.0 | 🟢 |
| lightonai/MonoQwen2-VL-v0.1 | Apache-2.0 (base Qwen2-VL-2B, Apache) | 🟢 |
| jinaai/jina-reranker-m0 | CC-BY-NC-4.0 | 🔴 |

## Document parsing / OCR models

| Model | Size | Licence | Verdict |
|---|---|---|---|
| PaddlePaddle/PP-OCRv6 (tiny/small/medium, ONNX) | 1.5–34.5M | Apache-2.0 | 🟢 |
| PaddlePaddle/PaddleOCR-VL-1.6 (+ official GGUF) | 0.9B | Apache-2.0 | 🟢 |
| zai-org/GLM-OCR | 0.9B | MIT | 🟢 |
| opendatalab/MinerU2.5-Pro (weights) | 1.2B | Apache-2.0 | 🟢 (the MinerU *pipeline code* is 🟡, see below) |
| ibm-granite/granite-docling-258M | 258M | Apache-2.0 | 🟢 |
| lightonai/LightOnOCR-2-1B, LightOnOCR-3-0.8B / 1B | 0.8–1B | Apache-2.0 | 🟢 |
| ATH-MaaS/OvisOCR2 | 0.8B | Apache-2.0 (base Qwen3.5-0.8B) | 🟢 |
| deepseek-ai/DeepSeek-OCR (MIT), DeepSeek-OCR-2 (Apache-2.0) | ~3B MoE | MIT / Apache-2.0 | 🟢 |
| infly/Infinity-Parser2-Pro | — | Apache-2.0 | 🟢 |
| allenai/olmOCR-2-7B | 7B | Apache-2.0 (base Qwen2.5-VL-7B, Apache) | 🟢 |
| rednote-hilab/dots.ocr | 1.7B | MIT + licence agreement (explicitly allows commercial and SaaS use) | 🟢 |
| StarDoc-AI/TeleOCR | 1.2B | Apache-2.0 per card (base not stated) | 🟢 (re-check) |
| tencent/HunyuanOCR | — | Hunyuan community licence: **not valid in the EU, UK and South Korea**; separate licence above 100M MAU | 🟡 |
| SoMarkAI/OXR-1.0 | 1.3B | Apache-2.0 + terms: separate licence above **$2M funding or revenue**; must credit "SoMark OXR"; no competing hosted parsing API | 🟡 |
| datalab-to/indra-ocr, chandra-ocr-2 | — | Modified OpenRAIL-M: no use above **$20M revenue or funding**, **no use by competitors of Datalab** | 🔴 |
| jinaai/jina-ocr-v1 | — | CC-BY-NC-4.0 | 🔴 |

## Captioning / chart VLMs

| Model | Licence | Verdict |
|---|---|---|
| Qwen/Qwen3-VL-2B / 4B / 8B-Instruct | Apache-2.0 | 🟢 |
| microsoft/Florence-2-base / large | MIT | 🟢 |
| HuggingFaceTB/SmolVLM-256M, SmolVLM2-2.2B | Apache-2.0 | 🟢 |
| ibm-granite/granite-vision-3.3-2b, granite-4.0-3b-vision, granite-vision-3.3-2b-chart2csv-preview | Apache-2.0 | 🟢 |
| Qwen/Qwen2.5-VL-3B-Instruct | Qwen Research licence | 🔴 |

## Code and runtimes

| Project | Licence | Verdict |
|---|---|---|
| PaddleOCR, RapidOCR | Apache-2.0 | 🟢 |
| Docling | MIT | 🟢 |
| MinerU (pipeline code) | Apache-2.0 + terms: separate licence above 100M MAU or $20M monthly revenue; **must credit MinerU** in the product or public docs | 🟡 |
| llama.cpp | MIT | 🟢 |
| vLLM, text-embeddings-inference | Apache-2.0 | 🟢 |
| Qdrant | Apache-2.0 | 🟢 |
| pdfium binaries (bblanchon) | MIT | 🟢 |
| pdfium-render (Rust crate, already used) | GitHub reports NOASSERTION | re-check the crate's declared licence |
| ort (ONNX Runtime for Rust) | Apache-2.0 | 🟢 |

## Cloud services

Cloud services (Mistral OCR, OpenRouter models such as Gemini or Qwen3-Embedding-8B) are
governed by each provider's terms of service and data-handling policy, not by model
licences. Review those before sending customer documents.
