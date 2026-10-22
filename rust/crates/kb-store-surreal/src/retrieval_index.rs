//! `RetrievalIndex` over the legacy SurrealDB `document_chunk` table.
//!
//! This is the migration path's read arm, not a new store. It wraps the queries
//! that already exist (`VECTOR_SEARCH_QUERY`, `BM25_SEARCH_QUERY`,
//! `NEIGHBOURS_QUERY`, the chunk write and delete statements) behind the
//! domain-shaped port, so the engine can move onto `RetrievalIndex` before any
//! Qdrant adapter exists, and so the contract suite has something real to run
//! against.
//!
//! # What it deliberately does not do
//!
//! * **It does not tune SurrealDB.** The vector arm is the linear cosine scan
//!   that costs 0.95 ms per chunk in scope and leaves the HNSW index unused
//!   (`docs/eval-baseline.md` §5). Replacing it is the Qdrant adapter's job;
//!   the plan says no tuning work goes into SurrealDB.
//! * **It does not use `bm25_search_global`.** The port has no unscoped method,
//!   so there is nowhere to call it from.
//! * **It does not change the analyzer.** The `kb_en` analyzer's English
//!   Snowball stemmer stays, which is why [`IndexCapabilities::sparse`] says
//!   [`KeywordSemantics::AllTerms`]: `@@` is boolean AND, so "Siapakah yang
//!   menulis Candy Candy?" matches nothing. The contract suite skips that
//!   property with the reason printed, rather than pretending an adapter passes
//!   a property it cannot hold.
//!
//! # Id encoding
//!
//! The port's `ChunkId` is the bare ident `{document_id}_{chunk_index}` — the
//! string Bun writes and the string every query in this crate already assumes.
//! The `document_chunk:⟨…⟩` record wrapper is applied only when a statement is
//! rendered and stripped when a row is read, so a `Candidate` handed back from
//! `search` can be passed straight to `fetch`. That is the rule the port means
//! by "the adapter owns the encoding", and getting it wrong here would make
//! `search` → `fetch` fail in a way no unit test would notice.
//!
//! Tenant scoping rests on [`DocumentId`] being globally unique: a
//! `document_chunk` row has no tenant column, and Postgres owns that
//! uniqueness. Filtering by document id is therefore tenant-safe for the same
//! reason a document id is usable as a row key at all.

use async_trait::async_trait;
use kb_ports::{
    ArmRanks, ArmScores, Candidate, ChunkId, DocumentId, DocumentScope, Fusion, HybridQuery,
    IndexCapabilities, IndexUnit, KeywordSemantics, PortError, RetrievalIndex, TenantId,
    sort_candidates,
};

use crate::chunks_meta::execute_delete_by_document;
use crate::client::{MAX_LIMIT, SurrealChunkStore, row_chunk_type, row_meta, row_to_chunk};
use crate::queries::{
    BM25_SEARCH_QUERY, NEIGHBOURS_QUERY, VECTOR_SEARCH_QUERY, surql_f32_array, surql_id_list,
    surql_json, surql_string, surql_u32_array, validate_document_id,
};

/// Read specific chunks by record ident, scoped to the caller's documents.
///
/// `id IN` picks the rows; the `document_id IN […]` filter is what makes the
/// scope hold. A tenant that cannot see a document gets nothing even if it
/// guesses the id exactly, because the guessed id is not in the document list.
const FETCH_QUERY: &str = "SELECT id, document_id, content, chunk_index, metadata, contextual_prefix \
     FROM document_chunk \
     WHERE id IN {ids} AND document_id IN {doc_ids};";

/// One arm's ranked result: the parsed row, its score and its 0-based rank.
struct ArmHit {
    row: crate::client::ChunkRow,
    score: f32,
    rank: u32,
}

#[async_trait]
impl RetrievalIndex for SurrealChunkStore {
    async fn upsert(&self, units: Vec<IndexUnit>) -> Result<(), PortError> {
        let mut statements: Vec<String> = Vec::with_capacity(units.len());
        for unit in &units {
            validate_document_id(unit.document_id.as_str())?;
            statements.push(create_statement(unit)?);
        }
        if statements.is_empty() {
            return Ok(());
        }
        // One statement per chunk, batched like the existing write path, so a
        // 400-chunk book is not one request the size of the book. A statement
        // SurrealDB rejects fails the whole request, so a partial write can
        // only leave whole batches behind.
        for batch in statements.chunks(64) {
            self.query(&batch.join("\n")).await?;
        }
        Ok(())
    }

