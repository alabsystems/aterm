// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! COMPILE-TIME licence gate for the bundled DISPLAY FACES (`assets/game/`) and
//! the non-macOS terminal font STACK (`assets/bundled/`).
//!
//! Every file in that directory is a candidate for `include_bytes!`, and
//! `include_bytes!` puts the bytes in the shipped binary whether or not any
//! menu names them — embedding IS redistribution. So a face may only sit there
//! if a sibling `<stem>.LICENSE.txt` grants redistribution: OFL, Apache-2.0, or
//! MIT. Anything else fails `cargo build` / `cargo check`, not merely tests.
//!
//! It exists because the alternative failed in practice: four faces with no
//! redistribution grant (Gill Sans UltraBold, FinkHeavy, Hylia Serif Beta,
//! Mario Kart F2) reached a release branch, and catching them depended on a
//! human opening four `LICENSE.txt` files and reading to the end. This encodes
//! that reading where a violation cannot ship — the same shape as the census
//! gates.
//!
//! The sibling match is EXACT (`Cinzel-var.ttf` -> `Cinzel-var.LICENSE.txt`),
//! not a prefix or a fuzzy stem, so a new asset can never inherit cover from a
//! neighbour's notice by being named near it.

use std::path::{Path, PathBuf};

/// The directories holding gated bundled faces, relative to the manifest: the
/// display faces (`assets/game/`) and the non-macOS terminal font STACK
/// (`assets/bundled/` — JetBrains Mono and the Noto symbol/math faces), which is
/// held to exactly the same exact-sibling grant rule.
const ASSET_DIRS: &[&str] = &["assets/game", "assets/bundled"];

/// The suffix that marks a file as a licence notice rather than an asset.
const LICENSE_SUFFIX: &str = ".LICENSE.txt";

/// Grant markers that permit redistribution inside a binary. Matched
/// case-insensitively against the whole notice, because these are the phrases
/// the upstream licence texts and font `name` tables actually use.
///
/// Deliberately short: this gate answers "is there an open grant here at all",
/// and the answer has to be legible in the notice itself. A face whose terms
/// need a paragraph of interpretation is exactly the case that failed before.
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

/// Walk one directory, recording a line per unlicensed asset. Recurses so a
/// subdirectory cannot be used as a place to park bytes out of the gate's view.
fn gate_dir(dir: &Path, root: &Path, failures: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            failures.push(format!("  - cannot read {}: {error}", dir.display()));
            return;
        }
    };
    // Sorted so the failure list is stable across filesystems (and so a CI log
    // diff of two builds is readable).
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    paths.sort();
    for path in paths {
        // Rerun when any individual asset or notice changes, not just when the
        // directory listing does — an edited notice must re-run the gate.
        println!("cargo:rerun-if-changed={}", path.display());
        if path.is_dir() {
            gate_dir(&path, root, failures);
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            failures.push(format!("  - {}: non-UTF-8 file name", path.display()));
            continue;
        };
        // Dotfiles are filesystem debris (`.DS_Store`), never a face: nothing
        // references one, so gating them would only break macOS builds.
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
