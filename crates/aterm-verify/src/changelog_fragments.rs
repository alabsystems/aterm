// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A change's changelog entry is its own file, `changes/<kind>/<name>.md`, never an edit to
//! `CHANGELOG.md`: two branches' entries are then disjoint paths, so neither re-judges nor
//! conflicts with the other, and `targo --unverified ship release` writes `[Unreleased]` from
//! the files ([`assemble`]) and deletes them in its release commit.
//!
//! `<kind>` is a Keep a Changelog kind in lowercase (`added`, `changed`, `deprecated`,
//! `removed`, `fixed`, `security`); `<name>` is `[a-z0-9][a-z0-9-]*`, at most
//! [`MAX_NAME_BYTES`] bytes, conventionally `<session>-<slug>`. The file holds exactly the bytes
//! that ship: one entry, a first line that starts `- `, later lines empty or indented two
//! spaces, one final newline and nothing else ([`fragment_findings`]).
//!
//! The rules live here, in the dev set's crate, rather than in `aterm-census`, which most of the
//! workspace depends on: `aterm-release`, the one other reader, depends on this crate.

use aterm_census::changelog::{CHANGELOG_FILE, HARNESS_ENTRY_HEAD, KINDS, is_section_header};

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
fn kind_dirs() -> String {
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

/// Which section of `CHANGELOG.md` a top-level entry sits in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section<'a> {
    /// `## [X.Y.Z] …`: a released section, by its version.
    Released(&'a str),
    /// `## [Unreleased]`, above every `## ` heading, or under one naming no release.
    Unreleased,
}

/// One top-level entry of `CHANGELOG.md`: its section, its 1-based first line and its whole
/// text (the `- ` or `* ` line and every line after it up to the next that is neither blank nor
/// indented, the blank lines between them included).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry<'a> {
    pub section: Section<'a>,
    pub line: usize,
    pub text: String,
}

/// The release a `## ` heading opens, if it names one.
fn released(heading: &str) -> Option<&str> {
    let (name, after) = heading.strip_prefix("## [")?.split_once(']')?;
    if !(after.trim().is_empty() || after.starts_with([' ', '\t', '-'])) {
        return None;
    }
    let parts: Vec<&str> = name.split('.').collect();
    (parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())))
    .then_some(name)
}

/// Every top-level entry of `changelog`, in file order, outside HTML comments.
#[must_use]
pub fn entries(changelog: &str) -> Vec<Entry<'_>> {
    let lines: Vec<&str> = changelog.lines().collect();
    let mut out = Vec::new();
    let mut section = Section::Unreleased;
    let mut comment = false;
    let mut at = 0;
    while at < lines.len() {
        let line = lines[at];
        if comment {
            comment = !line.contains("-->");
            at += 1;
            continue;
        }
        if line.starts_with("## ") {
            section = released(line).map_or(Section::Unreleased, Section::Released);
            at += 1;
            continue;
        }
        if !(line.starts_with("- ") || line.starts_with("* ")) {
            if let Some(start) = line.rfind("<!--") {
                comment = !line[start..].contains("-->");
            }
            at += 1;
            continue;
        }
        let mut end = at + 1;
        let mut next = end;
        while let Some(more) = lines.get(next) {
            if more.trim().is_empty() {
                next += 1;
            } else if more.starts_with([' ', '\t']) {
                next += 1;
                end = next;
            } else {
                break;
            }
        }
        out.push(Entry {
            section,
            line: at + 1,
            text: lines[at..end].join("\n"),
        });
        at = end;
    }
    out
}

