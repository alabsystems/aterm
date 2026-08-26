// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Tripwire: std's clock must never reach a wasm build.
//!
//! On `wasm32-unknown-unknown` BOTH `std::time::Instant::now()` and
//! `std::time::SystemTime::now()` PANIC — "time not implemented on this
//! platform". `aterm_time::Instant` / `aterm_time::SystemTime` shim to
//! `performance.now()` / `Date.now()` there and re-export std's types natively,
//! so swapping is byte-identical for native builds and correct for the web ones.
//!
//! This exists because the rule was broken in production and nothing caught it.
//! An `acquire_started` clock in this crate's `renderer.rs` used std::time, so
//! the web renderer trapped on its FIRST frame. The fallout was far worse than a
//! dead frame: the panic unwinds out of a `&mut self` wasm-bindgen method, so the
//! object's `RefCell` is never released and EVERY later access throws "recursive
//! use of an object detected which would lead to unsafe aliasing in rust". What
//! users saw was the shared render worker crashing, panes falling back to the
//! in-process CPU path, and a React boundary killing an overlay — none of which
//! names the clock. Every unit and integration suite was green throughout.
//!
//! The embedding app also carries a source patch that rewrites some of these call
//! sites for its wasm build, but it keys on VARIABLE NAMES, so a newly-named clock
//! slips straight past it. That patch is a backstop, not a guard. This is the
//! guard, and it walks the tree at runtime so new files are covered automatically.
//!
//! # The closure is DERIVED, not hand-listed
//!
//! It used to be a hand-written list of nine crates, and the list was wrong: the
//! `web-time` retirement had to migrate `aterm-types::shell_types`, whose
//! `SystemTime::now()` was in the wasm closure via `aterm-core` and which this
//! guard would not have caught on either axis — the crate was unlisted AND
//! `SystemTime` was not forbidden. A hand-list of a transitive closure is a fact
//! that goes stale the first time someone adds a dependency, so the closure is
//! now walked from the three wasm entry points' manifests on every run.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// The wasm entry points. Everything reachable from these through normal
/// (non-dev, non-build) dependency edges is code that can be compiled into a
/// browser bundle.
const WASM_ROOTS: &[&str] = &["aterm-wasm", "aterm-gpu-web", "aterm-effects-web"];

/// The std-clock constructors that panic on `wasm32-unknown-unknown`.
///
/// BOTH spellings, deliberately. `SystemTime::now()` panics exactly as
/// `Instant::now()` does, and it was the `SystemTime` half that this campaign
/// had to migrate by hand (`aterm-types::shell_types::current_time_ms`) because
/// nothing would have flagged it.
const FORBIDDEN: &[&str] = &["std::time::Instant::now()", "std::time::SystemTime::now()"];

/// The in-source opt-out marker.
///
/// A site that genuinely cannot reach wasm — the clearest case being one inside
/// a `#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]` arm whose
/// wasm twin sits right beside it — carries this marker plus a reason on one of
/// the lines directly above it. Deliberately a VISIBLE annotation at the call
/// site rather than a crate name on a list at the top of this file: an exemption
/// you have to open another file to discover is how the original bug survived
/// review, and this way the reason is read by whoever next edits the line.
const ALLOW_MARKER: &str = "wasm-clock-guard: allow";

/// How many lines above a site the marker may sit — enough for a marker with its
/// reason on the following line, not enough to reach an unrelated function.
const ALLOW_LOOKBACK: usize = 4;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

/// The workspace path dependencies one manifest declares through NORMAL edges.
///
/// `[dependencies]` and every `[target.'cfg(…)'.dependencies]` section count;
/// `dev-` and `build-` sections do not, because neither reaches a bundle. cfg
/// predicates are NOT evaluated — a cfg-gated edge counts on every platform, the
/// same fail-closed rule `aterm-census`'s scan-set derivation uses: this guard
/// would rather walk a file the browser never links than miss one it does.
fn normal_workspace_deps(root: &Path, crate_name: &str) -> Vec<String> {
    let manifest = root.join("crates").join(crate_name).join("Cargo.toml");
    let Ok(text) = fs::read_to_string(&manifest) else {
        return vec![];
    };
    let mut out = vec![];
    let mut in_deps = false;
    for line in text.lines() {
        let t = line.trim();
        if let Some(header) = t.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
            let h = header.trim();
            in_deps =
                h == "dependencies" || (h.starts_with("target.") && h.ends_with(".dependencies"));
            continue;
        }
        if !in_deps || t.is_empty() || t.starts_with('#') {
            continue;
        }
        // `name = { … }`, `name = "…"` or `name.workspace = true`.
        let key_end = t
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(t.len());
        let name = &t[..key_end];
        if !name.is_empty() && root.join("crates").join(name).join("Cargo.toml").is_file() {
            out.push(name.to_string());
        }
    }
    out
}