    async fn delete_document(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
    ) -> Result<u64, PortError> {
        // `tenant` is not in the row. Document ids are globally unique, so a
        // delete scoped to one document cannot reach another tenant's units.
        // Kept in the signature because Qdrant needs it (its point ids are a
        // UUIDv5 over the tenant) and because dropping it from the port to suit
        // one adapter is exactly the mistake the port exists to prevent.
        let _ = tenant;
        execute_delete_by_document(self, document_id).await
    }

    async fn search(
        &self,
        query: &HybridQuery,
        scope: &DocumentScope,
    ) -> Result<Vec<Candidate>, PortError> {
        let doc_ids: Vec<String> = scope
            .document_ids()
            .iter()
            .map(|d| d.as_str().to_string())
            .collect();
        if doc_ids.is_empty() {
            return Ok(Vec::new());
        }
        let doc_ids_sql = surql_id_list(&doc_ids)?;
        let limit = query.per_arm_k.clamp(1, MAX_LIMIT as usize);

        let dense = match &query.dense {
            None => None,
            Some(vector) if vector.is_empty() => None,
            Some(vector) => {
                let sql = VECTOR_SEARCH_QUERY
                    .replace("{embedding}", &surql_f32_array(vector))
                    .replace("{doc_ids}", &doc_ids_sql)
                    .replace("{limit}", &limit.to_string());
                let envelope = self.query(&sql).await?;
                Some(arm_hits(&envelope, ScoreColumn::Similarity))
            }
        };

        // The keyword arm goes through SurrealDB's own analyzer, so it is given
        // the query *terms*, not the hashes. `@@` is boolean AND: every term
        // must be present, which is exactly what this adapter declares.
        let sparse = match &query.sparse {
            Some(keyword) if !keyword.is_empty() => {
                let text = keyword.terms.join(" ");
                let sql = BM25_SEARCH_QUERY
                    .replace("{doc_ids}", &doc_ids_sql)
                    .replace("{limit}", &limit.to_string());
                let envelope = self.query_with_params(&sql, &[("q", &text)]).await?;
                Some(arm_hits(&envelope, ScoreColumn::Score))
            }
            _ => None,
        };

        let mut candidates = fuse(dense, sparse, query.fusion);
        sort_candidates(&mut candidates);
        candidates.truncate(query.k);
        // The tenant is the scope's: every row came back through its document
        // filter, so it belongs to the tenant the scope names.
        for candidate in &mut candidates {
            candidate.tenant = scope.tenant().clone();
        }
        Ok(candidates)
    }

    async fn fetch(
        &self,
        scope: &DocumentScope,
        ids: &[ChunkId],
    ) -> Result<Vec<Candidate>, PortError> {
        if ids.is_empty() || scope.document_ids().is_empty() {
            return Ok(Vec::new());
        }
        let record_ids: Vec<String> = ids.iter().map(|id| record_id(id.as_str())).collect();
        let doc_ids_sql = surql_id_list(
            &scope
                .document_ids()
                .iter()
                .map(|d| d.as_str().to_string())
                .collect::<Vec<_>>(),
        )?;
        let sql = FETCH_QUERY
            .replace("{ids}", &surql_id_array(&record_ids)?)
            .replace("{doc_ids}", &doc_ids_sql);
        let envelope = self.query(&sql).await?;
        let mut out: Vec<Candidate> = SurrealChunkStore::extract_rows(&envelope)
            .into_iter()
            .filter_map(|row| row_to_chunk(&row))
            .map(|row| candidate(row, None, None, scope.tenant()))
            .collect();
        // `id IN` promises no order and the caller asked for a set.
        out.sort_by(|a, b| a.unit_id.as_str().cmp(b.unit_id.as_str()));
        Ok(out)
    }

