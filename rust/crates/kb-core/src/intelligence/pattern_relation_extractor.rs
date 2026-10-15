//! Pattern-based Relation Extractor.
//!
//! Extracts common relationships using text patterns.
//! Fast and reliable for known relationship structures.
//! Mirrors `src/lib/document-intelligence/pattern-relation-extractor.ts`.

use fancy_regex::Regex;
use serde_json::json;
use std::collections::HashMap;

use super::types::{Entity, PatternRelationConfig, Relation};

struct RelationPattern {
    confidence: f32,
    pattern: Regex,
    relation_type: &'static str,
    source_group: usize,
    target_group: usize,
}

/// Pattern-based Relation Extractor.
pub struct PatternRelationExtractor {
    patterns: Vec<RelationPattern>,
}




fn make_pat(
    pattern: &str,
    relation_type: &'static str,
    confidence: f32,
    source_group: usize,
    target_group: usize,
) -> Option<RelationPattern> {
    Regex::new(pattern).ok().map(|re| RelationPattern {
        confidence,
        pattern: re,
        relation_type,
        source_group,
        target_group,
    })
}

impl PatternRelationExtractor {
    /// Create a new relation extractor with configuration.
    #[must_use]
    pub fn new(config: PatternRelationConfig) -> Self {
        let mut patterns = Vec::new();

        if config.extract_universal {
            patterns.extend(Self::universal_patterns());
        }
        if config.extract_legal {
            patterns.extend(Self::legal_patterns());
        }
        if config.extract_financial {
            patterns.extend(Self::financial_patterns());
        }
        if config.extract_technical {
            patterns.extend(Self::technical_patterns());
        }

        Self { patterns }
    }

