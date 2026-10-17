# Cross-scope leak matrix

- base url: `http://127.0.0.1:18080`
- server version: `dev`
- harness commit: `a6664c5-dirty`
- timestamp: 2026-10-17T23:17:49.795586829+00:00
- seeded: 8 documents across 2 tenants, 12 scopes, 288 probes
- result: **no leak found, some arms UNTESTED** — 0 leak(s) over 288 probes

- arm attribution: 240 answer(s) named their own arms (`retrieval.arms`), 0 had none and were attributed by the chunk-shape heuristic

## Matrix (arm × scope type)

Each cell is `probes / leaks / positive-control hits` and its status.

| arm | tenant | knowledge_base | category | document_ids | arm status |
|---|---|---|---|---|---|
| vector | 8 / 0 / 2-2 **PASS** | 16 / 0 / 4-4 **PASS** | 16 / 0 / 4-4 **PASS** | 8 / 0 / 2-2 **PASS** | **PASS** |
| bm25 | 8 / 0 / 2-2 **PASS** | 16 / 0 / 4-4 **PASS** | 16 / 0 / 4-4 **PASS** | 8 / 0 / 0-2 **UNTESTED** | **PARTIAL** |
| neighbour | 8 / 0 / 2-2 **PASS** | 16 / 0 / 4-4 **PASS** | 16 / 0 / 4-4 **PASS** | 8 / 0 / 2-2 **PASS** | **PASS** |
| figure | 8 / 0 / 0-2 **UNTESTED** | 16 / 0 / 0-4 **UNTESTED** | 16 / 0 / 0-4 **UNTESTED** | 8 / 0 / 0-2 **UNTESTED** | **UNTESTED** |
| graph | 8 / 0 / 0-2 **UNTESTED** | 16 / 0 / 0-4 **UNTESTED** | 16 / 0 / 0-4 **UNTESTED** | 8 / 0 / 0-2 **UNTESTED** | **UNTESTED** |
| entity | 8 / 0 / 2-2 **PASS** | 16 / 0 / 4-4 **PASS** | 16 / 0 / 4-4 **PASS** | 8 / 0 / 2-2 **PASS** | **PASS** |

## Per-arm detail

### vector

- coverage: chunks whose retrieval.arms include `dense`, from POST /v1/search
- probes: 48, leaks: 0, refused with 4xx: 0, http errors: 0, positive control: 12/12
- status: **PASS**
- `tenant`: 8 probes, 0 leaks, 0 refused, positive 2/2 — **PASS**
- `knowledge_base`: 16 probes, 0 leaks, 0 refused, positive 4/4 — **PASS**
- `category`: 16 probes, 0 leaks, 0 refused, positive 4/4 — **PASS**
- `document_ids`: 8 probes, 0 leaks, 0 refused, positive 2/2 — **PASS**

### bm25

