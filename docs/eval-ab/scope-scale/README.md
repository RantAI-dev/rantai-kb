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
SEC-2d (bounded scope probe, MCP probe with scope filters, per-stage bench timings) changed
the instruments **after** that run, so its numbers are the pre-SEC-2d baseline.

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

## SEC-2d SQL re-run (2026-11-25, GB10)

Scope-bench only (M1) after `ba8e0e35` (`perf(scope): semi-join for the KB
limit; indexed ACL tenant check`). Same method and setup as the run above,
reused exactly: branch `bench/sec2d-sql` @ `ba8e0e35`, release `kb-server`
built fresh with `CARGO_TARGET_DIR=$HOME/kb-target-s2q`, `CARGO_BUILD_JOBS=8`,
`cargo build --release --manifest-path rust/Cargo.toml -p kb-server`; Nemotron
at `127.0.0.1:8210` (omniembed `/v2/embed`, 2048-d) used, not started or
stopped; `qwen3-rerank-0.6b` started with
`~/llmhost/bakeoff/serve.sh up qwen3-rerank-0.6b` (served at
`127.0.0.1:8122`); `KB_MCP=on`; `--allow-db-writes` on both passes.
**Brand-new throwaway stack** `KB_STACK_NAME=s2q`,
`KB_STACK_DIR=$HOME/s2q/stack-s2q` (Postgres 25463, S3 29021, Qdrant
26365/26366, kb-server `http://127.0.0.1:28101`), no eval corpus ingested;
migrations applied from zero — `migrate: 10 applied, 0 already present` (the
new partial index + expression statistics included). `KB_SERVER_BIN` was set
to the fresh release binary and the serving process was verified, not assumed:

```text
$ ls -l /proc/2576312/exe
lrwxrwxrwx 1 rismana rismana 0 Nov 25 18:30 /proc/2576312/exe -> /home/rismana/kb-target-s2q/release/kb-server
```

Wrappers/logs: `~/s2q/run-m1-scope.sh`, `~/s2q/logs/m1-scope.out`,
`~/s2q/logs/m1-wrapper.out`, `~/s2q/logs/explain.out`; raw results:
`~/s2q/results/scope-scale/`. The server was restarted between passes with
`KB_SCOPE_ID_LIMIT=0` (ids) then `KB_SCOPE_ID_LIMIT=2000` (predicate). The
predicate pass used `--keep`, so the 100k tenant
`scope-bench-276fb7-6b06c808-100000` still existed for the EXPLAINs below.

### 1. Scope bench — measured (fresh release binary)

Raw lines from `~/s2q/logs/m1-scope.out`:

```text
scope-bench: 1000 n=200 p50 41.75 ms p95 61.37 ms dropped 0.00
scope-bench: 10000 n=200 p50 70.58 ms p95 204.56 ms dropped 0.00
scope-bench: 100000 n=200 p50 409.58 ms p95 2380.08 ms dropped 0.00
scope-bench: 1000 n=200 p50 38.12 ms p95 62.05 ms dropped 0.00
scope-bench: 10000 n=200 p50 48.08 ms p95 91.23 ms dropped 0.00
scope-bench: 100000 n=200 p50 324.43 ms p95 500.07 ms dropped 0.00
scope-bench: MISS — predicate p95 500.07 ms at 100000 / 62.05 ms at 1000 = 8.06x (limit 1.20x)
```

| size | mode | p50 ms | p95 ms | mean scopeVerifyDropped | queries | errors |
|---:|---|---:|---:|---:|---:|---:|
| 1000 | ids | 41.75 | 61.37 | 0.00 | 200 | 0 |
| 10000 | ids | 70.58 | 204.56 | 0.00 | 200 | 0 |
| 100000 | ids | 409.58 | 2380.08 | 0.00 | 200 | 0 |
| 1000 | predicate | 38.12 | 62.05 | 0.00 | 200 | 0 |
| 10000 | predicate | 48.08 | 91.23 | 0.00 | 200 | 0 |
| 100000 | predicate | 324.43 | 500.07 | 0.00 | 200 | 0 |

Per-stage p95 (ms), from `stage_timings_ms` in
`~/s2q/results/scope-scale/results.json`:

| stage | 1k ids | 1k predicate | 10k ids | 10k predicate | 100k ids | 100k predicate |
|---|---:|---:|---:|---:|---:|---:|
| scopeBuild | 7.537 | 5.961 | 14.139 | 17.751 | 349.967 | 47.822 |
| expand | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 | 0.001 |
| arms | 16.705 | 17.812 | 95.591 | 20.793 | 1901.846 | 279.339 |
| enrich | 0.002 | 0.002 | 0.001 | 0.002 | 0.004 | 0.007 |
| fusion | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 |
| rerank | 0.041 | 0.041 | 0.023 | 0.043 | 0.084 | 0.170 |
| pages | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 | 0.002 |
| neighbours | 0.000 | 0.000 | 0.000 | 0.000 | 0.001 | 0.001 |
| figures | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 | 0.001 |
| context | 0.001 | 0.001 | 0.001 | 0.001 | 0.004 | 0.011 |
| serialise | 0.010 | 0.012 | 0.009 | 0.010 | 0.033 | 0.036 |
| total | 58.866 | 58.298 | 201.089 | 89.110 | 2336.533 | 492.737 |

