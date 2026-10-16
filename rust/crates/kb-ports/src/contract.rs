//! The `RetrievalIndex` contract suite.
//!
//! One set of properties, run against every adapter: the in-memory reference,
//! Qdrant, and the legacy SurrealDB store. This is the file that makes the
//! Surreal → Qdrant move provably safe, and it exists because the two failures
//! we have actually hit were invisible in every other test:
//!
//! * **A scope escape.** `bm25_search_global` had no scope parameter, so another
//!   tenant's chunks were ranked and returned, and the caller filtered them out.
//!   No unit test failed; the red-team leak matrix caught it.
//! * **Keyword AND semantics.** SurrealDB's `@@` needs every query term, so
//!   "Siapakah yang menulis Candy Candy?" returned zero hits and nDCG@10 fell
//!   to 0.414 against 0.744 for an independent BM25. It was a correctness bug
//!   that looked like a quality regression.
//!
//! A property that cannot apply to an adapter is **skipped with a printed
//! reason**, never silently passed. `cargo test -- --nocapture` shows which.
//!
//! # Using it
//!
//! The suite needs a *fresh* index per property, because some properties
//! delete and re-upsert. Pass a factory:
//!
//! ```ignore
//! kb_ports::contract::run_contract(|| async { Ok(InMemoryIndex::new(8)) }).await;
//! ```
//!
//! Adapters that cannot give a fresh index (one live database) should pass a
//! factory that creates a fresh namespace or collection, as the SurrealDB
//! adapter does with its own database.

use kb_core::sparse::encode_document;

use crate::error::PortError;
use crate::ids::{ChunkId, DocumentId, KnowledgeBaseId, TenantId};
use crate::retrieval::{
    Candidate, Fusion, HybridQuery, IndexCapabilities, IndexUnit, KeywordQuery, KeywordSemantics,
    RetrievalIndex,
};
use crate::scope::DocumentScope;
use crate::scope::adapter::ScopeToken;
use crate::types::ChunkType;

/// How one property ended.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// The property held.
    Pass,
    /// The property does not apply to this adapter. Carries the reason so the
    /// run log says why instead of looking like coverage.
    Skipped(String),
    /// The property was violated. Carries the evidence.
    Failed(String),
}

/// One property's result.
#[derive(Debug, Clone)]
pub struct PropertyResult {
    pub name: &'static str,
    pub outcome: Outcome,
}

/// The whole suite's result. Adapters print it and the test fails on any
/// `Failed`.
#[derive(Debug, Clone, Default)]
pub struct ContractReport {
    pub results: Vec<PropertyResult>,
}

impl ContractReport {
    /// Failures only, in property order.
    #[must_use]
    pub fn failures(&self) -> Vec<&PropertyResult> {
        self.results
            .iter()
            .filter(|r| matches!(r.outcome, Outcome::Failed(_)))
            .collect()
    }

    /// Skips with their reasons.
    #[must_use]
    pub fn skips(&self) -> Vec<&PropertyResult> {
        self.results
            .iter()
            .filter(|r| matches!(r.outcome, Outcome::Skipped(_)))
            .collect()
    }

    /// One line per property, for `--nocapture`.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut out = String::new();
        for r in &self.results {
            let line = match &r.outcome {
                Outcome::Pass => "PASS".to_string(),
                Outcome::Skipped(reason) => format!("SKIP ({reason})"),
                Outcome::Failed(reason) => format!("FAIL ({reason})"),
            };
            out.push_str(&format!("  {:<28} {line}\n", r.name));
        }
        out
    }
}

/// A property the suite knows how to run. Public so adapters can run a
/// subset, and so the names are stable enough to grep for.
pub const PROPERTIES: &[&str] = &[
    "scope_containment",
    "keyword_or_semantics",
    "dense_top_k",
    "idempotent_upsert",
    "reingest_leaves_no_stale_tail",
    "deterministic_order",
    "fusion_merges_arms",
    "neighbours_are_adjacent_and_in_scope",
];

/// Expand to the whole dispatch expression: build a fresh index, run the
/// property, turn a construction failure into a failed property rather than a
/// panic. Each arm names its own property so `cargo test -- --nocapture` says
/// which one ran.
macro_rules! dispatch {
    ($factory:expr, $name:expr) => {{
        match $factory().await {
            Ok(index) => match $name {
                "scope_containment" => scope_containment(&index).await,
                "keyword_or_semantics" => keyword_or_semantics(&index).await,
                "dense_top_k" => dense_top_k(&index).await,
                "idempotent_upsert" => idempotent_upsert(&index).await,
                "reingest_leaves_no_stale_tail" => reingest_leaves_no_stale_tail(&index).await,
                "deterministic_order" => deterministic_order(&index).await,
                "fusion_merges_arms" => fusion_merges_arms(&index).await,
                "neighbours_are_adjacent_and_in_scope" => {
                    neighbours_are_adjacent_and_in_scope(&index).await
                }
                other => Outcome::Failed(format!("no such property: {other}")),
            },
            Err(err) => Outcome::Failed(format!("could not build the index: {err}")),
        }
    }};
}

