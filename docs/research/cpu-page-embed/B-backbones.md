# B — Backbones for a small CPU page-image embedding model (Indonesian + English)

Research agent B · 2026-11-18. Scope: text towers, vision towers, FLOP arithmetic, SEA/Indonesian doc models. Teacher: `nvidia/Nemotron-3.5-Embed-1B` (2048-d). Engine: Peregrine (GGUF, CPU-only, no vision support yet — plain ViT >> hybrid conv for implementation cost).

## 1. Small multilingual text encoders (query side)

| Model | Params (total / non-emb) | Arch | Tokenizer (vocab) | Max ctx | License | Indonesian evidence |
|---|---|---|---|---|---|---|
| mmBERT-small | 140M / 42M | ModernBERT, 22L, d=384, RoPE | Gemma 2 (262K) | 8,192 | MIT | 1,833 langs incl. id; MMTEB v2 multilingual retrieval SOTA-for-size, per-language id not reported (UNVERIFIED) — [GitHub](https://github.com/JHU-CLSP/mmBERT), [paper](https://arxiv.org/abs/2509.06888), [HF blog](https://huggingface.co/blog/mmbert) |
| mmBERT-base | 307M / 110M | ModernBERT, 22L, d=768 | Gemma 2 (262K) | 8,192 | MIT | same sources |
| multilingual-e5-small | 118M / ~96M | MiniLM-L12-H384 style, 12L, d=384 | XLM-R (250K) | 512 | MIT | MTEB mult. retrieval (18 langs) 50.9 — [granite r2 paper Tab.](https://arxiv.org/html/2605.13521v2); arch per [LazarusNLP](https://github.com/LazarusNLP/indonesian-sentence-embeddings) |
| multilingual-e5-base | 278M / ~185M | XLM-R base, 12L, d=768 | XLM-R (250K) | 512 | MIT | id STS: 41.29/45.04 (LazarusNLP table) |
| EmbeddingGemma-300M | 308M / ~106M | Gemma-3 enc (bidirectional), MRL 768→128, QAT Q4/Q8 ckpts | Gemma (262K) | 2,048 | Gemma Terms | MTEB mult. v2 61.15; languages incl. Indonesian; "performs well on… Indonesian" per SEA-LION paper — [model card](https://ai.google.dev/gemma/docs/embeddinggemma/model_card), [SEA-LION paper](https://arxiv.org/html/2606.03027v2) |
| granite-emb-97m-multilingual-r2 | 97M | ModernBERT, 12L, d=384, SiLU, 32K ctx | purpose-trained 180K | 32,768 | Apache-2.0 | **MIRACL-id nDCG@10 45.98** — [HF](https://huggingface.co/ibm-granite/granite-embedding-97m-multilingual-r2), [paper Tab. C.4](https://arxiv.org/html/2605.13521v2) |
| granite-emb-311m-multilingual-r2 | 311M | ModernBERT, 22L, d=768, GeGLU, MRL | 262K | 32,768 | Apache-2.0 | MIRACL-id 46.93; MTEB mult. retrieval 65.2 — same sources |
| Qwen3-Embedding-0.6B | 596M / 440M | Qwen3 enc, 28L, MRL 32–1024 | BPE 151K | 32,768 | Apache-2.0 | 100+ langs incl. id; MMTEB 64.33 — [HF](https://huggingface.co/Qwen/Qwen3-Embedding-0.6B) |
| jina-embeddings-v3 | 570M / ~316M active | XLM-R large + RoPE + 5 LoRA adapters | XLM-R (250K) | 8,192 | **CC-BY-NC 4.0** | 89 trained langs; id among supported — [HF](https://huggingface.co/jinaai/jina-embeddings-v3), [jina.ai](https://jina.ai/news/jina-embeddings-v3-a-frontier-multilingual-embedding-model) |
| bge-m3 | 568M | XLM-R large, dense+sparse+colbert | XLM-R (250K) | 8,192 | MIT | 100+ langs; dense/sparse hybrid — [HF](https://huggingface.co/BAAI/bge-m3) |
| gte-multilingual-base | 305M / 113M active | mGTE, RoPE, dense+sparse | custom | 8,192 | Apache-2.0 | MIRACL avg 56.6 dense — [HF](https://huggingface.co/Alibaba-NLP/gte-multilingual-base), [mGTE paper](https://aclanthology.org/2024.emnlp-industry.103.pdf) |
| SEA-LION-ModernBERT-Emb-300M | 300M | ModernBERT, 8K ctx | **Gemma 3 (262K)** | 8,192 | MIT | SEA-BED 10-lang avg **0.760**, best small model on 8/10 SEA langs incl. Indonesian — [docs](https://docs.sea-lion.ai/models/sea-embedding/sea-modernbert.md), [paper](https://arxiv.org/html/2606.03027v2) |
| 2026: harrier-oss-v1-270m | 270M | — | — | — | — | MTEB mult. retrieval 66.4 — [granite blog table](https://huggingface.co/blog/ibm-granite/granite-embedding-multilingual-r2) |
| 2026: F2LLM-v2-80M | 80M | — | — | — | — | MTEB mult. retrieval 50.1 — same source |
| 2026: jina-v5-text-nano | 239M | — | — | — | — (license UNVERIFIED) | MTEB mult. retrieval 63.3 — same source |

Notes:
- **Peregrine already ships ModernBERT, EmbeddingGemma and Qwen3-Embedding** — mmBERT, granite-r2 and SEA-LION-ModernBERT reuse the ModernBERT architecture (only tokenizer vocab changes), so they are near-free to support.
- **Tokenizer efficiency for Indonesian**: no model card reports tokens/word for id (all UNVERIFIED). Proxies: Gemma-family tokenizers (262K vocab) are used by mmBERT (Gemma 2) and SEA-LION (Gemma 3, claimed "excellent performance for SEA languages" — [SEA-LION docs](https://docs.sea-lion.ai/models/sea-embedding/sea-modernbert.md)); XLM-R (250K SentencePiece) is the incumbent. We must measure fertility on our own corpus.
- **Granite's MIRACL-id ≈ 46–47 nDCG@10 is close to its English 45.4–51.1** ([paper Tab. C.4](https://arxiv.org/html/2605.13521v2)) — i.e., Indonesian retrieval with a 97M ModernBERT is nearly as good as English; the text tower is not the bottleneck.
- Teacher: Nemotron-3.5-Embed-1B, 1.55B total / 1,286M non-emb, Pixtral vision + Ministral-3 text tower, 2048-d (MRL to 128), image longest_edge default 1,120 px (max 1,540), mean-pool + L2-norm, OpenMDW-1.1 — [HF](https://huggingface.co/nvidia/Nemotron-3.5-Embed-1B-BF16), [NVIDIA card](https://build.nvidia.com/nvidia/nemotron-3_5-embed-1b/modelcard), [support matrix](https://docs.nvidia.com/nim/nemo-retriever/embedding/latest/support-matrix.html).

## 2. Small vision encoders for document pages

| Encoder | Params | Patch | Native res | Tokens / 1024² (or A4) | Plain ViT? | Text-dense / multilingual | License |
|---|---|---|---|---|---|---|---|
| SigLIP2 B/16 (+NaFlex) | 86M vision | 16 | 224–512, NaFlex var | 64×64 = 4,096 | ViT | NaFlex explicitly built for documents/OCR; beats FixRes on TextCaps/HierText/SciCap/Screen2Words retrieval; text tower is multilingual (Gemma tok) | Apache-2.0 — [paper](https://arxiv.org/abs/2502.14786), [HF](https://huggingface.co/google/siglip2-base-patch16-naflex) |
| SigLIP2 so400m/16 (+NaFlex) | ~400M | 14/16 | 224–512, NaFlex | 4,096 (p16) | ViT | as above; So400m NaFlex ≈ FixRes on natural, better on docs | Apache-2.0 — [big_vision README](https://github.com/google-research/big_vision/blob/main/big_vision/configs/proj/image_text/README_siglip2.md) |
| SigLIP (orig) so400m/14 | 400M (428M in SmolVLM-2.2B listing) | 14 | 384² | 27×27=729 @384² | ViT | English-centric WebLI captions; doc ability via tiling only | Apache-2.0 — [HF](https://huggingface.co/google/siglip-so400m-patch14-384), [survey](https://jina.ai/vision-encoder-survey.pdf) |
| PaliGemma vision | = SigLIP so400m/14 | 14 | 224/448/896 → 256/1024/4096 tok | 4,096 @896² | ViT | doc-tuned via PaliGemma training | Gemma — [HF Paligemma](https://huggingface.co/google/paligemma-3b-pt-448) |
| Idefics3 / SmolVLM tower | SigLIP so400m/14 @384; SmolVLM 384² patches → **81 tok/patch** after 3×3 pixel-shuffle | 14 | 384² | ~729 tok/1024² (9 patches) | ViT + pixel-shuffle | OCR-capable at cost of fine detail ("slightly hurt precise OCR" — SmolVLM blog) | Apache-2.0 (Idefics3), Apache-2.0 (SmolVLM) — [Idefics3](https://huggingface.co/HuggingFaceM4/Idefics3-8B-Llama3), [SmolVLM blog](https://huggingface.co/blog/smolvlm) |
| Pixtral ViT | 400M, 24L, d=1024 | 16 | variable, RoPE-2D, max 1024 long edge | 64×64 = 4,096 (A4: 49×64=3,136) | ViT | trained for charts/documents; beats CLIPA on DocVQA-class tasks | Apache-2.0 — [paper](https://arxiv.org/abs/2410.07073) |
| DINOv3 ViT-S/16 | 21M (S+ 29M), d=384, RoPE, 4 registers | 16 | any multiple of 16 | 64×64 = 4,096 (+5) | ViT (SSL) | no text alignment; strong dense features; **DINOv3 License (restrictive)** | [GitHub](https://github.com/facebookresearch/dinov3) |
| DINOv3 ViT-B/16 | 86M | 16 | any | 4,096 | ViT | 216 GFLOPs @512² (paper Fig.16) | DINOv3 License |
| EVA02 small p14 | ~22M | 14 | 224 | 73×73≈5,329 @1024² | ViT (SwinGLU, RoPE) | MIM/CLIP-pretrained, English captions; weight license UNVERIFIED | [MMPretrain](https://mmpretrain.readthedocs.io/en/dev/papers/eva02.html) |
| Florence-2 DaViT-B | 90M | conv stem 7/3/3/3 (strides 4/2/2/2) | 768² | 24×24 = 576 final-stage tokens | **hybrid conv (DaViT dual attn)** | strong OCR (TextVQA SOTA-era); text side English | MIT (HF `microsoft/Florence-2-base`) — [paper](https://arxiv.org/abs/2311.06242) |
| MobileCLIP2-S0/B | 11.4M / 86.3M image (+63.4M text) | conv stem | 256² | — | **hybrid (FastViT, RepMixer, reparam)** | DFN captions, English-only; zero-shot 71.5 (S0) | model license UNVERIFIED (repo has separate LICENSE_MODELS) — [GitHub](https://github.com/apple-aiml-research/ml-mobileclip), [paper](https://arxiv.org/abs/2508.20691) |
| FastViT-T8 / FastViTHD | 3.6M / ~100M-class | conv | 256² var | — | **hybrid conv** | classification/VLM backbones; 0.8 ms iPhone-12 latency (T8) | FastViT code CC-BY-4.0-ish; UNVERIFIED — [paper](https://arxiv.org/abs/2303.14189), [FastVLM blog](https://machinelearning.apple.com/research/fast-vision-language-models) |
| TinyCLIP | ViT-61M (also ResNet-8/40M) | 16 | 224² | 64×64 = 4,096 | ViT (weight-inheritance distill) | English; token-affinity distilled | UNVERIFIED — [arXiv 2312.03519](https://arxiv.org/abs/2312.03519) |

CPU throughput is almost never published for these (latencies above are iPhone/mobile-GPU); the FLOP math in §3 is the only defensible proxy. **Plain ViTs (SigLIP2, Pixtral, DINOv3, EVA02, TinyCLIP) are implementable in Peregrine; MobileCLIP2/FastViT(FHD)/Florence-2/DaViT carry conv stems + reparameterization + dual attention — poor ROI for a first vision backend.**

## 3. Compute estimate per page (vision tower + connector; queries are 1 forward of a ≤16-token text encoder, ≲1 GFLOP, negligible)

Cost model (standard): per token per layer ≈ 12d² MACs linear (4d² QKVO + 8d² MLP-4) + 2Nd attention MACs; 1 MAC = 2 FLOPs ([derived here](http://www.aicassindra.com/blogs/gpu/gpu_vision_encoder.html)). Validation: predicts DINOv3 ViT-S @512² = 63 GFLOPs and ViT-B @512² = 216 GFLOPs, matching the paper's table ([DINOv3 Fig. 16](https://arxiv.org/html/2508.10104)).

Assumed sustained throughput: 2 cores ≈ 100 GFLOPS, 8 cores ≈ 400 GFLOPS (int8/Q8 GGUF on AVX2/AVX-512; to be measured).

**SigLIP2-B/16 NaFlex (d=768, L=12):**
- N=256 patches: linear 12L×7.08M MAC/tok×256 = 21.7G MACs → 43.5 GF; attn 12×2×256²×768 = 1.21G MACs → 2.4 GF ⇒ **≈46 GFLOPs → 0.46 s (2c) / 0.12 s (8c)**
- N=1024: linear 86.9G MACs → 173.9 GF; attn 19.3G MACs → 38.7 GF ⇒ **≈214 GFLOPs → 2.1 s / 0.53 s**

**SigLIP2-so400m/16 NaFlex (d=1152, L=27):**
- N=256: linear 110G MACs → 220 GF; attn 4.07G → 8.1 GF ⇒ **≈228 GFLOPs → 2.3 s / 0.57 s**
- N=1024: linear 440G → 880 GF; attn 64.7G → 130 GF ⇒ **≈1,010 GFLOPs → 10.1 s / 2.5 s**

**DINOv3 ViT-S/16 (d=384, L=12):**
- N=1024 (≈512×512): **63 GFLOPs → 0.63 s / 0.16 s** (matches paper's 63 GF)
- N=4096 (1024×1024): linear 174 GF; attn 12×2×4096²×384 = 154G MACs → 309 GF ⇒ **≈483 GFLOPs → 4.8 s / 1.2 s**

**Connector**, 2-layer d=768 cross-attention over N=1024 page tokens + query tokens: ≈14.5G+3.2G MACs ⇒ **≈35 GFLOPs → 0.35 s / 0.09 s**.

**Teacher reference**: Nemotron-3.5-Embed-1B, 1,286M non-emb params × ~1,000 page tokens × 2 ≈ **2.6 TFLOPs → ~26 s on 2 cores** — the 1–3 s/page student target is a 10–25× cut, consistent with the brief.

**Resolution guidance**: A4 rendered at 150 DPI is 1,240×1,754 px = 4,850 SigLIP p16 patches — unaffordable. NaFlex was trained at sequence lengths {128, 256, 576, 784, 1024} ([paper §2.4.2](https://arxiv.org/html/2502.14786)); staying ≤1,024 patches means ~90 DPI A4 pages, which is readable for headings/paragraph structure but marginal for small body text — a SmolVLM-style pixel-shuffle or a 2-pass (256-patch global + 256-patch zoom) design can buy back detail within the same budget.

## 4. Small VLMs / encoders already trained on Indonesian or SEA documents

- **SEA-LION-Embedding suite (Mar 2026)** — ModernBERT 300M/600M (Gemma-3 tokenizer, 8K ctx, MIT) and E5-600M (512 ctx, MIT); SEA-BED (10 SEA langs, 169 datasets, human-curated) small-model avg 0.760, best on 8/10 languages; beats harrier-oss-v1-270M (0.753) and EmbeddingGemma on low-resource SEA — [docs](https://docs.sea-lion.ai/models/sea-embedding), [paper](https://arxiv.org/html/2606.03027v2). Text-only.
- **GoTo × AISG embedding model (Aug 2026)** — Indonesian e-commerce/search embeddings built on SEA-LION-ModernBERT in four stages (MLM refresh → quality → contrastive → task tuning); weight availability UNVERIFIED — [blog](https://sea-lion.ai/blog/localised-by-design-how-goto-built-upon-sea-lion-embeddings-to-train-a-model-that-understands-indonesia).
- **SEA-LION v4 multimodal (Gemma 3 27B/4B; Qwen3-VL 4B/8B)** — first SEA multimodal line, "vision-text capabilities for document understanding", ~10M post-train samples across 11 SEA languages — [docs](https://docs.sea-lion.ai/models/sea-lion_adaptations.md). Far above our CPU budget; useful as an auxiliary teacher/captioner.
- **Sahabat-AI** — Llama-3-8B continued-pretrained on Indonesian (+ Javanese/Sundanese instruct pairs), GoTo + AI Singapore; text-only LLM — [docs](https://docs.sea-lion.ai/models/sea-lion_adaptations.md).
- **SeaLLM (Damo)** — earlier SEA-focused LLM family, text-only; [models page](https://sea-lion.ai/models).
- **SEA-CLIP-Tiny (<50M)** — 2026 CLIP-KD student for SEA image-text retrieval; 7 SEA langs; +12.1 R@10 over MobileCLIP2 with 38.4% fewer params and lower CPU latency — proof that SEA-specific distillation of small towers works, but natural-image retrieval, not documents — [paper](https://arxiv.org/html/2609.30739v2).
- **No small (≤500M) page-image encoder or doc-OCR VLM specifically for Indonesian was found.** Closest public assets: SEA-BED benchmark + SEA-LION text encoders. This is the gap our model fills; LazarusNLP's Indonesian embeddings (IndoBERT-based, text) are the local text baseline — [GitHub](https://github.com/LazarusNLP/indonesian-sentence-embeddings).

## Top 3 student architectures for our case

1. **SigLIP2-base-patch16-NaFlex (86M, Apache-2.0) + granite-embedding-97m-multilingual-r2 (97M, Apache-2.0) + 2-layer cross-attention connector (≈10M, mean-pooled page token, projected to teacher space).** Vision: plain ViT, variable A4 aspect via NaFlex, multilingual text side in the teacher family (Gemma tok). Text tower: ModernBERT arch (Peregrine-ready), MIRACL-id 45.98. Cost/page: 46 GFLOPs (256 patches) → **0.5 s on 2 cores**; 214+35 GFLOPs (1,024 patches) → **~2.5 s on 2 cores, 0.6 s on 8 cores**. Best license + best implementation fit + documented doc/OCR orientation.
2. **SigLIP2-so400m-patch16-NaFlex (400M, Apache-2.0) + SEA-LION-ModernBERT-Embedding-300M (MIT, Gemma-3 tok, SEA-BED-best) + same connector.** The quality option for 8-core boxes: 228 GFLOPs @256 patches → 2.3 s / 0.6 s; 1,010 GFLOPs @1,024 → 10 s / 2.5 s (use 576-patch mid-budget ≈ 340 GFLOPs → ~0.9 s on 8 cores). Same NaFlex doc orientation; SEA-LION tower is the strongest published small encoder for Indonesian queries.
3. **DINOv3 ViT-S/16 (21M) + EmbeddingGemma-300M (query tower, already supported in Peregrine) + MLP-on-pooled-patch-features connector.** Ultra-cheap: 63 GFLOPs @1,024 patches → **0.63 s on 2 cores**; 483 GFLOPs @4,096 → 1.2 s on 8 cores. Caveats: no pre-trained text-image alignment (all cross-modal behavior must come from distillation) and the **DINOv3 License is restrictive** — viable as an internal experiment, not an obvious ship choice; a Qwen3-Embedding-0.6B query tower (Apache-2.0) is the drop-in alternative.

## Open questions

1. **No Indonesian page-image retrieval benchmark exists** (MIRACL-id is Wikipedia passages, SEA-BED is text-only). We must build a labelled set (e.g., MIRACL-id/NusaX queries paired with rendered PDF pages, or an internal corpus) before trusting any backbone choice.
2. **Is ~90 DPI (≤1,024 patches) enough** for Indonesian body text on A4? Needs a readability/OCR-recall test at 256/576/1,024 patches before freezing the resolution policy; NaFlex beyond 1,024 patches is off-distribution ([paper §2.4.2](https://arxiv.org/html/2502.14786)).
3. **SigLIP2's document/OCR results are English captions** (TextCaps, HierText, SciCap, Screen2Words); Indonesian scene-text performance is UNVERIFIED.
4. **Tokenizer fertility for Indonesian** (Gemma-2/3 vs XLM-R) is unpublished by every card we found; measure tokens/word on our corpus.
5. **Sustained GFLOPS on our target CPUs with Q8_0 GGUF ViTs** (2-core 100 GFLOPS assumption) is unmeasured; int8 VNNI/AVX-512 throughput should be benchmarked before committing to a patch budget.
6. **Teacher alignment**: Nemotron-3.5-Embed-1B mean-pools at image longest_edge 1,120 — should the student match the teacher's preprocessing (so distillation targets are consistent), and do we distill from the teacher's 2,048-d vector directly (cosine) or via hard labels?
7. **Connector**: pixel-shuffle (SmolVLM-style, token-count ÷9) vs cross-attention pooling vs plain projection — the 3× cost/quality trade-off is untested for retrieval-style embeddings.
