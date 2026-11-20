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
use crate::tier::{EmbedPrecision, TIER_NAMES, Tier};

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
    /// The CUDA compute capability, when the driver reported it. It decides
    /// whether the NVFP4 build of the embedder can run on this card.
    pub compute_cap: Option<ComputeCapability>,
}

/// A CUDA compute capability: `10.1` is the GB10 (sm_121), `12.0` the RTX
/// 50-series (sm_120), `8.9` an RTX 4090 (sm_89).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ComputeCapability {
    pub major: u32,
    pub minor: u32,
}

impl ComputeCapability {
    /// Whether the card has **native FP4** tensor cores, which the NVFP4
    /// build of the embedder needs.
    ///
    /// Blackwell only: sm_100 (B200, GB200), sm_120 (RTX 50-series, RTX PRO)
    /// and sm_121 (GB10, DGX Spark). Nothing before it does — Ada is 8.9,
    /// Hopper 9.0, Ampere 8.x — so an RTX 4090 or an H100 falls back to BF16.
    #[must_use]
    pub fn supports_fp4(self) -> bool {
        matches!(self.major, 10 | 12)
    }
}

impl std::fmt::Display for ComputeCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

impl Gpu {
    /// Whether this card can run the NVFP4 build of the embedder.
    ///
    /// The compute capability is the answer. When the driver does not report
    /// it, the card's name is the fallback, and only for names that cannot
    /// mean anything else: the DGX Spark's GB10, the B200/GB200 data-centre
    /// parts and the cards that say "Blackwell" on them.
    #[must_use]
    pub fn supports_fp4(&self) -> bool {
        if self.vendor != GpuVendor::Nvidia {
            return false;
        }
        if let Some(capability) = self.compute_cap {
            return capability.supports_fp4();
        }
        let name = self.name.to_ascii_uppercase();
        // "RTX 5050".."RTX 5090" are Blackwell; "RTX 5000 Ada" is not, so the
        // consumer numbers are listed one by one instead of "RTX 50".
        [
            "GB10",
            "GB200",
            "GB300",
            "B200",
            "B300",
            "BLACKWELL",
            "RTX 5050",
            "RTX 5060",
            "RTX 5070",
            "RTX 5080",
            "RTX 5090",
        ]
        .iter()
        .any(|needle| name.contains(needle))
    }
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

    /// Whether any NVIDIA card here has native FP4, so the NVFP4 build of the
    /// embedder can run.
    #[must_use]
    pub fn has_fp4(&self) -> bool {
        self.gpus.iter().any(Gpu::supports_fp4)
    }

    /// The card the embedder would run on: the largest NVIDIA card, like
    /// [`Hardware::usable_vram_gb`].
    #[must_use]
    pub fn largest_nvidia(&self) -> Option<&Gpu> {
        self.gpus
            .iter()
            .filter(|gpu| gpu.vendor == GpuVendor::Nvidia)
            .max_by(|a, b| a.memory_gb.total_cmp(&b.memory_gb))
    }

    /// An NVIDIA card with native FP4, if there is one.
    #[must_use]
    pub fn fp4_gpu(&self) -> Option<&Gpu> {
        self.gpus
            .iter()
            .find(|gpu| gpu.vendor == GpuVendor::Nvidia && gpu.supports_fp4())
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
            compute_cap: None,
        });
    }
    if gpus.is_empty() {
        return Err("nvidia-smi returned no GPUs".to_string());
    }
    Ok(gpus)
}

/// `nvidia-smi --query-gpu=compute_cap --format=csv,noheader,nounits`: one
/// line per GPU, the CUDA compute capability (`10.1`).
///
/// A driver that does not know the field answers `[N/A]`, and a driver too old
/// to have it fails outright; both mean "unknown", never a wrong capability.
/// The lines are in the same device order as the name/VRAM query.
#[must_use]
pub fn parse_compute_caps(output: &str) -> Vec<Option<ComputeCapability>> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (major, minor) = line.split_once('.')?;
            let major = major.trim().parse::<u32>().ok()?;
            let minor = minor.trim().parse::<u32>().ok()?;
            Some(ComputeCapability { major, minor })
        })
        .collect()
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

    // The compute capability is a second query on purpose: a driver that does
    // not know the field would otherwise cost us the VRAM reading too. It
    // decides whether the NVFP4 build of the embedder can run here.
    let caps = runner
        .stdout(
            "nvidia-smi",
            &["--query-gpu=compute_cap", "--format=csv,noheader,nounits"],
        )
        .map(|output| parse_compute_caps(&output));
    if let Some(caps) = caps {
        let nvidia = gpus
            .iter_mut()
            .filter(|gpu| gpu.vendor == GpuVendor::Nvidia)
            .count();
        if caps.len() == nvidia {
            let mut caps = caps.into_iter();
            for gpu in gpus
                .iter_mut()
                .filter(|gpu| gpu.vendor == GpuVendor::Nvidia)
            {
                gpu.compute_cap = caps.next().flatten();
            }
        } else {
            notes.push(format!(
                "nvidia-smi reported {} compute capabilities for {nvidia} GPUs; \
                 the FP4 capability is unknown",
                caps.len()
            ));
        }
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
                    compute_cap: None,
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
                    compute_cap: None,
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
// The embedder build: NVFP4 where the hardware has native FP4, BF16 elsewhere
// ---------------------------------------------------------------------------

/// The weights a build holds, in GiB.
///
/// Read from the checkpoints' `model.safetensors.index.json` (`total_size`):
/// the BF16 checkpoint is 3,109,421,056 bytes (2.90 GiB) and the NVFP4 one
/// 1,267,468,496 bytes (1.18 GiB). The 4-bit build is 2.5× smaller, which is
/// the whole point of running it on a card shared with a chat model.
const NEMOTRON_BF16_WEIGHTS_GIB: f64 = 2.90;
const NEMOTRON_NVFP4_WEIGHTS_GIB: f64 = 1.18;

/// What vLLM needs besides the weights: the CUDA context, the activations and
/// the pooler. **An estimate**, not a measurement.
const VLLM_RUNTIME_GIB: f64 = 0.4;

/// The KV cache one token of this model costs: 16 layers × 8 KV heads × 128
/// head_dim × 2 (K and V) × 2 bytes (bf16) = 65,536 bytes, from the
/// checkpoint's `config.json` (`text_config`).
const KV_BYTES_PER_TOKEN: f64 = 65_536.0;

/// One GiB in bytes.
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// The context the embedder serves: 8,192 tokens.
///
/// The checkpoint supports 32,768 (the model card's serve example uses it),
/// but a chunk or a page is far shorter, and the KV cache is what the
/// embedder's footprint is made of. 8,192 is the setting the footprint was
/// measured in (issue #37: ~13.8 GiB with vLLM's defaults, ~3.7 GiB with
/// `--max-model-len 8192` and a sized memory fraction on a GB10).
const EMBED_MAX_MODEL_LEN: u32 = 8192;

/// The tokens one page image costs: the default 1,120 px longest edge with a
/// 14 px patch and a 2×2 merge is 1,600 tokens, and a page image is the
/// biggest request the tier sends. A text chunk is a few hundred.
const EMBED_PAGE_TOKENS: f64 = 1600.0;

/// The embedder checkpoint a setup run configures, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbedderChoice {
    /// The checkpoint the embedder will run.
    pub model: String,
    /// The build, when the checkpoint is one of ours.
    pub precision: Option<EmbedPrecision>,
    /// Why this one, in a sentence, printed by setup.
    pub reason: String,
    /// True when the checkpoint is not the tier preset's (BF16): the vectors
    /// of an existing index were made by another model, so this is a re-index.
    pub differs_from_preset: bool,
}

