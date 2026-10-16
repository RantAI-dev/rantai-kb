# Root Cause Analysis: BM25 Retrieval Collapse on Natural Language Queries

## Executive Summary

On the MIRACL Indonesian benchmark, rantai-kb scored **nDCG@10 0.003** with the stub embedder, while a 30-line independent BM25 (`rank_bm25`, `\w+` lowercase tokens) on the same corpus and queries scored **nDCG@10 0.708**.

### The Root Cause (in 3 lines)
1. **SurrealDB's `content @@ $q` enforces strict boolean AND semantics across all query tokens**: if any token in `$q` (such as question words "siapakah", "apakah", particles "yang", or punctuation "?") is absent from a document, the entire predicate evaluates to false and returns zero hits.
2. **`kb_en` analyzer applies English Snowball stemming (`snowball(english)`) to Indonesian text**: mangling morphological affixes (e.g. Indonesian verb "menulis" -> "menuli", proper noun "Candy" -> "candi" which collides with Indonesian temples "candi", and "nasional" -> "nasion").
3. **Retrieval pipeline receives 0 BM25 hits from SurrealDB**: hybrid RRF fusion falls back entirely to the stub embedder's deterministic byte-hash vector arm, populating the final top-10 with arbitrary, irrelevant documents.

---

## Hypothesis Investigation & Raw Evidence

### H1. SurrealDB `content @@ $q` requires ALL query terms to match (AND semantics)
**Verdict: CONFIRMED.**

SurrealDB's full-text search match operator `@@` is not an OR-based ranked scoring predicate; it acts as a boolean filter requiring every analyzed token in the query string to be present in the indexed document. In natural language search, documents rarely contain the interrogative words ("siapakah", "kapan", "apakah") or punctuation ("?") of the query.

#### Raw Evidence on Live SurrealDB

**Query 17: "Siapakah yang menulis Candy Candy?"** (Gold document: `338233_0`)
- Exact BM25 query adapter runs (`BM25_SEARCH_QUERY`):
  ```sql
  SELECT count() FROM document_chunk WHERE content @@ "Siapakah yang menulis Candy Candy?" GROUP ALL;
  ```
  Result:
  ```json
  [{"result":[{"count":0}],"status":"OK","time":"429.784µs"}]
  ```
  **0 matches.**

- Single distinctive term ("candy"):
  ```sql
  SELECT count() FROM document_chunk WHERE content @@ "candy" GROUP ALL;
  ```
  Result:
  ```json
  [{"result":[{"count":23}],"status":"OK","time":"911.16µs"}]
  ```
  **23 matches**, including gold document `338233_0` (`document_id: 902c88ea-4be7-4829-944b-1fab9cc5cf06`).

**Query 156: "apakah nama ibukota kuba?"** (Gold document: `16802_0`)
- Full question:
  ```sql
  SELECT id FROM document_chunk WHERE content @@ "apakah nama ibukota kuba?" LIMIT 5;
  ```
  Result: `[]` (**0 matches**).
- Single term ("kuba"):
  ```sql
  SELECT id, document_id FROM document_chunk WHERE content @@ "kuba" LIMIT 5;
  ```
  Result: **Matches all 3 gold documents** (`16802_0`, `16802_3`, `146299_1`).

**Query 167: "Kapan Badan Standardisasi Nasional didirikan?"** (Gold document: `66804_0`)
- Full question: `WHERE content @@ "Kapan Badan Standardisasi Nasional didirikan?"` -> `[]` (**0 matches**).
- Single term: `WHERE content @@ "standardisasi"` -> **Returns gold document `66804_0`**.

**Query 170: "siapakah rasul pertama menurut islam?"** (Gold document: `18748_3`)
- Full question: `WHERE content @@ "siapakah rasul pertama menurut islam?"` -> `[]` (**0 matches**).
- Single term: `WHERE content @@ "rasul"` -> **Returns gold document `18748_3`**.

**Query 611: "Apakah agama Kekaisaran Persia Akhaimenia?"** (Gold documents: `199748_0`, `199748_15`)
- Full question: `WHERE content @@ "Apakah agama Kekaisaran Persia Akhaimenia?"` -> `[]` (**0 matches**).
- Single term: `WHERE content @@ "akhaimenia"` -> **Returns gold documents `199748_0` and `199748_15` at rank 1 and 3**.

---

### H2. The analyzer `kb_en` mangles Indonesian terms
**Verdict: CONFIRMED.**

SurrealDB's schema defines:
```sql
DEFINE ANALYZER IF NOT EXISTS kb_en TOKENIZERS class FILTERS lowercase, snowball(english);
```
Running `search::analyze("kb_en", $q)` exposes the token distortions:

1. **Query 17**: `search::analyze("kb_en", "Siapakah yang menulis Candy Candy?");`
   - Tokens: `["siapakah", "yang", "menuli", "candi", "candi", "?"]`
   - "menulis" is stemmed by English rules removing trailing "-s" -> `"menuli"`.
   - "Candy" is stemmed by English rules changing "-y" to "-i" -> `"candi"`.
   - In Indonesian, "candi" means a stone temple/monument (e.g. Candi Borobudur, Candi Jabung). As a result, searching "Candy" matches Indonesian temple articles!
   - Punctuation `"?"` is retained as a token by `TOKENIZERS class`, which documents never contain.