/// `changelog` with its `[Unreleased]` body written from `fragments`: each kind present under
/// its `### ` heading in Keep a Changelog order, a blank line after every heading and entry,
/// `announcement` (the derived harness-versions line) first under `### Changed`, then, within a
/// kind, the entries `[Unreleased]` itself holds in file order, then the fragments in byte order
/// of file name. Every line outside `[Unreleased]`'s body stays as it is. This is a pure function
/// of the tree, so a clean clone, the reservation and its recheck write the same bytes.
///
/// # Errors
/// Every finding at once: a fragment [`fragment_findings`] refuses, a bound exceeded, no
/// `## [Unreleased]`, an `[Unreleased]` body holding anything but `### <kind>` headings,
/// entries, blank lines and closed HTML comments, or one entry the window holds twice
/// ([`entry_key`]: two files, a file and a raw entry, or two raw entries), each place named.
pub fn assemble(
    changelog: &str,
    fragments: &[Fragment],
    announcement: Option<&str>,
) -> Result<String, Vec<String>> {
    let mut found = Vec::new();
    if fragments.len() > MAX_FRAGMENTS {
        found.push(format!(
            "{FRAGMENT_DIR}/ holds {} entries, over the {MAX_FRAGMENTS} one release may ship",
            fragments.len()
        ));
    }
    let total: usize = fragments.iter().map(|f| f.text.len()).sum();
    if total > MAX_TOTAL_BYTES {
        found.push(format!(
            "{FRAGMENT_DIR}/ holds {total} bytes of entries, over the {MAX_TOTAL_BYTES} one \
             release may ship"
        ));
    }
    for fragment in fragments {
        found.extend(fragment_findings(&fragment.path, &fragment.text));
    }
    let lines: Vec<&str> = changelog.lines().collect();
    let Some(start) = lines
        .iter()
        .position(|l| is_section_header(l, "Unreleased"))
    else {
        found.push("CHANGELOG.md has no \"## [Unreleased]\" section".to_string());
        return Err(found);
    };
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.starts_with("## "))
        .map_or(lines.len(), |p| start + 1 + p);
    let mut by_kind: Vec<Vec<String>> = vec![Vec::new(); KINDS.len()];
    let mut lead = Vec::new();
    // Every entry the window ships, by its words, with where it came from, in render order.
    let mut seen: Vec<(String, String)> = Vec::new();
    for raw in raw_entries(&lines, start, end, &mut found) {
        seen.push((
            entry_key(&raw.text),
            format!("{CHANGELOG_FILE} line {}", raw.line),
        ));
        match raw.kind {
            Some(rank) => by_kind[rank].push(raw.text),
            None => lead.push(raw.text),
        }
    }
    let mut sorted: Vec<&Fragment> = fragments.iter().collect();
    sorted.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    for fragment in sorted {
        if let Some(rank) = kind_of(&fragment.path).and_then(|k| KINDS.iter().position(|x| *x == k))
        {
            let text = fragment.text.strip_suffix('\n').unwrap_or(&fragment.text);
            seen.push((entry_key(text), fragment.path.clone()));
            by_kind[rank].push(text.to_string());
        }
    }
    for (at, (key, place)) in seen.iter().enumerate() {
        if let Some((_, first)) = seen[..at].iter().find(|(other, _)| other == key) {
            found.push(format!(
                "{place}: repeats the entry {first} holds, so the release would ship it twice — \
                 delete one"
            ));
        }
    }
    if !found.is_empty() {
        return Err(found);
    }
    let changed = KINDS.iter().position(|k| *k == "Changed").unwrap_or(1);
    if let Some(line) = announcement {
        by_kind[changed].insert(0, line.to_string());
    }
    let mut out: Vec<&str> = lines[..=start].to_vec();
    out.push("");
    // Serves the drain: a raw entry written under no heading ships first, where the roll
    // always placed it; it goes once [Unreleased] holds none.
    for entry in &lead {
        out.push(entry);
        out.push("");
    }
    let headings: Vec<String> = KINDS.iter().map(|k| format!("### {k}")).collect();
    for (rank, entries) in by_kind.iter().enumerate() {
        if entries.is_empty() {
            continue;
        }
        out.push(&headings[rank]);
        out.push("");
        for entry in entries {
            out.push(entry);
            out.push("");
        }
    }
    if end == lines.len() {
        while out.len() > start + 1 && out.last() == Some(&"") {
            out.pop();
        }
    }
    out.extend_from_slice(&lines[end..]);
    let mut text = out.join("\n");
    if changelog.ends_with('\n') {
        text.push('\n');
    }
    Ok(text)
}

/// One raw entry `[Unreleased]` holds: its 1-based first line, its kind's rank in [`KINDS`]
/// (`None` under no heading) and its text.
struct Raw {
    line: usize,
    kind: Option<usize>,
    text: String,
}

/// Every rule `changelog`'s raw `[Unreleased]` body breaks, by 1-based line and message, as
/// [`assemble`] refuses it: what the dev rules hold a branch's added lines to. Empty where the
/// file has no `## [Unreleased]`.
/// Serves the drain of entries written before fragments; it goes once `[Unreleased]` holds none.
#[must_use]
pub fn unreleased_findings(changelog: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = changelog.lines().collect();
    let Some(start) = lines
        .iter()
        .position(|l| is_section_header(l, "Unreleased"))
    else {
        return Vec::new();
    };
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.starts_with("## "))
        .map_or(lines.len(), |p| start + 1 + p);
    let mut lined = Vec::new();
    raw_entries_lined(&lines, start, end, &mut lined);
    lined
}

