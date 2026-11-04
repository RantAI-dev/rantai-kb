//! The zero-spend entity extractor, behind the same port as the LLM one.
//!
//! # Why it exists
//!
//! The default [`LlmCombinedExtractor`] is an OpenRouter call. AGENTS rule 7 says
//! local open-weights first and paid APIs only as an optional profile, and the
//! graph arm is exactly the kind of thing you must be able to run many times —
//! once per ingest, once per eval arm-on/arm-off comparison — before you believe
//! it. An arm you can only measure by paying for it gets measured once.
//!
//! So this is [`kb_core::intelligence::heuristic`]: pure functions, no network,
//! no key, deterministic. It is not as good as a model, and it does not pretend
//! to be — it extracts title-case names, acronyms, quoted titles and
//! number-with-unit phrases, and it relates them by co-occurrence inside one
//! sentence. What it buys is that the comparison between "arm off" and "arm on"
//! costs nothing at either end.
//!
//! Selected with `KB_ENTITY_EXTRACTOR=heuristic`.

use async_trait::async_trait;
use kb_core::intelligence::heuristic::{self, HeuristicConfig};
use kb_core::intelligence::{Entity, Relation};
use kb_ports::{EntityExtractor, PortError};

/// Settings for [`HeuristicExtractor`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct HeuristicExtractorSettings {
    pub config: HeuristicConfig,
}

/// An [`EntityExtractor`] that runs entirely in process.
///
/// `async` only because the port is; there is no `.await` inside, so it can
/// never be the reason an ingest job fails slowly.
#[derive(Debug, Clone, Copy, Default)]
pub struct HeuristicExtractor {
    config: HeuristicConfig,
}

impl HeuristicExtractor {
    #[must_use]
    pub fn new(settings: HeuristicExtractorSettings) -> Self {
        Self {
            config: settings.config,
        }
    }
}

#[async_trait]
impl EntityExtractor for HeuristicExtractor {
    async fn extract_entities(
        &self,
        text: &str,
        document_id: Option<&str>,
        chunk_id: Option<&str>,
    ) -> Result<Vec<Entity>, PortError> {
        Ok(heuristic::extract(text, document_id, chunk_id, &self.config).0)
    }

    async fn extract_entities_and_relations(
        &self,
        text: &str,
        document_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<(Vec<Entity>, Vec<Relation>), PortError> {
        // `user_id` is the LLM extractor's tenancy attribution for the provider
        // call. There is no provider here and nothing is attributed, so it is
        // deliberately ignored rather than stored on every entity.
        let _ = user_id;
        let _ = document_id;
        Ok(heuristic::extract(text, document_id, None, &self.config))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// Text cut from the HotpotQA paragraph the first dev question is about, so
    /// the assertion is about a paragraph a real corpus will contain.
    const SAMPLE: &str = "Scott Derrickson (born July 16, 1966) is an American director, \
screenwriter and producer. He lives in Los Angeles, California. He is best known for directing \
horror films such as \"Sinister\", \"The Exorcism of Emily Rose\", and \"Deliver Us From Evil\"";

    #[tokio::test]
    async fn both_port_methods_find_the_same_entities() {
        let extractor = HeuristicExtractor::default();
        let entities = extractor
            .extract_entities(SAMPLE, Some("doc"), Some("chunk"))
            .await
            .expect("extraction never fails");
        let (combined, _) = extractor
            .extract_entities_and_relations(SAMPLE, Some("doc"), Some("user"))
            .await
            .expect("extraction never fails");

        // Not `assert_eq!` on the vectors: the port gives the combined method no
        // `chunk_id` argument, so every entity's `chunk_id` differs by
        // construction. That asymmetry is the LLM extractor's shape — it returns
        // document-level relations — and the keys are what has to agree.
        fn keys(entities: &[Entity]) -> Vec<String> {
            entities.iter().map(|e| e.name.clone()).collect()
        }
        assert_eq!(keys(&entities), keys(&combined));
    }

    #[tokio::test]
    async fn the_combined_method_returns_relations_and_the_entities_method_does_not() {
        let extractor = HeuristicExtractor::default();
        let entities = extractor
            .extract_entities(SAMPLE, None, None)
            .await
            .expect("extraction never fails");
        let (combined, relations) = extractor
            .extract_entities_and_relations(SAMPLE, None, None)
            .await
            .expect("extraction never fails");
        assert!(!relations.is_empty(), "co-occurrence must be found");
        fn names(entities: &[Entity]) -> Vec<String> {
            entities.iter().map(|e| e.name.clone()).collect()
        }
        assert_eq!(names(&entities), names(&combined));
    }

    #[tokio::test]
    async fn it_is_free_of_a_provider_and_of_a_failure_mode() {
        let extractor = HeuristicExtractor::default();
        // Empty and non-UTF-boundary-adjacent text are both ordinary inputs to
        // this port, and neither may be an error.
        assert!(
            extractor
                .extract_entities("", None, None)
                .await
                .expect("empty text is not an error")
                .is_empty()
        );
        assert!(
            extractor
                .extract_entities("🌍 日本語 — ünïcödé", None, None)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn two_extractors_over_the_same_text_agree() {
        let a = HeuristicExtractor::default();
        let b = HeuristicExtractor::new(HeuristicExtractorSettings::default());
        let first = a
            .extract_entities_and_relations(SAMPLE, None, None)
            .await
            .expect("ok");
        let second = b
            .extract_entities_and_relations(SAMPLE, None, None)
            .await
            .expect("ok");
        assert_eq!(first, second);
    }
}
