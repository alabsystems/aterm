// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared, std-only release-entry grammar. Census admits the same fragment bytes
//! that the release assembler ships; this module does no I/O or assembly.

/// The head of the harness-versions announcement the release derives and writes into its own
/// section (aterm-release's `harness_versions`): generated, so it never counts as a section's
/// release notes.
pub const HARNESS_ENTRY_HEAD: &str = "- **Harness versions since v";

/// Keep a Changelog's section order, shared with the release's assembly of `[Unreleased]` from
/// the entry files under `changes/<kind>/`.
pub const KINDS: [&str; 6] = [
    "Added",
    "Changed",
    "Deprecated",
    "Removed",
    "Fixed",
    "Security",
];

/// The directory, at the repository root, that holds one file per changelog entry.
pub const FRAGMENT_DIR: &str = "changes";

/// The longest `<name>` (without `.md`) a fragment may carry.
pub const MAX_NAME_BYTES: usize = 100;

/// The most fragments one release window may hold.
pub const MAX_FRAGMENTS: usize = 4096;

/// The largest one fragment may be.
pub const MAX_FRAGMENT_BYTES: usize = 64 << 10;

/// The most bytes every fragment of one window may hold together.
pub const MAX_TOTAL_BYTES: usize = 4 << 20;

/// One fragment as a reader found it: its repository-relative path and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    pub path: String,
    pub text: String,
}

/// The Keep a Changelog kind (`Added`, …) a fragment path's directory names, if any.
#[must_use]
pub fn kind_of(path: &str) -> Option<&'static str> {
    let dir = path
        .strip_prefix(FRAGMENT_DIR)?
        .strip_prefix('/')?
        .split('/')
        .next()?;
    KINDS
        .iter()
        .copied()
        .find(|kind| kind.to_ascii_lowercase() == dir)
}

/// The kinds' directory names, for messages: `added, changed, …, security`.
#[must_use]
pub fn kind_dirs() -> String {
    KINDS
        .iter()
        .map(|kind| kind.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether `path`'s first component names the entry directory in any case: `changes/…`,
/// `Changes/…`, or a root file `changes`, every path the release's strict read judges.
#[must_use]
pub fn in_entry_dir(path: &str) -> bool {
    path.split('/')
        .next()
        .is_some_and(|root| root.eq_ignore_ascii_case(FRAGMENT_DIR))
}

/// An entry's words for a repeat, as the release's shipment audit compares them: its bullet
/// marker dropped and its whitespace folded, so re-wrapping makes no new entry.
#[must_use]
pub fn entry_key(text: &str) -> String {
    let text = text.trim_start();
    let text = text
        .strip_prefix("- ")
        .or_else(|| text.strip_prefix("* "))
        .unwrap_or(text);
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The path rules: `changes/<kind>/<name>.md`, nothing directly in `changes/` and nothing
/// deeper, a lowercase kind and name; a root entry that is another case of `changes`, or a file
/// named `changes`, is named as the release's strict read names it.
#[must_use]
pub fn path_findings(path: &str) -> Vec<String> {
    let parts: Vec<&str> = path.split('/').collect();
    if in_entry_dir(path) && (parts[0] != FRAGMENT_DIR || parts.len() == 1) {
        return vec![format!(
            "{path}: changelog entries live in a directory spelled exactly {FRAGMENT_DIR}/, \
             which no release would otherwise read"
        )];
    }
    let shape = format!(
        "{path}: a changelog entry is a file changes/<kind>/<name>.md (kind: {}; name: \
         lowercase letters, digits and hyphens, at most {MAX_NAME_BYTES} bytes, conventionally \
         <session>-<slug>), and changes/ holds nothing else",
        kind_dirs()
    );
    let [root, kind, file] = parts.as_slice() else {
        return vec![shape];
    };
    if *root != FRAGMENT_DIR || kind_of(path).is_none() || *kind != kind.to_ascii_lowercase() {
        return vec![shape];
    }
    let Some(name) = file.strip_suffix(".md") else {
        return vec![shape];
    };
    let mut bytes = name.bytes();
    let good = name.len() <= MAX_NAME_BYTES
        && bytes
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if good { Vec::new() } else { vec![shape] }
}

/// A git conflict-marker line's head, as the release gate names it.
fn conflict_marker(line: &str) -> Option<&'static str> {
    [
        ("<<<<<<<", "<<<<<<<"),
        ("|||||||", "|||||||"),
        (">>>>>>>", ">>>>>>>"),
    ]
    .into_iter()
    .find(|(head, _)| line == *head || line.starts_with(&format!("{head} ")))
    .map(|(_, name)| name)
    .or((line == "=======").then_some("======="))
}

/// Every rule a fragment at `path` holding `text` breaks, each naming `path line N`; empty for a
/// fragment that ships as written. The reader checks the mode (a regular, non-executable file)
/// and UTF-8 before it has a `&str`.
#[must_use]
pub fn fragment_findings(path: &str, text: &str) -> Vec<String> {
    let mut out = path_findings(path);
    if text.is_empty() {
        out.push(format!("{path}: is empty — it holds one changelog entry"));
        return out;
    }
    if text.len() > MAX_FRAGMENT_BYTES {
        out.push(format!(
            "{path}: is {} bytes, over the {MAX_FRAGMENT_BYTES} one entry may hold",
            text.len()
        ));
    }
    if text.contains(HARNESS_ENTRY_HEAD) {
        out.push(format!(
            "{path}: holds the harness-versions announcement, which `ship release` derives \
             from the raised HARNESS_VER files itself — delete this file"
        ));
    }
    let Some(body) = text.strip_suffix('\n') else {
        out.push(format!(
            "{path}: does not end in a newline — the file ends in exactly one"
        ));
        return out;
    };
    let lines: Vec<&str> = body.split('\n').collect();
    for (at, line) in lines.iter().enumerate() {
        let n = at + 1;
        let mut report = |detail: &str| out.push(format!("{path} line {n}: {detail}"));
        if line.contains('\r') {
            report("contains a carriage return — write the file with LF line ends");
        }
        if let Some(bad) = line
            .chars()
            .find(|c| c.is_control() && *c != '\t' && *c != '\r')
        {
            report(&format!("contains control character {bad:?}"));
        }
        if line.contains("'''") {
            report(
                "contains ''' — this would terminate the release manifest's TOML multiline literal",
            );
        }
        if line.contains("<!--") {
            report("contains an HTML comment, which would ship in the notes as written");
        }
        if let Some(marker) = conflict_marker(line) {
            report(&format!("contains git conflict marker `{marker}`"));
            continue;
        }
        if at == 0 {
            if line.trim().is_empty() {
                report("is blank — the file starts with its entry's `- ` line");
            } else if !line.starts_with("- ") || line[2..].trim().is_empty() {
                report("does not start with `- ` and the entry's text");
            }
            continue;
        }
        if line.is_empty() {
            if at + 1 == lines.len() {
                report("is a trailing blank line — the file ends right after its entry");
            }
            continue;
        }
        if line.starts_with("  ") {
            continue;
        }
        if line.starts_with("## ") || line.starts_with("### ") {
            report(
                "is a heading — the directory names the entry's kind, and a file holds no heading",
            );
        } else if line.starts_with("- ") || line.starts_with("* ") {
            report("starts a second entry — each entry is its own file");
        } else {
            report("indent continuation lines with two spaces");
        }
    }
    out
}
