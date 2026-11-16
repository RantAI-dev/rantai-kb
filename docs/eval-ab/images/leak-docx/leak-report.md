# Cross-scope leak matrix

- base url: `http://127.0.0.1:28080`
- server version: `dev`
- harness commit: `f45b0155`
- timestamp: 2026-11-13T15:10:51.490863158+00:00
- seeded: 8 documents across 2 tenants, 14 scopes, 504 probes
- result: **no leak found, some arms UNTESTED** — 0 leak(s) over 504 probes

- arm attribution: 446 answer(s) named their own arms (`retrieval.arms`), 2 had none and were attributed by the chunk-shape heuristic (this server predates step S.1c, or the probes returned no chunks)

## Matrix (arm × scope type)

Each cell is `probes / leaks / positive-control hits` and its status.

| arm | tenant | knowledge_base | category | document_ids | external_ref | arm status |
|---|---|---|---|---|---|---|
| vector | 8 / 0 / 2-2 **PASS** | 16 / 0 / 4-4 **PASS** | 16 / 0 / 4-4 **PASS** | 8 / 0 / 2-2 **PASS** | 8 / 0 / 2-2 **PASS** | **PASS** |
| bm25 | 8 / 0 / 2-2 **PASS** | 16 / 0 / 4-4 **PASS** | 16 / 0 / 4-4 **PASS** | 8 / 0 / 0-2 **UNTESTED** | 8 / 0 / 2-2 **PASS** | **PARTIAL** |
| neighbour | 8 / 0 / 2-2 **PASS** | 16 / 0 / 4-4 **PASS** | 16 / 0 / 4-4 **PASS** | 8 / 0 / 2-2 **PASS** | 8 / 0 / 2-2 **PASS** | **PASS** |
| figure | 8 / 0 / 2-2 **PASS** | 16 / 0 / 4-4 **PASS** | 16 / 0 / 4-4 **PASS** | 8 / 0 / 2-2 **PASS** | 8 / 0 / 2-2 **PASS** | **PASS** |
| graph | 8 / 0 / 0-2 **UNTESTED** | 16 / 0 / 0-4 **UNTESTED** | 16 / 0 / 0-4 **UNTESTED** | 8 / 0 / 0-2 **UNTESTED** | 8 / 0 / 0-2 **UNTESTED** | **UNTESTED** |
| page | 8 / 0 / 0-2 **UNTESTED** | 16 / 0 / 0-4 **UNTESTED** | 16 / 0 / 0-4 **UNTESTED** | 8 / 0 / 0-2 **UNTESTED** | 8 / 0 / 0-2 **UNTESTED** | **UNTESTED** |
| tree | 8 / 0 / 0-2 **UNTESTED** | 16 / 0 / 0-4 **UNTESTED** | 16 / 0 / 0-4 **UNTESTED** | 8 / 0 / 0-2 **UNTESTED** | 8 / 0 / 0-2 **UNTESTED** | **UNTESTED** |
| global | 8 / 0 / 0-2 **UNTESTED** | 16 / 0 / 0-4 **UNTESTED** | 16 / 0 / 0-4 **UNTESTED** | 8 / 0 / 0-2 **UNTESTED** | 8 / 0 / 0-2 **UNTESTED** | **UNTESTED** |
| entity | 8 / 0 / 2-2 **PASS** | 16 / 0 / 4-4 **PASS** | 16 / 0 / 4-4 **PASS** | 8 / 0 / 2-2 **PASS** | 8 / 0 / 2-2 **PASS** | **PASS** |

## Per-arm detail

### vector

- coverage: chunks whose retrieval.arms include `dense`, from POST /v1/search
- probes: 56, leaks: 0, refused with 4xx: 0, http errors: 0, positive control: 14/14
- status: **PASS**
- `tenant`: 8 probes, 0 leaks, 0 refused, positive 2/2 — **PASS**
- `knowledge_base`: 16 probes, 0 leaks, 0 refused, positive 4/4 — **PASS**
- `category`: 16 probes, 0 leaks, 0 refused, positive 4/4 — **PASS**
- `document_ids`: 8 probes, 0 leaks, 0 refused, positive 2/2 — **PASS**
- `external_ref`: 8 probes, 0 leaks, 0 refused, positive 2/2 — **PASS**

### bm25

