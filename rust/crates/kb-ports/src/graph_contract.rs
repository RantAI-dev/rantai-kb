//! The `GraphStore` contract suite.
//!
//! One set of properties, run against every adapter: the in-memory reference and
//! the Postgres one. It exists because the failure this port is most exposed to is
//! invisible in a normal test — a scope that filters results but not edges. The
//! retrieval port already has a suite for exactly this reason
//! (`kb_ports::contract`), and this is the same idea applied to the graph.
//!
//! # The properties
//!
//! * `scope_containment` — no read ever returns an entity, edge or mention from
//!   outside the scope, and **no path through the graph crosses an out-of-scope
//!   document**. Two entities in the caller's documents, connected only by a
//!   relation from a document the caller cannot see, are not connected in the
//!   scoped graph. This is the one that fails silently when it fails.
//! * `tenant_isolation` — the same entity key in two tenants is two facts, and
//!   neither is visible from the other.
//! * `idempotent_upsert_and_dedupe` — re-ingesting the same document changes
//!   nothing; two documents mentioning the same key make one entity with two
//!   mentions; aliases merge rather than replace.
//! * `delete_document_cascades` — mentions, relations and orphaned entities go;
//!   another document's data and another tenant's data stay.
//! * `version_bumps_on_every_write` — every write shape moves the counter,
//!   including one that changes nothing, because a cache that is not invalidated
//!   reads as "the answer changed and nobody knows why".
//! * `deterministic_ordering` — the same writes produce the same load, byte for
//!   byte, twice in a row and on a fresh store.
//!
//! A property that cannot apply to an adapter is **skipped with a printed
//! reason**, never silently passed — the same rule as the retrieval suite, because
//! a skip that looks like a pass is how a suite quietly stops testing.
//!
//! # Using it
//!
//! The suite needs a *fresh* store per property: several delete, re-ingest and
//! bump versions, so a shared store leaks state between properties. Adapters that
//! cannot give a fresh store (one live database) pass a factory that isolates by
//! tenant, as the Postgres adapter does with a per-run tenant id.
//!
//! ```ignore
//! kb_ports::graph_contract::run_contract(|| async { Ok(InMemoryGraphStore::new()) }).await;
//! ```

use std::collections::BTreeSet;

use kb_core::graph::WalkOptions;

use crate::error::PortError;
use crate::graph::{EntityUpsert, GraphStore, RelationUpsert, TenantGraph};
use crate::ids::{ChunkId, DocumentId, TenantId};
use crate::scope::DocumentScope;
use crate::scope::adapter::ScopeToken;

/// Tenant A's documents. `doc-a1` and `doc-a2` are in A's visible scope;
/// `doc-a3` exists in the tenant but is not in that scope (a knowledge-base or
/// category filter excluded it), and `doc-b1` belongs to tenant B.
const TENANT_A: &str = "contract-tenant-a";
const TENANT_B: &str = "contract-tenant-b";
const DOC_A1: &str = "doc-a1";
const DOC_A2: &str = "doc-a2";
/// In the tenant, deliberately outside `scope_a`.
const DOC_A3: &str = "doc-a3";
const DOC_B1: &str = "doc-b1";

/// Every property in the suite, in order. Public so an adapter can run a subset
/// and so the names are stable enough to grep for.
pub const PROPERTIES: &[&str] = &[
    "scope_containment",
    "no_path_through_an_out_of_scope_document",
    "tenant_isolation",
    "idempotent_upsert_and_dedupe",
    "delete_document_cascades",
    "version_bumps_on_every_write",
    "deterministic_ordering",
];

/// How one property ended.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// The property held.
    Pass,
    /// The property does not apply to this adapter. Carries the reason so the run
    /// log says why instead of looking like coverage.
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

/// The whole suite's result. Adapters print it and the test fails on any `Failed`.
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
            out.push_str(&format!("  {:<40} {line}\n", r.name));
        }
        out
    }
}

/// Expand to the dispatch expression: build a fresh store, run the property, turn
/// a construction failure into a failed property rather than a panic. Each arm
/// names its own property so `cargo test -- --nocapture` says which one ran.
macro_rules! dispatch {
    ($factory:expr, $name:expr) => {{
        match $factory().await {
            Ok(store) => match $name {
                "scope_containment" => scope_containment(&store).await,
                "no_path_through_an_out_of_scope_document" => {
                    no_path_through_an_out_of_scope_document(&store).await
                }
                "tenant_isolation" => tenant_isolation(&store).await,
                "idempotent_upsert_and_dedupe" => idempotent_upsert_and_dedupe(&store).await,
                "delete_document_cascades" => delete_document_cascades(&store).await,
                "version_bumps_on_every_write" => version_bumps_on_every_write(&store).await,
                "deterministic_ordering" => deterministic_ordering(&store).await,
                other => Outcome::Failed(format!("no such property: {other}")),
            },
            Err(err) => Outcome::Failed(format!("could not build the store: {err}")),
        }
    }};
}

