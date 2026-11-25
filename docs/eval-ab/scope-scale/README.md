# Scope scale and stale payloads (SEC-2c)

Two instruments prove the two halves of SEC-2 on a live stack:

1. `kb-server eval scope-bench` — does predicate scope keep search latency flat
   as a tenant grows from 1k to 100k documents?
2. `kb-server eval leak --stale-payload` — when Qdrant disagrees with Postgres,
   does *any* retrieval arm or document-addressed route leak?

**Results: not run yet.** The instruments are built and unit-tested offline;
the numbers below are the method, not measurements. Never report a p50/p95 or
a leak count from this page without a fresh live run.

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
  --mode-label ids
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
  --mode-label predicate
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
  --stale-payload --database-url "$DATABASE_URL" \
  --out docs/eval-ab/scope-scale/out/leak-stale \
  --strict
```

A leak is **any** appearance of D under the KB-1-limited key, or of E or F
under a principal-less key. Probes cover every arm in the normal matrix
(vector, BM25, neighbour, figure, graph, page, tree, global, table, entity) plus
`GET /v1/documents/:id/content`, `/structure`, the table routes and `/mcp`
(`kb_search`, `kb_read` over JSON-RPC). `/mcp` is `UNTESTED` with a reason when
the server does not answer the path (`KB_MCP=off`); `--strict` makes UNTESTED a
failure. The `/mcp` row is also part of the normal, non-stale matrix now, so
API-4's "no leak row" gap is closed:

```bash
KB_MCP=on kb-server serve &
kb-server eval leak --base-url http://127.0.0.1:3000 \
  --api-key "$KB_EVAL_API_KEY" --peer-key "$KB_EVAL_PEER_API_KEY" \
  --out docs/eval-ab/scope-scale/out/leak-normal --strict
```

Outputs: `leak.json` (matrix, plan, notes, evidence) and `leak-report.md`.
