# `DEP-6` — one embedder on the GPU tiers (`Nemotron-3.5-Embed-1B`)

Decided by the product owner, 2026-11-17: **Desk, Studio and Station run one embedding model for
text chunks and page images** — `Nemotron-3.5-Embed-1B` on Desk and Studio, and
`Nemotron-3.5-Embed-8B` on Station (decided later the same day). `lite` and `lite-en` keep `multilingual-e5-small` /
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
| `kb-inference` text adapter | `EmbedFormat::OmniEmbed` — vLLM's `POST /v2/embed` (`input_type`, `inputs` as message content, `embeddings.float`). *Landed with this spec.* |
| `kb-inference` page adapter | the same format for page images (`PageEmbedFormat::OmniEmbed`) — **not yet** |
| `config` | `KB_EMBEDDING_FORMAT=openai|omniembed` (*landed*), `KB_PAGE_EMBEDDING_FORMAT=omniembed` (**not yet**) |
| `tier.rs` | `desk`, `studio`: one model at **2048-d** (1B); `station`: the **8B at 4096-d**; text and pages per tier; drop the TEI text embedder and the separate page embedder from the presets |
| `compose.yaml` | one `vllm-embed-<tier>` service instead of `tei-embed-*` + `vllm-page-embed-*`; one GPU reservation instead of two |
| `setup.rs` | licence entry (`openmdw-1.1` → 🟡 opt-in), budget estimate for one 1B VL model, and the page arm becoming default-on for Desk |
| docs | `README.md` model tables, `docs/kb-tiers.md` role table, `docs/benchmarks.md`, `CHANGELOG.md` |
| migration | **a re-index**: text vectors go 1024-d → 2048-d, and the page vectors change model. `docs/rust-cutover.md` gets the note; `kb-server reindex` rebuilds from the S3 originals |

## Why Station gets the 8B, and what it costs

The 1B was distilled from the 8B, so the 8B is the quality ceiling of the family and the natural
"station is the big box" choice. It is also the one part of this change nobody has measured yet:
every number in this spec is the **1B**. What the 8B costs, concretely:

- **A different index width.** 4096-d on Station, 2048-d on Studio and Desk. Moving a corpus
  between tiers is a re-embed, not a vector copy — the same class of change as today's 0.6B→2B
  page difference, and the reason `reindex` exists.
- **~2× the Qdrant memory per chunk** on Station against Studio (4096 floats vs 2048).
- **~8× the compute per embedding** against the 1B, on a card that also holds the chat LLM; the
  ingest budget in `eval-ab/ingest-big-pdf` (943 pages under 200 s) has to be re-checked on the
  8B, not assumed from the 1B.
- **16.3 GB of bf16 weights** on disk and resident, against 2.4 GB for the 1B.

The measurement that closes it is the same harness on the same pages (MIRACL-id text, ViDoRe HR
text/image/one-space) with `Nemotron-3.5-Embed-8B`, which the GB10 can serve. If the 8B does not
beat the 1B by more than the noise, Station should take the 1B too and keep Studio's index width —
a cheaper box and a portable corpus for no measured loss.

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

Until (1)–(4) are in, the presets stay as they are and this model is documented as `EVAL-6`'s
candidate. The adapter and config flag landing first is deliberate: they are inert until a preset
selects them.

## Risks

- **Licence.** `openmdw-1.1`, an opt-in in `setup`, not Apache-2.0. A deployment that cannot accept
  it keeps the Qwen pair, so the presets need an escape hatch rather than a replacement.
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