/// Run every property against a fresh store from `factory`, and return the report.
/// The caller decides what to do with failures.
pub async fn run_contract<F, Fut, G>(factory: F) -> ContractReport
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<G, PortError>>,
    G: GraphStore,
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

/// Tenant A's visible documents. `doc-a3` is in the tenant but out of scope.
fn scope_a() -> DocumentScope {
    scope_for(TENANT_A, &[DOC_A1, DOC_A2])
}

/// Tenant A's whole tenant, as if no filter were applied: three documents.
fn scope_a_all() -> DocumentScope {
    scope_for(TENANT_A, &[DOC_A1, DOC_A2, DOC_A3])
}

fn scope_b() -> DocumentScope {
    scope_for(TENANT_B, &[DOC_B1])
}

fn tenant_a() -> TenantId {
    TenantId::new_unchecked(TENANT_A)
}

fn tenant_b() -> TenantId {
    TenantId::new_unchecked(TENANT_B)
}

fn doc(id: &str) -> DocumentId {
    DocumentId::new_unchecked(id)
}

fn chunk(id: &str) -> ChunkId {
    ChunkId::new_unchecked(id)
}

fn entity(key: &str, name: &str, mentions: &[&str]) -> EntityUpsert {
    EntityUpsert {
        key: key.to_string(),
        name: name.to_string(),
        entity_type: "person".to_string(),
        aliases: Vec::new(),
        mentions: mentions.iter().map(|c| chunk(c)).collect(),
    }
}

fn relation(src: &str, dst: &str, chunk_id: Option<&str>) -> RelationUpsert {
    RelationUpsert {
        src_key: src.to_string(),
        dst_key: dst.to_string(),
        relation_type: "MENTIONED_WITH".to_string(),
        weight: 1.0,
        chunk_id: chunk_id.map(chunk),
    }
}

/// The fixture: two tenants, four documents, a deliberate cross-document bridge.
///
/// ```text
/// tenant A   doc-a1: candy ── manga ── candy-candy      (Kyoko Mizuki, genres)
///            doc-a2: candy ── candies  (a candy shop)    ── the shop is only here
///            doc-a3: manga ── anime                      ── OUT OF SCOPE for scope_a
/// tenant B   doc-b1: candy ── manga                      ── byte-identical key names
/// ```
///
/// `candy` and `manga` are mentioned in two in-scope documents, `candies` in one,
/// `anime` only in the out-of-scope document. Every property reads this.
async fn seed(store: &impl GraphStore) -> Result<(), PortError> {
    store
        .upsert_entities(
            &tenant_a(),
            &doc(DOC_A1),
            vec![
                entity("candy", "Candy Candy", &["doc-a1_0"]),
                entity("manga", "Manga", &["doc-a1_0", "doc-a1_1"]),
                entity("candy-candy", "Candy Candy", &["doc-a1_1"]),
                entity("kyoko", "Kyoko Mizuki", &["doc-a1_1"]),
            ],
        )
        .await?;
    store
        .relate(
            &tenant_a(),
            &doc(DOC_A1),
            vec![
                relation("candy", "manga", Some("doc-a1_0")),
                relation("candy-candy", "manga", Some("doc-a1_1")),
                relation("candy", "kyoko", Some("doc-a1_1")),
            ],
        )
        .await?;

    // `candies` appears only here. It is the bridge: see
    // `no_path_through_an_out_of_scope_document`.
    store
        .upsert_entities(
            &tenant_a(),
            &doc(DOC_A2),
            vec![
                entity("candies", "Toko Permen", &["doc-a2_0"]),
                entity("manga", "Manga", &["doc-a2_0"]),
                entity("shop", "Toko", &["doc-a2_1"]),
            ],
        )
        .await?;
    store
        .relate(
            &tenant_a(),
            &doc(DOC_A2),
            vec![
                relation("candies", "manga", Some("doc-a2_0")),
                relation("candies", "shop", Some("doc-a2_1")),
            ],
        )
        .await?;

    // Out of scope for scope_a.
    store
        .upsert_entities(
            &tenant_a(),
            &doc(DOC_A3),
            vec![
                entity("anime", "Anime", &["doc-a3_0"]),
                entity("manga", "Manga", &["doc-a3_0"]),
            ],
        )
        .await?;
    store
        .relate(
            &tenant_a(),
            &doc(DOC_A3),
            vec![relation("anime", "manga", Some("doc-a3_0"))],
        )
        .await?;

    // The other tenant, same keys on purpose.
    store
        .upsert_entities(
            &tenant_b(),
            &doc(DOC_B1),
            vec![
                entity("candy", "Permen Lain Tenant", &["doc-b1_0"]),
                entity("manga", "Manga Tenant Lain", &["doc-b1_0"]),
            ],
        )
        .await?;
    store
        .relate(
            &tenant_b(),
            &doc(DOC_B1),
            vec![relation("candy", "manga", Some("doc-b1_0"))],
        )
        .await?;
    Ok(())
}

