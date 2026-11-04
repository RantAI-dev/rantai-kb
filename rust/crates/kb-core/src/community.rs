//! Graph community detection and summary ranking ("global mode", `DEC-5`).
//!
//! This is the pure half of the graph-community arm: deterministic Louvain over
//! the entity co-occurrence graph ingest already stores, the hub-entity cap that
//! keeps a corpus-wide term from welding every topic into one community, and
//! BM25 over the community summaries. No I/O, no model, no clock — the same
//! input gives the same communities on every machine, which is the property the
//! storage layer and the cache key depend on.
//!
//! # Where the numbers come from
//!
//! The implementation is a port of the `EVAL-3` prototype
//! (`rust/crates/kb-eval/scripts/community_build.py` and `global_arm.py`), which
//! measured a **loss** for this arm even on the aggregative questions it
//! targets (`docs/eval-ab/retrieval-gaps/README.md` § `EVAL-3`). It is built
//! behind `KB_GLOBAL_ARM`, off by default, because the product owner asked for
//! it despite that result (`DEC-5`).
//!
//! # Determinism
//!
//! Nodes are visited in sorted order, a tie in modularity goes to the lowest
//! community id, and communities are renumbered by `(size desc, first member
//! asc)`. Nothing iterates a `HashMap`, so two runs — and two machines — cannot
//! disagree about the community set.

use std::collections::{BTreeMap, BTreeSet};

use crate::sparse::tokenize;

/// One detected community, in the shape the builder writes to storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Community {
    /// Entity keys in the community, sorted.
    pub entity_keys: Vec<String>,
    /// Chunk ids assigned to the community (majority of their entities), sorted.
    pub chunk_ids: Vec<String>,
}

/// Louvain options.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DetectOptions {
    /// Modularity resolution. `1.0` is plain Louvain; higher yields more,
    /// smaller communities.
    pub resolution: f32,
    /// Drop an entity seen on more than this fraction of the documents from the
    /// graph. `0.0` disables the cap. The prototype used `0.05`.
    pub max_df_fraction: f32,
    /// Communities with fewer chunks than this are not returned. The prototype
    /// summarised only communities with at least 2 pages (`--min-pages 2`).
    pub min_chunks: usize,
}

impl Default for DetectOptions {
    fn default() -> Self {
        Self {
            resolution: 1.0,
            max_df_fraction: 0.05,
            min_chunks: 2,
        }
    }
}

/// Entities that appear on more than `max_df_fraction` of the documents.
///
/// A surface extractor finds "Member States" and "access" on hundreds of
/// documents; leaving them in the graph joins every topic into one community
/// whose summary says nothing. They are returned (and therefore reported by the
/// builder) rather than silently dropped.
///
/// `document_entities` maps an entity key to the documents that mention it.
/// The denominator is the number of *distinct documents* that mention anything,
/// not the corpus size: a tenant with 10,000 documents and 40 with entities
/// should not have a 5%-of-10,000 threshold.
#[must_use]
pub fn hub_entities(
    document_entities: &BTreeMap<String, BTreeSet<String>>,
    max_df_fraction: f32,
) -> Vec<String> {
    if max_df_fraction <= 0.0 {
        return Vec::new();
    }
    let documents: BTreeSet<&str> = document_entities
        .values()
        .flat_map(|docs| docs.iter().map(String::as_str))
        .collect();
    let total = documents.len();
    if total == 0 {
        return Vec::new();
    }
    // The prototype's `max(2, int(frac * n))`: a cap of 1 would drop every
    // entity that appears on two documents, which is ordinary vocabulary.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let limit = 2usize.max((max_df_fraction * total as f32) as usize);
    let mut hubs: Vec<(usize, &String)> = document_entities
        .iter()
        .filter(|(_, docs)| docs.len() > limit)
        .map(|(key, docs)| (docs.len(), key))
        .collect();
    hubs.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    hubs.into_iter().map(|(_, key)| key.clone()).collect()
}

