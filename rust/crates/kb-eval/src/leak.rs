//! The red-team leak matrix: every retrieval arm × every scope type.
//!
//! Two things are being proved here, and the second is the one that is easy to
//! forget:
//!
//! 1. **No cross-scope hit.** Inside a scope, no chunk, source, figure,
//!    neighbour or entity may belong to a document outside that scope.
//! 2. **The probe can find the thing at all.** "Zero leaks" from a search that
//!    returns nothing is vacuous. Every probe therefore runs twice: once inside
//!    a scope that *does* contain the target (positive control, must hit) and
//!    once inside a scope that does not (must return nothing outside). An arm
//!    whose positive control never hits is reported `UNTESTED`, never `PASS`.
//!
//! This module is pure: it plans the probes, decides what a leak is, classifies
//! a cell and renders the report. [`crate::leak_http`] does the HTTP. That split
//! is what lets the logic be tested against fakes, including a fake that leaks
//! on purpose.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

/// Outside documents probed per scope.
///
/// The matrix grows as `tenants × knowledge bases × categories × documents`;
/// without a cap a 2×2×2 seed is already hundreds of probes and a real corpus
/// would be tens of thousands. The first N outside documents of a scope are
/// chosen deterministically, so the same seed always probes the same pairs.
pub const MAX_OUTSIDE_PER_SCOPE: usize = 3;

/// A retrieval surface, named after what it retrieves.
///
/// The current server fuses the vector and BM25 arms on one endpoint, so an arm
/// is separated *from the wire* in two ways. Since step S.1c the answer carries a
/// `retrieval.arms` list per chunk and that is what is read first
/// ([`Arm::from_wire`]); only a server that does not report it falls back to the
/// older chunk-shape heuristic (a `chunkType` means the vector arm, its absence
/// means the keyword arm, an `assetKey` means a figure). The fallback is counted
/// and reported, because an arm attributed by guesswork is weaker evidence than
/// one the server named.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Arm {
    Vector,
    Bm25,
    Neighbour,
    Figure,
    Entity,
}

impl Arm {
    /// Every arm, in the order the report lists them.
    pub const ALL: [Self; 5] = [
        Self::Vector,
        Self::Bm25,
        Self::Neighbour,
        Self::Figure,
        Self::Entity,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vector => "vector",
            Self::Bm25 => "bm25",
            Self::Neighbour => "neighbour",
            Self::Figure => "figure",
            Self::Entity => "entity",
        }
    }

    /// The `retrieval.arms` label the server uses for this arm, or `None` for an
    /// arm that is not a chunk-level retrieval phase (the entity graph is served
    /// by a document-addressed route, so no chunk is ever attributed to it).
    #[must_use]
    pub fn wire_label(self) -> Option<&'static str> {
        match self {
            Self::Vector => Some("dense"),
            Self::Bm25 => Some("sparse"),
            Self::Neighbour => Some("neighbour"),
            Self::Figure => Some("figure"),
            Self::Entity => None,
        }
    }

    /// The arm a `retrieval.arms` label names.
    ///
    /// `None` for a label this harness does not know: a newer server may add an
    /// arm, and silently mapping an unknown one onto a known one would put a
    /// hit in the wrong cell.
    #[must_use]
    pub fn from_wire(label: &str) -> Option<Self> {
        match label {
            "dense" => Some(Self::Vector),
            "sparse" => Some(Self::Bm25),
            "neighbour" => Some(Self::Neighbour),
            "figure" => Some(Self::Figure),
            _ => None,
        }
    }

    /// How the arm is exercised, and what "one answer" means for it.
    #[must_use]
    pub fn probe_kind(self) -> ProbeKind {
        match self {
            Self::Entity => ProbeKind::DocumentIntelligence,
            Self::Figure => ProbeKind::SearchOrAsset,
            Self::Vector | Self::Bm25 | Self::Neighbour => ProbeKind::Search,
        }
    }
}

/// How the arms of a returned chunk were decided.
///
/// Both are legitimate; they are not equally strong, so the report counts them
/// rather than quietly using whichever was available.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Attribution {
    /// The server named the arms (`retrieval.arms`).
    #[default]
    Provenance,
    /// No `retrieval` block: the arms were read off the chunk's shape.
    Heuristic,
}

/// How one probe reaches the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeKind {
    /// `POST /v1/search` inside a scope.
    Search,
    /// `POST /v1/search`, plus `GET /v1/documents/:id/assets` when the seeded
    /// document actually has a figure to fetch.
    SearchOrAsset,
    /// `GET /v1/documents/:id/intelligence` — the entity graph of one document,
    /// addressed by id, which is the entity arm's only surface in this API.
    DocumentIntelligence,
}

/// A way a caller narrows what a search may see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    /// No filter: everything the key's tenant may see.
    Tenant,
    /// `knowledgeBaseIds: [kb]`.
    KnowledgeBase,
    /// `category: <category id>`.
    Category,
    /// `documentIds: [...]`.
    DocumentIds,
}

impl ScopeKind {
    pub const ALL: [Self; 4] = [
        Self::Tenant,
        Self::KnowledgeBase,
        Self::Category,
        Self::DocumentIds,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tenant => "tenant",
            Self::KnowledgeBase => "knowledge_base",
            Self::Category => "category",
            Self::DocumentIds => "document_ids",
        }
    }
}

/// The unique strings planted in one seeded document.
///
/// One per arm, because each arm must be able to find *this* document and no
/// other: a single shared token would let a hit on one document be mistaken for
/// a hit on another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Canaries {
    /// A nonsense token nothing else in the corpus contains. BM25's target.
    pub bm25: String,
    /// A distinctive sentence. The vector arm's target.
    pub vector: String,
    /// An invented proper noun, the kind entity extraction picks up.
    pub entity: String,
    /// A figure caption.
    pub figure: String,
    /// A sentence that is *not* the head of the document: the neighbouring
    /// chunk's text, for the weave.
    pub neighbour: String,
}

/// A document the instrument seeded, with everything needed to probe it.
#[derive(Debug, Clone, Serialize)]
pub struct SeedDocument {
    pub id: String,
    pub tenant: String,
    pub knowledge_base_id: String,
    pub category_id: String,
    pub canaries: Canaries,
    /// Chunks the ingest produced; the neighbour arm needs more than one.
    pub n_chunks: usize,
    /// Whether the ingest produced figures for this document.
    pub has_figures: bool,
    /// First figure asset key, when there is one.
    pub figure_asset_key: Option<String>,
}