    async fn neighbours(
        &self,
        scope: &DocumentScope,
        document_id: &DocumentId,
        chunk_indices: &[u32],
    ) -> Result<Vec<Candidate>, PortError> {
        if chunk_indices.is_empty() || !scope.document_ids().contains(document_id) {
            return Ok(Vec::new());
        }
        validate_document_id(document_id.as_str())?;
        // Index ±1 of an anchor, excluding the anchors themselves. Computed
        // here so the bound is the port's and not the adapter's.
        let mut adjacent: Vec<u32> = Vec::new();
        for anchor in chunk_indices {
            if *anchor > 0 {
                adjacent.push(anchor - 1);
            }
            adjacent.push(anchor.saturating_add(1));
        }
        adjacent.sort_unstable();
        adjacent.dedup();

        let sql = NEIGHBOURS_QUERY
            .replace(
                "{doc_ids}",
                &surql_id_list(std::slice::from_ref(&document_id.as_str().to_string()))?,
            )
            .replace("{indices}", &surql_u32_array(&adjacent));
        let envelope = self.query(&sql).await?;
        let mut out: Vec<Candidate> = SurrealChunkStore::extract_rows(&envelope)
            .into_iter()
            .filter_map(|row| row_to_chunk(&row))
            .filter_map(|row| {
                let index = u32::try_from(row.chunk_index).ok()?;
                // Belt and braces: the statement already asked for exactly
                // these indices, so anything else means the store answered
                // with more than was requested and a bounded expansion would
                // silently become a document read.
                if !adjacent.contains(&index) || chunk_indices.contains(&index) {
                    return None;
                }
                Some(candidate(row, None, None, scope.tenant()))
            })
            .collect();
        // Ascending by chunk index: the order a reader assembles a document in.
        out.sort_by_key(|c| c.chunk_index);
        Ok(out)
    }

    async fn figures(
        &self,
        scope: &DocumentScope,
        document_ids: &[DocumentId],
    ) -> Result<Vec<Candidate>, PortError> {
        let matching_docs: Vec<String> = document_ids
            .iter()
            .filter(|d| scope.document_ids().contains(d))
            .map(|d| d.as_str().to_string())
            .collect();
        if matching_docs.is_empty() {
            return Ok(Vec::new());
        }
        for d in &matching_docs {
            validate_document_id(d)?;
        }
        let doc_ids_sql = surql_id_list(&matching_docs)?;
        let sql = crate::queries::FIGURES_QUERY.replace("{doc_ids}", &doc_ids_sql);
        let envelope = self.query(&sql).await?;
        let mut out: Vec<Candidate> = SurrealChunkStore::extract_rows(&envelope)
            .into_iter()
            .filter_map(|row| row_to_chunk(&row))
            .map(|row| candidate(row, None, None, scope.tenant()))
            .collect();
        out.sort_by(|a, b| a.unit_id.as_str().cmp(b.unit_id.as_str()));
        Ok(out)
    }

    async fn ordered_chunks(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
    ) -> Result<Vec<Candidate>, PortError> {
        validate_document_id(document_id.as_str())?;
        let chunks = crate::chunks_meta::execute_ordered_chunks(self, document_id).await?;
        Ok(chunks
            .into_iter()
            .map(|c| Candidate {
                unit_id: ChunkId::new_unchecked(c.id),
                document_id: document_id.clone(),
                tenant: tenant.clone(),
                chunk_index: c.chunk_index,
                page: c.metadata.page,
                chunk_type: kb_ports::ChunkType::from_storage(c.chunk_type.as_deref()),
                content: c.content,
                contextual_prefix: c.metadata.contextual_prefix.clone(),
                metadata: serde_json::to_value(&c.metadata).unwrap_or(serde_json::Value::Null),
                scores: ArmScores::default(),
                ranks: ArmRanks::default(),
            })
            .collect())
    }

    async fn health_check(&self) -> Result<(), PortError> {
        kb_ports::ChunkStore::health_check(self).await
    }

