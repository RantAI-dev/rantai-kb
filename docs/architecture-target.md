# Target architecture: matching industry and state-of-the-art structure

Written 2026-10-16. This compares the Rust port's structure today (audited from code) with
the structures that leading RAG systems and standards use, then lists the changes. It goes
with `docs/plan-best-multimodal-kb.md`; step S there carries out this document.

References used:
- Docling's `DoclingDocument` (the de facto open document model).
- Vespa's RAG Blueprint (multi-phase ranking).
- Qdrant's multitenancy and hybrid query guides.
- RAGFlow (layout-aware parsing with coordinate citations).
- Onyx (staged indexing).
- OpenAI's vector-store search API (the de facto search API shape).
- OpenTelemetry GenAI / OpenInference span kinds.
- BEIR/TREC eval formats.

## Verdict

- **Already at standard:** the code layering. It uses ports and adapters with a checked boundary,
  a pure core and a single composition root. Production Rust services of this kind are built
  this way.
- **Below standard in four places:**
  1. **The document model is flat.** Documents go straight to chunks. There are no stored
     pages, layout blocks, bounding boxes, or parent and child chunks.
  2. **The storage ports are shaped by SurrealDB and Bun, not by the domain.** Record-id
     formats, SurrealDB's NULL workaround, an unscoped BM25 method and figure heuristics all
     sit inside the port contracts. As a result, Qdrant and graph adapters can't simply be
     dropped in.
  3. **Nothing checks adapters.** No shared contract tests exist. Engine and server tests run
     only on hand-written fakes, and real stores are never exercised in `cargo test`.
  4. **There is no observability or API contract.** We have no spans, metrics, request ids,
     OpenAPI spec or machine-readable error codes.

## 1. Document model: from flat to layout-aware (Docling-style)

The standard is the `DoclingDocument` model, which RAGFlow, LightRAG/RAG-Anything and others
also follow. A document is a tree of typed items. Every item records its page and bounding
box, and body content is kept apart from "furniture" such as headers and footers.

Target model, with the database that owns each entity:

```
Tenant (PG, new table: limits, default profile)
└─ KnowledgeBase / Category (PG, exists)
   └─ Document (PG, exists) ── DocumentVersion (PG, new: content_hash, parser+version, created_at)
      ├─ ParsedDocument artifact (S3, new): DoclingDocument-compatible JSON. Holds the full tree:
      │    pages{size, render_key}, items{label, text, bbox, page, reading_order, parent},
      │    labels: title, section_header, paragraph, list_item, table, picture, caption, formula,
      │            footnote, page_header, page_footer (furniture)
      ├─ Page (Qdrant `pages` collection + S3 render): one point per page; image vector(s) + page text
      ├─ Chunk (Qdrant `chunks` collection): the retrieval unit (child)
      │    payload: tenant_id, kb_ids, category_ids, document_id, version, page_span, bboxes[],
      │             section_path[], element_labels[], parent_id, chunk_type, lang, content_hash
      ├─ Parent unit (no index): a section, or a page if there are no headings. Returned for context.
      ├─ Figure unit (Qdrant `chunks`, chunk_type=figure): caption + crop key + bbox
      ├─ Table (PG metadata + Parquet in S3, see section 10): structured grid, title, headers, spans
      │    └─ table units in Qdrant: table summary, rows (small tables), cell values (large tables)
      └─ Entity / Mention / Relation (PG tables, see section 11): graph, traversed in Rust memory
```

Why each part matters:
- **Page and bounding box on every chunk.** This enables grounded citations: the answer can
  highlight the exact region of the page. ViDoRe v3 now scores bounding-box grounding, and
  RAGFlow cites by layout coordinates.
- **Labelling headers and footers as furniture.** This is the standard replacement for
  ParaGPT's hand-written cleanup of repeated headers and footers. The layout parser labels
  them, and we skip them when chunking.
- **Storing the parsed artifact.** Re-chunking or re-embedding no longer needs a re-parse. That
  makes ablations free, which matters on a near-zero budget, and parser upgrades become a
  version bump.