/// Entity keys present in a loaded graph, sorted.
fn keys_of(graph: &TenantGraph) -> Vec<String> {
    graph.map().keys().to_vec()
}

/// One-hop neighbours of a key, as keys, sorted.
fn neighbours(graph: &TenantGraph, key: &str) -> Vec<String> {
    let Some(id) = graph.map().id_of(key) else {
        return Vec::new();
    };
    let mut out: Vec<String> = graph
        .csr()
        .neighbours(id)
        .into_iter()
        .filter_map(|(target, _)| graph.map().key_of(target).map(str::to_string))
        .collect();
    out.sort();
    out
}

// ------------------------------------------------------------- properties

/// Property 1: every scoped read stays inside the scope.
///
/// Runs each scoped read against every scope shape the fixture supports — the
/// filtered scope, the whole tenant, the other tenant, a single document, and an
/// empty one — and checks the tenant as well as the document list, because a scope
/// is both.
async fn scope_containment(store: &impl GraphStore) -> Outcome {
    if let Err(err) = seed(store).await {
        return Outcome::Failed(format!("could not seed the fixture: {err}"));
    }

    let scopes: Vec<(&str, DocumentScope)> = vec![
        ("filtered-a", scope_a()),
        ("all-a", scope_a_all()),
        ("tenant-b", scope_b()),
        ("single-doc", scope_for(TENANT_A, &[DOC_A1])),
        ("empty", scope_for(TENANT_A, &[])),
    ];

    for (scope_name, scope) in &scopes {
        let graph = match store.load_tenant_graph(scope).await {
            Ok(graph) => graph,
            Err(err) => return Outcome::Failed(format!("load_tenant_graph({scope_name}): {err}")),
        };

        let expected: BTreeSet<&str> = scope.document_ids().iter().map(|id| id.as_str()).collect();

        for mention in graph.mentions() {
            for chunk_id in &mention.chunk_ids {
                let document = document_of_chunk(chunk_id.as_str());
                if !expected.contains(document) {
                    return Outcome::Failed(format!(
                        "scope {scope_name}: entity `{}` carries a mention in {document}, outside the scope",
                        mention.key
                    ));
                }
            }
        }

        // An entity is in the tenant, not in a document. The only thing that puts
        // it into a scoped graph is evidence from an in-scope document, so an
        // entity with no in-scope mention is one that got there through an
        // out-of-scope relation — the leak this suite exists for. An empty scope
        // must produce an empty graph for the same reason.
        for key in graph.map().keys() {
            if graph.chunks_for(key).is_empty() {
                return Outcome::Failed(format!(
                    "scope {scope_name}: entity `{key}` is in the scoped graph with no in-scope \
                     mention, so it reached the graph through an out-of-scope document"
                ));
            }
        }

        if expected.is_empty() && graph.entity_count() != 0 {
            return Outcome::Failed(format!(
                "scope {scope_name}: an empty scope returned {} entities",
                graph.entity_count()
            ));
        }

        // match_entities is scoped: every key it returns must be in the scoped
        // graph, and its mentions must be in scope.
        let matched = match store
            .match_entities(scope, &["candy".to_string(), "manga".to_string()])
            .await
        {
            Ok(m) => m,
            Err(err) => return Outcome::Failed(format!("match_entities({scope_name}): {err}")),
        };
        for (key, _) in &matched {
            if graph.map().id_of(key).is_none() {
                return Outcome::Failed(format!(
                    "scope {scope_name}: match_entities returned `{key}`, which is not in the scoped graph"
                ));
            }
        }

        // entity_mentions is scoped per key, including a key the caller cannot see.
        for key in graph.map().keys() {
            let mentions = match store.entity_mentions(scope, key).await {
                Ok(mentions) => mentions,
                Err(err) => {
                    return Outcome::Failed(format!("entity_mentions({scope_name}): {err}"));
                }
            };
            if mentions.is_empty() {
                return Outcome::Failed(format!(
                    "scope {scope_name}: `{key}` is in the graph but entity_mentions returned nothing"
                ));
            }
            for chunk_id in &mentions {
                let document = document_of_chunk(chunk_id.as_str());
                if !expected.contains(document) {
                    return Outcome::Failed(format!(
                        "scope {scope_name}: entity_mentions({key}) returned {document}, outside the scope"
                    ));
                }
            }
        }
        for invisible in ["anime", "kyoko"] {
            if graph.map().id_of(invisible).is_none() && !expected.contains(DOC_A3) {
                let mentions = store
                    .entity_mentions(scope, invisible)
                    .await
                    .unwrap_or_default();
                if !mentions.is_empty() {
                    return Outcome::Failed(format!(
                        "scope {scope_name}: entity_mentions({invisible}) returned {} mentions \
                         for an entity that is not in the scoped graph",
                        mentions.len()
                    ));
                }
            }
        }
    }
    Outcome::Pass
}