- coverage: chunks whose retrieval.arms include `sparse`, from POST /v1/search
- probes: 48, leaks: 0, refused with 4xx: 0, http errors: 0, positive control: 10/12
- status: **PARTIAL**
- why: scopes untested: document_ids
- `tenant`: 8 probes, 0 leaks, 0 refused, positive 2/2 — **PASS**
- `knowledge_base`: 16 probes, 0 leaks, 0 refused, positive 4/4 — **PASS**
- `category`: 16 probes, 0 leaks, 0 refused, positive 4/4 — **PASS**
- `document_ids`: 8 probes, 0 leaks, 0 refused, positive 0/2 — **UNTESTED** (a `document_ids`-scoped search runs no keyword arm in either runtime, so this cell's positive control is unreachable by construction: it measures the route, not the arm)

### neighbour

- coverage: every chunk of a search answer: a woven neighbour is one thing the answer adds that no arm ranked, so this cell stays the widest containment check rather than the narrow one it could be
- probes: 48, leaks: 0, refused with 4xx: 0, http errors: 0, positive control: 12/12
- status: **PASS**
- `tenant`: 8 probes, 0 leaks, 0 refused, positive 2/2 — **PASS**
- `knowledge_base`: 16 probes, 0 leaks, 0 refused, positive 4/4 — **PASS**
- `category`: 16 probes, 0 leaks, 0 refused, positive 4/4 — **PASS**
- `document_ids`: 8 probes, 0 leaks, 0 refused, positive 2/2 — **PASS**

### figure

- coverage: sources and chunks carrying an assetKey or a `figure` arm, plus the asset route when a seeded document has a figure
- probes: 48, leaks: 0, refused with 4xx: 0, http errors: 0, positive control: 0/12
- status: **UNTESTED**
- why: positive control missed 12/12 times: this probe cannot find its own target, so 'no leaks' proves nothing
- `tenant`: 8 probes, 0 leaks, 0 refused, positive 0/2 — **UNTESTED** (positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing)
- `knowledge_base`: 16 probes, 0 leaks, 0 refused, positive 0/4 — **UNTESTED** (positive control missed 4/4 times: this probe cannot find its own target, so 'no leaks' proves nothing)
- `category`: 16 probes, 0 leaks, 0 refused, positive 0/4 — **UNTESTED** (positive control missed 4/4 times: this probe cannot find its own target, so 'no leaks' proves nothing)
- `document_ids`: 8 probes, 0 leaks, 0 refused, positive 0/2 — **UNTESTED** (positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing)

### graph

- coverage: chunks whose retrieval.arms include `graph`, from POST /v1/search: the chunks a walk over the caller's scoped entity graph ranked, fetched by id
- probes: 48, leaks: 0, refused with 4xx: 0, http errors: 0, positive control: 0/12
- status: **UNTESTED**
- why: positive control missed 12/12 times: this probe cannot find its own target, so 'no leaks' proves nothing
- `tenant`: 8 probes, 0 leaks, 0 refused, positive 0/2 — **UNTESTED** (positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing)
- `knowledge_base`: 16 probes, 0 leaks, 0 refused, positive 0/4 — **UNTESTED** (positive control missed 4/4 times: this probe cannot find its own target, so 'no leaks' proves nothing)
- `category`: 16 probes, 0 leaks, 0 refused, positive 0/4 — **UNTESTED** (positive control missed 4/4 times: this probe cannot find its own target, so 'no leaks' proves nothing)
- `document_ids`: 8 probes, 0 leaks, 0 refused, positive 0/2 — **UNTESTED** (positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing)

### entity

- coverage: GET /v1/documents/:id/intelligence for a document id
- probes: 48, leaks: 0, refused with 4xx: 25, http errors: 0, positive control: 12/12
- status: **PASS**
- `tenant`: 8 probes, 0 leaks, 6 refused, positive 2/2 — **PASS**
- `knowledge_base`: 16 probes, 0 leaks, 8 refused, positive 4/4 — **PASS**
- `category`: 16 probes, 0 leaks, 8 refused, positive 4/4 — **PASS**
- `document_ids`: 8 probes, 0 leaks, 3 refused, positive 2/2 — **PASS**

## Degradations reported by the server

None: every probe was answered without a `degrades` entry.

## What this run does not prove

- `bm25` × `document_ids`: a `document_ids`-scoped search runs no keyword arm in either runtime, so this cell's positive control is unreachable by construction: it measures the route, not the arm.
- `figure` × `tenant`: positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- `figure` × `knowledge_base`: positive control missed 4/4 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- `figure` × `category`: positive control missed 4/4 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- `figure` × `document_ids`: positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- `graph` × `tenant`: positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- `graph` × `knowledge_base`: positive control missed 4/4 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- `graph` × `category`: positive control missed 4/4 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- `graph` × `document_ids`: positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- no seeded document produced a figure: figures only come from a layout extractor (kb-extract: the MinerU sidecar, mineru-api or Mistral), which this stack does not run. The figure arm therefore has nothing to retrieve and is reported UNTESTED rather than passing; the asset route is probed only once a figure exists.

A cell marked UNTESTED found no leak because nothing was retrieved, not because retrieval was contained. It is a hole in the evidence, not a pass.