- **Parent and child chunks (small-to-big).** Small chunks are retrieved; their section or page
  is returned to the model. This decouples retrieval granularity from context size and is the
  common pattern in H-RAG, LlamaIndex and Vespa's top-3-chunks-per-document.
- **Contextual embedding text.** Embed `title > section path > chunk`. This is cheap and
  standard. Late chunking and LLM contextual retrieval (Anthropic reports 35–50% fewer misses)
  are ablations in the eval, because the second costs an LLM call per chunk.
- **DocumentVersion.** Re-ingest becomes an atomic switch between versions instead of
  delete-then-insert, and it gives us a provenance trail.

Compatibility: Bun reads `Document.content` and SurrealDB chunks. The new Postgres tables are
additive, so they are safe during the mixed-fleet period. The new chunk payload only appears in
Qdrant, which Bun never reads.

## 2. Index layout in Qdrant (vendor-recommended multitenancy)

- **One collection per (representation, embedding model)**, for example
  `chunks__qwen3-emb-0.6b__1024` and `pages__qwen3-vl-emb-2b__1024`. Spaces never mix, and
  changing models means building a new collection and then switching an alias with no
  downtime. A `kb_index` Postgres table records which collection backs which alias.
- **Named vectors in each collection.** `dense` (HNSW, scalar or binary quantised with
  rescoring) plus `sparse` (IDF modifier, for BM25-style search). `pages` adds an optional
  `multi` vector (MaxSim).
- **Tenancy.** One shared collection, with a `tenant_id` keyword index using `is_tenant: true`
  so each tenant's points are stored together.
  - Many small tenants: `m: 0` and `payload_m: 16`, which builds an HNSW graph per tenant.
  - Large tenants: tiered multitenancy (v1.16+), promoting tenants to dedicated shards at
    about 20k points.
- **IDF statistics per tenant.** The `idf` corpus filter (v1.19+) keeps another tenant's
  vocabulary from skewing keyword scores. This fixes the cross-tenant crowding problem we
  documented in the BM25 arm.
- **Point ids.** UUIDv5 of (tenant, document, version, chunk index). They are deterministic
  and idempotent.

## 3. Retrieval: multi-phase ranking (Vespa blueprint)

```
query ─ rewrite? (follow-ups only) ─ embed (cached)
  phase 1  candidate generation, one Qdrant round trip:
           prefetch dense(top 100) + sparse(top 100) [+ pages(top 24)], all filtered by scope → RRF
  phase 2  cross-encoder rerank of top N (text reranker; multimodal reranker for pages)
  phase 3  global: group by document (top 3 chunks/doc), diversity (MMR), per-doc cap,
           figure policy, parent expansion, token-budget packing
  output   chunks + parents + pages + figures, each with provenance {doc, version, page, bbox}
```

- **Learned fusion (stretch).** Vespa trains a logistic-regression or GBDT ranker on rank
  features gathered from labelled queries. Once our eval set exists, the same feature log
  (dense, sparse, rerank, page and title scores) can replace hand-tuned RRF weights.
- **Scope is a single value.** `DocumentScope` is built once and applied as a filter in every
  phase. No arm may run unscoped, so `bm25_search_global` is deleted.

## 4. Ports: domain-shaped, small, each with a contract suite

Today `DocumentStore` has 26 methods and `ChunkStore` mixes vector search, keyword search,
neighbours and figure heuristics. Target ports (traits in `kb-ports`, no storage concepts):