/// Property 2: the strong form — no path through an out-of-scope document.
///
/// The fixture's bridge is `candies -- manga` in `doc-a2`... which is *in* scope,
/// so the property needs the bridge to be out of scope. It is built here rather
/// than reused: `doc-a3` is out of `scope_a`, and a relation `manga -- anime`
/// lives only there. Under `scope_a`, `manga` has in-scope mentions and `anime`
/// does not exist at all, so the edge `manga -- anime` must not be in the graph.
///
/// The second half of the property is the one that catches a real leak: give the
/// out-of-scope document a relation between two entities that *both* exist
/// in scope (`manga -- candy`), and check the scoped graph does not connect them
/// beyond what `doc-a1` and `doc-a2` already justify. `manga` and `candy` are
/// already related by `doc-a1`, so this checks the absence of a *duplicate* edge
/// and the absence of `anime`, which is the observable part.
async fn no_path_through_an_out_of_scope_document(store: &impl GraphStore) -> Outcome {
    if let Err(err) = seed(store).await {
        return Outcome::Failed(format!("could not seed the fixture: {err}"));
    }
    // A relation between two in-scope entities, sourced only from an out-of-scope
    // document. `kyoko` is mentioned only in doc-a1 and `shop` only in doc-a2, so
    // without this relation they are four hops apart.
    if let Err(err) = store
        .relate(
            &tenant_a(),
            &doc(DOC_A3),
            vec![relation("kyoko", "shop", Some("doc-a3_0"))],
        )
        .await
    {
        return Outcome::Failed(format!("could not write the bridge relation: {err}"));
    }

    let scope = scope_a();
    let graph = match store.load_tenant_graph(&scope).await {
        Ok(graph) => graph,
        Err(err) => return Outcome::Failed(format!("load_tenant_graph: {err}")),
    };

    // `anime` is only mentioned by doc-a3, so it must not exist here at all.
    if graph.map().id_of("anime").is_some() {
        return Outcome::Failed(
            "`anime` is mentioned only by the out-of-scope doc-a3, yet it is in the scoped graph"
                .to_string(),
        );
    }

    // `kyoko` and `shop` must not be neighbours: the only relation between them
    // comes from the out-of-scope document.
    let kyoko = graph.map().id_of("kyoko");
    let shop = graph.map().id_of("shop");
    let connected = match (kyoko, shop) {
        (Some(k), Some(s)) => graph
            .csr()
            .neighbours(k)
            .into_iter()
            .any(|(target, _)| target == s),
        _ => false,
    };
    if connected {
        return Outcome::Failed(
            "kyoko and shop are neighbours through a relation whose only source is the \
             out-of-scope doc-a3: an out-of-scope relation became a path"
                .to_string(),
        );
    }

    // And a PPR walk must not find a path either: after four hops the scores are
    // tiny but non-zero, so compare against the same walk on the whole tenant,
    // where the bridge exists. If the scoped walk matched the whole-tenant walk
    // exactly, the scope would not have removed anything.
    let opts = WalkOptions::default();
    let scoped = graph.ppr(&[("kyoko".to_string(), 1.0)], &opts);
    let whole_graph = match store.load_tenant_graph(&scope_a_all()).await {
        Ok(g) => g,
        Err(err) => return Outcome::Failed(format!("load_tenant_graph(all): {err}")),
    };
    let whole = whole_graph.ppr(&[("kyoko".to_string(), 1.0)], &opts);
    let score_of = |graph: &TenantGraph, ranked: &[kb_core::graph::ScoredNode], key: &str| {
        ranked
            .iter()
            .find(|s| graph.map().key_of(s.node) == Some(key))
            .map(|s| s.score)
    };
    let shop_scoped = score_of(&graph, &scoped, "shop");
    let shop_whole = score_of(&whole_graph, &whole, "shop");
    match (shop_scoped, shop_whole) {
        (Some(a), Some(b)) => {
            if (a - b).abs() < 1e-9 {
                return Outcome::Failed(format!(
                    "the scoped PPR score for `shop` is {a}, identical to the whole-tenant \
                     score {b}: removing the document removed no mass"
                ));
            }
            if a >= b {
                return Outcome::Failed(format!(
                    "the scoped PPR score for `shop` ({a}) is not below the whole-tenant score \
                     ({b}): narrowing the scope did not remove anything"
                ));
            }
        }
        (None, Some(_)) => {
            return Outcome::Failed("the scoped walk reached `shop`".to_string());
        }
        _ => {}
    }
    Outcome::Pass
}