2. **Query 167**: `search::analyze("kb_en", "Kapan Badan Standardisasi Nasional didirikan?");`
   - Tokens: `["kapan", "badan", "standardisasi", "nasion", "didirikan", "?"]`
   - "Nasional" is stemmed to `"nasion"` by English Snowball.

A language-neutral analyzer (`TOKENIZERS class FILTERS lowercase, ascii;`) produces:
`["siapakah", "yang", "menulis", "candy", "candy", "?"]` without affix mutilation.

However, **H2 is secondary to H1**: even with the neutral analyzer, as long as `content @@ $q` uses AND semantics, a question containing "siapakah" or "?" returns 0 hits if the target document lacks those words.

---

### H3. Something in the engine discards good BM25 hits
**Verdict: REFUTED.**

We traced `kb-engine` retrieval (`rust/crates/kb-engine/src/retrieval.rs`) end to end:
1. `bm25_future`: runs `chunk_store.bm25_search_global(query, pool_size)`. Because of H1, `raw_hits` is already empty (`[]`).
2. Scope verification: filters `raw_hits` against `scope.document_ids()` and checks alive status in Postgres. In single-tenant evals, all indexed documents belong to the scope and are alive.
3. Fusion (`RRF`):
   ```rust
   for (i, hit) in v_hits.into_iter().enumerate() {
       candidates.push(Candidate { hit, score: 1.0 / (60.0 + i + 1.0), insert_index: 2 * i });
   }
   for (j, hit) in b_hits.into_iter().enumerate() {
       candidates.push(Candidate { hit, score: 1.0 / (60.0 + j + 1.0), insert_index: 2 * j + 1 });
   }
   ```
   Vector hits and BM25 hits are interleaved. If BM25 returns hits, they appear at ranks 1, 3, 5, etc.
4. Truncation & neighbours: truncates to `max_chunks` (default 8, eval requests 50).
5. Conclusion: The engine does not discard BM25 hits; SurrealDB never returns them in the first place.

---

## Measured Hypothesis Impact Table (`miracl-id-50`)

Evaluated on `miracl-id-50` (50 queries, 1,246 corpus documents) with `kb-server eval` (`--gain linear`):

| Configuration | nDCG@10 | Recall@10 | MRR@10 | Hit@1 | Precision@10 | Notes |
|---|---|---|---|---|---|---|
| **Current / Baseline** (AND semantics, `kb_en`) | **0.000** | **0.000** | **0.000** | **0.000** | **0.000** | SurrealDB BM25 returns 0 hits; vector arm stub dominates with noise |
| **H2-fix only** (AND semantics, neutral analyzer) | **0.000** | **0.000** | **0.000** | **0.000** | **0.000** | AND semantics still blocks every question containing "siapakah", "?", etc. |
| **H1-fix only** (OR-of-terms query, `kb_en`) | **0.410** | **0.667** | **0.352** | **0.000** | **0.196** | Matches individual terms; massive recovery (+0.410 nDCG) |
| **Both H1 + H2** (OR-of-terms + neutral analyzer) | **0.414** | **0.686** | **0.342** | **0.000** | **0.200** | Slight further gain in Recall (+0.019) from uncorrupted terms |
| **Independent BM25** (`rank_bm25` + `ranx`) | **0.744** | **0.938** | **0.741** | **0.620** | **0.272** | True BM25Okapi scoring over full vocabulary |

*Note on Hit@1 = 0.000 in hybrid search with stub embedder*: In RRF fusion, candidate 0 (vector hit) is assigned `insert_index: 0` and candidate 0 (BM25 hit) is assigned `insert_index: 1`. Because both have initial score `1/61`, the vector candidate wins tie-breaking and takes rank 1. With the stub embedder, rank 1 is always pseudo-random noise, capping Hit@1 at 0 and MRR@10 at ~0.35.

---

## Bun / TypeScript Runtime Parity (AGENTS Rule 4)

We inspected `src/lib/rag/bm25-search.ts` and `scripts/apply-vector-schema.ts` (read-only):
```typescript
// src/lib/rag/bm25-search.ts:24-30
const sql = `
  SELECT id, document_id, content, search::score(0) AS score
  FROM document_chunk
  WHERE content @@ $q
  ORDER BY score DESC, id ASC
  LIMIT ${safeLimit};
`;
```
```typescript
// scripts/apply-vector-schema.ts:23-24
`DEFINE ANALYZER IF NOT EXISTS kb_en TOKENIZERS class FILTERS lowercase, snowball(english);`,
`DEFINE INDEX IF NOT EXISTS content_search_idx ON document_chunk FIELDS content SEARCH ANALYZER kb_en BM25(1.2, 0.75) HIGHLIGHTS;`,
```

**Conclusion:** Bun has the **identical behavior and bug**.
- Bun executes `content @@ $q` without query term splitting.
- Bun defines the identical `kb_en` analyzer with `snowball(english)`.
- This is NOT a Rust port regression; Rust faithfully mirrored Bun's broken SurrealDB BM25 query.

---

## Recommendation for the New Store (Qdrant, Plan Step L)

As rantai-kb migrates from SurrealDB to Qdrant + BM25/sparse store:
1. **Never use strict boolean AND query parsing for natural language full-text retrieval.**
2. Standard BM25 implementations (Lucene, BM25Okapi, Tantivy, Qdrant sparse vectors, SPLADE) naturally sum term frequencies across overlapping terms (`sum(IDF * TF_norm)`), providing proper soft-matching.
3. Use language-appropriate or language-neutral tokenizers (e.g. Unicode word boundary tokenization without unvetted English stemmers).
