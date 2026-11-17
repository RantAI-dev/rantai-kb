//! `kb-server setup`: take a machine from nothing to a configured tier.
//!
//! The command is deliberately split into pure pieces so it can be tested on a
//! machine that has neither the hardware nor the services:
//!
//! 1. [`detect`] reads the hardware through a [`CommandRunner`], which is a
//!    trait so tests feed `nvidia-smi`/`sysctl` output instead of running them.
//! 2. [`suggest_tier`] is a pure table over the detected VRAM/unified memory.
//! 3. [`check_budget`] compares the estimates in `docs/kb-tiers.md` to that
//!    hardware, and [`gate_licences`] refuses a 🔴 model or an unaccepted 🟡 one.
//! 4. [`plan_and_write`] renders and writes `rust/deploy/.env` (and `.tier`),
//!    and never touches a file unless it was asked to.
//! 5. `--check` probes the resolved endpoints with one real request each.
//!
//! Nothing in detect/plan/write needs Postgres, Qdrant, a model or Docker: it
//! only writes the file `rust/scripts/kb-dev.sh up <tier>` reads next. Model
//! download and start stay with the containers (the first start pulls the
//! weights into the `hf-models` volume), and residency/caption queues are not
//! built yet — `docs/kb-tiers.md` says so.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::cli::CliError;
use crate::tier::{TIER_NAMES, Tier};

// ---------------------------------------------------------------------------
// Hardware detection
// ---------------------------------------------------------------------------

/// Runs external commands and reads files. Tests implement this with canned
/// output, so hardware detection is exercised without `nvidia-smi` or macOS.
pub trait CommandRunner {
    /// Trimmed stdout of a successful command, or `None` when the program is
    /// missing or exits non-zero.
    fn stdout(&self, program: &str, args: &[&str]) -> Option<String>;
    /// File contents, or `None` when it cannot be read.
    fn read_file(&self, path: &str) -> Option<String>;
    /// Logical CPU count.
    fn cores(&self) -> usize;
}

/// The real machine.
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn stdout(&self, program: &str, args: &[&str]) -> Option<String> {
        let output = std::process::Command::new(program)
            .args(args)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (!text.is_empty()).then_some(text)
    }

    fn read_file(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn cores(&self) -> usize {
        std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1)
    }
}

/// Which vendor serves the accelerator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuVendor {
    Nvidia,
    /// Apple silicon: unified memory, no separate VRAM.
    Apple,
    /// AMD, Intel or anything else: detected but unsupported.
    Other,
}

/// One accelerator as reported by the hardware.
#[derive(Clone, Debug, PartialEq)]
pub struct Gpu {
    pub vendor: GpuVendor,
    pub name: String,
    /// Per-GPU memory in GiB (unified memory for Apple).
    pub memory_gb: f64,
}

/// What the machine has, after detection.
#[derive(Clone, Debug, PartialEq)]
pub struct Hardware {
    /// `uname -s` (`Linux`, `Darwin`, …).
    pub os: String,
    /// CPU model string, when one could be read.
    pub cpu: Option<String>,
    pub cores: usize,
    /// Total RAM in GiB (unified memory on Apple silicon).
    pub ram_gb: f64,
    pub gpus: Vec<Gpu>,
    /// Things the operator should know: an unsupported accelerator, an
    /// `nvidia-smi` output we could not read.
    pub notes: Vec<String>,
}

impl Hardware {
    /// Whether an NVIDIA GPU was found. Only these can run the compose
    /// profiles' CUDA/TEI containers.
    #[must_use]
    pub fn has_nvidia(&self) -> bool {
        self.gpus.iter().any(|gpu| gpu.vendor == GpuVendor::Nvidia)
    }

    /// Whether this is Apple silicon (Darwin plus a unified-memory reading).
    #[must_use]
    pub fn is_apple(&self) -> bool {
        self.os == "Darwin" && self.gpus.iter().any(|gpu| gpu.vendor == GpuVendor::Apple)
    }

    /// The memory a tier's models can use: the largest single NVIDIA card
    /// (compose reserves one GPU per model container, so cards do not pool),
    /// or the unified memory on Apple silicon.
    #[must_use]
    pub fn usable_vram_gb(&self) -> Option<f64> {
        if self.is_apple() {
            return self.ram_gb.into();
        }
        self.gpus
            .iter()
            .filter(|gpu| gpu.vendor == GpuVendor::Nvidia)
            .map(|gpu| gpu.memory_gb)
            .fold(None, |best: Option<f64>, gb| {
                Some(best.map_or(gb, |b| b.max(gb)))
            })
    }
}

/// `nvidia-smi --query-gpu=name,memory.total --format=csv,noheader,nounits`.
///
/// One line per GPU, `name, MiB`. A line that cannot be parsed is an error, not
/// a silent zero: a wrong VRAM reading would suggest the wrong tier.
///
/// # Errors
///
/// Fails on an empty or unparseable line.
pub fn parse_nvidia_smi(output: &str) -> Result<Vec<Gpu>, String> {
    let mut gpus = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((name, memory)) = line.rsplit_once(',') else {
            return Err(format!("unrecognised nvidia-smi line {line:?}"));
        };
        let name = name.trim();
        let memory = memory.trim();
        let mib: f64 = memory
            .parse()
            .map_err(|_| format!("unrecognised nvidia-smi memory {memory:?} in {line:?}"))?;
        if name.is_empty() {
            return Err(format!("nvidia-smi line has no GPU name: {line:?}"));
        }
        gpus.push(Gpu {
            vendor: GpuVendor::Nvidia,
            name: name.to_string(),
            memory_gb: mib / 1024.0,
        });
    }
    if gpus.is_empty() {
        return Err("nvidia-smi returned no GPUs".to_string());
    }
    Ok(gpus)
}

/// Parse `sysctl -n hw.memsize` (bytes) into GiB.
fn parse_memsize(bytes: &str) -> Option<f64> {
    bytes
        .trim()
        .parse::<u64>()
        .ok()
        .map(|bytes| bytes as f64 / 1024.0 / 1024.0 / 1024.0)
}

/// Parse `MemTotal: 12246836 kB` from `/proc/meminfo`.
fn parse_meminfo(text: &str) -> Option<f64> {
    let line = text.lines().find(|line| line.starts_with("MemTotal:"))?;
    let kb: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024.0 / 1024.0)
}

/// The CPU model from `/proc/cpuinfo`.
fn parse_cpuinfo(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix("model name"))
        .and_then(|rest| rest.split_once(':'))
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// A virtual display adapter is not an accelerator; reporting it as "AMD/other"
/// would make every VM look like it had a GPU.
fn is_virtual_display(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    [
        "qemu",
        "bochs",
        "vmware",
        "virtualbox",
        "cirrus",
        "red hat",
        "1234:1111",
        "microsoft",
        "hyper-v",
        "parallels",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Detect the machine through `runner`.
#[must_use]
pub fn detect(runner: &dyn CommandRunner) -> Hardware {
    let os = runner
        .stdout("uname", &["-s"])
        .unwrap_or_else(|| std::env::consts::OS.to_string());
    let mut notes = Vec::new();
    let mut gpus = Vec::new();

    // NVIDIA first: `nvidia-smi` is the only source of per-card VRAM.
    match runner.stdout(
        "nvidia-smi",
        &[
            "--query-gpu=name,memory.total",
            "--format=csv,noheader,nounits",
        ],
    ) {
        None => {}
        Some(output) => match parse_nvidia_smi(&output) {
            Ok(found) => gpus.extend(found),
            Err(err) => notes.push(format!(
                "nvidia-smi answered but the output was not understood ({err}); \
                 treating this machine as having no NVIDIA GPU"
            )),
        },
    }

    let mut ram_gb = 0.0;
    let mut cpu = None;
    if os == "Darwin" {
        // Apple silicon: `hw.memsize` is the unified memory the GPU can use.
        if let Some(bytes) = runner.stdout("sysctl", &["-n", "hw.memsize"]) {
            if let Some(gb) = parse_memsize(&bytes) {
                ram_gb = gb;
                let name = runner
                    .stdout("sysctl", &["-n", "machdep.cpu.brand_string"])
                    .unwrap_or_else(|| "Apple silicon".to_string());
                cpu = Some(name.clone());
                gpus.push(Gpu {
                    vendor: GpuVendor::Apple,
                    name,
                    memory_gb: gb,
                });
            } else {
                notes.push(format!(
                    "sysctl hw.memsize returned {bytes:?}, which is not a byte count"
                ));
            }
        } else {
            notes.push("sysctl hw.memsize is unavailable: unified memory unknown".to_string());
        }
    } else {
        if let Some(meminfo) = runner.read_file("/proc/meminfo") {
            if let Some(gb) = parse_meminfo(&meminfo) {
                ram_gb = gb;
            }
        }
        if let Some(cpuinfo) = runner.read_file("/proc/cpuinfo") {
            cpu = parse_cpuinfo(&cpuinfo);
        }
        // AMD/Intel/other accelerators are detected only to say they are not
        // used: the tier profiles here are NVIDIA (vLLM/TEI) or Apple.
        if let Some(lspci) = runner.stdout("lspci", &[]) {
            for line in lspci.lines() {
                let lower = line.to_ascii_lowercase();
                let is_display = lower.contains("vga compatible controller")
                    || lower.contains("3d controller")
                    || lower.contains("display controller");
                if !is_display || is_virtual_display(line) {
                    continue;
                }
                let name = line
                    .split_once(':')
                    .and_then(|(_, rest)| rest.split_once(':'))
                    .map_or_else(
                        || line.trim().to_string(),
                        |(_, name)| name.trim().to_string(),
                    );
                gpus.push(Gpu {
                    vendor: GpuVendor::Other,
                    name,
                    memory_gb: 0.0,
                });
            }
        }
    }

    for gpu in gpus.iter().filter(|gpu| gpu.vendor == GpuVendor::Other) {
        notes.push(format!(
            "unsupported accelerator ({}), treated as CPU: only NVIDIA (vLLM/TEI) and \
             Apple silicon (MLX/llama.cpp) are supported, and there is no Windows tier",
            gpu.name
        ));
    }

    Hardware {
        os,
        cpu,
        cores: runner.cores(),
        ram_gb,
        gpus,
        notes,
    }
}

// ---------------------------------------------------------------------------
// Tier suggestion
// ---------------------------------------------------------------------------

/// A suggested tier and the sentence explaining why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TierSuggestion {
    pub tier: Tier,
    pub reason: String,
}

/// Suggest a tier from the detected hardware.
///
/// NVIDIA: ≥48 GiB → station, ≥24 → studio, ≥8 → desk, else CPU.
/// Apple unified memory: ≥128 GiB → station, ≥32 → studio, ≥16 → desk, else CPU.
/// Everything else is `lite` (or `lite-en` with `--english`).
///
/// `cloud` is never suggested: it spends money on every page and query, so only
/// an explicit `--tier cloud` selects it.
#[must_use]
pub fn suggest_tier(hardware: &Hardware, english: bool) -> TierSuggestion {
    let cpu_tier = if english { Tier::LiteEn } else { Tier::Lite };
    let cpu_reason = if english {
        "no supported GPU: the CPU-only tier, English-only variant (--english)"
    } else {
        "no supported GPU: the CPU-only tier (pass --english for the English-only variant)"
    };

    if hardware.is_apple() {
        let gb = hardware.ram_gb;
        let tier = if gb >= 128.0 {
            Tier::Station
        } else if gb >= 32.0 {
            Tier::Studio
        } else if gb >= 16.0 {
            Tier::Desk
        } else {
            return TierSuggestion {
                tier: cpu_tier,
                reason: format!(
                    "{gb:.1} GB unified memory is below Desk's 16 GB minimum: {cpu_reason}"
                ),
            };
        };
        return TierSuggestion {
            tier,
            reason: format!(
                "Apple silicon with {gb:.1} GB unified memory ({}); {} needs {}",
                hardware.cpu.as_deref().unwrap_or("unknown CPU"),
                tier.as_str(),
                apple_threshold(tier)
            ),
        };
    }

    if let Some(gb) = hardware.usable_vram_gb() {
        let tier = if gb >= 48.0 {
            Tier::Station
        } else if gb >= 24.0 {
            Tier::Studio
        } else if gb >= 8.0 {
            Tier::Desk
        } else {
            return TierSuggestion {
                tier: cpu_tier,
                reason: format!(
                    "the largest NVIDIA GPU has {gb:.1} GB VRAM, below Desk's 8 GB minimum: \
                     {cpu_reason}"
                ),
            };
        };
        let name = hardware
            .gpus
            .iter()
            .filter(|gpu| gpu.vendor == GpuVendor::Nvidia)
            .max_by(|a, b| a.memory_gb.total_cmp(&b.memory_gb))
            .map_or("NVIDIA GPU", |gpu| gpu.name.as_str());
        return TierSuggestion {
            tier,
            reason: format!(
                "NVIDIA {name} with {gb:.1} GB VRAM; {} needs {}",
                tier.as_str(),
                nvidia_threshold(tier)
            ),
        };
    }

    TierSuggestion {
        tier: cpu_tier,
        reason: cpu_reason.to_string(),
    }
}

fn apple_threshold(tier: Tier) -> &'static str {
    match tier {
        Tier::Station => "≥128 GB unified memory",
        Tier::Studio => "≥32 GB unified memory",
        Tier::Desk => "≥16 GB unified memory",
        _ => "no GPU",
    }
}

fn nvidia_threshold(tier: Tier) -> &'static str {
    match tier {
        Tier::Station => "≥48 GB VRAM",
        Tier::Studio => "≥24 GB VRAM",
        Tier::Desk => "≥8 GB VRAM",
        _ => "no GPU",
    }
}

// ---------------------------------------------------------------------------
// VRAM budget
// ---------------------------------------------------------------------------

/// The per-tier VRAM estimate from `docs/kb-tiers.md` ("What each tier runs").
/// **Every number is an estimate from model sizes, not a measurement.**
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TierBudget {
    pub chat_gb: f64,
    pub kb_gb: f64,
}

impl TierBudget {
    #[must_use]
    pub fn total_gb(self) -> f64 {
        self.chat_gb + self.kb_gb
    }
}

/// The estimate for a tier, or `None` when the tier holds no local model
/// (lite is CPU-only, cloud is hosted).
#[must_use]
pub fn tier_budget(tier: Tier) -> Option<TierBudget> {
    match tier {
        Tier::Desk => Some(TierBudget {
            chat_gb: 3.5,
            kb_gb: 4.5,
        }),
        Tier::Studio => Some(TierBudget {
            chat_gb: 9.0,
            kb_gb: 10.0,
        }),
        Tier::Station => Some(TierBudget {
            chat_gb: 26.0,
            kb_gb: 20.0,
        }),
        Tier::Lite | Tier::LiteEn | Tier::Cloud => None,
    }
}

/// The outcome of comparing a tier's estimate to the detected memory.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BudgetCheck {
    /// `None` for a tier with no local model.
    pub required_gb: Option<f64>,
    /// `None` when no usable accelerator was found.
    pub available_gb: Option<f64>,
    pub fits: bool,
}

/// Compare a tier's estimate to the hardware.
#[must_use]
pub fn check_budget(hardware: &Hardware, tier: Tier) -> BudgetCheck {
    let Some(budget) = tier_budget(tier) else {
        return BudgetCheck {
            required_gb: None,
            available_gb: hardware.usable_vram_gb(),
            fits: true,
        };
    };
    let required = budget.total_gb();
    let available = hardware.usable_vram_gb();
    BudgetCheck {
        required_gb: Some(required),
        available_gb: available,
        fits: available.is_some_and(|gb| gb >= required),
    }
}

/// Whether the compose profile for this tier needs an NVIDIA GPU.
#[must_use]
pub fn needs_nvidia(tier: Tier) -> bool {
    matches!(tier, Tier::Desk | Tier::Studio | Tier::Station)
}

/// Refuse a tier that this machine cannot run, unless `force`.
///
/// Returns the warnings to print when it is allowed. A CPU-only machine may
/// still write a desk/studio/station config with `--force` — for example when
/// `KB_EMBEDDING_BASE_URL` points at a GPU on another host — but it is not a
/// supported default.
///
/// # Errors
///
/// Fails when the tier needs an NVIDIA GPU and none was found, or when the
/// detected memory is below the tier's estimate.
pub fn check_tier_fit(hardware: &Hardware, tier: Tier, force: bool) -> Result<Vec<String>, String> {
    let mut warnings = Vec::new();

    if needs_nvidia(tier) && !hardware.has_nvidia() && !hardware.is_apple() {
        let message = format!(
            "tier {tier} runs its embedding and reranking models on an NVIDIA GPU \
             (rust/deploy/compose.yaml reserves one GPU per model container), and this \
             machine has none. Use lite/lite-en, or re-run with --force if you point \
             KB_EMBEDDING_BASE_URL / KB_RERANK_BASE_URL at a GPU on another host",
            tier = tier.as_str()
        );
        if !force {
            return Err(message);
        }
        warnings.push(format!("{message} (forced)"));
    }

    if needs_nvidia(tier) && hardware.is_apple() {
        warnings.push(format!(
            "Apple silicon is a supported tier in docs/kb-tiers.md, but \
             rust/deploy/compose.yaml only reserves NVIDIA GPUs: `kb-dev.sh up {}` will \
             not start the models here. Run the embedder and reranker outside compose \
             (MLX/llama.cpp Metal) and set KB_EMBEDDING_BASE_URL / KB_RERANK_BASE_URL",
            tier.as_str()
        ));
    }

    let budget = check_budget(hardware, tier);
    if let Some(required) = budget.required_gb {
        let available = budget.available_gb.unwrap_or(0.0);
        if !budget.fits {
            let message = format!(
                "tier {tier} needs an estimated {required:.1} GB of VRAM (chat LLM plus KB \
                 models; docs/kb-tiers.md estimates, not measurements) and this machine has \
                 {available:.1} GB. It will not fit; use a smaller tier, or re-run with \
                 --force if you have measured that it does",
                tier = tier.as_str()
            );
            if !force {
                return Err(message);
            }
            warnings.push(format!("{message} (forced)"));
        }
    }

    Ok(warnings)
}