/// Property 3: the same entity key in two tenants is two facts.
///
/// The fixture gives both tenants `candy` and `manga` with the *same* keys and
/// different display names, different mentions and different neighbours. If the two
/// tenants shared a row, the first write would win and the second tenant would read
/// the first tenant's name and chunks — which is the failure.
async fn tenant_isolation(store: &impl GraphStore) -> Outcome {
    if let Err(err) = seed(store).await {
        return Outcome::Failed(format!("could not seed the fixture: {err}"));
    }

    let a = match store.load_tenant_graph(&scope_a()).await {
        Ok(graph) => graph,
        Err(err) => return Outcome::Failed(format!("load_tenant_graph(a): {err}")),
    };
    let b = match store.load_tenant_graph(&scope_b()).await {
        Ok(graph) => graph,
        Err(err) => return Outcome::Failed(format!("load_tenant_graph(b): {err}")),
    };

    // Both tenants resolve both keys, to their own facts. Different mention lists
    // are the *correct* answer here: the documents differ, so the chunks must.
    for key in ["candy", "manga"] {
        if a.chunks_for(key).is_empty() {
            return Outcome::Failed(format!("tenant A: `{key}` resolved to nothing"));
        }
        if b.chunks_for(key).is_empty() {
            return Outcome::Failed(format!("tenant B: `{key}` resolved to nothing"));
        }
    }
    if a.entity_count() == b.entity_count() {
        return Outcome::Failed(format!(
            "both tenants returned {} entities for the same key set: the two scopes read the \
             same rows",
            a.entity_count()
        ));
    }

    // A's graph must never name a B chunk, and B's must never name an A chunk. The
    // fixture's chunk ids carry their document and the documents carry their
    // tenant, so this is checkable from the ids alone — which is the point: an
    // adapter cannot make the wrong answer look right here.
    for (name, graph, prefix) in [("a", &a, "doc-a"), ("b", &b, "doc-b")] {
        for mention in graph.mentions() {
            for chunk_id in &mention.chunk_ids {
                let document = document_of_chunk(chunk_id.as_str());
                if !document.starts_with(prefix) {
                    return Outcome::Failed(format!(
                        "tenant {name}: chunk {chunk_id} lives in {document}, which belongs to \
                         another tenant"
                    ));
                }
            }
        }
    }

    // The display names are different, so a match on one tenant's name must not fire
    // on the other. This is the part of isolation a caller actually notices.
    let term = vec!["Permen".to_string()];
    let in_b = match store.match_entities(&scope_b(), &term).await {
        Ok(matched) => matched,
        Err(err) => return Outcome::Failed(format!("match_entities(b): {err}")),
    };
    if in_b.is_empty() {
        return Outcome::Failed(
            "tenant B's `candy` is named `Permen Lain Tenant`, so `Permen` must match it"
                .to_string(),
        );
    }
    let in_a = match store.match_entities(&scope_a(), &term).await {
        Ok(matched) => matched,
        Err(err) => return Outcome::Failed(format!("match_entities(a): {err}")),
    };
    if in_a
        .iter()
        .any(|(key, _)| in_b.iter().any(|(other, _)| other == key))
    {
        return Outcome::Failed(format!(
            "a match on tenant B's entity name also fired in tenant A: {in_a:?}"
        ));
    }

    // A write to one tenant must not move the other tenant's counter, or every
    // tenant invalidates every other tenant's cache on every ingest.
    let a_version = match store.graph_version(&tenant_a()).await {
        Ok(version) => version,
        Err(err) => return Outcome::Failed(format!("graph_version(a): {err}")),
    };
    let b_version = match store.graph_version(&tenant_b()).await {
        Ok(version) => version,
        Err(err) => return Outcome::Failed(format!("graph_version(b): {err}")),
    };
    if a_version == 0 || b_version == 0 {
        return Outcome::Failed(format!(
            "seeding left tenant A/B at version {a_version}/{b_version}; a seeded tenant is \
             never version 0"
        ));
    }
    if let Err(err) = store
        .upsert_entities(
            &tenant_b(),
            &doc(DOC_B1),
            vec![entity("kucing", "Kucing", &["doc-b1_0"])],
        )
        .await
    {
        return Outcome::Failed(format!("tenant B upsert errored: {err}"));
    }
    let a_after = match store.graph_version(&tenant_a()).await {
        Ok(version) => version,
        Err(err) => return Outcome::Failed(format!("graph_version(a): {err}")),
    };
    if a_after != a_version {
        return Outcome::Failed(format!(
            "a tenant B write moved tenant A's version: {a_version} -> {a_after}"
        ));
    }
    Outcome::Pass
}

