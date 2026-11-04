//! Pattern-based Entity Extractor.
//!
//! Fast regex-based extraction for common entity types.
//! Zero API cost - runs entirely locally.
//! Mirrors `src/lib/document-intelligence/pattern-extractor.ts`.

use fancy_regex::Regex;
use serde_json::json;
use std::collections::HashSet;
use std::sync::LazyLock;

use super::types::{Entity, PatternExtractionConfig};

// Helper macro for creating regexes with a fallback abort on invariant violation
macro_rules! lazy_regex {
    ($re:expr) => {
        LazyLock::new(|| match Regex::new($re) {
            Ok(r) => r,
            Err(_) => std::process::abort(),
        })
    };
}

// Universal patterns
static RE_EMAIL: LazyLock<Regex> = lazy_regex!(r"\b[\w%+.-]+@[\d.A-Za-z-]+\.[A-Za-z]{2,}\b");
static RE_URL: LazyLock<Regex> = lazy_regex!(
    r"https?://(?:www\.)?[\w#%+.:=@~-]{1,256}\.[\d()A-Za-z]{1,6}\b(?:[\w#%&()+./:=?@~-]*)"
);
static RE_PHONE1: LazyLock<Regex> =
    lazy_regex!(r"(?:\+?(\d{1,3}))?[ (.-]*(\d{3})[ ).-]*(\d{3})[ .-]*(\d{4})");
static RE_PHONE2: LazyLock<Regex> =
    lazy_regex!(r"\+?\d{1,3}[-.\s]?\(?\d{2,4}\)?[-.\s]?\d{3,4}[-.\s]?\d{3,4}");
static RE_DATE1: LazyLock<Regex> = lazy_regex!(r"(?:\d{1,2}[/-]){2}\d{2,4}");
static RE_DATE2: LazyLock<Regex> = lazy_regex!(r"\d{4}(?:[/-]\d{1,2}){2}");
static RE_DATE3: LazyLock<Regex> = lazy_regex!(
    r"(?i)\b(?:January|February|March|April|May|June|July|August|September|October|November|December)\s+\d{1,2},?\s+\d{4}\b"
);
static RE_DATE4: LazyLock<Regex> =
    lazy_regex!(r"(?i)\b(?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)\s+\d{1,2},?\s+\d{4}\b");

// Financial patterns
static RE_CURR_USD1: LazyLock<Regex> = lazy_regex!(r"\$\s*[\d,]+(?:\.\d{2})?");
static RE_CURR_USD2: LazyLock<Regex> = lazy_regex!(r"(?i)USD\s*[\d,]+(?:\.\d{2})?");
static RE_CURR_EUR1: LazyLock<Regex> = lazy_regex!(r"€\s*[\d,]+(?:\.\d{2})?");
static RE_CURR_EUR2: LazyLock<Regex> = lazy_regex!(r"(?i)EUR\s*[\d,]+(?:\.\d{2})?");
static RE_CURR_GBP1: LazyLock<Regex> = lazy_regex!(r"£\s*[\d,]+(?:\.\d{2})?");
static RE_CURR_GBP2: LazyLock<Regex> = lazy_regex!(r"(?i)GBP\s*[\d,]+(?:\.\d{2})?");
static RE_CURR_IDR1: LazyLock<Regex> = lazy_regex!(r"(?i)Rp\.?\s*[\d,.]+");
static RE_CURR_IDR2: LazyLock<Regex> = lazy_regex!(r"(?i)IDR\s*[\d,.]+");

fn currency_regexes() -> [&'static LazyLock<Regex>; 8] {
    [
        &RE_CURR_USD1,
        &RE_CURR_USD2,
        &RE_CURR_EUR1,
        &RE_CURR_EUR2,
        &RE_CURR_GBP1,
        &RE_CURR_GBP2,
        &RE_CURR_IDR1,
        &RE_CURR_IDR2,
    ]
}
static RE_PERCENT: LazyLock<Regex> = lazy_regex!(r"\b\d+(?:\.\d+)?%");

// Technical patterns
static RE_API: LazyLock<Regex> = lazy_regex!(r"/api/[\w/-]+");
static RE_FUNCTION: LazyLock<Regex> = lazy_regex!(r"\b[A-Z_a-z]\w*\s*\(\)");
static RE_ENV_VAR: LazyLock<Regex> = lazy_regex!(r"\b[A-Z][A-Z0-9_]{2,}\b(?=\s*[=:]|\s+is|\s+set)");
static RE_ERROR_CODE: LazyLock<Regex> = lazy_regex!(r"(?i)\b(?:ERR|ERROR|E)[-_]?\d{3,5}\b");

