//! The layering rule, executable.
//!
//! `scripts/check-kb-boundary.ts` fails when the TypeScript engine imports
//! service infrastructure. The Rust equivalent is stronger and simpler: the
//! Cargo graph cannot express the forbidden edge without someone adding it to
//! a manifest, so this check reads `cargo metadata` and fails if one ever is.
//!
//! ```text
//! kb-core  ←  kb-ports  ←  kb-inference · kb-store-pg · kb-store-surreal · kb-store-blob
//!                       ←  kb-extract
//!                       ←  kb-engine  ←  kb-server (composition root)
//! ```
//!
//! What it buys is the property the import-boundary lint buys in TypeScript:
//! the domain crates stay runnable — and testable — without a database, an
//! inference provider or an HTTP server anywhere in sight.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

/// Crates that must not know an adapter exists.
pub const INNER_CRATES: &[&str] = &["kb-core", "kb-ports", "kb-extract", "kb-engine"];

/// Crates that are allowed to depend on the outside world, and therefore are
/// the ones an inner crate may never name.
pub const ADAPTER_CRATES: &[&str] = &[
    "kb-inference",
    "kb-store-pg",
    "kb-store-surreal",
    "kb-store-blob",
    "kb-server",
];

/// One forbidden edge, with the manifest it appears in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub crate_name: String,
    pub dependency: String,
    pub manifest_path: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} → {}  ({})",
            self.crate_name, self.dependency, self.manifest_path
        )
    }
}

#[derive(Debug, Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    #[serde(default)]
    workspace_members: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Package {
    name: String,
    id: String,
    #[serde(default)]
    dependencies: Vec<Dependency>,
    #[serde(default)]
    manifest_path: String,
}

#[derive(Debug, Deserialize)]
struct Dependency {
    /// The *package* name, which stays the package name even when the
    /// dependency is renamed in the manifest.
    name: String,
}

/// Root of the Rust workspace, derived from this crate's manifest location.
#[must_use]
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// Run the check against the workspace at `root`.
///
/// `cargo metadata` is asked for `--no-deps` so the check reads the manifests
/// directly and needs neither a resolved graph nor a network: a path dependency
/// that does not exist cannot compile anyway, so the manifests are the whole
/// truth about who depends on whom.
///
/// # Errors
///
/// Returns a message naming the failure when cargo cannot be run or its output
/// is not understood — a broken check must say so rather than pass silently.
pub fn check(root: &Path) -> Result<Vec<Violation>, String> {
    let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string()))
        .arg("metadata")
        .arg("--format-version")
        .arg("1")
        .arg("--no-deps")
        .arg("--offline")
        .current_dir(root)
        .output()
        .map_err(|e| format!("could not run cargo metadata in {}: {e}", root.display()))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed in {}: {}",
            root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    parse_metadata(&String::from_utf8_lossy(&output.stdout))
}

/// The forbidden edges in a `cargo metadata` document.
///
/// # Errors
///
/// Fails when the document is not the metadata this check expects.
pub fn parse_metadata(json: &str) -> Result<Vec<Violation>, String> {
    let metadata: Metadata =
        serde_json::from_str(json).map_err(|e| format!("cargo metadata is not readable: {e}"))?;

    let mut violations = Vec::new();
    for member_id in &metadata.workspace_members {
        let Some(package) = metadata.packages.iter().find(|p| &p.id == member_id) else {
            continue;
        };
        if !INNER_CRATES.contains(&package.name.as_str()) {
            continue;
        }
        for dependency in &package.dependencies {
            if ADAPTER_CRATES.contains(&dependency.name.as_str()) {
                violations.push(Violation {
                    crate_name: package.name.clone(),
                    dependency: dependency.name.clone(),
                    manifest_path: package.manifest_path.clone(),
                });
            }
        }
    }
    Ok(violations)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    fn metadata_json(packages: serde_json::Value, members: serde_json::Value) -> String {
        json!({ "packages": packages, "workspace_members": members }).to_string()
    }

    fn package(name: &str, deps: &[&str]) -> serde_json::Value {
        json!({
            "name": name,
            "id": format!("path+file:///w/{name}#{name}@0.1.0"),
            "manifest_path": format!("/w/crates/{name}/Cargo.toml"),
            "dependencies": deps
                .iter()
                .map(|d| json!({ "name": d, "req": "*", "kind": null }))
                .collect::<Vec<_>>(),
        })
    }

    #[test]
    fn a_clean_graph_has_no_violations() {
        let json = metadata_json(
            json!([
                package("kb-core", &["serde"]),
                package("kb-ports", &["kb-core", "async-trait"]),
                package("kb-engine", &["kb-ports", "kb-core", "tokio"]),
                package("kb-store-pg", &["kb-ports", "sqlx"]),
                package("kb-server", &["kb-engine", "kb-store-pg", "axum"]),
            ]),
            json!([
                "path+file:///w/kb-core#kb-core@0.1.0",
                "path+file:///w/kb-ports#kb-ports@0.1.0",
                "path+file:///w/kb-engine#kb-engine@0.1.0",
                "path+file:///w/kb-store-pg#kb-store-pg@0.1.0",
                "path+file:///w/kb-server#kb-server@0.1.0",
            ]),
        );
        assert!(parse_metadata(&json).expect("parse").is_empty());
    }

    #[test]
    fn an_engine_that_imports_an_adapter_is_reported() {
        let json = metadata_json(
            json!([
                package("kb-engine", &["kb-ports", "kb-store-pg"]),
                package("kb-store-pg", &["sqlx"]),
            ]),
            json!([
                "path+file:///w/kb-engine#kb-engine@0.1.0",
                "path+file:///w/kb-store-pg#kb-store-pg@0.1.0",
            ]),
        );
        let violations = parse_metadata(&json).expect("parse");
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].crate_name, "kb-engine");
        assert_eq!(violations[0].dependency, "kb-store-pg");
        assert!(
            violations[0]
                .manifest_path
                .ends_with("kb-engine/Cargo.toml")
        );
    }

    #[test]
    fn every_adapter_is_treated_as_forbidden() {
        for adapter in ADAPTER_CRATES {
            let json = metadata_json(
                json!([package("kb-core", &[adapter])]),
                json!(["path+file:///w/kb-core#kb-core@0.1.0"]),
            );
            let violations = parse_metadata(&json).expect("parse");
            assert_eq!(violations.len(), 1, "{adapter} was allowed");
            assert_eq!(violations[0].dependency, *adapter);
        }
    }

    #[test]
    fn adapters_may_depend_on_each_other() {
        let json = metadata_json(
            json!([
                package("kb-server", &["kb-engine", "kb-store-pg"]),
                package("kb-store-pg", &["kb-ports"]),
            ]),
            json!([
                "path+file:///w/kb-server#kb-server@0.1.0",
                "path+file:///w/kb-store-pg#kb-store-pg@0.1.0",
            ]),
        );
        assert!(parse_metadata(&json).expect("parse").is_empty());
    }

    #[test]
    fn unreadable_metadata_is_an_error_not_a_pass() {
        assert!(parse_metadata("{}").is_err());
        assert!(parse_metadata("not json").is_err());
    }

    #[test]
    fn the_workspace_root_is_the_rust_directory() {
        assert!(workspace_root().join("Cargo.toml").is_file());
        assert!(workspace_root().join("crates").is_dir());
    }
}