    fn capabilities(&self) -> IndexCapabilities {
        IndexCapabilities {
            // SurrealDB fixes the width at `DEFINE INDEX … DIMENSION` time and
            // never reads it back, so the connection carries it. Reporting a
            // real number rather than "unknown" keeps the dense arm's
            // properties running instead of skipping on an ambiguity.
            dense_dims: Some(self.settings.embedding_dims),
            // `@@` is boolean AND. Declared, not hidden.
            sparse: Some(KeywordSemantics::AllTerms),
            server_side_fusion: false,
            multivector: false,
            // `VECTOR_SEARCH_QUERY` computes cosine for every chunk in scope, so
            // the ranking is exact. It is also O(chunks in scope), which is what
            // is being replaced — but that is latency, not recall.
            approximate_dense: false,
        }
    }
}

/// Which projected column carries an arm's score.
enum ScoreColumn {
    Similarity,
    Score,
}

fn arm_hits(envelope: &[serde_json::Value], column: ScoreColumn) -> Vec<ArmHit> {
    SurrealChunkStore::extract_rows(envelope)
        .into_iter()
        .filter_map(|row| row_to_chunk(&row))
        .enumerate()
        .map(|(rank, row)| {
            let score = match column {
                ScoreColumn::Similarity => row.similarity.unwrap_or(0.0),
                ScoreColumn::Score => row.score.unwrap_or(0.0),
            };
            ArmHit {
                row,
                score,
                rank: rank as u32,
            }
        })
        .collect()
}

/// Merge two arm result lists into one candidate list.
///
/// Arms are absorbed in order (dense first) and a unit the second arm already
/// produced is merged into the existing entry rather than appended, so a unit
/// both arms found appears once with both scores set.
fn fuse(dense: Option<Vec<ArmHit>>, sparse: Option<Vec<ArmHit>>, fusion: Fusion) -> Vec<Candidate> {
    /// One merged unit: the row, the arm scores, the arm ranks.
    struct Merged {
        id: ChunkId,
        row: crate::client::ChunkRow,
        scores: ArmScores,
        ranks: ArmRanks,
    }

    let mut merged: Vec<Merged> = Vec::new();

    let absorb = |hits: Vec<ArmHit>, is_dense: bool, merged: &mut Vec<Merged>| {
        for hit in hits {
            let id = ChunkId::new_unchecked(bare_id(&hit.row.id));
            let position = match merged.iter().position(|m| m.id == id) {
                Some(p) => p,
                None => {
                    merged.push(Merged {
                        id: id.clone(),
                        row: hit.row,
                        scores: ArmScores::default(),
                        ranks: ArmRanks::default(),
                    });
                    merged.len() - 1
                }
            };
            let entry = &mut merged[position];
            match fusion {
                Fusion::Rrf => {
                    // Qdrant's arithmetic, done here because SurrealDB cannot:
                    // one contribution per arm, `1 / (2 + rank)` with the rank
                    // 0-based inside that arm's list.
                    entry.scores.fused += kb_core::rrf_contribution(hit.rank);
                }
                Fusion::None => {
                    // The arm's own score is the ranking key when no fusion was
                    // asked for, so a single-arm caller sees its own scale.
                    if is_dense || entry.scores.fused == 0.0 {
                        entry.scores.fused = hit.score;
                    }
                }
            }
            if is_dense {
                entry.scores.dense = Some(hit.score);
                entry.ranks.dense = Some(hit.rank);
            } else {
                entry.scores.sparse = Some(hit.score);
                entry.ranks.sparse = Some(hit.rank);
            }
        }
    };

    if let Some(hits) = dense {
        absorb(hits, true, &mut merged);
    }
    if let Some(hits) = sparse {
        absorb(hits, false, &mut merged);
    }

    // The tenant is filled in by `search` from the scope, which is where every
    // row came back through.
    let empty = TenantId::new_unchecked("");
    merged
        .into_iter()
        .map(|m| {
            let mut c = candidate(m.row, m.scores.dense, m.scores.sparse, &empty);
            // `candidate` derives fused from whichever arm score it was given;
            // restore the fused value the merge actually computed.
            c.scores.fused = m.scores.fused;
            c.ranks = m.ranks;
            debug_assert_eq!(c.unit_id, m.id);
            c
        })
        .collect()
}