// ---------------------------------------------------------------------------
// Licences
// ---------------------------------------------------------------------------

/// The verdict marks `docs/model-licences.md` uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LicenceClass {
    /// Permissive (Apache-2.0 / MIT): a default.
    Green,
    /// Commercial use allowed with conditions: an explicit opt-in.
    Yellow,
    /// Non-commercial or blocked: never.
    Red,
}

impl LicenceClass {
    /// The marker the licence doc already uses.
    #[must_use]
    pub fn marker(self) -> &'static str {
        match self {
            Self::Green => "🟢",
            Self::Yellow => "🟡",
            Self::Red => "🔴",
        }
    }
}

/// One model and its licence verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LicenceEntry {
    pub model: &'static str,
    pub licence: &'static str,
    pub class: LicenceClass,
    pub role: &'static str,
}

const fn entry(
    model: &'static str,
    licence: &'static str,
    class: LicenceClass,
    role: &'static str,
) -> LicenceEntry {
    LicenceEntry {
        model,
        licence,
        class,
        role,
    }
}

/// Every model a tier's preset or compose profile can load, with the verdict
/// from `docs/model-licences.md`, plus one 🔴 row so the gate's refusal is
/// exercised. A model named in `tier.rs` without an entry here fails
/// `every_tier_model_has_a_licence_entry`, so adding a model to a preset cannot
/// skip the licence review.
///
/// Hosted models (voyage, Mistral OCR) are 🟡: the licence doc says their
/// provider terms, not a model licence, govern them, and sending customer
/// documents to them deserves an explicit opt-in just like a 🟡 weight.
pub const LICENCES: &[LicenceEntry] = &[
    entry(
        "Qwen/Qwen3-Embedding-0.6B",
        "Apache-2.0",
        LicenceClass::Green,
        "text embedding, desk/studio/station",
    ),
    entry(
        "intfloat/multilingual-e5-small",
        "MIT",
        LicenceClass::Green,
        "text embedding, lite",
    ),
    entry(
        "sentence-transformers/all-MiniLM-L6-v2",
        "Apache-2.0",
        LicenceClass::Green,
        "text embedding, lite-en",
    ),
    entry(
        "BAAI/bge-reranker-v2-m3",
        "Apache-2.0",
        LicenceClass::Green,
        "text reranker, desk",
    ),
    entry(
        "voyageai/voyage-multimodal-3.5",
        "hosted service, provider terms",
        LicenceClass::Yellow,
        "cloud text and page embedding; billed per token/page",
    ),
    entry(
        "voyageai/rerank-2.5",
        "hosted service, provider terms",
        LicenceClass::Yellow,
        "cloud reranker; billed per search",
    ),
    entry(
        "rantai-kb/ocr-cpu",
        "Apache-2.0 (PP-OCRv6 tiny, PP-Layout CDLA, SLANet-plus)",
        LicenceClass::Green,
        "document OCR on the CPU, lite",
    ),
    entry(
        "xiaomi-research/Xiaomi-OCR-0",
        "Apache-2.0",
        LicenceClass::Green,
        "OCR, desk/studio/station",
    ),
    entry(
        "PaddlePaddle/PP-DocLayoutV3",
        "Apache-2.0",
        LicenceClass::Green,
        "OCR table pass (--ocr-tables accurate), desk/studio/station",
    ),
    entry(
        "PaddlePaddle/PaddleOCR-VL-1.6",
        "Apache-2.0",
        LicenceClass::Green,
        "OCR, alternative (not in a preset)",
    ),
    entry(
        "qihoo360/360LayoutAnalysis",
        "model licence: commercial use by application to 360",
        LicenceClass::Yellow,
        "faster layout model for the CPU OCR sidecar (KB_LITE_LAYOUT_MODEL), opt-in",
    ),
    entry(
        "opendatalab/MinerU2.5-Pro",
        "Apache-2.0 weights (pipeline code 🟡)",
        LicenceClass::Green,
        "OCR, station alternative (not in the preset)",
    ),
    entry(
        "Qwen/Qwen3-VL-2B-Instruct",
        "Apache-2.0",
        LicenceClass::Green,
        "picture reading (ING-5), every local tier",
    ),
    entry(
        "nvidia/Nemotron-3.5-Embed-1B-BF16",
        "openmdw-1.1 (Open Model Weights licence)",
        LicenceClass::Yellow,
        "one embedder for text and pages (DEP-6, KB_EMBEDDERS=one); opt-in like any \u{1f7e1} model",
    ),
    entry(
        "Qwen/Qwen3-Reranker-0.6B",
        "Apache-2.0",
        LicenceClass::Green,
        "chunk reranker for the one-embedder stack (DEP-6)",
    ),
    entry(
        "nvidia/nemotron-3.5-super-vl",
        "hosted service, provider terms",
        LicenceClass::Yellow,
        "cloud picture reading (ING-5); billed per token",
    ),
    entry(
        "Qwen/Qwen3-VL-Embedding-2B",
        "Apache-2.0",
        LicenceClass::Green,
        "page embedding, studio",
    ),
    entry(
        "Qwen/Qwen3-VL-Embedding-8B",
        "Apache-2.0",
        LicenceClass::Green,
        "page embedding, station",
    ),
    entry(
        "Qwen/Qwen3-VL-Reranker-2B",
        "Apache-2.0",
        LicenceClass::Green,
        "page reranker, studio",
    ),
    entry(
        "Qwen/Qwen3-VL-Reranker-8B",
        "Apache-2.0",
        LicenceClass::Green,
        "page reranker, station",
    ),
    entry(
        "mistral-ocr-latest",
        "hosted service, provider terms",
        LicenceClass::Yellow,
        "cloud OCR",
    ),
    // No tier selects a 🔴 model; this row is here so the gate's refusal is a
    // tested behaviour, not a branch nobody has run. If a future preset names
    // it, setup stops before writing anything.
    entry(
        "jinaai/jina-embeddings-v3",
        "CC-BY-NC-4.0",
        LicenceClass::Red,
        "blocked example, never selected",
    ),
];

/// The model a licence entry names, or `None` when the table does not cover it.
#[must_use]
pub fn licence_entry(model: &str) -> Option<&'static LicenceEntry> {
    LICENCES.iter().find(|entry| entry.model == model)
}

/// The models a tier can load, from `docs/kb-tiers.md` "What each tier runs".
/// Cloud also gets `mistral-ocr-latest` when the operator supplies its key.
#[must_use]
pub fn tier_models(tier: Tier) -> Vec<&'static str> {
    match tier {
        Tier::Lite => vec![
            "intfloat/multilingual-e5-small",
            "rantai-kb/ocr-cpu",
            "Qwen/Qwen3-VL-2B-Instruct",
        ],
        Tier::LiteEn => vec![
            "sentence-transformers/all-MiniLM-L6-v2",
            "rantai-kb/ocr-cpu",
            "Qwen/Qwen3-VL-2B-Instruct",
        ],
        Tier::Desk => vec![
            "Qwen/Qwen3-Embedding-0.6B",
            "BAAI/bge-reranker-v2-m3",
            "xiaomi-research/Xiaomi-OCR-0",
            "Qwen/Qwen3-VL-2B-Instruct",
        ],
        Tier::Studio => vec![
            "Qwen/Qwen3-Embedding-0.6B",
            "Qwen/Qwen3-VL-Reranker-2B",
            "xiaomi-research/Xiaomi-OCR-0",
            "Qwen/Qwen3-VL-Embedding-2B",
            "Qwen/Qwen3-VL-2B-Instruct",
        ],
        // MinerU2.5-Pro and PaddleOCR-VL stay in the licence table as
        // alternatives; every GPU tier's preset runs Xiaomi-OCR-0, the best
        // measured local OCR (docs/eval-ab/ocr-tiers).
        Tier::Station => vec![
            "Qwen/Qwen3-Embedding-0.6B",
            "Qwen/Qwen3-VL-Reranker-8B",
            "xiaomi-research/Xiaomi-OCR-0",
            "Qwen/Qwen3-VL-Embedding-8B",
            "Qwen/Qwen3-VL-2B-Instruct",
        ],
        Tier::Cloud => vec![
            "voyageai/voyage-multimodal-3.5",
            "voyageai/rerank-2.5",
            "nvidia/nemotron-3.5-super-vl",
        ],
    }
}

/// The result of the licence gate: what may run, and what needs a flag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LicenceReport {
    pub green: Vec<&'static LicenceEntry>,
    pub yellow_accepted: Vec<&'static LicenceEntry>,
}

/// Check every model a tier would load.
///
/// A 🔴 model is always refused. A 🟡 model needs `--accept-licence <model>`.
/// A model with no table entry is refused too: an unreviewed licence is not a
/// green one.
///
/// # Errors
///
/// Fails with every refusal in one message, so the operator can fix them at
/// once.
pub fn gate_licences(
    models: &[&'static str],
    accepted: &[String],
) -> Result<LicenceReport, String> {
    let mut report = LicenceReport {
        green: Vec::new(),
        yellow_accepted: Vec::new(),
    };
    let mut red = Vec::new();
    let mut missing = Vec::new();
    let mut unaccepted = Vec::new();
    for model in models {
        let Some(entry) = licence_entry(model) else {
            missing.push(*model);
            continue;
        };
        match entry.class {
            LicenceClass::Green => report.green.push(entry),
            LicenceClass::Yellow => {
                if accepted.iter().any(|value| value == model) {
                    report.yellow_accepted.push(entry);
                } else {
                    unaccepted.push(*model);
                }
            }
            LicenceClass::Red => red.push(*model),
        }
    }

    if red.is_empty() && missing.is_empty() && unaccepted.is_empty() {
        return Ok(report);
    }
    let mut lines = Vec::new();
    for model in red {
        lines.push(format!(
            "🔴 {model} is non-commercial or blocked for us (docs/model-licences.md); \
             it cannot be selected"
        ));
    }
    for model in missing {
        lines.push(format!(
            "{model} has no entry in docs/model-licences.md; add the verdict before using it"
        ));
    }
    for model in unaccepted {
        lines.push(format!(
            "🟡 {model} is commercial-use-with-conditions; accept it with \
             --accept-licence {model}"
        ));
    }
    Err(lines.join("\n"))
}

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

/// Where scanned pages are OCRed (`--ocr`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OcrChoice {
    /// The tier's own OCR container (the default).
    Local,
    /// Mistral OCR (hosted, paid per page): no local OCR container.
    Mistral,
    /// No OCR: a page without a text layer is reported as unread.
    Off,
    /// An OpenAI-compatible OCR server elsewhere, e.g. a shared Desk box.
    Url(String),
}

/// Where pictures are read (`--vision`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VisionChoice {
    Local,
    Off,
    Url(String),
}

/// Parsed `setup` arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SetupArgs {
    pub tier: Option<Tier>,
    /// Prefer `lite-en` when no tier is given.
    pub english: bool,
    pub force: bool,
    /// 🟡 models the operator accepted, by exact model id.
    pub accepted: Vec<String>,
    pub openrouter_key_env: Option<String>,
    pub mistral_key_env: Option<String>,
    /// `--ocr`; `None` is the tier's default.
    pub ocr: Option<OcrChoice>,
    /// `--ocr-model` for `--ocr <url>`.
    pub ocr_model: Option<String>,
    /// `--ocr-recipe` for `--ocr <url>` (`xiaomi` or `paddle`).
    pub ocr_recipe: Option<String>,
    /// `--ocr-key-env` for `--ocr <url>`.
    pub ocr_key_env: Option<String>,
    /// `--ocr-tables accurate`: the layout table pass on a GPU tier.
    pub ocr_tables_accurate: bool,
    /// `--embedders one|two` (`DEP-6`): one embedder for text and pages, or
    /// the two-model stack. `None` keeps whatever the env file already says.
    pub embedders: Option<crate::tier::Embedders>,
    /// `--vision`; `None` is the tier's default.
    pub vision: Option<VisionChoice>,
    /// `--vision-model` for `--vision <url>`.
    pub vision_model: Option<String>,
    /// `--set KEY=VALUE`, in order.
    pub sets: Vec<(String, String)>,
    pub chat_llm_url: Option<String>,
    pub chat_llm_model: Option<String>,
    pub port: Option<u16>,
    pub cache_dir: Option<PathBuf>,
    pub out: Option<PathBuf>,
    pub overwrite: bool,
    pub write_secrets: bool,
    pub detect_only: bool,
    pub check: bool,
    pub check_paid: bool,
    pub start: bool,
    pub help: bool,
}

