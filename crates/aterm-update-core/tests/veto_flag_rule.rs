// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE ONE READING of a boolean update knob is a law, not a convention.
//!
//! `aterm_types::control_socket::env_flag_engaged` is the single rule for every
//! `ATERM_NO_*` veto and every QA seam that behaves like one: engaged only by a value
//! that is non-empty and not `"0"`. Its doc says why — a present-but-EMPTY variable
//! travels (`export ATERM_X=` in a shell rc hands every descendant an `is_some()` veto
//! nothing intended), and that exact species disabled the seamless updater on the
//! owner's daily driver twice on 2026-09-01 — and ends with "Flag readers go through
//! here, or they re-grow the bug."
//!
//! The four boolean knobs the update contract names (the ones `ENV_DENY_VARS` lists
//! with a boolean reader: `ATERM_NO_AUTO_UPDATE`, `ATERM_NO_AUTO_APPLY`,
//! `ATERM_NO_SEAMLESS_UPDATE`, `ATERM_DEBUG_SEAMLESS_REEXEC`) are read in several
//! crates, and nothing but review kept a new reader on the rule. This test is that
//! review, mechanised: every `std::env::var(_os)("<knob>")` in `crates/*/src` must sit
//! inside an `env_flag_engaged(` call. A reader that spells `.is_some()` instead is
//! the 2026-09-01 bug, re-grown — for `ATERM_DEBUG_SEAMLESS_REEXEC` it means an empty
//! inherited variable makes every apply re-exec the SAME binary and report success
//! while the staged build never lands.
//!
//! Lives in `aterm-update-core` (rather than `aterm-types`, which owns the rule)
//! because this crate's test build is the cheap one every updater audit already runs,
//! and because the knobs are the UPDATE contract's.

use std::path::{Path, PathBuf};

/// The boolean update knobs. A value-carrying seam (`ATERM_DEBUG_RELAUNCH_NUDGE=<version>`,
/// `ATERM_UPDATE_INTERVAL_SECS=<n>`) is not a flag and is not listed.
const BOOLEAN_UPDATE_KNOBS: [&str; 4] = [
    "ATERM_NO_AUTO_UPDATE",
    "ATERM_NO_AUTO_APPLY",
    "ATERM_NO_SEAMLESS_UPDATE",
    "ATERM_DEBUG_SEAMLESS_REEXEC",
];

/// How far back from the read the wrapping `env_flag_engaged(` may sit. Every
/// conforming reader in the tree spells `env_flag_engaged(\n std::env::var_os(..)`,
/// so the call is within a line; the window is generous for a rustfmt re-wrap.
const WRAP_WINDOW_BYTES: usize = 200;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// `text[..at]`'s last `n` bytes, backed off to a char boundary (the sources carry
/// em-dashes in comments, so a raw byte slice could split one).
fn tail(text: &str, at: usize, n: usize) -> &str {
    let mut start = at.saturating_sub(n);
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    &text[start..at]
}

#[test]
fn every_boolean_update_knob_is_read_through_env_flag_engaged() {
    let root = workspace_root();
    let mut files = Vec::new();
    for entry in std::fs::read_dir(root.join("crates"))
        .expect("crates/ exists")
        .flatten()
    {
        // `aterm-census` is a linter whose tests embed COPIES of shipped readers as
        // synthetic fixtures (`lazy_init.rs`); those are inputs to its gate, not
        // readers of anything.
        if entry.file_name() == "aterm-census" {
            continue;
        }
        rust_sources(&entry.path().join("src"), &mut files);
    }
    assert!(files.len() > 50, "found only {} sources", files.len());

    let mut reads = 0usize;
    let mut violations = Vec::new();
    for file in &files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        for knob in BOOLEAN_UPDATE_KNOBS {
            // `var_os("KNOB")` / `var("KNOB")` — a READ. `.env("KNOB", ..)`, a
            // deny-list literal, a `var: "KNOB"` field and `take("KNOB")` do not
            // match: the closing paren follows the string only on a read.
            let needle = format!("(\"{knob}\")");
            let mut from = 0;
            while let Some(pos) = text[from..].find(&needle) {
                let at = from + pos;
                from = at + needle.len();
                let call = tail(&text, at, 12);
                if !(call.ends_with("var_os") || call.ends_with("env::var")) {
                    continue;
                }
                // A comment QUOTING a retired reader (`seamless::normalize_commit`
                // records the one it removed) is not a reader.
                let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
                if text[line_start..at].trim_start().starts_with("//") {
                    continue;
                }
                reads += 1;
                if !tail(&text, at, WRAP_WINDOW_BYTES).contains("env_flag_engaged(") {
                    let line = text[..at].matches('\n').count() + 1;
                    let shown = file.strip_prefix(&root).unwrap_or(file).display();
                    violations.push(format!(
                        "{shown}:{line}: reads ${knob} outside env_flag_engaged"
                    ));
                }
            }
        }
    }
    assert!(
        reads >= 4,
        "expected the four knobs' production readers; matched only {reads} reads — the \
         needle no longer matches how the tree spells an env read"
    );
    assert!(
        violations.is_empty(),
        "every boolean update knob is read through env_flag_engaged (unset, EMPTY and \
         \"0\" are NOT engaged); these readers re-grow the 2026-09-01 empty-variable \
         veto:\n{}",
        violations.join("\n")
    );
}
