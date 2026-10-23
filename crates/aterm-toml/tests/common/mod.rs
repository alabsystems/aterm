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

/// Every `.toml` file under the repository root, minus `corpus_skips` directories.
pub fn corpus() -> Vec<PathBuf> {
    let out = corpus_under(&repo_root());
    assert!(
        out.len() > 100,
        "the corpus should be the whole tree, found only {}",
        out.len()
    );
    out
}

/// Every `.toml` under `root` outside the directories `corpus_skips` names, sorted.
pub fn corpus_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

fn corpus_skips(root: &Path, parent: &Path, name: &str) -> bool {
    name == "target"
        || name == ".git"
        || name.ends_with(".noindex")
        || name == "node_modules"
        || (parent == root && (name.starts_with("target-") || name == ".aterm-verify"))
        || is_handoff_fixture_dir(parent, name)
}

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