impl SetupArgs {
    /// Parse `setup` flags. Every flag needs a value except the switches.
    ///
    /// # Errors
    ///
    /// Fails on an unknown flag, a missing value, or an invalid tier/port.
    pub fn parse(args: &[String]) -> Result<Self, CliError> {
        let mut parsed = Self::default();
        // The tier may also be the first argument (`setup desk`), which is
        // resolved after the flags: giving it twice is a mistake, not a
        // silent winner.
        let mut positional_tier: Option<Tier> = None;
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--tier" => {
                    let value = value_of(args, &mut index, "--tier")?;
                    parsed.tier = Some(Tier::parse(&value).ok_or_else(|| {
                        CliError::Usage(format!(
                            "--tier must be one of {}, got {value:?}",
                            TIER_NAMES.join(", ")
                        ))
                    })?);
                }
                "--english" => {
                    parsed.english = true;
                    index += 1;
                }
                "--force" => {
                    parsed.force = true;
                    index += 1;
                }
                "--accept-licence" => {
                    let value = value_of(args, &mut index, "--accept-licence")?;
                    parsed.accepted.push(value);
                }
                "--openrouter-key-env" => {
                    parsed.openrouter_key_env =
                        Some(value_of(args, &mut index, "--openrouter-key-env")?);
                }
                "--mistral-key-env" => {
                    parsed.mistral_key_env = Some(value_of(args, &mut index, "--mistral-key-env")?);
                }
                "--ocr" => {
                    let value = value_of(args, &mut index, "--ocr")?;
                    parsed.ocr = Some(match value.as_str() {
                        "local" => OcrChoice::Local,
                        "mistral" => OcrChoice::Mistral,
                        "off" | "none" => OcrChoice::Off,
                        url if url.starts_with("http://") || url.starts_with("https://") => {
                            OcrChoice::Url(url.to_string())
                        }
                        other => {
                            return Err(CliError::Usage(format!(
                                "--ocr must be local, mistral, off or an http(s) URL, got {other:?}"
                            )));
                        }
                    });
                }
                "--ocr-model" => {
                    parsed.ocr_model = Some(value_of(args, &mut index, "--ocr-model")?);
                }
                "--ocr-recipe" => {
                    let value = value_of(args, &mut index, "--ocr-recipe")?;
                    if value != "xiaomi" && value != "paddle" {
                        return Err(CliError::Usage(format!(
                            "--ocr-recipe must be xiaomi or paddle, got {value:?}"
                        )));
                    }
                    parsed.ocr_recipe = Some(value);
                }
                "--ocr-key-env" => {
                    parsed.ocr_key_env = Some(value_of(args, &mut index, "--ocr-key-env")?);
                }
                "--ocr-tables" => {
                    let value = value_of(args, &mut index, "--ocr-tables")?;
                    parsed.ocr_tables_accurate = match value.as_str() {
                        "accurate" => true,
                        "fast" => false,
                        other => {
                            return Err(CliError::Usage(format!(
                                "--ocr-tables must be fast or accurate, got {other:?}"
                            )));
                        }
                    };
                }
                "--embedders" => {
                    let value = value_of(args, &mut index, "--embedders")?;
                    parsed.embedders = Some(match value.as_str() {
                        "one" => crate::tier::Embedders::One,
                        "two" => crate::tier::Embedders::Two,
                        other => {
                            return Err(CliError::Usage(format!(
                                "--embedders must be one or two, got {other:?}"
                            )));
                        }
                    });
                }
                "--vision" => {
                    let value = value_of(args, &mut index, "--vision")?;
                    parsed.vision = Some(match value.as_str() {
                        "local" => VisionChoice::Local,
                        "off" | "none" => VisionChoice::Off,
                        url if url.starts_with("http://") || url.starts_with("https://") => {
                            VisionChoice::Url(url.to_string())
                        }
                        other => {
                            return Err(CliError::Usage(format!(
                                "--vision must be local, off or an http(s) URL, got {other:?}"
                            )));
                        }
                    });
                }
                "--vision-model" => {
                    parsed.vision_model = Some(value_of(args, &mut index, "--vision-model")?);
                }
                "--set" => {
                    let value = value_of(args, &mut index, "--set")?;
                    let Some((key, val)) = value.split_once('=') else {
                        return Err(CliError::Usage(format!(
                            "--set needs KEY=VALUE, got {value:?}"
                        )));
                    };
                    let key = key.trim();
                    if key.is_empty()
                        || !key
                            .chars()
                            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                    {
                        return Err(CliError::Usage(format!(
                            "--set key must be an UPPER_CASE environment name, got {key:?}"
                        )));
                    }
                    parsed.sets.push((key.to_string(), val.to_string()));
                }
                "--chat-llm-url" => {
                    parsed.chat_llm_url = Some(value_of(args, &mut index, "--chat-llm-url")?);
                }
                "--chat-llm-model" => {
                    parsed.chat_llm_model = Some(value_of(args, &mut index, "--chat-llm-model")?);
                }
                "--port" => {
                    let value = value_of(args, &mut index, "--port")?;
                    parsed.port = Some(value.parse().map_err(|_| {
                        CliError::Usage(format!("--port must be a number, got {value:?}"))
                    })?);
                }
                "--cache-dir" => {
                    parsed.cache_dir =
                        Some(PathBuf::from(value_of(args, &mut index, "--cache-dir")?));
                }
                "--out" => {
                    parsed.out = Some(PathBuf::from(value_of(args, &mut index, "--out")?));
                }
                "--overwrite" => {
                    parsed.overwrite = true;
                    index += 1;
                }
                "--write-secrets" => {
                    parsed.write_secrets = true;
                    index += 1;
                }
                "--detect-only" => {
                    parsed.detect_only = true;
                    index += 1;
                }
                "--check" => {
                    parsed.check = true;
                    index += 1;
                }
                "--check-paid" => {
                    parsed.check_paid = true;
                    index += 1;
                }
                "--start" => {
                    parsed.start = true;
                    index += 1;
                }
                "-h" | "--help" => {
                    parsed.help = true;
                    index += 1;
                }
                other => {
                    // `setup <tier>` is the short form of `setup --tier <tier>`:
                    // the first argument that names a tier is the tier.
                    if !other.starts_with('-') {
                        if let Some(tier) = Tier::parse(other) {
                            if positional_tier.is_some() {
                                return Err(CliError::Usage(format!(
                                    "the tier is given twice: {other:?}"
                                )));
                            }
                            positional_tier = Some(tier);
                            index += 1;
                            continue;
                        }
                    }
                    return Err(CliError::Usage(format!(
                        "unknown setup argument {other:?}; see `{} setup --help`",
                        crate::cli::program_name()
                    )));
                }
            }
        }
        let remote_ocr = matches!(parsed.ocr, Some(OcrChoice::Url(_)));
        if !remote_ocr
            && (parsed.ocr_model.is_some()
                || parsed.ocr_recipe.is_some()
                || parsed.ocr_key_env.is_some())
        {
            return Err(CliError::Usage(
                "--ocr-model, --ocr-recipe and --ocr-key-env go with --ocr <url>".to_string(),
            ));
        }
        if parsed.vision_model.is_some() && !matches!(parsed.vision, Some(VisionChoice::Url(_))) {
            return Err(CliError::Usage(
                "--vision-model goes with --vision <url>".to_string(),
            ));
        }
        if parsed.chat_llm_url.is_some() != parsed.chat_llm_model.is_some() {
            return Err(CliError::Usage(
                "--chat-llm-url and --chat-llm-model go together".to_string(),
            ));
        }
        if parsed.tier.is_some() && positional_tier.is_some() {
            return Err(CliError::Usage(
                "the tier is given twice (as an argument and with --tier)".to_string(),
            ));
        }
        if parsed.tier.is_none() {
            parsed.tier = positional_tier;
        }
        Ok(parsed)
    }
}

fn value_of(args: &[String], index: &mut usize, flag: &str) -> Result<String, CliError> {
    let value = args
        .get(*index + 1)
        .ok_or_else(|| CliError::Usage(format!("{flag} needs a value")))?;
    if value.starts_with("--") || value.trim().is_empty() {
        return Err(CliError::Usage(format!("{flag} needs a value")));
    }
    *index += 2;
    Ok(value.clone())
}

/// The `setup --help` text. `{prog}` is the name the operator invoked.
pub const HELP: &str = "\
{prog} setup — detect the hardware, check the budget and licences, write the tier config

USAGE
    {prog} setup [tier] [flags]

WHAT IT DOES
    1. Detects NVIDIA (nvidia-smi), Apple silicon (sysctl), CPU cores and RAM.
    2. Suggests a tier and prints why. `cloud` is never suggested: it spends money.
    3. Compares the tier's VRAM estimate (docs/kb-tiers.md) to the hardware and
       refuses a tier that does not fit unless --force.
    4. Refuses a 🔴 model, and needs --accept-licence <model> for each 🟡 one.
    5. Writes the deploy env file (from .env.example) and the tier file next to
       it. The tier preset fills the model roles, so no preset value is
       duplicated into the env file.
    6. Prints `{prog} up <tier>`, or runs it with --start.

FLAGS
    --tier <lite|lite-en|desk|studio|station|cloud>   Use this tier, not the suggestion.
                                                       The tier may also be given as the first
                                                       argument: `{prog} setup desk`.
    --english                     Suggest lite-en instead of lite on a CPU-only box
    --force                       Write a tier the budget or GPU check refused
    --accept-licence <model>      Accept a 🟡 model (repeatable; exact model id)
    --openrouter-key-env <NAME>   Read the OpenRouter key from $NAME (cloud)
    --mistral-key-env <NAME>      Read the Mistral OCR key from $NAME (implies --ocr mistral on cloud)
MODEL ROLES (each defaults to the tier's own local model)
    --ocr <local|mistral|off|URL> Where scanned pages are read. `mistral` sends them to
                                  Mistral OCR (hosted, billed; needs --accept-licence
                                  mistral-ocr-latest and the key in $MISTRAL_API_KEY or
                                  --mistral-key-env); a URL is an OCR server elsewhere,
                                  e.g. a Desk box shared by Lite machines. No local OCR
                                  container runs unless `local`.
    --ocr-model <NAME>            Model name at --ocr <URL> (default xiaomi-research/Xiaomi-OCR-0)
    --ocr-recipe <xiaomi|paddle>  Prompt/postprocessing for --ocr <URL> (default xiaomi)
    --ocr-key-env <NAME>          Read the --ocr <URL> server's key from $NAME
    --ocr-tables <fast|accurate>  accurate adds the layout table pass (GPU tiers; ~+10
                                  points on tables, slower; detector on the CPU)
    --embedders <one|two>         One embedder for text and pages (DEP-6) or two; one is a re-index
    --vision <local|off|URL>      Where pictures are read (ING-5); off keeps them undescribed
    --vision-model <NAME>         Model name at --vision <URL>
    --set KEY=VALUE               Write any setting into .env (repeatable), e.g.
                                  --set KB_OCR_DPI=150 --set KB_PORT=9000

OTHER FLAGS
    --chat-llm-url <URL>          OpenAI-compatible chat endpoint for page captions
    --chat-llm-model <NAME>       Model name at that endpoint (needs --chat-llm-url)
    --port <n>                    KB_PORT for the stack (default 8080)
    --cache-dir <path>            KB_CACHE_DIR (default /app/cache in compose)
    --out <path>                  Where to write the env file (default the deploy directory's .env)
    --overwrite                   Replace an existing env file
    --write-secrets               Write key values into .env instead of an empty placeholder
    --detect-only                 Print detection and the plan; write nothing
    --check                       Probe the configured endpoints and stores; exit non-zero on failure
    --check-paid                  Also probe paid hosts (openrouter.ai, api.mistral.ai, voyage)
    --start                       Start the stack afterwards (`{prog} up <tier>`)

EXAMPLES
    {prog} setup lite                                 # everything on this CPU box
    {prog} setup lite --ocr mistral --accept-licence mistral-ocr-latest
                                                      # scans to Mistral, the rest local
    {prog} setup lite --ocr http://desk-box:8000/v1/chat/completions
                                                      # scans to a shared Xiaomi server
    {prog} setup desk --ocr-tables accurate           # best tables, slower OCR
    {prog} setup cloud --ocr mistral --accept-licence mistral-ocr-latest
                                                      # no local models at all
";

/// The `setup --help` text, with the invoked program's name.
#[must_use]
pub fn help_text() -> String {
    HELP.replace("{prog}", &crate::cli::program_name())
}

// ---------------------------------------------------------------------------
// Env file: reading, merging, diffing
// ---------------------------------------------------------------------------

/// One key setup wants in the env file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvChange {
    pub key: String,
    pub value: String,
    /// A comment written above the key when it is appended.
    pub comment: Option<String>,
    /// Never print the value (only set/unset/empty).
    pub secret: bool,
}

impl EnvChange {
    fn plain(key: &str, value: impl Into<String>) -> Self {
        Self {
            key: key.to_string(),
            value: value.into(),
            comment: None,
            secret: false,
        }
    }

    fn with_comment(mut self, comment: impl Into<String>) -> Self {
        self.comment = Some(comment.into());
        self
    }

    fn secret(key: &str, value: impl Into<String>) -> Self {
        Self {
            key: key.to_string(),
            value: value.into(),
            comment: None,
            secret: true,
        }
    }
}

/// Parse a `KEY=value` env file. Comments and blank lines are skipped; a
/// matching pair of surrounding quotes is removed. Process environment is
/// layered on top by the caller, as compose does.
#[must_use]
pub fn parse_env(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .or_else(|| {
                value
                    .strip_prefix('\'')
                    .and_then(|rest| rest.strip_suffix('\''))
            })
            .unwrap_or(value);
        map.insert(key.to_string(), value.to_string());
    }
    map
}

/// Merge `changes` into `base` (the example or the existing file), replacing a
/// key in place when it is already there and appending the rest under one
/// section header. Comments above replaced lines are left alone; an appended
/// key gets its own comment.
#[must_use]
pub fn render_env(base: &str, changes: &[EnvChange], header: &str) -> String {
    let mut lines: Vec<String> = base.lines().map(str::to_string).collect();
    let mut appended = Vec::new();
    for change in changes {
        let existing = lines
            .iter()
            .position(|line| line_key(line).is_some_and(|key| key == change.key));
        match existing {
            Some(index) => lines[index] = format!("{}={}", change.key, change.value),
            None => appended.push(change),
        }
    }
    if !appended.is_empty() {
        if lines.last().is_some_and(|line| !line.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.push(format!("# ─── {header} ───"));
        for change in appended {
            if let Some(comment) = &change.comment {
                lines.push(format!("# {comment}"));
            }
            lines.push(format!("{}={}", change.key, change.value));
        }
    }
    let mut rendered = lines.join("\n");
    rendered.push('\n');
    rendered
}

fn line_key(line: &str) -> Option<&str> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    line.split_once('=').map(|(key, _)| key.trim())
}

/// How a value is shown in the diff: secrets never print their contents.
fn display_value(value: Option<&str>, secret: bool) -> String {
    match value {
        None => "(unset)".to_string(),
        Some(value) if secret => {
            if value.is_empty() {
                "(empty)".to_string()
            } else {
                "(set)".to_string()
            }
        }
        Some("") => "(empty)".to_string(),
        Some(value) => value.to_string(),
    }
}

