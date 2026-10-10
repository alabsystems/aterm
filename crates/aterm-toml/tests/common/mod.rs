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

/// Every `.toml` file tracked in the repository.
///
/// This corpus IS the specification for the purposes of these tests: real
/// manifests, real vendored manifests, real art assets, the real config files.
/// [`corpus_skips`] names what is skipped: build output, gate state, `.git/`
/// and recorded wire captures.
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
/// `target`, `.git` and `node_modules` at any depth, and at the repository root
/// every `target-*` sibling build dir (the gate's per-lane dirs) and the gate's
/// `.aterm-verify` state dir, which the checked-in .gitignore names as
/// `/target-*` and `/.aterm-verify/`.
fn corpus_skips(root: &Path, parent: &Path, name: &str) -> bool {
    name == "target"
        || name == ".git"
        || name == "node_modules"
        || (parent == root && (name.starts_with("target-") || name == ".aterm-verify"))
        || is_handoff_fixture_dir(parent, name)
}

/// `crates/aterm-gui/tests/fixtures/handoff/`: byte-exact RECORDINGS of what a
/// shipped release's seamless-update producer wrote, kept so the current consumer
/// is proven against them. The handoff manifest keeps the producer's own
/// `seamless-<pid>-<nonce>.toml` name, but on the wire it is a nonce line
/// followed by TOML — not a TOML document — and no file in there may ever be
/// reformatted, because the recorded digests are over those exact bytes. They are
/// wire captures, not config, so this gate does not claim them.
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
