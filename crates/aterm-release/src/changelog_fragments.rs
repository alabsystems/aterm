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
//! spaces, one final newline and nothing else ([`fragment_findings`]). The census guard
//! refuses malformed fragments before landing; the release independently refuses a window
//! that breaks a rule ([`assemble`]).

use super::rules::{CHANGELOG_FILE, is_section_header};
pub use aterm_census::release_notes::*;

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
    raw_entries(&lines, start, end, &mut Vec::new())
        .into_iter()
        .map(|raw| raw.text)
        .collect()
}

/// The entries `[Unreleased]` (lines `start+1..end`) holds, in file order, each with its kind
/// (`None` under no heading); a closed HTML comment is no entry and ships nowhere.
/// Serves the drain of entries written before fragments; it goes, and an `[Unreleased]` body
/// is then refused outright, once `[Unreleased]` holds none.
fn raw_entries(lines: &[&str], start: usize, end: usize, found: &mut Vec<String>) -> Vec<Raw> {
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
                found.push(format!(
                    "{CHANGELOG_FILE} line {n}: an HTML comment that never closes inside \
                     [Unreleased] — it would hide every entry after it from the notes; \
                     close it with -->"
                ));
                return out;
            };
            at += close + 1;
            continue;
        }
        if let Some(name) = line.strip_prefix("### ") {
            match KINDS.iter().position(|k| *k == name.trim()) {
                Some(rank) => kind = Some(rank),
                None => found.push(format!(
                    "{CHANGELOG_FILE} line {n}: [Unreleased] has an unknown subsection \
                     {line:?}"
                )),
            }
            at += 1;
            continue;
        }
        if !(line.starts_with("- ") || line.starts_with("* ")) {
            found.push(format!(
                "{CHANGELOG_FILE} line {n}: [Unreleased] holds text that is no entry — a \
                 change's changelog entry is its own file, changes/<kind>/<name>.md (kind: \
                 {}); move it there",
                kind_dirs()
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