/// The lines of the "file exists, not overwriting" summary.
fn diff_lines(existing: &BTreeMap<String, String>, changes: &[EnvChange]) -> Vec<String> {
    let mut lines = Vec::new();
    for change in changes {
        let old = existing.get(&change.key).map(String::as_str);
        let new = Some(change.value.as_str());
        if old != new.map(str::trim) {
            lines.push(format!(
                "  {}: {} -> {}",
                change.key,
                display_value(old, change.secret),
                display_value(new, change.secret)
            ));
        }
    }
    lines
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// The deploy directory: the one `up`, `down`, `logs` and `key` read.
///
/// `$RANTAI_KB_HOME/deploy` when that is set, else `rust/deploy` or `deploy`
/// next to the working directory (a source checkout, today's behaviour), else
/// `~/.rantai-kb/deploy` (an installed bundle). The container never runs
/// `setup`, so only the host matters.
#[must_use]
pub fn deploy_dir() -> PathBuf {
    crate::stack::resolve_deploy_dir(
        &|key: &str| std::env::var(key).ok(),
        &std::env::current_dir().unwrap_or_default(),
    )
    .path
}

/// The deploy directory an env file belongs to.
fn deploy_dir_of(env_path: &Path) -> PathBuf {
    match env_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// The version of an installed bundle: the `VERSION` file the release tarball
/// carries, else this crate's own version.
fn bundle_version(deploy_dir: &Path) -> String {
    if let Ok(text) = std::fs::read_to_string(deploy_dir.join("VERSION")) {
        let version = text.trim().trim_start_matches('v');
        if !version.is_empty() {
            return version.to_string();
        }
    }
    env!("CARGO_PKG_VERSION").to_string()
}

/// The published images an installed bundle pulls.
///
/// A source checkout builds the image from source; an installed bundle has no
/// source to build from, so the env file points compose at the published
/// images and compose pulls them.
fn installed_image_changes(deploy_dir: &Path) -> Vec<EnvChange> {
    let version = bundle_version(deploy_dir);
    vec![
        EnvChange::plain(
            "KB_IMAGE",
            format!("ghcr.io/rantai-dev/rantai-kb:{version}"),
        )
        .with_comment(
            "Published image: this install has no source to build from, so compose pulls it.",
        ),
        EnvChange::plain(
            "KB_OCR_CPU_IMAGE",
            format!("ghcr.io/rantai-dev/rantai-kb-ocr-cpu:{version}"),
        )
        .with_comment("Published CPU OCR sidecar image."),
        EnvChange::plain(
            "KB_OCR_LAYOUT_IMAGE",
            format!("ghcr.io/rantai-dev/rantai-kb-ocr-layout:{version}"),
        )
        .with_comment("Published layout sidecar image."),
    ]
}

fn env_example_path(env_path: &Path) -> PathBuf {
    env_path.with_file_name(".env.example")
}

fn tier_path(env_path: &Path) -> PathBuf {
    env_path.with_file_name(".tier")
}

// ---------------------------------------------------------------------------
// Planning and writing
// ---------------------------------------------------------------------------

/// The process environment first, then the env file — compose's precedence.
fn env_and_file_lookup(env_path: &Path) -> impl Fn(&str) -> Option<String> + use<> {
    let file = std::fs::read_to_string(env_path)
        .map(|text| parse_env(&text))
        .unwrap_or_default();
    move |key: &str| std::env::var(key).ok().or_else(|| file.get(key).cloned())
}

/// Whether a tier has the page-image arm, and therefore a caption role.
/// `docs/kb-tiers.md`: yes on Studio, Station and Cloud; off on Lite and Desk.
#[must_use]
pub fn has_page_arm(tier: Tier) -> bool {
    matches!(tier, Tier::Studio | Tier::Station | Tier::Cloud)
}

/// Where this setup sends scanned pages: `--ocr`, else Mistral when cloud got
/// `--mistral-key-env` (the flag that predates `--ocr`), else the tier default
/// (local on every tier but cloud, which has no OCR container).
#[must_use]
pub fn effective_ocr(args: &SetupArgs, tier: Tier) -> OcrChoice {
    match &args.ocr {
        Some(choice) => choice.clone(),
        None if tier == Tier::Cloud && args.mistral_key_env.is_some() => OcrChoice::Mistral,
        None if tier == Tier::Cloud => OcrChoice::Off,
        None => OcrChoice::Local,
    }
}

/// Where pictures are read: `--vision`, else the tier default.
#[must_use]
pub fn effective_vision(args: &SetupArgs, tier: Tier) -> VisionChoice {
    match &args.vision {
        Some(choice) => choice.clone(),
        // Cloud reads pictures through its hosted preset; nothing local runs.
        None if tier == Tier::Cloud => VisionChoice::Off,
        None => VisionChoice::Local,
    }
}

/// Refuse role choices the tier cannot run.
///
/// # Errors
///
/// `--ocr local` / `--vision local` on cloud (no model containers there), and
/// `--ocr-tables accurate` anywhere but a GPU tier reading with its own OCR.
pub fn check_roles(args: &SetupArgs, tier: Tier) -> Result<(), CliError> {
    if tier == Tier::Cloud {
        if args.ocr == Some(OcrChoice::Local) {
            return Err(CliError::Usage(
                "cloud runs no OCR container: use --ocr mistral, --ocr <url> or --ocr off"
                    .to_string(),
            ));
        }
        if args.vision == Some(VisionChoice::Local) {
            return Err(CliError::Usage(
                "cloud runs no picture model: use --vision <url> or --vision off".to_string(),
            ));
        }
    }
    if args.ocr_tables_accurate {
        let gpu = matches!(tier, Tier::Desk | Tier::Studio | Tier::Station);
        if !gpu || effective_ocr(args, tier) != OcrChoice::Local {
            return Err(CliError::Usage(
                "--ocr-tables accurate is the Xiaomi-OCR-0 table pass: it needs a GPU tier \
                 (desk, studio, station) with --ocr local"
                    .to_string(),
            ));
        }
    }
    Ok(())
}

/// The compose profiles this setup turns on: the tier, plus each model role it
/// runs locally (`<tier>-ocr`, `<tier>-vision`, `<tier>-ocr-layout`). Written
/// to `.env` as `COMPOSE_PROFILES`, which `docker compose` reads by itself.
#[must_use]
pub fn compose_profiles(args: &SetupArgs, tier: Tier) -> String {
    let name = tier.as_str();
    let mut profiles = vec![name.to_string()];
    if tier != Tier::Cloud {
        if effective_ocr(args, tier) == OcrChoice::Local {
            profiles.push(format!("{name}-ocr"));
            if args.ocr_tables_accurate {
                profiles.push(format!("{name}-ocr-layout"));
            }
        }
        if effective_vision(args, tier) == VisionChoice::Local {
            profiles.push(format!("{name}-vision"));
        }
        // One embedder or two (`DEP-6`): the model services live behind
        // `<tier>-one` / `<tier>-two` so exactly one stack starts. An unset
        // flag keeps the stack the env file already runs, defaulting to two.
        let one = match args.embedders {
            Some(crate::tier::Embedders::One) => true,
            Some(crate::tier::Embedders::Two) => false,
            None => false,
        };
        profiles.push(format!("{name}-{}", if one { "one" } else { "two" }));
    }
    profiles.join(",")
}

/// A key read from `$NAME`: the value when `--write-secrets`, an empty
/// placeholder otherwise, and a comment that never contains the value.
fn secret_from_env(
    args: &SetupArgs,
    lookup: &dyn Fn(&str) -> Option<String>,
    key: &str,
    name: &str,
    what: &str,
) -> EnvChange {
    let value = lookup(name).unwrap_or_default();
    let comment = if value.trim().is_empty() {
        format!("{what}: ${name} was not set; put the key here.")
    } else if args.write_secrets {
        format!("{what}: from ${name}; never printed by setup.")
    } else {
        format!("{what}: put the key from ${name} here (or re-run setup with --write-secrets).")
    };
    let value = if args.write_secrets {
        value
    } else {
        String::new()
    };
    EnvChange::secret(key, value).with_comment(comment)
}

/// The keys setup writes for a tier.
///
/// Only operator-level settings: `KB_TIER` itself is the preset, so writing
/// `KB_EMBEDDING_MODEL` here would duplicate what the preset already fills.
fn env_changes(
    args: &SetupArgs,
    tier: Tier,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Vec<EnvChange> {
    let mut changes = vec![EnvChange::plain("KB_TIER", tier.as_str()).with_comment(
        "Deployment tier (docs/kb-tiers.md). The preset fills the model roles; an \
         explicit KB_* setting still wins.",
    )];
    if let Some(embedders) = args.embedders {
        if embedders == crate::tier::Embedders::One {
            changes.push(EnvChange::plain("KB_EMBEDDERS", "one").with_comment(
                "One embedder for text and pages (DEP-6): Nemotron-3.5-Embed-1B in a \
                     single 2048-d space, with Qwen3-Reranker-0.6B. Adopting it is a \
                     re-index; adopting or leaving it needs no other change.",
            ));
            // Inside compose the model services are named, not loopback.
            changes.push(EnvChange::plain(
                "KB_EMBEDDING_BASE_URL",
                "http://vllm-embed:8000/v2/embed",
            ));
            changes.push(EnvChange::plain(
                "KB_PAGE_EMBEDDING_BASE_URL",
                "http://vllm-embed:8000/v2/embed",
            ));
            changes.push(EnvChange::plain("KB_RERANK_PROVIDER", "cohere"));
            changes.push(EnvChange::plain(
                "KB_RERANK_BASE_URL",
                "http://llama-rerank:8082/v1/rerank",
            ));
            changes.push(EnvChange::plain("KB_RERANK_API_KEY", "local"));
        } else {
            changes.push(EnvChange::plain("KB_EMBEDDERS", "two").with_comment(
                "Two embedders: Qwen3-Embedding-0.6B for text, Qwen3-VL-Embedding-* \
                 for pages (the 0.3/0.4 default).",
            ));
        }
    }
    if let Some(port) = args.port {
        changes.push(EnvChange::plain("KB_PORT", port.to_string()));
    }
    if let Some(dir) = &args.cache_dir {
        changes.push(EnvChange::plain("KB_CACHE_DIR", dir.display().to_string()));
    }

    if tier == Tier::Cloud {
        // The key is read from $NAME and never echoed. Without --write-secrets
        // the file gets an empty placeholder plus the instruction to fill it.
        let name = args
            .openrouter_key_env
            .as_deref()
            .unwrap_or("OPENROUTER_API_KEY");
        let value = lookup(name).unwrap_or_default();
        let comment = if value.trim().is_empty() {
            format!(
                "Cloud embeds and reranks at OpenRouter. Put the key from ${name} here \
                 (or re-run setup with --write-secrets)."
            )
        } else {
            format!("From ${name}; never printed by setup.")
        };
        let value = if args.write_secrets {
            value
        } else {
            String::new()
        };
        changes.push(EnvChange::secret("OPENROUTER_API_KEY", value).with_comment(comment));
    }

    match effective_ocr(args, tier) {
        OcrChoice::Local => {}
        OcrChoice::Mistral => {
            let name = args.mistral_key_env.as_deref().unwrap_or("MISTRAL_API_KEY");
            changes.push(EnvChange::plain("KB_OCR_BASE_URL", "").with_comment(
                "--ocr mistral: no local OCR server; scanned pages go to Mistral OCR \
                     (hosted, billed per page).",
            ));
            changes.push(secret_from_env(
                args,
                lookup,
                "KB_MISTRAL_OCR_KEY",
                name,
                "Mistral OCR key",
            ));
            changes.push(EnvChange::plain("KB_LAYOUT_EXTRACTOR_ORDER", "mistral"));
        }
        OcrChoice::Off => {
            if tier != Tier::Cloud || args.ocr.is_some() {
                changes
                    .push(EnvChange::plain("KB_OCR_BASE_URL", "").with_comment(
                        "--ocr off: pages without a text layer are reported unread.",
                    ));
                changes.push(EnvChange::plain("KB_OCR_MODE", "off"));
            }
        }
        OcrChoice::Url(url) => {
            let recipe = args.ocr_recipe.as_deref().unwrap_or("xiaomi");
            let model = args.ocr_model.clone().unwrap_or_else(|| {
                if recipe == "xiaomi" {
                    "xiaomi-research/Xiaomi-OCR-0".to_string()
                } else {
                    "rantai-kb/ocr-cpu".to_string()
                }
            });
            changes.push(EnvChange::plain("KB_OCR_BASE_URL", url).with_comment(
                "--ocr <url>: an OCR server elsewhere (e.g. a shared Desk box); no local \
                 OCR container runs here.",
            ));
            changes.push(EnvChange::plain("KB_OCR_MODEL", model));
            changes.push(EnvChange::plain("KB_OCR_RECIPE", recipe));
            if let Some(name) = &args.ocr_key_env {
                changes.push(secret_from_env(
                    args,
                    lookup,
                    "KB_OCR_API_KEY",
                    name,
                    "OCR server key",
                ));
            }
        }
    }
    if args.ocr_tables_accurate {
        changes.push(
            EnvChange::plain("KB_OCR_LAYOUT_URL", "http://ocr-layout:8193").with_comment(
                "--ocr-tables accurate: re-read a page region by region when the layout \
                 detector sees a table the whole-page answer missed (detector on the CPU).",
            ),
        );
    }
    match effective_vision(args, tier) {
        VisionChoice::Local => {}
        VisionChoice::Off => {
            if tier != Tier::Cloud || args.vision.is_some() {
                // An empty URL would fall back to OpenRouter (paid): turn the
                // describer off by its model instead.
                changes.push(
                    EnvChange::plain("KB_IMAGE_DESCRIBE_MODEL", "")
                        .with_comment("--vision off: pictures are kept but not described."),
                );
            }
        }
        VisionChoice::Url(url) => {
            changes.push(EnvChange::plain("KB_IMAGE_DESCRIBE_BASE_URL", url));
            if let Some(model) = &args.vision_model {
                changes.push(EnvChange::plain("KB_IMAGE_DESCRIBE_MODEL", model.clone()));
            }
        }
    }
    changes.push(
        EnvChange::plain("COMPOSE_PROFILES", compose_profiles(args, tier)).with_comment(
            "Which containers `rantai-kb up` starts: the tier and the model roles that \
             run here (rewritten by setup).",
        ),
    );
    for (key, value) in &args.sets {
        let change = if crate::tier::is_secret_name(key) {
            EnvChange::secret(key, value.clone())
        } else {
            EnvChange::plain(key, value.clone())
        };
        changes.push(change.with_comment("--set"));
    }

    if let (Some(url), Some(model)) = (&args.chat_llm_url, &args.chat_llm_model) {
        if has_page_arm(tier) {
            changes.push(
                EnvChange::plain("KB_PAGE_CAPTION_MODEL", model.clone()).with_comment(
                    "Describe charts/figures with the app's chat LLM's vision. The page \
                     arm must be on (KB_PAGE_EMBEDDING_MODEL); captions are opt-in.",
                ),
            );
            changes.push(EnvChange::plain("KB_PAGE_CAPTION_BASE_URL", url.clone()));
        }
    }

    changes
}

/// Print detection, the suggestion, the budget and the licences; write the
/// files unless `--detect-only`.
///
/// Split from [`run`] so tests can pass a canned [`Hardware`], a lookup and a
/// `Vec<u8>` sink: the real command must not be the only way to exercise it.
///
/// # Errors
///
/// Fails on a refused tier, a refused licence, an existing file without
/// `--overwrite`, or an I/O error.
pub fn plan_and_write(
    args: &SetupArgs,
    hardware: &Hardware,
    tier: Tier,
    lookup: &dyn Fn(&str) -> Option<String>,
    env_path: &Path,
    out: &mut dyn Write,
) -> Result<(), CliError> {
    check_roles(args, tier)?;
    write_hardware(out, hardware, args.english)?;
    let suggestion = suggest_tier(hardware, args.english);
    writeln!(
        out,
        "Tier\n  {} — {}",
        tier.as_str(),
        if args.tier == Some(tier) {
            "chosen with --tier".to_string()
        } else {
            format!("suggested: {}", suggestion.reason)
        }
    )
    .map_err(io_failed)?;
    if args.tier == Some(tier) && args.tier != Some(suggestion.tier) {
        writeln!(
            out,
            "  note: the hardware suggests {} ({})",
            suggestion.tier.as_str(),
            suggestion.reason
        )
        .map_err(io_failed)?;
    }
    if (args.chat_llm_url.is_some() || args.chat_llm_model.is_some()) && !has_page_arm(tier) {
        writeln!(
            out,
            "  note: --chat-llm-url/--chat-llm-model ignored: {} has no page-image arm \
             (Studio, Station and Cloud do), and captions describe rendered pages",
            tier.as_str()
        )
        .map_err(io_failed)?;
    }
    writeln!(out).map_err(io_failed)?;

    write_budget(out, hardware, tier, args.force, args.detect_only)?;
    let models = models_for(args, tier, lookup);
    write_licences(out, &models, args)?;
    writeln!(out).map_err(io_failed)?;

    let mut changes = env_changes(args, tier, lookup);
    // An installed bundle has no source to build the image from, so its env
    // file points compose at the published images.
    let deploy_dir = deploy_dir_of(env_path);
    if !crate::stack::is_source_checkout(&deploy_dir) {
        changes.extend(installed_image_changes(&deploy_dir));
    }

    if args.detect_only {
        writeln!(out, "Nothing written (--detect-only).").map_err(io_failed)?;
        write_next_step(out, tier, false)?;
        return Ok(());
    }

    let existing_text = std::fs::read_to_string(env_path).ok();
    if let Some(text) = &existing_text {
        if !args.overwrite {
            let existing = parse_env(text);
            writeln!(
                out,
                "{} already exists; not overwriting (use --overwrite to rewrite it).",
                env_path.display()
            )
            .map_err(io_failed)?;
            let diff = diff_lines(&existing, &changes);
            if diff.is_empty() {
                writeln!(out, "  no key would change").map_err(io_failed)?;
            } else {
                writeln!(out, "  keys that would change:").map_err(io_failed)?;
                for line in diff {
                    writeln!(out, "{line}").map_err(io_failed)?;
                }
            }
            return Err(CliError::Failed(format!(
                "{} exists; re-run with --overwrite to replace it",
                env_path.display()
            )));
        }
    }

    let base = match &existing_text {
        // Preserve the operator's other settings and comments on --overwrite;
        // the keys setup owns are replaced in place.
        Some(text) => text.clone(),
        None => std::fs::read_to_string(env_example_path(env_path)).unwrap_or_default(),
    };
    let header = format!("kb-server setup ({} tier)", tier.as_str());
    let rendered = render_env(&base, &changes, &header);
    if let Some(parent) = env_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|err| {
                CliError::Failed(format!("could not create {}: {err}", parent.display()))
            })?;
        }
    }
    std::fs::write(env_path, rendered).map_err(|err| {
        CliError::Failed(format!("could not write {}: {err}", env_path.display()))
    })?;
    writeln!(out, "Wrote {}", env_path.display()).map_err(io_failed)?;

    let tier_file = tier_path(env_path);
    std::fs::write(&tier_file, format!("{}\n", tier.as_str())).map_err(|err| {
        CliError::Failed(format!("could not write {}: {err}", tier_file.display()))
    })?;
    writeln!(out, "Wrote {}", tier_file.display()).map_err(io_failed)?;

    if tier == Tier::Cloud {
        let openrouter = changes
            .iter()
            .find(|change| change.key == "OPENROUTER_API_KEY")
            .is_some_and(|change| change.value.is_empty());
        if openrouter {
            writeln!(
                out,
                "OPENROUTER_API_KEY is empty in the file; cloud cannot start until it is filled."
            )
            .map_err(io_failed)?;
        }
    }

    write_next_step(out, tier, args.start)?;
    Ok(())
}

fn io_failed(err: std::io::Error) -> CliError {
    CliError::Failed(format!("could not write output: {err}"))
}

fn models_for(
    args: &SetupArgs,
    tier: Tier,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Vec<&'static str> {
    let _ = lookup;
    let mut models = tier_models(tier);
    let ocr = effective_ocr(args, tier);
    if ocr != OcrChoice::Local {
        // The tier's own OCR model does not run.
        let own = ["rantai-kb/ocr-cpu", "xiaomi-research/Xiaomi-OCR-0"];
        models.retain(|m| !own.contains(m));
    }
    if ocr == OcrChoice::Mistral {
        // Customer pages leave the machine and are billed: a 🟡 opt-in.
        models.push("mistral-ocr-latest");
    }
    if args.ocr_tables_accurate {
        models.push("PaddlePaddle/PP-DocLayoutV3");
    }
    if effective_vision(args, tier) != VisionChoice::Local {
        models.retain(|m| *m != "Qwen/Qwen3-VL-2B-Instruct");
    }
    models
}

fn write_hardware(out: &mut dyn Write, hardware: &Hardware, english: bool) -> Result<(), CliError> {
    writeln!(out, "kb-server setup — hardware").map_err(io_failed)?;
    writeln!(out).map_err(io_failed)?;
    writeln!(out, "  OS        {}", hardware.os).map_err(io_failed)?;
    writeln!(
        out,
        "  CPU       {}, {} core(s)",
        hardware.cpu.as_deref().unwrap_or("unknown"),
        hardware.cores
    )
    .map_err(io_failed)?;
    writeln!(
        out,
        "  Memory    {:.1} GB{}",
        hardware.ram_gb,
        if hardware.is_apple() {
            " (unified)"
        } else {
            ""
        }
    )
    .map_err(io_failed)?;
    if hardware.gpus.is_empty() {
        writeln!(out, "  GPU       none detected").map_err(io_failed)?;
    } else {
        for gpu in &hardware.gpus {
            let kind = match gpu.vendor {
                GpuVendor::Nvidia => "NVIDIA",
                GpuVendor::Apple => "Apple silicon",
                GpuVendor::Other => "unsupported",
            };
            writeln!(
                out,
                "  GPU       {kind} {} — {:.1} GB",
                gpu.name, gpu.memory_gb
            )
            .map_err(io_failed)?;
        }
    }
    for note in &hardware.notes {
        writeln!(out, "  note      {note}").map_err(io_failed)?;
    }
    if english {
        writeln!(
            out,
            "  note      --english: a CPU-only suggestion prefers lite-en"
        )
        .map_err(io_failed)?;
    }
    writeln!(out).map_err(io_failed)?;
    Ok(())
}

