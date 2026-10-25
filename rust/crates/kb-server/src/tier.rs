//! `KB_TIER`: the deployment-tier preset from `docs/kb-tiers.md`.
//!
//! A tier is a **preset**, not a mode. It fills a role default for every
//! variable the operator left unset, so the resolved [`crate::Config`] is
//! exactly what the operator would have got by writing those values by hand.
//! Any variable present in the environment wins, including an empty one, which
//! suppresses the preset for that role and lets the setting's own default
//! apply. With `KB_TIER` unset, resolution is unchanged from before this
//! module existed.
//!
//! Only **text roles** are filled. The local tiers leave the page-image arm off
//! because the multimodal embedder (`Qwen3-VL-Embedding`) is not wired yet; an
//! operator who runs it sets `KB_PAGE_EMBEDDING_MODEL` (and
//! `KB_PAGE_EMBEDDING_FORMAT=vllm`) by hand, and that wins over the tier.

use anyhow::Result;

/// A deployment tier, by the name `KB_TIER` accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Lite,
    LiteEn,
    Desk,
    Studio,
    Station,
    Cloud,
}

/// Every accepted `KB_TIER` value, in `docs/kb-tiers.md` order.
pub const TIER_NAMES: [&str; 6] = ["lite", "lite-en", "desk", "studio", "station", "cloud"];

const QWEN3_EMBEDDING: &str = "Qwen/Qwen3-Embedding-0.6B";
const E5_SMALL_EMBEDDING: &str = "intfloat/multilingual-e5-small";
const MINILM_EMBEDDING: &str = "sentence-transformers/all-MiniLM-L6-v2";
const BGE_RERANKER: &str = "BAAI/bge-reranker-v2-m3";
const VOYAGE_EMBEDDING: &str = "voyageai/voyage-multimodal-3.5";
const VOYAGE_RERANKER: &str = "voyageai/rerank-2.5";

/// The local OpenAI-compatible embedding endpoint (`llama server --embedding`
/// or TEI), as `docs/local-models.md` starts it.
const LOCAL_EMBEDDINGS: &str = "http://127.0.0.1:8081/v1/embeddings";
/// The local `/rerank` base; the TEI adapter appends `/rerank`.
const LOCAL_RERANK_URL: &str = "http://127.0.0.1:8082";
/// OpenRouter's rerank router, which takes the Cohere request shape.
const OPENROUTER_RERANK: &str = "https://openrouter.ai/api/v1/rerank";

/// Qwen3-Embedding's query instruction, the same string
/// `rust/scripts/stack-up.sh` and `docs/local-models.md` set. It is part of the
/// local embedder's wire contract, so one constant keeps the four local tiers
/// in step (`docs/eval-ab/vidore3-hr-en-night`: +0.012 text nDCG@10).
const QWEN3_QUERY_PREFIX: &str =
    "Instruct: Given a question, retrieve passages that answer it\nQuery: ";

// ---------------------------------------------------------------------------
// The preset table. This is the only place in the server that names a model
// for a tier: edit a value here and nothing else needs to know. Variables the
// table does not name keep their own defaults.
//
// Lite's concurrency and batch numbers are **placeholders** until the 2-core
// measurement in `docs/kb-tiers.md` (build order 5) tunes them; the Desk /
// Studio / Station rerank widths are estimates from the same document.
// ---------------------------------------------------------------------------

/// The text stack every local tier shares: Qwen3-Embedding-0.6B at a local
/// OpenAI-compatible endpoint, with the query instruction the repo already
/// uses. The 120 s timeout is the measured local setting: the 15 s default
/// times out on long PDFs once batches queue (`docs/local-models.md`).
///
/// `KB_PAGE_RERANK=false` on every local tier: bge is a poor page reranker
/// (−0.058 alone offline, ~10 s per search on the RX 590), so it stays off
/// until a stronger local reranker is measured (`docs/eval-ab/vidore3-hr-en-night`,
/// "bge should not rerank pages"). It only matters when an operator turns the
/// page arm on.
const LOCAL: &[(&str, &str)] = &[
    ("KB_EMBEDDING_BASE_URL", LOCAL_EMBEDDINGS),
    ("KB_EMBEDDING_TIMEOUT_MS", "120000"),
    ("KB_PAGE_RERANK", "false"),
    // The default extractor is a paid OpenRouter call per document, and its
    // output (the Postgres entity graph) feeds no search arm. Off on every tier.
    ("KB_ENTITY_EXTRACTION_ENABLED", "false"),
];

