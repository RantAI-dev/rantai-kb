//! The VLM gate: let a vision model veto figures before a student sees them.
//!
//! Mirrors `src/lib/rag/figure-gate.ts`.
//! Off unless `KB_FIGURE_VLM_ENABLED` is set to "1" or "true".

use fancy_regex::Regex;
use std::sync::LazyLock;

/// Verbatim prompt from TypeScript reference (`src/lib/rag/figure-gate.ts:60`).
/// Do not alter the wording: the prompt was empirically calibrated for precision.
pub const PROMPT_STRICT: &str = "\
Kamu menilai apakah sebuah gambar dari buku pelajaran WAJIB ditampilkan untuk menjawab pertanyaan siswa.\n\
\n\
Pertanyaan siswa: {Q}\n\
\n\
Lihat gambar di atas.\n\
\n\
Jawab \"YA\" HANYA jika gambar ini memuat informasi yang DIBUTUHKAN untuk menjawab pertanyaan itu \u{2014}\n\
misalnya angka, bentuk, langkah, atau bagian berlabel yang tidak bisa dijelaskan dengan kata-kata saja.\n\
\n\
Jawab \"TIDAK\" untuk semua kasus lain, termasuk:\n\
- gambar yang topiknya berhubungan tetapi tidak dibutuhkan untuk menjawab\n\
- foto orang, suasana, atau kegiatan sebagai hiasan\n\
- gambar pembuka bab atau latar halaman\n\
- gambar yang hanya \"cocok temanya\"\n\
\n\
Patokan penting: dari setiap 20 gambar dalam buku, biasanya HANYA 1 yang benar-benar dibutuhkan\n\
untuk sebuah pertanyaan tertentu. Kalau kamu menjawab YA lebih sering dari itu, kamu terlalu longgar.\n\
\n\
Kalau ragu sedikit pun, jawab TIDAK.\n\
\n\
Jawab HANYA satu kata: YA atau TIDAK.";

static RE_YA: LazyLock<Regex> = LazyLock::new(|| match Regex::new(r"(?i)\bYA\b") {
    Ok(r) => r,
    Err(_) => std::process::abort(),
});

static RE_TIDAK: LazyLock<Regex> = LazyLock::new(|| match Regex::new(r"(?i)\bTIDAK\b") {
    Ok(r) => r,
    Err(_) => std::process::abort(),
});

/// "YA" only when the model said yes and did not also say no.
/// Anything unparseable is false. Failing toward silence prevents false positives.
#[must_use]
pub fn parse_verdict(reply: &str) -> bool {
    let text = reply.trim();
    let has_ya = RE_YA.is_match(text).unwrap_or(false);
    let has_tidak = RE_TIDAK.is_match(text).unwrap_or(false);
    has_ya && !has_tidak
}

/// Configuration for the VLM figure gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateConfig {
    /// Base URL of the vision model endpoint.
    pub base: String,
    /// Vision model name (e.g. "google/gemini-2.5-flash").
    pub model: String,
    /// Maximum candidate figures submitted to the gate.
    pub top_n: usize,
    /// Maximum surviving figures allowed through.
    pub max_keep: usize,
    /// Timeout in milliseconds per vision model call.
    pub timeout_ms: u64,
}

impl GateConfig {
    /// Read gate configuration using a lookup function for environment variables.
    pub fn from_lookup<F>(lookup: F) -> Option<Self>
    where
        F: Fn(&str) -> Option<String>,
    {
        let enabled = lookup("KB_FIGURE_VLM_ENABLED")?;
        if enabled != "1" && enabled != "true" {
            return None;
        }

        let base_raw = lookup("KB_FIGURE_VLM_BASE")?;
        let base = base_raw.trim().trim_end_matches('/').to_string();
        if base.is_empty() {
            return None;
        }

        let model = lookup("KB_FIGURE_VLM_MODEL")?.trim().to_string();
        if model.is_empty() {
            return None;
        }

        let top_n = lookup("KB_FIGURE_VLM_TOPN")
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(2);

        let max_keep = lookup("KB_FIGURE_VLM_MAX")
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(1);

        let timeout_ms = lookup("KB_FIGURE_VLM_TIMEOUT_MS")
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|&ms| ms > 0)
            .unwrap_or(4000);

        Some(Self {
            base,
            model,
            top_n,
            max_keep,
            timeout_ms,
        })
    }

    /// Read gate configuration from standard environment variables.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn parse_verdict_accepts_bare_ya() {
        assert!(parse_verdict("YA"));
        assert!(parse_verdict(" ya \n"));
        assert!(parse_verdict("Jawaban: YA"));
    }

    #[test]
    fn parse_verdict_rejects_tidak_including_hedging() {
        assert!(!parse_verdict("TIDAK"));
        assert!(!parse_verdict("YA, tapi TIDAK dibutuhkan"));
        assert!(!parse_verdict("tidak"));
    }

    #[test]
    fn parse_verdict_rejects_unparseable_junk() {
        for junk in ["", "   ", "mungkin", "1", "YANG", "bisa jadi"] {
            assert!(!parse_verdict(junk));
        }
    }

    #[test]
    fn gate_config_is_off_unless_explicitly_enabled() {
        let env = HashMap::<String, String>::new();
        assert!(GateConfig::from_lookup(|k| env.get(k).cloned()).is_none());

        let mut env2 = HashMap::new();
        env2.insert("KB_FIGURE_VLM_BASE".to_string(), "http://x/v1".to_string());
        env2.insert("KB_FIGURE_VLM_MODEL".to_string(), "m".to_string());
        assert!(GateConfig::from_lookup(|k| env2.get(k).cloned()).is_none());
    }

    #[test]
    fn gate_config_stays_off_when_incomplete() {
        let mut env = HashMap::new();
        env.insert("KB_FIGURE_VLM_ENABLED".to_string(), "1".to_string());
        assert!(GateConfig::from_lookup(|k| env.get(k).cloned()).is_none());

        env.insert("KB_FIGURE_VLM_BASE".to_string(), "http://x/v1".to_string());
        // Missing MODEL
        assert!(GateConfig::from_lookup(|k| env.get(k).cloned()).is_none());
    }

    #[test]
    fn gate_config_reads_defaults_and_trims_slash() {
        let mut env = HashMap::new();
        env.insert("KB_FIGURE_VLM_ENABLED".to_string(), "1".to_string());
        env.insert("KB_FIGURE_VLM_BASE".to_string(), "http://x/v1/".to_string());
        env.insert("KB_FIGURE_VLM_MODEL".to_string(), "m".to_string());

        let cfg = GateConfig::from_lookup(|k| env.get(k).cloned()).unwrap();
        assert_eq!(cfg.base, "http://x/v1");
        assert_eq!(cfg.model, "m");
        assert_eq!(cfg.top_n, 2);
        assert_eq!(cfg.max_keep, 1);
        assert_eq!(cfg.timeout_ms, 4000);
    }

    #[test]
    fn gate_config_ignores_zero_top_n() {
        let mut env = HashMap::new();
        env.insert("KB_FIGURE_VLM_ENABLED".to_string(), "true".to_string());
        env.insert("KB_FIGURE_VLM_BASE".to_string(), "http://x/v1".to_string());
        env.insert("KB_FIGURE_VLM_MODEL".to_string(), "m".to_string());
        env.insert("KB_FIGURE_VLM_TOPN".to_string(), "0".to_string());

        let cfg = GateConfig::from_lookup(|k| env.get(k).cloned()).unwrap();
        assert_eq!(cfg.top_n, 2);
    }
}