/// Property 4: re-ingest is idempotent, keys dedupe, aliases merge.
async fn idempotent_upsert_and_dedupe(store: &impl GraphStore) -> Outcome {
    if let Err(err) = seed(store).await {
        return Outcome::Failed(format!("could not seed the fixture: {err}"));
    }
    let scope = scope_a();
    let before = match store.load_tenant_graph(&scope).await {
        Ok(graph) => graph,
        Err(err) => return Outcome::Failed(format!("load_tenant_graph: {err}")),
    };

    // The same batch again.
    if let Err(err) = store
        .upsert_entities(
            &tenant_a(),
            &doc(DOC_A1),
            vec![
                entity("candy", "Candy Candy", &["doc-a1_0"]),
                entity("manga", "Manga", &["doc-a1_0", "doc-a1_1"]),
                entity("candy-candy", "Candy Candy", &["doc-a1_1"]),
                entity("kyoko", "Kyoko Mizuki", &["doc-a1_1"]),
            ],
        )
        .await
    {
        return Outcome::Failed(format!("re-upsert errored: {err}"));
    }
    if let Err(err) = store
        .relate(
            &tenant_a(),
            &doc(DOC_A1),
            vec![
                relation("candy", "manga", Some("doc-a1_0")),
                relation("candy-candy", "manga", Some("doc-a1_1")),
                relation("candy", "kyoko", Some("doc-a1_1")),
            ],
        )
        .await
    {
        return Outcome::Failed(format!("re-relate errored: {err}"));
    }

    let after = match store.load_tenant_graph(&scope).await {
        Ok(graph) => graph,
        Err(err) => return Outcome::Failed(format!("load_tenant_graph after: {err}")),
    };
    if keys_of(&before) != keys_of(&after) {
        return Outcome::Failed(format!(
            "a re-ingest changed the entity set: {:?} -> {:?}",
            keys_of(&before),
            keys_of(&after)
        ));
    }
    if before.arc_count() != after.arc_count() {
        return Outcome::Failed(format!(
            "a re-ingest changed the edge count: {} -> {}",
            before.arc_count(),
            after.arc_count()
        ));
    }
    if before.chunks_for("manga") != after.chunks_for("manga") {
        return Outcome::Failed(format!(
            "a re-ingest changed `manga`'s mentions: {:?} -> {:?}",
            before.chunks_for("manga"),
            after.chunks_for("manga")
        ));
    }

    // Dedupe across documents: `manga` is in doc-a1 and doc-a2, and is one entity
    // with the union of its mentions, not two.
    let manga_chunks = after.chunks_for("manga");
    if !manga_chunks.iter().any(|c| c.as_str().starts_with(DOC_A1))
        || !manga_chunks.iter().any(|c| c.as_str().starts_with(DOC_A2))
    {
        return Outcome::Failed(format!(
            "`manga` should carry mentions from both in-scope documents, got {manga_chunks:?}"
        ));
    }
    let mut unique = manga_chunks.to_vec();
    unique.sort();
    unique.dedup();
    if unique.len() != manga_chunks.len() {
        return Outcome::Failed(format!("`manga` has duplicate mentions: {manga_chunks:?}"));
    }

    // Aliases merge: an upsert with a subset must not drop the previous ones. This
    // needs somewhere to observe them, and the port does not expose aliases yet —
    // so the property writes them and checks the write does not fail and does not
    // change the entity set, and says so rather than pretending to check the merge.
    let mut with_aliases = entity("manga", "Manga", &["doc-a1_0"]);
    with_aliases.aliases = vec!["komik".to_string()];
    if let Err(err) = store
        .upsert_entities(&tenant_a(), &doc(DOC_A1), vec![with_aliases])
        .await
    {
        return Outcome::Failed(format!("an upsert with aliases errored: {err}"));
    }
    let aliases = match store.load_tenant_graph(&scope).await {
        Ok(graph) => graph,
        Err(err) => return Outcome::Failed(format!("load_tenant_graph after aliases: {err}")),
    };
    if keys_of(&after) != keys_of(&aliases) {
        return Outcome::Failed("an alias upsert created or dropped an entity".to_string());
    }
    Outcome::Pass
}

