//! Request and response DTOs for `kb-server`.
//!
//! Explicit domain-to-wire conversions matching the TypeScript service's JSON shape.

use kb_core::SourceEntry;
use kb_engine::{RetrievalOutcome, timing::StageTimings};
use kb_ports::{ProvenanceArm, RetrievalProvenance, SearchHit};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Parse a request body the way the TypeScript routes do: `request.json()`
/// wrapped in `.catch(() => ({}))`, so a malformed body becomes an empty object
/// and the route answers with its own field-level error instead of a parser
/// message the service never produces.
pub fn parse_json<T: DeserializeOwned + Default>(body: &[u8]) -> T {
    serde_json::from_slice(body).unwrap_or_default()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchRequestDto {
    /// Left as raw JSON: the TypeScript route rejects a non-string query with
    /// its own 400, which a typed field would turn into a parser error.
    #[serde(default)]
    pub query: Option<serde_json::Value>,
    #[serde(rename = "maxChunks", default)]
    pub max_chunks: Option<u32>,
    #[serde(rename = "knowledgeBaseIds", default)]
    pub knowledge_base_ids: Option<Vec<String>>,
    #[serde(rename = "documentIds", default)]
    pub document_ids: Option<Vec<String>>,
    pub category: Option<String>,
    pub hybrid: Option<bool>,
    pub format: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchResponseDto {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub chunks: Option<Vec<ChunkDto>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub sources: Option<Vec<SourceDto>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub results: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub stats: Option<SearchStatsDto>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub degrades: Vec<String>,
}

/// The `stats` object of a search response.
///
/// Every field of the Bun `HybridSearchStats` is kept, with the same name and
/// the same meaning, because clients already read them and this service is
/// meant to be a drop-in for it. `stageTimingsMs` is additive: a client that
/// has never heard of it keeps working, and a client that reads it gets the
/// part of the latency that the five legacy fields cannot explain — the arms,
/// the rerank, the Postgres round trips and the serialisation.
///
/// The legacy fields are now measured rather than zeroed, so `embeddingTimeMs`
/// on the standard path is a number instead of a lie. Milliseconds keep
/// microsecond resolution: a query-cache hit and a 0.4 ms arm are both "0 ms"
/// once rounded to an integer, and the difference between them is the whole
/// reason this exists.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchStatsDto {
    /// Route entry to just before serialisation, in milliseconds.
    #[serde(rename = "totalTimeMs")]
    pub total_time_ms: f64,
    #[serde(rename = "embeddingTimeMs")]
    pub embedding_time_ms: f64,
    #[serde(rename = "vectorSearchTimeMs")]
    pub vector_search_time_ms: f64,
    #[serde(rename = "entitySearchTimeMs")]
    pub entity_search_time_ms: f64,
    #[serde(rename = "graphSearchTimeMs")]
    pub graph_search_time_ms: f64,
    #[serde(rename = "fusionTimeMs")]
    pub fusion_time_ms: f64,
    #[serde(rename = "vectorResults")]
    pub vector_results: usize,
    #[serde(rename = "entityResults")]
    pub entity_results: usize,
    #[serde(rename = "graphResults")]
    pub graph_results: usize,
    #[serde(rename = "totalResults")]
    pub total_results: usize,
    #[serde(rename = "stageTimingsMs")]
    pub stage_timings_ms: StageTimingsDto,
}

/// The per-stage breakdown, in milliseconds.
///
/// The top level is a *partition* of the request: the stages there run one
/// after another and add up to no more than [`SearchStatsDto::total_time_ms`].
/// `detail` is the overlapping part — the query embed and the two arms all
/// happen inside `arms`, so they are reported, never summed with it.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StageTimingsDto {
    /// `build_scope` in Postgres: which documents this caller may see.
    pub scope_build: f64,
    pub expand: f64,
    /// The concurrent retrieval phase: query embed + both arms.
    pub arms: f64,
    /// Alive check and title/category enrichment on the hits that survived.
    pub enrich: f64,
    pub fusion: f64,
    pub rerank: f64,
    /// Page-image arm after the text rerank: page fusion and page rerank.
    pub pages: f64,
    pub neighbours: f64,
    pub figures: f64,
    /// Building the context string and the numbered sources.
    pub context: f64,
    /// Turning the response into bytes.
    pub serialise: f64,
    /// Route entry to just before serialisation. The sum of everything above
    /// is bounded by it.
    pub total: f64,
    /// The overlapping measurements, reported for diagnosis only.
    pub detail: StageDetailDto,
}