/// The `CREATE document_chunk SET …` statement for one unit.
///
/// Ids are `${document_id}_${chunk_index}` like every other write in this
/// crate, so a re-ingest overwrites rather than duplicates. Values are
/// JSON-escaped literals, never spliced.
fn create_statement(unit: &IndexUnit) -> Result<String, PortError> {
    let mut sets = vec![
        format!(
            "id = {}",
            surql_string(&format!(
                "{}_{}",
                unit.document_id.as_str(),
                unit.chunk_index
            ))
        ),
        format!("document_id = {}", surql_string(unit.document_id.as_str())),
        format!("content = {}", surql_string(&unit.content)),
        format!("chunk_index = {}", unit.chunk_index),
        format!("embedding = {}", surql_f32_array(&unit.dense)),
        "created_at = time::now()".to_string(),
    ];
    // SurrealDB rejects an explicit NULL for `option<T>`, so an absent value is
    // left out of the assignment list entirely rather than set to null.
    if let Some(prefix) = unit.contextual_prefix.as_deref().filter(|p| !p.is_empty()) {
        sets.push(format!("contextual_prefix = {}", surql_string(prefix)));
    }
    // The unit's own metadata is stored verbatim, as the engine's existing write
    // path does, so Bun and the legacy read path see the same column shape.
    // `chunkIndex`, `chunkType` and `page` are filled from the domain fields so
    // a round trip does not depend on the caller having set them twice.
    let mut metadata = unit.metadata.clone();
    if let Some(object) = metadata.as_object_mut() {
        object.insert(
            "chunkIndex".to_string(),
            serde_json::json!(unit.chunk_index),
        );
        object.insert(
            "chunkType".to_string(),
            serde_json::json!(unit.chunk_type.as_str()),
        );
        if let Some(page) = unit.page {
            object.insert("page".to_string(), serde_json::json!(page));
        }
    }
    sets.push(format!("metadata = {}", surql_json(&metadata)));
    // UPSERT, not CREATE. Measured against SurrealDB v2: a second `CREATE`
    // under an existing record id fails with "Database record already exists",
    // so a `CREATE`-based upsert silently keeps the *first* write. The legacy
    // `store_chunks` path gets away with CREATE because ingest deletes a
    // document's chunks before rewriting them; this port promises last-write-
    // wins on its own, so it cannot rely on that.
    Ok(format!("UPSERT document_chunk SET {};", sets.join(", ")))
}

/// `document_chunk:⟨ident⟩`, for a `WHERE id IN` comparison or a `CREATE`.
fn record_id(ident: &str) -> String {
    format!("document_chunk:⟨{ident}⟩")
}

/// The bare ident inside a record id. SurrealDB answers with
/// `document_chunk:⟨x⟩`; the port speaks `x`.
fn bare_id(record: &str) -> String {
    match record.split_once(':') {
        Some((_, rest)) => rest
            .trim_matches(|c| c == '⟨' || c == '⟩' || c == '`')
            .to_string(),
        None => record.to_string(),
    }
}

/// Render record ids as a SurrealQL array, validating the bare idents.
fn surql_id_array(ids: &[String]) -> Result<String, PortError> {
    for id in ids {
        validate_document_id(&bare_id(id))?;
    }
    let parts: Vec<String> = ids.to_vec();
    Ok(format!("[{}]", parts.join(", ")))
}

