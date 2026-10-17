| `KB_FIGURE_VLM_TOPN` | Top candidate figures evaluated by figure gate | `5` |
| `KB_FIGURE_VLM_MAX` | Maximum number of candidate figures accepted | `2` |
| `KB_FIGURE_VLM_TIMEOUT_MS` | Figure gate evaluation timeout in ms | `15000` |
| **Qdrant Store (Opt-in)** | | |
| `QDRANT_GRPC_URL` | Qdrant gRPC endpoint (e.g. `http://127.0.0.1:16334`) | `http://127.0.0.1:16334` |
| `QDRANT_URL` | Alternative Qdrant URL if gRPC URL is unset | `http://127.0.0.1:16334` |
| `QDRANT_API_KEY` | Optional Qdrant API key | `None` |
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

---

## Testing & Parity Verification

### 1. Workspace Unit & Integration Tests
```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

### 2. Live Smoke Test
Runs Postgres 16, SurrealDB v2, S3 (RustFS), and stub embeddings to execute an end-to-end ingest, chunking, SSE events, search, detail, list, and soft/hard delete cycle:
```bash
./rust/scripts/smoke.sh
```

### 3. Bidirectional Search Parity Matrix
Validates 100% search response parity between TypeScript (Bun) and Rust running against the same database and storage:
```bash
./rust/scripts/search-parity.sh
```
Runs a matrix of search bodies (queries, hybrid true/false/omitted, maxChunks 1/3/8/50, format "context", documentIds, knowledgeBaseIds, and categories), diffing normalized JSON responses (`jq -S`, rounded to 4 decimal places) and requiring 0 diffs.

### 4. Adapter Contract Tests

The shared `RetrievalIndex` contract suite (`kb-ports/contract`) runs against storage engines:

- **Legacy SurrealDB adapter**:
  ```bash
  rust/scripts/stack-up.sh && source /tmp/kb-stack/stack.env
  KB_SURREAL_CONTRACT=1 cargo test -p kb-store-surreal --features integration --test retrieval_index_contract -- --nocapture
  ```
- **Qdrant adapter**:
  The suite requires `KB_QDRANT_CONTRACT=1` and fails loudly if run without the env var set:
  ```bash
  KB_STACK_QDRANT=1 rust/scripts/stack-up.sh
  KB_QDRANT_CONTRACT=1 cargo test -p kb-store-qdrant --features integration --test retrieval_index_contract -- --nocapture
  ```

### 5. Qdrant exact-vs-HNSW benchmark

The number behind `kb_store_qdrant::index::EXACT_SEARCH_THRESHOLD` — the point
count below which the dense arm scans exactly instead of walking the graph. A
measurement rather than a pass/fail check, so it is `#[ignore]`d and needs a
live Qdrant:

```bash
KB_STACK_QDRANT=1 rust/scripts/stack-up.sh
cargo test -p kb-store-qdrant --features integration --test exact_vs_hnsw -- --ignored --nocapture
rust/scripts/stack-down.sh
```

It builds 1.5k / 15k / 50k 1024-dim points, runs 50 queries each in both modes,
and prints latency plus HNSW `recall@20` against the exact ranking. Results and
the decision they justify: `docs/eval-baseline.md` §7.5.3.
