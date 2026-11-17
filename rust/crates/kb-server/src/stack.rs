//! `kb-server up|down|logs|ps|status|key` — the stack commands.
//!
//! This is a Rust port of `rust/scripts/kb-dev.sh`, so an operator can drive
//! the same compose stack with either. The script only works from a source
//! checkout (it resolves paths relative to the repo); an installed bundle has
//! no checkout, so the commands have to live in the binary. Both use the same
//! compose project (`rantai-kb-dev`), the same compose file and the same env
//! file, so the two agree about what "up" means.
//!
//! **Profiles.** The compose services are activated by profiles. If the env
//! file sets `COMPOSE_PROFILES`, compose reads it natively and this module
//! passes no `--profile` flag at all; otherwise the tier's own profiles are
//! passed (`<tier>`, `<tier>-ocr`, `<tier>-vision`). `rust/scripts/kb-dev.sh`
//! follows the same rule, so the script and the binary start the same
//! containers.
//!
//! Nothing here decides *what* a tier runs: that is the tier preset in
//! `tier.rs` and the compose file. This module starts, stops and inspects.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::cli::CliError;
use crate::tier::Tier;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// The compose project the dev stack lives in (`KB_DEV_PROJECT` overrides it).
pub const DEFAULT_PROJECT: &str = "rantai-kb-dev";

/// How long `up` waits for `/health`, in seconds (`KB_WAIT_SECONDS`).
pub const DEFAULT_WAIT_SECONDS: u64 = 900;

/// The oldest `docker compose` this stack needs: `depends_on.required: false`
/// landed in compose v2.20.0.
pub const MIN_COMPOSE: (u32, u32) = (2, 20);

/// The default host port of kb-server.
pub const DEFAULT_PORT: u16 = 8080;

// ---------------------------------------------------------------------------
// Where the deploy directory is
// ---------------------------------------------------------------------------

/// What kind of install a deploy directory belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeployKind {
    /// `$RANTAI_KB_HOME/deploy` — the operator named the home.
    Home,
    /// `rust/deploy` or `deploy` under the working directory: a source checkout.
    Checkout,
    /// `~/.rantai-kb/deploy`: an installed bundle.
    Installed,
}

/// The deploy directory, and the kind of install it belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeployDir {
    /// The directory holding `compose.yaml`, `.env` and `.tier`.
    pub path: PathBuf,
    /// How the directory was found.
    pub kind: DeployKind,
}

/// Resolve the deploy directory.
///
/// The order is the one an operator would guess:
///
/// 1. `$RANTAI_KB_HOME/deploy` when `RANTAI_KB_HOME` is set — the installer
///    and the operator's own home.
/// 2. `rust/deploy` or `deploy` under the working directory, when one of them
///    exists: a source checkout, today's behaviour.
/// 3. `~/.rantai-kb/deploy`: an installed bundle.
///
/// The lookup is injected so a test can point it at a temporary directory
/// instead of mutating the process environment.
#[must_use]
pub fn resolve_deploy_dir(lookup: &dyn Fn(&str) -> Option<String>, cwd: &Path) -> DeployDir {
    if let Some(home) = non_empty(lookup("RANTAI_KB_HOME")) {
        return DeployDir {
            path: PathBuf::from(home).join("deploy"),
            kind: DeployKind::Home,
        };
    }
    for candidate in ["rust/deploy", "deploy"] {
        let path = cwd.join(candidate);
        if path.is_dir() {
            return DeployDir {
                path,
                kind: DeployKind::Checkout,
            };
        }
    }
    let home = non_empty(lookup("HOME")).or_else(|| non_empty(lookup("USERPROFILE")));
    let path = match home {
        Some(home) => PathBuf::from(home).join(".rantai-kb").join("deploy"),
        None => PathBuf::from(".rantai-kb").join("deploy"),
    };
    DeployDir {
        path,
        kind: DeployKind::Installed,
    }
}

/// Whether the deploy directory belongs to a source checkout.
///
/// A checkout has the workspace next to the deploy directory (`../crates`); an
/// installed bundle has only `bin/`, `lib/` and `deploy/`. An installed bundle
/// cannot build the image, so it pulls the published one instead.
#[must_use]
pub fn is_source_checkout(deploy_dir: &Path) -> bool {
    deploy_dir
        .parent()
        .is_some_and(|root| root.join("crates").is_dir())
}

/// The first non-blank value of a lookup.
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

// ---------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------

/// Which compose profiles a command activates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Profiles {
    /// `COMPOSE_PROFILES` is set in the env file. Compose reads it natively,
    /// so no `--profile` flag is passed: the operator's list is the list.
    FromEnvFile(String),
    /// The tier's own profiles: `<tier>`, `<tier>-ocr`, `<tier>-vision`.
    Tier(Vec<String>),
}

