// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! No-mint-reachability conformance (ATERM_DESIGN §5.4) — the Tier-1 partner of
//! `aterm_spec::derive::mint_reachability_model`.
//!
//! The `ty` model proves the ABSTRACT property (an untrusted actor never reaches
//! the `Top` mint — invariant `NoUntrustedTop`). This source-scan test binds that
//! property to the REAL tree, mirroring the `aterm-core/tests/capability_ceremony.rs`
//! idiom: rustc's feature/visibility rules already enforce the seal in isolated
//! builds, but a refactor could silently regress it. This guards against exactly
//! three regressions:
//!
//! 1. dropping the seal attributes on `root_authority` (feature-cfg + `doc(hidden)`
//!    + `unsafe`), which would make the mint nameable / discoverable again;
//! 2. enabling `launcher-mint` in a NON-launcher PRODUCTION dependency table, which
//!    would give that crate the mint at compile time;
//! 3. reaching the mint from production (non-`#[cfg(test)]`) code outside the two
//!    trusted launcher binaries.

use std::fs;
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

/// Every crate directory under `crates/` that carries a `Cargo.toml`.
fn crate_dirs(root: &Path) -> Vec<PathBuf> {
    let mut v = vec![];
    for e in fs::read_dir(root.join("crates"))
        .expect("read crates/")
        .flatten()
    {
        let p = e.path();
        if p.join("Cargo.toml").is_file() {
            v.push(p);
        }
    }
    v.sort();
    v
}

/// Every `.rs` file under `dir`, recursively.
fn rs_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(rs_files(&p));
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
    out
}

/// (1) The seal attributes on `root_authority` must remain intact: the constructor
/// is `pub unsafe fn`, `#[cfg(any(test, feature = "launcher-mint"))]` (the seal),
/// and `#[doc(hidden)]` (the unwelcoming shape).
#[test]
fn root_authority_stays_feature_sealed_and_hidden() {
    let lib = workspace_root().join("crates/aterm-cap/src/lib.rs");
    let src = fs::read_to_string(&lib).expect("read aterm-cap lib.rs");
    // The `find` itself asserts the `pub unsafe fn` shape (removing `unsafe` breaks it).
    let idx = src
        .find("pub unsafe fn root_authority(")
        .expect("root_authority must stay a `pub unsafe fn` (the audit marker)");
    let head = &src[idx.saturating_sub(400)..idx];
    assert!(
        head.contains(r#"#[cfg(any(test, feature = "launcher-mint"))]"#),
        "root_authority must stay gated behind \
         `#[cfg(any(test, feature = \"launcher-mint\"))]` — the no-mint-reachability seal (§5.4)"
    );
    assert!(
        head.contains("#[doc(hidden)]"),
        "root_authority must stay `#[doc(hidden)]` (non-discoverable mint shape)"
    );
}

/// (2) Exactly the two trusted launcher binaries enable `launcher-mint` in a
/// PRODUCTION (non-dev) dependency table. Any other production enabler would give
/// that crate the mint at compile time; dev-dependency enablers (test-only) are fine.
#[test]
fn only_launchers_enable_launcher_mint_in_production() {
    let root = workspace_root();
    let mut enablers = vec![];
    for dir in crate_dirs(&root) {
        let toml = fs::read_to_string(dir.join("Cargo.toml")).expect("read Cargo.toml");
        let mut production_section = false;
        for line in toml.lines() {
            let t = line.trim();
            if t.starts_with('[') && t.ends_with(']') {
                // A production dependency table is `[dependencies]` or
                // `[target.'cfg(...)'.dependencies]` — never a `*dev-dependencies*`.
                production_section = (t == "[dependencies]"
                    || (t.starts_with("[target.") && t.ends_with(".dependencies]")))
                    && !t.contains("dev-dependencies");
                continue;
            }
            if production_section && t.starts_with("aterm-cap") && t.contains("launcher-mint") {
                enablers.push(dir.file_name().unwrap().to_string_lossy().into_owned());
            }
        }
    }
    enablers.sort();
    enablers.dedup();
    assert_eq!(
        enablers,
        vec!["aterm-cli".to_string(), "aterm-gui".to_string()],
        "exactly the two trusted launcher binaries may enable `launcher-mint` as a \
         PRODUCTION dependency; found {enablers:?}. A non-launcher production enabler \
         would let that crate name `Authority::root_authority()` — the mint would be reachable."
    );
}

/// (3) No production code outside a launcher `main.rs` reaches the mint: every
/// `Authority::root_authority(` CALL in `crates/*/src/**` is either inside a
/// `#[cfg(test)]` region (test-only, trusted) or in one of the two launcher mains.
#[test]
fn no_production_code_outside_launchers_reaches_the_mint() {
    let root = workspace_root();
    const LAUNCHERS: [&str; 2] = ["aterm-cli", "aterm-gui"];
    let mut offenders = vec![];
    for dir in crate_dirs(&root) {
        let crate_name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let is_launcher = LAUNCHERS.contains(&crate_name.as_str());
        for file in rs_files(&dir.join("src")) {
            let text = fs::read_to_string(&file).expect("read src");
            // Once a file enters a `#[cfg(test)]` region, the rest is test-only code
            // (matches the real layout: every mint lives under `#[cfg(test)] mod tests`).
            let mut in_test = false;
            for (i, line) in text.lines().enumerate() {
                let t = line.trim_start();
                if t == "#[cfg(test)]" {
                    in_test = true;
                }
                // Skip comments (the constructor is named in several doc/comment lines).
                if t.starts_with("//") || t.starts_with('*') {
                    continue;
                }
                if !line.contains("Authority::root_authority(") {
                    continue;
                }
                if in_test || is_launcher {
                    continue;
                }
                offenders.push(format!("{}:{}", file.display(), i + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "these production (non-#[cfg(test)]) sites reach the sealed mint outside a \
         trusted launcher `main.rs` — the mint must be launcher-only (§5.4):\n{offenders:#?}"
    );
}
