//! Bake the harness's own git commit into the binary.
//!
//! Every number `kb-eval` writes has to say which code produced it. Reading the
//! working directory at run time does not answer that: the harness is invoked
//! from `/tmp` in CI, from the repo root by hand, and from a shell whose `cwd`
//! is someone else's repository — and a `git rev-parse` there reports a commit
//! that has nothing to do with the harness.
//!
//! So the commit is resolved once, at build time, from the crate's own
//! directory, and read back through `option_env!("KB_EVAL_HARNESS_COMMIT")`.
//! A source tree without git, or a build from a tarball, leaves it unset and
//! the report says `unknown` rather than inventing a value.
//!
//! A tree with uncommitted changes records `<sha>-dirty`. Without that marker a
//! run from a modified checkout would be attributed to a commit it is not,
//! which is the one thing provenance exists to prevent.
//!
//! `rerun-if-changed` covers the whole crate, not just this file, so the commit
//! is refreshed whenever the harness itself is recompiled — otherwise the first
//! build would pin a commit for the life of the target directory.

use std::process::Command;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    if manifest_dir.is_empty() {
        println!("cargo:rerun-if-changed=build.rs");
        return;
    }
    println!("cargo:rerun-if-changed={manifest_dir}");
    println!("cargo:rerun-if-changed=build.rs");

    let git = |args: &[&str]| -> Option<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&manifest_dir)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    };

    let Some(commit) = git(&["rev-parse", "--short", "HEAD"]) else {
        return;
    };
    if commit.is_empty() {
        return;
    }
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
        .is_some_and(|status| !status.is_empty());
    let commit = if dirty {
        format!("{commit}-dirty")
    } else {
        commit
    };
    println!("cargo:rustc-env=KB_EVAL_HARNESS_COMMIT={commit}");
}