/// Resolve the profiles for a tier.
///
/// `compose_profiles` is the raw value of `COMPOSE_PROFILES` from the env
/// file (`None` when the file does not set it). `embedders_one` is
/// `KB_EMBEDDERS=one`: the model services move from `<tier>-two` (a text
/// embedder, a page embedder and a reranker) to `<tier>-one` (one embedder and
/// one reranker), so exactly one stack starts.
#[must_use]
pub fn resolve_profiles(
    tier: Tier,
    compose_profiles: Option<&str>,
    embedders_one: bool,
) -> Profiles {
    match compose_profiles.map(str::trim) {
        Some(value) if !value.is_empty() => Profiles::FromEnvFile(value.to_string()),
        _ => Profiles::Tier(vec![
            tier.as_str().to_string(),
            format!("{}-ocr", tier.as_str()),
            format!("{}-vision", tier.as_str()),
            format!(
                "{}-{}",
                tier.as_str(),
                if embedders_one { "one" } else { "two" }
            ),
        ]),
    }
}

/// The `--profile` flags for a profile plan.
///
/// An env-file list produces no flags: compose reads `COMPOSE_PROFILES` from
/// the env file itself, and passing `--profile` on top would override it.
#[must_use]
pub fn profile_args(profiles: &Profiles) -> Vec<String> {
    match profiles {
        Profiles::FromEnvFile(_) => Vec::new(),
        Profiles::Tier(names) => names
            .iter()
            .flat_map(|name| vec!["--profile".to_string(), name.clone()])
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// The stack
// ---------------------------------------------------------------------------

/// The resolved stack: where the compose file is and which project it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stack {
    /// The deploy directory (compose.yaml, .env, .tier).
    pub deploy: DeployDir,
    /// The compose project name.
    pub project: String,
    /// An extra compose file layered on top (`KB_DEV_OVERRIDE`).
    pub override_file: Option<PathBuf>,
}

impl Stack {
    /// Resolve the stack from the environment and the working directory.
    #[must_use]
    pub fn resolve(lookup: &dyn Fn(&str) -> Option<String>, cwd: &Path) -> Self {
        let deploy = resolve_deploy_dir(lookup, cwd);
        let project =
            non_empty(lookup("KB_DEV_PROJECT")).unwrap_or_else(|| DEFAULT_PROJECT.to_string());
        let override_file = non_empty(lookup("KB_DEV_OVERRIDE")).map(PathBuf::from);
        Self {
            deploy,
            project,
            override_file,
        }
    }

    /// The compose file.
    #[must_use]
    pub fn compose_file(&self) -> PathBuf {
        self.deploy.path.join("compose.yaml")
    }

    /// The env file (`<deploy>/.env`).
    #[must_use]
    pub fn env_file(&self) -> PathBuf {
        self.deploy.path.join(".env")
    }

    /// The file that remembers the tier (`<deploy>/.tier`).
    #[must_use]
    pub fn tier_file(&self) -> PathBuf {
        self.deploy.path.join(".tier")
    }

    /// The env file's contents, parsed.
    #[must_use]
    pub fn read_env_file(&self) -> BTreeMap<String, String> {
        std::fs::read_to_string(self.env_file())
            .map(|text| crate::setup::parse_env(&text))
            .unwrap_or_default()
    }

    /// A setting, with the process environment first: compose's precedence.
    #[must_use]
    pub fn env_value(&self, key: &str) -> Option<String> {
        std::env::var(key)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| self.read_env_file().get(key).cloned())
    }

    /// The tier the last `up` remembered.
    #[must_use]
    pub fn tier(&self) -> Option<Tier> {
        let text = std::fs::read_to_string(self.tier_file()).ok()?;
        let name = text.lines().next()?.trim();
        if name.is_empty() {
            return None;
        }
        Tier::parse(name)
    }

    /// Remember the tier for the commands that take none (`down`, `logs`, …).
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be written.
    pub fn remember_tier(&self, tier: Tier) -> Result<(), CliError> {
        if let Some(parent) = self.tier_file().parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|err| {
                    CliError::Failed(format!("could not create {}: {err}", parent.display()))
                })?;
            }
        }
        std::fs::write(self.tier_file(), format!("{}\n", tier.as_str())).map_err(|err| {
            CliError::Failed(format!(
                "could not write {}: {err}",
                self.tier_file().display()
            ))
        })
    }

    /// The profiles a command for `tier` activates.
    #[must_use]
    pub fn profiles(&self, tier: Tier) -> Profiles {
        let from_file = self.read_env_file().get("COMPOSE_PROFILES").cloned();
        let embedders_one = self
            .read_env_file()
            .get("KB_EMBEDDERS")
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("one"));
        resolve_profiles(tier, from_file.as_deref(), embedders_one)
    }

    /// The host port of kb-server.
    #[must_use]
    pub fn api_port(&self) -> u16 {
        self.env_value("KB_PORT")
            .and_then(|value| value.trim().parse::<u16>().ok())
            .unwrap_or(DEFAULT_PORT)
    }

    /// The base URL of the running stack.
    #[must_use]
    pub fn api_url(&self) -> String {
        match self.env_value("KB_API_URL") {
            Some(url) => url.trim_end_matches('/').to_string(),
            None => format!("http://127.0.0.1:{}", self.api_port()),
        }
    }

    /// The image the kb-server service runs.
    ///
    /// `KB_IMAGE` wins (an installed bundle points it at the published
    /// image); otherwise the source-checkout name is used.
    #[must_use]
    pub fn kb_image(&self) -> String {
        if let Some(image) = self.env_value("KB_IMAGE") {
            return image;
        }
        let tag = self
            .env_value("KB_IMAGE_TAG")
            .unwrap_or_else(|| "dev".to_string());
        format!("rantai-kb-rust:{tag}")
    }

    /// The compose arguments, up to (but not including) the command.
    #[must_use]
    pub fn compose_args(&self, profiles: &Profiles) -> Vec<String> {
        let mut args = vec![
            "compose".to_string(),
            "-f".to_string(),
            self.compose_file().display().to_string(),
        ];
        if let Some(override_file) = &self.override_file {
            args.push("-f".to_string());
            args.push(override_file.display().to_string());
        }
        args.push("-p".to_string());
        args.push(self.project.clone());
        args.extend(profile_args(profiles));
        args
    }

    /// Run `docker compose …`, streaming its output.
    ///
    /// # Errors
    ///
    /// Fails when docker cannot be run; a non-zero exit becomes
    /// [`CliError::Exit`] so the child's own code reaches the shell.
    pub fn run_compose(&self, profiles: &Profiles, args: &[&str]) -> Result<(), CliError> {
        let status = Command::new("docker")
            .args(self.compose_args(profiles))
            .args(args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|err| CliError::Failed(format!("could not run docker compose: {err}")))?;
        if status.success() {
            return Ok(());
        }
        Err(CliError::Exit(exit_code(status)))
    }
}