/// Run every property against a fresh index from `factory`, and return the
/// report. The caller decides what to do with failures.
///
/// `factory` is called once per property: several properties delete and
/// re-upsert, so a shared index would leak state between them. A live adapter
/// gives each call its own namespace, collection or database.
pub async fn run_contract<F, Fut, I>(factory: F) -> ContractReport
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<I, PortError>>,
    I: RetrievalIndex,
{
    let mut report = ContractReport::default();
    for &name in PROPERTIES {
        report.results.push(PropertyResult {
            name,
            outcome: dispatch!(&factory, name),
        });
    }
    report
}

// ---------------------------------------------------------------- fixtures

const DENSE_DIMS: usize = 8;

/// A fixture document: which tenant, which KB, which category, and one content
/// string per chunk.
struct FixtureDoc {
    id: &'static str,
    tenant: &'static str,
    kb: Option<&'static str>,
    category: Option<&'static str>,
    contents: &'static [&'static str],
}

/// Two tenants, two knowledge bases, two categories, documents interleaved so
/// that a leak shows up as an out-of-place hit rather than as an empty result.
///
/// `doc-b1_0` deliberately has the same content as the gold keyword unit in
/// tenant A, so a scope escape shows up as a wrong top-1 rather than as nothing.
const FIXTURE_DOCS: &[FixtureDoc] = &[
    FixtureDoc {
        id: "doc-a1",
        tenant: "contract-tenant-a",
        kb: Some("kb-1"),
        category: Some("kategori-1"),
        contents: &[
            // The gold for the Candy Candy question. `candy` twice in a short
            // document is what makes it win on term frequency, which is the
            // point: under OR-of-terms a question must reach the document that
            // is actually about the topic.
            "Candy Candy adalah manga karya Kyoko Mizuki",
            // A distractor on the same term, so top-1 is earned rather than
            // being the only match.
            "Toko permen candy itu berada di ujung jalan besar",
            // The `candi` / `Candy` collision SurrealDB's English stemmer
            // created: under our tokenizer this document scores zero.
            "Candi Borobudur dibangun pada abad kedelapan secara bertahap",
        ],
    },
    FixtureDoc {
        id: "doc-a2",
        tenant: "contract-tenant-a",
        kb: Some("kb-2"),
        category: Some("kategori-2"),
        contents: &[
            "Gula aren adalah pemanis alami dari pohon aren",
            "Menulis laporan tahunan butuh ketelitian dan waktu panjang",
        ],
    },
    FixtureDoc {
        id: "doc-a3",
        tenant: "contract-tenant-a",
        kb: Some("kb-1"),
        category: Some("kategori-2"),
        contents: &[
            "Belajar menulis puisi membutuhkan latihan setiap hari",
            "Metode menulis alternatif dikenal sebagai metode Diana",
            "Kelas menulis profesional dibuka setiap tahun oleh sekolah",
        ],
    },
    // Filler whose only job is to make `yang` and `menulis` ordinary words in
    // this corpus. Without it their IDF is near-maximal on a 14-document scope
    // and any document mentioning them outranks the gold, which would make the
    // property test BM25's behaviour on a toy corpus rather than the port's
    // OR-of-terms semantics. This mirrors the real corpus, where `menulis` and
    // `yang` are common and `candy` is rare.
    FixtureDoc {
        id: "doc-a4",
        tenant: "contract-tenant-a",
        kb: Some("kb-2"),
        category: Some("kategori-1"),
        contents: &[
            "Surat yang panjang dibaca teliti oleh penerima yang sabar",
            "Panduan menulis dokumen resmi tersedia untuk semua warga",
            "Halaman yang sering dibuka memuat informasi berguna",
            "Buku yang tebal biasanya ditulis oleh penulis yang sibuk",
            "Alat yang bagus membuat pekerjaan yang sulit jadi mudah",
            "Tempat yang nyaman adalah syarat belajar yang penting",
        ],
    },
    FixtureDoc {
        id: "doc-b1",
        tenant: "contract-tenant-b",
        kb: Some("kb-9"),
        category: Some("kategori-9"),
        contents: &[
            // Byte-identical to the gold. A scope escape therefore shows up as
            // a wrong top-1 with an identical score, not as a missing result.
            "Candy Candy adalah manga karya Kyoko Mizuki",
            "Dokumen ini milik tenant lain sepenuhnya",
            "Halaman kosong untuk menguji jumlah chunk",
        ],
    },
    FixtureDoc {
        id: "doc-b2",
        tenant: "contract-tenant-b",
        kb: Some("kb-9"),
        category: Some("kategori-9"),
        contents: &["Isi tenant lain dengan istilah candy"],
    },
];