/// A scope: the documents a caller may see, and the request that selects them.
#[derive(Debug, Clone, Serialize)]
pub struct Scope {
    pub kind: ScopeKind,
    /// Human name, e.g. `tenant=A` or `kb=A/kb0`.
    pub label: String,
    /// Tenant the scope is issued under — which key signs the request.
    pub tenant: String,
    /// The documents in scope. Anything else in an answer is a leak.
    pub allowed: Vec<String>,
    /// Body fields that select this scope.
    pub knowledge_base_ids: Vec<String>,
    pub category: Option<String>,
    pub document_ids: Option<Vec<String>>,
}

/// What one probe does and what it expects.
#[derive(Debug, Clone, Serialize)]
pub struct PlannedProbe {
    pub arm: Arm,
    pub kind: ProbeKind,
    /// Index into [`SeedPlan::scopes`].
    pub scope: usize,
    /// The query (or, for the entity arm, the document id being addressed).
    pub query: String,
    /// The document whose canary this probe uses. For a leak probe it is
    /// outside the scope; for a positive control it is inside.
    pub canary_owner: String,
    pub positive: bool,
}

/// Everything the probe half of the instrument needs.
#[derive(Debug, Clone, Serialize)]
pub struct SeedPlan {
    pub documents: Vec<SeedDocument>,
    pub scopes: Vec<Scope>,
    pub probes: Vec<PlannedProbe>,
}

impl SeedPlan {
    #[must_use]
    pub fn document(&self, id: &str) -> Option<&SeedDocument> {
        self.documents.iter().find(|d| d.id == id)
    }

    #[must_use]
    pub fn scope(&self, index: usize) -> Option<&Scope> {
        self.scopes.get(index)
    }

    /// Every document outside a scope, in a deterministic order.
    fn outside(&self, scope: &Scope) -> Vec<&SeedDocument> {
        self.documents
            .iter()
            .filter(|d| !scope.allowed.contains(&d.id))
            .collect()
    }
}

/// Build the scopes and probes for a seeded corpus.
///
/// Pure, and therefore testable without a server: given documents it decides
/// which (scope, arm, canary) triples are worth asking about.
#[must_use]
pub fn plan(documents: Vec<SeedDocument>) -> SeedPlan {
    let mut scopes: Vec<Scope> = Vec::new();
    let mut tenants: Vec<String> = documents.iter().map(|d| d.tenant.clone()).collect();
    tenants.dedup();
    tenants.sort();
    tenants.dedup();

    for tenant in &tenants {
        let of_tenant: Vec<&SeedDocument> =
            documents.iter().filter(|d| &d.tenant == tenant).collect();

        // 1. Tenant: no filter at all.
        scopes.push(Scope {
            kind: ScopeKind::Tenant,
            label: format!("tenant={tenant}"),
            tenant: tenant.clone(),
            allowed: of_tenant.iter().map(|d| d.id.clone()).collect(),
            knowledge_base_ids: Vec::new(),
            category: None,
            document_ids: None,
        });

        // 2. One knowledge base each.
        let mut kbs: Vec<&str> = of_tenant
            .iter()
            .map(|d| d.knowledge_base_id.as_str())
            .collect();
        kbs.sort_unstable();
        kbs.dedup();
        for kb in kbs {
            scopes.push(Scope {
                kind: ScopeKind::KnowledgeBase,
                label: format!("tenant={tenant} kb={kb}"),
                tenant: tenant.clone(),
                allowed: of_tenant
                    .iter()
                    .filter(|d| d.knowledge_base_id == kb)
                    .map(|d| d.id.clone())
                    .collect(),
                knowledge_base_ids: vec![kb.to_string()],
                category: None,
                document_ids: None,
            });
        }

        // 3. One category each.
        let mut cats: Vec<&str> = of_tenant.iter().map(|d| d.category_id.as_str()).collect();
        cats.sort_unstable();
        cats.dedup();
        for cat in cats {
            scopes.push(Scope {
                kind: ScopeKind::Category,
                label: format!("tenant={tenant} category={cat}"),
                tenant: tenant.clone(),
                allowed: of_tenant
                    .iter()
                    .filter(|d| d.category_id == cat)
                    .map(|d| d.id.clone())
                    .collect(),
                knowledge_base_ids: Vec::new(),
                category: Some(cat.to_string()),
                document_ids: None,
            });
        }

        // 4. Explicit document ids: the tenant's first document, and nothing
        //    else. Probing with a canary from a sibling document and from the
        //    other tenant is what exercises this branch.
        if let Some(first) = of_tenant.first() {
            scopes.push(Scope {
                kind: ScopeKind::DocumentIds,
                label: format!("tenant={tenant} documentIds=[{}]", first.id),
                tenant: tenant.clone(),
                allowed: vec![first.id.clone()],
                knowledge_base_ids: Vec::new(),
                category: None,
                document_ids: Some(vec![first.id.clone()]),
            });
        }
    }

    let mut probes = Vec::new();
    for (index, scope) in scopes.iter().enumerate() {
        let holder = SeedPlan {
            documents: documents.clone(),
            scopes: scopes.clone(),
            probes: Vec::new(),
        };
        for outside in holder
            .outside(scope)
            .into_iter()
            .take(MAX_OUTSIDE_PER_SCOPE)
        {
            for arm in Arm::ALL {
                probes.push(PlannedProbe {
                    arm,
                    kind: arm.probe_kind(),
                    scope: index,
                    query: canary_query(outside, arm),
                    canary_owner: outside.id.clone(),
                    positive: false,
                });
            }
        }
        // One positive control per arm per scope, from the first document that
        // this scope can see. If this cannot be found, the scope proves nothing.
        if let Some(inside) = documents.iter().find(|d| scope.allowed.contains(&d.id)) {
            for arm in Arm::ALL {
                probes.push(PlannedProbe {
                    arm,
                    kind: arm.probe_kind(),
                    scope: index,
                    query: canary_query(inside, arm),
                    canary_owner: inside.id.clone(),
                    positive: true,
                });
            }
        }
    }

    SeedPlan {
        documents,
        scopes,
        probes,
    }
}

