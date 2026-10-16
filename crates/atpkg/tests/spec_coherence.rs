// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The shipped pack surface, held: the root `Cargo.toml`'s `[workspace.metadata.atpkg]`
//! is what a pack of aterm exposes on every install's PATH.
//!
//! The program table this file once held to the hand-kept spec is the producer's
//! `atpkg_publish::programs::PROGRAMS` now (its invariants are that crate's
//! `tests/shared/programs.rs`), and the pins live only in the signed index.

use std::path::{Path, PathBuf};

/// `crates/atpkg` -> the workspace root, verified by its manifest.
fn repo_root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("CARGO_MANIFEST_DIR has a workspace root two levels up")
        .to_path_buf();
    assert!(
        root.join("Cargo.toml").is_file(),
        "{} has no Cargo.toml — if the crate moved, follow it here",
        root.display()
    );
    root
}

/// The root Cargo.toml's `[workspace.metadata.atpkg]` still ships ONE command. `expose`
/// is the PATH surface every install gets (one-binary collapse: everything else is an
/// argv0 symlink).
#[test]
fn workspace_metadata_pins_the_shipped_surface() {
    let root = repo_root();
    let manifest: aterm_toml::Value = std::fs::read_to_string(root.join("Cargo.toml"))
        .expect("read the root Cargo.toml")
        .parse()
        .expect("root Cargo.toml parses as TOML");
    let meta = manifest
        .get("workspace")
        .and_then(|w| w.get("metadata"))
        .and_then(|m| m.get("atpkg"))
        .unwrap_or_else(|| {
            panic!(
                "root Cargo.toml has no [workspace.metadata.atpkg] block — the packer falls \
                 back to its name-token guess; restore the block (expose/bundle)"
            )
        });
    let expose: Vec<&str> = meta
        .get("expose")
        .and_then(aterm_toml::Value::as_array)
        .map(|a| a.iter().filter_map(aterm_toml::Value::as_str).collect())
        .unwrap_or_default();
    assert_eq!(
        expose,
        vec!["aterm"],
        "root Cargo.toml [workspace.metadata.atpkg] expose must be exactly \
         [\"aterm\"] — ONE command on PATH is the one-binary collapse's \
         contract (ctl/pkg/fleet/drive are in-process verbs; siblings ride as \
         argv0 symlinks). Restore `expose = [\"aterm\"]`"
    );
}