/// A scope over the given fixture document ids.
fn scope_for(tenant: &str, doc_ids: &[&str]) -> DocumentScope {
    let token = ScopeToken::new_for_adapter();
    DocumentScope::new(
        &token,
        TenantId::new_unchecked(tenant),
        doc_ids
            .iter()
            .map(|id| DocumentId::new_unchecked(*id))
            .collect(),
    )
}

/// Every scope shape the fixture supports: a whole tenant, a knowledge-base
/// subset, a category subset, a single document, and the other tenant. The
/// containment property runs all of them rather than one.
fn all_scopes() -> Vec<(&'static str, DocumentScope)> {
    vec![
        ("tenant-a", scope_tenant_a()),
        ("kb-1", scope_kb_1()),
        ("kategori-2", scope_kategori_2()),
        ("single-doc", scope_single_doc()),
        ("tenant-b", scope_tenant_b()),
    ]
}

/// Tenant A's whole scope: 14 units across 4 documents and 2 knowledge bases.
fn scope_tenant_a() -> DocumentScope {
    scope_for(
        "contract-tenant-a",
        &["doc-a1", "doc-a2", "doc-a3", "doc-a4"],
    )
}

/// Tenant A's `kb-1` scope.
fn scope_kb_1() -> DocumentScope {
    scope_for("contract-tenant-a", &["doc-a1", "doc-a3"])
}

/// Tenant A's `kategori-2` scope. Deliberately contains no `candy` document, so
/// a keyword search here must return results from the other terms rather than
/// the gold.
fn scope_kategori_2() -> DocumentScope {
    scope_for("contract-tenant-a", &["doc-a2", "doc-a3"])
}

/// Tenant A's `doc-a2` alone.
fn scope_single_doc() -> DocumentScope {
    scope_for("contract-tenant-a", &["doc-a2"])
}

fn scope_tenant_b() -> DocumentScope {
    scope_for("contract-tenant-b", &["doc-b1", "doc-b2"])
}

/// Build the fixture units. Deterministic dense vectors derived from the unit
/// id, so the exact-top-k property has a ground truth both sides agree on.
fn fixture_units() -> Vec<IndexUnit> {
    let mut units = Vec::new();
    for doc in FIXTURE_DOCS {
        for (index, content) in doc.contents.iter().enumerate() {
            let i = index as u32;
            let unit_id = format!("{}_{}", doc.id, i);
            units.push(IndexUnit {
                id: ChunkId::new_unchecked(unit_id.clone()),
                document_id: DocumentId::new_unchecked(doc.id),
                tenant: TenantId::new_unchecked(doc.tenant),
                knowledge_base_id: doc.kb.map(KnowledgeBaseId::new_unchecked),
                category: doc.category.map(std::string::ToString::to_string),
                chunk_index: i,
                page: Some(i + 1),
                chunk_type: ChunkType::Text,
                content: (*content).to_string(),
                contextual_prefix: Some(format!("{} > chunk {i}", doc.id)),
                metadata: serde_json::json!({ "documentTitle": doc.id, "chunkIndex": i }),
                dense: dense_for(&unit_id),
                // Documents get BM25-saturated tf; a query would get 1.0 each.
                sparse: encode_document(content),
            });
        }
    }
    units
}

/// A deterministic "embedding": unit `i` of the fixture is a unit vector in
/// its own direction. Distinct enough for a top-k comparison, trivial to
/// reproduce in the property.
fn dense_for(id: &str) -> Vec<f32> {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in id.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    let mut v = vec![0.0f32; DENSE_DIMS];
    for (i, slot) in v.iter_mut().enumerate() {
        // Two hashes per slot: one for the value, one for the sign.
        let h = hash.wrapping_add((i as u32).wrapping_mul(0x9e37_79b9));
        let h2 = h.wrapping_mul(0x85eb_ca6b) ^ h.rotate_left(13);
        let magnitude = 0.25 + (f64::from((h % 1000) as u16) / 1000.0) as f32;
        *slot = if h2 & 1 == 0 { magnitude } else { -magnitude };
    }
    v
}

/// A query vector equal to one fixture unit's vector, so the exact answer is
/// "that unit first".
fn dense_query_for(id: &str) -> Vec<f32> {
    dense_for(id)
}

/// The question from `docs/findings-bm25-collapse.md` query 17, and the gold
/// unit its OR-of-terms search must find first.
const CANDY_QUERY: &str = "Siapakah yang menulis Candy Candy?";
const CANDY_GOLD: &str = "doc-a1_0";

// ------------------------------------------------------------- properties