| Port | Methods (sketch) | Adapters |
|---|---|---|
| `DocumentRepo` | documents and versions CRUD, status as an enum | pg |
| `CatalogRepo` | knowledge bases, categories | pg |
| `AuthRepo` | API keys → `Principal` + `DocumentScope` | pg |
| `ArtifactStore` | put/get raw file, parsed doc, page render, crop | s3 |
| `RetrievalIndex` | `upsert(units)`, `delete(doc, version?)`, `search(HybridQuery, &Scope) -> Candidates`, `fetch(ids)`, `capabilities()` (dims, sparse, multivector, server-side fusion) | qdrant, surreal (legacy, read-only until it is removed), in-memory |
| `GraphStore` | `upsert_entities`, `relate`, `delete_document`, `neighbourhood(entity_keys, depth, &Scope)`, `ppr(seeds, &Scope)` | pg (+ in-memory CSR cache), in-memory; ArcadeDB only if the benchmark requires it |
| `TableStore` | `put(table)`, `get(id)`, `query(id, TableQuery, &Scope)`, `delete_document` | pg + parquet/S3, DuckDB executor |
| `JobQueue` | as today | pg |
| `Embedder` / `MultimodalEmbedder` / `Reranker` / `LayoutParser` / `Captioner` | as today plus image inputs | local (ONNX, llama.cpp, vLLM, TEI) and cloud (OpenRouter, Voyage, Mistral) |

Rules:
- **The domain owns ids.** Adapters own the id encoding. Engine code never formats `entity:…`
  or SurrealDB record ids.
- **Figure, neighbour and parent logic live in `kb-engine`**, not in the store adapter.
- **Every port gets a contract test suite** (`kb-ports/contract/`, generic over the trait).
  The in-memory fake and every real adapter must pass the same suite. Real adapters run it
  against containers (testcontainers or the CI service containers), behind a `--features
  integration` gate. This is the standard way to make adapters swappable, and it is what makes
  the Surreal → Qdrant move safe.

## 5. Ingest: staged, content-addressed, resumable (like a build system)

```
raw (S3, sha256) → parse (ParsedDocument, keyed by sha256+parser@ver)
  → enrich (captions, keyed by crop sha256+captioner@ver+prompt@ver)
  → chunk (keyed by parsed hash+chunker@ver) → embed (keyed by text hash+model)
  → index (version switch) → graph (entities, keyed by chunk hash+extractor@ver)
```

- Each stage's output is cached under its key, so a re-run only executes stages whose inputs
  or versions changed. This is the near-zero-budget rule built into the structure, and it
  matches Onyx's multi-pass indexing and ParaGPT's content-hash dedupe.
- Stage boundaries map to job-queue steps, so a crash resumes at the failed stage instead of
  starting over.

## 6. API: native plus the de facto standards

- **Keep `/v1/search`.** Add `include: [parents, pages, figures, bboxes]` and machine-readable
  provenance.
- **Add an OpenAI-compatible vector-store search endpoint:**
  - Path: `POST /v1/vector_stores/{kb}/search`.
  - Request: `query`, `filters` (comparison/compound), `max_num_results`,
    `ranking_options{ranker, score_threshold}`, `rewrite_query`.
  - Response: `{data:[{file_id, filename, score, attributes, content:[{type,text}]}]}`.
  - Many agent frameworks then work with no client code.
- **Add an MCP server** (`kb-server mcp`) exposing `search`, `get_document` and `get_page`, so
  local and cloud agents can use the KB directly.
- **Generate an OpenAPI 3.1 spec** from the code (utoipa) and test it in CI.
- **Use RFC 9457 problem+json errors with stable `code`s.** `PortError` must stop becoming a
  bare 500.

## 7. Observability: OpenTelemetry with GenAI conventions

- `TraceLayer` with request ids, and `#[instrument]` spans per stage using OpenInference kinds
  (`RETRIEVER`, `EMBEDDING`, `RERANKER`, `VECTOR_DB`, `LLM`).
- Attributes: model, top_k, candidate counts, scores, `degrades`.
- OTLP export (opt-in).
- `/metrics` (Prometheus): per-stage latency histograms, ingest throughput, queue depth,
  cache hit rates, cost counters per provider.
- JSON logs in production.

## 8. Evaluation: standard formats

Datasets are stored in BEIR/TREC layout (`corpus.jsonl`, `queries.jsonl`, `qrels/*.tsv`), so
ViDoRe, BEIR and MMLongBench import directly. Our metric code is cross-checked once against
`ranx`/`ir_measures`. Runs are saved as TREC run files, so any standard tool can re-score them.

