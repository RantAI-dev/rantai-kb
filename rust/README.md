| `KB_MAX_FILE_BYTES` | Largest accepted upload, in bytes. A `413` names this variable, the limit in bytes and the limit in MiB | `268435456` (256 MiB) |
| `KB_UPLOAD_DEDUPE` | What an upload of a file the tenant already has does: `reject` answers `409` with the id of the document that already holds the bytes, `off` stores the file again. The scope is the tenant's, so another tenant's identical file is never a duplicate | `reject` |
| `KB_MCP` | Mount the thin Streamable-HTTP MCP endpoint at `POST /mcp` (`docs/api.md#mcp`). Off means `/mcp` is an unknown path (404) | `off` |
| `KB_FIGURE_VLM_TOPN` | Top candidate figures evaluated by figure gate | `5` |
| `KB_FIGURE_VLM_MAX` | Maximum number of candidate figures accepted | `2` |
| `KB_FIGURE_VLM_TIMEOUT_MS` | Figure gate evaluation timeout in ms | `15000` |
| `KB_MIN_SCORE` | Relevance floor for `/v1/search`: reranker score when a reranker ran, else dense cosine; a request's `minScore` overrides it | `None` (no floor) |
| `KB_ADAPTIVE_ARMS` | Classify each query and skip the retrieval arms its class cannot use (RET-5); a request's `adaptiveArms` overrides it. Skipped arms are named in `stats.skippedArms` | `false` |
| `KB_TREE_ARM` | Navigate a document's section outline for questions the classifier labels `structural` (DEC-4). Contributed chunks carry `retrieval.arms: ["tree"]` and `stats.treeArm` reports `{nodes, pages}`. Ingest always stores `Document.metadata.outline`; this switch gates only the search arm | `false` |
| `KB_TREE_ARM_WEIGHT` | RRF weight of the tree arm's page list when it is fused with the text candidates | `0.5` |
| `KB_TREE_ARM_NODES` | Outline nodes the tree arm keeps per query (the best-scoring sections) | `5` |
| `KB_GLOBAL_ARM` | **Deprecated (DEC-6); enabling it logs a startup warning and it may be removed after v1.0.** Global community-summary arm (`DEC-5`) for aggregative queries. A measured loss in EVAL-3; off by default. Needs `KB_GRAPH_STORE=pg` and communities built by `kb-server communities build` | `false` |
| `KB_GRAPH_ARM` | **Deprecated (DEC-6); enabling it logs a startup warning and it may be removed after v1.0.** Entity-graph retrieval arm, off by default; needs `KB_GRAPH_STORE=pg`. Behaviour is unchanged — warning only | `false` |
| `KB_GLOBAL_ARM_COMMUNITIES` | Communities selected per aggregative query | `3` |
| `KB_GLOBAL_ARM_WEIGHT` | RRF weight of the global arm's chunk list | `0.5` |
| `KB_GLOBAL_ARM_CHUNKS` | Chunks the global arm may contribute per query | `200` |
| `ENTITY_EXTRACTION_LLM_MODEL` / `_BASE_URL` / `_API_KEY` | Chat model the community build uses (the same settings as the LLM entity extractor; `OPENROUTER_API_KEY` is the key fallback) | `openai/gpt-4.1-nano` on OpenRouter |
| `KB_COMMUNITY_PRICE_INPUT` / `KB_COMMUNITY_PRICE_OUTPUT` | USD per 1,000,000 tokens for the build's cost estimate (`--price-input`/`--price-output` override) | built-in table for known models |
| `KB_CACHE_DIR` | Content-addressed disk cache. **Required for `communities build`** (every summary is cached by model + `sha256(prompt)`; an unset directory refuses a paid run) | unset: no cache |
| **Qdrant Store (the only chunk store)** | | |
| `QDRANT_GRPC_URL` | Qdrant gRPC endpoint (e.g. `http://127.0.0.1:16334`) | `http://127.0.0.1:16334` |
| `QDRANT_URL` | Alternative Qdrant URL if gRPC URL is unset | `http://127.0.0.1:16334` |
| `QDRANT_API_KEY` | Optional Qdrant API key | `None` |
| `KB_QDRANT_QUANTIZATION` | Opt-in dense-vector quantization (`off`/`scalar`/`binary`); applied at collection creation only, unknown values fail startup | `off` |
| `KB_QDRANT_QUANT_OVERSAMPLING` | Quantized-search oversampling factor, clamped to 1.0..8.0 | `2.0` |
| `KB_QDRANT_QUANT_RESCORE` | Re-score quantized candidates with the original float vectors | `true` |
| `KB_QDRANT_CONTRACT` | Set to `1` to run the `RetrievalIndex` contract test suite against live Qdrant | `None` |

---

## Docker

Multi-stage build with `cargo-chef` dependency caching:

```bash
# Build local amd64 image
docker build -t rantai-kb:rust -f rust/Dockerfile .

# Run container
docker run -p 8080:8080 --env-file .env rantai-kb:rust
```

The runtime image is built on `debian:bookworm-slim`, runs as non-root user `appuser` (uid 10001), includes liberation fonts and CA certificates, and bundles `libpdfium.so` dynamically loaded per `TARGETARCH`.