/// Pattern-based Entity Extractor.
#[derive(Debug, Clone, Default)]
pub struct PatternEntityExtractor {
    config: PatternExtractionConfig,
}

impl PatternEntityExtractor {
    /// Create a new pattern extractor with optional configuration.
    #[must_use]
    pub fn new(config: PatternExtractionConfig) -> Self {
        Self { config }
    }

    /// Extract entities using regex patterns matching TypeScript implementation.
    #[must_use]
    pub fn extract(
        &self,
        text: &str,
        document_id: Option<&str>,
        chunk_id: Option<&str>,
    ) -> Vec<Entity> {
        let mut entities = Vec::new();

        if self.config.extract_universal {
            self.extract_universal(text, document_id, chunk_id, &mut entities);
        }

        if self.config.extract_financial {
            self.extract_financial(text, document_id, chunk_id, &mut entities);
        }

        if self.config.extract_technical {
            self.extract_technical(text, document_id, chunk_id, &mut entities);
        }

        Self::deduplicate(entities)
    }

    fn extract_universal(
        &self,
        text: &str,
        doc_id: Option<&str>,
        chunk_id: Option<&str>,
        entities: &mut Vec<Entity>,
    ) {
        // Email
        for mat in RE_EMAIL.find_iter(text).flatten() {
            let val = mat.as_str();
            entities.push(Entity {
                name: val.to_string(),
                entity_type: "Email".to_string(),
                confidence: 0.95,
                document_id: doc_id.map(str::to_string),
                chunk_id: chunk_id.map(str::to_string),
                metadata: Some(json!({ "pattern": "email", "rawText": val })),
            });
        }

        // URL
        for mat in RE_URL.find_iter(text).flatten() {
            let val = mat.as_str();
            entities.push(Entity {
                name: val.to_string(),
                entity_type: "URL".to_string(),
                confidence: 0.95,
                document_id: doc_id.map(str::to_string),
                chunk_id: chunk_id.map(str::to_string),
                metadata: Some(json!({ "pattern": "url", "rawText": val })),
            });
        }

        // Phone numbers
        for re in [&RE_PHONE1, &RE_PHONE2] {
            for mat in re.find_iter(text).flatten() {
                let val = mat.as_str();
                if !entities
                    .iter()
                    .any(|e| e.name == val && e.entity_type == "Phone")
                {
                    entities.push(Entity {
                        name: val.to_string(),
                        entity_type: "Phone".to_string(),
                        confidence: 0.7,
                        document_id: doc_id.map(str::to_string),
                        chunk_id: chunk_id.map(str::to_string),
                        metadata: Some(json!({ "pattern": "phone", "rawText": val })),
                    });
                }
            }
        }

        // Dates
        for re in [&RE_DATE1, &RE_DATE2, &RE_DATE3, &RE_DATE4] {
            for mat in re.find_iter(text).flatten() {
                let val = mat.as_str();
                if !entities
                    .iter()
                    .any(|e| e.name == val && e.entity_type == "Date")
                {
                    entities.push(Entity {
                        name: val.to_string(),
                        entity_type: "Date".to_string(),
                        confidence: 0.85,
                        document_id: doc_id.map(str::to_string),
                        chunk_id: chunk_id.map(str::to_string),
                        metadata: Some(json!({ "pattern": "date", "rawText": val })),
                    });
                }
            }
        }
    }

    fn extract_financial(
        &self,
        text: &str,
        doc_id: Option<&str>,
        chunk_id: Option<&str>,
        entities: &mut Vec<Entity>,
    ) {
        for re in currency_regexes() {
            for mat in re.find_iter(text).flatten() {
                let val = mat.as_str();
                entities.push(Entity {
                    name: val.to_string(),
                    entity_type: "Currency".to_string(),
                    confidence: 0.9,
                    document_id: doc_id.map(str::to_string),
                    chunk_id: chunk_id.map(str::to_string),
                    metadata: Some(json!({ "pattern": "currency", "rawText": val })),
                });
            }
        }

        for mat in RE_PERCENT.find_iter(text).flatten() {
            let val = mat.as_str();
            entities.push(Entity {
                name: val.to_string(),
                entity_type: "Number".to_string(),
                confidence: 0.9,
                document_id: doc_id.map(str::to_string),
                chunk_id: chunk_id.map(str::to_string),
                metadata: Some(json!({ "pattern": "percentage", "rawText": val })),
            });
        }
    }

