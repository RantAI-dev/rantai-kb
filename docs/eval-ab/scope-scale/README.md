# Scope scale and stale payloads (SEC-2c)

> **Both instruments write straight into Postgres and Qdrant** (synthetic tenants; stale
> moves, soft-deletes and ACLs). Run them only against a throwaway stack. Each refuses to run
> without `--allow-db-writes`.

Two instruments prove the two halves of SEC-2 on a live stack:

1. `kb-server eval scope-bench` — does predicate scope keep search latency flat
   as a tenant grows from 1k to 100k documents?
2. `kb-server eval leak --stale-payload` — when Qdrant disagrees with Postgres,
   does *any* retrieval arm or document-addressed route leak?

**Live results were run 2026-11-25 on GB10; see ["Results (2026-11-25, GB10)"](#results-2026-11-25-gb10)
at the end of this page.** Everything above that section is the method; the
numbers below it are measurements from a fresh run, not claims.

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

## Results (2026-11-25, GB10)

Setup: branch `bench/v1-measure` @ `231a25bf` (`origin/main`), fresh release
kb-server in `~/kb-target-v1m` run natively against our own throwaway
`stack-up.sh` stack (`KB_STACK_NAME=v1mhr`, kb-server `http://127.0.0.1:28091`,
Postgres 25443, Qdrant 26345/26346). Nemotron at `127.0.0.1:8210` served
embeddings; qwen3-rerank-0.6B at `127.0.0.1:8122` served reranking. The stack was
**only** the one started for this task; every `DATABASE_URL` used came from its
`stack.env`. Both tools were passed `--allow-db-writes` as required.

### 1. Scope bench — measured

Wrapper `~/v1m/run-m2-scope.sh`; stdout `~/v1m/logs/m2-scope-final.out`;
raw `~/v1m/results/scope-scale/{results.json,report.md}`. Two passes shared the
same `--out`; the server was restarted between them with
`KB_SCOPE_ID_LIMIT=0` (ids) then `KB_SCOPE_ID_LIMIT=2000` (predicate).

| size | mode | p50 ms | p95 ms | mean scopeVerifyDropped | queries | errors |
|---:|---|---:|---:|---:|---:|---:|
| 1000 | ids | 110.66 | 257.99 | 0.00 | 200 | 0 |
| 10000 | ids | 150.79 | 795.11 | 0.00 | 200 | 0 |
| 100000 | ids | 938.17 | 5172.76 | 0.00 | 200 | 0 |
| 1000 | predicate | 190.97 | 295.85 | 0.00 | 200 | 0 |
| 10000 | predicate | 118.83 | 257.51 | 0.00 | 200 | 0 |
| 100000 | predicate | 627.94 | 3566.69 | 0.00 | 200 | 0 |

**Verdict: MISS (target missed).** Raw line:
`scope-bench: MISS — predicate p95 3566.69 ms at 100000 / 295.85 ms at 1000 = 12.06x (limit 1.20x)`.
All six rows had `mean_scope_verify_dropped = 0.00` and 0 errors. An earlier
aborted attempt (server restart race: port 28091 not released) produced 0.06 ms
"latencies" with no server; those numbers were discarded and the two passes were
re-run cleanly. The large p95 growth is measured on this box with reranking
enabled; it is not hidden.

### 2. Stale-payload leak matrix — measured

Wrappers `~/v1m/run-m3-leak.sh`; stdout `~/v1m/logs/m3-leak.out`; raw
`~/v1m/results/scope-scale/leak-normal/{leak.json,leak-report.md}` and
`~/v1m/results/scope-scale/leak-stale/{leak.json,leak-report.md}`. Server run with
`KB_MCP=on`; both runs used `--strict`; the stale run added
`--stale-payload --allow-db-writes --database-url "$DATABASE_URL"`.

Raw summary lines:

```text
leak matrix: 724 probes, 24 leak(s)
24 leak(s) across 724 probes — see .../leak-normal/leak-report.md
leak matrix: 54 probes, 1 leak(s)
1 leak(s) across 54 probes — see .../leak-stale/leak-report.md
```

> **Reviewer finding (2026-11-25): the 24 `mcp` leaks are false positives, not a scope
> failure.** The leak run used the tenant `v1m-disclosure`, which also holds the EVAL-9 HR
> corpus. `mcp_probe` sent only `{query, maxChunks}`, with no scope filters, and `finish`
> judged the MCP row against the harness's seed set. The one "foreign" document,
> `f2343726…`, is an HR document that the same key may legitimately see. Every HTTP arm in
> the same run passed, the stale-payload rows passed, and the in-process leak tests with
> `/mcp` mounted pass. SEC-2d makes the MCP probe send the HTTP probe's filters and judge
> against the scope, and the matrix is re-run in a clean tenant.

**Target 0 leaks: MISS.** Every leak is in the `mcp` arm; the foreign document
reported by the probes is `f2343726-85e8-48d3-a686-90e120c220ee`, an
HR-corpus document present in the same tenant. Every probe was answered without
a `degrades` entry.

Normal (non-stale) matrix — each cell is `probes / leaks / positive hits` with
status (from `leak-normal/leak-report.md`):

| arm | tenant | knowledge_base | category | document_ids | external_ref | arm status |
|---|---|---|---|---|---|---|
| vector | 8 / 0 / 2-2 **PASS** | 16 / 0 / 4-4 **PASS** | 16 / 0 / 4-4 **PASS** | 8 / 0 / 2-2 **PASS** | 8 / 0 / 2-2 **PASS** | **PASS** |
| bm25 | 8 / 0 / 2-2 **PASS** | 16 / 0 / 4-4 **PASS** | 16 / 0 / 4-4 **PASS** | 8 / 0 / 2-2 **PASS** | 8 / 0 / 2-2 **PASS** | **PASS** |
| neighbour | 8 / 0 / 2-2 **PASS** | 16 / 0 / 4-4 **PASS** | 16 / 0 / 4-4 **PASS** | 8 / 0 / 2-2 **PASS** | 8 / 0 / 2-2 **PASS** | **PASS** |
| figure | 8 / 0 / 0-2 **UNTESTED** | 16 / 0 / 0-4 **UNTESTED** | 16 / 0 / 0-4 **UNTESTED** | 8 / 0 / 0-2 **UNTESTED** | 8 / 0 / 0-2 **UNTESTED** | **UNTESTED** |
| graph | 8 / 0 / 0-2 **UNTESTED** | 16 / 0 / 0-4 **UNTESTED** | 16 / 0 / 0-4 **UNTESTED** | 8 / 0 / 0-2 **UNTESTED** | 8 / 0 / 0-2 **UNTESTED** | **UNTESTED** |
| page | 8 / 0 / 0-2 **UNTESTED** | 16 / 0 / 0-4 **UNTESTED** | 16 / 0 / 0-4 **UNTESTED** | 8 / 0 / 0-2 **UNTESTED** | 8 / 0 / 0-2 **UNTESTED** | **UNTESTED** |
| tree | 8 / 0 / 0-2 **UNTESTED** | 16 / 0 / 0-4 **UNTESTED** | 16 / 0 / 0-4 **UNTESTED** | 8 / 0 / 0-2 **UNTESTED** | 8 / 0 / 0-2 **UNTESTED** | **UNTESTED** |
| global | 8 / 0 / 0-2 **UNTESTED** | 16 / 0 / 0-4 **UNTESTED** | 16 / 0 / 0-4 **UNTESTED** | 8 / 0 / 0-2 **UNTESTED** | 8 / 0 / 0-2 **UNTESTED** | **UNTESTED** |
| table | 8 / 0 / 2-2 **PASS** | 16 / 0 / 4-4 **PASS** | 16 / 0 / 4-4 **PASS** | 6 / 0 / 0-0 **UNTESTED** | 6 / 0 / 0-0 **UNTESTED** | **PARTIAL** |
| entity | 8 / 0 / 2-2 **PASS** | 16 / 0 / 4-4 **PASS** | 16 / 0 / 4-4 **PASS** | 8 / 0 / 2-2 **PASS** | 8 / 0 / 2-2 **PASS** | **PASS** |
| content | 8 / 0 / 2-2 **PASS** | 16 / 0 / 4-4 **PASS** | 16 / 0 / 4-4 **PASS** | 8 / 0 / 2-2 **PASS** | 8 / 0 / 2-2 **PASS** | **PASS** |
| structure | 8 / 0 / 2-2 **PASS** | 16 / 0 / 4-4 **PASS** | 16 / 0 / 4-4 **PASS** | 8 / 0 / 2-2 **PASS** | 8 / 0 / 2-2 **PASS** | **PASS** |
| mcp | 8 / 4 / 2-2 **LEAK** | 16 / 7 / 4-4 **LEAK** | 16 / 7 / 4-4 **LEAK** | 8 / 3 / 2-2 **LEAK** | 8 / 3 / 2-2 **LEAK** | **LEAK** |

Stale-payload matrix — `probes / leaks / positive hits` (from
`leak-stale/leak-report.md`). Only `knowledge_base` scope type was generated in
this mode; all other cells are `n/a` (the run generated 54 probes over 14 scopes,
one scope type).

| arm | tenant | knowledge_base | category | document_ids | external_ref | arm status |
|---|---|---|---|---|---|---|
| vector | n/a | 4 / 0 / 1-1 **PASS** | n/a | n/a | n/a | **PASS** |
| bm25 | n/a | 4 / 0 / 1-1 **PASS** | n/a | n/a | n/a | **PASS** |
| neighbour | n/a | 4 / 0 / 1-1 **PASS** | n/a | n/a | n/a | **PASS** |
| figure | n/a | 4 / 0 / 0-1 **UNTESTED** | n/a | n/a | n/a | **UNTESTED** |
| graph | n/a | 4 / 0 / 0-1 **UNTESTED** | n/a | n/a | n/a | **UNTESTED** |
| page | n/a | 4 / 0 / 0-1 **UNTESTED** | n/a | n/a | n/a | **UNTESTED** |
| tree | n/a | 4 / 0 / 0-1 **UNTESTED** | n/a | n/a | n/a | **UNTESTED** |
| global | n/a | 4 / 0 / 0-1 **UNTESTED** | n/a | n/a | n/a | **UNTESTED** |
| table | n/a | 6 / 0 / 3-3 **PASS** | n/a | n/a | n/a | **PASS** |
| entity | n/a | 4 / 0 / 1-1 **PASS** | n/a | n/a | n/a | **PASS** |
| content | n/a | 4 / 0 / 1-1 **PASS** | n/a | n/a | n/a | **PASS** |
| structure | n/a | 4 / 0 / 1-1 **PASS** | n/a | n/a | n/a | **PASS** |
| mcp | n/a | 4 / 1 / 1-1 **LEAK** | n/a | n/a | n/a | **LEAK** |

Reasons for `UNTESTED` cells (verbatim from both reports): "positive control
missed … times: this probe cannot find its own target, so 'no leaks' proves
nothing"; figure — "no seeded document produced a figure. Pass `--docx` …";
page — "the corpus was seeded as markdown, and the page-image arm renders only
PDFs … Pass `--pdf` …". Normal `table` × `document_ids`/`external_ref`: "no
positive control was possible for this cell". Stale notes: "stale drift injected
in Postgres only: D=d95e031f-… moved 5649eff9-… -> d9f8e31d-…,
E=8a4b3515-… soft-deleted, F=60d21891-… ACL'd with user:42"; "no Qdrant payload
was changed and every ScopeOutbox row for D/E/F was deleted"; "F's tenant has an
ACL'd document, so predicate mode is disabled there: its probes exercise the
Ids path (KB_SCOPE_ID_LIMIT is ignored)". A cell marked UNTESTED found no leak
because nothing was retrieved, not because retrieval was contained.