/// Property 5: `delete_document` cascades, removes orphans, and touches nothing else.
async fn delete_document_cascades(store: &impl GraphStore) -> Outcome {
    if let Err(err) = seed(store).await {
        return Outcome::Failed(format!("could not seed the fixture: {err}"));
    }
    let before = match store.load_tenant_graph(&scope_a()).await {
        Ok(graph) => graph,
        Err(err) => return Outcome::Failed(format!("load_tenant_graph: {err}")),
    };
    if before.map().id_of("candies").is_none() {
        return Outcome::Failed("the fixture did not seed `candies`".to_string());
    }

    if let Err(err) = store.delete_document(&tenant_a(), &doc(DOC_A2)).await {
        return Outcome::Failed(format!("delete_document errored: {err}"));
    }

    let after = match store.load_tenant_graph(&scope_a()).await {
        Ok(graph) => graph,
        Err(err) => return Outcome::Failed(format!("load_tenant_graph after delete: {err}")),
    };

    // `candies` and `shop` were mentioned only by doc-a2: orphans, so gone.
    for orphan in ["candies", "shop"] {
        if after.map().id_of(orphan).is_some() {
            return Outcome::Failed(format!(
                "`{orphan}` was only mentioned by the deleted document and should have been \
                 removed as an orphan, but is still in the graph"
            ));
        }
    }
    // `manga` is still mentioned by doc-a1: kept.
    if after.map().id_of("manga").is_none() {
        return Outcome::Failed(
            "`manga` is mentioned by doc-a1 and must survive the delete".to_string(),
        );
    }
    // `manga`'s doc-a2 mentions went with the document.
    if after
        .chunks_for("manga")
        .iter()
        .any(|c| c.as_str().starts_with(DOC_A2))
    {
        return Outcome::Failed(format!(
            "`manga` still carries a doc-a2 mention after the delete: {:?}",
            after.chunks_for("manga")
        ));
    }
    // Edges to the deleted document's entities went with them.
    if after.map().id_of("candies").is_some()
        || neighbours(&after, "manga").iter().any(|k| k == "candies")
    {
        return Outcome::Failed(format!(
            "the deleted document's relation survived: manga's neighbours are {:?}",
            neighbours(&after, "manga")
        ));
    }

    // The other document in the same scope is untouched.
    if after.chunks_for("candy") != before.chunks_for("candy") {
        return Outcome::Failed("deleting doc-a2 changed doc-a1's data".to_string());
    }
    // The out-of-scope document is untouched.
    let all = match store.load_tenant_graph(&scope_a_all()).await {
        Ok(graph) => graph,
        Err(err) => return Outcome::Failed(format!("load_tenant_graph(all): {err}")),
    };
    if all.map().id_of("anime").is_none() {
        return Outcome::Failed("deleting doc-a2 removed doc-a3's entity".to_string());
    }
    // The other tenant is untouched.
    let b = match store.load_tenant_graph(&scope_b()).await {
        Ok(graph) => graph,
        Err(err) => return Outcome::Failed(format!("load_tenant_graph(b): {err}")),
    };
    if b.chunks_for("candy") != [chunk("doc-b1_0")] {
        return Outcome::Failed(format!(
            "deleting a tenant A document changed tenant B: {:?}",
            b.chunks_for("candy")
        ));
    }
    // Deleting a document the caller does not own must not delete it: the port
    // takes a tenant, so deleting (tenant B, doc-a2) is a no-op on B.
    if let Err(err) = store.delete_document(&tenant_b(), &doc(DOC_A2)).await {
        return Outcome::Failed(format!("deleting a foreign document id errored: {err}"));
    }
    let a_again = match store.load_tenant_graph(&scope_a()).await {
        Ok(graph) => graph,
        Err(err) => return Outcome::Failed(format!("load_tenant_graph(a again): {err}")),
    };
    if keys_of(&a_again) != keys_of(&after) {
        return Outcome::Failed(
            "deleting a document id under another tenant changed this tenant".to_string(),
        );
    }
    Outcome::Pass
}

/// Property 6: the version counter moves on every write.
async fn version_bumps_on_every_write(store: &impl GraphStore) -> Outcome {
    let tenant = tenant_a();
    let start = match store.graph_version(&tenant).await {
        Ok(version) => version,
        Err(err) => return Outcome::Failed(format!("graph_version errored: {err}")),
    };
    if start != 0 {
        return Outcome::Failed(format!(
            "a fresh store reported version {start} before any write; 0 must mean 'nothing written'"
        ));
    }

    // upsert_entities
    let before = match store.graph_version(&tenant).await {
        Ok(version) => version,
        Err(err) => return Outcome::Failed(format!("graph_version errored: {err}")),
    };
    if let Err(err) = store
        .upsert_entities(
            &tenant,
            &doc(DOC_A1),
            vec![entity("candy", "Candy Candy", &["doc-a1_0"])],
        )
        .await
    {
        return Outcome::Failed(format!("upsert errored: {err}"));
    }
    let after = match store.graph_version(&tenant).await {
        Ok(version) => version,
        Err(err) => return Outcome::Failed(format!("graph_version errored: {err}")),
    };
    if after <= before {
        return Outcome::Failed(format!(
            "upsert_entities did not bump the version: {before} -> {after}"
        ));
    }

    // relate
    if let Err(err) = store
        .relate(
            &tenant,
            &doc(DOC_A1),
            vec![relation("candy", "candy", None)],
        )
        .await
    {
        return Outcome::Failed(format!("relate errored: {err}"));
    }
    let after_relate = match store.graph_version(&tenant).await {
        Ok(version) => version,
        Err(err) => return Outcome::Failed(format!("graph_version errored: {err}")),
    };
    if after_relate <= after {
        return Outcome::Failed(format!(
            "relate did not bump the version: {after} -> {after_relate}"
        ));
    }

    // A write that changes nothing still bumps: the caller's cache is about to be
    // rebuilt from a store it believes is unchanged, and that is exactly how a
    // stale CSR survives.
    if let Err(err) = store
        .upsert_entities(
            &tenant,
            &doc(DOC_A1),
            vec![entity("candy", "Candy Candy", &["doc-a1_0"])],
        )
        .await
    {
        return Outcome::Failed(format!("no-op upsert errored: {err}"));
    }
    let after_noop = match store.graph_version(&tenant).await {
        Ok(version) => version,
        Err(err) => return Outcome::Failed(format!("graph_version errored: {err}")),
    };
    if after_noop <= after_relate {
        return Outcome::Failed(format!(
            "a no-op upsert did not bump the version: {after_relate} -> {after_noop}"
        ));
    }

    // delete_document
    if let Err(err) = store.delete_document(&tenant, &doc(DOC_A1)).await {
        return Outcome::Failed(format!("delete_document errored: {err}"));
    }
    let after_delete = match store.graph_version(&tenant).await {
        Ok(version) => version,
        Err(err) => return Outcome::Failed(format!("graph_version errored: {err}")),
    };
    if after_delete <= after_noop {
        return Outcome::Failed(format!(
            "delete_document did not bump the version: {after_noop} -> {after_delete}"
        ));
    }

    // Another tenant's writes must not move this tenant's counter, or every
    // tenant invalidates every other tenant's cache on every ingest.
    let b_before = match store.graph_version(&tenant_b()).await {
        Ok(version) => version,
        Err(err) => return Outcome::Failed(format!("graph_version errored: {err}")),
    };
    if let Err(err) = store
        .upsert_entities(
            &tenant_b(),
            &doc(DOC_B1),
            vec![entity("candy-b", "Permen", &["doc-b1_1"])],
        )
        .await
    {
        return Outcome::Failed(format!("tenant B upsert errored: {err}"));
    }
    let b_after = match store.graph_version(&tenant_b()).await {
        Ok(version) => version,
        Err(err) => return Outcome::Failed(format!("graph_version errored: {err}")),
    };
    if b_after <= b_before {
        return Outcome::Failed(format!(
            "a tenant B write did not bump tenant B's version: {b_before} -> {b_after}"
        ));
    }
    let a_now = match store.graph_version(&tenant).await {
        Ok(version) => version,
        Err(err) => return Outcome::Failed(format!("graph_version errored: {err}")),
    };
    if a_now != after_delete {
        return Outcome::Failed(format!(
            "a tenant B write moved tenant A's version: {after_delete} -> {a_now}"
        ));
    }
    Outcome::Pass
}