/// A child's exit code, clamped to the 1..=255 a shell can carry.
fn exit_code(status: std::process::ExitStatus) -> u8 {
    let code = status.code().unwrap_or(1);
    u8::try_from(code.clamp(1, 255)).unwrap_or(1)
}

// ---------------------------------------------------------------------------
// Docker
// ---------------------------------------------------------------------------

/// The compose version, as `(major, minor)`.
///
/// Accepts `v2.24.6`, `2.24.6` and `Docker Compose version v2.24.6`.
#[must_use]
pub fn parse_compose_version(text: &str) -> Option<(u32, u32)> {
    for token in text.split_whitespace() {
        let token = token.trim_start_matches('v');
        if !token.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        let mut parts = token.split('.');
        let Some(major) = parts.next().and_then(|part| part.parse::<u32>().ok()) else {
            continue;
        };
        let minor = parts
            .next()
            .and_then(|part| part.parse::<u32>().ok())
            .unwrap_or(0);
        return Some((major, minor));
    }
    None
}

/// Whether a compose version understands `depends_on.required: false`.
#[must_use]
pub fn compose_version_supported(text: &str) -> bool {
    let Some((major, minor)) = parse_compose_version(text) else {
        return false;
    };
    major > MIN_COMPOSE.0 || (major == MIN_COMPOSE.0 && minor >= MIN_COMPOSE.1)
}

/// The docker/compose check every stack command starts with.
///
/// # Errors
///
/// Fails with a message that names the missing piece: docker, the compose
/// plugin, a compose that is too old, or the compose file itself.
pub fn require_docker(stack: &Stack) -> Result<(), CliError> {
    let output = Command::new("docker")
        .args(["compose", "version"])
        .output()
        .map_err(|_| {
            CliError::Failed(
                "docker is not installed (or not on PATH). The stack runs in containers: \
                 install Docker Desktop or docker-ce, then run this command again."
                    .to_string(),
            )
        })?;
    if !output.status.success() {
        return Err(CliError::Failed(
            "docker compose v2 is required (the 'docker compose' plugin). \
             `docker compose version` failed."
                .to_string(),
        ));
    }
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !compose_version_supported(&text) {
        return Err(CliError::Failed(format!(
            "docker compose {} is too old: this stack needs compose >= {}.{} \
             (`depends_on.required: false` landed in 2.20).",
            text.trim(),
            MIN_COMPOSE.0,
            MIN_COMPOSE.1
        )));
    }
    let compose_file = stack.compose_file();
    if !compose_file.is_file() {
        return Err(CliError::Failed(format!(
            "missing {} — run `{} setup <tier>` first, or point RANTAI_KB_HOME at \
             the directory that holds deploy/",
            compose_file.display(),
            crate::cli::program_name()
        )));
    }
    Ok(())
}