/// The query that should find `document` through `arm`.
///
/// The entity arm addresses the document itself, so its "query" is the id the
/// caller must not be able to reach from outside the scope.
#[must_use]
pub fn canary_query(document: &SeedDocument, arm: Arm) -> String {
    match arm {
        Arm::Vector => document.canaries.vector.clone(),
        Arm::Bm25 => document.canaries.bm25.clone(),
        Arm::Neighbour => document.canaries.neighbour.clone(),
        Arm::Figure => document.canaries.figure.clone(),
        Arm::Entity => document.id.clone(),
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            scope: ScopeKind::Tenant,
            probes: 0,
            errors: 0,
            refusals: 0,
            leaks: 0,
            positive_probes: 0,
            positive_hits: 0,
            status: Status::default(),
            reason: None,
        }
    }
}

impl Default for ArmReport {
    fn default() -> Self {
        Self {
            arm: Arm::Vector,
            cells: Vec::new(),
            leaks: 0,
            probes: 0,
            errors: 0,
            refusals: 0,
            positive_probes: 0,
            positive_hits: 0,
            status: Status::default(),
            reason: None,
            coverage: String::new(),
            leak_examples: Vec::new(),
        }
    }
}

/// The documents a document-addressed route may speak for.
///
/// `GET /v1/documents/:id/intelligence` and `…/assets` are authorised by
/// **tenant and document visibility**, not by the search filters: a caller who
/// can see a document in their own tenant may read its graph and its figures
/// whichever knowledge base the *search* was scoped to. Judging those routes
/// against a `knowledgeBaseIds` scope would report a leak the API contract does
/// not consider one, and would bury a real cross-tenant leak in noise.
#[must_use]
pub fn tenant_allowed(plan: &SeedPlan, tenant: &str) -> Vec<String> {
    plan.documents
        .iter()
        .filter(|d| d.tenant == tenant)
        .map(|d| d.id.clone())
        .collect()
}

/// What one probe came back with.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Answer {
    /// Documents the answer named, in rank order, duplicates collapsed.
    pub documents: Vec<String>,
    /// Whatever the server said was degraded. Empty is the healthy case; a
    /// non-empty list is published, never swallowed.
    pub degrades: Vec<String>,
    pub note: Option<String>,
    pub http_status: u16,
    /// A request that did not complete, or an HTTP status that means the server
    /// is broken. **Not** a 404 from a document-addressed route: refusing a
    /// document outside the scope is the correct answer, and counting it as an
    /// error would bury the evidence that the guard works.
    pub error: Option<String>,
    /// The server refused to serve a document this scope may not see.
    pub refused: bool,
    /// How this answer's arms were decided. Recorded per answer so the report
    /// can separate "the server named the arm" from "the harness guessed it".
    pub attribution: Attribution,
}

impl Answer {
    #[must_use]
    pub fn failed(status: u16, error: impl Into<String>) -> Self {
        Self {
            http_status: status,
            error: Some(error.into()),
            ..Self::default()
        }
    }

    /// A document-addressed route answering "not yours".
    #[must_use]
    pub fn refused(status: u16) -> Self {
        Self {
            http_status: status,
            refused: true,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }
}

/// The documents in `answer` that are not in `scope`.
///
/// The whole check in one place: a leak is *any* document outside the scope,
/// whether it arrived as a chunk, a source, a figure or a neighbour.
///
/// Deduplicated on the way out, so one foreign document returned as four chunks
/// is counted once. A leak count that grows with the chunk count measures
/// nothing.
#[must_use]
pub fn leaks_in(answer: &Answer, allowed: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    answer
        .documents
        .iter()
        .filter(|id| !allowed.contains(id) && seen.insert((*id).clone()))
        .cloned()
        .collect()
}

/// Whether a positive control succeeded: the target was in the answer.
#[must_use]
pub fn positive_hit(answer: &Answer, document_id: &str) -> bool {
    answer.documents.iter().any(|id| id == document_id)
}

/// PASS / LEAK / UNTESTED for one cell or one arm.
///
/// The default is `Untested`: a tally nobody has filled in is evidence of
/// nothing, and the only safe thing to report for it is a hole.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Status {
    Pass,
    Leak,
    Partial,
    #[default]
    Untested,
}

impl Status {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Leak => "LEAK",
            Self::Partial => "PARTIAL",
            Self::Untested => "UNTESTED",
        }
    }

    /// The rule, in one place, so the arm status and the cell status cannot
    /// disagree: a leak always wins; without a positive control the rest is
    /// untested, never "no leaks found".
    #[must_use]
    pub fn decide(leaks: usize, positive_probes: usize, positive_hits: usize) -> Self {
        if leaks > 0 {
            Self::Leak
        } else if positive_probes == 0 || positive_hits == 0 {
            Self::Untested
        } else {
            Self::Pass
        }
    }

    /// The arm-level status derived from its cells:
    /// - LEAK if any cell leaks;
    /// - otherwise PARTIAL if some cells are UNTESTED and others PASS;
    /// - UNTESTED if all cells are UNTESTED;
    /// - PASS only if every cell passes.
    #[must_use]
    pub fn decide_arm(cells: &[Cell]) -> Self {
        if cells.is_empty() {
            return Self::Untested;
        }
        if cells.iter().any(|c| c.status == Self::Leak || c.leaks > 0) {
            return Self::Leak;
        }
        let any_untested = cells.iter().any(|c| c.status == Self::Untested);
        let any_pass = cells.iter().any(|c| c.status == Self::Pass);
        if any_untested && any_pass {
            Self::Partial
        } else if any_pass {
            Self::Pass
        } else {
            Self::Untested
        }
    }

    /// Why a cell or arm is `UNTESTED`, for the report.
    #[must_use]
    pub fn untested_reason(self, positive_probes: usize, positive_hits: usize) -> Option<String> {
        if self != Self::Untested {
            return None;
        }
        Some(if positive_probes == 0 {
            "no positive control was possible for this cell".to_string()
        } else {
            format!(
                "positive control missed {}/{} times: this probe cannot find its own target, so 'no leaks' proves nothing",
                positive_probes - positive_hits,
                positive_probes
            )
        })
    }
}