/// Property 1: no method ever returns a unit outside the scope.
///
/// Runs `search` with each arm combination, `fetch` and `neighbours` against
/// every scope shape in the fixture, and checks the tenant as well as the
/// document list — a scope is both.
async fn scope_containment(index: &impl RetrievalIndex) -> Outcome {
    index.upsert(fixture_units()).await.ok();
    let caps = index.capabilities();
    let (dense_query, sparse_query) = hybrid_probe(caps);

    let query_shapes: Vec<(&str, HybridQuery)> = vec![
        (
            "dense-only",
            HybridQuery {
                dense: Some(dense_query.clone()),
                sparse: None,
                per_arm_k: 10,
                k: 10,
                fusion: Fusion::default(),
            },
        ),
        (
            "sparse-only",
            HybridQuery {
                dense: None,
                sparse: Some(sparse_query.clone()),
                per_arm_k: 10,
                k: 10,
                fusion: Fusion::default(),
            },
        ),
        (
            "both",
            HybridQuery {
                dense: Some(dense_query),
                sparse: Some(sparse_query),
                per_arm_k: 10,
                k: 10,
                fusion: Fusion::default(),
            },
        ),
    ];

    for (scope_name, scope) in all_scopes() {
        for (query_name, query) in &query_shapes {
            // A shape the adapter cannot run is not a containment failure.
            if *query_name == "dense-only" && caps.dense_dims == 0 {
                continue;
            }
            if *query_name == "sparse-only" && caps.sparse.is_none() {
                continue;
            }
            let candidates = match index.search(query, &scope).await {
                Ok(c) => c,
                Err(err) => {
                    return Outcome::Failed(format!("search({query_name}) errored: {err}"));
                }
            };
            if let Some(bad) = first_out_of_scope(&candidates, &scope) {
                return Outcome::Failed(format!(
                    "search({query_name}) in scope {scope_name} returned a unit outside it: {bad}"
                ));
            }
        }

        // fetch: in-scope ids come back, out-of-scope ids return nothing at all
        // rather than an error that would confirm existence.
        let in_scope_id = ChunkId::new_unchecked(format!("{}_0", first_doc_of(&scope)));
        match index
            .fetch(&scope, std::slice::from_ref(&in_scope_id))
            .await
        {
            Ok(_) => {}
            Err(err) => return Outcome::Failed(format!("fetch(in-scope) errored: {err}")),
        }
        // The out-of-scope id has to be chosen against *this* scope. Hardcoding
        // one id would make the tenant-B run fetch a document that tenant B can
        // see, and the property would be asserting nothing.
        let Some((foreign_id, foreign_document)) = out_of_scope_probe(&scope) else {
            return Outcome::Skipped(format!(
                "scope {scope_name} sees every fixture document, so there is no out-of-scope \
                 id to probe"
            ));
        };
        match index.fetch(&scope, std::slice::from_ref(&foreign_id)).await {
            Ok(rows) => {
                if !rows.is_empty() {
                    return Outcome::Failed(format!(
                        "fetch of {foreign_id} — outside scope {scope_name} — returned {} row(s)",
                        rows.len()
                    ));
                }
            }
            Err(err) => {
                return Outcome::Failed(format!(
                    "fetch of {foreign_id}, outside scope {scope_name}, leaked existence via an \
                     error: {err}"
                ));
            }
        }

        // neighbours: a scope never reaches into a document it cannot see.
        match index.neighbours(&scope, &foreign_document, &[1]).await {
            Ok(rows) => {
                if !rows.is_empty() {
                    return Outcome::Failed(format!(
                        "neighbours of {} in scope {scope_name} returned {} row(s); it is not in \
                         the scope",
                        foreign_document.as_str(),
                        rows.len()
                    ));
                }
            }
            Err(err) => {
                return Outcome::Failed(format!(
                    "neighbours of {} in scope {scope_name} errored: {err}",
                    foreign_document.as_str()
                ));
            }
        }
    }

    // An empty scope must short-circuit to nothing, not to "everything".
    let empty = scope_for("contract-tenant-a", &[]);
    match index
        .search(
            &HybridQuery {
                dense: Some(dense_query_for(CANDY_GOLD)),
                sparse: Some(KeywordQuery::from_text(CANDY_QUERY)),
                per_arm_k: 10,
                k: 10,
                fusion: Fusion::default(),
            },
            &empty,
        )
        .await
    {
        Ok(rows) if rows.is_empty() => {}
        Ok(rows) => {
            return Outcome::Failed(format!(
                "an empty scope returned {} row(s); it must short-circuit to none",
                rows.len()
            ));
        }
        Err(err) => return Outcome::Failed(format!("an empty scope errored: {err}")),
    }

    Outcome::Pass
}