/// What the embedder role gets in the env file: the checkpoint, the build and
/// the vLLM flags sized for the card.
#[derive(Clone, Debug, PartialEq)]
pub struct EmbedderPlan {
    /// The checkpoint and why it was chosen.
    pub choice: EmbedderChoice,
    /// The vLLM flags, when the embedder runs under vLLM (the one-embedder
    /// stack on a GPU tier).
    pub sizing: Option<EmbedderSizing>,
}

/// The embedder's vLLM flags, sized for this machine.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EmbedderSizing {
    /// `--max-model-len`.
    pub max_model_len: u32,
    /// `--max-num-seqs`: how many requests the KV pool serves at once.
    pub max_num_seqs: u32,
    /// `--gpu-memory-utilization`: the share of the whole card the embedder
    /// may hold. `None` when there is no accelerator to size it for.
    pub gpu_fraction: Option<f64>,
    /// The footprint the flags aim for, in GiB.
    pub target_gb: f64,
    /// The checkpoint's weights, in GiB.
    pub weights_gb: f64,
    /// The KV cache the flags leave for, in GiB.
    pub kv_gb: f64,
}

/// The VRAM the embedder may hold, per tier, in GiB.
///
/// It is the embedder's **share** of the tier's model budget, from the VRAM
/// tables in `docs/kb-tiers.md`: Desk's is the ~4.5 GB the card has left
/// after the chat model, less the ~1.2 GB reranker — so the 4-bit build
/// (2.1 GB) fits and the 16-bit one (3.8 GB) does not. Studio's is the
/// ~10 GB budget less the reranker and the ~4.4 GB OCR model, and
/// Station's 8 GB of its ~20 GB (the card has room, but the chat LLM comes
/// first).
///
/// This is a **cap**, not a target: the flags are sized for what the model
/// needs, and a build that fills the cap is reported, with the smaller one
/// beside it.
///
/// **Every number is an estimate from model sizes, not a measurement.**
fn embed_vram_cap_gb(tier: Tier) -> f64 {
    match tier {
        Tier::Desk => 3.3,
        Tier::Studio => 4.4,
        Tier::Station => 8.0,
        _ => 4.0,
    }
}

/// How close to the cap a build has to be before it is worth saying so.
///
/// The 4-bit build leaves room on every tier; the 16-bit one fills Desk's
/// share exactly, which is the thing an operator on a Desk card needs to
/// hear.
const EMBED_CAP_WARN_RATIO: f64 = 0.9;

/// The weights a build holds, in GiB.
fn embed_weights_gib(precision: EmbedPrecision) -> f64 {
    match precision {
        EmbedPrecision::Nvfp4 => NEMOTRON_NVFP4_WEIGHTS_GIB,
        EmbedPrecision::Bf16 => NEMOTRON_BF16_WEIGHTS_GIB,
    }
}

/// How many embedding requests the tier has in flight, which is how many the
/// model server should serve at once.
///
/// These are the tier preset's `KB_EMBED_CONCURRENCY` values (Desk 2, Studio
/// 4, Station 8): kb-server never has more text-chunk requests in flight than
/// that. The page arm has its own knob (`KB_PAGE_EMBEDDING_CONCURRENCY`,
/// 8 by default); requests over `--max-num-seqs` queue in the model server
/// instead of failing.
#[must_use]
pub fn embed_max_num_seqs(tier: Tier) -> u32 {
    match tier {
        Tier::Desk => 2,
        Tier::Studio => 4,
        Tier::Station => 8,
        _ => 4,
    }
}