/// Weighted entity co-occurrence: one edge per pair of entities that share a
/// chunk, weighted by how many chunks they share.
#[must_use]
pub fn cooccurrence(chunk_entities: &BTreeMap<String, Vec<String>>) -> Vec<(String, String, f32)> {
    let mut edges: BTreeMap<(String, String), f32> = BTreeMap::new();
    for entities in chunk_entities.values() {
        let unique: BTreeSet<&String> = entities.iter().collect();
        let unique: Vec<&String> = unique.into_iter().collect();
        for (index, left) in unique.iter().enumerate() {
            for right in &unique[index + 1..] {
                let (a, b) = if left <= right {
                    (left, right)
                } else {
                    (right, left)
                };
                *edges.entry(((*a).clone(), (*b).clone())).or_insert(0.0) += 1.0;
            }
        }
    }
    edges
        .into_iter()
        .map(|((left, right), weight)| (left, right, weight))
        .collect()
}

/// Deterministic weighted Louvain. Returns `node -> community id`, renumbered
/// by `(size desc, first member asc)` so the ids are stable and readable.
#[must_use]
pub fn louvain(
    nodes: &[String],
    edges: &[(String, String, f32)],
    resolution: f32,
) -> BTreeMap<String, usize> {
    let unique: BTreeSet<&String> = nodes.iter().collect();
    let keys: Vec<&String> = unique.into_iter().collect();
    let index: BTreeMap<&String, usize> = keys
        .iter()
        .enumerate()
        .map(|(id, key)| (*key, id))
        .collect();
    let n = index.len();
    if n == 0 {
        return BTreeMap::new();
    }
    let mut graph: Vec<BTreeMap<usize, f64>> = vec![BTreeMap::new(); n];
    for (left, right, weight) in edges {
        let (Some(&a), Some(&b)) = (index.get(left), index.get(right)) else {
            continue;
        };
        if a == b {
            continue;
        }
        let weight = f64::from(*weight);
        *graph[a].entry(b).or_insert(0.0) += weight;
        *graph[b].entry(a).or_insert(0.0) += weight;
    }

    let mut membership: Vec<usize> = (0..n).collect();
    for _ in 0..20 {
        let community = one_level(&graph, resolution);
        let groups: BTreeSet<usize> = community.iter().copied().collect();
        if groups.len() == graph.len() {
            break;
        }
        let (next, renumber) = aggregate(&graph, &community);
        membership = membership
            .iter()
            .map(|node| renumber[&community[*node]])
            .collect();
        graph = next;
    }

    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (node, label) in membership.iter().enumerate() {
        groups.entry(*label).or_default().push(node);
    }
    let mut ordered: Vec<Vec<usize>> = groups.into_values().collect();
    ordered.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a[0].cmp(&b[0])));

    let mut labels: BTreeMap<String, usize> = BTreeMap::new();
    for (label, members) in ordered.iter().enumerate() {
        for node in members {
            // `keys` is the sorted node order the ids were built from.
            labels.insert(keys[*node].clone(), label);
        }
    }
    labels
}