fn write_budget(
    out: &mut dyn Write,
    hardware: &Hardware,
    tier: Tier,
    force: bool,
    detect_only: bool,
) -> Result<(), CliError> {
    writeln!(
        out,
        "VRAM budget (ESTIMATES from docs/kb-tiers.md, not measured on this hardware)"
    )
    .map_err(io_failed)?;
    writeln!(
        out,
        "  {:<8} {:>8} {:>8} {:>8} {:>10}",
        "tier", "chat", "KB", "total", "available"
    )
    .map_err(io_failed)?;
    let available = hardware.usable_vram_gb();
    for name in TIER_NAMES {
        let Some(candidate) = Tier::parse(name) else {
            continue;
        };
        let budget = tier_budget(candidate);
        let marker = if candidate == tier { "*" } else { " " };
        match budget {
            Some(budget) => writeln!(
                out,
                "{marker} {:<8} {:>7.1}G {:>7.1}G {:>7.1}G {:>9}",
                candidate.as_str(),
                budget.chat_gb,
                budget.kb_gb,
                budget.total_gb(),
                available.map_or_else(|| "none".to_string(), |gb| format!("{gb:.1}G"))
            ),
            None => writeln!(
                out,
                "{marker} {:<8} {:>8} {:>8} {:>8} {:>9}",
                candidate.as_str(),
                "—",
                "—",
                "—",
                available.map_or_else(|| "none".to_string(), |gb| format!("{gb:.1}G"))
            ),
        }
        .map_err(io_failed)?;
    }
    writeln!(out).map_err(io_failed)?;

    match check_tier_fit(hardware, tier, force) {
        Ok(warnings) => {
            for warning in warnings {
                writeln!(out, "  warning   {warning}").map_err(io_failed)?;
            }
            if let Some(budget) = tier_budget(tier) {
                let fits = check_budget(hardware, tier).fits;
                writeln!(
                    out,
                    "  {tier}: estimated {:.1} GB needed, {} — {}",
                    budget.total_gb(),
                    available.map_or_else(|| "no GPU".to_string(), |gb| format!("{gb:.1} GB")),
                    if fits {
                        "fits"
                    } else {
                        "does not fit (forced)"
                    },
                    tier = tier.as_str()
                )
                .map_err(io_failed)?;
            } else {
                writeln!(out, "  {}: no local model holds VRAM", tier.as_str())
                    .map_err(io_failed)?;
            }
        }
        Err(message) => {
            if detect_only {
                writeln!(out, "  would refuse (without --force): {message}").map_err(io_failed)?;
            } else {
                return Err(CliError::Failed(message));
            }
        }
    }
    writeln!(out).map_err(io_failed)?;
    Ok(())
}

fn write_licences(
    out: &mut dyn Write,
    models: &[&'static str],
    args: &SetupArgs,
) -> Result<(), CliError> {
    writeln!(out, "Licences (docs/model-licences.md)").map_err(io_failed)?;
    match gate_licences(models, &args.accepted) {
        Ok(report) => {
            for entry in &report.green {
                writeln!(
                    out,
                    "  {} {:<45} {}",
                    entry.class.marker(),
                    entry.model,
                    entry.licence
                )
                .map_err(io_failed)?;
            }
            for entry in &report.yellow_accepted {
                writeln!(
                    out,
                    "  {} {:<45} {} (accepted)",
                    entry.class.marker(),
                    entry.model,
                    entry.licence
                )
                .map_err(io_failed)?;
            }
        }
        Err(message) => {
            for line in message.lines() {
                writeln!(out, "  {line}").map_err(io_failed)?;
            }
            if !args.detect_only {
                return Err(CliError::Failed(
                    "the tier's models are not licensed for this deployment".to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn write_next_step(out: &mut dyn Write, tier: Tier, started: bool) -> Result<(), CliError> {
    let program = crate::cli::program_name();
    writeln!(out).map_err(io_failed)?;
    writeln!(out, "Next step:").map_err(io_failed)?;
    writeln!(
        out,
        "  {program} up {}{}",
        tier.as_str(),
        if started { "   (running now)" } else { "" }
    )
    .map_err(io_failed)?;
    writeln!(
        out,
        "  (the containers download their models on first start; setup does not)"
    )
    .map_err(io_failed)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The command
// ---------------------------------------------------------------------------

/// `kb-server setup [flags]`.
///
/// # Errors
///
/// Returns the plan's refusal or the check's failures; `--start` propagates the
/// script's exit code through [`CliError::Exit`].
pub async fn run(args: &[String]) -> Result<(), CliError> {
    let parsed = SetupArgs::parse(args)?;
    if parsed.help {
        print!("{}", help_text());
        return Ok(());
    }
    let hardware = detect(&SystemRunner);
    let env_path = parsed
        .out
        .clone()
        .unwrap_or_else(|| deploy_dir().join(".env"));

    if parsed.check {
        let lookup = env_and_file_lookup(&env_path);
        let tier = match parsed.tier {
            Some(tier) => Some(tier),
            None => Tier::from_lookup(&lookup).map_err(|err| CliError::Failed(err.to_string()))?,
        };
        let resolved = crate::tier::with_preset(tier, &lookup);
        return check(&parsed, tier, &resolved).await;
    }

    let tier = parsed
        .tier
        .unwrap_or_else(|| suggest_tier(&hardware, parsed.english).tier);
    let lookup = env_and_file_lookup(&env_path);
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    plan_and_write(&parsed, &hardware, tier, &lookup, &env_path, &mut out)?;
    drop(out);

    if parsed.start && !parsed.detect_only {
        return start(tier).await;
    }
    Ok(())
}

/// Start the stack after `setup` wrote the files.
///
/// The Docker logic lives in `stack.rs` — a Rust port of `kb-dev.sh` — so an
/// installed bundle (which has no script to run) starts the same stack.
async fn start(tier: Tier) -> Result<(), CliError> {
    crate::stack::up(&[tier.as_str().to_string()]).await
}

// ---------------------------------------------------------------------------
// --check: one real request per configured role
// ---------------------------------------------------------------------------

/// How a configured endpoint is probed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeKind {
    /// An OpenAI-compatible embedding request (text `ping`).
    Embedding {
        model: String,
        dim: u32,
        key: Option<String>,
        /// vLLM's Chat Embeddings shape (`messages`, one text part).
        chat_shape: bool,
    },
    /// A rerank request with two documents.
    Rerank {
        model: String,
        key: Option<String>,
        shape: RerankShape,
    },
    /// One chat completion (`ping`).
    Chat { model: String, key: Option<String> },
    /// `GET {base}/v1/models`: a real request that proves the server and key.
    Models { key: Option<String> },
    /// A TCP connection.
    Tcp,
    /// An HTTP GET where any answer means the server is reachable.
    HttpGet,
}

/// The wire shape a reranker speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RerankShape {
    /// TEI `/rerank`: `{query, texts, truncate}`.
    Tei,
    /// Cohere `/v2/rerank` and vLLM's `/rerank`: `{model, query, documents}`.
    Cohere,
}

/// One endpoint or store to probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    pub role: String,
    pub url: String,
    pub kind: ProbeKind,
    /// Paid hosts are skipped unless `--check-paid`: a check must not spend.
    pub paid: bool,
}

/// Whether a URL is a paid host. Mirrors the list in the work brief.
#[must_use]
pub fn is_paid_host(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    [
        "openrouter.ai",
        "api.mistral.ai",
        "voyageai.com",
        "api.cohere.com",
    ]
    .iter()
    .any(|host| lower.contains(host))
}

fn lookup_bool(lookup: &dyn Fn(&str) -> Option<String>, name: &str, default: bool) -> bool {
    match lookup(name) {
        None => default,
        Some(value) if value.trim().is_empty() => default,
        Some(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        ),
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

/// Build the probe list from the resolved configuration (tier preset applied).
#[must_use]
pub fn probes_from_config(lookup: &dyn Fn(&str) -> Option<String>) -> Vec<Probe> {
    let mut probes = Vec::new();

    // Embedding: the one role every tier has.
    let base = lookup("KB_EMBEDDING_BASE_URL")
        .unwrap_or_else(|| "https://openrouter.ai/api/v1/embeddings".to_string());
    let model =
        lookup("KB_EMBEDDING_MODEL").unwrap_or_else(|| "qwen/qwen3-embedding-8b".to_string());
    let dim = lookup("KB_EMBEDDING_DIM")
        .and_then(|value| value.trim().parse::<u32>().ok())
        .unwrap_or(4096);
    let key = non_empty(lookup("KB_EMBEDDING_API_KEY")).or_else(|| {
        is_paid_host(&base)
            .then(|| non_empty(lookup("OPENROUTER_API_KEY")))
            .flatten()
    });
    probes.push(Probe {
        role: "embedding".to_string(),
        paid: is_paid_host(&base),
        url: base,
        kind: ProbeKind::Embedding {
            model,
            dim,
            key,
            chat_shape: false,
        },
    });

    // Reranker, when the tier or the operator turned it on.
    if lookup_bool(lookup, "KB_RERANK_ENABLED", false) {
        let provider = lookup("KB_RERANK_PROVIDER")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let model = lookup("KB_RERANK_MODEL").unwrap_or_default();
        let base = non_empty(lookup("KB_RERANK_BASE_URL"));
        let key = non_empty(lookup("KB_RERANK_API_KEY"))
            .or_else(|| non_empty(lookup("COHERE_API_KEY")))
            .or_else(|| {
                base.as_deref()
                    .is_some_and(|url| url.contains("openrouter.ai"))
                    .then(|| non_empty(lookup("OPENROUTER_API_KEY")))
                    .flatten()
            });
        match provider.as_str() {
            "tei" => {
                let base = base.unwrap_or_else(|| "http://tei-rerank:80".to_string());
                probes.push(Probe {
                    role: "rerank (tei)".to_string(),
                    paid: is_paid_host(&base),
                    url: format!("{}/rerank", base.trim_end_matches('/')),
                    kind: ProbeKind::Rerank {
                        model: if model.is_empty() {
                            "BAAI/bge-reranker-v2-m3".to_string()
                        } else {
                            model
                        },
                        key: None,
                        shape: RerankShape::Tei,
                    },
                });
            }
            "vllm" => {
                let base = base.unwrap_or_else(|| "http://localhost:8200".to_string());
                probes.push(Probe {
                    role: "rerank (vllm)".to_string(),
                    paid: is_paid_host(&base),
                    url: format!("{}/rerank", base.trim_end_matches('/')),
                    kind: ProbeKind::Rerank {
                        model: if model.is_empty() {
                            "nvidia/llama-nemotron-rerank-1b-v2".to_string()
                        } else {
                            model
                        },
                        key: None,
                        shape: RerankShape::Cohere,
                    },
                });
            }
            "cohere" => {
                let base = base.unwrap_or_else(|| "https://api.cohere.com/v2/rerank".to_string());
                probes.push(Probe {
                    role: "rerank (cohere)".to_string(),
                    paid: is_paid_host(&base),
                    url: base,
                    kind: ProbeKind::Rerank {
                        model: if model.is_empty() {
                            "rerank-v3.5".to_string()
                        } else {
                            model
                        },
                        key,
                        shape: RerankShape::Cohere,
                    },
                });
            }
            _ => {
                // LLM-as-reranker (OpenRouter). Paid, and skipped by default.
                probes.push(Probe {
                    role: "rerank (llm)".to_string(),
                    paid: true,
                    url: kb_inference::OPENROUTER_CHAT_URL.to_string(),
                    kind: ProbeKind::Chat {
                        model: if model.is_empty() {
                            "openai/gpt-4.1-nano".to_string()
                        } else {
                            model
                        },
                        key: non_empty(lookup("OPENROUTER_API_KEY")),
                    },
                });
            }
        }
    }

    // Page embedder, when the arm is on.
    if let Some(model) = non_empty(lookup("KB_PAGE_EMBEDDING_MODEL")) {
        let base = lookup("KB_PAGE_EMBEDDING_BASE_URL")
            .unwrap_or_else(|| "https://openrouter.ai/api/v1/embeddings".to_string());
        let chat_shape = lookup("KB_PAGE_EMBEDDING_FORMAT")
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("vllm"));
        let key = non_empty(lookup("KB_PAGE_EMBEDDING_API_KEY")).or_else(|| {
            is_paid_host(&base)
                .then(|| non_empty(lookup("OPENROUTER_API_KEY")))
                .flatten()
        });
        probes.push(Probe {
            role: "page embedding".to_string(),
            paid: is_paid_host(&base),
            url: base,
            kind: ProbeKind::Embedding {
                model,
                dim: lookup("KB_PAGE_EMBEDDING_DIM")
                    .and_then(|value| value.trim().parse::<u32>().ok())
                    .unwrap_or(1024),
                key,
                chat_shape,
            },
        });
    }

    // Caption role, when configured.
    if let Some(model) = non_empty(lookup("KB_PAGE_CAPTION_MODEL")) {
        let url = non_empty(lookup("KB_PAGE_CAPTION_BASE_URL"))
            .unwrap_or_else(|| kb_inference::DEFAULT_PAGE_CAPTION_BASE_URL.to_string());
        let key = non_empty(lookup("KB_PAGE_CAPTION_API_KEY")).or_else(|| {
            is_paid_host(&url)
                .then(|| non_empty(lookup("OPENROUTER_API_KEY")))
                .flatten()
        });
        probes.push(Probe {
            role: "page caption".to_string(),
            paid: is_paid_host(&url),
            url,
            kind: ProbeKind::Chat { model, key },
        });
    }

    // Local OCR (`KB_OCR_*`). `lookup` already carries the tier preset, so this
    // sees the same URL `kb_extract::ExtractConfig` resolves.
    if let Some(base) = non_empty(lookup("KB_OCR_BASE_URL")) {
        probes.push(Probe {
            role: "ocr (local)".to_string(),
            paid: is_paid_host(&base),
            url: models_endpoint(&base),
            kind: ProbeKind::Models {
                key: non_empty(lookup("KB_OCR_API_KEY")),
            },
        });
    }
    if let Some(key) = non_empty(lookup("KB_MISTRAL_OCR_KEY")) {
        let base = non_empty(lookup("KB_MISTRAL_OCR_BASE"))
            .unwrap_or_else(|| "https://api.mistral.ai".to_string());
        probes.push(Probe {
            role: "ocr (mistral)".to_string(),
            paid: is_paid_host(&base),
            url: models_endpoint(&base),
            kind: ProbeKind::Models { key: Some(key) },
        });
    }

    // Stores, only when configured.
    if let Some(database_url) = non_empty(lookup("DATABASE_URL")) {
        if let Some(address) = host_port(&database_url, 5432) {
            probes.push(Probe {
                role: "postgres".to_string(),
                paid: false,
                url: address,
                kind: ProbeKind::Tcp,
            });
        }
    }
    if let Some(url) = non_empty(lookup("QDRANT_URL")) {
        probes.push(Probe {
            role: "qdrant".to_string(),
            paid: false,
            url: format!("{}/healthz", url.trim_end_matches('/')),
            kind: ProbeKind::HttpGet,
        });
    } else if let Some(url) = non_empty(lookup("QDRANT_GRPC_URL")) {
        if let Some(address) = host_port(&url, 6334) {
            probes.push(Probe {
                role: "qdrant".to_string(),
                paid: false,
                url: address,
                kind: ProbeKind::Tcp,
            });
        }
    }
    if let Some(url) = non_empty(lookup("S3_ENDPOINT")) {
        probes.push(Probe {
            role: "blob (S3)".to_string(),
            paid: false,
            url,
            kind: ProbeKind::HttpGet,
        });
    }

    probes
}

/// Turn an OpenAI-compatible base URL into its `/v1/models` endpoint.
fn models_endpoint(base: &str) -> String {
    let base = base.trim_end_matches('/');
    for suffix in ["/v1/chat/completions", "/v1/ocr", "/v1/models"] {
        if let Some(root) = base.strip_suffix(suffix) {
            return format!("{root}/v1/models");
        }
    }
    format!("{base}/v1/models")
}

/// `host:port` from a URL or DSN, for a TCP probe and for the redacted display.
#[must_use]
pub fn host_port(url: &str, default_port: u16) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    if authority.is_empty() {
        return None;
    }
    if let Some(after_bracket) = authority.strip_prefix('[') {
        let (host, tail) = after_bracket.split_once(']')?;
        let port = tail
            .strip_prefix(':')
            .and_then(|port| port.parse::<u16>().ok())
            .unwrap_or(default_port);
        return Some(format!("[{host}]:{port}"));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => {
            let port = port.parse::<u16>().unwrap_or(default_port);
            Some(format!("{host}:{port}"))
        }
        _ => Some(format!("{authority}:{default_port}")),
    }
}

/// The host part of a URL, for the check table. No credentials, no query.
#[must_use]
pub fn display_host(url: &str) -> String {
    host_port(url, 0)
        .map(|address| address.trim_end_matches(":0").to_string())
        .unwrap_or_else(|| url.to_string())
}

/// Print one row per probe and return a failure when a required probe failed.
///
/// A paid host without `allow_paid` is reported as skipped, not failed: a
/// health check must never spend money by itself.
///
/// # Errors
///
/// Fails when any non-skipped probe failed.
pub async fn run_probes(
    probes: &[Probe],
    allow_paid: bool,
    out: &mut dyn Write,
) -> Result<(), CliError> {
    writeln!(
        out,
        "  {:<16} {:<34} {:<10} {:>8}",
        "role", "host", "status", "latency"
    )
    .map_err(io_failed)?;
    let mut failures = 0usize;
    let mut skipped = 0usize;
    for probe in probes {
        if probe.paid && !allow_paid {
            skipped += 1;
            writeln!(
                out,
                "  {:<16} {:<34} {:<10} {:>8}",
                probe.role,
                display_host(&probe.url),
                "skipped",
                "—"
            )
            .map_err(io_failed)?;
            continue;
        }
        let started = Instant::now();
        let result = probe_one(probe).await;
        let elapsed = started.elapsed().as_millis();
        match result {
            Ok(detail) => {
                writeln!(
                    out,
                    "  {:<16} {:<34} {:<10} {:>6} ms   {}",
                    probe.role,
                    display_host(&probe.url),
                    "ok",
                    elapsed,
                    detail
                )
                .map_err(io_failed)?;
            }
            Err(err) => {
                failures += 1;
                writeln!(
                    out,
                    "  {:<16} {:<34} {:<10} {:>6} ms   {}",
                    probe.role,
                    display_host(&probe.url),
                    "FAIL",
                    elapsed,
                    err
                )
                .map_err(io_failed)?;
            }
        }
    }
    if skipped > 0 {
        writeln!(
            out,
            "  {skipped} paid host(s) skipped; pass --check-paid to probe them (this spends money)."
        )
        .map_err(io_failed)?;
    }
    if failures > 0 {
        return Err(CliError::Failed(format!(
            "{failures} required probe(s) failed"
        )));
    }
    Ok(())
}

async fn probe_one(probe: &Probe) -> Result<String, String> {
    match &probe.kind {
        ProbeKind::Embedding {
            model,
            dim,
            key,
            chat_shape,
        } => probe_embedding(&probe.url, model, *dim, key.as_deref(), *chat_shape).await,
        ProbeKind::Rerank { model, key, shape } => {
            probe_rerank(&probe.url, model, key.as_deref(), *shape).await
        }
        ProbeKind::Chat { model, key } => probe_chat(&probe.url, model, key.as_deref()).await,
        ProbeKind::Models { key } => probe_models(&probe.url, key.as_deref()).await,
        ProbeKind::Tcp => probe_tcp(&probe.url).await,
        ProbeKind::HttpGet => probe_http_get(&probe.url).await,
    }
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|err| format!("HTTP client: {err}"))
}

async fn probe_embedding(
    url: &str,
    model: &str,
    dim: u32,
    key: Option<&str>,
    chat_shape: bool,
) -> Result<String, String> {
    let body = if chat_shape {
        serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": [{"type": "text", "text": "ping"}]}]
        })
    } else {
        serde_json::json!({"model": model, "input": ["ping"]})
    };
    let mut request = http_client()?.post(url).json(&body);
    if let Some(key) = key {
        request = request.bearer_auth(key);
    }
    let response = request
        .send()
        .await
        .map_err(|err| format!("request failed: {err}"))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|err| format!("body failed: {err}"))?;
    if !status.is_success() {
        return Err(format!("HTTP {status}: {}", excerpt(&bytes)));
    }
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|err| format!("not JSON: {err}"))?;
    let vector = value
        .pointer("/data/0/embedding")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("no data[0].embedding in {}", excerpt(&bytes)))?;
    let returned = vector.len();
    if dim > 0 && returned != dim as usize {
        return Err(format!(
            "returned {returned} dims but KB_*_DIM is {dim}; the index width would not match"
        ));
    }
    Ok(format!("{returned} dims"))
}