/// Desk and up embed text with Qwen3-Embedding-0.6B on a GPU.
const QWEN3_TEXT: &[(&str, &str)] = &[
    ("KB_EMBEDDING_MODEL", QWEN3_EMBEDDING),
    ("KB_EMBEDDING_DIM", "1024"),
    ("KB_EMBEDDING_QUERY_PREFIX", QWEN3_QUERY_PREFIX),
];

/// Lite embeds text with multilingual-e5-small on the CPU. Measured on 2 pinned
/// cores, the embedder capped at 1 thread (2026-10-24, MIRACL-id-50,
/// `docs/kb-tiers.md` "Lite, measured"): nDCG@10 0.802 against 0.811 for
/// Qwen3-0.6B (−0.009 [−0.047, +0.027], a tie), but 1,485 passages ingest in
/// ~2 min instead of 86 min and a search takes 42 ms instead of 802 ms.
const E5_SMALL_TEXT: &[(&str, &str)] = &[
    ("KB_EMBEDDING_MODEL", E5_SMALL_EMBEDDING),
    ("KB_EMBEDDING_DIM", "384"),
    ("KB_EMBEDDING_QUERY_PREFIX", "query: "),
    ("KB_EMBEDDING_PASSAGE_PREFIX", "passage: "),
];

/// Lite-en embeds text with all-MiniLM-L6-v2 (6 layers, English only) and
/// gives the dense vector only the first 512 characters of each chunk; BM25 and
/// the stored chunk keep the full text. Measured on 2 pinned cores
/// (2026-10-25, ViDoRe HR, `docs/eval-ab/lite-2core`): 6.43 pages/s at page
/// nDCG@10 0.517, against 1.55 pages/s at 0.545 for Lite's e5-small. On
/// Indonesian (MIRACL-id-50) it scores 0.679 against 0.802, so it is for
/// English-only deployments. MiniLM takes no prefixes, and the prefix
/// settings already default to empty.
const MINILM_TEXT: &[(&str, &str)] = &[
    ("KB_EMBEDDING_MODEL", MINILM_EMBEDDING),
    ("KB_EMBEDDING_DIM", "384"),
    ("KB_EMBEDDING_MAX_CHARS", "512"),
];

/// The chunk reranker Desk and up run: bge at the local `/rerank` endpoint.
/// The provider is `tei` because that is the base-URL-plus-`/rerank` shape
/// (`docs/local-models.md`); llama.cpp's `/v1/rerank` is the same Cohere shape
/// reached as `cohere`, and an operator who uses it sets the provider by hand.
const LOCAL_RERANK: &[(&str, &str)] = &[
    ("KB_RERANK_ENABLED", "true"),
    ("KB_RERANK_PROVIDER", "tei"),
    ("KB_RERANK_BASE_URL", LOCAL_RERANK_URL),
    ("KB_RERANK_MODEL", BGE_RERANKER),
];

/// Lite: no reranker, one embedding at a time.
///
/// Embedding concurrency is filled as `KB_EMBED_CONCURRENCY`, the name Bun
/// reads, which the Rust config honours when `KB_EMBEDDING_CONCURRENCY` is
/// unset. Filling the Rust name instead would shadow an operator's Bun-name
/// setting; this way neither name can be shadowed by the preset.
const LITE: &[(&str, &str)] = &[
    ("KB_EMBED_CONCURRENCY", "1"),
    ("KB_EMBEDDING_BATCH_SIZE", "16"),
    ("KB_INGEST_CONCURRENCY", "1"),
    ("KB_RERANK_ENABLED", "false"),
];

const DESK: &[(&str, &str)] = &[
    ("KB_EMBED_CONCURRENCY", "2"),
    ("KB_RERANK_CANDIDATE_LIMIT", "10"),
];

const STUDIO: &[(&str, &str)] = &[
    ("KB_EMBED_CONCURRENCY", "4"),
    ("KB_RERANK_CANDIDATE_LIMIT", "20"),
];

const STATION: &[(&str, &str)] = &[
    ("KB_EMBED_CONCURRENCY", "8"),
    ("KB_RERANK_CANDIDATE_LIMIT", "30"),
];