/// Property 2: a natural-language question finds the document that is about
/// the topic.
///
/// Only meaningful for OR-of-terms adapters. An AND-semantics adapter skips
/// with the reason, because that failure is already documented and measured
/// (`docs/findings-bm25-collapse.md`), not something to re-litigate per run.
async fn keyword_or_semantics(index: &impl RetrievalIndex) -> Outcome {
    let caps = index.capabilities();
    match caps.sparse {
        Some(KeywordSemantics::AnyTerm) => {}
        Some(KeywordSemantics::AllTerms) => {
            return Outcome::Skipped(
                "adapter declares AllTerms keyword semantics (SurrealDB `@@` is boolean AND), \
                 so an OR-of-terms question is not a property it can hold"
                    .to_string(),
            );
        }
        None => {
            return Outcome::Skipped(
                "adapter has no keyword arm, so keyword semantics cannot be observed".to_string(),
            );
        }
    }

    index.upsert(fixture_units()).await.ok();
    let scope = scope_tenant_a();
    let results = match index
        .search(
            &HybridQuery {
                dense: None,
                sparse: Some(KeywordQuery::from_text(CANDY_QUERY)),
                per_arm_k: 10,
                k: 5,
                fusion: Fusion::None,
            },
            &scope,
        )
        .await
    {
        Ok(rows) => rows,
        Err(err) => return Outcome::Failed(format!("sparse search errored: {err}")),
    };

    if results.is_empty() {
        return Outcome::Failed(format!(
            "no keyword hit at all for {CANDY_QUERY:?}: the question's terms ('menulis', \
             'candy') are present in the corpus, so an empty result is a failure, not an \
             empty index"
        ));
    }
    let top = &results[0];
    if top.unit_id.as_str() != CANDY_GOLD {
        let found: Vec<&str> = results.iter().map(|c| c.unit_id.as_str()).collect();
        return Outcome::Failed(format!(
            "top keyword hit for {CANDY_QUERY:?} was {}, expected {CANDY_GOLD}; got {found:?}",
            top.unit_id.as_str()
        ));
    }
    if top.scores.sparse.is_none() {
        return Outcome::Failed("the keyword hit carried no sparse score".to_string());
    }
    Outcome::Pass
}

/// Property 3: the dense arm's top-k is the true top-k.
///
/// An adapter that declares `approximate_dense` gets recall@5 ≥ 0.8 instead of
/// an exact match, because an HNSW graph is allowed to miss a little and the
/// plan's target is "under 1 point of recall lost".
async fn dense_top_k(index: &impl RetrievalIndex) -> Outcome {
    let caps = index.capabilities();
    if caps.dense_dims == 0 {
        return Outcome::Skipped("adapter has no dense arm".to_string());
    }
    index.upsert(fixture_units()).await.ok();
    let scope = scope_tenant_a();
    let query_vec = dense_query_for("doc-a2_1");

    let got = match index
        .search(
            &HybridQuery {
                dense: Some(query_vec.clone()),
                sparse: None,
                per_arm_k: 5,
                k: 5,
                fusion: Fusion::None,
            },
            &scope,
        )
        .await
    {
        Ok(rows) => rows,
        Err(err) => return Outcome::Failed(format!("dense search errored: {err}")),
    };

    // Brute force over exactly the same fixture, same scope, same vectors.
    let mut truth: Vec<(String, f32)> = fixture_units()
        .iter()
        .filter(|u| u.tenant == *scope.tenant() && scope.document_ids().contains(&u.document_id))
        .map(|u| (u.id.as_str().to_string(), cosine(&u.dense, &query_vec)))
        .collect();
    truth.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    let expected: Vec<String> = truth.iter().take(5).map(|(id, _)| id.clone()).collect();

    let got_ids: Vec<String> = got.iter().map(|c| c.unit_id.as_str().to_string()).collect();
    if got_ids.len() != expected.len() {
        return Outcome::Failed(format!(
            "dense top-k returned {} candidates, expected {}",
            got_ids.len(),
            expected.len()
        ));
    }
    if got_ids == expected {
        return Outcome::Pass;
    }
    if caps.approximate_dense {
        let hits = got_ids.iter().filter(|id| expected.contains(id)).count();
        let recall = hits as f32 / expected.len() as f32;
        if recall >= 0.8 {
            return Outcome::Pass;
        }
        return Outcome::Failed(format!(
            "approximate dense recall@5 was {recall:.2} (need >= 0.80): expected {expected:?}, got {got_ids:?}"
        ));
    }
    Outcome::Failed(format!(
        "adapter declares an exact dense arm but its top-5 differs from brute force: \
         expected {expected:?}, got {got_ids:?}"
    ))
}

/// Property 4: upserting the same id twice leaves one unit, holding the last
/// write.
async fn idempotent_upsert(index: &impl RetrievalIndex) -> Outcome {
    let mut first = fixture_units();
    first.retain(|u| u.id.as_str() == "doc-a1_0");
    index.upsert(first).await.ok();

    let mut second: Vec<IndexUnit> = fixture_units()
        .into_iter()
        .filter(|u| u.id.as_str() == "doc-a1_0")
        .collect();
    for u in &mut second {
        u.content = "Candy Candy adalah manga karya Kyoko Mizuki revisi".to_string();
    }
    index.upsert(second).await.ok();

    let scope = scope_tenant_a();
    let rows = match index
        .fetch(&scope, &[ChunkId::new_unchecked("doc-a1_0")])
        .await
    {
        Ok(rows) => rows,
        Err(err) => return Outcome::Failed(format!("fetch errored: {err}")),
    };
    if rows.len() != 1 {
        return Outcome::Failed(format!(
            "two upserts of the same id left {} rows, expected 1",
            rows.len()
        ));
    }
    if !rows[0].content.contains("revisi") {
        return Outcome::Failed(format!(
            "the surviving row holds the first write, not the last: {:?}",
            rows[0].content
        ));
    }
    Outcome::Pass
}