## 9. Code layout (target crates)

```
kb-core           pure domain: document model, chunking, fusion, packing, policies
kb-ports          traits + contract test suites (feature `contract`)
kb-engine         ingest stages, retrieval phases (no adapter names)
kb-store-pg       documents, versions, catalog, auth, jobs, index registry
kb-store-qdrant   RetrievalIndex (new)
kb-store-pg       (also) GraphStore: entity/mention/relation tables + Rust CSR traversal
kb-tables         table model, serialisers, DuckDB query executor (new)
kb-store-blob     ArtifactStore
kb-store-surreal  legacy RetrievalIndex: read-only for migration, then deleted
kb-inference      embed/rerank/caption/layout clients, local + cloud profiles
kb-extract        format parsers → ParsedDocument
kb-eval           datasets, metrics, bench (new)
kb-api            DTOs + OpenAPI (split from kb-server)
kb-server         composition root, HTTP, MCP, worker, CLI
```

## 10. Tables: first-class, structured, queryable

Evidence:
- **T²-RAGBench** (EACL 2026, 23k questions over financial reports with text and tables):
  hybrid search + rerank wins, with Recall@5 0.816 against 0.695 for hybrid alone. BM25 beats
  dense retrieval on numeric content (0.644 vs 0.587). HyDE, multi-query and corrective RAG
  give "limited benefit for precise numerical queries".
- **Serialisation studies:**
  - Markdown is best for simple tables (TQA-Bench), and CSV is the most token-efficient.
  - HTML preserves structure best for merged cells and multi-level headers (about 3× the
    tokens).
  - Key–value rows ("Header: value") scored highest for row lookups in one study.
- **TableRAG (Google):** never put a whole large table into context. Retrieve the *schema*
  (columns with types and sample values) and *cell values* separately, then answer with a
  program.
- **ParaGPT in production:**
  - Tables are stored as structured grids with real titles.
  - A manifest says how reliable each table is: XLSX formulas are known, while PDF and CSV
    derived rows are guessed.
  - The model computes numbers through `tabel_query`/`tabel_hitung` tools, never by reading
    flattened text.
  - When a document is named in the question, its exact table labels are injected; models
    otherwise invent table names and give up.

Target structure:
- **Table model** (`kb-tables`): `Table {id, document, version, source: xlsx|csv|pdf|ocr|docx,
  sheet, page_span, bbox, title, caption, header_rows (multi-level), cells with
  row_span/col_span, column types, footnotes, manifest{formulas_known, derived_rows_guessed,
  truncated, confidence}}`.
  - Metadata lives in Postgres. The grid is stored as Parquet in S3: columnar, compressed, and
    directly queryable.
- **Extraction rules:**
  - Spread merged cells.
  - Detect multi-level headers.
  - Skip hidden sheets, with a note.
  - Use cached formula values, and reject formula-only files.
  - Stitch PDF tables that continue across pages.
  - Drop repeated "furniture" tables (letterheads, signature blocks).
  - Take the title from the text just above the table.
  - Rebuild tables that a converter split into one paragraph per cell.
  - Pick the parser per table type using TEDS, the table-structure metric from OmniDocBench.
