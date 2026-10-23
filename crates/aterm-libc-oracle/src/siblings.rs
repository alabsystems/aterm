// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HAND-WRITTEN SIBLING-BODY GATE.
//!
//! The C macros and `siginfo_t` accessors are the shim's only Rust bodies, and
//! the generator emits one text per OS family. Each same-OS pair of cells must
//! therefore carry a byte-identical body section. This is text, so it covers all
//! four declaration cells on every box: it binds a cell no box hosts to the body
//! its native sibling executes. It does not decide that cell.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::Path;

use crate::cells::{self, Os};

/// Where a cell source's hand-written section starts: the first line of code
/// the generator writes there (`regen::shim::bodies` writes it from this), never
/// a comment, so pruning a cell source's comments cannot undefine the section.
pub const BODY_MARKER: &str = "type fd_word = ";

/// A sibling-body defect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SiblingFinding {
    /// The source does not carry exactly one marker, so its section is undefined.
    Marker { file: String, count: usize },
    /// The pair's sections differ, first at `line` of the section (1-based).
    Differs {
        os: Os,
        left: String,
        right: String,
        line: usize,
    },
}

impl SiblingFinding {
    /// The family the finding is about.
    #[must_use]
    pub fn os(&self) -> Option<Os> {
        match self {
            Self::Differs { os, .. } => Some(*os),
            Self::Marker { file, .. } => cells::CELLS
                .iter()
                .find(|cell| cell.source == file)
                .map(|cell| cell.os),
        }
    }
}

impl fmt::Display for SiblingFinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Marker { file, count } => write!(
                f,
                "{file}: {count} hand-written-body markers (`{BODY_MARKER}`), not exactly one"
            ),
            Self::Differs {
                os,
                left,
                right,
                line,
            } => write!(
                f,
                "{}: {left} and {right} hand-written body sections differ (section line {line})",
                os.label()
            ),
        }
    }
}

/// The marker and everything after it, or how many markers there were.
///
/// # Errors
/// The marker count, when it is not exactly one.
pub fn body_section(source: &str) -> Result<&str, usize> {
    let count = source.matches(BODY_MARKER).count();
    match source.find(BODY_MARKER) {
        Some(at) if count == 1 => Ok(&source[at..]),
        _ => Err(count),
    }
}

/// Every sibling finding over `sources` (cell source file name -> text). A cell
/// absent from `sources` is a marker finding with a count of zero.
#[must_use]
pub fn compare(sources: &BTreeMap<&str, String>) -> Vec<SiblingFinding> {
    let section = |file: &str| -> Result<&str, SiblingFinding> {
        let text = sources.get(file).map_or("", String::as_str);
        body_section(text).map_err(|count| SiblingFinding::Marker {
            file: file.into(),
            count,
        })
    };
    let mut findings = Vec::new();
    for (os, left, right) in cells::sibling_pairs() {
        match (section(left.source), section(right.source)) {
            (Ok(a), Ok(b)) if a != b => {
                let line = a
                    .lines()
                    .zip(b.lines())
                    .position(|(x, y)| x != y)
                    .unwrap_or_else(|| a.lines().count().min(b.lines().count()))
                    + 1;
                findings.push(SiblingFinding::Differs {
                    os,
                    left: left.source.into(),
                    right: right.source.into(),
                    line,
                });
            }
            (Ok(_), Ok(_)) => {}
            (a, b) => {
                for finding in [a.err(), b.err()].into_iter().flatten() {
                    if !findings.contains(&finding) {
                        findings.push(finding);
                    }
                }
            }
        }
    }
    findings
}

/// Every cell source under `src` (`crates/aterm-libc/src`), by file name.
///
/// # Errors
/// When a source cannot be read.
pub fn read_sources(src: &Path) -> io::Result<BTreeMap<&'static str, String>> {
    cells::CELLS
        .iter()
        .map(|cell| Ok((cell.source, std::fs::read_to_string(src.join(cell.source))?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(head: &str, body: &str) -> String {
        format!("{head}\n{BODY_MARKER} hand-written\n{body}\n")
    }

    fn all(darwin_x86: &str, linux_arm: &str) -> BTreeMap<&'static str, String> {
        [
            (
                "darwin_aarch64.rs",
                source("// arm", "fn a() {}\nfn b() {}"),
            ),
            ("darwin_x86_64.rs", darwin_x86.to_string()),
            ("linux_gnu_x86_64.rs", source("// x86", "fn l() {}")),
            ("linux_gnu_aarch64.rs", linux_arm.to_string()),
        ]
        .into()
    }

    #[test]
    fn identical_sections_pass_whatever_precedes_them() {
        let sources = all(
            &source("// different generated head", "fn a() {}\nfn b() {}"),
            &source("// another head", "fn l() {}"),
        );
        assert_eq!(compare(&sources), []);
    }

    #[test]
    fn an_architecture_only_body_edit_is_named_with_its_line() {
        let sources = all(
            &source("// x86", "fn a() {}\nfn b() { 1 }"),
            &source("// arm", "fn l() {}"),
        );
        assert_eq!(
            compare(&sources),
            [SiblingFinding::Differs {
                os: Os::Darwin,
                left: "darwin_aarch64.rs".into(),
                right: "darwin_x86_64.rs".into(),
                line: 3,
            }]
        );
    }

    #[test]
    fn a_missing_or_doubled_marker_is_its_own_finding() {
        let doubled = format!("{BODY_MARKER}\n{BODY_MARKER}\n");
        let sources = all(&doubled, "no marker at all");
        let findings = compare(&sources);
        assert_eq!(
            findings,
            [
                SiblingFinding::Marker {
                    file: "darwin_x86_64.rs".into(),
                    count: 2
                },
                SiblingFinding::Marker {
                    file: "linux_gnu_aarch64.rs".into(),
                    count: 0
                },
            ]
        );
        let families: Vec<_> = findings.iter().map(SiblingFinding::os).collect();
        assert_eq!(families, [Some(Os::Darwin), Some(Os::GnuLinux)]);
    }
}