async fn probe_rerank(
    url: &str,
    model: &str,
    key: Option<&str>,
    shape: RerankShape,
) -> Result<String, String> {
    let body = match shape {
        RerankShape::Tei => serde_json::json!({
            "query": "ping",
            "texts": ["a document", "another document"],
            "truncate": true
        }),
        RerankShape::Cohere => serde_json::json!({
            "model": model,
            "query": "ping",
            "documents": ["a document", "another document"],
            "top_n": 2
        }),
    };
    let mut request = http_client()?.post(url).json(&body);
    if let Some(key) = key {
        request = request.bearer_auth(key);
    }
    let response = request
        .send()
        .await
        .map_err(|err| format!("request failed: {err}"))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|err| format!("body failed: {err}"))?;
    if !status.is_success() {
        return Err(format!("HTTP {status}: {}", excerpt(&bytes)));
    }
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|err| format!("not JSON: {err}"))?;
    let rows = value.as_array().map_or_else(
        || value.get("results").or_else(|| value.get("data")),
        |_| None,
    );
    let count = match rows {
        Some(serde_json::Value::Array(rows)) => rows.len(),
        _ => 0,
    };
    if count == 0 {
        return Err(format!("no rerank results in {}", excerpt(&bytes)));
    }
    Ok(format!("{count} scores"))
}

async fn probe_chat(url: &str, model: &str, key: Option<&str>) -> Result<String, String> {
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "ping"}],
        "max_tokens": 1
    });
    let mut request = http_client()?.post(url).json(&body);
    if let Some(key) = key {
        request = request.bearer_auth(key);
    }
    let response = request
        .send()
        .await
        .map_err(|err| format!("request failed: {err}"))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|err| format!("body failed: {err}"))?;
    if !status.is_success() {
        return Err(format!("HTTP {status}: {}", excerpt(&bytes)));
    }
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|err| format!("not JSON: {err}"))?;
    let choices = value
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    if choices == 0 {
        return Err(format!("no choices in {}", excerpt(&bytes)));
    }
    Ok(format!("{choices} choice(s)"))
}

async fn probe_models(url: &str, key: Option<&str>) -> Result<String, String> {
    let mut request = http_client()?.get(url);
    if let Some(key) = key {
        request = request.bearer_auth(key);
    }
    let response = request
        .send()
        .await
        .map_err(|err| format!("request failed: {err}"))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|err| format!("body failed: {err}"))?;
    if !status.is_success() {
        return Err(format!("HTTP {status}: {}", excerpt(&bytes)));
    }
    let count = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|value| {
            value
                .get("data")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len)
        });
    Ok(match count {
        Some(count) => format!("{count} model(s)"),
        None => "reachable".to_string(),
    })
}

async fn probe_tcp(address: &str) -> Result<String, String> {
    match tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::TcpStream::connect(address),
    )
    .await
    {
        Ok(Ok(_)) => Ok("connected".to_string()),
        Ok(Err(err)) => Err(format!("connect failed: {err}")),
        Err(_) => Err("connect timed out after 5 s".to_string()),
    }
}

async fn probe_http_get(url: &str) -> Result<String, String> {
    let response = http_client()?
        .get(url)
        .send()
        .await
        .map_err(|err| format!("request failed: {err}"))?;
    Ok(format!("HTTP {}", response.status()))
}

fn excerpt(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).chars().take(200).collect()
}

