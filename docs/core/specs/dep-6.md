# `DEP-6` — one embedder on the GPU tiers (`Nemotron-3.5-Embed-1B`)

Decided by the product owner, 2026-11-17: **Desk, Studio and Station run one embedding model for
text chunks and page images — `Nemotron-3.5-Embed-1B`, the same one on all three tiers.** `lite` and `lite-en` keep `multilingual-e5-small` /
`all-MiniLM-L6-v2` — a 1B VL model on CPU is slower than the corpus is worth.

This is the first architecture change to come out of [`EVAL-4`](../../eval-ab/one-embedder/README.md),
and it reverses EVAL-4's answer for a reason that did not exist when EVAL-4's rule was written:
the rule was judged on `Qwen3-VL-Embedding-2B`, and `Nemotron-3.5-Embed-1B` is a different kind
of model.

## Why

Measured on the same harness, the same pages, the same queries
([`eval-ab/one-embedder/`](../../eval-ab/one-embedder/README.md)):

| | local today (Qwen stack) | `Nemotron-3.5-Embed-1B` |
|---|---:|---:|
| text chunks (MIRACL-id, nDCG@10) | 0.796 | **0.826** (+0.030 [+0.012, +0.048]) |
| ViDoRe HR pages, image only | 0.277 | **0.626** |
| ViDoRe HR pages, text only | 0.058 | **0.611** |
| ViDoRe HR pages, one space | 0.221 | **0.621** |
| fusion weight between arms | w≈0.5, k=5, tuned on one domain | none |
| Desk page arm | impossible (text-only embedder) | **0.626**, one model instead of two |

Every arm of the one space is within ±0.015 of the others, so the modalities agree and mixing
costs nothing — the property `Qwen3-VL-2B` lacked (its text vectors cost 0.056 on pages).

The comparison against the cloud tier's 0.705 is **not** part of the claim: that number includes a
page reranker, and these runs have none. The claim is local-to-local: page retrieval goes from
0.277 to 0.626, text from 0.796 to 0.826, and a tuned fusion weight stops existing.

## What changes

| Area | Change |
|---|---|
| `kb-inference` text adapter | `EmbedFormat::OmniEmbed` — vLLM's `POST /v2/embed` (`input_type`, `inputs` as message content, `embeddings.float`). **Landed.** |
| `kb-inference` page adapter | `PageEmbedFormat::OmniEmbed`, the same shape for page images. **Landed.** |
| `config` | `KB_EMBEDDING_FORMAT=openai|omniembed` and `KB_PAGE_EMBEDDING_FORMAT=openrouter|vllm|omniembed`. **Landed.** |
| `tier.rs` | the 1B at **2048-d** for text and pages plus `Qwen3-Reranker-0.6B` on Desk, Studio and Station. **The default since 0.6.0**; `KB_EMBEDDERS=two` keeps the 0.3–0.5 stack. 0.6.0 also fixed the config overlay, which in 0.5.0 read the two-embedder table whatever `KB_EMBEDDERS` said |
| reranker | `Qwen3-Reranker-0.6B` on every GPU tier, replacing `bge-reranker-v2-m3` (Desk) and `Qwen3-VL-Reranker-2B/8B` (Studio/Station). Measured on MIRACL-id: nDCG@10 **0.795 → 0.850** (+0.055) over no-rerank, at 1.19 GB against 4.26 GB and 17.5 GB. The two VL rerankers it replaces had never been measured — [`eval-ab/rerank-2026-11-17`](../eval-ab/rerank-2026-11-17/README.md) |
| `compose.yaml` | one `vllm-embed-<tier>` service instead of `tei-embed-*` + `vllm-page-embed-*`; one GPU reservation instead of two |
| `setup.rs` | licence entry (OpenMDW-1.1, 🟢 after reading the licence: permissive, notice-keeping and a patent-suit clause only), the GPU tiers' model list, and the page arm default-on for Desk |
| docs | `README.md` model tables, `docs/kb-tiers.md` role table, `docs/benchmarks.md`, `CHANGELOG.md` |
| migration | **a re-index**: text vectors go 1024-d → 2048-d, and the page vectors change model. `docs/rust-cutover.md` gets the note; `kb-server reindex` rebuilds from the S3 originals |

## The 8B: measured, and not the default