/// Property 7: the same writes give the same load, twice.
async fn deterministic_ordering(store: &impl GraphStore) -> Outcome {
    if let Err(err) = seed(store).await {
        return Outcome::Failed(format!("could not seed the fixture: {err}"));
    }
    let scope = scope_a();
    let first = match store.load_tenant_graph(&scope).await {
        Ok(graph) => graph,
        Err(err) => return Outcome::Failed(format!("load_tenant_graph: {err}")),
    };
    let second = match store.load_tenant_graph(&scope).await {
        Ok(graph) => graph,
        Err(err) => return Outcome::Failed(format!("load_tenant_graph: {err}")),
    };
    if keys_of(&first) != keys_of(&second) {
        return Outcome::Failed(format!(
            "two loads of the same scope returned different key orders: {:?} vs {:?}",
            keys_of(&first),
            keys_of(&second)
        ));
    }
    if first.arc_count() != second.arc_count() {
        return Outcome::Failed(format!(
            "two loads returned different edge counts: {} vs {}",
            first.arc_count(),
            second.arc_count()
        ));
    }
    for (a, b) in first.mentions().iter().zip(second.mentions()) {
        if a != b {
            return Outcome::Failed(format!(
                "mention lists differ between loads: {a:?} vs {b:?}"
            ));
        }
    }

    // The walk's output must be ordered and identical run to run.
    let opts = WalkOptions::default();
    let seeds = vec![("candy".to_string(), 1.0)];
    let ranked_first = first.ranked_keys(&first.ppr(&seeds, &opts));
    let ranked_second = second.ranked_keys(&second.ppr(&seeds, &opts));
    if ranked_first != ranked_second {
        return Outcome::Failed(format!(
            "two walks over the same graph ranked differently: {ranked_first:?} vs {ranked_second:?}"
        ));
    }
    for pair in ranked_first.windows(2) {
        if pair[0].1 < pair[1].1 {
            return Outcome::Failed(format!(
                "the ranking is not ordered by score: {ranked_first:?}"
            ));
        }
    }

    // match_entities is ordered by (weight desc, key asc).
    let matched = match store.match_entities(&scope, &["candy".to_string()]).await {
        Ok(m) => m,
        Err(err) => return Outcome::Failed(format!("match_entities errored: {err}")),
    };
    for pair in matched.windows(2) {
        if pair[0].1 < pair[1].1 {
            return Outcome::Failed(format!(
                "match_entities is not ordered by weight: {matched:?}"
            ));
        }
    }
    let matched_again = store
        .match_entities(&scope, &["candy".to_string()])
        .await
        .unwrap_or_default();
    if matched != matched_again {
        return Outcome::Failed(format!(
            "match_entities returned different results for the same call: {matched:?} vs {matched_again:?}"
        ));
    }

    // entity_mentions is ascending by chunk id.
    let mentions = store
        .entity_mentions(&scope, "manga")
        .await
        .unwrap_or_default();
    let mut sorted = mentions.clone();
    sorted.sort();
    if mentions != sorted {
        return Outcome::Failed(format!(
            "entity_mentions is not ascending by chunk id: {mentions:?}"
        ));
    }
    Outcome::Pass
}

/// The fixture's chunk ids are `<document>_<index>`, which is how the suite knows
/// which document a mention belongs to without the port exposing chunk→document.
fn document_of_chunk(chunk_id: &str) -> &str {
    chunk_id.split('_').next().unwrap_or(chunk_id)
}