/// One probe's outcome, as the matrix needs it.
#[derive(Debug, Clone)]
pub struct Observation {
    pub arm: Arm,
    pub scope: ScopeKind,
    /// True for a positive control, false for a leak probe.
    pub positive: bool,
    /// Whether the positive control found its target.
    pub hit: bool,
    /// Documents outside the scope that came back.
    pub leaked: Vec<String>,
    /// The request itself failed, or the server answered 5xx.
    pub errored: bool,
    /// The server refused to serve a document outside the scope.
    pub refused: bool,
    pub degrades: Vec<String>,
    /// How this answer's arms were decided: the server named them, or the harness
    /// read them off the chunk shape.
    pub attribution: Attribution,
    /// Kept only when something leaked: the request and response that did it.
    pub example: Option<LeakExample>,
}

/// One leaked document, with the evidence kept.
#[derive(Debug, Clone, Serialize)]
pub struct LeakExample {
    pub arm: Arm,
    pub scope: String,
    pub query: String,
    /// The document whose canary was used — the one that must not have been
    /// reachable from inside this scope.
    pub canary_owner: String,
    pub request: serde_json::Value,
    pub foreign_document_ids: Vec<String>,
    pub response: serde_json::Value,
    /// What the server said was degraded on this answer, if anything.
    pub degrades: Vec<String>,
}

/// Tallies for one (arm, scope kind) cell.
#[derive(Debug, Clone, Serialize)]
pub struct Cell {
    pub scope: ScopeKind,
    pub probes: usize,
    pub errors: usize,
    /// Probes the server refused with a 4xx: a document outside the scope was
    /// asked for by name and did not come back.
    pub refusals: usize,
    pub leaks: usize,
    pub positive_probes: usize,
    pub positive_hits: usize,
    pub status: Status,
    pub reason: Option<String>,
}

/// Tallies for one arm across every scope kind.
#[derive(Debug, Clone, Serialize)]
pub struct ArmReport {
    pub arm: Arm,
    pub cells: Vec<Cell>,
    pub leaks: usize,
    pub probes: usize,
    pub errors: usize,
    pub refusals: usize,
    pub positive_probes: usize,
    pub positive_hits: usize,
    pub status: Status,
    pub reason: Option<String>,
    /// Why this arm is or is not separately observable over HTTP.
    pub coverage: String,
    pub leak_examples: Vec<LeakExample>,
}

/// The whole matrix.
#[derive(Debug, Clone, Serialize)]
pub struct LeakMatrix {
    pub base_url: String,
    pub server_version: Option<String>,
    pub harness_commit: String,
    pub timestamp: String,
    pub arms: Vec<ArmReport>,
    /// Every degradation label the server reported during the probes.
    pub degrades: BTreeSet<String>,
    /// How many search answers named their own arms and how many had to be
    /// attributed by the chunk-shape heuristic.
    pub attribution: AttributionCounts,
    pub notes: Vec<String>,
}

/// Provenance attribution, counted. `heuristic > 0` means part of this matrix was
/// decided by guesswork, and the report says so rather than calling it a pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct AttributionCounts {
    /// Search answers whose chunks carried `retrieval.arms`.
    pub provenance: usize,
    /// Search answers with no `retrieval` block (an older server, or a route that
    /// returns no chunks).
    pub heuristic: usize,
}

impl Default for LeakMatrix {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            server_version: None,
            harness_commit: crate::report::harness_commit(),
            timestamp: String::new(),
            arms: Vec::new(),
            degrades: BTreeSet::new(),
            attribution: AttributionCounts::default(),
            notes: Vec::new(),
        }
    }
}

impl LeakMatrix {
    #[must_use]
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            harness_commit: crate::report::harness_commit(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn arm(&mut self, arm: Arm) -> &mut ArmReport {
        if let Some(position) = self.arms.iter().position(|a| a.arm == arm) {
            return &mut self.arms[position];
        }
        let position = self.arms.len();
        self.arms.push(ArmReport {
            arm,
            coverage: coverage_note(arm).to_string(),
            ..ArmReport::default()
        });
        &mut self.arms[position]
    }

    /// Record one probe's outcome. Pure bookkeeping; the caller has already
    /// decided what the answer meant.
    pub fn record(&mut self, seen: Observation) {
        for label in &seen.degrades {
            self.degrades.insert(label.clone());
        }
        // Only the arms that retrieve chunks have an attribution to count. The
        // entity arm answers from a document-addressed route, where no chunk and
        // therefore no `retrieval` block is ever involved.
        if seen.arm != Arm::Entity {
            match seen.attribution {
                Attribution::Provenance => self.attribution.provenance += 1,
                Attribution::Heuristic => self.attribution.heuristic += 1,
            }
        }
        let report = self.arm(seen.arm);
        report.probes += 1;
        if seen.errored {
            report.errors += 1;
        }
        if seen.refused {
            report.refusals += 1;
        }
        if seen.positive {
            report.positive_probes += 1;
            if seen.hit {
                report.positive_hits += 1;
            }
        }
        report.leaks += seen.leaked.len();
        let cell = match report.cells.iter_mut().position(|c| c.scope == seen.scope) {
            Some(index) => &mut report.cells[index],
            None => {
                report.cells.push(Cell {
                    scope: seen.scope,
                    ..Cell::default()
                });
                let last = report.cells.len() - 1;
                &mut report.cells[last]
            }
        };
        cell.probes += 1;
        if seen.errored {
            cell.errors += 1;
        }
        if seen.refused {
            cell.refusals += 1;
        }
        if seen.positive {
            cell.positive_probes += 1;
            if seen.hit {
                cell.positive_hits += 1;
            }
        }
        cell.leaks += seen.leaked.len();
        if let Some(example) = seen.example {
            report.leak_examples.push(example);
        }
    }

    /// Freeze every tally into its PASS / LEAK / UNTESTED status.
    #[must_use]
    pub fn finish(mut self) -> Self {
        for arm in &mut self.arms {
            for cell in &mut arm.cells {
                cell.status = Status::decide(cell.leaks, cell.positive_probes, cell.positive_hits);
                cell.reason = cell
                    .status
                    .untested_reason(cell.positive_probes, cell.positive_hits);
            }
            arm.status = Status::decide_arm(&arm.cells);
            arm.reason = match arm.status {
                Status::Untested => arm
                    .status
                    .untested_reason(arm.positive_probes, arm.positive_hits),
                Status::Partial => {
                    let untested_scopes: Vec<_> = arm
                        .cells
                        .iter()
                        .filter(|c| c.status == Status::Untested)
                        .map(|c| c.scope.as_str())
                        .collect();
                    Some(format!("scopes untested: {}", untested_scopes.join(", ")))
                }
                _ => None,
            };
        }
        self
    }

    /// Any LEAK anywhere, including a cell-level leak under an arm that is
    /// otherwise `UNTESTED`. The gate never looks only at arm status.
    #[must_use]
    pub fn any_leak(&self) -> bool {
        self.arms
            .iter()
            .any(|a| a.leaks > 0 || a.cells.iter().any(|c| c.leaks > 0))
    }

    #[must_use]
    pub fn any_untested(&self) -> bool {
        self.arms
            .iter()
            .any(|a| matches!(a.status, Status::Untested | Status::Partial))
    }

    #[must_use]
    pub fn total_probes(&self) -> usize {
        self.arms.iter().map(|a| a.probes).sum()
    }

    #[must_use]
    pub fn total_leaks(&self) -> usize {
        self.arms.iter().map(|a| a.leaks).sum()
    }

    #[must_use]
    pub fn total_refusals(&self) -> usize {
        self.arms.iter().map(|a| a.refusals).sum()
    }
}

/// What "this arm" can actually be observed to do, in one sentence.
#[must_use]
pub fn coverage_note(arm: Arm) -> &'static str {
    match arm {
        Arm::Vector => "chunks whose retrieval.arms include `dense`, from POST /v1/search",
        Arm::Bm25 => "chunks whose retrieval.arms include `sparse`, from POST /v1/search",
        Arm::Neighbour => {
            "every chunk of a search answer: a woven neighbour is one thing the answer adds \
             that no arm ranked, so this cell stays the widest containment check rather than \
             the narrow one it could be"
        }
        Arm::Figure => {
            "sources and chunks carrying an assetKey or a `figure` arm, plus the asset route \
             when a seeded document has a figure"
        }
        Arm::Entity => "GET /v1/documents/:id/intelligence for a document id",
    }
}