/// Every workspace crate reachable from the wasm entry points.
fn wasm_closure(root: &Path) -> BTreeSet<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut stack: Vec<String> = WASM_ROOTS.iter().map(|s| (*s).to_string()).collect();
    while let Some(name) = stack.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        stack.extend(normal_workspace_deps(root, &name));
    }
    seen
}

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
    out.sort();
    out
}

/// Whether the line at `idx` is inside test-only code. Test modules and `#[test]`
/// functions never reach wasm, so their clocks are legitimate. Approximated the
/// way this repo's other source tripwires do: scan backwards for the nearest
/// `#[cfg(test)]` / `#[test]` attribute and treat everything after it in the file
/// as test code — the test module is always the file's tail here.
fn is_test_code(lines: &[&str], idx: usize) -> bool {
    lines[..idx].iter().rev().any(|l| {
        let t = l.trim();
        t.starts_with("#[cfg(test)]") || t.starts_with("#[test]")
    })
}

/// A line that is entirely a comment names the clock, it does not call it —
/// and this guard's own subject matter means the forbidden spellings turn up in
/// prose (the doc that explains WHY they are forbidden) more often than in code.
fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

/// Whether the site at `idx` carries an [`ALLOW_MARKER`] just above it.
fn is_allowed(lines: &[&str], idx: usize) -> bool {
    let start = idx.saturating_sub(ALLOW_LOOKBACK);
    lines[start..idx].iter().any(|l| l.contains(ALLOW_MARKER))
}

/// The derived closure must actually contain the crates the migration proved are
/// in it, and it must be walked rather than assumed. Pinned as a LOWER BOUND, not
/// an equality: a new dependency legitimately grows the set, and a guard that
/// fails on growth is a guard people delete.
#[test]
fn the_derived_wasm_closure_covers_the_crates_the_migration_found() {
    let root = workspace_root();
    let closure = wasm_closure(&root);
    // The nine the hand-list had, plus the five it was missing — every one of
    // which reaches a browser bundle through `aterm-wasm`.
    for expected in [
        "aterm-core",
        "aterm-effects",
        "aterm-gpu",
        "aterm-gpu-web",
        "aterm-grid",
        "aterm-render",
        "aterm-scrollback",
        "aterm-search",
        "aterm-wasm",
        "aterm-policy",
        "aterm-predict",
        "aterm-selection",
        "aterm-shell-integration",
        "aterm-types",
    ] {
        assert!(
            closure.contains(expected),
            "the derived wasm closure lost `{expected}` — the derivation or the \
             manifests drifted. Derived set: {closure:?}"
        );
    }
}

#[test]
fn no_wasm_reachable_crate_calls_the_std_clock_in_production_code() {
    let root = workspace_root();
    let closure = wasm_closure(&root);
    assert!(
        closure.len() > 9,
        "the derived closure collapsed to {} crate(s) — the manifest walk broke",
        closure.len()
    );
    let mut offenders = vec![];

    for name in &closure {
        let src = root.join("crates").join(name).join("src");
        if !src.is_dir() {
            continue;
        }
        for file in rs_files(&src) {
            let text = fs::read_to_string(&file).expect("read src");
            if !FORBIDDEN.iter().any(|f| text.contains(f)) {
                continue;
            }
            let lines: Vec<&str> = text.lines().collect();
            for (idx, line) in lines.iter().enumerate() {
                if is_comment(line) || is_test_code(&lines, idx) || is_allowed(&lines, idx) {
                    continue;
                }
                for forbidden in FORBIDDEN {
                    if line.contains(forbidden) {
                        let rel = file.strip_prefix(&root).unwrap_or(&file);
                        offenders.push(format!("{}:{}: {}", rel.display(), idx + 1, line.trim()));
                    }
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "std's clock PANICS on wasm32 (\"time not implemented on this platform\") and these \
         crates compile into the wasm bundles. Use `aterm_time::Instant::now()` / \
         `aterm_time::SystemTime::now()` — byte-identical natively, shimmed to \
         performance.now()/Date.now() on wasm. A panic here does not merely lose a frame: it \
         poisons the wasm-bindgen RefCell and every later access throws \"recursive use of an \
         object detected\". If a site genuinely cannot reach wasm, mark it `{ALLOW_MARKER}` \
         with the reason on the lines directly above it. Offenders:\n  {}",
        offenders.join("\n  ")
    );
}