/// Cloud: one hosted embedder for text and pages, voyage's reranker over the
/// same OpenRouter key. Qdrant is the only chunk store, so the page arm has
/// nothing to select; set `KB_PAGE_EMBEDDING_MODEL=` (empty) to drop the arm.
const CLOUD: &[(&str, &str)] = &[
    ("KB_EMBEDDING_MODEL", VOYAGE_EMBEDDING),
    ("KB_EMBEDDING_DIM", "1024"),
    ("KB_EMBEDDING_INPUT_TYPE", "true"),
    ("KB_PAGE_EMBEDDING_MODEL", VOYAGE_EMBEDDING),
    ("KB_PAGE_EMBEDDING_DIM", "1024"),
    ("KB_PAGE_RERANK", "true"),
    ("KB_RERANK_ENABLED", "true"),
    ("KB_RERANK_PROVIDER", "cohere"),
    ("KB_RERANK_BASE_URL", OPENROUTER_RERANK),
    ("KB_RERANK_MODEL", VOYAGE_RERANKER),
    ("KB_RERANK_CANDIDATE_LIMIT", "20"),
    // Entity extraction is an LLM call per document (OpenRouter) whose output
    // goes only to the Postgres graph when `KB_GRAPH_STORE=pg`: pure spend
    // otherwise.
    ("KB_ENTITY_EXTRACTION_ENABLED", "false"),
];

impl Tier {
    /// The tier's `KB_TIER` name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lite => "lite",
            Self::LiteEn => "lite-en",
            Self::Desk => "desk",
            Self::Studio => "studio",
            Self::Station => "station",
            Self::Cloud => "cloud",
        }
    }

    /// Parse a `KB_TIER` value (case-insensitive, trimmed). `None` when the
    /// value names no tier; [`Tier::from_lookup`] turns that into an error
    /// that lists the valid names.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "lite" => Some(Self::Lite),
            "lite-en" => Some(Self::LiteEn),
            "desk" => Some(Self::Desk),
            "studio" => Some(Self::Studio),
            "station" => Some(Self::Station),
            "cloud" => Some(Self::Cloud),
            _ => None,
        }
    }

    /// Read `KB_TIER`. Unset or blank means "no preset", which is exactly the
    /// behaviour before tiers existed.
    ///
    /// # Errors
    ///
    /// Fails when `KB_TIER` names a tier that does not exist; the message lists
    /// the valid values.
    pub fn from_lookup<F>(lookup: &F) -> Result<Option<Self>>
    where
        F: Fn(&str) -> Option<String>,
    {
        let Some(raw) = lookup("KB_TIER") else {
            return Ok(None);
        };
        if raw.trim().is_empty() {
            return Ok(None);
        }
        match Self::parse(&raw) {
            Some(tier) => Ok(Some(tier)),
            None => anyhow::bail!(
                "KB_TIER must be one of {} (or unset), got {raw:?}",
                TIER_NAMES.join(", ")
            ),
        }
    }

    /// Whether the tier's default stack is hosted and therefore needs an
    /// OpenRouter key at boot. The local tiers embed and rerank on the
    /// operator's own hardware and need no paid key.
    #[must_use]
    pub fn needs_openrouter_key(self) -> bool {
        matches!(self, Self::Cloud)
    }

    /// The preset tables that apply to this tier, most general first.
    fn tables(self) -> &'static [&'static [(&'static str, &'static str)]] {
        match self {
            Self::Lite => &[LOCAL, E5_SMALL_TEXT, LITE],
            Self::LiteEn => &[LOCAL, MINILM_TEXT, LITE],
            Self::Desk => &[LOCAL, QWEN3_TEXT, LOCAL_RERANK, DESK],
            Self::Studio => &[LOCAL, QWEN3_TEXT, LOCAL_RERANK, STUDIO],
            Self::Station => &[LOCAL, QWEN3_TEXT, LOCAL_RERANK, STATION],
            Self::Cloud => &[CLOUD],
        }
    }

    /// Every `(variable, value)` pair this tier could fill, in table order.
    #[must_use]
    pub fn presets(self) -> Vec<(&'static str, &'static str)> {
        self.tables()
            .iter()
            .flat_map(|table| table.iter().copied())
            .collect()
    }

    /// The preset value for `variable`, or `None` when this tier fills nothing.
    #[must_use]
    pub fn preset(self, variable: &str) -> Option<&'static str> {
        self.tables()
            .iter()
            .flat_map(|table| table.iter())
            .find(|(name, _)| *name == variable)
            .map(|(_, value)| *value)
    }

    /// The variables this tier fills for an operator who set none of them, as
    /// `(variable, value)`, skipping anything present in the environment.
    #[must_use]
    pub fn filled<F>(self, lookup: &F) -> Vec<(&'static str, &'static str)>
    where
        F: Fn(&str) -> Option<String>,
    {
        self.presets()
            .into_iter()
            .filter(|(name, _)| lookup(name).is_none())
            .collect()
    }
}

