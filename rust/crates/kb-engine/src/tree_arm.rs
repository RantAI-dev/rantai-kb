//! The tree arm (`DEC-4`): answer structural questions by walking a document's
//! section hierarchy.
//!
//! EVAL-2 measured a +0.086 nDCG@10 on structural questions ("what does chapter
//! 2 say about…") and no change on ordinary ones, when the arm ran only for the
//! queries the production classifier labels [`QueryClass::Structural`]. This
//! module is that arm: it reads the outlines the ingest worker stored, scores
//! their nodes with `kb_core::tree`, and hands back the chunks on the winning
//! nodes' pages.
//!
//! What this module does **not** do: decide whether to run (the pipeline checks
//! the flag and the classifier), fetch outlines without a scope (that is
//! [`DocumentStore::outlines`]'s contract), or reorder the final answer (it
//! returns a ranked list the pipeline fuses by weighted RRF).
//!
//! # Scope
//!
//! Two independent locks, because a tree arm is a second way to reach a chunk:
//!
//! 1. [`DocumentStore::outlines`] takes the caller's [`DocumentScope`], so a
//!    document outside it contributes no nodes to score.
//! 2. [`RetrievalIndex::fetch_pages`] takes the same scope and repeats the
//!    tenant + document condition in the adapter's own filter. A page number
//!    alone is not a document, and a chunk id is never constructed here.
//!
//! The engine test suite and the leak matrix both probe a matching outline
//! outside the scope, so "it cannot happen" is checked rather than assumed.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use kb_core::tree::{
    DEFAULT_MAX_PAGES, DEFAULT_NODES, DEFAULT_PAGES_PER_NODE, DEFAULT_RRF_K, DEFAULT_WEIGHT,
    Outline, TreeDocument, node_pages, score_tree,
};
use kb_ports::{
    Candidate, ChunkId, DocumentId, DocumentScope, DocumentStore, PortError, RetrievalIndex,
    sort_candidates,
};

/// Chunks read per page. A page is a coarse unit (a dense report can have a
/// dozen chunks on one), and the reranker only sees the candidate pool, so four
/// is enough for the section-level answer the arm exists for.
pub const DEFAULT_PER_PAGE: usize = 4;

/// Tunables, one per environment variable.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TreeArmConfig {
    /// `KB_TREE_ARM_WEIGHT`: RRF weight of the arm's list.
    pub weight: f32,
    /// `KB_TREE_ARM_NODES`: outline nodes kept per query.
    pub nodes: usize,
    /// Pages taken from each chosen node, heading page first.
    pub pages_per_node: u32,
    /// Hard cap on distinct pages one query asks for.
    pub max_pages: usize,
    /// Chunks read per page.
    pub per_page: usize,
    /// RRF constant, the same one the rest of the pipeline uses.
    pub rrf_k: u32,
}

impl Default for TreeArmConfig {
    fn default() -> Self {
        Self {
            weight: DEFAULT_WEIGHT,
            nodes: DEFAULT_NODES,
            pages_per_node: DEFAULT_PAGES_PER_NODE,
            max_pages: DEFAULT_MAX_PAGES,
            per_page: DEFAULT_PER_PAGE,
            rrf_k: DEFAULT_RRF_K,
        }
    }
}

/// What the arm did, for `stats.treeArm`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TreeStats {
    /// Outline nodes the query scored positively and the arm kept.
    pub nodes: usize,
    /// Distinct pages it then asked the index for.
    pub pages: usize,
}

/// The arm's output: chunks ordered by the winning nodes' pages, plus its
/// counts.
#[derive(Debug, Clone, Default)]
pub struct TreeRanking {
    pub chunks: Vec<Candidate>,
    pub stats: TreeStats,
}

/// The arm itself. Stateless beyond its settings, so one instance serves every
/// request.
#[derive(Debug, Clone)]
pub struct TreeArm {
    pub config: TreeArmConfig,
}

impl TreeArm {
    #[must_use]
    pub fn new(config: TreeArmConfig) -> Self {
        Self { config }
    }