/// `leak-report.md`: the matrix, the evidence, and what was not covered.
#[must_use]
pub fn render_markdown(matrix: &LeakMatrix, plan: &SeedPlan) -> String {
    let mut out = String::new();
    out.push_str("# Cross-scope leak matrix\n\n");
    out.push_str(&format!("- base url: `{}`\n", matrix.base_url));
    out.push_str(&format!(
        "- server version: {}\n",
        matrix
            .server_version
            .as_deref()
            .map_or_else(|| "unknown".to_string(), |v| format!("`{v}`"))
    ));
    out.push_str(&format!("- harness commit: `{}`\n", matrix.harness_commit));
    out.push_str(&format!("- timestamp: {}\n", matrix.timestamp));
    out.push_str(&format!(
        "- seeded: {} documents across {} tenants, {} scopes, {} probes\n",
        plan.documents.len(),
        plan.documents
            .iter()
            .map(|d| d.tenant.clone())
            .collect::<BTreeSet<_>>()
            .len(),
        plan.scopes.len(),
        matrix.total_probes()
    ));
    out.push_str(&format!(
        "- result: **{}** — {} leak(s) over {} probes\n\n",
        if matrix.any_leak() {
            "LEAK"
        } else if matrix.any_untested() {
            "no leak found, some arms UNTESTED"
        } else {
            "no leak found"
        },
        matrix.total_leaks(),
        matrix.total_probes()
    ));
    out.push_str(&format!(
        "- arm attribution: {} answer(s) named their own arms (`retrieval.arms`), {} had none \
         and were attributed by the chunk-shape heuristic",
        matrix.attribution.provenance, matrix.attribution.heuristic
    ));
    if matrix.attribution.heuristic > 0 {
        out.push_str(" (this server predates step S.1c, or the probes returned no chunks)");
    }
    out.push_str("\n\n");

    out.push_str("## Matrix (arm × scope type)\n\n");
    out.push_str("Each cell is `probes / leaks / positive-control hits` and its status.\n\n");
    out.push_str("| arm | ");
    for kind in ScopeKind::ALL {
        out.push_str(&format!("{} | ", kind.as_str()));
    }
    out.push_str("arm status |\n|---|");
    for _ in ScopeKind::ALL {
        out.push_str("---|");
    }
    out.push_str("---|\n");
    for arm in Arm::ALL {
        let Some(report) = matrix.arms.iter().find(|a| a.arm == arm) else {
            out.push_str(&format!("| {} | not run |||||\n", arm.as_str()));
            continue;
        };
        out.push_str(&format!("| {} | ", arm.as_str()));
        for kind in ScopeKind::ALL {
            match report.cells.iter().find(|c| c.scope == kind) {
                Some(cell) => out.push_str(&format!(
                    "{} / {} / {}-{} **{}** | ",
                    cell.probes,
                    cell.leaks,
                    cell.positive_hits,
                    cell.positive_probes,
                    cell.status.as_str()
                )),
                None => out.push_str("n/a | "),
            }
        }
        out.push_str(&format!("**{}** |\n", report.status.as_str()));
    }

    out.push_str("\n## Per-arm detail\n");
    for arm in Arm::ALL {
        let Some(report) = matrix.arms.iter().find(|a| a.arm == arm) else {
            out.push_str(&format!("\n### {}\n\nnot run\n", arm.as_str()));
            continue;
        };
        out.push_str(&format!("\n### {}\n\n", arm.as_str()));
        out.push_str(&format!("- coverage: {}\n", report.coverage));
        out.push_str(&format!(
            "- probes: {}, leaks: {}, refused with 4xx: {}, http errors: {}, positive control: \
             {}/{}\n",
            report.probes,
            report.leaks,
            report.refusals,
            report.errors,
            report.positive_hits,
            report.positive_probes
        ));
        out.push_str(&format!("- status: **{}**\n", report.status.as_str()));
        if let Some(reason) = &report.reason {
            out.push_str(&format!("- why: {reason}\n"));
        }
        for cell in &report.cells {
            out.push_str(&format!(
                "- `{}`: {} probes, {} leaks, {} refused, positive {}/{} — **{}**{}\n",
                cell.scope.as_str(),
                cell.probes,
                cell.leaks,
                cell.refusals,
                cell.positive_hits,
                cell.positive_probes,
                cell.status.as_str(),
                cell.reason
                    .as_ref()
                    .map_or_else(String::new, |r| format!(" ({r})"))
            ));
        }
        for example in &report.leak_examples {
            out.push_str(&format!(
                "\n#### LEAK: {} outside `{}`\n\n- query: `{}`\n- canary belonged to: `{}`\n- foreign documents returned: {}\n- server degrades: {}\n- request: `{}`\n- response: `{}`\n",
                example.arm.as_str(),
                example.scope,
                example.query,
                example.canary_owner,
                example.foreign_document_ids.join(", "),
                if example.degrades.is_empty() {
                    "none".to_string()
                } else {
                    example.degrades.join(", ")
                },
                serde_json::to_string(&example.request).unwrap_or_default(),
                serde_json::to_string(&example.response).unwrap_or_default()
            ));
        }
    }

    out.push_str("\n## Degradations reported by the server\n\n");
    if matrix.degrades.is_empty() {
        out.push_str("None: every probe was answered without a `degrades` entry.\n");
    } else {
        for label in &matrix.degrades {
            out.push_str(&format!("- {label}\n"));
        }
    }

    out.push_str("\n## What this run does not prove\n\n");
    let mut unproven = 0;
    for arm in Arm::ALL {
        let Some(report) = matrix.arms.iter().find(|a| a.arm == arm) else {
            continue;
        };
        for cell in &report.cells {
            if cell.positive_hits < cell.positive_probes {
                out.push_str(&format!(
                    "- `{}` × `{}`: the positive control found its own target only {}/{} times, \
                     so the {} leak probe(s) in that cell do not prove containment — the probe \
                     cannot always reach what it is looking for.\n",
                    arm.as_str(),
                    cell.scope.as_str(),
                    cell.positive_hits,
                    cell.positive_probes,
                    cell.probes - cell.positive_probes
                ));
                unproven += 1;
            }
        }
    }
    for arm in Arm::ALL {
        let Some(report) = matrix.arms.iter().find(|a| a.arm == arm) else {
            continue;
        };
        if report.probes == report.positive_probes && report.probes > 0 {
            out.push_str(&format!(
                "- `{}`: no out-of-scope probe could be issued for this arm, so nothing was \
                 actually looked for outside a scope.\n",
                arm.as_str()
            ));
            unproven += 1;
        }
    }
    if matrix.attribution.heuristic > 0 {
        out.push_str(&format!(
            "- {} of {} probe answers carried no `retrieval.arms`, so their arms were read off \
             the chunk shape (`chunkType` present or not). That heuristic is weaker evidence \
             than the server naming the arm, and it is exactly what made the keyword arm \
             UNTESTED before step S.1c.\n",
            matrix.attribution.heuristic,
            matrix.attribution.provenance + matrix.attribution.heuristic,
        ));
        unproven += 1;
    }
    if unproven == 0 && matrix.notes.is_empty() {
        out.push_str("- nothing beyond the cells above\n");
    }
    for note in &matrix.notes {
        out.push_str(&format!("- {note}\n"));
    }
    out.push_str(
        "\nA cell marked UNTESTED found no leak because nothing was retrieved, not because \
         retrieval was contained. It is a hole in the evidence, not a pass.\n",
    );
    out
}