/// Property 5: re-ingesting a shorter document leaves no stale tail, and
/// deleting a document in one tenant never touches another tenant's units at
/// the same position.
async fn reingest_leaves_no_stale_tail(index: &impl RetrievalIndex) -> Outcome {
    index.upsert(fixture_units()).await.ok();

    // doc-a1 has three chunks; re-ingest it with one.
    index
        .delete_document(
            &TenantId::new_unchecked("contract-tenant-a"),
            &DocumentId::new_unchecked("doc-a1"),
        )
        .await
        .ok();
    let mut shorter: Vec<IndexUnit> = fixture_units()
        .into_iter()
        .filter(|u| u.document_id.as_str() == "doc-a1")
        .take(1)
        .collect();
    for u in &mut shorter {
        u.content = "Candy Candy adalah manga karya Kyoko Mizuki".to_string();
    }
    index.upsert(shorter).await.ok();

    let scope = scope_kb_1();
    let rows = match index
        .fetch(
            &scope,
            &[
                ChunkId::new_unchecked("doc-a1_0"),
                ChunkId::new_unchecked("doc-a1_1"),
                ChunkId::new_unchecked("doc-a1_2"),
            ],
        )
        .await
    {
        Ok(rows) => rows,
        Err(err) => return Outcome::Failed(format!("fetch errored: {err}")),
    };
    if rows.len() != 1 || rows[0].unit_id.as_str() != "doc-a1_0" {
        let ids: Vec<&str> = rows.iter().map(|c| c.unit_id.as_str()).collect();
        return Outcome::Failed(format!(
            "after re-ingesting doc-a1 with one chunk, its units are {ids:?}; expected [\"doc-a1_0\"]"
        ));
    }

    // Tenant isolation on delete: doc-b1 sits at the same chunk positions.
    let before = match index
        .fetch(
            &scope_tenant_b(),
            &[
                ChunkId::new_unchecked("doc-b1_0"),
                ChunkId::new_unchecked("doc-b1_1"),
            ],
        )
        .await
    {
        Ok(rows) => rows.len(),
        Err(err) => return Outcome::Failed(format!("fetch errored: {err}")),
    };
    if before != 2 {
        return Outcome::Failed(format!(
            "tenant B had {before} units before the delete, expected 2; the fixture is wrong"
        ));
    }
    let removed = match index
        .delete_document(
            &TenantId::new_unchecked("contract-tenant-b"),
            &DocumentId::new_unchecked("doc-b1"),
        )
        .await
    {
        Ok(n) => n,
        Err(err) => return Outcome::Failed(format!("delete_document errored: {err}")),
    };
    if removed == 0 {
        return Outcome::Failed("delete_document reported removing nothing".to_string());
    }
    let after = match index
        .fetch(
            &scope_tenant_a(),
            &[
                ChunkId::new_unchecked("doc-b1_0"),
                ChunkId::new_unchecked("doc-b1_1"),
            ],
        )
        .await
    {
        Ok(rows) => rows.len(),
        Err(err) => return Outcome::Failed(format!("fetch errored: {err}")),
    };
    if after != 0 {
        return Outcome::Failed(format!(
            "deleting doc-b1 for tenant B left {after} of its units readable from any scope; \
             the delete is not tenant-scoped"
        ));
    }
    // Tenant A's units must be untouched by a tenant-B delete.
    let a_rows = match index
        .fetch(
            &scope_tenant_a(),
            &[
                ChunkId::new_unchecked("doc-a1_0"),
                ChunkId::new_unchecked("doc-a2_0"),
                ChunkId::new_unchecked("doc-a3_0"),
            ],
        )
        .await
    {
        Ok(rows) => rows.len(),
        Err(err) => return Outcome::Failed(format!("fetch errored: {err}")),
    };
    if a_rows != 3 {
        return Outcome::Failed(format!(
            "a tenant-B delete changed tenant A's unit count to {a_rows}, expected 3"
        ));
    }
    Outcome::Pass
}