/// One Louvain pass: move each node to the neighbouring community that gains
/// the most modularity, visiting nodes in id order. Ties go to the lower id.
fn one_level(graph: &[BTreeMap<usize, f64>], resolution: f32) -> Vec<usize> {
    let n = graph.len();
    let degree: Vec<f64> = graph.iter().map(|edges| edges.values().sum()).collect();
    let m2 = degree.iter().sum::<f64>();
    let m2 = if m2 > 0.0 { m2 } else { 1.0 };
    let resolution = f64::from(resolution);

    let mut community: Vec<usize> = (0..n).collect();
    let mut total: Vec<f64> = degree.clone();
    loop {
        let mut moved = false;
        for (node, edges) in graph.iter().enumerate() {
            let current = community[node];
            let k = degree[node];
            total[current] -= k;

            let mut weights: BTreeMap<usize, f64> = BTreeMap::new();
            for (&other, &weight) in edges {
                if other == node {
                    continue;
                }
                *weights.entry(community[other]).or_insert(0.0) += weight;
            }

            let mut best = current;
            let mut best_gain = weights.get(&current).copied().unwrap_or(0.0)
                - resolution * total[current] * k / m2;
            for (&candidate, &weight) in &weights {
                if candidate == current {
                    continue;
                }
                let gain =
                    weight - resolution * total.get(candidate).copied().unwrap_or(0.0) * k / m2;
                if gain > best_gain + 1e-12 {
                    best = candidate;
                    best_gain = gain;
                }
            }

            community[node] = best;
            total[best] += k;
            if best != current {
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    community
}

/// Collapse each community into one node. Internal edges become a self-loop:
/// dropping them (the obvious shortcut) loses the community's cohesion and the
/// next level then merges everything into one community at modularity 0.
fn aggregate(
    graph: &[BTreeMap<usize, f64>],
    community: &[usize],
) -> (Vec<BTreeMap<usize, f64>>, BTreeMap<usize, usize>) {
    let groups: BTreeSet<usize> = community.iter().copied().collect();
    let renumber: BTreeMap<usize, usize> = groups
        .iter()
        .enumerate()
        .map(|(index, group)| (*group, index))
        .collect();
    let mut next: Vec<BTreeMap<usize, f64>> = vec![BTreeMap::new(); groups.len()];
    for (node, edges) in graph.iter().enumerate() {
        let a = renumber[&community[node]];
        for (&other, &weight) in edges {
            let b = renumber[&community[other]];
            *next[a].entry(b).or_insert(0.0) += weight;
        }
    }
    (next, renumber)
}

/// Assign each chunk to the community holding most of its entities.
///
/// A chunk joins exactly one community; ties go to the lowest community id.
/// Chunks with no entity in `labels` (because every entity they mention was a
/// hub, or the chunk yielded nothing) are absent from the result rather than
/// silently assigned to community 0.
#[must_use]
pub fn assign_chunks(
    chunk_entities: &BTreeMap<String, Vec<String>>,
    labels: &BTreeMap<String, usize>,
) -> BTreeMap<usize, Vec<String>> {
    let mut members: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for (chunk, entities) in chunk_entities {
        let mut votes: BTreeMap<usize, usize> = BTreeMap::new();
        for key in entities {
            if let Some(label) = labels.get(key) {
                *votes.entry(*label).or_insert(0) += 1;
            }
        }
        let Some(best) = votes
            .iter()
            .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
            .map(|(label, _)| *label)
        else {
            continue;
        };
        members.entry(best).or_default().push(chunk.clone());
    }
    for chunks in members.values_mut() {
        chunks.sort();
    }
    members
}

/// Build communities from per-chunk entity keys: cap hubs, cluster, assign.
///
/// The one entry point the build command calls. Communities below
/// `options.min_chunks` are dropped; the rest are ordered by
/// `(chunks desc, entities desc, first chunk asc)` and renumbered, which is the
/// prototype's order and makes the stored ids a function of the graph.
#[must_use]
pub fn detect(
    chunk_entities: &BTreeMap<String, Vec<String>>,
    document_entities: &BTreeMap<String, BTreeSet<String>>,
    options: DetectOptions,
) -> (Vec<Community>, Vec<String>) {
    let hubs = hub_entities(document_entities, options.max_df_fraction);
    let hub_set: BTreeSet<&String> = hubs.iter().collect();
    let filtered: BTreeMap<String, Vec<String>> = chunk_entities
        .iter()
        .filter_map(|(chunk, entities)| {
            let kept: Vec<String> = entities
                .iter()
                .filter(|key| !hub_set.contains(key))
                .cloned()
                .collect();
            if kept.is_empty() {
                None
            } else {
                Some((chunk.clone(), kept))
            }
        })
        .collect();

    let nodes: BTreeSet<String> = filtered.values().flatten().cloned().collect();
    let nodes: Vec<String> = nodes.into_iter().collect();
    let edges = cooccurrence(&filtered);
    let labels = louvain(&nodes, &edges, options.resolution);
    let members = assign_chunks(&filtered, &labels);

    let mut entity_by_label: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for (key, label) in &labels {
        entity_by_label.entry(*label).or_default().push(key.clone());
    }
    for keys in entity_by_label.values_mut() {
        keys.sort();
    }

    let mut communities: Vec<Community> = members
        .into_iter()
        .filter(|(_, chunks)| chunks.len() >= options.min_chunks)
        .map(|(label, chunk_ids)| Community {
            entity_keys: entity_by_label.remove(&label).unwrap_or_default(),
            chunk_ids,
        })
        .collect();
    communities.sort_by(|a, b| {
        b.chunk_ids
            .len()
            .cmp(&a.chunk_ids.len())
            .then_with(|| b.entity_keys.len().cmp(&a.entity_keys.len()))
            .then_with(|| a.chunk_ids.first().cmp(&b.chunk_ids.first()))
    });
    (communities, hubs)
}

/// BM25 score of every summary against `query`.
///
/// Okapi BM25 with the prototype's constants (`k1 = 1.2`, `b = 0.75`). IDF is
/// computed over the summaries themselves, which is the only corpus this arm
/// scores against.
#[must_use]
pub fn bm25_scores(summaries: &[&str], query: &str, k1: f32, b: f32) -> Vec<f32> {
    let terms: BTreeSet<String> = tokenize(query)
        .into_iter()
        .filter(|term| term.len() >= 2)
        .collect();
    if summaries.is_empty() {
        return Vec::new();
    }
    if terms.is_empty() {
        return vec![0.0; summaries.len()];
    }
    let documents: Vec<Vec<String>> = summaries
        .iter()
        .map(|summary| {
            tokenize(summary)
                .into_iter()
                .filter(|t| t.len() >= 2)
                .collect()
        })
        .collect();
    #[allow(clippy::cast_precision_loss)]
    let n = documents.len() as f32;
    let total: usize = documents.iter().map(Vec::len).sum();
    #[allow(clippy::cast_precision_loss)]
    let avgdl = if documents.is_empty() {
        0.0
    } else {
        total as f32 / n
    };

    let mut df: BTreeMap<&str, usize> = BTreeMap::new();
    for document in &documents {
        let present: BTreeSet<&str> = document
            .iter()
            .map(String::as_str)
            .filter(|term| terms.contains(*term))
            .collect();
        for term in present {
            *df.entry(term).or_insert(0) += 1;
        }
    }

    let mut scores = Vec::with_capacity(documents.len());
    for document in &documents {
        let mut tf: BTreeMap<&str, usize> = BTreeMap::new();
        for term in document {
            if terms.contains(term) {
                *tf.entry(term.as_str()).or_insert(0) += 1;
            }
        }
        #[allow(clippy::cast_precision_loss)]
        let dl = document.len() as f32;
        let mut score = 0.0_f32;
        for (term, frequency) in tf {
            #[allow(clippy::cast_precision_loss)]
            let document_frequency = df.get(term).copied().unwrap_or(0) as f32;
            let idf = (1.0 + (n - document_frequency + 0.5) / (document_frequency + 0.5)).ln();
            let norm = if avgdl > 0.0 {
                1.0 - b + b * dl / avgdl
            } else {
                1.0
            };
            #[allow(clippy::cast_precision_loss)]
            let frequency = frequency as f32;
            score += idf * frequency * (k1 + 1.0) / (frequency + k1 * norm);
        }
        scores.push(score);
    }
    scores
}

/// The communities the arm would select for `query`, best first.
///
/// Only communities with a positive BM25 score are selected: if no summary
/// shares a term with the query, the arm has nothing to say and the fuser
/// leaves the answer alone. Ties are broken by community order, so the same
/// query over the same summaries always selects the same set.
#[must_use]
pub fn rank_summaries(summaries: &[&str], query: &str, limit: usize) -> Vec<(usize, f32)> {
    if limit == 0 {
        return Vec::new();
    }
    let scores = bm25_scores(summaries, query, 1.2, 0.75);
    let mut ranked: Vec<(usize, f32)> = scores
        .into_iter()
        .enumerate()
        .filter(|(_, score)| *score > 0.0)
        .collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    ranked.truncate(limit);
    ranked
}

/// The summarisation prompt for one community.
///
/// Deterministic by construction: the cache is keyed by `model + sha256(prompt)`
/// and a prompt that changed on every run would pay for the same summary twice
/// (AGENTS rule 7). `excerpts` are `(chunk id, text)` pairs, in the order the
/// builder collected them.
#[must_use]
pub fn summary_prompt(
    entity_names: &[String],
    excerpts: &[(String, String)],
    chars_per_chunk: usize,
) -> String {
    let entities = entity_names
        .iter()
        .take(60)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let mut body = String::new();
    for (chunk, text) in excerpts.iter().take(12) {
        let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let cut = collapsed
            .char_indices()
            .take_while(|(index, _)| *index < chars_per_chunk)
            .last()
            .map_or(0, |(index, ch)| index + ch.len_utf8());
        body.push_str(&format!("[{}] {}\n", chunk, &collapsed[..cut]));
    }
    if body.is_empty() {
        body.push_str("(no chunk text available)\n");
    }
    format!(
        "You are summarising one community of a document collection so that a \
         search engine can answer broad questions (\"what are the main themes\", \
         \"list all the ...\") from the summary instead of from single chunks.\n\n\
         Community entities ({n_entities}): {entities}\n\n\
         Chunks in this community ({n_chunks}): {chunk_ids}\n\n\
         Chunk excerpts:\n{body}\n\
         Write 120-180 words of plain prose. Say what this community is about, \
         its main themes, the entities and numbers it centres on, and which of \
         the listed chunks carry which theme. Use only the material above; do \
         not invent facts, names or figures.",
        n_entities = entity_names.len(),
        n_chunks = excerpts.len(),
        chunk_ids = excerpts
            .iter()
            .take(40)
            .map(|(chunk, _)| chunk.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn clique(prefix: &str, size: usize) -> (Vec<String>, Vec<(String, String, f32)>) {
        let nodes: Vec<String> = (0..size).map(|i| format!("{prefix}{i}")).collect();
        let mut edges = Vec::new();
        for (i, left) in nodes.iter().enumerate() {
            for right in &nodes[i + 1..] {
                edges.push((left.clone(), right.clone(), 1.0));
            }
        }
        (nodes, edges)
    }

    #[test]
    fn two_cliques_are_two_communities_and_the_result_is_deterministic() {
        let (mut nodes, mut edges) = clique("a", 5);
        let (b_nodes, b_edges) = clique("b", 5);
        nodes.extend(b_nodes);
        edges.extend(b_edges);

        let labels = louvain(&nodes, &edges, 1.0);
        let groups: BTreeSet<usize> = labels.values().copied().collect();
        assert_eq!(groups.len(), 2, "two disconnected cliques: {labels:?}");

        let a_labels: BTreeSet<usize> = labels
            .iter()
            .filter(|(key, _)| key.starts_with('a'))
            .map(|(_, label)| *label)
            .collect();
        assert_eq!(a_labels.len(), 1, "a clique must not be split");
        assert_eq!(labels, louvain(&nodes, &edges, 1.0), "not deterministic");
    }

    #[test]
    fn a_single_clique_is_one_community() {
        let (nodes, edges) = clique("n", 6);
        let labels = louvain(&nodes, &edges, 1.0);
        assert_eq!(labels.values().copied().collect::<BTreeSet<_>>().len(), 1);
    }

    #[test]
    fn no_nodes_is_no_communities() {
        assert!(louvain(&[], &[], 1.0).is_empty());
    }

    #[test]
    fn hub_entities_drops_an_entity_on_more_than_five_percent_of_documents() {
        let mut document_entities: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        // 100 documents: the cap is max(2, 5) = 5.
        for index in 0..100 {
            document_entities
                .entry("ubiquitous".to_string())
                .or_default()
                .insert(format!("doc-{index}"));
        }
        for index in 0..4 {
            document_entities
                .entry("rare".to_string())
                .or_default()
                .insert(format!("doc-{index}"));
        }
        let hubs = hub_entities(&document_entities, 0.05);
        assert_eq!(hubs, vec!["ubiquitous".to_string()]);
    }

    #[test]
    fn a_zero_fraction_disables_the_hub_cap() {
        let mut document_entities: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        document_entities
            .entry("ubiquitous".to_string())
            .or_default()
            .extend((0..10).map(|i| format!("doc-{i}")));
        assert!(hub_entities(&document_entities, 0.0).is_empty());
    }

    #[test]
    fn chunks_join_the_community_of_the_majority_of_their_entities() {
        let mut labels: BTreeMap<String, usize> = BTreeMap::new();
        labels.insert("a1".into(), 0);
        labels.insert("a2".into(), 0);
        labels.insert("b1".into(), 1);
        let mut chunks: BTreeMap<String, Vec<String>> = BTreeMap::new();
        chunks.insert("c1".into(), vec!["a1".into(), "a2".into(), "b1".into()]);
        chunks.insert("c2".into(), vec!["b1".into()]);
        chunks.insert("c3".into(), vec!["a1".into(), "b1".into()]);
        let members = assign_chunks(&chunks, &labels);
        // c1 votes 2:1 for community 0; c3 is a 1:1 tie, which goes to the
        // lower community id.
        assert_eq!(
            members.get(&0).cloned().unwrap_or_default(),
            vec!["c1", "c3"]
        );
        assert_eq!(members.get(&1).cloned().unwrap_or_default(), vec!["c2"]);
    }

    #[test]
    fn detect_splits_two_cliques_and_caps_hubs() {
        // Two topics, each on two chunks, plus one hub on every chunk.
        let mut chunks: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut documents: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (index, topic) in ["alpha", "beta"].iter().enumerate() {
            for offset in 0..2 {
                let chunk = format!("{topic}-chunk-{offset}");
                chunks.insert(
                    chunk.clone(),
                    vec![
                        format!("{topic}-a"),
                        format!("{topic}-b"),
                        "hub".to_string(),
                    ],
                );
                for entity in [format!("{topic}-a"), format!("{topic}-b")] {
                    documents
                        .entry(entity)
                        .or_default()
                        .insert(format!("{topic}-doc-{offset}"));
                }
                documents
                    .entry("hub".to_string())
                    .or_default()
                    .insert(format!("doc-{index}-{offset}"));
            }
        }
        // 8 documents, so the cap is max(2, 0) = 2: the hub (4 documents) goes.
        let (communities, hubs) = detect(
            &chunks,
            &documents,
            DetectOptions {
                min_chunks: 1,
                ..DetectOptions::default()
            },
        );
        assert_eq!(hubs, vec!["hub".to_string()]);
        assert_eq!(communities.len(), 2, "{communities:?}");
        for community in &communities {
            assert_eq!(community.chunk_ids.len(), 2);
            assert_eq!(community.entity_keys.len(), 2);
        }
        // The largest community comes first; both have two chunks, so the tie is
        // broken by the first chunk id.
        assert!(communities[0].chunk_ids[0] < communities[1].chunk_ids[0]);
    }

    #[test]
    fn min_chunks_drops_small_communities() {
        let mut chunks: BTreeMap<String, Vec<String>> = BTreeMap::new();
        chunks.insert("only".into(), vec!["a".into(), "b".into()]);
        let documents: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let (communities, _) = detect(
            &chunks,
            &documents,
            DetectOptions {
                max_df_fraction: 0.0,
                min_chunks: 2,
                ..DetectOptions::default()
            },
        );
        assert!(communities.is_empty());
    }

    #[test]
    fn bm25_ranks_the_summary_that_shares_the_query_terms_first() {
        let summaries = [
            "labour mobility across member states and wage trends",
            "fisheries quotas and the north sea catch limits",
        ];
        let ranked = rank_summaries(&summaries, "labour mobility trends", 3);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].0, 0);
        assert!(ranked[0].1 > 0.0);
    }

    #[test]
    fn a_query_with_no_shared_term_selects_nothing() {
        let summaries = ["fisheries quotas", "harbour tariffs"];
        assert!(rank_summaries(&summaries, "quantum chromodynamics", 3).is_empty());
    }

    #[test]
    fn rank_summaries_caps_the_selection_and_breaks_ties_by_order() {
        let summaries = ["tariff report", "tariff report", "tariff report"];
        let ranked = rank_summaries(&summaries, "tariff", 2);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].0, 0);
        assert_eq!(ranked[1].0, 1);
    }

    #[test]
    fn the_summary_prompt_is_deterministic_and_names_the_material() {
        let names = vec!["Bank Indonesia".to_string(), "rupiah".to_string()];
        let excerpts = vec![
            (
                "doc-1_0".to_string(),
                "The rupiah weakened  \n against the dollar".to_string(),
            ),
            (
                "doc-1_1".to_string(),
                "Bank Indonesia raised rates".to_string(),
            ),
        ];
        let prompt = summary_prompt(&names, &excerpts, 40);
        assert_eq!(prompt, summary_prompt(&names, &excerpts, 40));
        assert!(prompt.contains("Bank Indonesia"));
        assert!(prompt.contains("[doc-1_0]"));
        assert!(prompt.contains("Chunks in this community (2)"));
        assert!(!prompt.contains('\n') || prompt.contains("Chunk excerpts:\n"));
    }

    #[test]
    fn the_summary_prompt_handles_an_empty_community() {
        let prompt = summary_prompt(&[], &[], 40);
        assert!(prompt.contains("(no chunk text available)"));
    }
}