    /// Score the scoped documents' outlines and read the chunks on the best
    /// nodes' pages.
    ///
    /// An empty scope, a corpus with no outlines and a query that matches no
    /// heading all return an empty ranking without an error: "the tree has
    /// nothing to say" is not a failure. A store or index error is returned, so
    /// the pipeline can record `tree_arm_failed` rather than pretend the arm
    /// found nothing.
    pub async fn rank(
        &self,
        documents: &Arc<dyn DocumentStore>,
        index: &Arc<dyn RetrievalIndex>,
        scope: &DocumentScope,
        query: &str,
    ) -> Result<TreeRanking, PortError> {
        if scope.document_ids().is_empty() {
            return Ok(TreeRanking::default());
        }
        let outlines = documents.outlines(scope).await?;
        let outlines: Vec<(DocumentId, Outline)> = outlines
            .into_iter()
            // Belt and braces: the adapter promises this, and the arm does not
            // depend on the promise to stay scoped.
            .filter(|(id, _)| scope.document_ids().contains(id))
            .collect();
        if outlines.is_empty() {
            return Ok(TreeRanking::default());
        }

        let tree_documents: Vec<TreeDocument<'_>> = outlines
            .iter()
            .map(|(id, outline)| TreeDocument {
                key: id.as_str(),
                title: outline.document_title.as_deref().unwrap_or_default(),
                nodes: &outline.nodes,
            })
            .collect();
        let matches = score_tree(query, &tree_documents);
        let chosen = &matches[..matches.len().min(self.config.nodes.max(1))];
        if chosen.is_empty() {
            return Ok(TreeRanking::default());
        }

        let mut wanted: Vec<(DocumentId, u32)> = Vec::new();
        let mut seen: HashSet<(usize, u32)> = HashSet::new();
        for hit in chosen {
            let document = &outlines[hit.document].0;
            let node = &tree_documents[hit.document].nodes[hit.node];
            for page in node_pages(node, self.config.pages_per_node) {
                if wanted.len() >= self.config.max_pages {
                    break;
                }
                if seen.insert((hit.document, page)) {
                    wanted.push((document.clone(), page));
                }
            }
            if wanted.len() >= self.config.max_pages {
                break;
            }
        }
        if wanted.is_empty() {
            return Ok(TreeRanking::default());
        }

        let mut chunks = index
            .fetch_pages(scope, &wanted, self.config.per_page)
            .await?;
        // The pages are already in node-score order; within a page the store
        // returns chunk order. Anything the store returned on a page that was
        // not requested (it must not) is dropped rather than trusted.
        let page_rank: HashMap<(String, u32), usize> = wanted
            .iter()
            .enumerate()
            .map(|(rank, (document, page))| ((document.as_str().to_string(), *page), rank))
            .collect();
        chunks.retain(|chunk| {
            chunk.page.is_some_and(|page| {
                page_rank.contains_key(&(chunk.document_id.as_str().to_string(), page))
            })
        });
        chunks.sort_by(|a, b| {
            let rank = |c: &Candidate| {
                c.page
                    .and_then(|page| {
                        page_rank
                            .get(&(c.document_id.as_str().to_string(), page))
                            .copied()
                    })
                    .unwrap_or(usize::MAX)
            };
            rank(a)
                .cmp(&rank(b))
                .then_with(|| a.chunk_index.cmp(&b.chunk_index))
        });

        Ok(TreeRanking {
            stats: TreeStats {
                nodes: chosen.len(),
                pages: wanted.len(),
            },
            chunks,
        })
    }
}