/// Property 6: the same query, run repeatedly, returns the same candidates in
/// the same order.
///
/// A store that iterates a hash map, or that ranks ties by whatever the index
/// returned first, fails here.
async fn deterministic_order(index: &impl RetrievalIndex) -> Outcome {
    index.upsert(fixture_units()).await.ok();
    let caps = index.capabilities();
    let (dense_query, sparse_query) = hybrid_probe(caps);
    let scope = scope_tenant_a();

    let mut queries = vec![HybridQuery {
        dense: None,
        sparse: Some(sparse_query.clone()),
        per_arm_k: 10,
        k: 10,
        fusion: Fusion::default(),
    }];
    if caps.dense_dims > 0 {
        queries.push(HybridQuery {
            dense: Some(dense_query.clone()),
            sparse: Some(sparse_query),
            per_arm_k: 10,
            k: 10,
            fusion: Fusion::default(),
        });
    }
    for query in queries {
        let mut previous: Option<Vec<String>> = None;
        for run in 0..3 {
            let rows = match index.search(&query, &scope).await {
                Ok(rows) => rows,
                Err(err) => return Outcome::Failed(format!("search errored on run {run}: {err}")),
            };
            let ids: Vec<String> = rows
                .iter()
                .map(|c| c.unit_id.as_str().to_string())
                .collect();
            if let Some(first) = &previous
                && *first != ids
            {
                return Outcome::Failed(format!(
                    "run {run} returned {ids:?}, the first run returned {first:?}"
                ));
            }
            previous = Some(ids);
        }
    }
    Outcome::Pass
}