/// The services of this project that are running, by service name.
///
/// The list is what `check_no_other_tier` needs: a second tier on the same
/// project would fight over the host port and the volumes.
fn running_services(stack: &Stack) -> Vec<String> {
    let Ok(output) = Command::new("docker")
        .args([
            "ps",
            "--filter",
            &format!("label=com.docker.compose.project={}", stack.project),
            "--format",
            "{{.Label \"com.docker.compose.service\"}}",
        ])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("kb-"))
        .map(str::to_string)
        .collect()
}

/// Refuse a second tier on the same project.
///
/// # Errors
///
/// Fails when another tier's stack is running.
fn check_no_other_tier(stack: &Stack, tier: Tier) -> Result<(), CliError> {
    let mine = format!("kb-{}", tier.as_str());
    for service in running_services(stack) {
        if service != mine {
            return Err(CliError::Failed(format!(
                "the '{service}' stack is already running; run `{} down` first \
                 (or `down -v` to also wipe the data)",
                crate::cli::program_name()
            )));
        }
    }
    Ok(())
}

/// Whether an image is in the local docker cache.
fn image_exists(image: &str) -> bool {
    Command::new("docker")
        .args(["image", "inspect", image])
        .output()
        .is_ok_and(|output| output.status.success())
}

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

/// Wait for `/health` to answer, up to `seconds`.
///
/// # Errors
///
/// Fails when the deadline passes without a healthy answer.
pub async fn wait_healthy(url: &str, seconds: u64) -> Result<(), CliError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|err| CliError::Failed(format!("HTTP client: {err}")))?;
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut reported = false;
    loop {
        let healthy = match client.get(format!("{url}/health")).send().await {
            Ok(response) => response.status().is_success(),
            Err(_) => false,
        };
        if healthy {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(CliError::Failed(format!(
                "kb-server did not become healthy at {url} within {seconds}s"
            )));
        }
        if !reported {
            reported = true;
            println!(
                "waiting for {url}/health (first start downloads the model; up to {seconds}s)"
            );
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

/// `kb-server up [tier] [--build]`.
pub async fn up(args: &[String]) -> Result<(), CliError> {
    let (tier_arg, build) = parse_up_args(args)?;
    let stack = current();
    require_docker(&stack)?;

    let tier = match tier_arg {
        Some(name) => Tier::parse(&name).ok_or_else(|| {
            CliError::Usage(format!(
                "unknown tier {name:?} (choose: lite, lite-en, desk, studio, station, cloud)"
            ))
        })?,
        None => stack.tier().ok_or_else(|| {
            CliError::Failed(format!(
                "no tier recorded in {} — run `{} setup <tier>` first",
                stack.tier_file().display(),
                crate::cli::program_name()
            ))
        })?,
    };

    if tier == Tier::Cloud && stack.env_value("OPENROUTER_API_KEY").is_none() {
        return Err(CliError::Failed(format!(
            "KB_TIER=cloud needs OPENROUTER_API_KEY — put it in {} (copy .env.example \
             first) or export it. Cloud embeds and reranks at OpenRouter, so it cannot \
             run without a paid key.",
            stack.env_file().display()
        )));
    }

    check_no_other_tier(&stack, tier)?;
    stack.remember_tier(tier)?;

    let profiles = stack.profiles(tier);
    let image = stack.kb_image();

    // A source checkout builds the image when it is missing; an installed
    // bundle cannot build (there is no source), so it pulls the published one.
    let needs_build = build || (is_source_checkout(&stack.deploy.path) && !image_exists(&image));
    if needs_build {
        if !is_source_checkout(&stack.deploy.path) {
            return Err(CliError::Failed(
                "this install has no source to build from — drop --build, or set KB_IMAGE \
                 to a published image."
                    .to_string(),
            ));
        }
        println!(
            "building the {} image{}",
            image,
            if build { "" } else { " (not found locally)" }
        );
        stack.run_compose(&profiles, &["build"])?;
    }

    if !is_source_checkout(&stack.deploy.path) {
        // Best effort: an image that cannot be pulled (a local-only sidecar)
        // must not stop the ones that can.
        let _ = stack.run_compose(&profiles, &["pull", "--ignore-pull-failures"]);
    }

    stack.run_compose(&profiles, &["up", "-d", "--no-build"])?;

    let url = stack.api_url();
    let seconds = stack
        .env_value("KB_WAIT_SECONDS")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_WAIT_SECONDS);
    if let Err(err) = wait_healthy(&url, seconds).await {
        let _ = stack.run_compose(&profiles, &["ps"]);
        let _ = stack.run_compose(
            &profiles,
            &["logs", "--tail=40", &format!("kb-{}", tier.as_str())],
        );
        return Err(err);
    }

    print_up_summary(&stack, tier, &url, serves_docs(&url).await);
    Ok(())
}

/// Whether the running server serves an OpenAPI page at `/docs`.
///
/// The summary must not point at a URL that answers "Unauthorized": the
/// OpenAPI work (`/docs`, `/openapi.json`) is not in every build, so the line
/// is printed only when the page is really there.
async fn serves_docs(url: &str) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    else {
        return false;
    };
    client
        .get(format!("{url}/docs"))
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}