/// Measurements that overlap other stages and so are not part of the sum.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StageDetailDto {
    /// What the engine spent inside `execute_pipeline`.
    pub engine_total: f64,
    pub query_embed: f64,
    /// True when the query embedding came from the cache rather than the
    /// provider.
    pub query_embed_cached: bool,
    /// The vector arm's store call on its own.
    pub vector_arm: f64,
    /// The keyword arm's store call on its own.
    pub keyword_arm: f64,
    /// Always 0: the entity graph reaches search through the graph arm, not as an
    /// arm of its own. Present so a client that reads it does not lose a field.
    pub entity_arm: f64,
    /// The graph arm, end to end: seed lookup, scoped-graph load or cache hit,
    /// the walk, and the chunk fetch it needs to fuse.
    pub graph_arm: f64,
    /// The page-image arm's query embed and page search, concurrent with the
    /// text arms.
    pub page_arm: f64,
    /// The live vision rerank of the top page images, inside `pages`.
    pub vision_rerank: f64,
    /// `find_alive_meta_by_ids` inside the keyword arm (Postgres).
    pub scope_check: f64,
}

/// The accounted stages as `(name, ms)`, for logging and for tests.
///
/// `total` is excluded: it is the bound, not one of the things that adds up to
/// it.
#[must_use]
pub fn accounted_pairs(s: &StageTimingsDto) -> [(&'static str, f64); 11] {
    [
        ("scopeBuild", s.scope_build),
        ("expand", s.expand),
        ("arms", s.arms),
        ("enrich", s.enrich),
        ("fusion", s.fusion),
        ("rerank", s.rerank),
        ("pages", s.pages),
        ("neighbours", s.neighbours),
        ("figures", s.figures),
        ("context", s.context),
        ("serialise", s.serialise),
    ]
}

impl StageTimingsDto {
    /// The sum of the accounted stages. Must never exceed `total`.
    #[must_use]
    pub fn accounted_sum_ms(&self) -> f64 {
        accounted_pairs(self).iter().map(|(_, ms)| *ms).sum()
    }

    /// A stage report for a route that ran nothing: the shape stays the same,
    /// every number is zero.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Round every millisecond to the microsecond the clock actually measured.
    ///
    /// A raw `f64` millisecond carries a tail of digits that no clock in the
    /// pipeline could have produced, and `metrics.json` is a file people diff.
    #[must_use]
    pub fn rounded(self) -> Self {
        Self {
            scope_build: round_ms(self.scope_build),
            expand: round_ms(self.expand),
            arms: round_ms(self.arms),
            enrich: round_ms(self.enrich),
            fusion: round_ms(self.fusion),
            rerank: round_ms(self.rerank),
            pages: round_ms(self.pages),
            neighbours: round_ms(self.neighbours),
            figures: round_ms(self.figures),
            context: round_ms(self.context),
            serialise: round_ms(self.serialise),
            total: round_ms(self.total),
            detail: self.detail.rounded(),
        }
    }
}

/// Round milliseconds to microsecond resolution.
#[must_use]
pub fn round_ms(ms: f64) -> f64 {
    (ms * 1000.0).round() / 1000.0
}

impl StageDetailDto {
    /// Round every millisecond to the microsecond the clock measured.
    #[must_use]
    pub fn rounded(self) -> Self {
        Self {
            engine_total: round_ms(self.engine_total),
            query_embed: round_ms(self.query_embed),
            query_embed_cached: self.query_embed_cached,
            vector_arm: round_ms(self.vector_arm),
            keyword_arm: round_ms(self.keyword_arm),
            entity_arm: round_ms(self.entity_arm),
            graph_arm: round_ms(self.graph_arm),
            page_arm: round_ms(self.page_arm),
            vision_rerank: round_ms(self.vision_rerank),
            scope_check: round_ms(self.scope_check),
        }
    }
}

impl From<&StageTimings> for StageDetailDto {
    fn from(t: &StageTimings) -> Self {
        Self {
            engine_total: t.engine_total_ms,
            query_embed: t.query_embed_ms,
            query_embed_cached: t.query_embed_cached,
            vector_arm: t.vector_arm_ms,
            keyword_arm: t.keyword_arm_ms,
            entity_arm: t.entity_arm_ms,
            graph_arm: t.graph_arm_ms,
            page_arm: t.page_arm_ms,
            vision_rerank: t.vision_rerank_ms,
            scope_check: t.scope_check_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceDto {
    #[serde(rename = "documentId")]
    pub document_id: Option<String>,
    #[serde(rename = "documentTitle")]
    pub document_title: String,
    pub section: Option<String>,
    pub categories: Vec<String>,
    #[serde(rename = "assetKey")]
    pub asset_key: Option<String>,
    pub page: Option<u32>,
    #[serde(rename = "chunkType")]
    pub chunk_type: Option<String>,
}

/// Which retrieval arms produced a chunk, as the response reports it.
///
/// **Additive** (step S.1c): Bun does not emit `retrieval`, and every field Bun
/// does emit is unchanged. `search-parity.sh` deletes `.chunks[].retrieval`
/// before diffing a Bun answer against a Rust one, because a field only one
/// implementation produces is not a divergence to fix — see
/// `docs/rust-cutover.md`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RetrievalDto {
    /// The arms, in a canonical order: `dense`, `sparse`, `neighbour`, `figure`.
    pub arms: Vec<ProvenanceArm>,
    #[serde(rename = "denseRank", skip_serializing_if = "Option::is_none")]
    pub dense_rank: Option<u32>,
    #[serde(rename = "sparseRank", skip_serializing_if = "Option::is_none")]
    pub sparse_rank: Option<u32>,
    #[serde(rename = "fusedScore", skip_serializing_if = "Option::is_none")]
    pub fused_score: Option<f32>,
}

impl From<RetrievalProvenance> for RetrievalDto {
    fn from(mut p: RetrievalProvenance) -> Self {
        // Last stop before the wire: sort and dedupe here rather than trust every
        // constructor upstream, so two servers can never report the same arms in
        // two orders. `ProvenanceArm`'s `Ord` *is* the canonical order.
        p.arms.sort_unstable();
        p.arms.dedup();
        Self {
            arms: p.arms,
            dense_rank: p.dense_rank,
            sparse_rank: p.sparse_rank,
            fused_score: p.fused_score,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChunkDto {
    pub id: String,
    pub content: String,
    #[serde(rename = "documentId")]
    pub document_id: String,
    #[serde(rename = "documentTitle")]
    pub document_title: String,
    pub categories: Vec<String>,
    pub subcategory: Option<String>,
    pub section: Option<String>,
    pub similarity: f32,
    #[serde(rename = "contextualPrefix", skip_serializing_if = "Option::is_none")]
    pub contextual_prefix: Option<Option<String>>,
    #[serde(rename = "assetKey", skip_serializing_if = "Option::is_none")]
    pub asset_key: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<Option<u32>>,
    #[serde(rename = "chunkType", skip_serializing_if = "Option::is_none")]
    pub chunk_type: Option<Option<String>>,
    /// Present only when the engine knows which arms produced this chunk. Omitted
    /// rather than null, so a caller can tell "no arms" from "old server".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retrieval: Option<RetrievalDto>,
}

impl SearchResponseDto {
    /// The "nothing matched" shape, shared by the empty-scope and empty-result
    /// paths so all three answer with the same keys.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            chunks: Some(Vec::new()),
            sources: Some(Vec::new()),
            context: Some(String::new()),
            results: None,
            stats: None,
            note: None,
            degrades: Vec::new(),
        }
    }
}

impl From<RetrievalOutcome> for SearchResponseDto {
    fn from(outcome: RetrievalOutcome) -> Self {
        let vector_results = outcome.chunks.iter().filter(|h| !h.is_bm25).count();
        let total_results = outcome.chunks.len();
        let chunks: Vec<ChunkDto> = outcome.chunks.into_iter().map(ChunkDto::from).collect();
        let sources = outcome.sources.into_iter().map(SourceDto::from).collect();
        let degrades = outcome
            .degrades
            .into_iter()
            .map(|d| d.label().to_string())
            .collect();
        // The route adds its own stages (scope build, serialisation) and the
        // total afterwards; what the engine knows goes in here.
        let stats = SearchStatsDto {
            total_time_ms: outcome.timings.total_ms,
            embedding_time_ms: outcome.timings.query_embed_ms,
            vector_search_time_ms: outcome.timings.vector_arm_ms,
            entity_search_time_ms: outcome.timings.entity_arm_ms,
            graph_search_time_ms: 0.0,
            fusion_time_ms: outcome.timings.fusion_ms,
            vector_results,
            entity_results: 0,
            graph_results: 0,
            total_results,
            stage_timings_ms: StageTimingsDto {
                expand: outcome.timings.expand_ms,
                arms: outcome.timings.arms_ms,
                enrich: outcome.timings.enrich_ms,
                fusion: outcome.timings.fusion_ms,
                rerank: outcome.timings.rerank_ms,
                pages: outcome.timings.pages_ms,
                neighbours: outcome.timings.neighbours_ms,
                figures: outcome.timings.figures_ms,
                context: outcome.timings.context_ms,
                detail: StageDetailDto::from(&outcome.timings),
                ..StageTimingsDto::default()
            },
        };
        SearchResponseDto {
            chunks: Some(chunks),
            sources: Some(sources),
            context: Some(outcome.context),
            results: None,
            stats: Some(stats),
            note: None,
            degrades,
        }
    }
}

impl From<SearchHit> for ChunkDto {
    fn from(hit: SearchHit) -> Self {
        let (contextual_prefix, asset_key, page, chunk_type) = if hit.is_bm25 {
            (
                Some(None), // BM25 hit serializes contextualPrefix: null
                None,       // BM25 hit omits assetKey
                // A keyword hit carries its page when the store knows it. Bun's
                // keyword arm had none, so its shape omitted the key; dropping a
                // known page left every keyword-only hit uncitable
                // (docs/rust-cutover.md §10). Still omitted when unknown.
                hit.chunk.metadata.page.map(Some),
                None, // BM25 hit omits chunkType
            )
        } else {
            (
                hit.chunk.metadata.contextual_prefix.map(Some), // vector hit omits contextualPrefix if None
                Some(hit.chunk.metadata.asset_key), // vector hit serializes assetKey (null if None)
                Some(hit.chunk.metadata.page),      // vector hit serializes page (null if None)
                Some(Some(hit.chunk.chunk_type.as_storage().to_string())), // vector hit serializes chunkType: "text"
            )
        };
        ChunkDto {
            id: hit.chunk.id.0,
            content: hit.chunk.content,
            document_id: hit.chunk.document_id.0,
            document_title: hit.chunk.document_title,
            categories: hit.chunk.categories,
            subcategory: hit.chunk.subcategory,
            section: hit.chunk.metadata.section,
            similarity: hit.similarity,
            contextual_prefix,
            asset_key,
            page,
            chunk_type,
            retrieval: hit.retrieval.map(RetrievalDto::from),
        }
    }
}

impl From<SourceEntry> for SourceDto {
    fn from(s: SourceEntry) -> Self {
        SourceDto {
            document_id: s.document_id,
            document_title: s.document_title,
            section: s.section,
            categories: s.categories,
            asset_key: s.asset_key,
            page: s.page,
            chunk_type: s.chunk_type,
        }
    }
}

/// Add one member to an already-serialised JSON object, without walking it
/// again.
///
/// The route has to report how long serialisation took *inside* the response it
/// is serialising, so it cannot simply serialise once and be done: the number
/// it would report is the cost of a response that does not contain it. Doing it
/// the obvious way — serialise the whole body twice — means the second pass is
/// unmeasured and the cost of a result set grows with the result set, which is
/// exactly the kind of thing this instrument exists to catch.
///
/// Splicing is safe because `serde_json` emits a top-level object as exactly
/// `{`, members separated by `,`, and a final `}` with no trailing whitespace.
/// The empty-object case is handled separately so `{,"stats":…}` is never
/// produced.
///
/// # Panics
///
/// If `body` is not a JSON object — which would mean a caller serialised
/// something other than a struct. It is a programming error, not an input.
#[must_use]
pub fn splice_json_member(mut body: Vec<u8>, key: &str, value: &[u8]) -> Vec<u8> {
    assert_eq!(
        body.first(),
        Some(&b'{'),
        "splice_json_member expects a JSON object"
    );
    let empty = body.len() == 2;
    assert_eq!(
        body.last(),
        Some(&b'}'),
        "splice_json_member expects a JSON object"
    );
    body.pop();
    if !empty {
        body.push(b',');
    }
    body.extend_from_slice(format!("\"{key}\":").as_bytes());
    body.extend_from_slice(value);
    body.push(b'}');
    body
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// A `SearchHit` as the engine produces it, with `bm25` deciding whether the
    /// Bun "keyword hit" shape applies.
    fn hit(bm25: bool, retrieval: Option<RetrievalProvenance>) -> SearchHit {
        SearchHit {
            chunk: kb_ports::Chunk {
                id: kb_ports::ChunkId("c-1".to_string()),
                document_id: kb_ports::DocumentId("d-1".to_string()),
                document_title: "Doc".to_string(),
                content: "body".to_string(),
                chunk_type: kb_ports::ChunkType::Text,
                metadata: kb_ports::ChunkMetadata {
                    page: Some(4),
                    section: Some("Intro".to_string()),
                    chunk_index: Some(2),
                    contextual_prefix: None,
                    ..Default::default()
                },
                categories: vec!["guides".to_string()],
                subcategory: None,
            },
            similarity: if bm25 { 0.0 } else { 0.81f32 },
            rank: 0,
            is_bm25: bm25,
            retrieval,
        }
    }

    #[test]
    fn a_vector_hit_keeps_every_bun_key_and_adds_only_retrieval() {
        let dto = ChunkDto::from(hit(
            false,
            Some(RetrievalProvenance {
                arms: vec![ProvenanceArm::Dense],
                dense_rank: Some(0),
                sparse_rank: None,
                graph_rank: None,
                fused_score: Some(0.81f32),
            }),
        ));
        let value = serde_json::to_value(&dto).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "assetKey",
                "categories",
                "chunkType",
                "content",
                "documentId",
                "documentTitle",
                "id",
                "page",
                "retrieval",
                "section",
                "similarity",
                "subcategory",
            ],
            "{value}"
        );
        // Bun's exact values, one for one.
        assert_eq!(value["chunkType"], "text");
        assert_eq!(value["page"], 4);
        assert_eq!(value["assetKey"], serde_json::Value::Null);
        assert_eq!(value["section"], "Intro");
        // f32 all the way out: 0.81 is not representable, and the wire value is
        // what the server computes, not the decimal a test happens to write.
        assert_eq!(value["similarity"], 0.81f32);
        assert_eq!(value["categories"], serde_json::json!(["guides"]));
        assert_eq!(
            value["retrieval"],
            serde_json::json!({"arms": ["dense"], "denseRank": 0, "fusedScore": 0.81f32})
        );
    }

    #[test]
    fn a_keyword_hit_without_a_known_page_still_omits_the_key() {
        let mut h = hit(true, None);
        h.chunk.metadata.page = None;
        let value = serde_json::to_value(ChunkDto::from(h)).unwrap();
        assert!(value.get("page").is_none(), "{value}");
    }

    #[test]
    fn a_keyword_hit_keeps_its_bun_omissions_and_gains_retrieval() {
        // The shape `search-parity.sh` and the Bun client both rely on: no
        // chunkType, no assetKey, and an explicit `contextualPrefix: null`; the
        // page is kept when known (docs/rust-cutover.md §10).
        let value = serde_json::to_value(ChunkDto::from(hit(
            true,
            Some(RetrievalProvenance {
                arms: vec![ProvenanceArm::Sparse],
                dense_rank: None,
                sparse_rank: Some(2),
                graph_rank: None,
                fused_score: Some(0.5),
            }),
        )))
        .unwrap();
        assert!(value.get("chunkType").is_none(), "{value}");
        assert_eq!(value["page"], 4, "a known page is kept: {value}");
        assert!(value.get("assetKey").is_none(), "{value}");
        assert_eq!(value["contextualPrefix"], serde_json::Value::Null);
        assert_eq!(value["similarity"], 0.0);
        assert_eq!(
            value["retrieval"],
            serde_json::json!({"arms": ["sparse"], "sparseRank": 2, "fusedScore": 0.5})
        );
    }

    #[test]
    fn no_provenance_means_the_key_is_absent_not_null() {
        // A caller has to be able to tell "this server reported no arm" from "an
        // old server that does not report arms at all".
        let value = serde_json::to_value(ChunkDto::from(hit(false, None))).unwrap();
        assert!(value.get("retrieval").is_none(), "{value}");
        assert_eq!(
            value.as_object().map(serde_json::Map::len),
            Some(11),
            "without provenance a chunk is exactly the Bun object: {value}"
        );
    }

    #[test]
    fn an_unscored_phase_reports_no_score_at_all() {
        let value = serde_json::to_value(ChunkDto::from(hit(
            false,
            Some(RetrievalProvenance::unscored(ProvenanceArm::Neighbour)),
        )))
        .unwrap();
        assert_eq!(
            value["retrieval"],
            serde_json::json!({"arms": ["neighbour"]})
        );
    }

    #[test]
    fn arms_serialise_in_the_canonical_order() {
        let dto = RetrievalDto::from(RetrievalProvenance {
            arms: vec![
                ProvenanceArm::Figure,
                ProvenanceArm::Dense,
                ProvenanceArm::Neighbour,
            ],
            dense_rank: None,
            sparse_rank: None,
            graph_rank: None,
            fused_score: None,
        });
        assert_eq!(
            serde_json::to_value(dto).unwrap(),
            serde_json::json!({"arms": ["dense", "neighbour", "figure"]})
        );
    }

    #[test]
    fn a_member_is_added_to_a_serialised_object() {
        let body = serde_json::to_vec(&serde_json::json!({"a": 1})).unwrap();
        let out = splice_json_member(body, "stats", br#"{"totalTimeMs":2}"#);
        let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(parsed["a"], 1);
        assert_eq!(parsed["stats"]["totalTimeMs"], 2);
    }

    #[test]
    fn an_empty_object_does_not_get_a_leading_comma() {
        let body = serde_json::to_vec(&serde_json::json!({})).unwrap();
        assert_eq!(body, b"{}");
        let out = splice_json_member(body, "stats", br#"{"totalTimeMs":0}"#);
        assert_eq!(out, br#"{"stats":{"totalTimeMs":0}}"#);
        let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(parsed["stats"]["totalTimeMs"], 0);
    }

    #[test]
    fn nested_objects_and_strings_survive_the_splice() {
        let body = serde_json::to_vec(&serde_json::json!({
            "chunks": [{"content": "a } brace, and , commas"}],
            "context": "line\nbreak"
        }))
        .unwrap();
        let out = splice_json_member(body, "stats", br#"{"total":1.5}"#);
        let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(parsed["chunks"][0]["content"], "a } brace, and , commas");
        assert_eq!(parsed["context"], "line\nbreak");
        assert_eq!(parsed["stats"]["total"], 1.5);
    }

    #[test]
    fn the_accounted_stages_add_up_to_at_most_the_total() {
        let stats = StageTimingsDto {
            scope_build: 1.0,
            arms: 200.0,
            serialise: 0.5,
            total: 210.0,
            detail: StageDetailDto {
                engine_total: 205.0,
                vector_arm: 190.0,
                keyword_arm: 12.0,
                query_embed: 150.0,
                ..StageDetailDto::default()
            },
            ..StageTimingsDto::default()
        };
        assert!(stats.accounted_sum_ms() <= stats.total);
        // The two arms together exceed the phase they ran in, because they ran
        // at the same time. That is exactly why the detail is not part of the
        // sum: adding it would report 190 + 12 ms of work inside a 200 ms
        // phase and the total would stop meaning anything.
        assert!(
            stats.detail.vector_arm + stats.detail.keyword_arm > stats.arms,
            "two concurrent arms cost more than their phase, summed"
        );
    }

    #[test]
    fn the_stats_keep_every_bun_field_and_add_one() {
        let stats = SearchStatsDto {
            total_time_ms: 5.0,
            embedding_time_ms: 1.0,
            vector_search_time_ms: 2.0,
            entity_search_time_ms: 0.0,
            graph_search_time_ms: 0.0,
            fusion_time_ms: 0.5,
            vector_results: 2,
            entity_results: 0,
            graph_results: 0,
            total_results: 3,
            stage_timings_ms: StageTimingsDto::empty(),
        };
        let value = serde_json::to_value(&stats).unwrap();
        for key in [
            "totalTimeMs",
            "embeddingTimeMs",
            "vectorSearchTimeMs",
            "entitySearchTimeMs",
            "graphSearchTimeMs",
            "fusionTimeMs",
            "vectorResults",
            "entityResults",
            "graphResults",
            "totalResults",
            "stageTimingsMs",
        ] {
            assert!(value.get(key).is_some(), "{key} is missing");
        }
        assert_eq!(
            value.as_object().map(serde_json::Map::len),
            Some(11),
            "exactly the Bun fields plus stageTimingsMs, nothing renamed away"
        );
    }

    #[test]
    fn the_vision_rerank_is_reported_as_detail_inside_pages() {
        let mut stats = StageTimingsDto {
            pages: 3.0,
            total: 10.0,
            ..StageTimingsDto::default()
        };
        stats.detail.vision_rerank = 2.0;
        let value = serde_json::to_value(stats).unwrap();
        assert_eq!(value["detail"]["visionRerank"], 2.0);
        assert!(
            accounted_pairs(&stats)
                .iter()
                .all(|(name, _)| *name != "visionRerank"),
            "already counted inside pages"
        );
        assert!((stats.accounted_sum_ms() - 3.0).abs() < 1e-9);
    }
}
