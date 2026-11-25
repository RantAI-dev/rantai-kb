# Scope scale and stale payloads (SEC-2c)

> **Both instruments write straight into Postgres and Qdrant** (synthetic tenants; stale
> moves, soft-deletes and ACLs). Run them only against a throwaway stack. Each refuses to run
> without `--allow-db-writes`.

Two instruments prove the two halves of SEC-2 on a live stack:

1. `kb-server eval scope-bench` — does predicate scope keep search latency flat
   as a tenant grows from 1k to 100k documents?
2. `kb-server eval leak --stale-payload` — when Qdrant disagrees with Postgres,
   does *any* retrieval arm or document-addressed route leak?

**Results: not run yet on this branch.** The instruments are built and
unit-tested offline; the numbers below are the method, not measurements. A
pre-SEC-2d live run exists on `origin/bench/v1-measure` (its "Results" section
records the 2026-11-25 baseline: predicate p95 growth at 100k and 24 `mcp`
false positives from a shared tenant). SEC-2d changed the scope builder, the
MCP probe and the bench output **after** that run, so those numbers are a
baseline to beat, not a current result. Never report a p50/p95 or a leak count
from this page without a fresh live run with the method below.

## 1. Scope bench: two passes, one output directory

The tool creates one synthetic tenant per size, inserts its `Document` rows and
`DocumentGroup` links directly in Postgres in batches, and upserts one small
point per document directly into Qdrant with the same payload `unit_payload`
writes in production (tenant, document, KB array, chunk index, page, content
with a unique token). No ingest, no embedding provider, no paid batch.

`KB_SCOPE_ID_LIMIT` is read by the **server at startup**, so the operator makes
two passes and restarts between them. The tool only labels rows:

```bash
export CARGO_TARGET_DIR=/home/shiro/kb-target-sec2c
export CARGO_BUILD_JOBS=2
export PDFIUM_DYNAMIC_LIB_PATH=/home/shiro/rantai-kb/rust/.vendor/pdfium/lib/libpdfium.so
export DATABASE_URL='postgresql://kb:kb@localhost:5432/rantai_kb'

# Pass 1 — Ids mode. Note that 0 is also the default.
KB_SCOPE_ID_LIMIT=0 kb-server serve &
IDS_PID=$!
# wait for /health
kb-server eval scope-bench \
  --base-url http://127.0.0.1:3000 \
  --database-url "$DATABASE_URL" \
  --sizes 1000,10000,100000 \
  --queries 200 \
  --out docs/eval-ab/scope-scale/out \
  --mode-label ids --allow-db-writes
kill "$IDS_PID"

# Pass 2 — Predicate mode. Restart with the new limit.
KB_SCOPE_ID_LIMIT=2000 kb-server serve &
PRED_PID=$!
kb-server eval scope-bench \
  --base-url http://127.0.0.1:3000 \
  --database-url "$DATABASE_URL" \
  --sizes 1000,10000,100000 \
  --queries 200 \
  --out docs/eval-ab/scope-scale/out \
  --mode-label predicate --allow-db-writes
kill "$PRED_PID"
```

Flags: `--base-url`, `--database-url` (or `$DATABASE_URL`), `--key-admin`
(optional; keys are minted in Postgres with `create-api-key` internals),
`--sizes`, `--queries`, `--out`, `--mode-label`, `--keep`.

Both passes append to the same `results.json`; `report.md` shows size × mode,
p50/p95, mean `stats.scopeVerifyDropped`, and the verdict. The target is
**predicate p95 at 100k ≤ 1.2 × predicate p95 at 1k**. Before both passes exist
the verdict is `NOT_RUN`, never `PASS`. `--keep` leaves the synthetic tenants
behind (their ids are in `results.json`); by default they are removed from
Postgres and Qdrant after each size.

Queries are deterministic per size, so both passes run the same workload. The
vectors are random unit vectors of the server's configured embedding dimension;
they measure scope-filter cost, not retrieval quality.

Since SEC-2d, each row also carries the p50/p95 of **every scalar field** in the
response's `stats.stageTimingsMs` — `scopeBuild`, `expand`, `arms`, `enrich`,
`fusion`, `rerank`, `pages`, `neighbours`, `figures`, `context`, `serialise` and
`total` — in `results.json` and in a second "Stage timings" table of
`report.md`. A growth in `scopeBuild` now points at Postgres scope resolution;
a growth in `arms` or `rerank` points elsewhere. The nested `detail` object is
not aggregated: its measurements overlap those stages (the query embed and both
arms happen inside `arms`), so a p95 over them would not be additive. Rows
written before this change have no stage timings and are shown as "not reported".

