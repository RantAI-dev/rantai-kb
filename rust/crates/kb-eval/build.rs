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
//! A source tree without git, or a build with `--offline` in a tarball, leaves
//! it unset and the report says `unknown` rather than inventing a value.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    if manifest_dir.is_empty() {
        return;
    }
    let Ok(output) = Command::new("git")
        .args(["-C", &manifest_dir, "rev-parse", "--short", "HEAD"])
        .output()
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if commit.is_empty() {
        return;
    }
    println!("cargo:rustc-env=KB_EVAL_HARNESS_COMMIT={commit}");
}
