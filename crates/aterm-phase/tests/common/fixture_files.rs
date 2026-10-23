// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fixture screen loaders for phase and policy test sweeps.

use std::path::{Path, PathBuf};

use aterm_phase::prompt::fixtures as fx;

/// Extracts screen rows from a fixture file, dropping provenance and capture headers.
pub fn file_rows(text: &str) -> Vec<String> {
    let mut r = fx::screen(text);
    if r.first().is_some_and(|l| l.starts_with("== ")) {
        r.remove(0);
    }
    if r.last().is_some_and(|l| l.starts_with("exit=")) {
        r.pop();
    }
    r
}

fn dir_screens(crate_dir: &Path, dir: &Path, into: &mut Vec<(String, Vec<String>)>) {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.expect("a fixture entry").path())
        .filter(|p| p.is_file())
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "{}: no fixtures", dir.display());
    for p in paths {
        let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        let name = p
            .strip_prefix(crate_dir)
            .unwrap_or(&p)
            .display()
            .to_string();
        into.push((name, file_rows(&text)));
    }
}

/// Loads all fixture screens from `src/fixtures` and sibling policy fixtures when present.
pub fn fixture_screens(sweep: &str) -> Vec<(String, Vec<String>)> {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = Vec::new();
    dir_screens(crate_dir, &crate_dir.join("src/fixtures"), &mut out);
    let agent = crate_dir.join("../aterm-agent/src/supervise/policy/fixtures");
    if agent.is_dir() {
        dir_screens(crate_dir, &agent, &mut out);
    } else {
        eprintln!(
            "{sweep}: {} absent (not in the aterm workspace): its captures are not swept",
            agent.display()
        );
    }
    out
}