/// `setup --check`: resolve the config and probe every configured endpoint.
async fn check(
    args: &SetupArgs,
    tier: Option<Tier>,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<(), CliError> {
    let probes = probes_from_config(lookup);
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "kb-server setup --check (tier {})",
        tier.map_or("none", Tier::as_str)
    )
    .map_err(io_failed)?;
    if probes.is_empty() {
        writeln!(out, "  no endpoints or stores configured").map_err(io_failed)?;
        return Ok(());
    }
    writeln!(out).map_err(io_failed)?;
    run_probes(&probes, args.check_paid, &mut out).await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ─── hardware parsing ───────────────────────────────────────────────────

    #[test]
    fn nvidia_smi_with_one_gpu_is_parsed_in_gib() {
        let gpus = parse_nvidia_smi("NVIDIA GeForce RTX 4090, 24564\n").expect("one GPU");
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].vendor, GpuVendor::Nvidia);
        assert_eq!(gpus[0].name, "NVIDIA GeForce RTX 4090");
        assert!((gpus[0].memory_gb - 23.988).abs() < 0.01, "{gpus:?}");
    }

    #[test]
    fn nvidia_smi_with_two_gpus_reports_each() {
        let gpus = parse_nvidia_smi("NVIDIA A100, 81920\nNVIDIA T4, 15360\n").expect("two GPUs");
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[1].name, "NVIDIA T4");
        assert!((gpus[1].memory_gb - 15.0).abs() < 0.001);
    }

    #[test]
    fn nvidia_smi_garbage_is_an_error() {
        assert!(parse_nvidia_smi("this is not a GPU").is_err());
        assert!(parse_nvidia_smi("NVIDIA T4, lots").is_err());
        assert!(parse_nvidia_smi("").is_err());
    }

    #[test]
    fn a_missing_nvidia_smi_is_no_gpu() {
        let runner = FakeRunner::linux(8, 11.7);
        let hardware = detect(&runner);
        assert!(!hardware.has_nvidia());
        assert!(hardware.gpus.is_empty());
        assert_eq!(suggest_tier(&hardware, false).tier, Tier::Lite);
    }

    #[test]
    fn garbage_nvidia_smi_output_becomes_a_note_not_a_panic() {
        let mut runner = FakeRunner::linux(8, 11.7);
        runner.commands.insert(
            "nvidia-smi --query-gpu=name,memory.total --format=csv,noheader,nounits".to_string(),
            "hello, world".to_string(),
        );
        let hardware = detect(&runner);
        assert!(!hardware.has_nvidia());
        assert!(
            hardware
                .notes
                .iter()
                .any(|note| note.contains("not understood")),
            "{:?}",
            hardware.notes
        );
    }

    #[test]
    fn a_virtual_display_is_not_an_accelerator() {
        let mut runner = FakeRunner::linux(8, 11.7);
        runner.commands.insert(
            "lspci".to_string(),
            "00:02.0 VGA compatible controller: Device 1234:1111 (rev 02)".to_string(),
        );
        let hardware = detect(&runner);
        assert!(hardware.gpus.is_empty(), "{:?}", hardware.gpus);
        assert!(hardware.notes.is_empty(), "{:?}", hardware.notes);
    }

    #[test]
    fn an_amd_gpu_is_reported_as_unsupported() {
        let mut runner = FakeRunner::linux(8, 11.7);
        runner.commands.insert(
            "lspci".to_string(),
            "0a:00.0 VGA compatible controller: Advanced Micro Devices, Inc. [AMD/ATI] Navi 22"
                .to_string(),
        );
        let hardware = detect(&runner);
        assert_eq!(hardware.gpus.len(), 1);
        assert_eq!(hardware.gpus[0].vendor, GpuVendor::Other);
        assert!(
            hardware
                .notes
                .iter()
                .any(|note| note.contains("treated as CPU")),
            "{:?}",
            hardware.notes
        );
        assert_eq!(suggest_tier(&hardware, false).tier, Tier::Lite);
    }

    #[test]
    fn darwin_reads_unified_memory_and_cpu_brand() {
        let runner = FakeRunner::darwin("Apple M3 Ultra", 128.0);
        let hardware = detect(&runner);
        assert!(hardware.is_apple());
        assert_eq!(hardware.cpu.as_deref(), Some("Apple M3 Ultra"));
        assert!((hardware.ram_gb - 128.0).abs() < 0.001);
        assert_eq!(suggest_tier(&hardware, false).tier, Tier::Station);
    }

    #[test]
    fn darwin_edges_are_16_32_and_128() {
        for (gb, expected) in [
            (15.9, Tier::Lite),
            (16.0, Tier::Desk),
            (31.9, Tier::Desk),
            (32.0, Tier::Studio),
            (127.9, Tier::Studio),
            (128.0, Tier::Station),
        ] {
            let hardware = detect(&FakeRunner::darwin("Apple M3", gb));
            assert_eq!(
                suggest_tier(&hardware, false).tier,
                expected,
                "{gb} GB unified"
            );
        }
    }

    // ─── tier suggestion edges ─────────────────────────────────────────────

    #[test]
    fn nvidia_suggestion_edges_are_8_24_and_48() {
        for (gb, expected) in [
            (7.9, Tier::Lite),
            (8.0, Tier::Desk),
            (23.9, Tier::Desk),
            (24.0, Tier::Studio),
            (47.9, Tier::Studio),
            (48.0, Tier::Station),
        ] {
            let hardware = nvidia_box(gb);
            assert_eq!(
                suggest_tier(&hardware, false).tier,
                expected,
                "{gb} GB VRAM"
            );
        }
    }

    #[test]
    fn cloud_is_never_suggested() {
        for hardware in [nvidia_box(80.0), FakeRunner::linux(8, 11.7).hardware()] {
            assert_ne!(suggest_tier(&hardware, false).tier, Tier::Cloud);
        }
    }

    #[test]
    fn english_switches_only_the_cpu_suggestion() {
        let cpu = FakeRunner::linux(8, 11.7).hardware();
        assert_eq!(suggest_tier(&cpu, true).tier, Tier::LiteEn);
        assert_eq!(suggest_tier(&nvidia_box(24.0), true).tier, Tier::Studio);
    }

    // ─── budget / refusal ──────────────────────────────────────────────────

    #[test]
    fn cpu_refuses_desk_without_force() {
        let cpu = FakeRunner::linux(8, 11.7).hardware();
        let err = check_tier_fit(&cpu, Tier::Desk, false).unwrap_err();
        assert!(err.contains("NVIDIA"), "{err}");
        let warnings = check_tier_fit(&cpu, Tier::Desk, true).expect("forced");
        assert!(
            warnings.iter().any(|warning| warning.contains("forced")),
            "{warnings:?}"
        );
    }

    #[test]
    fn a_small_nvidia_card_refuses_a_bigger_tier() {
        let hardware = nvidia_box(8.0);
        let err = check_tier_fit(&hardware, Tier::Studio, false).unwrap_err();
        assert!(err.contains("19.0"), "{err}");
        assert!(err.contains("8.0"), "{err}");
        assert!(check_tier_fit(&hardware, Tier::Studio, true).is_ok());
        assert!(check_tier_fit(&hardware, Tier::Desk, false).is_ok());
    }

    #[test]
    fn lite_and_cloud_never_refuse_on_vram() {
        let cpu = FakeRunner::linux(1, 2.0).hardware();
        for tier in [Tier::Lite, Tier::LiteEn, Tier::Cloud] {
            assert!(check_tier_fit(&cpu, tier, false).is_ok(), "{tier:?}");
        }
    }

    // ─── licences ──────────────────────────────────────────────────────────

    #[test]
    fn every_tier_model_has_a_licence_entry() {
        for name in TIER_NAMES {
            let tier = Tier::parse(name).expect("known tier");
            for model in tier_models(tier) {
                assert!(
                    licence_entry(model).is_some(),
                    "{name} lists {model}, which has no licence entry"
                );
            }
        }
    }

    #[test]
    fn every_model_id_named_in_tier_rs_is_in_the_licence_table() {
        // The guard the work package asks for: adding a model to a preset
        // without a licence verdict fails here.
        for name in TIER_NAMES {
            let tier = Tier::parse(name).expect("known tier");
            for (variable, value) in tier.presets() {
                if variable.ends_with("_MODEL") && !value.trim().is_empty() {
                    assert!(
                        licence_entry(value).is_some(),
                        "{name} sets {variable}={value} but there is no licence entry"
                    );
                }
            }
        }
    }

    #[test]
    fn a_red_model_is_refused_even_when_accepted() {
        // CC-BY-NC-4.0 in docs/model-licences.md; no --accept-licence can
        // override a 🔴 verdict.
        let model = "jinaai/jina-embeddings-v3";
        let accepted = vec![model.to_string()];
        let err = gate_licences(&[model], &accepted).unwrap_err();
        assert!(err.contains("🔴"), "{err}");
        assert!(err.contains("non-commercial"), "{err}");
    }

    #[test]
    fn a_model_with_no_licence_entry_is_refused() {
        let err = gate_licences(&["someone/unreviewed-model"], &[]).unwrap_err();
        assert!(err.contains("no entry"), "{err}");
    }

    #[test]
    fn a_yellow_model_needs_the_flag() {
        let models = tier_models(Tier::Cloud);
        let err = gate_licences(&models, &[]).unwrap_err();
        assert!(
            err.contains("--accept-licence voyageai/voyage-multimodal-3.5"),
            "{err}"
        );
        let accepted = vec![
            "voyageai/voyage-multimodal-3.5".to_string(),
            "voyageai/rerank-2.5".to_string(),
            "nvidia/nemotron-3.5-super-vl".to_string(),
        ];
        let report = gate_licences(&models, &accepted).expect("accepted");
        assert!(report.green.is_empty());
        assert_eq!(report.yellow_accepted.len(), 3);
    }

    #[test]
    fn green_models_pass_without_flags() {
        let models = tier_models(Tier::Studio);
        let report = gate_licences(&models, &[]).expect("all green");
        assert_eq!(report.green.len(), models.len());
    }

    #[test]
    fn the_lite_licence_list_is_green() {
        let report = gate_licences(&tier_models(Tier::Lite), &[]).expect("lite is green");
        assert!(
            report
                .green
                .iter()
                .any(|entry| entry.model == "intfloat/multilingual-e5-small")
        );
    }

    // ─── env file ──────────────────────────────────────────────────────────

    #[test]
    fn render_env_replaces_a_key_in_place_and_appends_the_rest() {
        let base = "# header\nKB_PORT=8080\nOTHER=1\n";
        let changes = vec![
            EnvChange::plain("KB_PORT", "18080"),
            EnvChange::plain("KB_TIER", "lite"),
        ];
        let rendered = render_env(base, &changes, "setup");
        assert!(rendered.contains("KB_PORT=18080"), "{rendered}");
        assert!(!rendered.contains("KB_PORT=8080"), "{rendered}");
        assert!(rendered.contains("OTHER=1"), "{rendered}");
        assert!(rendered.contains("# ─── setup ───"), "{rendered}");
        assert!(rendered.contains("KB_TIER=lite"), "{rendered}");
    }

    #[test]
    fn parse_env_handles_quotes_comments_and_blanks() {
        let map = parse_env("# c\nA=1\n\nB=\"two words\"\nC='x'\nD=\n");
        assert_eq!(map.get("A").map(String::as_str), Some("1"));
        assert_eq!(map.get("B").map(String::as_str), Some("two words"));
        assert_eq!(map.get("C").map(String::as_str), Some("x"));
        assert_eq!(map.get("D").map(String::as_str), Some(""));
        assert!(!map.contains_key("# c"));
    }

    fn args_of(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    fn written(
        args: &SetupArgs,
        tier: Tier,
        lookup: &dyn Fn(&str) -> Option<String>,
    ) -> BTreeMap<String, String> {
        let hardware = FakeRunner::linux(8, 11.7).hardware();
        written_with(&hardware, args, tier, lookup)
    }

    /// `written` against a given machine, for tiers that need an accelerator.
    fn written_with(
        hardware: &Hardware,
        args: &SetupArgs,
        tier: Tier,
        lookup: &dyn Fn(&str) -> Option<String>,
    ) -> BTreeMap<String, String> {
        let dir = TempDir::new("roles");
        let env_path = dir.path.join(".env");
        let mut output = Vec::new();
        plan_and_write(args, hardware, tier, lookup, &env_path, &mut output)
            .unwrap_or_else(|err| panic!("plan: {err}\n{}", String::from_utf8_lossy(&output)));
        parse_env(&std::fs::read_to_string(&env_path).expect("written"))
    }

    #[test]
    fn a_tier_runs_every_local_role_by_default() {
        let args = SetupArgs::default();
        assert_eq!(
            compose_profiles(&args, Tier::Lite),
            "lite,lite-ocr,lite-vision,lite-two"
        );
        assert_eq!(
            compose_profiles(&args, Tier::Desk),
            "desk,desk-ocr,desk-vision,desk-two"
        );
        assert_eq!(compose_profiles(&args, Tier::Cloud), "cloud");
        let env = written(
            &SetupArgs {
                force: true,
                ..SetupArgs::default()
            },
            Tier::Desk,
            &|_: &str| None,
        );
        assert_eq!(
            env.get("COMPOSE_PROFILES").map(String::as_str),
            Some("desk,desk-ocr,desk-vision,desk-two")
        );
        assert!(
            !env.contains_key("KB_OCR_BASE_URL"),
            "the preset fills the local OCR: {env:?}"
        );
    }

    #[test]
    fn mistral_ocr_on_a_local_tier_drops_the_ocr_container_and_needs_the_licence_opt_in() {
        let parsed = SetupArgs::parse(&args_of(&[
            "--ocr",
            "mistral",
            "--mistral-key-env",
            "MY_MISTRAL",
        ]))
        .expect("parse");
        assert_eq!(
            compose_profiles(&parsed, Tier::Lite),
            "lite,lite-vision,lite-two"
        );
        // Pages leave the machine: refused without --accept-licence.
        let dir = TempDir::new("mistral-refused");
        let mut output = Vec::new();
        let refused = plan_and_write(
            &parsed,
            &FakeRunner::linux(8, 11.7).hardware(),
            Tier::Lite,
            &|_: &str| None,
            &dir.path.join(".env"),
            &mut output,
        );
        assert!(refused.is_err(), "mistral-ocr-latest is a 🟡 opt-in");

        let accepted = SetupArgs {
            accepted: vec!["mistral-ocr-latest".to_string()],
            ..parsed
        };
        let lookup = |key: &str| (key == "MY_MISTRAL").then(|| "mistral-secret".to_string());
        let env = written(&accepted, Tier::Lite, &lookup);
        assert_eq!(
            env.get("KB_OCR_BASE_URL").map(String::as_str),
            Some(""),
            "{env:?}"
        );
        assert_eq!(
            env.get("KB_MISTRAL_OCR_KEY").map(String::as_str),
            Some(""),
            "no secret without --write-secrets"
        );
        assert_eq!(
            env.get("KB_LAYOUT_EXTRACTOR_ORDER").map(String::as_str),
            Some("mistral")
        );
        assert_eq!(
            env.get("COMPOSE_PROFILES").map(String::as_str),
            Some("lite,lite-vision,lite-two")
        );
    }

    #[test]
    fn a_shared_ocr_server_is_used_with_the_xiaomi_recipe_unless_told_otherwise() {
        let parsed = SetupArgs::parse(&args_of(&[
            "--ocr",
            "http://desk-box:8000/v1/chat/completions",
        ]))
        .expect("parse");
        let env = written(&parsed, Tier::Lite, &|_: &str| None);
        assert_eq!(
            env.get("KB_OCR_BASE_URL").map(String::as_str),
            Some("http://desk-box:8000/v1/chat/completions")
        );
        assert_eq!(env.get("KB_OCR_RECIPE").map(String::as_str), Some("xiaomi"));
        assert_eq!(
            env.get("KB_OCR_MODEL").map(String::as_str),
            Some("xiaomi-research/Xiaomi-OCR-0")
        );
        assert_eq!(
            env.get("COMPOSE_PROFILES").map(String::as_str),
            Some("lite,lite-vision,lite-two")
        );
        assert!(
            SetupArgs::parse(&args_of(&["--ocr-model", "x"])).is_err(),
            "--ocr-model needs --ocr <url>"
        );
    }

    #[test]
    fn vision_off_turns_the_describer_off_by_its_model_not_its_url() {
        // An empty KB_IMAGE_DESCRIBE_BASE_URL falls back to OpenRouter (paid).
        let parsed = SetupArgs::parse(&args_of(&["--vision", "off"])).expect("parse");
        let env = written(&parsed, Tier::Lite, &|_: &str| None);
        assert_eq!(
            env.get("KB_IMAGE_DESCRIBE_MODEL").map(String::as_str),
            Some("")
        );
        assert!(!env.contains_key("KB_IMAGE_DESCRIBE_BASE_URL"));
        assert_eq!(
            env.get("COMPOSE_PROFILES").map(String::as_str),
            Some("lite,lite-ocr,lite-two")
        );
    }

    /// `--embedders one`: `DEP-6`'s single model for text and pages, with the
    /// compose service names the tier would reach inside the stack.
    #[test]
    fn embedders_one_writes_the_endpoints_and_the_one_profile() {
        let parsed = SetupArgs::parse(&args_of(&["--embedders", "one"])).expect("parse");
        assert_eq!(
            compose_profiles(&parsed, Tier::Studio),
            "studio,studio-ocr,studio-vision,studio-one"
        );
        let env = written_with(&nvidia_box(24.0), &parsed, Tier::Studio, &|_: &str| None);
        assert_eq!(env.get("KB_EMBEDDERS").map(String::as_str), Some("one"));
        assert_eq!(
            env.get("KB_EMBEDDING_BASE_URL").map(String::as_str),
            Some("http://vllm-embed:8000/v2/embed")
        );
        assert_eq!(
            env.get("KB_PAGE_EMBEDDING_BASE_URL").map(String::as_str),
            Some("http://vllm-embed:8000/v2/embed"),
            "pages use the same endpoint as text"
        );
        assert_eq!(
            env.get("KB_RERANK_PROVIDER").map(String::as_str),
            Some("cohere")
        );
        assert_eq!(
            env.get("KB_RERANK_BASE_URL").map(String::as_str),
            Some("http://llama-rerank:8082/v1/rerank")
        );

        // Without the flag the two-model stack and its profile stay.
        let plain = SetupArgs::parse(&args_of(&[])).expect("parse");
        assert_eq!(
            compose_profiles(&plain, Tier::Studio),
            "studio,studio-ocr,studio-vision,studio-two"
        );
        assert!(
            !written_with(&nvidia_box(24.0), &plain, Tier::Studio, &|_: &str| None)
                .contains_key("KB_EMBEDDERS")
        );
    }

    #[test]
    fn accurate_tables_add_the_layout_pass_only_on_a_gpu_tier() {
        let parsed = SetupArgs::parse(&args_of(&["--ocr-tables", "accurate"])).expect("parse");
        assert!(check_roles(&parsed, Tier::Lite).is_err());
        assert!(check_roles(&parsed, Tier::Desk).is_ok());
        assert_eq!(
            compose_profiles(&parsed, Tier::Studio),
            "studio,studio-ocr,studio-ocr-layout,studio-vision,studio-two"
        );
        let env = written(
            &SetupArgs {
                force: true,
                ..parsed
            },
            Tier::Desk,
            &|_: &str| None,
        );
        assert_eq!(
            env.get("KB_OCR_LAYOUT_URL").map(String::as_str),
            Some("http://ocr-layout:8193")
        );
    }

    #[test]
    fn cloud_refuses_local_roles_and_set_writes_any_setting() {
        let local = SetupArgs::parse(&args_of(&["--ocr", "local"])).expect("parse");
        assert!(check_roles(&local, Tier::Cloud).is_err());
        assert!(SetupArgs::parse(&args_of(&["--set", "lower=1"])).is_err());
        assert!(SetupArgs::parse(&args_of(&["--set", "NOEQUALS"])).is_err());
        let parsed = SetupArgs::parse(&args_of(&[
            "--set",
            "KB_OCR_DPI=150",
            "--set",
            "KB_FOO_API_KEY=s3cret",
        ]))
        .expect("parse");
        let dir = TempDir::new("set");
        let env_path = dir.path.join(".env");
        let mut output = Vec::new();
        plan_and_write(
            &parsed,
            &FakeRunner::linux(8, 11.7).hardware(),
            Tier::Lite,
            &|_: &str| None,
            &env_path,
            &mut output,
        )
        .expect("plan");
        let env = parse_env(&std::fs::read_to_string(&env_path).expect("written"));
        assert_eq!(env.get("KB_OCR_DPI").map(String::as_str), Some("150"));
        assert_eq!(
            env.get("KB_FOO_API_KEY").map(String::as_str),
            Some("s3cret"),
            "--set writes what it is given"
        );
        let printed = String::from_utf8(output).expect("utf8");
        assert!(
            !printed.contains("s3cret"),
            "a secret --set value is never printed: {printed}"
        );
    }

    #[test]
    fn secrets_are_never_written_without_write_secrets() {
        let dir = TempDir::new("secret");
        let env_path = dir.path.join(".env");
        let mut args = SetupArgs {
            tier: Some(Tier::Cloud),
            accepted: vec![
                "voyageai/voyage-multimodal-3.5".to_string(),
                "voyageai/rerank-2.5".to_string(),
                "nvidia/nemotron-3.5-super-vl".to_string(),
            ],
            ..SetupArgs::default()
        };
        args.openrouter_key_env = Some("TEST_OPENROUTER_KEY".to_string());
        let lookup =
            |key: &str| (key == "TEST_OPENROUTER_KEY").then(|| "sk-or-very-secret".to_string());
        let mut output = Vec::new();
        plan_and_write(
            &args,
            &FakeRunner::linux(8, 11.7).hardware(),
            Tier::Cloud,
            &lookup,
            &env_path,
            &mut output,
        )
        .expect("plan");
        let text = std::fs::read_to_string(&env_path).expect("written");
        assert!(text.contains("OPENROUTER_API_KEY=\n"), "{text}");
        assert!(!text.contains("sk-or-very-secret"), "{text}");
        let printed = String::from_utf8(output).expect("utf8");
        assert!(!printed.contains("sk-or-very-secret"), "{printed}");
        assert!(printed.contains("OPENROUTER_API_KEY"), "{printed}");
    }

    #[test]
    fn write_secrets_writes_the_value_but_never_prints_it() {
        let dir = TempDir::new("secret-ok");
        let env_path = dir.path.join(".env");
        let args = SetupArgs {
            tier: Some(Tier::Cloud),
            accepted: vec![
                "voyageai/voyage-multimodal-3.5".to_string(),
                "voyageai/rerank-2.5".to_string(),
                "nvidia/nemotron-3.5-super-vl".to_string(),
            ],
            write_secrets: true,
            openrouter_key_env: Some("TEST_OPENROUTER_KEY".to_string()),
            ..SetupArgs::default()
        };
        let lookup =
            |key: &str| (key == "TEST_OPENROUTER_KEY").then(|| "sk-or-very-secret".to_string());
        let mut output = Vec::new();
        plan_and_write(
            &args,
            &FakeRunner::linux(8, 11.7).hardware(),
            Tier::Cloud,
            &lookup,
            &env_path,
            &mut output,
        )
        .expect("plan");
        let text = std::fs::read_to_string(&env_path).expect("written");
        assert!(
            text.contains("OPENROUTER_API_KEY=sk-or-very-secret"),
            "{text}"
        );
        let printed = String::from_utf8(output).expect("utf8");
        assert!(!printed.contains("sk-or-very-secret"), "{printed}");
    }

    #[test]
    fn an_existing_file_is_not_overwritten_without_the_flag() {
        let dir = TempDir::new("existing");
        let env_path = dir.path.join(".env");
        std::fs::write(&env_path, "KB_TIER=lite\nKEEP=me\n").expect("seed");
        let args = SetupArgs {
            tier: Some(Tier::Lite),
            ..SetupArgs::default()
        };
        let mut output = Vec::new();
        let err = plan_and_write(
            &args,
            &FakeRunner::linux(8, 11.7).hardware(),
            Tier::Lite,
            &|_: &str| None,
            &env_path,
            &mut output,
        )
        .unwrap_err();
        assert!(matches!(err, CliError::Failed(_)), "{err:?}");
        let text = std::fs::read_to_string(&env_path).expect("unchanged");
        assert_eq!(text, "KB_TIER=lite\nKEEP=me\n");
        let printed = String::from_utf8(output).expect("utf8");
        assert!(printed.contains("would change"), "{printed}");
    }

    #[test]
    fn overwrite_preserves_other_keys_and_replaces_ours() {
        let dir = TempDir::new("overwrite");
        let env_path = dir.path.join(".env");
        std::fs::write(&env_path, "KB_TIER=lite\nKEEP=me\n").expect("seed");
        let args = SetupArgs {
            tier: Some(Tier::Studio),
            accepted: vec![],
            overwrite: true,
            ..SetupArgs::default()
        };
        let mut output = Vec::new();
        plan_and_write(
            &args,
            &nvidia_box(24.0),
            Tier::Studio,
            &|_: &str| None,
            &env_path,
            &mut output,
        )
        .expect("plan");
        let text = std::fs::read_to_string(&env_path).expect("written");
        assert!(text.contains("KB_TIER=studio"), "{text}");
        assert!(text.contains("KEEP=me"), "{text}");
    }

    // ─── tier on the command line ───────────────────────────────────────────

    #[test]
    fn the_tier_may_be_the_first_argument() {
        // `setup desk` is the short form of `setup --tier desk`.
        let parsed = SetupArgs::parse(&["desk".to_string()]).expect("parse");
        assert_eq!(parsed.tier, Some(Tier::Desk));
    }

    #[test]
    fn a_positional_tier_and_the_flag_agree() {
        let parsed =
            SetupArgs::parse(&["cloud".to_string(), "--start".to_string()]).expect("parse");
        assert_eq!(parsed.tier, Some(Tier::Cloud));
        assert!(parsed.start);
    }

    #[test]
    fn the_tier_may_not_be_given_twice() {
        let err = SetupArgs::parse(&["lite".to_string(), "--tier".to_string(), "desk".to_string()])
            .unwrap_err();
        assert!(matches!(err, CliError::Usage(_)), "{err:?}");
    }

    #[test]
    fn an_unknown_positional_argument_is_still_a_usage_error() {
        let err = SetupArgs::parse(&["nonsense".to_string()]).unwrap_err();
        assert!(matches!(err, CliError::Usage(_)), "{err:?}");
    }

    #[test]
    fn setup_writes_the_tier_file_next_to_the_env() {
        let dir = TempDir::new("tier-file");
        let env_path = dir.path.join(".env");
        let args = SetupArgs {
            tier: Some(Tier::Lite),
            ..SetupArgs::default()
        };
        let mut output = Vec::new();
        plan_and_write(
            &args,
            &FakeRunner::linux(8, 11.7).hardware(),
            Tier::Lite,
            &|_: &str| None,
            &env_path,
            &mut output,
        )
        .expect("plan");
        let tier = std::fs::read_to_string(dir.path.join(".tier")).expect("tier file");
        assert_eq!(tier.trim(), "lite");
    }

    #[test]
    fn detect_only_reports_a_refusal_instead_of_failing() {
        let dir = TempDir::new("detect-desk");
        let env_path = dir.path.join(".env");
        let args = SetupArgs {
            tier: Some(Tier::Desk),
            detect_only: true,
            ..SetupArgs::default()
        };
        let mut output = Vec::new();
        plan_and_write(
            &args,
            &FakeRunner::linux(8, 11.7).hardware(),
            Tier::Desk,
            &|_: &str| None,
            &env_path,
            &mut output,
        )
        .expect("detect-only never refuses");
        let printed = String::from_utf8(output).expect("utf8");
        assert!(printed.contains("would refuse"), "{printed}");
        assert!(!env_path.exists());
    }

    #[test]
    fn a_forced_tier_says_it_does_not_fit() {
        let dir = TempDir::new("forced-desk");
        let env_path = dir.path.join(".env");
        let args = SetupArgs {
            tier: Some(Tier::Desk),
            force: true,
            ..SetupArgs::default()
        };
        let mut output = Vec::new();
        plan_and_write(
            &args,
            &FakeRunner::linux(8, 11.7).hardware(),
            Tier::Desk,
            &|_: &str| None,
            &env_path,
            &mut output,
        )
        .expect("forced");
        let printed = String::from_utf8(output).expect("utf8");
        assert!(printed.contains("does not fit (forced)"), "{printed}");
    }

    #[test]
    fn detect_only_writes_nothing() {
        let dir = TempDir::new("detect");
        let env_path = dir.path.join(".env");
        let args = SetupArgs {
            detect_only: true,
            ..SetupArgs::default()
        };
        let mut output = Vec::new();
        plan_and_write(
            &args,
            &FakeRunner::linux(8, 11.7).hardware(),
            Tier::Lite,
            &|_: &str| None,
            &env_path,
            &mut output,
        )
        .expect("plan");
        assert!(!env_path.exists());
        assert!(!dir.path.join(".tier").exists());
        let printed = String::from_utf8(output).expect("utf8");
        assert!(printed.contains("Nothing written"), "{printed}");
    }

    #[test]
    fn a_preset_value_is_not_duplicated_into_the_env_file() {
        let dir = TempDir::new("no-preset");
        let env_path = dir.path.join(".env");
        let args = SetupArgs {
            tier: Some(Tier::Desk),
            ..SetupArgs::default()
        };
        let mut output = Vec::new();
        plan_and_write(
            &args,
            &nvidia_box(8.0),
            Tier::Desk,
            &|_: &str| None,
            &env_path,
            &mut output,
        )
        .expect("plan");
        let text = std::fs::read_to_string(&env_path).expect("written");
        assert!(text.contains("KB_TIER=desk"), "{text}");
        for preset in [
            "KB_EMBEDDING_MODEL",
            "KB_RERANK_MODEL",
            "KB_EMBED_CONCURRENCY",
        ] {
            assert!(!text.contains(preset), "{preset} leaked into .env:\n{text}");
        }
    }

    #[test]
    fn chat_llm_flags_are_ignored_on_a_tier_without_the_page_arm() {
        let args = SetupArgs {
            tier: Some(Tier::Desk),
            chat_llm_url: Some("http://llm.local:8080/v1/chat/completions".to_string()),
            chat_llm_model: Some("qwen".to_string()),
            ..SetupArgs::default()
        };
        let changes = env_changes(&args, Tier::Desk, &|_: &str| None);
        assert!(
            !changes
                .iter()
                .any(|change| change.key.starts_with("KB_PAGE_CAPTION")),
            "{changes:?}"
        );
        // Studio has the page arm, so the same flags are written there.
        let studio = env_changes(&args, Tier::Studio, &|_: &str| None);
        assert!(
            studio
                .iter()
                .any(|change| change.key == "KB_PAGE_CAPTION_MODEL"),
            "{studio:?}"
        );
    }

    #[test]
    fn chat_llm_flags_print_that_they_are_ignored_on_a_tier_without_the_page_arm() {
        let dir = TempDir::new("caption-ignored");
        let env_path = dir.path.join(".env");
        let args = SetupArgs {
            tier: Some(Tier::Desk),
            chat_llm_url: Some("http://llm.local:8080/v1/chat/completions".to_string()),
            chat_llm_model: Some("qwen".to_string()),
            ..SetupArgs::default()
        };
        let mut output = Vec::new();
        plan_and_write(
            &args,
            &nvidia_box(8.0),
            Tier::Desk,
            &|_: &str| None,
            &env_path,
            &mut output,
        )
        .expect("plan");
        let printed = String::from_utf8(output).expect("utf8");
        assert!(printed.contains("ignored"), "{printed}");
        let text = std::fs::read_to_string(&env_path).expect("written");
        assert!(!text.contains("KB_PAGE_CAPTION"), "{text}");
    }

    #[test]
    fn chat_llm_flags_are_written_on_studio() {
        let dir = TempDir::new("caption-studio");
        let env_path = dir.path.join(".env");
        let args = SetupArgs {
            tier: Some(Tier::Studio),
            chat_llm_url: Some("http://llm.local:8080/v1/chat/completions".to_string()),
            chat_llm_model: Some("qwen".to_string()),
            ..SetupArgs::default()
        };
        let mut output = Vec::new();
        plan_and_write(
            &args,
            &nvidia_box(24.0),
            Tier::Studio,
            &|_: &str| None,
            &env_path,
            &mut output,
        )
        .expect("plan");
        let text = std::fs::read_to_string(&env_path).expect("written");
        assert!(text.contains("KB_PAGE_CAPTION_MODEL=qwen"), "{text}");
        assert!(
            text.contains("KB_PAGE_CAPTION_BASE_URL=http://llm.local:8080/v1/chat/completions"),
            "{text}"
        );
    }

    // ─── --check ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn check_passes_when_every_probe_answers() {
        let embedding = MockServer::spawn(r#"{"data":[{"embedding":[0.1,0.2,0.3],"index":0}]}"#);
        let rerank = MockServer::spawn(r#"{"results":[{"index":0,"relevance_score":0.9}]}"#);
        let probes = vec![
            Probe {
                role: "embedding".to_string(),
                url: embedding.url("/v1/embeddings"),
                kind: ProbeKind::Embedding {
                    model: "m".to_string(),
                    dim: 3,
                    key: None,
                    chat_shape: false,
                },
                paid: false,
            },
            Probe {
                role: "rerank (tei)".to_string(),
                url: rerank.url("/rerank"),
                kind: ProbeKind::Rerank {
                    model: "bge".to_string(),
                    key: None,
                    shape: RerankShape::Tei,
                },
                paid: false,
            },
        ];
        let mut output = Vec::new();
        run_probes(&probes, false, &mut output)
            .await
            .expect("all probes ok");
        let printed = String::from_utf8(output).expect("utf8");
        assert!(printed.contains("ok"), "{printed}");
    }

    #[tokio::test]
    async fn check_fails_non_zero_when_one_probe_fails() {
        let embedding = MockServer::spawn(r#"{"data":[{"embedding":[0.1,0.2,0.3],"index":0}]}"#);
        let failing = MockServer::failing(500, "boom");
        let probes = vec![
            Probe {
                role: "embedding".to_string(),
                url: embedding.url("/v1/embeddings"),
                kind: ProbeKind::Embedding {
                    model: "m".to_string(),
                    dim: 3,
                    key: None,
                    chat_shape: false,
                },
                paid: false,
            },
            Probe {
                role: "rerank (tei)".to_string(),
                url: failing.url("/rerank"),
                kind: ProbeKind::Rerank {
                    model: "bge".to_string(),
                    key: None,
                    shape: RerankShape::Tei,
                },
                paid: false,
            },
        ];
        let mut output = Vec::new();
        let err = run_probes(&probes, false, &mut output).await.unwrap_err();
        assert!(matches!(err, CliError::Failed(_)), "{err:?}");
        let printed = String::from_utf8(output).expect("utf8");
        assert!(printed.contains("FAIL"), "{printed}");
    }

    #[tokio::test]
    async fn a_paid_host_is_skipped_without_check_paid() {
        let probes = vec![Probe {
            role: "embedding".to_string(),
            // A host that cannot answer: if the probe were attempted, the
            // result would be a failure, not "skipped".
            url: "https://openrouter.ai/api/v1/embeddings".to_string(),
            kind: ProbeKind::Embedding {
                model: "voyage".to_string(),
                dim: 1024,
                key: Some("not-used".to_string()),
                chat_shape: false,
            },
            paid: true,
        }];
        let mut output = Vec::new();
        run_probes(&probes, false, &mut output)
            .await
            .expect("skipped is not a failure");
        let printed = String::from_utf8(output).expect("utf8");
        assert!(printed.contains("skipped"), "{printed}");
        assert!(printed.contains("--check-paid"), "{printed}");
    }

    #[test]
    fn paid_hosts_are_recognised() {
        assert!(is_paid_host("https://openrouter.ai/api/v1/embeddings"));
        assert!(is_paid_host("https://api.mistral.ai/v1/ocr"));
        assert!(is_paid_host("https://api.voyageai.com/v1/embeddings"));
        assert!(!is_paid_host("http://127.0.0.1:8081/v1/embeddings"));
        assert!(!is_paid_host("http://tei-lite:80/v1/embeddings"));
    }

    #[test]
    fn probes_from_config_names_every_configured_role() {
        let mut env = HashMap::new();
        env.insert(
            "KB_EMBEDDING_BASE_URL",
            "http://127.0.0.1:8081/v1/embeddings",
        );
        env.insert("KB_EMBEDDING_MODEL", "e5");
        env.insert("KB_EMBEDDING_DIM", "384");
        env.insert("KB_RERANK_ENABLED", "true");
        env.insert("KB_RERANK_PROVIDER", "tei");
        env.insert("KB_RERANK_BASE_URL", "http://127.0.0.1:8082");
        env.insert("KB_PAGE_EMBEDDING_MODEL", "qwen-vl");
        env.insert(
            "KB_PAGE_EMBEDDING_BASE_URL",
            "http://127.0.0.1:8000/v1/embeddings",
        );
        env.insert("KB_OCR_BASE_URL", "http://127.0.0.1:8100");
        env.insert("DATABASE_URL", "postgresql://kb:kb@127.0.0.1:5432/kb");
        env.insert("QDRANT_URL", "http://127.0.0.1:6333");
        env.insert("S3_ENDPOINT", "http://127.0.0.1:9000");
        let lookup = |key: &str| env.get(key).map(|value| (*value).to_string());
        let probes = probes_from_config(&lookup);
        let roles: Vec<&str> = probes.iter().map(|probe| probe.role.as_str()).collect();
        for expected in [
            "embedding",
            "rerank (tei)",
            "page embedding",
            "ocr (local)",
            "postgres",
            "qdrant",
            "blob (S3)",
        ] {
            assert!(roles.contains(&expected), "{roles:?} misses {expected}");
        }
        assert!(probes.iter().all(|probe| !probe.paid), "{probes:?}");
    }

    #[test]
    fn models_endpoint_handles_a_full_chat_url() {
        assert_eq!(
            models_endpoint("http://ocr:8100/v1/chat/completions"),
            "http://ocr:8100/v1/models"
        );
        assert_eq!(
            models_endpoint("https://api.mistral.ai"),
            "https://api.mistral.ai/v1/models"
        );
    }

    #[test]
    fn host_port_redacts_credentials_and_keeps_the_port() {
        assert_eq!(
            host_port("postgresql://kb:secret@db:5432/kb?schema=public", 5432).as_deref(),
            Some("db:5432")
        );
        assert_eq!(
            display_host("https://user:pass@example.com/x"),
            "example.com"
        );
        assert_eq!(
            display_host("http://tei-lite:80/v1/embeddings"),
            "tei-lite:80"
        );
    }

    // ─── argument parsing ──────────────────────────────────────────────────

    #[test]
    fn setup_parses_every_flag() {
        let args = SetupArgs::parse(&[
            "--tier".into(),
            "studio".into(),
            "--force".into(),
            "--accept-licence".into(),
            "a/b".into(),
            "--accept-licence".into(),
            "c/d".into(),
            "--port".into(),
            "18080".into(),
            "--cache-dir".into(),
            "/tmp/c".into(),
            "--write-secrets".into(),
            "--check-paid".into(),
        ])
        .expect("parse");
        assert_eq!(args.tier, Some(Tier::Studio));
        assert!(args.force);
        assert_eq!(args.accepted, ["a/b", "c/d"]);
        assert_eq!(args.port, Some(18080));
        assert_eq!(args.cache_dir.as_deref(), Some(Path::new("/tmp/c")));
        assert!(args.write_secrets);
        assert!(args.check_paid);
    }

    #[test]
    fn an_unknown_tier_is_a_usage_error() {
        let err = SetupArgs::parse(&["--tier".into(), "gpu".into()]).unwrap_err();
        assert!(matches!(err, CliError::Usage(_)), "{err:?}");
    }

    #[test]
    fn a_chat_url_without_a_model_is_a_usage_error() {
        let err = SetupArgs::parse(&["--chat-llm-url".into(), "http://x".into()]).unwrap_err();
        assert!(matches!(err, CliError::Usage(_)), "{err:?}");
    }

    // ─── test doubles ──────────────────────────────────────────────────────

    /// Canned command output.
    struct FakeRunner {
        commands: HashMap<String, String>,
        files: HashMap<String, String>,
        cores: usize,
    }

    impl FakeRunner {
        fn linux(cores: usize, ram_gb: f64) -> Self {
            let mut files = HashMap::new();
            files.insert(
                "/proc/meminfo".to_string(),
                format!("MemTotal: {} kB\n", (ram_gb * 1024.0 * 1024.0) as u64),
            );
            files.insert(
                "/proc/cpuinfo".to_string(),
                "model name\t: Intel(R) Xeon(R) CPU\n".to_string(),
            );
            let mut commands = HashMap::new();
            commands.insert("uname -s".to_string(), "Linux".to_string());
            Self {
                commands,
                files,
                cores,
            }
        }

        fn darwin(cpu: &str, ram_gb: f64) -> Self {
            let mut commands = HashMap::new();
            commands.insert("uname -s".to_string(), "Darwin".to_string());
            commands.insert(
                "sysctl -n hw.memsize".to_string(),
                format!("{}", (ram_gb * 1024.0 * 1024.0 * 1024.0) as u64),
            );
            commands.insert(
                "sysctl -n machdep.cpu.brand_string".to_string(),
                cpu.to_string(),
            );
            Self {
                commands,
                files: HashMap::new(),
                cores: 12,
            }
        }

        fn hardware(&self) -> Hardware {
            detect(self)
        }
    }

    impl CommandRunner for FakeRunner {
        fn stdout(&self, program: &str, args: &[&str]) -> Option<String> {
            let key = if args.is_empty() {
                program.to_string()
            } else {
                format!("{program} {}", args.join(" "))
            };
            self.commands.get(&key).cloned()
        }

        fn read_file(&self, path: &str) -> Option<String> {
            self.files.get(path).cloned()
        }

        fn cores(&self) -> usize {
            self.cores
        }
    }

    fn nvidia_box(vram_gb: f64) -> Hardware {
        Hardware {
            os: "Linux".to_string(),
            cpu: Some("test CPU".to_string()),
            cores: 16,
            ram_gb: 64.0,
            gpus: vec![Gpu {
                vendor: GpuVendor::Nvidia,
                name: "Test GPU".to_string(),
                memory_gb: vram_gb,
            }],
            notes: Vec::new(),
        }
    }

    /// A one-request HTTP server: enough for one probe, then it keeps
    /// answering the same way. Threads are detached and die with the test
    /// process.
    struct MockServer {
        address: std::net::SocketAddr,
    }

    impl MockServer {
        fn spawn(body: &'static str) -> Self {
            Self::start(200, body)
        }

        fn failing(status: u16, body: &'static str) -> Self {
            Self::start(status, body)
        }

        fn start(status: u16, body: &'static str) -> Self {
            use std::io::{Read, Write as _};
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock server");
            let address = listener.local_addr().expect("local addr");
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else {
                        continue;
                    };
                    let mut buffer = [0u8; 4096];
                    let _ = stream.read(&mut buffer);
                    let response = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
                         content-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                }
            });
            Self { address }
        }

        fn url(&self, path: &str) -> String {
            format!("http://{}{path}", self.address)
        }
    }

    /// A temp directory that removes itself; tests must never write into the
    /// checkout's `rust/deploy/.env`.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "kb-setup-{label}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}