/// Group scopes by kind, for a compact summary line.
#[must_use]
pub fn scopes_by_kind(plan: &SeedPlan) -> BTreeMap<ScopeKind, usize> {
    let mut counts: BTreeMap<ScopeKind, usize> = BTreeMap::new();
    for scope in &plan.scopes {
        *counts.entry(scope.kind).or_default() += 1;
    }
    counts
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn canaries(n: usize) -> Canaries {
        Canaries {
            bm25: format!("qzx{n}"),
            vector: format!("The Qzx{n} committee reconvened at dawn."),
            entity: format!("Qzx{n} Vorlath"),
            figure: format!("Figure {n}: the Qzx{n} apparatus"),
            neighbour: format!("A neighbouring passage about Qzx{n}."),
        }
    }

    fn doc(id: &str, tenant: &str, kb: &str, category: &str, n: usize) -> SeedDocument {
        SeedDocument {
            id: id.to_string(),
            tenant: tenant.to_string(),
            knowledge_base_id: kb.to_string(),
            category_id: category.to_string(),
            canaries: canaries(n),
            n_chunks: 2,
            has_figures: false,
            figure_asset_key: None,
        }
    }

    fn corpus() -> Vec<SeedDocument> {
        vec![
            doc("a-kb0-c0", "A", "kb0", "cat0", 1),
            doc("a-kb1-c0", "A", "kb1", "cat0", 2),
            doc("a-kb0-c1", "A", "kb0", "cat1", 3),
            doc("a-kb1-c1", "A", "kb1", "cat1", 4),
            doc("b-kb0-c0", "B", "kb0", "cat0", 5),
        ]
    }

    #[test]
    fn the_matrix_covers_every_arm_and_every_scope_kind() {
        let plan = plan(corpus());
        for kind in ScopeKind::ALL {
            assert!(
                plan.scopes.iter().any(|s| s.kind == kind),
                "no scope of kind {kind:?}"
            );
        }
        for arm in Arm::ALL {
            assert!(
                plan.probes.iter().any(|p| p.arm == arm),
                "no probe for arm {arm:?}"
            );
        }
        // Both tenants appear, and each scope is issued under one of them.
        assert!(plan.scopes.iter().any(|s| s.tenant == "A"));
        assert!(plan.scopes.iter().any(|s| s.tenant == "B"));
    }

    #[test]
    fn a_scope_only_allows_its_own_documents() {
        let plan = plan(corpus());
        let kb = plan
            .scopes
            .iter()
            .find(|s| s.kind == ScopeKind::KnowledgeBase && s.tenant == "A")
            .expect("a knowledge-base scope");
        assert_eq!(kb.knowledge_base_ids.len(), 1);
        for id in &kb.allowed {
            let d = plan.document(id).expect("a seeded document");
            assert_eq!(d.tenant, "A");
            assert_eq!(&d.knowledge_base_id, &kb.knowledge_base_ids[0]);
        }
        // The other tenant's identically-named knowledge base is not in it.
        assert!(!kb.allowed.contains(&"b-kb0-c0".to_string()));
    }

    #[test]
    fn every_leak_probe_uses_a_canary_from_outside_its_scope() {
        let plan = plan(corpus());
        for probe in plan.probes.iter().filter(|p| !p.positive) {
            let scope = plan.scope(probe.scope).expect("a scope");
            assert!(
                !scope.allowed.contains(&probe.canary_owner),
                "probe {:?} used a canary that was inside its own scope",
                probe
            );
        }
        for probe in plan.probes.iter().filter(|p| p.positive) {
            let scope = plan.scope(probe.scope).expect("a scope");
            assert!(scope.allowed.contains(&probe.canary_owner));
        }
    }

    #[test]
    fn the_probe_budget_is_capped_and_deterministic() {
        let one = plan(corpus());
        let two = plan(corpus());
        assert_eq!(one.probes.len(), two.probes.len());
        for (a, b) in one.probes.iter().zip(two.probes.iter()) {
            assert_eq!(a.arm, b.arm);
            assert_eq!(a.scope, b.scope);
            assert_eq!(a.query, b.query);
        }
        for scope_index in 0..one.scopes.len() {
            let per_scope = one
                .probes
                .iter()
                .filter(|p| p.scope == scope_index && !p.positive)
                .count();
            let outside = one.outside(one.scope(scope_index).unwrap()).len();
            assert!(
                per_scope == outside.min(MAX_OUTSIDE_PER_SCOPE) * Arm::ALL.len(),
                "scope {scope_index}: {per_scope} probes for {outside} outside documents"
            );
        }
    }

    #[test]
    fn an_answer_naming_a_document_outside_the_scope_is_a_leak() {
        let scope = vec!["in-1".to_string()];
        let answer = Answer {
            documents: vec!["in-1".into(), "out-1".into(), "out-1".into()],
            ..Answer::default()
        };
        // Four chunks of one foreign document is one leak, not four.
        assert_eq!(leaks_in(&answer, &scope), vec!["out-1".to_string()]);
        assert!(positive_hit(&answer, "in-1"));
        assert!(!positive_hit(&answer, "never-retrieved"));
    }

    #[test]
    fn an_empty_answer_leaks_nothing_and_also_hits_nothing() {
        let answer = Answer::default();
        assert!(leaks_in(&answer, &["in-1".to_string()]).is_empty());
        assert!(!positive_hit(&answer, "in-1"));
        assert_eq!(answer.http_status, 0);
    }

    #[test]
    fn no_leak_without_a_positive_control_is_untested_not_pass() {
        // The vacuous-pass trap: a search that returns nothing finds nothing
        // out of scope either.
        assert_eq!(Status::decide(0, 0, 0), Status::Untested);
        assert_eq!(Status::decide(0, 3, 0), Status::Untested);
        assert_eq!(Status::decide(0, 3, 3), Status::Pass);
        // A leak wins over everything, including a missing positive control.
        assert_eq!(Status::decide(1, 0, 0), Status::Leak);
        assert_eq!(Status::decide(2, 3, 3), Status::Leak);
        assert!(
            Status::Untested
                .untested_reason(3, 0)
                .is_some_and(|r| r.contains("proves nothing"))
        );
        assert!(Status::Pass.untested_reason(3, 3).is_none());
    }

    #[test]
    fn a_matrix_with_a_leak_is_a_leak() {
        let mut matrix = LeakMatrix::new("http://x");
        // Tenant A's knowledge base 0 scope; document b-kb0-c0 is outside it.
        let plan = plan(corpus());
        let index = plan
            .scopes
            .iter()
            .position(|s| s.kind == ScopeKind::KnowledgeBase && s.tenant == "A")
            .expect("a kb scope");
        let scope = plan.scope(index).unwrap();
        matrix.record(Observation {
            arm: Arm::Vector,
            scope: ScopeKind::KnowledgeBase,
            positive: false,
            hit: false,
            leaked: vec!["b-kb0-c0".to_string()],
            errored: false,
            refused: false,
            degrades: Vec::new(),
            attribution: Attribution::Provenance,
            example: Some(LeakExample {
                arm: Arm::Vector,
                scope: scope.label.clone(),
                query: "canary".to_string(),
                canary_owner: "b-kb0-c0".to_string(),
                request: serde_json::json!({"query": "canary"}),
                foreign_document_ids: vec!["b-kb0-c0".to_string()],
                response: serde_json::json!({"chunks": []}),
                degrades: Vec::new(),
            }),
        });
        let matrix = matrix.finish();
        assert!(matrix.any_leak());
        let vector = matrix.arms.iter().find(|a| a.arm == Arm::Vector).unwrap();
        assert_eq!(vector.status, Status::Leak);
        assert_eq!(vector.leaks, 1);
        assert_eq!(vector.leak_examples.len(), 1);
        assert_eq!(vector.cells[0].status, Status::Leak);
    }

    #[test]
    fn a_cell_with_a_leak_is_a_leak_even_when_the_arm_is_untested() {
        // An arm with a zero positive rate and one leak must not be filed as
        // UNTESTED: there is a leak in it.
        let mut matrix = LeakMatrix::new("http://x");
        matrix.record(Observation {
            arm: Arm::Figure,
            scope: ScopeKind::Category,
            positive: false,
            hit: false,
            leaked: vec!["x".to_string()],
            errored: false,
            refused: false,
            degrades: Vec::new(),
            attribution: Attribution::Provenance,
            example: None,
        });
        let matrix = matrix.finish();
        assert!(matrix.any_leak());
        assert_eq!(matrix.arms[0].status, Status::Leak);
    }

    #[test]
    fn a_clean_matrix_with_positive_controls_passes_and_records_degrades() {
        let mut matrix = LeakMatrix::new("http://x");
        matrix.record(Observation {
            arm: Arm::Bm25,
            scope: ScopeKind::Tenant,
            positive: true,
            hit: true,
            leaked: Vec::new(),
            errored: false,
            refused: false,
            degrades: Vec::new(),
            attribution: Attribution::Provenance,
            example: None,
        });
        matrix.record(Observation {
            arm: Arm::Bm25,
            scope: ScopeKind::Tenant,
            positive: false,
            hit: false,
            leaked: Vec::new(),
            errored: false,
            refused: false,
            degrades: vec!["vector arm down".to_string()],
            attribution: Attribution::Provenance,
            example: None,
        });
        let matrix = matrix.finish();
        assert!(!matrix.any_leak());
        assert!(!matrix.any_untested());
        assert_eq!(matrix.total_probes(), 2);
        // A degradation is surfaced, never swallowed.
        assert!(matrix.degrades.contains("vector arm down"));
        let md = render_markdown(&matrix, &plan(corpus()));
        assert!(md.contains("## Matrix (arm × scope type)"), "{md}");
        assert!(md.contains("vector arm down"), "{md}");
        assert!(md.contains("UNTESTED"), "untested arms must be named: {md}");
        assert!(md.contains("What this run does not prove"), "{md}");
    }

    #[test]
    fn a_refusal_is_evidence_not_an_error() {
        let mut matrix = LeakMatrix::new("http://x");
        matrix.record(Observation {
            arm: Arm::Entity,
            scope: ScopeKind::Tenant,
            positive: false,
            hit: false,
            leaked: Vec::new(),
            errored: false,
            refused: true,
            degrades: Vec::new(),
            attribution: Attribution::Provenance,
            example: None,
        });
        matrix.record(Observation {
            arm: Arm::Entity,
            scope: ScopeKind::Tenant,
            positive: true,
            hit: true,
            leaked: Vec::new(),
            errored: false,
            refused: false,
            degrades: Vec::new(),
            attribution: Attribution::Provenance,
            example: None,
        });
        let matrix = matrix.finish();
        let entity = &matrix.arms[0];
        assert_eq!(entity.refusals, 1);
        assert_eq!(entity.errors, 0, "a 404 is the guard working");
        assert_eq!(entity.status, Status::Pass);
        assert_eq!(matrix.total_refusals(), 1);
        let md = render_markdown(&matrix, &plan(corpus()));
        assert!(md.contains("refused with 4xx: 1"), "{md}");
    }

    #[test]
    fn errors_are_counted_and_a_failed_answer_is_not_a_pass() {
        let mut matrix = LeakMatrix::new("http://x");
        matrix.record(Observation {
            arm: Arm::Vector,
            scope: ScopeKind::Tenant,
            positive: true,
            hit: false,
            leaked: Vec::new(),
            errored: true,
            refused: false,
            degrades: Vec::new(),
            attribution: Attribution::Provenance,
            example: None,
        });
        let matrix = matrix.finish();
        let vector = &matrix.arms[0];
        assert_eq!(vector.errors, 1);
        assert_eq!(vector.status, Status::Untested);
        let md = render_markdown(&matrix, &plan(corpus()));
        assert!(md.contains("http errors: 1"), "{md}");
    }

    #[test]
    fn an_arm_with_any_untested_cell_is_partial_not_pass() {
        let mut matrix = LeakMatrix::new("http://x");
        matrix.record(Observation {
            arm: Arm::Bm25,
            scope: ScopeKind::Tenant,
            positive: true,
            hit: true,
            leaked: Vec::new(),
            errored: false,
            refused: false,
            degrades: Vec::new(),
            attribution: Attribution::Provenance,
            example: None,
        });
        matrix.record(Observation {
            arm: Arm::Bm25,
            scope: ScopeKind::KnowledgeBase,
            positive: true,
            hit: true,
            leaked: Vec::new(),
            errored: false,
            refused: false,
            degrades: Vec::new(),
            attribution: Attribution::Provenance,
            example: None,
        });
        matrix.record(Observation {
            arm: Arm::Bm25,
            scope: ScopeKind::DocumentIds,
            positive: true,
            hit: false,
            leaked: Vec::new(),
            errored: false,
            refused: false,
            degrades: Vec::new(),
            attribution: Attribution::Provenance,
            example: None,
        });
        let matrix = matrix.finish();
        let bm25 = matrix.arms.iter().find(|a| a.arm == Arm::Bm25).unwrap();
        assert_eq!(
            bm25.cells
                .iter()
                .find(|c| c.scope == ScopeKind::Tenant)
                .unwrap()
                .status,
            Status::Pass
        );
        assert_eq!(
            bm25.cells
                .iter()
                .find(|c| c.scope == ScopeKind::DocumentIds)
                .unwrap()
                .status,
            Status::Untested
        );
        assert_eq!(bm25.status, Status::Partial);
        assert_eq!(bm25.status.as_str(), "PARTIAL");
        assert!(matrix.any_untested());
    }

    #[test]
    fn arm_status_resolution_follows_hierarchy() {
        let make_cell = |status: Status, leaks: usize| Cell {
            status,
            leaks,
            ..Cell::default()
        };

        // All PASS -> PASS
        assert_eq!(
            Status::decide_arm(&[make_cell(Status::Pass, 0), make_cell(Status::Pass, 0)]),
            Status::Pass
        );
        // Any UNTESTED with PASS -> PARTIAL
        assert_eq!(
            Status::decide_arm(&[make_cell(Status::Pass, 0), make_cell(Status::Untested, 0)]),
            Status::Partial
        );
        // All UNTESTED -> UNTESTED
        assert_eq!(
            Status::decide_arm(&[
                make_cell(Status::Untested, 0),
                make_cell(Status::Untested, 0)
            ]),
            Status::Untested
        );
        // Any LEAK wins over everything -> LEAK
        assert_eq!(
            Status::decide_arm(&[make_cell(Status::Pass, 0), make_cell(Status::Leak, 1)]),
            Status::Leak
        );
        assert_eq!(
            Status::decide_arm(&[make_cell(Status::Untested, 0), make_cell(Status::Leak, 1)]),
            Status::Leak
        );
        assert_eq!(
            Status::decide_arm(&[
                make_cell(Status::Pass, 0),
                make_cell(Status::Untested, 0),
                make_cell(Status::Leak, 1)
            ]),
            Status::Leak
        );
        // Empty -> UNTESTED
        assert_eq!(Status::decide_arm(&[]), Status::Untested);
    }

    #[test]
    fn every_arm_has_a_coverage_note() {
        for arm in Arm::ALL {
            assert!(!coverage_note(arm).is_empty(), "{arm:?}");
            assert!(arm.as_str().is_ascii());
        }
        assert_eq!(Arm::Entity.probe_kind(), ProbeKind::DocumentIntelligence);
        assert_eq!(Arm::Vector.probe_kind(), ProbeKind::Search);
        assert_eq!(Arm::Figure.probe_kind(), ProbeKind::SearchOrAsset);
    }
}
