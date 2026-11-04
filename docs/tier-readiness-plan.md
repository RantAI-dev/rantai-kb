# Tier readiness: every tier runnable, one command to set it up

Started 2026-10-30. Goal: every row of `docs/kb-tiers.md` "What each tier runs" is either wired
in code (preset + compose + adapter) or explicitly marked "not built" with the reason, and
`kb-server setup` takes a machine from nothing to a healthy tier.

## Gap list (code read 2026-10-30)

| Role | Doc says | Code today | Work package |
|---|---|---|---|
| OCR, Lite | PP-OCRv6 ONNX on CPU | ✅ preset → `rust/deploy/ocr-ppocr` sidecar (RapidOCR 3.10); image not built here yet | WP1 + WP2 |
| OCR, Desk/Studio | PaddleOCR-VL-1.6 | ✅ preset; compose: llama.cpp GGUF on Desk, vLLM on Studio (config-validated only) | WP1 + WP2 |
| OCR, Station | MinerU2.5-Pro / PaddleOCR-VL | ✅ preset + compose run PaddleOCR-VL on vLLM; MinerU stays an option | WP2 |
| OCR, Cloud | Mistral OCR | adapter exists; only on when `KB_MISTRAL_OCR_KEY` is set | WP3 (setup asks for the key) |
| Page embedder, Studio/Station | Qwen3-VL-Embedding-2B / 8B | ✅ preset + compose (`vllm-page-embed-*`); mock-tested, no GPU run | WP2 |
| Reranker, Studio/Station | Qwen3-VL-Reranker-2B / 8B | ✅ preset (`vllm` provider) + compose (`vllm-rerank-*`, model-card recipe + vLLM score template); no GPU run | WP2 |
| Charts/figures (captions) | chat LLM's vision (local), Gemini Flash (cloud) | `KB_PAGE_CAPTION_*` exists, opt-in, unmeasured | WP3 (setup asks for the chat LLM URL / enables Gemini Flash on cloud) |
| Parser / caption residency | `KB_PARSE_RESIDENCY`, `KB_CAPTION_RESIDENCY`, priority queue | not built | out of scope; stays documented as not built |
| `kb-server setup --tier` | detect, download, start, health-check, write config | ✅ detect, suggest, budget/licence gate, write `.env`/`.tier`, `--check` probes, `--start`; model download and start stay with the containers | WP3 |
| VRAM budget check | setup and server start warn/refuse | ✅ setup refuses unless `--force`; server start does not check yet | WP3 (setup only) |

## Fixed names (all packages use these)

- Local OCR adapter (WP1): extractor name `ocr` in `KB_LAYOUT_EXTRACTOR_ORDER`; settings
  `KB_OCR_BASE_URL` (OpenAI-compatible `/v1/chat/completions`), `KB_OCR_MODEL`,
  `KB_OCR_API_KEY` (optional; a hosted key is never sent to a loopback/LAN URL),
  `KB_OCR_PROMPT` (default `OCR:`, PaddleOCR-VL's task prefix), `KB_OCR_DPI` (default 150),
  `KB_OCR_CONCURRENCY` (default 2), `KB_OCR_TIMEOUT_MS` (default 120000),
  `KB_OCR_MAX_TOKENS` (default 4096).
- Compose services (WP2): `ocr-ppocr` (lite, CPU sidecar speaking the same chat shape),
  `ocr-paddlevl-desk` (llama.cpp), `vllm-ocr-<tier>`, `vllm-page-embed-<tier>`,
  `vllm-rerank-<tier>`. Loopback ports in the presets: 8081 text embed, 8082 rerank,
  8083 OCR, 8084 page embed.

## Validation

No NVIDIA box here, so GPU profiles are validated by `docker compose config`, unit tests and
mock servers only, and marked so. Live checks possible on this machine:
PaddleOCR-VL-1.6 GGUF under llama.cpp on CPU (the WP1 adapter, end to end, on a scanned PDF),
the PP-OCRv6 sidecar, `kb-server setup` on this CPU box (must suggest `lite`), and the lite and
cloud profiles.

## Measured: PaddleOCR-VL-1.6 on CPU (2026-10-30)

Official GGUF (`PaddlePaddle/PaddleOCR-VL-1.6-GGUF`, model 936 MB + mmproj 882 MB) under
`ghcr.io/ggml-org/llama.cpp:server`, 3 CPU threads (`--cpus 3`, shared machine), one ViDoRe HR
page (annual_report_on_intra-eu_labour_mobility, p. 12) rendered at 150 dpi, prompt `OCR:`:
**102.5 s for the page** (1,273 prompt tokens, 150 completion tokens). The output matched the
PDF's text layer word for word. So the request shape works end to end, and PaddleOCR-VL is a
GPU-tier parser: on CPU it is ~100× too slow for bulk ingest, which is why Lite keeps PP-OCRv6.