/// The KV cache the flags leave for, in GiB.
///
/// It has to hold **one full-length sequence** (`max_model_len` tokens), or
/// vLLM refuses to start, and the requests the tier has in flight on top of
/// that.
fn embed_kv_gib(tier: Tier) -> f64 {
    let seq_kv_gib = EMBED_MAX_MODEL_LEN as f64 * KV_BYTES_PER_TOKEN / GIB;
    let page_kv_gib = EMBED_PAGE_TOKENS * KV_BYTES_PER_TOKEN / GIB;
    seq_kv_gib.max(page_kv_gib * embed_max_num_seqs(tier) as f64)
}

/// Size the embedder's vLLM flags for this tier, build and card.
///
/// The arithmetic, and where every number comes from:
///
/// * the **weights** are the checkpoint's own size (2.90 GiB BF16, 1.18 GiB
///   NVFP4), read from the Hub's safetensors index;
/// * the **runtime** is an estimate (0.4 GiB: the CUDA context, the
///   activations, the pooler);
/// * the **KV cache** is what is left: one full-length sequence (8,192
///   tokens at 64 KiB each = 0.5 GiB) or the tier's in-flight requests,
///   whichever is bigger;
/// * `--max-num-seqs` is the tier's in-flight count (2, 4, 8);
/// * `--gpu-memory-utilization` is that footprint over the card's memory:
///   vLLM's fraction is of the **whole** card, so one number means a
///   different budget on an 8 GB card than on a 128 GB one.
///
/// The BF16 footprint this gives on a GB10 is ~3.8 GB, next to the ~3.7 GB
/// measured there with the sized flags (issue #37). **The NVFP4 footprint is
/// an estimate: it has not been measured.**
#[must_use]
pub fn embedder_sizing(
    tier: Tier,
    precision: EmbedPrecision,
    hardware: &Hardware,
) -> EmbedderSizing {
    let weights = embed_weights_gib(precision);
    let kv_gb = embed_kv_gib(tier);
    let target_gb = weights + VLLM_RUNTIME_GIB + kv_gb;
    let gpu_fraction = hardware
        .usable_vram_gb()
        .map(|vram| (target_gb / vram).clamp(0.02, 0.6));
    EmbedderSizing {
        max_model_len: EMBED_MAX_MODEL_LEN,
        max_num_seqs: embed_max_num_seqs(tier),
        gpu_fraction,
        target_gb,
        weights_gb: weights,
        kv_gb,
    }
}

/// Choose the embedder checkpoint for a tier on this machine.
///
/// The rules, in order:
///
/// 1. `--embed-precision` wins: the operator asked for that build.
/// 2. An env file that already names a build (`KB_EMBED_PRECISION`) or a
///    model keeps it: an explicit setting always wins.
/// 3. An env file for **this** tier that names neither keeps the preset's
///    BF16 build. The index behind it was made by BF16, and changing the
///    embedder is a re-index and not an upgrade, so an existing install is
///    never switched silently.
/// 4. Otherwise the tier's own preference (NVFP4 on Desk and Studio, the
///    product owner's decision of 2026-11-20) when the card has native FP4,
///    and BF16 everywhere else.
#[must_use]
pub fn choose_embedder(
    args: &SetupArgs,
    tier: Tier,
    hardware: &Hardware,
    existing: Option<&BTreeMap<String, String>>,
) -> EmbedderChoice {
    if let Some(precision) = args.embed_precision {
        return EmbedderChoice {
            model: precision.model_id().to_string(),
            precision: Some(precision),
            reason: format!(
                "you asked for the {} build with --embed-precision",
                precision.as_str()
            ),
            differs_from_preset: precision != EmbedPrecision::Bf16,
        };
    }

    if let Some(file) = existing {
        if let Some(named) = file
            .get("KB_EMBED_PRECISION")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
        {
            if let Some(precision) = EmbedPrecision::parse(named) {
                return EmbedderChoice {
                    model: precision.model_id().to_string(),
                    precision: Some(precision),
                    reason: format!(
                        "the env file already names the {} build",
                        precision.as_str()
                    ),
                    differs_from_preset: false,
                };
            }
        }
        // `KB_EMBED_MODEL` is what the model server serves, the other two are
        // what the server asks for: any of them naming a build keeps it, so
        // the two sides cannot end up on different builds.
        for key in [
            "KB_EMBED_MODEL",
            "KB_EMBEDDING_MODEL",
            "KB_PAGE_EMBEDDING_MODEL",
        ] {
            let Some(model) = file
                .get(key)
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            return EmbedderChoice {
                model: model.to_string(),
                precision: EmbedPrecision::of_model(model),
                reason: format!("the env file already names {model}"),
                differs_from_preset: false,
            };
        }
    }

    let same_tier = existing
        .is_some_and(|file| file.get("KB_TIER").map(|value| value.trim()) == Some(tier.as_str()));
    if same_tier {
        return EmbedderChoice {
            model: EmbedPrecision::Bf16.model_id().to_string(),
            precision: Some(EmbedPrecision::Bf16),
            reason: "this install already embeds with the BF16 build; the NVFP4 build \
                     makes other vectors, so it stays until you opt in \
                     (--embed-precision nvfp4) and re-index"
                .to_string(),
            differs_from_preset: false,
        };
    }

    let preferred = EmbedPrecision::for_tier(tier);
    if preferred == EmbedPrecision::Nvfp4 && hardware.has_fp4() {
        return EmbedderChoice {
            model: preferred.model_id().to_string(),
            precision: Some(preferred),
            reason: fp4_reason(hardware),
            differs_from_preset: true,
        };
    }

    EmbedderChoice {
        model: EmbedPrecision::Bf16.model_id().to_string(),
        precision: Some(EmbedPrecision::Bf16),
        reason: bf16_reason(hardware, tier),
        differs_from_preset: false,
    }
}

