//! The layering rule, checked against this workspace rather than a fixture.
//!
//! The unit tests in `kb_server::boundary` cover the parsing; this file runs
//! the same check over the real `cargo metadata` of `rust/`, which is the
//! thing CI has to fail on when someone adds a forbidden dependency.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use kb_server::boundary;

#[test]
fn no_inner_crate_depends_on_an_adapter() {
    let root = boundary::workspace_root();
    let violations = boundary::check(&root).expect("cargo metadata");
    assert!(
        violations.is_empty(),
        "KB boundary violated:\n  {}",
        violations
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

#[test]
fn the_boundary_sets_cover_every_workspace_crate() {
    // A crate that is in neither set is unchecked by the rule above, so the
    // two lists together must account for every member.
    let metadata =
        std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .arg("metadata")
            .arg("--format-version")
            .arg("1")
            .arg("--no-deps")
            .arg("--offline")
            .current_dir(boundary::workspace_root())
            .output()
            .expect("cargo metadata");
    assert!(metadata.status.success());

    let json: serde_json::Value = serde_json::from_slice(&metadata.stdout).expect("metadata json");
    let names: Vec<&str> = json["workspace_members"]
        .as_array()
        .expect("members")
        .iter()
        .filter_map(|id| id.as_str())
        .filter_map(|id| {
            json["packages"]
                .as_array()?
                .iter()
                .find(|p| p["id"].as_str() == Some(id))
                .and_then(|p| p["name"].as_str())
        })
        .collect();

    for name in &names {
        assert!(
            boundary::INNER_CRATES.contains(name) || boundary::ADAPTER_CRATES.contains(name),
            "{name} is in neither the inner nor the adapter list, so the boundary check ignores it"
        );
    }
}
