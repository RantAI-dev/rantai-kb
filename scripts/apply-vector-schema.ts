#!/usr/bin/env bun
/**
 * Apply the SurrealDB schema the KB needs (tables, vector index, FTS analyzer).
 *
 * Idempotent — every statement uses IF NOT EXISTS, so it is safe to run on
 * every deploy. Run it before the service starts serving traffic.
 *
 *   bun run kb:apply-schema
 *
 * The embedding dimension MUST match KB_EMBEDDING_DIM: SurrealDB fixes the
 * vector width at index definition time, and a mismatch fails at insert with a
 * dimension error rather than silently degrading.
 */
import { getSurrealClient } from "../src/lib/surrealdb"

const DIM = Number(process.env.KB_EMBEDDING_DIM || 4096)

const STATEMENTS = [
  `DEFINE TABLE IF NOT EXISTS document_chunk SCHEMALESS;`,
  `DEFINE FIELD IF NOT EXISTS document_id ON document_chunk TYPE string;`,
  `DEFINE FIELD IF NOT EXISTS content ON document_chunk TYPE string;`,
  `DEFINE FIELD IF NOT EXISTS chunk_index ON document_chunk TYPE int;`,
  `DEFINE FIELD IF NOT EXISTS embedding ON document_chunk TYPE array<float>;`,
  `DEFINE FIELD IF NOT EXISTS contextual_prefix ON document_chunk TYPE option<string>;`,
  `DEFINE FIELD IF NOT EXISTS embedding_model ON document_chunk TYPE option<string>;`,
  `DEFINE INDEX IF NOT EXISTS document_id_idx ON document_chunk FIELDS document_id;`,
  `DEFINE ANALYZER IF NOT EXISTS kb_en TOKENIZERS class FILTERS lowercase, snowball(english);`,
  `DEFINE INDEX IF NOT EXISTS content_search_idx ON document_chunk FIELDS content SEARCH ANALYZER kb_en BM25(1.2, 0.75) HIGHLIGHTS;`,
  `DEFINE INDEX IF NOT EXISTS embedding_idx ON document_chunk FIELDS embedding HNSW DIMENSION ${DIM} DIST COSINE;`,
]

const surreal = await getSurrealClient()
for (const sql of STATEMENTS) {
  console.log(`[kb-schema] ${sql.slice(0, 90)}${sql.length > 90 ? "…" : ""}`)
  const res = await surreal.query(sql)
  const bad = (res as Array<{ status?: string }>).find((r) => r?.status === "ERR")
  if (bad) throw new Error(`DDL returned ERR: ${JSON.stringify(bad)} for: ${sql}`)
}
console.log(`[kb-schema] done (embedding dimension ${DIM})`)
process.exit(0)