/// Hydrate a candidate from a `document_chunk` row.
fn candidate(
    row: crate::client::ChunkRow,
    dense: Option<f32>,
    sparse: Option<f32>,
    tenant: &TenantId,
) -> Candidate {
    let metadata = row_meta(row.metadata.as_ref());
    let chunk_index = u32::try_from(row.chunk_index).unwrap_or(0);
    // The row column is authoritative, exactly as `build_hit` treats it: the
    // neighbour lookup keys on it.
    Candidate {
        unit_id: ChunkId::new_unchecked(bare_id(&row.id)),
        document_id: DocumentId::new_unchecked(row.document_id),
        tenant: tenant.clone(),
        chunk_index,
        page: metadata.page,
        chunk_type: row_chunk_type(row.metadata.as_ref()),
        content: row.content,
        contextual_prefix: row.contextual_prefix.or(metadata.contextual_prefix),
        metadata: row.metadata.unwrap_or(serde_json::Value::Null),
        scores: ArmScores {
            dense,
            sparse,
            fused: dense.or(sparse).unwrap_or(0.0),
        },
        ranks: ArmRanks::default(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use kb_ports::{ChunkType, IndexUnit};

    fn unit(id: &str) -> IndexUnit {
        IndexUnit {
            id: ChunkId::new_unchecked(id),
            document_id: DocumentId::new_unchecked("doc1"),
            tenant: TenantId::new_unchecked("t"),
            knowledge_base_id: None,
            category: None,
            chunk_index: 0,
            page: Some(3),
            chunk_type: ChunkType::Figure,
            content: "body text".to_string(),
            contextual_prefix: Some("title > chunk".to_string()),
            metadata: serde_json::json!({ "documentTitle": "T" }),
            dense: vec![0.5, 0.25],
            sparse: kb_ports::KeywordQuery::from_text("body text").vector,
        }
    }

    #[test]
    fn upsert_uses_upsert_not_create() {
        // `CREATE` under an existing record id fails in SurrealDB v2 ("Database
        // record already exists"), which would make the second write of a
        // re-ingest vanish. Pinned here so the change back is deliberate.
        let sql = create_statement(&unit("doc1_0")).unwrap();
        assert!(sql.starts_with("UPSERT "), "{sql}");
        assert!(!sql.contains("CREATE "), "{sql}");
    }

    #[test]
    fn a_record_id_round_trips_through_the_bare_ident() {
        // The whole reason this function exists: a Candidate from `search` must
        // be usable as a `fetch` argument without unwrapping a record id.
        let row = crate::client::ChunkRow {
            id: "document_chunk:⟨doc1_0⟩".to_string(),
            document_id: "doc1".to_string(),
            content: "body".to_string(),
            chunk_index: 0,
            metadata: None,
            contextual_prefix: None,
            similarity: None,
            score: None,
        };
        assert_eq!(bare_id(&row.id), "doc1_0");
        assert_eq!(bare_id("document_chunk:`doc1_0`"), "doc1_0");
        assert_eq!(bare_id("doc1_0"), "doc1_0");
        assert_eq!(record_id("doc1_0"), "document_chunk:⟨doc1_0⟩");
        // Round trip, which is the property `search` → `fetch` depends on.
        assert_eq!(bare_id(&record_id("doc1_0")), "doc1_0");
    }

    #[test]
    fn a_written_chunk_uses_the_bun_id_shape_and_the_domain_fields() {
        let sql = create_statement(&unit("ignored")).unwrap();
        assert!(sql.starts_with("UPSERT document_chunk SET "));
        assert!(sql.contains(r#"id = "doc1_0""#), "{sql}");
        assert!(sql.contains("chunk_index = 0"));
        assert!(sql.contains("embedding = [0.5, 0.25]"));
        assert!(sql.contains(r#"contextual_prefix = "title > chunk""#));
        // chunkIndex, chunkType and page come from the domain fields, so the
        // round trip does not depend on the caller having set them twice.
        assert!(sql.contains(r#""chunkIndex":0"#), "{sql}");
        assert!(sql.contains(r#""chunkType":"figure""#), "{sql}");
        assert!(sql.contains(r#""page":3"#), "{sql}");
        assert!(sql.contains(r#""documentTitle":"T""#), "{sql}");
        // The unit's own id is never written: the record id is derived, so a
        // caller cannot create two chunks under different id shapes.
        assert!(!sql.contains("ignored"), "{sql}");
    }

    #[test]
    fn content_cannot_close_the_statement() {
        let mut u = unit("doc1_0");
        u.content = "x\"; DELETE document_chunk; --".to_string();
        let sql = create_statement(&u).unwrap();
        assert_eq!(sql.matches("UPSERT document_chunk").count(), 1);
        assert!(
            sql.contains(r#"content = "x\"; DELETE document_chunk; --""#),
            "{sql}"
        );
    }

    #[test]
    fn a_unit_without_a_prefix_omits_the_column_rather_than_nulling_it() {
        let mut u = unit("doc1_0");
        u.contextual_prefix = None;
        let sql = create_statement(&u).unwrap();
        assert!(!sql.contains("contextual_prefix"));
        assert!(!sql.to_lowercase().contains("null"));
    }

    #[test]
    fn record_ids_are_validated_before_reaching_the_query_text() {
        assert!(surql_id_array(&[record_id("doc1_0")]).is_ok());
        assert!(surql_id_array(&[record_id("doc 1_0")]).is_err());
        assert!(surql_id_array(&[record_id("doc1_0\"; REMOVE DATABASE kb; --")]).is_err());
    }

    #[test]
    fn capabilities_declare_all_terms_and_an_exact_dense_arm() {
        // No store instance is needed: the declaration is a compile-time fact
        // about the backend, which is the point of declaring it at all.
        let store = SurrealChunkStore::new(crate::client::SurrealSettings {
            url: "http://127.0.0.1:1".to_string(),
            namespace: "n".to_string(),
            database: "d".to_string(),
            username: None,
            password: None,
            request_timeout: std::time::Duration::from_millis(1),
            embedding_dims: 1024,
        })
        .unwrap();
        let caps = store.capabilities();
        assert_eq!(caps.sparse, Some(KeywordSemantics::AllTerms));
        // The dense arm exists; its width is reported rather than "unknown", so
        // the contract's dense properties run instead of skipping.
        assert_eq!(caps.dense_dims, Some(1024));
        assert!(!caps.approximate_dense, "the arm is a linear cosine scan");
        assert!(!caps.server_side_fusion);
    }

    #[test]
    fn fusion_merges_a_unit_both_arms_found_and_keeps_both_scores() {
        let row = |id: &str| crate::client::ChunkRow {
            id: record_id(id),
            document_id: "doc1".to_string(),
            content: "body".to_string(),
            chunk_index: 0,
            metadata: None,
            contextual_prefix: None,
            similarity: None,
            score: None,
        };
        let hit = |id: &str, rank: u32, score: f32| ArmHit {
            row: row(id),
            score,
            rank,
        };
        let merged = fuse(
            Some(vec![hit("doc1_0", 0, 0.9), hit("doc1_1", 1, 0.5)]),
            Some(vec![hit("doc1_1", 0, 12.0), hit("doc1_2", 1, 3.0)]),
            Fusion::Rrf,
        );
        let ids: Vec<&str> = merged.iter().map(|c| c.unit_id.as_str()).collect();
        assert_eq!(ids, vec!["doc1_0", "doc1_1", "doc1_2"]);

        // doc1_1 was found by both: one row, both scores, both ranks.
        let both = &merged[1];
        assert_eq!(both.scores.dense, Some(0.5));
        assert_eq!(both.scores.sparse, Some(12.0));
        assert_eq!(both.ranks.dense, Some(1));
        assert_eq!(both.ranks.sparse, Some(0));
        // Qdrant's RRF: 1/(2 + 1) from the dense arm, 1/(2 + 0) from the sparse one.
        assert!((both.scores.fused - (1.0 / 3.0 + 1.0 / 2.0)).abs() < 1e-6);

        // Two arms agreeing outranks one arm alone at the same rank.
        let single = &merged[0];
        assert!(
            both.scores.fused > single.scores.fused,
            "{} vs {}",
            both.scores.fused,
            single.scores.fused
        );
        assert_eq!(single.scores.sparse, None);
        assert_eq!(single.ranks.sparse, None);
    }

    #[test]
    fn fusion_none_keeps_the_arms_in_order_without_combining_them() {
        let row = |id: &str| crate::client::ChunkRow {
            id: record_id(id),
            document_id: "doc1".to_string(),
            content: "body".to_string(),
            chunk_index: 0,
            metadata: None,
            contextual_prefix: None,
            similarity: None,
            score: None,
        };
        let hit = |id: &str, rank: u32, score: f32| ArmHit {
            row: row(id),
            score,
            rank,
        };
        let merged = fuse(
            Some(vec![hit("doc1_0", 0, 0.9)]),
            Some(vec![hit("doc1_1", 0, 12.0)]),
            Fusion::None,
        );
        let ids: Vec<&str> = merged.iter().map(|c| c.unit_id.as_str()).collect();
        assert_eq!(ids, vec!["doc1_0", "doc1_1"]);
        assert_eq!(merged[0].scores.fused, 0.9);
        assert_eq!(merged[1].scores.fused, 12.0);
    }
}