- **Four retrieval units per table** (Qdrant `chunks`, `chunk_type=table*`):
  1. `table_summary`: title, caption, headers, column types and sample values. This is schema
     retrieval and finds *which* table.
  2. `table_row`: one unit per row, as `Header: value; …`, for small tables, glossaries and
     key–value tables (ParaGPT's glossary promotion, generalised).
  3. `table_cells`: distinct values per column, capped by frequency (TableRAG), in the sparse
     index only, so exact codes and names hit.
  4. The table rendered inside normal text chunks: Markdown when simple, HTML when it has spans
     or multi-level headers. The header row repeats in every chunk that splits the table.
- **Computing answers in code:**
  - `POST /v1/tables/{id}/query` takes a restricted, typed DSL (select / filter / group /
    aggregate / sort / limit). The MCP tool is `table_query`.
  - It runs on **DuckDB** (MIT, embedded, `duckdb` crate) over the table's Parquet: read-only,
    no file or network access, row and time limits.
  - The response carries the manifest, so the model can say "this total is computed from a PDF
    table; derived rows were guessed".
  - Optional text-to-DSL helper. A DSL rather than raw SQL keeps the attack surface small.
- **Search response:** table hits return `table_id`, matched rows with their header, the
  rendered snippet, and exact table labels for the documents involved, so the caller can call
  `table_query` without guessing names.

## 11. Graph: Postgres storage, Rust in-memory traversal

Evidence:
- **"When to use Graphs in RAG"** (GraphRAG-Bench, ICLR 2026): graph RAG often *underperforms*
  vanilla RAG on fact lookup, and wins on multi-hop, synthesis and summarisation.
  - HippoRAG2 (entity graph + personalised PageRank, about 1k-token prompts) ranks first at
    56.48.
  - LightRAG ranks tenth at 45.09, with about 10k-token prompts.
- **Store shape:** LightRAG replaced its Apache AGE backend with plain Postgres tables
  (`PGTableGraphStorage`) because the tables were faster and smaller.

Licence screen of graph databases:

| Database | Licence | Status |
|---|---|---|
| FalkorDB | SSPLv1 | ❌ |
| Memgraph | BSL 1.1 | ❌ |
| HelixDB | AGPL-3.0 (official repo) | ❌ for a hosted service |
| Neo4j Community | GPLv3, enterprise features locked | not chosen |
| Kùzu | archived 2025-10 | ❌ |
| LadybugDB (its fork) | MIT, Rust crate `lbug` | ⚠ embedded and single-process, so it can't be shared by API and worker replicas; young project |
| ArcadeDB | Apache-2.0, JVM server, Cypher/Bolt | ✅ challenger |
| Postgres tables | PostgreSQL licence, already running | ✅ chosen |

Design:
- **Postgres tables:**
  - `kb_entity(id, tenant_id, canonical_name, type, aliases[], embedding_ref)`
  - `kb_mention(entity_id, chunk_id, document_id, version)`
  - `kb_relation(src, dst, type, weight, chunk_id, document_id)`

  All are indexed by `(tenant_id, …)`. Deleting a document cascades. Entity dedupe and
  canonicalisation happen at ingest; that is HippoRAG2's typed consolidation step.
- **Traversal in Rust:**
  - The graph for each tenant is loaded into a **compressed adjacency array (CSR) in memory**
    and cached. It is invalidated by a version counter that ingest bumps.
  - Personalised PageRank from seed entities (matched from the query through the sparse
    index), 1–2 hop expansion and mention → chunk scoring all run in microseconds to
    milliseconds, in process, with no network hop.
  - This is faster than querying any external graph database, and Postgres stays the single
    source of truth.
- **Retrieval arm:** a `graph` prefetch candidate list (chunks ranked by PPR), fused like
  the other arms. It stays behind a flag until the eval shows gains on multi-hop and
  cross-page questions.
- **Targets:** PPR + expansion p95 < 10 ms for a tenant graph with 1M edges. Memory for the
  tenant cache stays under 200 MB per 1M edges, with LRU eviction across tenants.
- **Fallback:** if those targets fail at scale, benchmark ArcadeDB behind the same
  `GraphStore` port.

## Risks and open decisions

- **Graph store.** FalkorDB was rejected (SSPLv1). Memgraph (BSL) and HelixDB (AGPL) were
  rejected for the same reason. Kùzu was archived in October 2025. Decision: Postgres tables
  plus in-memory traversal in Rust (section 11). ArcadeDB (Apache-2.0) is the challenger if
  the benchmark misses its target.
- **Port refactor during the mixed-fleet period.** The new ports must still drive the SurrealDB
  adapter in the Bun-compatible format until cutover. The contract suites are what keep this
  safe.
- **Scope of change.** Sections 1–4 are a large refactor. Step S in the plan orders it so that
  each stage ships alone and is measured.