/// Weighted reciprocal-rank fusion of the text candidate list with the tree
/// arm's list.
///
/// `score(chunk) = 1/(k + rank_text) + weight/(k + rank_tree)`, the formula
/// `tree_fuse.py` measured with: the text retriever keeps full authority and
/// the tree list enters at half weight, because an outline is a structural hint
/// rather than a second retriever. Existing candidates keep their text-arm
/// scores and provenance; a chunk only the tree arm found carries none, which
/// is the honest answer for a chunk no arm scored.
#[must_use]
pub fn fuse_tree(
    base: Vec<Candidate>,
    tree: Vec<Candidate>,
    config: &TreeArmConfig,
) -> Vec<Candidate> {
    if tree.is_empty() {
        return base;
    }
    let k = f64::from(config.rrf_k.max(1));
    let weight = f64::from(config.weight.max(0.0));
    let mut order: Vec<ChunkId> = Vec::with_capacity(base.len() + tree.len());
    let mut candidates: HashMap<ChunkId, Candidate> = HashMap::new();
    let mut scores: HashMap<ChunkId, f64> = HashMap::new();

    for (rank, candidate) in base.into_iter().enumerate() {
        let id = candidate.unit_id.clone();
        *scores.entry(id.clone()).or_insert(0.0) += 1.0 / (k + rank as f64 + 1.0);
        if candidates.insert(id.clone(), candidate).is_none() {
            order.push(id);
        }
    }
    for (rank, candidate) in tree.into_iter().enumerate() {
        let id = candidate.unit_id.clone();
        *scores.entry(id.clone()).or_insert(0.0) += weight / (k + rank as f64 + 1.0);
        if candidates.insert(id.clone(), candidate).is_none() {
            order.push(id);
        }
    }

    let mut fused: Vec<Candidate> = order
        .into_iter()
        .filter_map(|id| {
            let mut candidate = candidates.remove(&id)?;
            candidate.scores.fused = scores.get(&id).copied().unwrap_or(0.0) as f32;
            Some(candidate)
        })
        .collect();
    sort_candidates(&mut fused);
    fused
}

/// The ids the tree arm contributed, for provenance. Computed before the tree
/// list is moved into [`fuse_tree`].
#[must_use]
pub fn tree_chunk_ids(chunks: &[Candidate]) -> HashSet<ChunkId> {
    chunks.iter().map(|chunk| chunk.unit_id.clone()).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use kb_ports::{ArmRanks, ArmScores, ChunkType, TenantId};

    fn candidate(document: &str, index: u32, page: Option<u32>, content: &str) -> Candidate {
        Candidate {
            unit_id: ChunkId(format!("{document}_{index}")),
            document_id: DocumentId(document.to_string()),
            tenant: TenantId("tenant-a".to_string()),
            chunk_index: index,
            page,
            chunk_type: ChunkType::Text,
            content: content.to_string(),
            contextual_prefix: None,
            metadata: serde_json::json!({}),
            scores: ArmScores::default(),
            ranks: ArmRanks::default(),
        }
    }

    #[test]
    fn the_arm_list_enters_at_its_weight() {
        let config = TreeArmConfig {
            weight: 0.5,
            rrf_k: 60,
            ..TreeArmConfig::default()
        };
        let base = vec![
            candidate("d1", 0, Some(0), "a"),
            candidate("d1", 1, Some(1), "b"),
        ];
        let tree = vec![candidate("d1", 9, Some(40), "tree-only")];
        let fused = fuse_tree(base, tree, &config);
        // Tree-only chunk gets 0.5/61 ≈ 0.0082; the base rank-1 chunk gets
        // 1/61 ≈ 0.0164, so the text list still leads.
        assert_eq!(fused[0].content, "a");
        assert_eq!(fused[1].content, "b");
        assert_eq!(fused[2].content, "tree-only");
        assert!((fused[2].scores.fused - (0.5 / 61.0) as f32).abs() < 1e-6);
    }

    #[test]
    fn a_chunk_in_both_lists_gains_both_contributions() {
        let config = TreeArmConfig::default();
        let base = vec![candidate("d1", 0, Some(0), "shared")];
        let tree = vec![candidate("d1", 0, Some(0), "shared")];
        let fused = fuse_tree(base, tree, &config);
        assert_eq!(fused.len(), 1);
        let expected = 1.0 / 61.0 + 0.5 / 61.0;
        assert!((fused[0].scores.fused as f64 - expected).abs() < 1e-6);
        // The text arm's candidate object is the one kept.
        assert_eq!(fused[0].unit_id.as_str(), "d1_0");
    }

    #[test]
    fn an_empty_tree_list_returns_the_candidates_untouched() {
        let base = vec![candidate("d1", 0, Some(0), "a")];
        let fused = fuse_tree(base.clone(), Vec::new(), &TreeArmConfig::default());
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].scores.fused, base[0].scores.fused);
    }

    #[test]
    fn provenance_ids_are_the_arm_list() {
        let tree = vec![
            candidate("d1", 3, Some(4), "x"),
            candidate("d2", 7, Some(8), "y"),
        ];
        let ids = tree_chunk_ids(&tree);
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&ChunkId("d1_3".into())));
    }
}