/// Why the embedder runs in NVFP4 here, in a sentence.
fn fp4_reason(hardware: &Hardware) -> String {
    let Some(gpu) = hardware.fp4_gpu() else {
        return "no NVIDIA GPU with native FP4 was found, so the BF16 build runs".to_string();
    };
    match gpu.compute_cap {
        Some(capability) => format!(
            "the {} has compute capability {capability}, which has native FP4",
            gpu.name
        ),
        None => format!(
            "the {} is a Blackwell part (the driver did not report its compute \
             capability), so it has native FP4",
            gpu.name
        ),
    }
}

/// The embedder plan for a tier, or `None` when the tier runs no local
/// embedder under vLLM (Lite and Cloud run none; the two-embedder stack runs
/// TEI and the Qwen3-VL embedders, which have no NVFP4 build).
fn embedder_plan(
    args: &SetupArgs,
    tier: Tier,
    hardware: &Hardware,
    existing: Option<&BTreeMap<String, String>>,
) -> Option<EmbedderPlan> {
    if !tier.has_embedder_stacks() {
        return None;
    }
    if args.embedders.unwrap_or_default() != crate::tier::Embedders::One {
        return None;
    }
    let choice = choose_embedder(args, tier, hardware, existing);
    let sizing = choice
        .precision
        .map(|precision| embedder_sizing(tier, precision, hardware));
    Some(EmbedderPlan { choice, sizing })
}

/// Print the embedder's build, the flags sized for it and the note about
/// re-indexing.
fn write_embedder(
    out: &mut dyn Write,
    plan: &EmbedderPlan,
    tier: Tier,
    hardware: &Hardware,
) -> Result<(), CliError> {
    writeln!(out, "Embedder").map_err(io_failed)?;
    let build = plan
        .choice
        .precision
        .map(|precision| precision.as_str())
        .unwrap_or("the tier's model");
    writeln!(out, "  {} ({build})", plan.choice.model).map_err(io_failed)?;
    writeln!(out, "  {}", plan.choice.reason).map_err(io_failed)?;
    if let Some(sizing) = plan.sizing {
        let fraction = sizing
            .gpu_fraction
            .map(|fraction| format!("{fraction:.3}"))
            .unwrap_or_else(|| "unset".to_string());
        writeln!(
            out,
            "  vLLM flags: --max-model-len {}, --max-num-seqs {}, --gpu-memory-utilization {fraction}",
            sizing.max_model_len, sizing.max_num_seqs
        )
        .map_err(io_failed)?;
        writeln!(
            out,
            "  footprint: ~{:.1} GB = {:.1} GB weights + {:.1} GB runtime + {:.1} GB KV cache",
            sizing.target_gb, sizing.weights_gb, VLLM_RUNTIME_GIB, sizing.kv_gb
        )
        .map_err(io_failed)?;
        let cap = embed_vram_cap_gb(tier);
        if sizing.target_gb >= cap * EMBED_CAP_WARN_RATIO {
            let nvfp4 = embedder_sizing(tier, crate::tier::EmbedPrecision::Nvfp4, hardware);
            writeln!(
                out,
                "  note: the {} build needs ~{:.1} GB of {}'s ~{:.1} GB share of the model \
                 budget; the NVFP4 build needs ~{:.1} GB",
                build,
                sizing.target_gb,
                tier.as_str(),
                cap,
                nvfp4.target_gb
            )
            .map_err(io_failed)?;
        }
    }
    if plan.choice.differs_from_preset {
        writeln!(
            out,
            "  note: the other build of this checkpoint makes other vectors: run \
             `kb-server reindex` after switching, or the index stays empty"
        )
        .map_err(io_failed)?;
    }
    Ok(())
}