Station was going to take `Nemotron-3.5-Embed-8B` (the teacher the 1B was distilled from). It was
measured on the same harness, and it is **better on pages** — one space 0.645 vs 0.621
(+0.024 [+0.011, +0.036]), image 0.642 vs 0.626 (+0.017 [+0.002, +0.031]), text-on-pages 0.639 vs
0.611 (+0.028 [+0.017, +0.040]), and a tie on MIRACL-id text (+0.005 [−0.008, +0.019]). Real, but
small, and it does not fit a 48 GB Station with the rest of the stack resident:

| Station, everything resident | 1B | 8B (bf16) |
|---|---:|---:|
| embedder | 2.4 GB | 16.3 GB |
| reranker (Qwen3-VL-Reranker-8B, FP8) | ~8.2 | ~8.2 |
| OCR (Xiaomi FP8 + 2 GB KV) | ~3.0 | ~3.0 |
| vision (Qwen3-VL-2B Q4) | ~1.5 | ~1.5 |
| the app's chat LLM | ~26 | ~26 |
| **total** | **~41 GB** | **~55 GB** |

The 8B only fits by quantising the embedder to FP8 (~8.5 GB, ~47 GB total) — an unmeasured
quality change on the model whose 0.024 was the reason to consider it. So the default is the 1B on
all three tiers, and the 8B stays the documented option for an 80 GB card, or for a Station
deployment whose pages are the measured bottleneck, or once the 8B in FP8 is measured.

What the 8B would cost beyond VRAM:

- **A different index width.** 4096-d on Station against 2048-d on Studio and Desk: a corpus
  would no longer move between tiers without a re-embed.
- **~2× the Qdrant memory per chunk** on Station against Studio (4096 floats vs 2048).
- **~8× the compute per embedding** against the 1B, on a card that also holds the chat LLM.
- **16.3 GB of bf16 weights** resident, against 2.4 GB.

The same arithmetic applies to every tier it might touch: one width (2048-d) across Desk, Studio
and Station is what makes a corpus portable and the image build identical.

## What must be measured before the preset flips

1. **Page reranker on top of the one-space list.** The shipped cloud number (0.705) has one and my
   0.621 does not, so the final Studio/Station claim is unknown until this runs. Expected: the
   reranker added +0.049 to the fused list before, which would land ≈0.67.
2. **Ingest throughput** at 32k context: a 1B VL model on the hot path against a 0.6B text model.
   `docs/eval-ab/ingest-big-pdf` (943 pages) is the case to re-run.
3. **VRAM on an 8 GB card** (Desk, emulated on the GB10 as `ocr-tiers` does): one model + the app's
   chat LLM, with the page arm on.
4. **One end-to-end Studio/Station run** — the `DEP-2` shape: health, upload, search — because the
   presets change, and a preset that cannot start is worse than a slower one.

**Decision, 2026-11-18:** the product owner flipped the default without waiting for (1)–(4)
(0.6.0). They remain open as follow-ups, and the numbers above are still the only ones measured;
none of (1)–(4) is claimed.

## Risks

- **Licence.** OpenMDW-1.1, read in full on 2026-11-18: permission "to deal in the Model Materials
  without restriction", keep the licence and notices when redistributing, rights end for a party
  that sues over the weights, nothing on outputs. That is Apache-2.0-like, so it is 🟢 and needs no
  opt-in. `--embedders two` stays as the escape hatch for a deployment that cannot re-index.
- **One model serves everything.** An embedder outage now costs retrieval on every arm, text
  included, instead of one role. `degrades` covers it, but the blast radius is larger.
- **Re-index cost.** 2048-d vectors for text chunks: roughly twice the Qdrant memory per chunk, and
  a full re-embed of every corpus. That is a migration, not a deploy.
- **32k context is not free.** The model takes whole pages with their text; that is what makes the
  one space possible, and it is also what makes the memory and throughput numbers above necessary.

## Done when

- `cargo test -p kb-inference -p kb-server` covers the new wire shape and the preset resolution;
- a live `rantai-kb up studio` ingests a scanned PDF and answers a page question with the model
  serving both roles, and `/health` reports the tier;
- `docs/benchmarks.md` carries the final number *with* the reranker, and the README's model table
  shows one model where it used to show two.