For a complete stack (stores + models + kb-server) rather than a bare container, use the
one-command dev deployment:

```bash
cp rust/deploy/.env.example rust/deploy/.env   # optional; only cloud needs a key
rust/scripts/kb-dev.sh up lite-en              # lite | lite-en | desk | studio | station | cloud
```

It runs `rust/deploy/compose.yaml` under the compose project `rantai-kb-dev`, publishes only
kb-server's port, and remembers the tier for `logs`/`status`/`key`/`down`. Tiers, measured
numbers, upload/search examples, GPU notes and troubleshooting: [`deploy/README.md`](deploy/README.md).

---

## Stateless Processor Mode (Microservice)

To run `kb-server` purely as a document-processing microservice — **zero Postgres, Qdrant,
or S3** — start it with `--stateless` or `KB_MODE=stateless`:

```bash
KB_MODE=stateless \
KB_STATELESS_API_KEY=change-me \
kb-server serve
# or:
kb-server serve --stateless
```

| Env var | Purpose | Default |
|---|---|---|
| `KB_MODE` | `full` (default) or `stateless` (`processor` alias) | `full` |
| `KB_STATELESS_API_KEY` | Comma-separated static API keys (`Authorization: Bearer` or `X-API-Key`) | unset |
| `KB_STATELESS_ALLOW_UNAUTHENTICATED` | Explicit opt-in to disable auth on a private VPC / sidecar network | `false` |
| `KB_STATELESS_EMBED` | Set to `false` to disable dense embeddings even when `OPENROUTER_API_KEY` is set for OCR | `true` (when embedder config is present) |
| `KB_STATELESS_RERANK` | Set to `false` to disable `POST /v1/rerank` even when a tier preset enables reranking | `true` (when `KB_RERANK_*` or tier enables rerank) |
| `KB_STATELESS_MAX_INFLIGHT` | Max concurrent `POST /v1/process` uploads admitted before shedding with `503` + `Retry-After` (`0` = unlimited) | `max(extract_seats * 4, 4)` |

Print the stateless OpenAPI 3.1 contract without booting:

```bash
kb-server openapi --stateless
```

---

## API reference

The HTTP API is described by an OpenAPI 3.1 document that the server serves
itself, so the contract can be read — and tried in a browser — before a client
is written:

- `GET /openapi.json` — the document.
- `GET /docs` — Swagger UI rendering that document.

Both answer **without a key**, like `/health`: a caller has to be able to read
the contract before it has one. The `/v1` routes keep exactly the
authentication they always had — `Authorization: Bearer <key>` or
`X-API-Key: <key>` — and the document says which scheme and scope each
operation needs.

```bash
kb-server openapi > ../docs/openapi.json   # the same document, printed
```

The UI's assets are embedded in the binary (`utoipa-swagger-ui`'s `vendored`
feature), so the service never reaches a CDN at runtime and the docs work on
an air-gapped host.

The document is derived from the handlers themselves, and
`tests/openapi_tests.rs` checks it against the route table in both
directions: a route that is missing from the document, or an operation that is
not a route, fails the build.

---

## Testing & Live Verification

### 1. Workspace Unit & Integration Tests
```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

### 2. Live Smoke Test
Runs Postgres 16, Qdrant, S3 (RustFS), and stub embeddings to execute an end-to-end ingest, chunking, SSE events, search, detail, list, and soft/hard delete cycle:
```bash
./rust/scripts/smoke.sh
```

### 3. Retrieval Eval & Scope Gate
The Qdrant retrieval-quality and cross-scope leak gate (the same one CI runs):
```bash
./rust/scripts/ci-eval-gate.sh
```

Qdrant is the only chunk store, so there is no live search-parity harness. The Bun
service was removed in v0.2.0 (last Bun release: tag `v0.1.0`); a v0.1.0 deployment
moves its index with `kb-server reindex` (see below).

### 4. Adapter Contract Tests

The shared `RetrievalIndex` contract suite (`kb-ports/contract`) runs against Qdrant:

The suite requires `KB_QDRANT_CONTRACT=1` and fails loudly if run without the env var set:
```bash
rust/scripts/stack-up.sh
KB_QDRANT_CONTRACT=1 cargo test -p kb-store-qdrant --features integration --test retrieval_index_contract -- --nocapture
```

### 5. Qdrant exact-vs-HNSW benchmark

The number behind `kb_store_qdrant::index::EXACT_SEARCH_THRESHOLD` — the point
count below which the dense arm scans exactly instead of walking the graph. A
measurement rather than a pass/fail check, so it is `#[ignore]`d and needs a
live Qdrant:

```bash
rust/scripts/stack-up.sh
cargo test -p kb-store-qdrant --features integration --test exact_vs_hnsw -- --ignored --nocapture
rust/scripts/stack-down.sh
```

It builds 1.5k / 15k / 50k 1024-dim points, runs 50 queries each in both modes,
and prints latency plus HNSW `recall@20` against the exact ranking. Results and
the decision they justify: `docs/eval-baseline.md` §7.5.3.