/// The arguments of `up`: an optional tier and `--build`.
fn parse_up_args(args: &[String]) -> Result<(Option<String>, bool), CliError> {
    let mut tier: Option<String> = None;
    let mut build = false;
    for arg in args {
        match arg.as_str() {
            "--build" => build = true,
            other if other.starts_with('-') => {
                return Err(CliError::Usage(format!("up: unknown option {other:?}")));
            }
            other => {
                if tier.is_some() {
                    return Err(CliError::Usage(format!(
                        "up: unexpected argument {other:?}; usage: up [tier] [--build]"
                    )));
                }
                tier = Some(other.to_string());
            }
        }
    }
    Ok((tier, build))
}

/// What a successful `up` prints.
fn print_up_summary(stack: &Stack, tier: Tier, url: &str, docs: bool) {
    let program = crate::cli::program_name();
    println!();
    for line in summary_lines(&program, tier, url, docs, &stack.env_file()) {
        println!("{line}");
    }
}

/// The lines of the `up` summary.
///
/// The Swagger line is printed only when the running server really serves
/// `/docs`: the summary is the operator's map of the stack, and a URL that
/// answers "Unauthorized" is worse than no line at all.
fn summary_lines(program: &str, tier: Tier, url: &str, docs: bool, config: &Path) -> Vec<String> {
    let mut lines = vec![
        format!("{program} ({}) is up.", tier.as_str()),
        format!("  API        {url}"),
        format!("  health     {url}/health"),
    ];
    if docs {
        lines.push(format!("  Swagger UI {url}/docs"));
    }
    lines.push(format!("  API key    {program} key my-tenant"));
    lines.push(format!(
        "  upload     curl -H \"Authorization: Bearer <key>\" -F file=@doc.pdf {url}/v1/documents"
    ));
    lines.push(format!("  logs       {program} logs"));
    lines.push(format!(
        "  stop       {program} down        (down -v also wipes data)"
    ));
    lines.push(format!("  config     {}", config.display()));
    lines
}

/// `kb-server down [-v|--volumes]`.
pub async fn down(args: &[String]) -> Result<(), CliError> {
    let (volumes, tier_arg) = parse_down_args(args)?;
    let stack = current();
    require_docker(&stack)?;

    let tier = resolve_remembered_tier(&stack, tier_arg)?;
    let profiles = stack.profiles(tier);
    if volumes {
        println!(
            "removing the {} stack including its volumes (this deletes data)",
            tier.as_str()
        );
        stack.run_compose(&profiles, &["down", "--volumes"])?;
    } else {
        stack.run_compose(&profiles, &["down"])?;
        println!(
            "stopped {}; data volumes kept (use `down -v` to delete them)",
            tier.as_str()
        );
    }
    Ok(())
}

/// The arguments of `down`: `-v` and an optional tier.
fn parse_down_args(args: &[String]) -> Result<(bool, Option<String>), CliError> {
    let mut volumes = false;
    let mut tier: Option<String> = None;
    for arg in args {
        match arg.as_str() {
            "-v" | "--volumes" => volumes = true,
            other if other.starts_with('-') => {
                return Err(CliError::Usage(format!("down: unknown option {other:?}")));
            }
            other => tier = Some(other.to_string()),
        }
    }
    Ok((volumes, tier))
}

/// The tier a command uses: the argument, else the remembered one.
///
/// # Errors
///
/// Fails when neither is available, or when the argument names no tier.
fn resolve_remembered_tier(stack: &Stack, tier_arg: Option<String>) -> Result<Tier, CliError> {
    let tier = match tier_arg {
        Some(name) => Tier::parse(&name).ok_or_else(|| {
            CliError::Usage(format!(
                "unknown tier {name:?} (choose: lite, lite-en, desk, studio, station, cloud)"
            ))
        })?,
        None => stack.tier().ok_or_else(|| {
            CliError::Failed(format!(
                "no tier recorded in {} — run `{} setup <tier>` first",
                stack.tier_file().display(),
                crate::cli::program_name()
            ))
        })?,
    };
    stack.remember_tier(tier)?;
    Ok(tier)
}

/// `kb-server logs [--no-follow] [service...]`.
pub async fn logs(args: &[String]) -> Result<(), CliError> {
    let (follow, services) = parse_logs_args(args)?;
    let stack = current();
    require_docker(&stack)?;

    let tier = resolve_remembered_tier(&stack, None)?;
    let profiles = stack.profiles(tier);
    let tail = stack
        .env_value("KB_LOG_TAIL")
        .unwrap_or_else(|| "200".to_string());
    let mut compose_args: Vec<String> = vec!["logs".to_string(), format!("--tail={tail}")];
    if follow {
        compose_args.push("--follow".to_string());
    }
    for service in &services {
        compose_args.push(service.clone());
    }
    let refs: Vec<&str> = compose_args.iter().map(String::as_str).collect();
    stack.run_compose(&profiles, &refs)
}