**Verdict: MISS** — `scope-bench: MISS — predicate p95 500.07 ms at 100000 /
62.05 ms at 1000 = 8.06x (limit 1.20x)`. The growth is still in `arms` in
predicate mode (17.812 → 20.793 → 279.339 ms p95), but `scopeBuild` no longer
blows up: 5.961 → 17.751 → 47.822 ms p95, down from 18.05 → 146.69 → 2417.68
ms in the previous run. Predicate p95 growth improved from 49.66× to 8.06×
(pre-SEC-2d baseline 12.06×). All six rows had `mean_scope_verify_dropped =
0.00` and 0 errors.

### 2. EXPLAIN plans on the s2q Postgres (100k tenant still present)

`EXPLAIN (ANALYZE, TIMING OFF, SUMMARY ON)` against
`scope-bench-276fb7-6b06c808-100000`, with the binds substituted. Verbatim
from `~/s2q/logs/explain.out`. The tenant-wide scope query is
`scope_sql_inner` with the bench key's empty scope/KB/principal sets; the ACL
tenant check is `SELECT_TENANT_HAS_ACL`. An extra KB-limited variant is
included because that is where the old `JOIN`/`DISTINCT` was replaced by
`EXISTS`.

Tenant-wide scope query (`scope_sql_inner`):

```text
                                                                                                      QUERY PLAN                                                                                                      
----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------
 Seq Scan on "Document" d  (cost=0.00..6033.00 rows=100007 width=34) (actual rows=100000 loops=1)
   Filter: (("deletedAt" IS NULL) AND ("isLatest" OR ("docKey" IS NULL)) AND ("tenantId" = 'scope-bench-276fb7-6b06c808-100000'::text) AND ((cardinality("aclPrincipals") = 0) OR ("aclPrincipals" && '{}'::text[])))
   Rows Removed by Filter: 11000
 Planning Time: 0.474 ms
 Execution Time: 15.607 ms
(5 rows)
```

Tenant-wide scope query with the KB limit as `EXISTS` (`scope_sql_inner`,
tenant + bench KB `bench-kb-276fb7-6b06c808-100000`):

```text
                                                                                                         QUERY PLAN                                                                                                         
----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------
 Hash Semi Join  (cost=5191.09..14832.60 rows=89977 width=34) (actual rows=100000 loops=1)
   Hash Cond: (d.id = dg."documentId")
   ->  Seq Scan on "Document" d  (cost=0.00..6033.00 rows=100007 width=34) (actual rows=100000 loops=1)
         Filter: (("deletedAt" IS NULL) AND ("isLatest" OR ("docKey" IS NULL)) AND ("tenantId" = 'scope-bench-276fb7-6b06c808-100000'::text) AND ((cardinality("aclPrincipals") = 0) OR ("aclPrincipals" && '{}'::text[])))
         Rows Removed by Filter: 11000
   ->  Hash  (cost=3161.75..3161.75 rows=99867 width=34) (actual rows=100000 loops=1)
         Buckets: 131072  Batches: 2  Memory Usage: 4273kB
         ->  Seq Scan on "DocumentGroup" dg  (cost=0.00..3161.75 rows=99867 width=34) (actual rows=100000 loops=1)
               Filter: ("knowledgeBaseId" = ANY ('{bench-kb-276fb7-6b06c808-100000}'::text[]))
               Rows Removed by Filter: 11000
 Planning Time: 0.404 ms
 Execution Time: 56.132 ms
(12 rows)
```

ACL tenant check (`SELECT_TENANT_HAS_ACL`) — the new partial index is used as
an index-only scan:

```text
                                                              QUERY PLAN                                                               
---------------------------------------------------------------------------------------------------------------------------------------
 Result  (cost=7.28..7.29 rows=1 width=1) (actual rows=1 loops=1)
   InitPlan 1 (returns $0)
     ->  Index Only Scan using "Document_tenantId_has_acl_idx" on "Document"  (cost=0.12..7.28 rows=1 width=0) (actual rows=0 loops=1)
           Index Cond: ("tenantId" = 'scope-bench-276fb7-6b06c808-100000'::text)
           Heap Fetches: 0
 Planning Time: 0.101 ms
 Execution Time: 0.012 ms
(7 rows)
```