- coverage: chunks whose retrieval.arms include `sparse`, from POST /v1/search
- probes: 56, leaks: 0, refused with 4xx: 0, http errors: 0, positive control: 12/14
- status: **PARTIAL**
- why: scopes untested: document_ids
- `tenant`: 8 probes, 0 leaks, 0 refused, positive 2/2 — **PASS**
- `knowledge_base`: 16 probes, 0 leaks, 0 refused, positive 4/4 — **PASS**
- `category`: 16 probes, 0 leaks, 0 refused, positive 4/4 — **PASS**
- `document_ids`: 8 probes, 0 leaks, 0 refused, positive 0/2 — **UNTESTED** (a `document_ids`-scoped search runs no keyword arm in either runtime, so this cell's positive control is unreachable by construction: it measures the route, not the arm)
- `external_ref`: 8 probes, 0 leaks, 0 refused, positive 2/2 — **PASS**

### neighbour

- coverage: every chunk of a search answer: a woven neighbour is one thing the answer adds that no arm ranked, so this cell stays the widest containment check rather than the narrow one it could be
- probes: 56, leaks: 0, refused with 4xx: 0, http errors: 0, positive control: 14/14
- status: **PASS**
- `tenant`: 8 probes, 0 leaks, 0 refused, positive 2/2 — **PASS**
- `knowledge_base`: 16 probes, 0 leaks, 0 refused, positive 4/4 — **PASS**
- `category`: 16 probes, 0 leaks, 0 refused, positive 4/4 — **PASS**
- `document_ids`: 8 probes, 0 leaks, 0 refused, positive 2/2 — **PASS**
- `external_ref`: 8 probes, 0 leaks, 0 refused, positive 2/2 — **PASS**

### figure

- coverage: sources and chunks carrying an assetKey or a `figure` arm, plus the asset route when a seeded document has a figure
- probes: 56, leaks: 0, refused with 4xx: 0, http errors: 0, positive control: 14/14
- status: **PASS**
- `tenant`: 8 probes, 0 leaks, 0 refused, positive 2/2 — **PASS**
- `knowledge_base`: 16 probes, 0 leaks, 0 refused, positive 4/4 — **PASS**
- `category`: 16 probes, 0 leaks, 0 refused, positive 4/4 — **PASS**
- `document_ids`: 8 probes, 0 leaks, 0 refused, positive 2/2 — **PASS**
- `external_ref`: 8 probes, 0 leaks, 0 refused, positive 2/2 — **PASS**

### graph

- coverage: chunks whose retrieval.arms include `graph`, from POST /v1/search: the chunks a walk over the caller's scoped entity graph ranked, fetched by id
- probes: 56, leaks: 0, refused with 4xx: 0, http errors: 0, positive control: 0/14
- status: **UNTESTED**
- why: positive control missed 14/14 times: this probe cannot find its own target, so 'no leaks' proves nothing
- `tenant`: 8 probes, 0 leaks, 0 refused, positive 0/2 — **UNTESTED** (positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing)
- `knowledge_base`: 16 probes, 0 leaks, 0 refused, positive 0/4 — **UNTESTED** (positive control missed 4/4 times: this probe cannot find its own target, so 'no leaks' proves nothing)
- `category`: 16 probes, 0 leaks, 0 refused, positive 0/4 — **UNTESTED** (positive control missed 4/4 times: this probe cannot find its own target, so 'no leaks' proves nothing)
- `document_ids`: 8 probes, 0 leaks, 0 refused, positive 0/2 — **UNTESTED** (positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing)
- `external_ref`: 8 probes, 0 leaks, 0 refused, positive 0/2 — **UNTESTED** (positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing)

### page

- coverage: chunks whose retrieval.arms include `page`, from POST /v1/search: chunks on the PDF pages the page-image arm ranked inside the caller's scope, fetched by id. Only PDFs have pages, so the row needs a PDF corpus (`eval leak --pdf`)
- probes: 56, leaks: 0, refused with 4xx: 0, http errors: 0, positive control: 0/14
- status: **UNTESTED**
- why: positive control missed 14/14 times: this probe cannot find its own target, so 'no leaks' proves nothing
- `tenant`: 8 probes, 0 leaks, 0 refused, positive 0/2 — **UNTESTED** (positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing)
- `knowledge_base`: 16 probes, 0 leaks, 0 refused, positive 0/4 — **UNTESTED** (positive control missed 4/4 times: this probe cannot find its own target, so 'no leaks' proves nothing)
- `category`: 16 probes, 0 leaks, 0 refused, positive 0/4 — **UNTESTED** (positive control missed 4/4 times: this probe cannot find its own target, so 'no leaks' proves nothing)
- `document_ids`: 8 probes, 0 leaks, 0 refused, positive 0/2 — **UNTESTED** (a `document_ids`-scoped search is answered by one dense search and never reaches the page-image arm, so this cell's positive control is unreachable by construction: it measures the route, not the arm)
- `external_ref`: 8 probes, 0 leaks, 0 refused, positive 0/2 — **UNTESTED** (positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing)

### tree

- coverage: chunks whose retrieval.arms include `tree`, from POST /v1/search: chunks on the section pages the outline scored inside the caller's scope, fetched by (document, page)
- probes: 56, leaks: 0, refused with 4xx: 0, http errors: 0, positive control: 0/14
- status: **UNTESTED**
- why: positive control missed 14/14 times: this probe cannot find its own target, so 'no leaks' proves nothing
- `tenant`: 8 probes, 0 leaks, 0 refused, positive 0/2 — **UNTESTED** (positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing)
- `knowledge_base`: 16 probes, 0 leaks, 0 refused, positive 0/4 — **UNTESTED** (positive control missed 4/4 times: this probe cannot find its own target, so 'no leaks' proves nothing)
- `category`: 16 probes, 0 leaks, 0 refused, positive 0/4 — **UNTESTED** (positive control missed 4/4 times: this probe cannot find its own target, so 'no leaks' proves nothing)
- `document_ids`: 8 probes, 0 leaks, 0 refused, positive 0/2 — **UNTESTED** (a `document_ids`-scoped search is answered by one dense search and never reaches the tree arm, so this cell's positive control is unreachable by construction: it measures the route, not the arm)
- `external_ref`: 8 probes, 0 leaks, 0 refused, positive 0/2 — **UNTESTED** (positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing)

### global

- coverage: chunks whose retrieval.arms include `global`, from POST /v1/search: the member chunks of the communities the summary matched inside the caller's scope, fetched by id. The summary text itself is never returned, so it cannot be a canary
- probes: 56, leaks: 0, refused with 4xx: 0, http errors: 0, positive control: 0/14
- status: **UNTESTED**
- why: positive control missed 14/14 times: this probe cannot find its own target, so 'no leaks' proves nothing
- `tenant`: 8 probes, 0 leaks, 0 refused, positive 0/2 — **UNTESTED** (positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing)
- `knowledge_base`: 16 probes, 0 leaks, 0 refused, positive 0/4 — **UNTESTED** (positive control missed 4/4 times: this probe cannot find its own target, so 'no leaks' proves nothing)
- `category`: 16 probes, 0 leaks, 0 refused, positive 0/4 — **UNTESTED** (positive control missed 4/4 times: this probe cannot find its own target, so 'no leaks' proves nothing)
- `document_ids`: 8 probes, 0 leaks, 0 refused, positive 0/2 — **UNTESTED** (a `document_ids`-scoped search is answered by one dense search and never reaches the global (community) arm, so this cell's positive control is unreachable by construction: it measures the route, not the arm)
- `external_ref`: 8 probes, 0 leaks, 0 refused, positive 0/2 — **UNTESTED** (positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing)

### entity

- coverage: GET /v1/documents/:id/intelligence for a document id
- probes: 56, leaks: 0, refused with 4xx: 28, http errors: 0, positive control: 14/14
- status: **PASS**
- `tenant`: 8 probes, 0 leaks, 6 refused, positive 2/2 — **PASS**
- `knowledge_base`: 16 probes, 0 leaks, 8 refused, positive 4/4 — **PASS**
- `category`: 16 probes, 0 leaks, 8 refused, positive 4/4 — **PASS**
- `document_ids`: 8 probes, 0 leaks, 3 refused, positive 2/2 — **PASS**
- `external_ref`: 8 probes, 0 leaks, 3 refused, positive 2/2 — **PASS**

## Degradations reported by the server

None: every probe was answered without a `degrades` entry.

## What this run does not prove

- `bm25` × `document_ids`: a `document_ids`-scoped search runs no keyword arm in either runtime, so this cell's positive control is unreachable by construction: it measures the route, not the arm.
- `graph` × `tenant`: positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- `graph` × `knowledge_base`: positive control missed 4/4 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- `graph` × `category`: positive control missed 4/4 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- `graph` × `document_ids`: positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- `graph` × `external_ref`: positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- `page` × `tenant`: positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- `page` × `knowledge_base`: positive control missed 4/4 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- `page` × `category`: positive control missed 4/4 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- `page` × `document_ids`: a `document_ids`-scoped search is answered by one dense search and never reaches the page-image arm, so this cell's positive control is unreachable by construction: it measures the route, not the arm.
- `page` × `external_ref`: positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- `tree` × `tenant`: positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- `tree` × `knowledge_base`: positive control missed 4/4 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- `tree` × `category`: positive control missed 4/4 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- `tree` × `document_ids`: a `document_ids`-scoped search is answered by one dense search and never reaches the tree arm, so this cell's positive control is unreachable by construction: it measures the route, not the arm.
- `tree` × `external_ref`: positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- `global` × `tenant`: positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- `global` × `knowledge_base`: positive control missed 4/4 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- `global` × `category`: positive control missed 4/4 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- `global` × `document_ids`: a `document_ids`-scoped search is answered by one dense search and never reaches the global (community) arm, so this cell's positive control is unreachable by construction: it measures the route, not the arm.
- `global` × `external_ref`: positive control missed 2/2 times: this probe cannot find its own target, so 'no leaks' proves nothing.
- 2 of 448 probe answers carried no `retrieval.arms`, so their arms were read off the chunk shape (`chunkType` present or not). That heuristic is weaker evidence than the server naming the arm, and it is exactly what made the keyword arm UNTESTED before step S.1c.
- no seeded document produced a figure. Pass --docx: each seeded .docx holds a picture whose alt text is the figure canary, which the visual stage (ING-5, KB_IMAGES) indexes as a figure chunk with no model. Without it the figure arm has nothing to retrieve and is reported UNTESTED rather than passing; the asset route is probed only once a figure exists.
- the corpus was seeded as markdown, and the page-image arm renders only PDFs: its row has no page to find and is UNTESTED whether or not the server runs the arm. Pass --pdf to seed PDFs.

A cell marked UNTESTED found no leak because nothing was retrieved, not because retrieval was contained. It is a hole in the evidence, not a pass.
