// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test verdicts must not depend on the cutter's environment or the owner's release history.
//! This lexical guard follows direct environment reads and the two ambient release adapters;
//! it does not claim to follow arbitrary calls through a test's dependency graph.

use super::rules::PRODUCT;
use super::{Bound, Finding, Rule, SourceFile, Tree, brace_close, hits, literal, token};

pub(super) const RELEASE_TEST_STATE: Rule = Rule {
    id: "release-test-state",
    why: "inject release state and scheduling context in tests: HOME's release history and \
          an inherited ATERM_RELEASE_RUN must not choose a test's verdict; use a fixture \
          directory, a fixed Machine or an explicit Record",
    scope: &[],
    check,
};

fn check(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let test_files = tree.corpus.test_code();
    for file in tree.corpus.under(PRODUCT) {
        if !file.mentions(&["HOME", "RELEASE_RUN", "Live", "disk_record", "record_in"]) {
            continue;
        }
        let lexed = file.lexed();
        let code = &lexed.code;
        // Whether the file is test code is asked once, its blanked lines when a `fn` first needs
        // them, and each `fn`'s line is counted on from the one before.
        let held = test_files.holds(&file.rel);
        let mut blanked: Option<Blanked<'_>> = None;
        let mut lines = LineOf::new(code);
        for at in hits(code, "fn", Bound::Word) {
            let attr_start = code[..at].rfind([';', '{', '}']).map_or(0, |i| i + 1);
            let is_test = held
                || blanked
                    .get_or_insert_with(|| Blanked::of(file))
                    .at(at, &mut lines)
                || code[attr_start..at].contains("#[test]");
            if !is_test {
                continue;
            }
            let Some(open) = code[at..].find(['{', ';']).map(|i| at + i) else {
                continue;
            };
            if code.as_bytes()[open] != b'{' {
                continue;
            }
            let Some(close) = brace_close(code, open) else {
                continue;
            };
            let body = &code[open..=close];
            let text = &lexed.text[open..=close];
            let compact: String = body.chars().filter(|c| !c.is_whitespace()).collect();
            let direct_marker = ["var", "var_os"].iter().any(|name| {
                hits(body, name, Bound::Word).any(|p| {
                    let rest = &body[p + name.len()..];
                    let Some(end) = rest.find(')') else {
                        return false;
                    };
                    let argument = &text[p + name.len()..p + name.len() + end];
                    argument.contains("ATERM_RELEASE_RUN")
                        || rest[..end].contains("RELEASE_RUN_ENV")
                })
            });
            // A child probe may read the marker it explicitly injects. Ordinary tests must
            // pass the value into their subject instead of reading their launcher's marker.
            let injected_marker = compact.contains(".env(aterm_capacity::RELEASE_RUN_ENV,")
                || compact.contains(".env(RELEASE_RUN_ENV,")
                || injects(body, text, "ATERM_RELEASE_RUN");
            let home_release = text.contains("HOME")
                && (text.contains(".local/state/aterm-release") || body.contains("RELEASE_STATE"))
                && (compact.contains("env::var(") || compact.contains("env::var_os("))
                && [
                    "read",
                    "read_to_string",
                    "read_dir",
                    "load",
                    "open",
                    "metadata",
                ]
                .iter()
                .any(|name| hits(body, name, Bound::Word).next().is_some())
                && !injects(body, text, "HOME");
            // A locally constructed need admitted on Live can silently change rank under
            // the cutter. A need handed into a test's injected admission callback already
            // carries the subject's policy (for example a line's explicit Release rank).
            let live_admission = hits(body, "Live", Bound::Word).next().is_some()
                && compact.contains("Need::new(")
                && (compact.contains("admit_with(") || compact.contains("Waiter::enter("));
            let real_record = compact.contains("Real::new(")
                && (compact.contains(".disk_record(")
                    || compact.contains("release_disk::record_in("))
                && !compact.contains(".state=");
            if (direct_marker && !injected_marker) || home_release || live_admission || real_record
            {
                let line = lines.at(at);
                out.push(file.finding(RELEASE_TEST_STATE.id, line,
                    "test reads ambient release state or scheduling context; inject its home/state, Machine or Record".into()));
            }
        }
    }
}

/// Which offsets of a file's CODE view its shipped view ([`SourceFile::shipped_code`]) holds a
/// blank at.
enum Blanked<'a> {
    /// A CODE view without a `\r`: its shipped view is itself with each test-only line blanked,
    /// byte for byte, so the line of an offset that holds no blank says. Each flag is the
    /// file's own: whether that line ships.
    Lines(&'a [bool]),
    /// Any other CODE view: the shipped view itself, whose lines `str::lines` cut short of a
    /// `\r`.
    View(String),
}

impl<'a> Blanked<'a> {
    fn of(file: &'a SourceFile) -> Self {
        if file.lexed().code.contains('\r') {
            Self::View(file.shipped_code())
        } else {
            Self::Lines(file.shipped_flags())
        }
    }

    /// Does the shipped view hold a blank at `at`, a `fn` keyword's offset in the CODE view?
    fn at(&self, at: usize, lines: &mut LineOf<'_>) -> bool {
        match self {
            Self::Lines(flags) => !flags.get(lines.at(at) - 1).copied().unwrap_or(true),
            Self::View(view) => view.as_bytes().get(at) == Some(&b' '),
        }
    }
}

/// The 1-based line of each offset of a text, asked in rising order: each count goes on from
/// the offset asked before.
struct LineOf<'a> {
    text: &'a [u8],
    from: usize,
    line: usize,
}

impl<'a> LineOf<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text: text.as_bytes(),
            from: 0,
            line: 1,
        }
    }

    /// The line of `at`, no earlier than any offset asked before.
    fn at(&mut self, at: usize) -> usize {
        self.line += self.text[self.from..at]
            .iter()
            .filter(|&&b| b == b'\n')
            .count();
        self.from = at;
        self.line
    }
}

fn injects(code: &str, text: &str, key: &str) -> bool {
    hits(code, "env", Bound::Word).any(|at| {
        token(code, at + 3, "(")
            .and_then(|open| literal(code, text, open))
            .is_some_and(|(name, _)| name == key)
    })
}

#[cfg(test)]
#[path = "release_test_state_tests.rs"]
mod tests;