/// The arguments of `logs`: the follow switch and the service names.
fn parse_logs_args(args: &[String]) -> Result<(bool, Vec<String>), CliError> {
    let mut follow = true;
    let mut services = Vec::new();
    for arg in args {
        match arg.as_str() {
            "-f" | "--follow" => follow = true,
            "--no-follow" => follow = false,
            other if other.starts_with('-') => {
                return Err(CliError::Usage(format!("logs: unknown option {other:?}")));
            }
            other => services.push(other.to_string()),
        }
    }
    Ok((follow, services))
}

/// `kb-server ps`.
pub async fn ps(_args: &[String]) -> Result<(), CliError> {
    let stack = current();
    require_docker(&stack)?;

    let tier = resolve_remembered_tier(&stack, None)?;
    let profiles = stack.profiles(tier);
    println!("profile: {}", tier.as_str());
    stack.run_compose(&profiles, &["ps"])
}

/// `kb-server status` — `GET /health` on the running stack.
pub async fn status(_args: &[String]) -> Result<(), CliError> {
    let stack = current();
    let url = stack.api_url();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|err| CliError::Failed(format!("HTTP client: {err}")))?;
    match client.get(format!("{url}/health")).send().await {
        Ok(response) if response.status().is_success() => {
            let body = response.text().await.unwrap_or_default();
            println!("{body}");
            Ok(())
        }
        Ok(response) => {
            eprintln!(
                "kb-server at {url} is not healthy (HTTP {})",
                response.status()
            );
            Err(CliError::Failed(format!("no healthy kb-server at {url}")))
        }
        Err(err) => {
            eprintln!("kb-server at {url} is not reachable: {err}");
            Err(CliError::Failed(format!("no healthy kb-server at {url}")))
        }
    }
}

/// `kb-server key <tenant> [name]` — an API key, made inside the container.
///
/// The key is created by the running server (it writes to Postgres), so the
/// command execs `kb-server create-api-key` in the `kb-<tier>` container
/// instead of talking to the API from the host.
pub async fn key(args: &[String]) -> Result<(), CliError> {
    let (tenant, name) = parse_key_args(args)?;
    let stack = current();
    require_docker(&stack)?;

    let tier = resolve_remembered_tier(&stack, None)?;
    let profiles = stack.profiles(tier);
    let service = format!("kb-{}", tier.as_str());
    let mut compose_args: Vec<String> = vec![
        "exec".to_string(),
        "-T".to_string(),
        service.clone(),
        "kb-server".to_string(),
        "create-api-key".to_string(),
        tenant.clone(),
    ];
    if let Some(name) = &name {
        compose_args.push(name.clone());
    }
    let refs: Vec<&str> = compose_args.iter().map(String::as_str).collect();
    stack.run_compose(&profiles, &refs)
}

/// The arguments of `key`: the tenant and an optional name.
fn parse_key_args(args: &[String]) -> Result<(String, Option<String>), CliError> {
    let mut positionals: Vec<&String> = args.iter().filter(|arg| !arg.starts_with('-')).collect();
    let tenant = positionals
        .first()
        .map(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| CliError::Usage("usage: key <tenant> [name]".to_string()))?
        .to_string();
    positionals.remove(0);
    let name = positionals
        .first()
        .map(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    Ok((tenant, name))
}

// ---------------------------------------------------------------------------
// Serve
// ---------------------------------------------------------------------------

/// What `serve` should do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServeMode {
    /// Run the in-process server: `DATABASE_URL` is set, so this process *is*
    /// the server (the container's CMD).
    Foreground,
    /// Bring the compose stack up and wait for it: a configured deploy
    /// directory, no `DATABASE_URL`.
    Stack,
    /// Neither: the operator has to run `setup` first.
    Unconfigured,
}

/// Decide what `serve` means.
///
/// `DATABASE_URL` wins: a container (or an operator who configured Postgres)
/// wants the in-process server, exactly as before. Without it, a configured
/// deploy directory means "serve the stack".
#[must_use]
pub fn decide_serve_mode(has_database_url: bool, has_configured_deploy: bool) -> ServeMode {
    if has_database_url {
        ServeMode::Foreground
    } else if has_configured_deploy {
        ServeMode::Stack
    } else {
        ServeMode::Unconfigured
    }
}

/// Whether the deploy directory is configured: it has a `.tier` file, so a
/// tier was chosen and the compose file is the one to run.
#[must_use]
pub fn is_configured(stack: &Stack) -> bool {
    stack.tier_file().is_file()
}