/// Property 7: a unit both arms find appears once, with both scores set, and
/// outranks a unit only one arm found at the same ranks.
///
/// The old engine assigned both arms' rank-1 hits the same score and let
/// insertion order decide, which handed rank 1 to the vector arm every time.
async fn fusion_merges_arms(index: &impl RetrievalIndex) -> Outcome {
    let caps = index.capabilities();
    if caps.dense_dims == 0 || caps.sparse.is_none() {
        return Outcome::Skipped("needs both a dense and a keyword arm".to_string());
    }
    index.upsert(fixture_units()).await.ok();
    let scope = scope_tenant_a();

    // doc-a1_0 is the gold for the candy question *and* carries a dense vector,
    // so craft a dense query that lands on doc-a1_1 instead: a unit the
    // keyword arm also ranks first.
    let query = HybridQuery {
        dense: Some(dense_query_for("doc-a1_0")),
        sparse: Some(KeywordQuery::from_text(CANDY_QUERY)),
        per_arm_k: 10,
        k: 10,
        fusion: Fusion::Rrf { k: 60 },
    };
    let rows = match index.search(&query, &scope).await {
        Ok(rows) => rows,
        Err(err) => return Outcome::Failed(format!("hybrid search errored: {err}")),
    };
    if rows.len() < 2 {
        return Outcome::Failed(format!(
            "the hybrid query returned {} candidates; there is nothing to merge",
            rows.len()
        ));
    }

    let mut ids: Vec<&str> = Vec::new();
    for row in &rows {
        if ids.contains(&row.unit_id.as_str()) {
            return Outcome::Failed(format!(
                "{} appears more than once in the fused result",
                row.unit_id.as_str()
            ));
        }
        ids.push(row.unit_id.as_str());
    }

    let both: Vec<&Candidate> = rows
        .iter()
        .filter(|c| c.scores.dense.is_some() && c.scores.sparse.is_some())
        .collect();
    if both.is_empty() {
        return Outcome::Failed(
            "no candidate was found by both arms, so the merge cannot be observed".to_string(),
        );
    }
    let top_both = both
        .iter()
        .max_by(|a, b| {
            a.scores
                .fused
                .partial_cmp(&b.scores.fused)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .copied();
    if let Some(top) = top_both {
        if top.scores.dense.is_none() || top.scores.sparse.is_none() {
            return Outcome::Failed("a merged candidate lost one of its arm scores".to_string());
        }
        if top.ranks.dense.is_none() || top.ranks.sparse.is_none() {
            return Outcome::Failed("a merged candidate lost one of its arm ranks".to_string());
        }
    }

    // A candidate one arm found at rank 0 must not outrank a candidate both
    // arms found at the same ranks: two arms agreeing is evidence.
    let single_best = rows
        .iter()
        .filter(|c| {
            (c.scores.dense.is_some()) ^ (c.scores.sparse.is_some())
                && (c.ranks.dense == Some(0) || c.ranks.sparse == Some(0))
        })
        .map(|c| c.scores.fused)
        .fold(f32::NEG_INFINITY, f32::max);
    if single_best.is_finite()
        && let Some(top) = top_both
        && single_best > top.scores.fused
    {
        return Outcome::Failed(format!(
            "a single-arm rank-0 hit fused to {single_best}, above the both-arms candidate at {}",
            top.scores.fused
        ));
    }
    Outcome::Pass
}

/// Property 8: neighbours are adjacent chunks of the same document, in scope
/// only.
async fn neighbours_are_adjacent_and_in_scope(index: &impl RetrievalIndex) -> Outcome {
    index.upsert(fixture_units()).await.ok();
    let scope = scope_tenant_a();

    // doc-a1 has chunks 0..2. Anchoring on 1 must give 0 and 2, both from
    // doc-a1, both in the scope.
    let rows = match index
        .neighbours(&scope, &DocumentId::new_unchecked("doc-a1"), &[1])
        .await
    {
        Ok(rows) => rows,
        Err(err) => return Outcome::Failed(format!("neighbours errored: {err}")),
    };
    if rows.is_empty() {
        return Outcome::Failed(
            "neighbours of chunk 1 in doc-a1 returned nothing; doc-a1 has chunks 0 and 2"
                .to_string(),
        );
    }
    for row in &rows {
        if row.document_id.as_str() != "doc-a1" {
            return Outcome::Failed(format!(
                "neighbours of doc-a1 returned a chunk of {}",
                row.document_id.as_str()
            ));
        }
        // doc-a1's chunks are 0, 1 and 2. The anchor is 1, so the only legal
        // neighbours are 0 and 2 — anything else means the adapter ignored the
        // adjacency bound and returned the document.
        if row.chunk_index != 0 && row.chunk_index != 2 {
            return Outcome::Failed(format!(
                "neighbours of chunk 1 returned chunk {}; only 0 and 2 are adjacent",
                row.chunk_index
            ));
        }
        if row.tenant != *scope.tenant() {
            return Outcome::Failed(format!(
                "a neighbour came from tenant {} in a scope for {}",
                row.tenant,
                scope.tenant()
            ));
        }
    }
    let got: Vec<u32> = rows.iter().map(|r| r.chunk_index).collect();
    if got != vec![0, 2] {
        return Outcome::Failed(format!(
            "neighbours of chunk 1 returned {got:?}, expected [0, 2] in ascending order"
        ));
    }

    // Anchoring at index 0 must reach forward only: chunk 0 has no predecessor,
    // and returning one would mean the adapter wrapped or filtered wrongly.
    let edge = match index
        .neighbours(&scope, &DocumentId::new_unchecked("doc-a1"), &[0])
        .await
    {
        Ok(rows) => rows,
        Err(err) => return Outcome::Failed(format!("neighbours errored: {err}")),
    };
    let edge_indices: Vec<u32> = edge.iter().map(|r| r.chunk_index).collect();
    if edge_indices != vec![1] {
        return Outcome::Failed(format!(
            "neighbours of chunk 0 returned {edge_indices:?}, expected [1]"
        ));
    }

    // An anchor past the end of a document must reach nothing at all.
    let none = match index
        .neighbours(&scope, &DocumentId::new_unchecked("doc-a1"), &[99])
        .await
    {
        Ok(rows) => rows,
        Err(err) => return Outcome::Failed(format!("neighbours errored: {err}")),
    };
    if !none.is_empty() {
        return Outcome::Failed(format!(
            "anchoring on chunk 99 of a three-chunk document returned {} row(s)",
            none.len()
        ));
    }
    Outcome::Pass
}

// ------------------------------------------------------------------ helpers

/// The first document id in a scope, used to pick an in-scope id for `fetch`.
fn first_doc_of(scope: &DocumentScope) -> String {
    scope
        .document_ids()
        .first()
        .map_or_else(|| "none".to_string(), |d| d.as_str().to_string())
}

/// The first fixture unit that this scope must not see, as `(unit id, document
/// id)`.
///
/// Chosen against the scope rather than hardcoded: a probe id that the scope
/// happens to contain asserts nothing, and the tenant-B leg of the containment
/// property would silently pass. `None` when the scope sees everything, which
/// is not the case for any fixture scope but is handled rather than assumed.
fn out_of_scope_probe(scope: &DocumentScope) -> Option<(ChunkId, DocumentId)> {
    FIXTURE_DOCS.iter().find_map(|doc| {
        if scope
            .document_ids()
            .contains(&DocumentId::new_unchecked(doc.id))
            && doc.tenant == scope.tenant().as_str()
        {
            return None;
        }
        Some((
            ChunkId::new_unchecked(format!("{}_0", doc.id)),
            DocumentId::new_unchecked(doc.id),
        ))
    })
}

/// A dense and a sparse probe query, using whichever arms the adapter has.
fn hybrid_probe(caps: IndexCapabilities) -> (Vec<f32>, KeywordQuery) {
    let dense = dense_query_for("doc-a1_0");
    let _ = caps;
    (dense, KeywordQuery::from_text(CANDY_QUERY))
}

/// The first candidate that is not visible in `scope`, described.
fn first_out_of_scope(candidates: &[Candidate], scope: &DocumentScope) -> Option<String> {
    for c in candidates {
        let in_docs = scope.document_ids().contains(&c.document_id);
        let in_tenant = c.tenant == *scope.tenant();
        if !in_docs || !in_tenant {
            return Some(format!(
                "{} (document {}, tenant {}; scope has tenant {} and {} document(s))",
                c.unit_id.as_str(),
                c.document_id.as_str(),
                c.tenant,
                scope.tenant(),
                scope.document_ids().len()
            ));
        }
    }
    None
}

/// Cosine similarity. Zero-length vectors score 0 rather than NaN, so an adapter
/// that stores a unit without a dense vector cannot poison the ordering.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..n {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}