/// What a tier did, kept on the resolved config so `serve` can say so at boot
/// and tests can assert it.
#[derive(Clone, Debug)]
pub struct TierPlan {
    pub tier: Tier,
    /// Variables the operator left unset, so the preset filled them.
    pub filled: Vec<(&'static str, &'static str)>,
}

impl TierPlan {
    /// Resolve what `tier` will fill against the operator's environment.
    #[must_use]
    pub fn resolve<F>(tier: Tier, lookup: &F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        Self {
            tier,
            filled: tier.filled(lookup),
        }
    }

    /// Log one INFO line: the tier, then every variable it filled, so an
    /// operator can see what the preset did. Values are shown (they are model
    /// names and numbers); a key-like name is redacted as a guard.
    pub fn log(&self) {
        tracing::info!(
            tier = self.tier.as_str(),
            settings = %self.describe(),
            "KB_TIER preset applied (an explicit environment variable wins)"
        );
    }

    fn describe(&self) -> String {
        if self.filled.is_empty() {
            return "nothing: every tier setting was set explicitly".into();
        }
        self.filled
            .iter()
            .map(|(name, value)| {
                if is_secret(name) {
                    format!("{name}=<redacted>")
                } else {
                    // `escape_default` keeps the line one line: the Qwen3
                    // query instruction contains a newline.
                    format!("{name}={}", value.escape_default())
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// A variable whose value must never reach a log line.
fn is_secret(name: &str) -> bool {
    ["KEY", "SECRET", "TOKEN", "PASSWORD"]
        .iter()
        .any(|fragment| name.contains(fragment))
}

/// Wrap an environment lookup so an unset variable takes the tier's preset
/// value. This is what the composition root reads from, so a preset-filled
/// value behaves exactly like one written in the environment.
pub fn with_preset<'a, F>(tier: Option<Tier>, lookup: &'a F) -> impl Fn(&str) -> Option<String> + 'a
where
    F: Fn(&str) -> Option<String>,
{
    move |name| lookup(name).or_else(|| tier.and_then(|tier| tier.preset(name)).map(str::to_string))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn every_tier_name_round_trips() {
        for name in TIER_NAMES {
            let tier = Tier::parse(name).expect("known tier");
            assert_eq!(tier.as_str(), name);
            assert_eq!(Tier::parse(&name.to_uppercase()), Some(tier));
            assert_eq!(Tier::parse(&format!("  {name}\n")), Some(tier));
        }
        assert_eq!(Tier::parse("tiny"), None);
    }

    #[test]
    fn no_table_names_a_variable_twice() {
        // A duplicate would silently shadow the earlier row.
        for name in TIER_NAMES {
            let tier = Tier::parse(name).expect("known tier");
            let presets = tier.presets();
            let mut seen = std::collections::HashSet::new();
            for (variable, _) in &presets {
                assert!(
                    seen.insert(*variable),
                    "{name} fills {variable} more than once"
                );
            }
        }
    }

    #[test]
    fn unknown_tier_is_an_error_that_lists_the_valid_values() {
        let lookup = |k: &str| (k == "KB_TIER").then(|| "gpu".to_string());
        let err = Tier::from_lookup(&lookup).unwrap_err().to_string();
        assert!(err.contains("KB_TIER"), "{err}");
        assert!(err.contains("gpu"), "{err}");
        for name in TIER_NAMES {
            assert!(err.contains(name), "{err} does not name {name}");
        }
    }

    #[test]
    fn lite_en_is_lite_with_minilm_and_a_512_char_embedding_cap() {
        let none = |_: &str| None;
        let lite_en = with_preset(Some(Tier::LiteEn), &none);
        let lite = with_preset(Some(Tier::Lite), &none);
        assert_eq!(
            lite_en("KB_EMBEDDING_MODEL").as_deref(),
            Some("sentence-transformers/all-MiniLM-L6-v2")
        );
        assert_eq!(lite_en("KB_EMBEDDING_DIM").as_deref(), Some("384"));
        assert_eq!(lite_en("KB_EMBEDDING_MAX_CHARS").as_deref(), Some("512"));
        // MiniLM takes no e5 prefixes.
        assert_eq!(lite_en("KB_EMBEDDING_QUERY_PREFIX"), None);
        assert_eq!(lite_en("KB_EMBEDDING_PASSAGE_PREFIX"), None);
        // Everything else is Lite's.
        for variable in [
            "KB_EMBEDDING_BASE_URL",
            "KB_RERANK_ENABLED",
            "KB_INGEST_CONCURRENCY",
        ] {
            assert_eq!(lite_en(variable), lite(variable), "{variable}");
        }
        assert!(!Tier::LiteEn.needs_openrouter_key());
    }

    #[test]
    fn no_tier_pays_for_entity_extraction_by_default() {
        let none = |_: &str| None;
        for name in TIER_NAMES {
            let tier = Tier::parse(name).expect("known tier");
            assert_eq!(
                with_preset(Some(tier), &none)("KB_ENTITY_EXTRACTION_ENABLED").as_deref(),
                Some("false"),
                "{name}"
            );
        }
    }

    #[test]
    fn a_blank_tier_is_no_tier() {
        let lookup = |k: &str| (k == "KB_TIER").then(|| "  ".to_string());
        assert_eq!(Tier::from_lookup(&lookup).unwrap(), None);
        assert_eq!(Tier::from_lookup(&|_: &str| None).unwrap(), None);
    }

    #[test]
    fn with_preset_fills_only_unset_variables() {
        let mut env = HashMap::new();
        env.insert("KB_EMBEDDING_MODEL", "operator/model");
        // Present but empty: explicit, so it suppresses the preset and the
        // setting falls back to its own default.
        env.insert("KB_RERANK_ENABLED", "");
        let lookup = |k: &str| env.get(k).map(|v| (*v).to_string());

        let effective = with_preset(Some(Tier::Lite), &lookup);
        assert_eq!(
            effective("KB_EMBEDDING_MODEL").as_deref(),
            Some("operator/model"),
            "an explicit value wins"
        );
        assert_eq!(effective("KB_RERANK_ENABLED").as_deref(), Some(""));
        assert_eq!(
            effective("KB_EMBEDDING_DIM").as_deref(),
            Some("384"),
            "an unset variable takes the preset"
        );
        assert_eq!(effective("NOT_A_KB_SETTING"), None);

        // No tier: every read is the operator's environment, untouched.
        let bare = with_preset(None, &lookup);
        assert_eq!(bare("KB_EMBEDDING_DIM"), None);

        let plan = TierPlan::resolve(Tier::Lite, &lookup);
        assert_eq!(plan.tier, Tier::Lite);
        assert!(
            !plan.filled.iter().any(|(n, _)| *n == "KB_EMBEDDING_MODEL"),
            "an explicit value is not reported as filled"
        );
        assert!(plan.filled.iter().any(|(n, _)| *n == "KB_EMBEDDING_DIM"));
    }

    #[test]
    fn apply_schema_builds_the_index_at_the_tiers_width() {
        // `kb-server apply-schema` resolves the tier exactly like this before
        // reading the dimension, or the schema and the server would disagree.
        let env = |k: &str| (k == "KB_TIER").then(|| "cloud".to_string());
        let tier = Tier::from_lookup(&env).expect("valid tier");
        let lookup = with_preset(tier, &env);
        assert_eq!(crate::config::Config::embedding_dim(&lookup), 1024);
        // Without a tier the dimension is today's 4096.
        assert_eq!(crate::config::Config::embedding_dim(&|_: &str| None), 4096);
    }

    #[test]
    fn the_plan_never_prints_a_secret_shaped_value() {
        let plan = TierPlan {
            tier: Tier::Cloud,
            filled: vec![("KB_RERANK_API_KEY", "sk-do-not-print")],
        };
        let line = plan.describe();
        assert!(!line.contains("sk-do-not-print"), "{line}");
        assert!(line.contains("KB_RERANK_API_KEY=<redacted>"), "{line}");
    }

    #[test]
    fn the_plan_stays_on_one_line_even_for_a_multiline_value() {
        // Desk carries the multiline Qwen3 instruction.
        let plan = TierPlan::resolve(Tier::Desk, &|_: &str| None);
        let line = plan.describe();
        assert!(!line.contains('\n'), "the log must stay one line: {line}");
        assert!(
            line.contains("KB_EMBEDDING_QUERY_PREFIX=Instruct:"),
            "{line}"
        );
        assert!(
            line.contains("\\nQuery: "),
            "the newline is escaped: {line}"
        );
    }
}
