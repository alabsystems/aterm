// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Tripwire: a `#[test]` function may not have an empty or comments-only body.
//!
//! libtest has no assertion-count concept, so `#[test] fn f() {}` is a GREEN
//! test asserting nothing. That is not hypothetical: `scene_surface_parity` sat
//! in `aterm-effects/tests/web_binding_parity.rs` from birth, and the commit
//! introducing it cited the module as delivered coverage. rustfmt puts the empty
//! braces on the signature line, so it reads as finished rather than stubbed,
//! and clippy has no lint for it. A list of `#[test] fn` names scans as a
//! coverage inventory — which is exactly how the false claim survived review.
//!
//! SCOPE is `crates/*/{src,tests}`. Deliberately not a walk from the workspace
//! root: `.claude/worktrees/` holds gitignored checkouts whose copies of these
//! files would be reported as violations of the working tree.
//!
//! The narrow cut is deliberate. "Body contains no assert/panic" flags ~100
//! legitimate tests (macro-generated bodies, `#[should_panic]`, no-panic
//! fuzzers). Empty-or-comments-only has no false positives.

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

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn is_test_attr(line: &str) -> bool {
    let t = line.trim();
    t == "#[test]" || t == "#[tokio::test]" || t.starts_with("#[tokio::test(")
}

/// A body is a violation when every line between the braces is blank or a
/// `//` comment. Returns the offending signature when so.
fn empty_test_body(lines: &[&str], fn_line: usize) -> bool {
    // Single-line form: `fn f() {}`.
    let sig = lines[fn_line].trim_end();
    if sig.ends_with("{}") {
        return true;
    }
    if !sig.ends_with('{') {
        // Multi-line signature: find the line opening the body.
        return false;
    }
    // NO BRACE ARITHMETIC. Counting `{`/`}` per line cannot tell a delimiter from a
    // brace inside a STRING LITERAL, and it ran BEFORE the is-this-real-code test, so
    // the count decided the verdict first. A test whose opening line was
    // `let banned = [":<11}", …];` drove the depth to zero on that very line and was
    // reported as an empty body — a fully implemented test, failed for containing a
    // `}` in a string. Format strings make that spelling ordinary.
    //
    // None of it was needed. The question is only "does anything but blanks and
    // comments appear before the close", and that is answered by looking at the lines
    // themselves: the first real line proves the body is not empty, and a bare `}`
    // reached before one proves it is.
    for line in lines.iter().skip(fn_line + 1) {
        let trimmed = line.trim();
        if trimmed == "}" {
            return true; // reached the close having seen only blanks/comments
        }
        if !trimmed.is_empty() && !trimmed.starts_with("//") {
            return false;
        }
    }
    false
}

#[test]
fn no_test_function_has_an_empty_body() {
    let root = workspace_root();
    let crates = root.join("crates");
    assert!(crates.is_dir(), "crates/ is missing — update this scope");

    let mut files = vec![];
    for entry in fs::read_dir(&crates).expect("read crates/").flatten() {
        for sub in ["src", "tests"] {
            let dir = entry.path().join(sub);
            if dir.is_dir() {
                rs_files(&dir, &mut files);
            }
        }
    }
    assert!(
        !files.is_empty(),
        "scanned zero .rs files — the scope is wrong, so this guard checks nothing"
    );

    let mut offenders = vec![];
    for file in &files {
        let Ok(text) = fs::read_to_string(file) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if !is_test_attr(line) {
                continue;
            }
            // Skip any further attributes between `#[test]` and the signature.
            let Some(fn_line) =
                (i + 1..lines.len().min(i + 8)).find(|&j| lines[j].trim_start().starts_with("fn "))
            else {
                continue;
            };
            if empty_test_body(&lines, fn_line) {
                let rel = file.strip_prefix(&root).unwrap_or(file);
                offenders.push(format!(
                    "{}:{}: {}",
                    rel.display(),
                    fn_line + 1,
                    lines[fn_line].trim()
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "these #[test] functions have empty or comments-only bodies — libtest reports them as \
         PASSING, so they ship a coverage claim they do not honour:\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn the_empty_body_detector_is_not_vacuous() {
    let empty = ["#[test]", "fn f() {}"];
    assert!(empty_test_body(&empty, 1), "must flag the single-line form");

    let comments = ["#[test]", "fn f() {", "    // TODO", "}"];
    assert!(
        empty_test_body(&comments, 1),
        "must flag a comments-only body"
    );

    let real = ["#[test]", "fn f() {", "    assert!(true);", "}"];
    assert!(!empty_test_body(&real, 1), "must not flag a real body");
}