/// Why the embedder runs in BF16 here, in a sentence.
fn bf16_reason(hardware: &Hardware, tier: Tier) -> String {
    if tier == Tier::Station {
        return "Station runs the BF16 build (the product owner's decision of 2026-11-20)"
            .to_string();
    }
    let Some(gpu) = hardware.largest_nvidia() else {
        return "no NVIDIA GPU was found, so the BF16 build runs".to_string();
    };
    match gpu.compute_cap {
        Some(capability) => format!(
            "the {} has compute capability {capability}, which has no native FP4",
            gpu.name
        ),
        None => format!(
            "the compute capability of the {} could not be read, so the BF16 build \
             (which runs on every CUDA GPU) is used",
            gpu.name
        ),
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
            // What is left of an 8 GB card after the chat model. The BF16
            // embedder (3.8 GB) plus the reranker (1.2 GB) do **not** fit
            // here; the 4-bit build's 3.3 GB does.
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
        "OpenMDW-1.1 (permissive: keep the licence and notices; patent-suit termination)",
        LicenceClass::Green,
        "text and page embedder on desk, studio and station (DEP-6, the default since 0.6.0)",
    ),
    entry(
        "nvidia/Nemotron-3.5-Embed-1B-NVFP4",
        "OpenMDW-1.1 (permissive: keep the licence and notices; patent-suit termination)",
        LicenceClass::Green,
        "the same checkpoint in 4-bit; the embedder on a Blackwell card (KB_EMBED_PRECISION=nvfp4)",
    ),
    entry(
        "Qwen/Qwen3-Reranker-0.6B",
        "Apache-2.0",
        LicenceClass::Green,
        "chunk reranker on desk, studio and station (DEP-6)",
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

/// The models a tier loads with its default embedding stack, from
/// `docs/kb-tiers.md` "What each tier runs".
#[must_use]
pub fn tier_models(tier: Tier) -> Vec<&'static str> {
    tier_models_for(
        tier,
        crate::tier::Embedders::default(),
        crate::tier::EmbedPrecision::default(),
    )
}

/// The models a tier loads with an embedding stack. The GPU tiers' default is
/// one embedder (`DEP-6`); `--embedders two` is the 0.3–0.5 stack. Cloud also
/// gets `mistral-ocr-latest` when the operator supplies its key.
///
/// The one-embedder checkpoint has two builds; `precision` says which one the
/// tier loads, and the licence gate covers both.
#[must_use]
pub fn tier_models_for(
    tier: Tier,
    embedders: crate::tier::Embedders,
    precision: crate::tier::EmbedPrecision,
) -> Vec<&'static str> {
    if embedders == crate::tier::Embedders::One && tier.has_embedder_stacks() {
        return vec![
            precision.model_id(),
            "Qwen/Qwen3-Reranker-0.6B",
            "xiaomi-research/Xiaomi-OCR-0",
            "Qwen/Qwen3-VL-2B-Instruct",
        ];
    }
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
    /// `--embed-precision nvfp4|bf16`: which build of the one-embedder
    /// checkpoint runs. `None` lets the hardware and the tier decide, and
    /// keeps what an existing install already uses.
    pub embed_precision: Option<crate::tier::EmbedPrecision>,
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
                "--embed-precision" => {
                    let value = value_of(args, &mut index, "--embed-precision")?;
                    parsed.embed_precision =
                        Some(crate::tier::EmbedPrecision::parse(&value).ok_or_else(|| {
                            CliError::Usage(format!(
                                "--embed-precision must be nvfp4 or bf16, got {value:?}"
                            ))
                        })?);
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
       The CUDA compute capability is read too: it decides whether the 4-bit
       (NVFP4) build of the embedder can run on this card.
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
    --embed-precision <nvfp4|bf16> Which build of the embedder runs. nvfp4 is the same model in
                                   4-bit and needs a Blackwell card (FP4 tensor cores); bf16
                                   runs on every CUDA GPU. Without the flag the hardware
                                   decides (nvfp4 on Desk and Studio, bf16 on Station) and an
                                   existing install keeps the build it already uses. Changing
                                   the build is a re-index (kb-server reindex).
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
        // One embedder or two (`DEP-6`): a GPU tier's model services live
        // behind `<tier>-one` / `<tier>-two` so exactly one stack starts.
        // Unset is one, the default since 0.6.0.
        if tier.has_embedder_stacks() {
            let one = args.embedders.unwrap_or_default() == crate::tier::Embedders::One;
            profiles.push(format!("{name}-{}", if one { "one" } else { "two" }));
        }
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
/// The embedder's **build** is not a preset value — it depends on the card —
/// so `KB_EMBED_PRECISION` is written, and the preset turns it into the
/// model name.
fn env_changes(
    args: &SetupArgs,
    tier: Tier,
    lookup: &dyn Fn(&str) -> Option<String>,
    embedder: Option<&EmbedderPlan>,
) -> Vec<EnvChange> {
    let mut changes = vec![EnvChange::plain("KB_TIER", tier.as_str()).with_comment(
        "Deployment tier (docs/kb-tiers.md). The preset fills the model roles; an \
         explicit KB_* setting still wins.",
    )];
    if tier.has_embedder_stacks() {
        // Written either way, so the file says which stack it runs and the
        // endpoints match the compose services that stack starts.
        if args.embedders.unwrap_or_default() == crate::tier::Embedders::One {
            changes.push(EnvChange::plain("KB_EMBEDDERS", "one").with_comment(
                "One embedder for text and pages (DEP-6, the default since 0.6.0): \
                     Nemotron-3.5-Embed-1B in a single 2048-d space, with \
                     Qwen3-Reranker-0.6B. Moving to it from the two-embedder stack is \
                     a re-index (kb-server reindex).",
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
            // The build of the checkpoint, and the vLLM flags sized for this
            // card. The model name itself is not written: the preset fills
            // `KB_EMBEDDING_MODEL` and `KB_PAGE_EMBEDDING_MODEL` from
            // `KB_EMBED_PRECISION`.
            if let Some(plan) = embedder {
                if let Some(precision) = plan.choice.precision {
                    let comment = if precision == crate::tier::EmbedPrecision::Nvfp4 {
                        "The 4-bit build of the embedder (the same checkpoint in NVFP4). It \
                         needs a Blackwell card (compute capability 10.x/12.x) and holds \
                         ~1.2 GB of weights instead of ~2.9 GB. It makes other vectors \
                         than BF16: switching is a re-index (kb-server reindex)."
                    } else {
                        "The 16-bit build of the embedder; it runs on every CUDA GPU. \
                         Switching to NVFP4 is a re-index (kb-server reindex)."
                    };
                    changes.push(
                        EnvChange::plain("KB_EMBED_PRECISION", precision.as_str())
                            .with_comment(comment),
                    );
                    changes.push(
                        EnvChange::plain("KB_EMBED_MODEL", &plan.choice.model).with_comment(
                            "The checkpoint the model server serves. The server asks for it by \
                         the name the tier preset fills from KB_EMBED_PRECISION; both must \
                         name the same build.",
                        ),
                    );
                    if let Some(sizing) = plan.sizing {
                        changes.push(
                            EnvChange::plain(
                                "KB_EMBED_MAX_MODEL_LEN",
                                sizing.max_model_len.to_string(),
                            )
                            .with_comment(
                                "The model server's context. The checkpoint supports 32768, \
                                 but a chunk or a page is far shorter and the KV cache is \
                                 what the embedder's footprint is made of.",
                            ),
                        );
                        changes.push(
                            EnvChange::plain(
                                "KB_EMBED_MAX_NUM_SEQS",
                                sizing.max_num_seqs.to_string(),
                            )
                            .with_comment(
                                "How many page-sized requests the KV cache holds at once \
                                 (~1,600 tokens each), sized from the card's memory.",
                            ),
                        );
                        if let Some(fraction) = sizing.gpu_fraction {
                            changes.push(
                                EnvChange::plain("KB_EMBED_GPU_FRACTION", format!("{fraction:.3}"))
                                    .with_comment(
                                        "The share of the whole card the model server may hold. \
                                     vLLM's fraction is of the card, not of the model, so \
                                     it is sized from the card's memory.",
                                    ),
                            );
                        }
                    }
                }
            }
        } else {
            changes.push(EnvChange::plain("KB_EMBEDDERS", "two").with_comment(
                "Two embedders (the 0.3-0.5 stack): Qwen3-Embedding-0.6B for text, \
                 Qwen3-VL-Embedding-* for pages on studio and station.",
            ));
            let (rerank_provider, rerank_url, page_url) = match tier {
                Tier::Desk => ("tei", "http://tei-rerank-desk:80", None),
                Tier::Studio => (
                    "vllm",
                    "http://vllm-rerank-studio:8000",
                    Some("http://vllm-page-embed-studio:8000/v1/embeddings"),
                ),
                _ => (
                    "vllm",
                    "http://vllm-rerank-station:8000",
                    Some("http://vllm-page-embed-station:8000/v1/embeddings"),
                ),
            };
            changes.push(EnvChange::plain(
                "KB_EMBEDDING_BASE_URL",
                format!("http://tei-embed-{}:80/v1/embeddings", tier.as_str()),
            ));
            changes.push(EnvChange::plain("KB_RERANK_PROVIDER", rerank_provider));
            changes.push(EnvChange::plain("KB_RERANK_BASE_URL", rerank_url));
            // Desk's two-embedder stack has no page arm; an empty value keeps
            // the preset from filling one.
            changes.push(EnvChange::plain(
                "KB_PAGE_EMBEDDING_BASE_URL",
                page_url.unwrap_or_default(),
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

    // The env file is read before the plan is written: an install that is
    // already there keeps the embedder build its index was made with.
    let existing_text = std::fs::read_to_string(env_path).ok();
    let existing = existing_text.as_deref().map(parse_env);
    let embedder = embedder_plan(args, tier, hardware, existing.as_ref());

    write_budget(out, hardware, tier, args.force, args.detect_only)?;
    if let Some(plan) = &embedder {
        write_embedder(out, plan, tier, hardware)?;
        writeln!(out).map_err(io_failed)?;
    }
    let models = models_for(args, tier, lookup, embedder.as_ref());
    write_licences(out, &models, args)?;
    writeln!(out).map_err(io_failed)?;

    let mut changes = env_changes(args, tier, lookup, embedder.as_ref());
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
    embedder: Option<&EmbedderPlan>,
) -> Vec<&'static str> {
    let _ = lookup;
    let precision = embedder
        .and_then(|plan| plan.choice.precision)
        .unwrap_or_default();
    let mut models = tier_models_for(tier, args.embedders.unwrap_or_default(), precision);
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
            let capability = match gpu.compute_cap {
                Some(capability) => format!(", compute capability {capability}"),
                None => String::new(),
            };
            writeln!(
                out,
                "  GPU       {kind} {} — {:.1} GB{capability}",
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
            "lite,lite-ocr,lite-vision"
        );
        assert_eq!(
            compose_profiles(&args, Tier::Desk),
            "desk,desk-ocr,desk-vision,desk-one"
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
            Some("desk,desk-ocr,desk-vision,desk-one")
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
        assert_eq!(compose_profiles(&parsed, Tier::Lite), "lite,lite-vision");
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
            Some("lite,lite-vision")
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
            Some("lite,lite-vision")
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
            Some("lite,lite-ocr")
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

        // Without the flag it is the same: one embedder is the default.
        let plain = SetupArgs::parse(&args_of(&[])).expect("parse");
        assert_eq!(
            compose_profiles(&plain, Tier::Studio),
            "studio,studio-ocr,studio-vision,studio-one"
        );
        let env = written_with(&nvidia_box(24.0), &plain, Tier::Studio, &|_: &str| None);
        assert_eq!(env.get("KB_EMBEDDERS").map(String::as_str), Some("one"));

        // Lite has one stack: no selector, no embedder profile.
        let lite = written_with(&nvidia_box(4.0), &plain, Tier::Lite, &|_: &str| None);
        assert!(!lite.contains_key("KB_EMBEDDERS"));
    }

    // ─── the embedder build (NVFP4 vs BF16) ────────────────────────────────

    /// A Blackwell card gets the 4-bit build; a card without native FP4 falls
    /// back to BF16, which runs on every CUDA GPU.
    #[test]
    fn a_blackwell_card_gets_the_nvfp4_build_and_an_older_card_the_bf16_one() {
        let blackwell = nvidia_runner("NVIDIA GB10", 128.0, Some("12.1")).hardware();
        assert!(blackwell.has_fp4());
        assert_eq!(
            blackwell.gpus[0].compute_cap,
            Some(ComputeCapability {
                major: 12,
                minor: 1
            })
        );
        let args = SetupArgs::parse(&args_of(&[])).expect("parse");
        let choice = choose_embedder(&args, Tier::Desk, &blackwell, None);
        assert_eq!(choice.precision, Some(EmbedPrecision::Nvfp4));
        assert_eq!(choice.model, "nvidia/Nemotron-3.5-Embed-1B-NVFP4");
        assert!(choice.differs_from_preset);

        // An RTX 4090 (sm_89) has no FP4 tensor cores.
        let ada = nvidia_runner("NVIDIA GeForce RTX 4090", 24.0, Some("8.9")).hardware();
        assert!(!ada.has_fp4());
        let choice = choose_embedder(&args, Tier::Desk, &ada, None);
        assert_eq!(choice.precision, Some(EmbedPrecision::Bf16));
        assert_eq!(choice.model, "nvidia/Nemotron-3.5-Embed-1B-BF16");
        assert!(!choice.differs_from_preset);

        // Station keeps BF16 even on a Blackwell card (the owner's decision).
        let choice = choose_embedder(&args, Tier::Station, &blackwell, None);
        assert_eq!(choice.precision, Some(EmbedPrecision::Bf16));
    }

    /// A driver that does not know the field answers `[N/A]`: that is
    /// "unknown", not a wrong capability. The card's name is then the
    /// fallback, and only for names that cannot mean anything else.
    #[test]
    fn an_unknown_compute_capability_falls_back_to_the_cards_name() {
        let caps = parse_compute_caps("[N/A]\n");
        assert_eq!(caps, vec![None]);
        let blackwell = nvidia_runner("NVIDIA GB10", 128.0, Some("[N/A]")).hardware();
        assert_eq!(blackwell.gpus[0].compute_cap, None);
        assert!(blackwell.gpus[0].supports_fp4());
        assert!(blackwell.has_fp4());

        // A card whose name says nothing about Blackwell keeps BF16.
        let older = nvidia_runner("NVIDIA GeForce RTX 4090", 24.0, Some("[N/A]")).hardware();
        assert!(!older.gpus[0].supports_fp4());
        assert!(!older.has_fp4());
    }

    /// An install that is already there keeps the build its index was made
    /// with: switching the embedder is a re-index, not an upgrade.
    #[test]
    fn an_existing_install_keeps_its_build_until_the_operator_opts_in() {
        let blackwell = nvidia_runner("NVIDIA GB10", 128.0, Some("12.1")).hardware();
        let args = SetupArgs::parse(&args_of(&[])).expect("parse");
        let mut existing = BTreeMap::new();
        existing.insert("KB_TIER".to_string(), "desk".to_string());
        let choice = choose_embedder(&args, Tier::Desk, &blackwell, Some(&existing));
        assert_eq!(
            choice.precision,
            Some(EmbedPrecision::Bf16),
            "an existing Desk install must not be switched to NVFP4 on an upgrade"
        );
        assert!(choice.reason.contains("re-index"), "{}", choice.reason);

        // The operator opts in.
        let asked = SetupArgs::parse(&args_of(&["--embed-precision", "nvfp4"])).expect("parse");
        let choice = choose_embedder(&asked, Tier::Desk, &blackwell, Some(&existing));
        assert_eq!(choice.precision, Some(EmbedPrecision::Nvfp4));

        // An install that already runs NVFP4 keeps it.
        let mut existing = BTreeMap::new();
        existing.insert("KB_TIER".to_string(), "desk".to_string());
        existing.insert("KB_EMBED_PRECISION".to_string(), "nvfp4".to_string());
        let choice = choose_embedder(&args, Tier::Desk, &blackwell, Some(&existing));
        assert_eq!(choice.precision, Some(EmbedPrecision::Nvfp4));

        // An env file that names only the checkpoint the model server serves
        // keeps it too: otherwise the server and the model server would end
        // up on different builds of the same checkpoint.
        let mut existing = BTreeMap::new();
        existing.insert("KB_TIER".to_string(), "desk".to_string());
        existing.insert(
            "KB_EMBED_MODEL".to_string(),
            "nvidia/Nemotron-3.5-Embed-1B-NVFP4".to_string(),
        );
        let choice = choose_embedder(&args, Tier::Desk, &blackwell, Some(&existing));
        assert_eq!(choice.precision, Some(EmbedPrecision::Nvfp4));
        assert_eq!(choice.model, "nvidia/Nemotron-3.5-Embed-1B-NVFP4");
    }

    /// The vLLM flags are sized from the card: the memory fraction is of the
    /// whole card, so one number cannot serve an 8 GB card and a 48 GB one.
    #[test]
    fn the_vllm_flags_are_sized_from_the_card() {
        let desk = nvidia_box(8.0);
        let nvfp4 = embedder_sizing(Tier::Desk, EmbedPrecision::Nvfp4, &desk);
        assert_eq!(nvfp4.max_model_len, 8192);
        assert_eq!(nvfp4.weights_gb, 1.18);
        assert_eq!(nvfp4.max_num_seqs, 2, "Desk has two requests in flight");
        // The 4-bit build fits Desk's share with room to spare; the footprint
        // is the weights, the runtime and one full-length sequence in the KV
        // cache.
        assert!(nvfp4.target_gb <= embed_vram_cap_gb(Tier::Desk) * EMBED_CAP_WARN_RATIO);
        assert!(
            (nvfp4.gpu_fraction.expect("fraction") - nvfp4.target_gb / 8.0).abs() < 0.001,
            "the fraction is of the whole card"
        );

        // The BF16 build does not fit Desk's share: the flags grow with it,
        // and the plan says so.
        let bf16 = embedder_sizing(Tier::Desk, EmbedPrecision::Bf16, &desk);
        assert_eq!(bf16.weights_gb, 2.90);
        assert!(bf16.target_gb > embed_vram_cap_gb(Tier::Desk));
        assert!(
            bf16.kv_gb >= 0.49,
            "the KV cache must hold one full-length sequence, got {}",
            bf16.kv_gb
        );

        // The BF16 footprint on a GB10 (~3.8 GB) is the ~3.7 GB measured there
        // with the sized flags (issue #37).
        let gb10 = nvidia_box(128.0);
        let bf16 = embedder_sizing(Tier::Desk, EmbedPrecision::Bf16, &gb10);
        assert!(
            (bf16.target_gb - 3.8).abs() < 0.05,
            "the BF16 footprint should land near the measured 3.7 GB, got {}",
            bf16.target_gb
        );

        let station = nvidia_box(48.0);
        let bf16 = embedder_sizing(Tier::Station, EmbedPrecision::Bf16, &station);
        assert!((bf16.gpu_fraction.expect("fraction") - (bf16.target_gb / 48.0)).abs() < 0.001);
        assert_eq!(bf16.max_num_seqs, 8, "Station has eight requests in flight");
    }

    /// The env file names the build and the flags sized for it.
    #[test]
    fn the_env_file_names_the_build_and_the_sized_flags() {
        let blackwell = nvidia_runner("NVIDIA GB10", 128.0, Some("12.1")).hardware();
        let args = SetupArgs::parse(&args_of(&[])).expect("parse");
        let env = written_with(&blackwell, &args, Tier::Desk, &|_: &str| None);
        assert_eq!(
            env.get("KB_EMBED_PRECISION").map(String::as_str),
            Some("nvfp4")
        );
        assert_eq!(
            env.get("KB_EMBED_MODEL").map(String::as_str),
            Some("nvidia/Nemotron-3.5-Embed-1B-NVFP4")
        );
        assert_eq!(
            env.get("KB_EMBED_MAX_MODEL_LEN").map(String::as_str),
            Some("8192")
        );
        assert!(env.contains_key("KB_EMBED_MAX_NUM_SEQS"));
        assert!(env.contains_key("KB_EMBED_GPU_FRACTION"));
        // The model name itself is the preset's: it fills it from the build.
        assert!(!env.contains_key("KB_EMBEDDING_MODEL"));

        // The two-embedder stack runs other models; no build is written.
        let two = SetupArgs::parse(&args_of(&["--embedders", "two"])).expect("parse");
        let env = written_with(&blackwell, &two, Tier::Desk, &|_: &str| None);
        assert!(!env.contains_key("KB_EMBED_PRECISION"));
        assert!(!env.contains_key("KB_EMBED_MODEL"));
    }

    /// The licence gate covers the 4-bit build too: a model without a verdict
    /// is refused.
    #[test]
    fn the_nvfp4_build_has_a_licence_verdict() {
        assert!(licence_entry("nvidia/Nemotron-3.5-Embed-1B-NVFP4").is_some());
        let report = gate_licences(&["nvidia/Nemotron-3.5-Embed-1B-NVFP4"], &[]).expect("gate");
        assert_eq!(report.green.len(), 1);
    }

    /// `--embedders two` writes the 0.3-0.5 stack's compose endpoints, which
    /// the compose file no longer defaults to.
    #[test]
    fn embedders_two_writes_the_old_stack_endpoints() {
        let parsed = SetupArgs::parse(&args_of(&["--embedders", "two"])).expect("parse");
        assert_eq!(
            compose_profiles(&parsed, Tier::Desk),
            "desk,desk-ocr,desk-vision,desk-two"
        );
        let desk = written_with(&nvidia_box(8.0), &parsed, Tier::Desk, &|_: &str| None);
        assert_eq!(desk.get("KB_EMBEDDERS").map(String::as_str), Some("two"));
        assert_eq!(
            desk.get("KB_EMBEDDING_BASE_URL").map(String::as_str),
            Some("http://tei-embed-desk:80/v1/embeddings")
        );
        assert_eq!(
            desk.get("KB_RERANK_PROVIDER").map(String::as_str),
            Some("tei")
        );
        assert_eq!(
            desk.get("KB_RERANK_BASE_URL").map(String::as_str),
            Some("http://tei-rerank-desk:80")
        );
        assert_eq!(
            desk.get("KB_PAGE_EMBEDDING_BASE_URL").map(String::as_str),
            Some("")
        );

        let station = written_with(&nvidia_box(48.0), &parsed, Tier::Station, &|_: &str| None);
        assert_eq!(
            station
                .get("KB_PAGE_EMBEDDING_BASE_URL")
                .map(String::as_str),
            Some("http://vllm-page-embed-station:8000/v1/embeddings")
        );
        assert_eq!(
            station.get("KB_RERANK_BASE_URL").map(String::as_str),
            Some("http://vllm-rerank-station:8000")
        );
    }

    #[test]
    fn accurate_tables_add_the_layout_pass_only_on_a_gpu_tier() {
        let parsed = SetupArgs::parse(&args_of(&["--ocr-tables", "accurate"])).expect("parse");
        assert!(check_roles(&parsed, Tier::Lite).is_err());
        assert!(check_roles(&parsed, Tier::Desk).is_ok());
        assert_eq!(
            compose_profiles(&parsed, Tier::Studio),
            "studio,studio-ocr,studio-ocr-layout,studio-vision,studio-one"
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
        let changes = env_changes(&args, Tier::Desk, &|_: &str| None, None);
        assert!(
            !changes
                .iter()
                .any(|change| change.key.starts_with("KB_PAGE_CAPTION")),
            "{changes:?}"
        );
        // Studio has the page arm, so the same flags are written there.
        let studio = env_changes(&args, Tier::Studio, &|_: &str| None, None);
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

    /// A runner whose `nvidia-smi` answers for one card, with its compute
    /// capability when the driver knows it.
    fn nvidia_runner(name: &str, vram_gb: f64, capability: Option<&str>) -> FakeRunner {
        let mut runner = FakeRunner::linux(16, 64.0);
        let mib = (vram_gb * 1024.0) as u64;
        runner.commands.insert(
            "nvidia-smi --query-gpu=name,memory.total --format=csv,noheader,nounits".to_string(),
            format!("{name}, {mib}"),
        );
        if let Some(capability) = capability {
            runner.commands.insert(
                "nvidia-smi --query-gpu=compute_cap --format=csv,noheader,nounits".to_string(),
                capability.to_string(),
            );
        }
        runner
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
                compute_cap: None,
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
