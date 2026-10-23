// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Compile-time licence gate for bundled display, terminal, and test font assets.

use std::path::{Path, PathBuf};

#[path = "../aterm-log/clean_islands.rs"]
mod clean_islands;

// `cfg(anchor_rules)`
#[path = "../aterm-spec-anchor/anchor_rules.rs"]
mod anchor_rules;

const ASSET_DIRS: &[&str] = &["assets/game", "assets/bundled", "assets/test"];

const LICENSE_SUFFIX: &str = ".LICENSE.txt";

const GRANT_MARKERS: &[&str] = &[
    "sil open font license",
    "open font license",
    "ofl-1.1",
    "apache license",
    "apache-2.0",
    "apache 2.0",
    "mit license",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    clean_islands::emit();
    anchor_rules::emit();
    let manifest =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets the manifest dir"));
    let mut failures = Vec::new();
    for dir in ASSET_DIRS {
        println!("cargo:rerun-if-changed={dir}");
        let root = manifest.join(dir);
        gate_dir(&root, &manifest, &mut failures);
    }
    if !failures.is_empty() {
        panic!(
            "\n\nBUNDLED-FACE LICENCE GATE failed for {}:\n\n{}\n\n\
             Embedding a face in the aterm binary is redistribution. Ship a face only \
             with a sibling <stem>{LICENSE_SUFFIX} whose text names an OFL / Apache / MIT \
             grant, or remove the asset.\n",
            ASSET_DIRS.join("/, "),
            failures.join("\n")
        );
    }
}

fn gate_dir(dir: &Path, root: &Path, failures: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            failures.push(format!("  - cannot read {}: {error}", dir.display()));
            return;
        }
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    paths.sort();
    for path in paths {
        println!("cargo:rerun-if-changed={}", path.display());
        if path.is_dir() {
            gate_dir(&path, root, failures);
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            failures.push(format!("  - {}: non-UTF-8 file name", path.display()));
            continue;
        };
        if name.starts_with('.') || name.ends_with(LICENSE_SUFFIX) {
            continue;
        }
        let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
        let notice = path.with_file_name(format!("{stem}{LICENSE_SUFFIX}"));
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .display()
            .to_string();
        let Ok(text) = std::fs::read_to_string(&notice) else {
            failures.push(format!(
                "  - {relative}: no sibling {stem}{LICENSE_SUFFIX} — an asset with no \
                 notice has no grant to read"
            ));
            continue;
        };
        let lowered = text.to_lowercase();
        if !GRANT_MARKERS.iter().any(|marker| lowered.contains(marker)) {
            failures.push(format!(
                "  - {relative}: {stem}{LICENSE_SUFFIX} names no OFL / Apache / MIT grant"
            ));
        }
    }
}