    /// Extract relations from text with known entities.
    #[must_use]
    pub fn extract(
        &self,
        text: &str,
        entities: &[Entity],
        file_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Vec<Relation> {
        if entities.len() < 2 {
            return Vec::new();
        }

        let mut entity_map: HashMap<String, &Entity> = HashMap::new();
        for entity in entities {
            entity_map.insert(entity.name.to_lowercase(), entity);
        }

        let mut relations = Vec::new();

        for pat in &self.patterns {
            for caps_res in pat.pattern.captures_iter(text) {
                let Ok(caps) = caps_res else { continue };

                let source_raw = caps.get(pat.source_group).map(|m| m.as_str().trim());
                let target_raw = caps.get(pat.target_group).map(|m| m.as_str().trim());

                let (Some(src), Some(tgt)) = (source_raw, target_raw) else {
                    continue;
                };

                let src_ent = entity_map.get(&src.to_lowercase());
                let tgt_ent = entity_map.get(&tgt.to_lowercase());

                if let (Some(source_entity), Some(target_entity)) = (src_ent, tgt_ent) {
                    let matched_context = caps.get(0).map_or("", |m| m.as_str());
                    relations.push(Relation {
                        relation_type: pat.relation_type.to_string(),
                        confidence: pat.confidence,
                        source_entity: source_entity.name.clone(),
                        target_entity: target_entity.name.clone(),
                        context: Some(matched_context.to_string()),
                        metadata: Some(json!({
                            "context": matched_context,
                            "pattern": pat.pattern.as_str(),
                            "source_entity": source_entity.name,
                            "target_entity": target_entity.name,
                            "file_id": file_id,
                            "user_id": user_id,
                            "extraction_method": "pattern",
                        })),
                    });
                }
            }
        }

        Self::deduplicate(relations)
    }

    fn universal_patterns() -> Vec<RelationPattern> {
        [
            make_pat(r"(?i)(\w+)\s+(?:mentions?|refers? to|discusses?)\s+(\w+)", "MENTIONS", 0.7, 1, 2),
            make_pat(r"(?i)(\w+)\s+(?:is part of|belongs to|in)\s+(\w+)", "PART_OF", 0.8, 1, 2),
            make_pat(r"(?i)(\w+)\s+(?:cites?|references?|according to)\s+(\w+)", "CITES", 0.85, 1, 2),
            make_pat(r"(?i)(\w+)\s+(?:works? for|employed by|at)\s+(\w+)", "WORKS_FOR", 0.8, 1, 2),
            make_pat(r"(?i)(\w+)\s+(?:created|developed|built) by\s+(\w+)", "CREATED_BY", 0.8, 1, 2),
            make_pat(r"(?i)(\w+)\s+(?:located in|based in|headquartered in)\s+(\w+)", "LOCATED_IN", 0.8, 1, 2),
        ].into_iter().flatten().collect()
    }

    fn legal_patterns() -> Vec<RelationPattern> {
        [
            make_pat(r"(?i)(pp\s+\S+)\s+(?:implements?|melaksanakan)\s+(uu\s+\S+)", "IMPLEMENTS", 0.9, 1, 2),
            make_pat(r"(?i)(pp\s+\S+)\s+(?:refines?|clarifies?|memperjelas)\s+(uu\s+\S+)", "REFINES", 0.9, 1, 2),
            make_pat(r"(?i)(uu\s+\S+|pp\s+\S+)\s+(?:supersedes?|replaces?|menggantikan)\s+(uu\s+\S+|pp\s+\S+)", "SUPERSEDES", 0.9, 1, 2),
            make_pat(r"(?i)(uu\s+\S+|pp\s+\S+)\s+(?:applies to|berlaku untuk)\s+([a-z][\s\w]+)", "APPLIES_TO", 0.8, 1, 2),
            make_pat(r"(?i)(pasal\s+\d+)\s+(?:of|dalam|from)\s+(uu\s+\S+|pp\s+\S+)", "PART_OF", 0.95, 1, 2),
            make_pat(r"(?i)(uu\s+\S+|pp\s+\S+)\s+(?:requires?|mewajibkan|mengharuskan)\s+([a-z][\s\w]+)", "REQUIRES", 0.85, 1, 2),
        ].into_iter().flatten().collect()
    }

    fn financial_patterns() -> Vec<RelationPattern> {
        [
            make_pat(r"(?i)(\w+)\s+(?:pays?|transfers?)\s+[\d\s$,.prp£€]+ to\s+(\w+)", "PAYS", 0.85, 1, 2),
            make_pat(r"(?i)(\w+)\s+(?:owns?|possesses?)\s+(\w+)", "OWNED_BY", 0.8, 2, 1),
            make_pat(r"(?i)(\w+)\s+(?:transfers?|sends?)\s+(?:to|ke)\s+(\w+)", "TRANSFERS", 0.8, 1, 2),
        ].into_iter().flatten().collect()
    }

    fn technical_patterns() -> Vec<RelationPattern> {
        [
            make_pat(r"(?i)(\w+\(\))\s+calls?\s+(\w+\(\))", "CALLS", 0.9, 1, 2),
            make_pat(r"(?i)(\w+\(\))\s+throws?\s+(\w+(?:error|exception))", "THROWS", 0.9, 1, 2),
            make_pat(r"(?i)(\w+)\s+(?:depends on|requires?)\s+(\w+)", "DEPENDS_ON", 0.8, 1, 2),
            make_pat(r"(?i)(\w+\(\))\s+returns?\s+(\w+)", "RETURNS", 0.85, 1, 2),
        ].into_iter().flatten().collect()
    }

    fn deduplicate(relations: Vec<Relation>) -> Vec<Relation> {
        let mut seen: HashMap<String, Relation> = HashMap::new();
        for rel in relations {
            let key = format!(
                "{}:{}:{}",
                rel.source_entity.to_lowercase(),
                rel.relation_type,
                rel.target_entity.to_lowercase()
            );
            match seen.get_mut(&key) {
                Some(existing) => {
                    if rel.confidence > existing.confidence {
                        *existing = rel;
                    }
                }
                None => {
                    seen.insert(key, rel);
                }
            }
        }
        seen.into_values().collect()
    }
}

impl Default for PatternRelationExtractor {
    fn default() -> Self {
        Self::new(PatternRelationConfig::default())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn extracts_works_for_and_located_in() {
        let extractor = PatternRelationExtractor::default();
        let entities = vec![
            Entity {
                name: "Alice".to_string(),
                entity_type: "Person".to_string(),
                confidence: 0.9,
                document_id: None,
                chunk_id: None,
                metadata: None,
            },
            Entity {
                name: "Acme".to_string(),
                entity_type: "Organization".to_string(),
                confidence: 0.9,
                document_id: None,
                chunk_id: None,
                metadata: None,
            },
            Entity {
                name: "Jakarta".to_string(),
                entity_type: "Location".to_string(),
                confidence: 0.9,
                document_id: None,
                chunk_id: None,
                metadata: None,
            },
        ];

        let text = "Alice works for Acme. Acme located in Jakarta.";
        let rels = extractor.extract(text, &entities, Some("doc1"), None);

        let types: Vec<&str> = rels.iter().map(|r| r.relation_type.as_str()).collect();
        assert!(types.contains(&"WORKS_FOR"));
        assert!(types.contains(&"LOCATED_IN"));
    }

    #[test]
    fn extracts_technical_calls_relation() {
        let extractor = PatternRelationExtractor::default();
        let entities = vec![
            Entity {
                name: "main()".to_string(),
                entity_type: "Function".to_string(),
                confidence: 0.8,
                document_id: None,
                chunk_id: None,
                metadata: None,
            },
            Entity {
                name: "init()".to_string(),
                entity_type: "Function".to_string(),
                confidence: 0.8,
                document_id: None,
                chunk_id: None,
                metadata: None,
            },
        ];

        let text = "The entrypoint main() calls init() before running.";
        let rels = extractor.extract(text, &entities, Some("doc1"), None);

        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].relation_type, "CALLS");
        assert_eq!(rels[0].source_entity, "main()");
        assert_eq!(rels[0].target_entity, "init()");
    }
}
