// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared test scaffolding: the repository's own `.toml` corpus.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// The repository root, two levels up from this crate's manifest.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/aterm-toml sits two levels under the repository root")
        .to_path_buf()
}

/// Every `.toml` file under the repository root (manifests, vendored manifests, art
/// assets, config files), minus what [`corpus_skips`] names.
pub fn corpus() -> Vec<PathBuf> {
    let out = corpus_under(&repo_root());
    assert!(
        out.len() > 100,
        "the corpus should be the whole tree, found only {}",
        out.len()
    );
    out
}

/// Every `.toml` under `root`, sorted, minus the directories [`corpus_skips`]
/// names.
pub fn corpus_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// Whether the corpus walk stays out of the directory `name` inside `parent`:
/// `target`, `.git`, `node_modules` and every `*.noindex` build dir a tool chose
/// (`libc-oracle/target.noindex`) at any depth, and at the repository root every
/// `target-*` sibling build dir (`target-trust.noindex`, older lanes' leftovers)
/// and a leftover `.aterm-verify` state dir, which the checked-in .gitignore names as
/// `/target-*` and `/.aterm-verify/`.
fn corpus_skips(root: &Path, parent: &Path, name: &str) -> bool {
    name == "target"
        || name == ".git"
        || name.ends_with(".noindex")
        || name == "node_modules"
        || (parent == root && (name.starts_with("target-") || name == ".aterm-verify"))
        || is_handoff_fixture_dir(parent, name)
}

/// `crates/aterm-gui/tests/fixtures/handoff/`: byte-exact recordings of what a shipped
/// release's seamless-update producer wrote. A `seamless-*.toml` there is a nonce line
/// followed by TOML, not a TOML document, and the recorded digests cover its exact bytes.
fn is_handoff_fixture_dir(parent: &Path, name: &str) -> bool {
    name == "handoff" && parent.ends_with(Path::new("tests").join("fixtures"))
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if corpus_skips(root, dir, &name) {
                continue;
            }
            walk(root, &path, out);
        } else if path.extension().is_some_and(|e| e == "toml") {
            out.push(path);
        }
    }
}