/// The text of every raw entry `changelog`'s `[Unreleased]` holds, harness lines aside, as
/// [`assemble`] ships it: what a release cut from `changelog` shipped raw. Empty where the file
/// has no `## [Unreleased]`.
/// Serves the drain of entries written before fragments; it goes once `[Unreleased]` holds none.
#[must_use]
pub fn unreleased_entries(changelog: &str) -> Vec<String> {
    let lines: Vec<&str> = changelog.lines().collect();
    let Some(start) = lines
        .iter()
        .position(|l| is_section_header(l, "Unreleased"))
    else {
        return Vec::new();
    };
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.starts_with("## "))
        .map_or(lines.len(), |p| start + 1 + p);
    let mut lined = Vec::new();
    raw_entries_lined(&lines, start, end, &mut lined)
        .into_iter()
        .map(|raw| raw.text)
        .collect()
}

/// The entries `[Unreleased]` (lines `start+1..end`) holds, in file order, each with its kind
/// (`None` under no heading); a closed HTML comment is no entry and ships nowhere.
/// Serves the drain of entries written before fragments; it goes, and an `[Unreleased]` body
/// is then refused outright, once `[Unreleased]` holds none.
fn raw_entries(lines: &[&str], start: usize, end: usize, found: &mut Vec<String>) -> Vec<Raw> {
    let mut lined = Vec::new();
    let out = raw_entries_lined(lines, start, end, &mut lined);
    found.extend(lined.into_iter().map(|(_, message)| message));
    out
}

/// [`raw_entries`], its findings in `lined` by line as well as in words.
fn raw_entries_lined(
    lines: &[&str],
    start: usize,
    end: usize,
    lined: &mut Vec<(usize, String)>,
) -> Vec<Raw> {
    let mut out = Vec::new();
    let mut kind: Option<usize> = None;
    let mut at = start + 1;
    while at < end {
        let line = lines[at];
        let n = at + 1;
        if line.trim().is_empty() {
            at += 1;
            continue;
        }
        if line.trim_start().starts_with("<!--") {
            let Some(close) = lines[at..end].iter().position(|l| l.contains("-->")) else {
                lined.push((
                    n,
                    format!(
                        "{CHANGELOG_FILE} line {n}: an HTML comment that never closes inside \
                         [Unreleased] — it would hide every entry after it from the notes; \
                         close it with -->"
                    ),
                ));
                return out;
            };
            at += close + 1;
            continue;
        }
        if let Some(name) = line.strip_prefix("### ") {
            match KINDS.iter().position(|k| *k == name.trim()) {
                Some(rank) => kind = Some(rank),
                None => lined.push((
                    n,
                    format!(
                        "{CHANGELOG_FILE} line {n}: [Unreleased] has an unknown subsection \
                         {line:?}"
                    ),
                )),
            }
            at += 1;
            continue;
        }
        if !(line.starts_with("- ") || line.starts_with("* ")) {
            lined.push((
                n,
                format!(
                    "{CHANGELOG_FILE} line {n}: [Unreleased] holds text that is no entry — a \
                     change's changelog entry is its own file, changes/<kind>/<name>.md (kind: \
                     {}); move it there",
                    kind_dirs()
                ),
            ));
            at += 1;
            continue;
        }
        let mut last = at;
        let mut next = at + 1;
        while next < end {
            let more = lines[next];
            if more.starts_with("### ") || more.starts_with("- ") || more.starts_with("* ") {
                break;
            }
            if more.trim().is_empty() {
                // A blank line joins the entry only when an indented line follows it.
                match lines[next..end].iter().position(|l| !l.trim().is_empty()) {
                    Some(p) if lines[next + p].starts_with([' ', '\t']) => {
                        last = next + p;
                        next = last + 1;
                    }
                    _ => break,
                }
            } else {
                last = next;
                next += 1;
            }
        }
        let text = lines[at..=last].join("\n");
        if !text.starts_with(HARNESS_ENTRY_HEAD) {
            out.push(Raw {
                line: n,
                kind,
                text,
            });
        }
        at = last + 1;
    }
    out
}

#[cfg(test)]
#[path = "changelog_fragments_tests.rs"]
mod tests;
