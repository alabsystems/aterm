// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Compile-time licence gate for bundled display, terminal, and test font assets.

use std::path::{Path, PathBuf};

#[path = "../aterm-log/clean_islands.rs"]
mod clean_islands;

// `cfg(anchor_rules)`: which anchor attributes this crate writes.
#[path = "../aterm-spec-anchor/anchor_rules.rs"]
mod anchor_rules;

// The cut Windows' default faces are made with (`src/no_ligatures.rs`).
#[path = "src/no_ligatures.rs"]
mod no_ligatures;

/// The bundled Iosevka Term files Windows' default faces are cut from.
const NO_LIGATURE_CUTS: &[&str] = &[
    "IosevkaTerm-Regular",
    "IosevkaTerm-Bold",
    "IosevkaTerm-Italic",
    "IosevkaTerm-BoldItalic",
];

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
    println!("cargo:rerun-if-changed=src/no_ligatures.rs");
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
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var_os("CARGO_FEATURE_EMBEDDED_FONT").is_some()
    {
        cut_no_ligature_faces(&manifest);
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

/// Write `<stem>.nl.ttf` into `OUT_DIR` for each of [`NO_LIGATURE_CUTS`]: the bundled file with
/// its programming-ligature features taken out, which `bundled.rs` compiles in on Windows.
fn cut_no_ligature_faces(manifest: &Path) {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    for stem in NO_LIGATURE_CUTS {
        let source = manifest.join("assets/bundled").join(format!("{stem}.ttf"));
        let font = std::fs::read(&source)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", source.display()));
        let cut = no_ligatures::without_ligature_features(&font).unwrap_or_else(|| {
            panic!(
                "{}: no `calt` or `liga` feature to take out, so the cut would be the file itself",
                source.display()
            )
        });
        let target = out.join(format!("{stem}.nl.ttf"));
        std::fs::write(&target, cut)
            .unwrap_or_else(|error| panic!("cannot write {}: {error}", target.display()));
    }
}
