// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Source-scan gate: NO panicking access to the `App.windows` map.
//!
//! # Why a source-scanning test
//!
//! `App.windows: BTreeMap<WindowId, WindowState>` is event-routing state. A
//! winit `Wake`/`WindowEvent` can arrive for a `WindowId` that was just closed
//! (the OS event was already in flight when the window was dropped, or a
//! control-socket verb references a stale id). Routing that stale event MUST be
//! a silent no-op — looking the id up with `.get`/`.get_mut` (which return
//! `Option`) and early-returning when it is absent. This is the *code-level*
//! guard standing behind the type-proven `NoStaleDelivery` property: the
//! property says a stale id is never *delivered* a live action; this scan makes
//! sure the lookup itself can never *panic* if one slips through.
//!
//! A `windows.get(&id).unwrap()`, `.expect(...)`, or `self.windows[id]` would
//! turn that benign stale event into a crash of the WHOLE process — taking down
//! every other live window. Rust's type system cannot forbid `.unwrap()` on an
//! `Option`, so this structural test does: it reads the committed `main.rs`
//! source and fails the build on any of the panicking access patterns below.
//!
//! ## The allowed pattern
//!
//! The one blessed way to reach the map is the guarded early-return:
//!
//! ```ignore
//! let Some(ws) = self.windows.get_mut(&id) else { return };
//! ```
//!
//! (or `.get(&id)`), which compiles to a no-op on a missing key rather than a
//! panic. Every map access in `main.rs` must use it.
//!
//! ## Forbidden anti-patterns (regex semantics)
//!
//! Scanned line-by-line, ignoring `//` comments:
//!
//!   * `windows\s*\.\s*get(_mut)?\s*\([^)]*\)\s*\.\s*unwrap\s*\(`
//!     — e.g. `windows.get(&id).unwrap()`
//!   * `windows\s*\.\s*get(_mut)?\s*\([^)]*\)\s*\.\s*expect\s*\(`
//!     — e.g. `windows.get_mut(&id).expect("...")`
//!   * `self\s*\.\s*windows\s*\[`
//!     — a direct index, which panics on a missing key.

use std::fs;
use std::path::{Path, PathBuf};

/// Absolute path to the committed GUI `main.rs` (the routing surface scanned).
fn main_rs_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("main.rs")
}

/// Strip a line-trailing `//` comment and any leading/inner indentation that
/// does not affect token matching, then collapse ALL whitespace so the
/// `\s*`-tolerant regex semantics reduce to plain substring checks. A line that
/// is ENTIRELY a comment collapses to the part before `//` (empty), so it never
/// matches.
///
/// `//` inside a string literal is rare in this file and, if present, only ever
/// makes the scan MORE conservative (it would truncate a line early), never less
/// — it can produce no false negative for the patterns we forbid.
fn strip_comment_and_whitespace(line: &str) -> String {
    let code = match line.find("//") {
        Some(idx) => &line[..idx],
        None => line,
    };
    code.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Whether the whitespace-collapsed `code` contains a panicking `windows.get`/
/// `get_mut(...).unwrap(`/`.expect(` access. After collapsing whitespace, the
/// `\s*` in the regex are gone, so we just look for the literal token run
/// `windows.get(` or `windows.get_mut(`, skip to its closing `)`, and require
/// `.unwrap(` or `.expect(` immediately after.
fn has_panicking_get(code: &str) -> bool {
    for tail in find_all(code, "windows.get") {
        // `tail` starts right after the literal `windows.get`. Accept an
        // optional `_mut`, then the call's `(`.
        let after_get = tail.strip_prefix("_mut").unwrap_or(tail);
        let Some(after_open) = after_get.strip_prefix('(') else {
            continue;
        };
        // Skip the argument list up to the FIRST `)` (the regex's `[^)]*\)`).
        let Some(close_idx) = after_open.find(')') else {
            continue;
        };
        let after_close = &after_open[close_idx + 1..];
        if after_close.starts_with(".unwrap(") || after_close.starts_with(".expect(") {
            return true;
        }
    }
    false
}

/// All suffixes of `hay` that immediately follow an occurrence of `needle`.
fn find_all<'a>(hay: &'a str, needle: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut start = 0;
    while let Some(rel) = hay[start..].find(needle) {
        let after = start + rel + needle.len();
        out.push(&hay[after..]);
        start = after;
    }
    out
}

/// `main.rs` contains ZERO panicking accesses to the `windows` routing map.
/// A stale `Wake`/`WindowEvent` for a closed `WindowId` must be a silent no-op
/// via `.get`/`.get_mut` returning `Option` — never a panic that crashes the
/// process (and with it every other live window).
#[test]
fn no_panicking_windows_map_access() {
    let path = main_rs_path();
    let src = fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));

    for (lineno, line) in src.lines().enumerate() {
        let code = strip_comment_and_whitespace(line);
        if code.is_empty() {
            continue;
        }

        // `windows.get(...).unwrap(` / `.expect(`  (with or without `_mut`).
        if has_panicking_get(&code) {
            panic!(
                "{}:{}: panicking access to the `windows` map — \
                 `windows.get[_mut](..).unwrap()/.expect(..)` crashes the whole \
                 process on a stale WindowId. Use the guarded form \
                 `if let Some(ws) = self.windows.get_mut(&id) else {{ return }}`.\n  {}",
                path.display(),
                lineno + 1,
                line.trim()
            );
        }

        // `self.windows[..]` — a direct index panics on a missing key.
        if code.contains("self.windows[") {
            panic!(
                "{}:{}: direct index `self.windows[..]` panics on a missing key. \
                 Use the guarded form \
                 `if let Some(ws) = self.windows.get_mut(&id) else {{ return }}`.\n  {}",
                path.display(),
                lineno + 1,
                line.trim()
            );
        }
    }
}

/// The scanned source actually exists where we expect it (guards against a file
/// rename silently dropping this gate's coverage).
#[test]
fn scanned_main_rs_exists() {
    let path = main_rs_path();
    assert!(
        path.is_file(),
        "expected the GUI routing surface at {} (did main.rs move?)",
        path.display()
    );
}