/// Serve the stack: `up`, then the URLs.
///
/// # Errors
///
/// Fails when the stack cannot be started or never becomes healthy.
pub async fn serve_stack() -> Result<(), CliError> {
    up(&[]).await?;
    let stack = current();
    let url = stack.api_url();
    println!();
    println!(
        "{program} is serving the stack.",
        program = crate::cli::program_name()
    );
    println!("  API        {url}");
    println!("  health     {url}/health");
    if serves_docs(&url).await {
        println!("  Swagger UI {url}/docs");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// The process environment.
fn env_lookup(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

/// The working directory.
fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// The stack of this process: the real environment, the real working
/// directory.
#[must_use]
pub fn current() -> Stack {
    Stack::resolve(&env_lookup, &cwd())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|value| (*value).to_string()).collect()
    }

    /// A lookup over a table, so a test never touches the real environment.
    fn lookup_from(table: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = table
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        move |key: &str| {
            owned
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kb-stack-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    // ─── deploy directory ───────────────────────────────────────────────────

    #[test]
    fn a_named_home_wins_over_the_checkout() {
        let home = temp_dir("home");
        std::fs::create_dir_all(home.join("deploy")).expect("deploy dir");
        let cwd = temp_dir("cwd-home");
        std::fs::create_dir_all(cwd.join("rust/deploy")).expect("checkout");

        let resolved = resolve_deploy_dir(
            &lookup_from(&[("RANTAI_KB_HOME", home.to_str().unwrap())]),
            &cwd,
        );
        assert_eq!(resolved.path, home.join("deploy"));
        assert_eq!(resolved.kind, DeployKind::Home);
    }

    #[test]
    fn a_source_checkout_wins_over_the_installed_home() {
        let cwd = temp_dir("cwd-checkout");
        std::fs::create_dir_all(cwd.join("rust/deploy")).expect("checkout");
        let home = temp_dir("home-checkout");

        let resolved = resolve_deploy_dir(&lookup_from(&[("HOME", home.to_str().unwrap())]), &cwd);
        assert_eq!(resolved.path, cwd.join("rust/deploy"));
        assert_eq!(resolved.kind, DeployKind::Checkout);
    }

    #[test]
    fn a_deploy_directory_next_to_the_working_directory_is_found() {
        let cwd = temp_dir("cwd-deploy");
        std::fs::create_dir_all(cwd.join("deploy")).expect("deploy dir");
        let home = temp_dir("home-deploy");

        let resolved = resolve_deploy_dir(&lookup_from(&[("HOME", home.to_str().unwrap())]), &cwd);
        assert_eq!(resolved.path, cwd.join("deploy"));
        assert_eq!(resolved.kind, DeployKind::Checkout);
    }

    #[test]
    fn without_a_checkout_the_installed_home_is_used() {
        let cwd = temp_dir("cwd-installed");
        let home = temp_dir("home-installed");

        let resolved = resolve_deploy_dir(&lookup_from(&[("HOME", home.to_str().unwrap())]), &cwd);
        assert_eq!(resolved.path, home.join(".rantai-kb/deploy"));
        assert_eq!(resolved.kind, DeployKind::Installed);
    }

    #[test]
    fn a_source_checkout_is_told_apart_from_an_installed_bundle() {
        let checkout = temp_dir("is-checkout");
        std::fs::create_dir_all(checkout.join("crates")).expect("crates");
        assert!(is_source_checkout(&checkout.join("deploy")));

        let bundle = temp_dir("is-bundle");
        assert!(!is_source_checkout(&bundle.join("deploy")));
    }

    // ─── profiles ───────────────────────────────────────────────────────────

    #[test]
    fn without_compose_profiles_the_tier_owns_three_profiles() {
        let profiles = resolve_profiles(Tier::Lite, None, false);
        assert_eq!(
            profiles,
            Profiles::Tier(vec![
                "lite".to_string(),
                "lite-ocr".to_string(),
                "lite-vision".to_string()
            ])
        );
        assert_eq!(
            profile_args(&profiles),
            vec![
                "--profile",
                "lite",
                "--profile",
                "lite-ocr",
                "--profile",
                "lite-vision"
            ]
        );
    }

    #[test]
    fn compose_profiles_in_the_env_file_win_and_add_no_flag() {
        let profiles = resolve_profiles(Tier::Studio, Some("lite,ocr"), false);
        assert_eq!(profiles, Profiles::FromEnvFile("lite,ocr".to_string()));
        // Compose reads COMPOSE_PROFILES itself; a --profile flag would
        // override the operator's list.
        assert!(profile_args(&profiles).is_empty());
    }

    #[test]
    fn an_empty_compose_profiles_is_not_a_list() {
        let profiles = resolve_profiles(Tier::Desk, Some("   "), false);
        assert!(matches!(profiles, Profiles::Tier(_)));
    }

    #[test]
    fn every_tier_names_its_own_profiles() {
        for name in crate::tier::TIER_NAMES {
            let tier = Tier::parse(name).expect("known tier");
            let Profiles::Tier(profiles) = resolve_profiles(tier, None, false) else {
                panic!("{name} should own its profiles");
            };
            assert_eq!(profiles[0], name);
            assert_eq!(profiles[1], format!("{name}-ocr"));
            assert_eq!(profiles[2], format!("{name}-vision"));
        }
    }

    // ─── compose version ────────────────────────────────────────────────────

    #[test]
    fn the_compose_version_is_parsed_from_every_shape() {
        assert_eq!(parse_compose_version("v2.24.6"), Some((2, 24)));
        assert_eq!(parse_compose_version("2.24.6"), Some((2, 24)));
        assert_eq!(
            parse_compose_version("Docker Compose version v2.20.0"),
            Some((2, 20))
        );
        assert_eq!(parse_compose_version("no version here"), None);
    }

    #[test]
    fn compose_2_20_is_the_floor() {
        assert!(compose_version_supported("v2.20.0"));
        assert!(compose_version_supported("v2.29.1"));
        assert!(compose_version_supported("v3.0.0"));
        assert!(!compose_version_supported("v2.19.3"));
        assert!(!compose_version_supported("v1.29.2"));
        assert!(!compose_version_supported("nonsense"));
    }

    // ─── serve mode ─────────────────────────────────────────────────────────

    #[test]
    fn a_database_url_means_the_in_process_server() {
        assert_eq!(
            decide_serve_mode(true, true),
            ServeMode::Foreground,
            "the container's CMD runs `serve` with DATABASE_URL set"
        );
        assert_eq!(decide_serve_mode(true, false), ServeMode::Foreground);
    }

    #[test]
    fn without_a_database_url_a_configured_deploy_serves_the_stack() {
        assert_eq!(decide_serve_mode(false, true), ServeMode::Stack);
    }

    #[test]
    fn without_a_database_url_and_without_a_deploy_it_refuses() {
        assert_eq!(decide_serve_mode(false, false), ServeMode::Unconfigured);
    }

    // ─── argument parsing ───────────────────────────────────────────────────

    #[test]
    fn up_takes_an_optional_tier_and_a_build_flag() {
        assert_eq!(
            parse_up_args(&args(&["lite", "--build"])).expect("parse"),
            (Some("lite".to_string()), true)
        );
        assert_eq!(parse_up_args(&args(&[])).expect("parse"), (None, false));
        assert!(parse_up_args(&args(&["--nope"])).is_err());
        assert!(parse_up_args(&args(&["lite", "desk"])).is_err());
    }

    #[test]
    fn down_takes_a_volumes_flag() {
        assert_eq!(
            parse_down_args(&args(&["-v"])).expect("parse"),
            (true, None)
        );
        assert_eq!(
            parse_down_args(&args(&["--volumes", "lite"])).expect("parse"),
            (true, Some("lite".to_string()))
        );
        assert!(parse_down_args(&args(&["--nope"])).is_err());
    }

    #[test]
    fn logs_follow_by_default_and_take_service_names() {
        assert_eq!(
            parse_logs_args(&args(&[])).expect("parse"),
            (true, Vec::new())
        );
        assert_eq!(
            parse_logs_args(&args(&["--no-follow", "kb-lite"])).expect("parse"),
            (false, vec!["kb-lite".to_string()])
        );
    }

    #[test]
    fn key_takes_a_tenant_and_an_optional_name() {
        assert_eq!(
            parse_key_args(&args(&["acme"])).expect("parse"),
            ("acme".to_string(), None)
        );
        assert_eq!(
            parse_key_args(&args(&["acme", "ci-key"])).expect("parse"),
            ("acme".to_string(), Some("ci-key".to_string()))
        );
        assert!(parse_key_args(&args(&[])).is_err());
    }

    // ─── the summary ────────────────────────────────────────────────────────

    #[test]
    fn the_summary_names_the_api_the_health_url_and_the_key_hint() {
        let lines = summary_lines(
            "rantai-kb",
            Tier::Lite,
            "http://127.0.0.1:8080",
            false,
            Path::new("/home/op/deploy/.env"),
        );
        assert_eq!(lines[0], "rantai-kb (lite) is up.");
        assert!(
            lines
                .iter()
                .any(|line| line.contains("http://127.0.0.1:8080"))
        );
        assert!(lines.iter().any(|line| line.contains("/health")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("rantai-kb key my-tenant"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("/home/op/deploy/.env"))
        );
    }

    #[test]
    fn the_swagger_line_appears_only_when_the_server_serves_the_page() {
        let without = summary_lines(
            "kb-server",
            Tier::Lite,
            "http://127.0.0.1:8080",
            false,
            Path::new("/deploy/.env"),
        );
        assert!(!without.iter().any(|line| line.contains("Swagger")));
        let with = summary_lines(
            "kb-server",
            Tier::Lite,
            "http://127.0.0.1:8080",
            true,
            Path::new("/deploy/.env"),
        );
        assert!(
            with.iter()
                .any(|line| line.contains("Swagger UI http://127.0.0.1:8080/docs"))
        );
    }
}