## 2. Stale-payload leak matrix

Seed the normal leak corpus, then inject drift in **Postgres only** and leave
the Qdrant payloads untouched:

| case | SQL (ids are bound parameters) | expected fence |
|---|---|---|
| D | `UPDATE "DocumentGroup" SET "knowledgeBaseId" = $2 WHERE "documentId" = $1 AND "knowledgeBaseId" = $3` | predicate verify drops D for a KB-1-limited key |
| E | `UPDATE "Document" SET "deletedAt" = now(), "updatedAt" = now() WHERE "id" = $1` | soft-delete exclusion |
| F | `UPDATE "Document" SET "aclPrincipals" = $2::text[] WHERE "id" = $1` | SEC-3 default-deny for a principal-less key |
| all | `DELETE FROM "ScopeOutbox" WHERE "documentId" = $1` | no repair may heal the drift mid-run |

F's tenant has an ACL'd document, so `predicate_spec_for` refuses predicate
mode there **regardless of `KB_SCOPE_ID_LIMIT`**: F's probes are the evidence
that the Ids path is exercised. The tool records that in `leak.json` notes.

```bash
kb-server eval leak \
  --base-url http://127.0.0.1:3000 \
  --api-key "$KB_EVAL_API_KEY" --peer-key "$KB_EVAL_PEER_API_KEY" \
  --stale-payload --allow-db-writes --database-url "$DATABASE_URL" \
  --out docs/eval-ab/scope-scale/out/leak-stale \
  --strict
```

A leak is **any** appearance of D under the KB-1-limited key, or of E or F
under a principal-less key. Probes cover every arm in the normal matrix
(vector, BM25, neighbour, figure, graph, page, tree, global, table, entity) plus
`GET /v1/documents/:id/content`, `/structure`, the table routes and `/mcp`
(`kb_search` over JSON-RPC; `kb_read` was dropped in SEC-2d — see below).
`/mcp` is `UNTESTED` with a reason when
the server does not answer the path (`KB_MCP=off`); `--strict` makes UNTESTED a
failure. The `/mcp` row is also part of the normal, non-stale matrix now, so
API-4's "no leak row" gap is closed:

```bash
KB_MCP=on kb-server serve &
kb-server eval leak --base-url http://127.0.0.1:3000 \
  --api-key "$KB_EVAL_API_KEY" --peer-key "$KB_EVAL_PEER_API_KEY" \
  --out docs/eval-ab/scope-scale/out/leak-normal --strict
```

#### SEC-2d: the MCP row and a clean tenant

`mcp_probe` used to send only `{query, maxChunks}` to `kb_search`, and `finish`
judged that answer against the whole tenant. In a tenant holding other people's
documents that flagged legitimately visible documents as leaks, and the search
filters were never exercised through JSON-RPC. `mcp_probe` now builds its
arguments with the same `search_body(scope, query, max_chunks)` as the HTTP
search probe, and the result is judged against `scope.allowed`, exactly like the
`Search` row. The `kb_read` follow-up was dropped: it is a document-addressed
read with a tenant-wide allowed set, which made a single answer judge two
different things; the `content` row already covers document reads. The MCP tool
schema now names the filter arguments it accepts.

The harness does **not** create a tenant. `seed` discovers the tenant from the
knowledge base it creates with the supplied key, so the live 2026-11-25 run
pointed `--api-key` at `v1m-disclosure`, which also held the EVAL-9 HR corpus.
The run now refuses a tenant whose key lists more documents than the run seeded
(and warns loudly if the list route cannot be read). Create two throwaway
tenants and use only those keys:

```bash
# Against the throwaway stack, with DATABASE_URL set for kb-server:
kb-server create-api-key sec2d-leak-a leak-a
kb-server create-api-key sec2d-leak-b leak-b
# Copy the printed plaintext keys into these variables:
KB_EVAL_API_KEY=... KB_EVAL_PEER_API_KEY=... kb-server eval leak \
  --base-url http://127.0.0.1:3000 \
  --api-key "$KB_EVAL_API_KEY" --peer-key "$KB_EVAL_PEER_API_KEY" \
  --stale-payload --allow-db-writes --database-url "$DATABASE_URL" \
  --out docs/eval-ab/scope-scale/out/leak-stale --strict
```

Outputs: `leak.json` (matrix, plan, notes, evidence) and `leak-report.md`.