    fn extract_technical(
        &self,
        text: &str,
        doc_id: Option<&str>,
        chunk_id: Option<&str>,
        entities: &mut Vec<Entity>,
    ) {
        // API endpoints
        for mat in RE_API.find_iter(text).flatten() {
            let val = mat.as_str();
            entities.push(Entity {
                name: val.to_string(),
                entity_type: "API".to_string(),
                confidence: 0.85,
                document_id: doc_id.map(str::to_string),
                chunk_id: chunk_id.map(str::to_string),
                metadata: Some(json!({ "pattern": "api_endpoint", "rawText": val })),
            });
        }

        // Function calls
        let skip_words = ["if()", "for()", "while()", "switch()", "catch()"];
        for mat in RE_FUNCTION.find_iter(text).flatten() {
            let val = mat.as_str();
            let lower = val.to_lowercase().replace(' ', "");
            if !skip_words.contains(&lower.as_str()) {
                entities.push(Entity {
                    name: val.to_string(),
                    entity_type: "Function".to_string(),
                    confidence: 0.7,
                    document_id: doc_id.map(str::to_string),
                    chunk_id: chunk_id.map(str::to_string),
                    metadata: Some(json!({ "pattern": "function_call", "rawText": val })),
                });
            }
        }

        // Environment variables
        for mat in RE_ENV_VAR.find_iter(text).flatten() {
            let val = mat.as_str();
            entities.push(Entity {
                name: val.to_string(),
                entity_type: "Technology".to_string(),
                confidence: 0.6,
                document_id: doc_id.map(str::to_string),
                chunk_id: chunk_id.map(str::to_string),
                metadata: Some(json!({ "pattern": "env_variable", "rawText": val })),
            });
        }

        // Error codes
        for mat in RE_ERROR_CODE.find_iter(text).flatten() {
            let val = mat.as_str();
            entities.push(Entity {
                name: val.to_string(),
                entity_type: "Error".to_string(),
                confidence: 0.8,
                document_id: doc_id.map(str::to_string),
                chunk_id: chunk_id.map(str::to_string),
                metadata: Some(json!({ "pattern": "error_code", "rawText": val })),
            });
        }
    }

    /// Deduplicate entities by `type:name`.
    fn deduplicate(entities: Vec<Entity>) -> Vec<Entity> {
        let mut seen = HashSet::new();
        let mut unique = Vec::new();
        for entity in entities {
            let key = format!("{}:{}", entity.entity_type, entity.name);
            if seen.insert(key) {
                unique.push(entity);
            }
        }
        unique
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn extracts_universal_entities() {
        let extractor = PatternEntityExtractor::default();
        let text = "Contact alice@example.com or visit https://rantai.dev on 2026-10-15 or call +1 555-123-4567.";
        let entities = extractor.extract(text, Some("doc1"), None);

        let types: Vec<&str> = entities.iter().map(|e| e.entity_type.as_str()).collect();
        assert!(types.contains(&"Email"));
        assert!(types.contains(&"URL"));
        assert!(types.contains(&"Date"));
        assert!(types.contains(&"Phone"));
    }

    #[test]
    fn extracts_financial_entities() {
        let extractor = PatternEntityExtractor::default();
        let text =
            "Total revenue was $1,234.56 or Rp 15.000.000 representing an increase of 25.5%.";
        let entities = extractor.extract(text, Some("doc1"), None);

        let types: Vec<&str> = entities.iter().map(|e| e.entity_type.as_str()).collect();
        assert!(types.contains(&"Currency"));
        assert!(types.contains(&"Number"));
    }

    #[test]
    fn extracts_technical_entities() {
        let extractor = PatternEntityExtractor::default();
        let text =
            "API /api/v1/search returned ERROR-404 when processData() ran with DATABASE_URL set.";
        let entities = extractor.extract(text, Some("doc1"), None);

        let types: Vec<&str> = entities.iter().map(|e| e.entity_type.as_str()).collect();
        assert!(types.contains(&"API"));
        assert!(types.contains(&"Error"));
        assert!(types.contains(&"Function"));
        assert!(types.contains(&"Technology"));
    }
}
