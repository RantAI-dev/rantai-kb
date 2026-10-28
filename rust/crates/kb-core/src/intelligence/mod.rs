//! Document intelligence: entity and relation extraction algorithms.

pub mod heuristic;
pub mod pattern_extractor;
pub mod pattern_relation_extractor;
pub mod types;

pub use heuristic::{HeuristicConfig, key_of, query_terms};
pub use pattern_extractor::PatternEntityExtractor;
pub use pattern_relation_extractor::PatternRelationExtractor;
pub use types::{Entity, PatternExtractionConfig, PatternRelationConfig, Relation};
