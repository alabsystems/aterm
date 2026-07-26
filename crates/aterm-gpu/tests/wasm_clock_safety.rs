// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Tripwire: `std::time::Instant::now()` must never reach a wasm build.
//!
//! On `wasm32-unknown-unknown` it PANICS — "time not implemented on this
//! platform". `web_time::Instant` shims to `performance.now()` there and
//! re-exports std's `Instant` natively, so swapping is byte-identical for native
//! builds and correct for the web ones.
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

use std::fs;
use std::path::{Path, PathBuf};

/// Crates whose code is compiled into the wasm bundles (`aterm-wasm`,
/// `aterm-gpu-web`) and therefore must not call std's clock. Adding a crate to
/// either wasm entry point's dependency closure means adding it here.
const WASM_REACHABLE: &[&str] = &[
    "aterm-core",
    "aterm-effects",
    "aterm-gpu",
    "aterm-gpu-web",
    "aterm-grid",
    "aterm-render",
    "aterm-scrollback",
    "aterm-search",
    "aterm-wasm",
];

const FORBIDDEN: &str = "std::time::Instant::now()";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("workspace root")
        .to_path_buf()
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

#[test]
fn no_wasm_reachable_crate_calls_the_std_clock_in_production_code() {
    let root = workspace_root();
    let mut offenders = vec![];

    for name in WASM_REACHABLE {
        let src = root.join("crates").join(name).join("src");
        assert!(
            src.is_dir(),
            "{name}/src is missing — update WASM_REACHABLE if the crate was renamed or removed"
        );
        for file in rs_files(&src) {
            let text = fs::read_to_string(&file).expect("read src");
            if !text.contains(FORBIDDEN) {
                continue;
            }
            let lines: Vec<&str> = text.lines().collect();
            for (idx, line) in lines.iter().enumerate() {
                if line.contains(FORBIDDEN) && !is_test_code(&lines, idx) {
                    let rel = file.strip_prefix(&root).unwrap_or(&file);
                    offenders.push(format!("{}:{}: {}", rel.display(), idx + 1, line.trim()));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "`{FORBIDDEN}` PANICS on wasm32 (\"time not implemented on this platform\") and these \
         crates compile into the wasm bundles. Use `web_time::Instant::now()` — it is \
         byte-identical natively and shims to performance.now() on wasm. A panic here does not \
         merely lose a frame: it poisons the wasm-bindgen RefCell and every later access throws \
         \"recursive use of an object detected\". Offenders:\n  {}",
        offenders.join("\n  ")
    );
}
