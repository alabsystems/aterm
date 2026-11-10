// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Changelog gate/roll/extract (release spec §3): the one Rust port of `changelog_real_body` semantics (strip HTML comments, `###` scaffolds, blanks) over the hand-written Keep-a-Changelog `CHANGELOG.md`.

use crate::ledger::{Error, Result};

#[path = "changelog_history.rs"]
mod history;
pub use history::gate as gate_history;

/// The changelog's repo-root-relative path: the one hand-written source of release notes.
pub const CHANGELOG_FILE: &str = "CHANGELOG.md";

/// The head of the line `ship harness-bump` (and `ship release --bump`'s version commit) writes
/// under `[Unreleased]`: generated, so it never counts as a section's release notes.
pub const HARNESS_ENTRY_HEAD: &str = "- **Harness versions since v";

/// What the pre-claim gate learned about a section: `entries`, the count of top-level bullets in the real body, for the transcript only.
#[derive(Debug)]
pub struct GateSummary {
    pub entries: usize,
}

fn is_section_header(line: &str, section: &str) -> bool {
    line.strip_prefix("## [")
        .and_then(|rest| rest.strip_prefix(section))
        .and_then(|rest| rest.strip_prefix(']'))
        .is_some_and(|rest| rest.trim().is_empty() || rest.starts_with([' ', '\t', '-']))
}

/// The real body of a section: the lines between `## [<name>]` and the next `## ` heading, with HTML comments (multi-line included), `###` scaffold headings and blank lines removed.
pub fn real_body(text: &str, section: &str) -> Vec<String> {
    let mut in_section = false;
    let mut in_comment = false;
    let mut out = Vec::new();
    for raw in text.lines() {
        if !in_section {
            in_section = is_section_header(raw, section);
            continue;
        }
        if raw.starts_with("## ") {
            break;
        }
        let rest = if in_comment {
            match raw.find("-->") {
                Some(p) => {
                    in_comment = false;
                    &raw[p + 3..]
                }
                None => continue,
            }
        } else {
            raw
        };
        if !rest.contains("<!--") {
            if rest.trim_start().starts_with("###") || rest.trim().is_empty() {
                continue;
            }
            out.push(rest.to_string());
            continue;
        }
        let mut line = rest.to_string();
        while let Some(start) = line.find("<!--") {
            match line.rfind("-->").filter(|&end| end >= start + 4) {
                Some(end) => line.replace_range(start..end + 3, ""),
                None => break,
            }
        }
        if let Some(start) = line.find("<!--") {
            line.truncate(start);
            in_comment = true;
        }
        if line.trim_start().starts_with("###") {
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        out.push(line);
    }
    out
}

/// Whether the changelog already carries a `## [<version>]` section: the roll's double-roll guard and the claim's "cut elsewhere" abort (ledger.rs) share it.
pub fn has_section(text: &str, version: &str) -> bool {
    text.lines().any(|l| is_section_header(l, version))
}

/// Pre-claim gates: `[Unreleased]` exists, has no entry already released, its real body is non-empty, and the raw section carries no `'''` or raw control character.
/// Leftover git conflict-marker lines (`<<<<<<<`, `|||||||`, `=======`, `>>>>>>>`) in the raw section are also refused.
pub fn gate_unreleased(text: &str) -> Result<GateSummary> {
    gate_section(text, "Unreleased")
}

fn conflict_marker_head(raw: &str) -> Option<&'static str> {
    if raw == "=======" {
        Some("=======")
    } else if raw == "<<<<<<<" || raw.starts_with("<<<<<<< ") {
        Some("<<<<<<<")
    } else if raw == "|||||||" || raw.starts_with("||||||| ") {
        Some("|||||||")
    } else if raw == ">>>>>>>" || raw.starts_with(">>>>>>> ") {
        Some(">>>>>>>")
    } else {
        None
    }
}

/// Refuse rather than reorder the verbatim notes during a cut. Historical sections
/// are left alone; a recut judges the section it will actually publish.
fn refuse_repeated_subsections(text: &str, section: &str) -> Result<()> {
    let Some((start, end)) = section_span(text, section) else {
        return Ok(());
    };
    let mut headings = std::collections::BTreeMap::new();
    let mut in_comment = false;
    for (idx, raw) in text.lines().enumerate().take(end).skip(start + 1) {
        let mut visible = String::new();
        let mut rest = raw;
        loop {
            if in_comment {
                let Some((_, tail)) = rest.split_once("-->") else {
                    break;
                };
                in_comment = false;
                rest = tail;
            } else if let Some((head, tail)) = rest.split_once("<!--") {
                visible.push_str(head);
                in_comment = true;
                rest = tail;
            } else {
                visible.push_str(rest);
                break;
            }
        }
        if let Some(kind) = visible.trim().strip_prefix("### ") {
            let kind = kind
                .trim()
                .trim_end_matches('#')
                .trim()
                .to_ascii_lowercase();
            if let Some(first) = headings.insert(kind.clone(), idx + 1) {
                return Err(Error::new(format!(
                    "{CHANGELOG_FILE}: [{section}] repeats ### {kind} at lines {first} and {} — \
                     combine the subsections before cutting; release notes ship verbatim",
                    idx + 1
                )));
            }
        }
    }
    Ok(())
}

/// The 1-based line of the first top-level entry (`- ` or `* `) above the first `## ` heading,
/// outside HTML comments: an entry there belongs to no section, so no release's notes carry it.
fn entry_above_every_section(text: &str) -> Option<usize> {
    let mut in_comment = false;
    for (idx, raw) in text.lines().enumerate() {
        if raw.starts_with("## ") {
            return None;
        }
        let mut line = raw;
        if in_comment {
            match line.find("-->") {
                Some(p) => {
                    line = &line[p + 3..];
                    in_comment = false;
                }
                None => continue,
            }
        }
        if line.starts_with("- ") || line.starts_with("* ") {
            return Some(idx + 1);
        }
        if let Some(start) = line.rfind("<!--") {
            in_comment = !line[start..].contains("-->");
        }
    }
    None
}

/// The same gate over any section: a recut re-cuts a version whose body an earlier wedged cut already rolled into `## [X.Y.Z]`, so it judges that section (the fresh `[Unreleased]` above it is legitimately empty).
/// An entry above the first `## ` heading is refused too: no section holds it, so the release would drop it without a word.
pub fn gate_section(text: &str, section: &str) -> Result<GateSummary> {
    if !has_section(text, section) {
        return Err(Error::new(format!(
            "{CHANGELOG_FILE} has no \"## [{section}]\" section"
        )));
    }
    if let Some(line) = entry_above_every_section(text) {
        return Err(Error::new(format!(
            "{CHANGELOG_FILE} line {line}: an entry above the first `## ` heading belongs to no \
             section, so no release's notes would carry it — move it under \"## [{section}]\""
        )));
    }
    refuse_repeated_notes(text, section)?;
    let body = real_body(text, section);
    if note_entries(text, section)
        .iter()
        .all(|l| l.starts_with(HARNESS_ENTRY_HEAD))
    {
        return Err(Error::new(format!(
            "the [{section}] section of {CHANGELOG_FILE} has no release notes — add a \
             hand-written \"### Added/Changed/Fixed\" entry before cutting (comments, \
             blank lines, bare ### scaffolds and the generated harness-versions line do not \
             count)"
        )));
    }
    refuse_repeated_subsections(text, section)?;
    crate::harness_versions::notes_gate(text, section)?;
    if let Some((start, end)) = section_span(text, section) {
        for (idx, raw) in text.lines().enumerate().take(end).skip(start + 1) {
            if raw.contains("'''") {
                return Err(Error::new(format!(
                    "{CHANGELOG_FILE} line {}: [{section}] contains ''' — this would \
                     terminate the release manifest's TOML multiline literal; rewrite \
                     the entry (use backticks or double quotes)",
                    idx + 1
                )));
            }
            if let Some(marker) = conflict_marker_head(raw) {
                return Err(Error::new(format!(
                    "{CHANGELOG_FILE} line {}: [{section}] contains git conflict marker \
                     `{marker}` — resolve the merge artifact before cutting",
                    idx + 1
                )));
            }
            if let Some(bad) = raw.chars().find(|c| c.is_control() && *c != '\t') {
                return Err(Error::new(format!(
                    "{CHANGELOG_FILE} line {}: [{section}] contains control character \
                     {bad:?} — TOML forbids it raw inside the release manifest's \
                     multiline literal; rewrite the entry",
                    idx + 1
                )));
            }
        }
    }
    let entries = body
        .iter()
        .filter(|l| l.starts_with("- ") || l.starts_with("* "))
        .count();
    Ok(GateSummary { entries })
}

/// Roll `## [Unreleased]` → `## [<version>] - <date>` (spec §3): the new heading is inserted right below the Unreleased header with one blank line between — the whole current body changes ownership to the release and a fresh EMPTY `[Unreleased]` scaffold is what remains on top.
pub fn roll(text: &str, version: &str, date: &str) -> Result<String> {
    refuse_repeated_notes(text, "Unreleased")?;
    // Double-roll guard: a second `## [X.Y.Z]` would split the notes across two headings,
    // and recut detection keys on this section's presence.
    if has_section(text, version) {
        return Err(Error::new(format!(
            "{CHANGELOG_FILE} already has a \"## [{version}]\" section — already \
             rolled; a recut reuses it instead of rolling again"
        )));
    }
    if !has_section(text, "Unreleased") {
        return Err(Error::new(format!(
            "{CHANGELOG_FILE} has no \"## [Unreleased]\" section"
        )));
    }
    let mut rolled = String::with_capacity(text.len() + version.len() + date.len() + 16);
    let mut done = false;
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            rolled.push('\n');
        }
        rolled.push_str(line);
        if !done && is_section_header(line, "Unreleased") {
            let _ = std::fmt::Write::write_fmt(
                &mut rolled,
                format_args!("\n\n## [{version}] - {date}"),
            );
            done = true;
        }
    }
    if text.ends_with('\n') {
        rolled.push('\n');
    }
    Ok(rolled)
}

/// The two changelogs a release claim writes, from the published commit's `source` and
/// main's current `main`:
///
/// * the release commit's: `source` rolled ([`roll`]), so the notes that ship are exactly
///   the entries the published commit carried (taken as it stands if already rolled);
/// * main's: [`roll_shipped`], the same section with the same shipped body, while main's
///   `[Unreleased]` keeps every entry added after the publish, for the next release
///   (taken as it stands if it already has the section, from an earlier wedged claim).
pub fn claim_changelogs(
    source: &str,
    main: &str,
    version: &str,
    date: &str,
) -> Result<(String, String)> {
    let source_rolled = has_section(source, version);
    let release = release_changelog(source, version, date)?;
    let landing = if has_section(main, version) {
        main.to_string()
    } else if source_rolled {
        return Err(Error::new(format!(
            "the published commit's {CHANGELOG_FILE} already carries \"## [{version}]\" and \
             main's does not — main has dropped a released section; restore it before cutting"
        )));
    } else {
        roll_shipped(main, source, version, date)?
    };
    Ok((release, landing))
}

/// The release commit's changelog ([`claim_changelogs`]' first half, shared by the reservation that writes it and the claim that checks it): `source` rolled for `version` on `date`, or `source` as it stands when it already carries the section.
/// # Errors
/// [`roll`]'s, or an already-rolled section repeats an older release's entry.
pub fn release_changelog(source: &str, version: &str, date: &str) -> Result<String> {
    if has_section(source, version) {
        refuse_repeated_notes(source, version)?;
        Ok(source.to_string())
    } else {
        roll(source, version, date)
    }
}

/// The date of `version`'s section, `## [version] - <date>`: what the release commit's roll was dated, read back so a claim on a later day checks the roll it was made with.
#[must_use]
pub fn section_date<'a>(text: &'a str, version: &str) -> Option<&'a str> {
    text.lines().find_map(|l| {
        l.strip_prefix("## [")?
            .strip_prefix(version)?
            .strip_prefix("] - ")
            .map(str::trim)
    })
}

/// Roll `main` for a release whose notes are `source`'s (the published commit's) `[Unreleased]` body rather than main's own.
pub fn roll_shipped(main: &str, source: &str, version: &str, date: &str) -> Result<String> {
    refuse_repeated_notes(source, "Unreleased")?;
    if has_section(main, version) {
        return Err(Error::new(format!(
            "{CHANGELOG_FILE} already has a \"## [{version}]\" section — already rolled"
        )));
    }
    let shipped = unreleased_body(source)?;
    let lines: Vec<&str> = main.lines().collect();
    let start = lines
        .iter()
        .position(|l| is_section_header(l, "Unreleased"))
        .ok_or_else(|| {
            Error::new(format!(
                "{CHANGELOG_FILE} has no \"## [Unreleased]\" section"
            ))
        })?;
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.starts_with("## "))
        .map_or(lines.len(), |p| start + 1 + p);
    let leftovers = unshipped_entries(&lines[start + 1..end], &shipped);

    let mut out: Vec<&str> = lines[..=start].to_vec();
    out.push("");
    out.extend_from_slice(&leftovers);
    let heading = format!("## [{version}] - {date}");
    out.push(&heading);
    out.extend(shipped.iter().copied());
    out.extend_from_slice(&lines[end..]);
    let mut rolled = out.join("\n");
    if main.ends_with('\n') {
        rolled.push('\n');
    }
    Ok(rolled)
}

/// The raw lines of `text`'s `[Unreleased]` body, between its header and the next
/// `## ` heading.
fn unreleased_body(text: &str) -> Result<Vec<&str>> {
    let mut lines = text.lines();
    if !lines.any(|l| is_section_header(l, "Unreleased")) {
        return Err(Error::new(format!(
            "{CHANGELOG_FILE} has no \"## [Unreleased]\" section"
        )));
    }
    Ok(lines.take_while(|l| !l.starts_with("## ")).collect())
}

/// One unit of a changelog section body: a `###` heading, or an entry — a line
/// that starts a bullet (or a paragraph after a blank or a heading) plus every
/// following non-blank line that does not start another.
enum Unit<'a> {
    Heading(&'a str),
    Entry(Vec<&'a str>),
}

/// Full entries, with comments/scaffolds removed and whitespace folded. The whole
/// entry matters: a follow-up may reuse a bold title while describing a new fix.
/// Harness versions have their own release-window rule in `harness_versions`.
pub(crate) fn note_keys(text: &str, section: &str) -> Vec<String> {
    note_entries(text, section)
        .into_iter()
        .filter(|entry| !entry.starts_with(HARNESS_ENTRY_HEAD))
        .map(|entry| {
            entry
                .strip_prefix("- ")
                .or_else(|| entry.strip_prefix("* "))
                .unwrap_or(&entry)
                .to_string()
        })
        .collect()
}

/// Whole entries with whitespace folded, including harness announcements.
pub(crate) fn note_entries(text: &str, section: &str) -> Vec<String> {
    let body = real_body(text, section);
    let lines: Vec<&str> = body.iter().map(String::as_str).collect();
    units(&lines)
        .into_iter()
        .filter_map(|unit| match unit {
            Unit::Entry(lines) => {
                let joined = lines.join("\n");
                Some(joined.split_whitespace().collect::<Vec<_>>().join(" "))
            }
            _ => None,
        })
        .collect()
}

/// Compare only sections below the candidate. A recut must not compare its notes
/// with a newer release or with a peer's reintroduced Unreleased entry.
fn refuse_repeated_notes(text: &str, section: &str) -> Result<()> {
    let Some((_, end)) = section_span(text, section) else {
        return Ok(());
    };
    let mut released = std::collections::BTreeMap::new();
    for line in text.lines().skip(end) {
        let Some(name) = line
            .strip_prefix("## [")
            .and_then(|s| s.split_once(']').map(|p| p.0))
        else {
            continue;
        };
        let parts: Vec<_> = name.split('.').collect();
        if parts.len() != 3
            || !parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
            || !is_section_header(line, name)
        {
            continue;
        }
        for entry in note_keys(text, name) {
            released.entry(entry).or_insert(name);
        }
    }
    for entry in note_keys(text, section) {
        if let Some(earlier) = released.get(&entry) {
            return Err(Error::new(format!(
                "{CHANGELOG_FILE}: [{section}] repeats an entry already released in [{earlier}]: \
                 {entry} — remove the merged duplicate before cutting"
            )));
        }
    }
    Ok(())
}

fn units<'a>(body: &[&'a str]) -> Vec<Unit<'a>> {
    let mut out = Vec::new();
    let mut entry: Option<Vec<&'a str>> = None;
    for &line in body {
        let starts_entry = line.starts_with("- ") || line.starts_with("* ");
        if (line.trim().is_empty() || line.starts_with("### ") || starts_entry)
            && let Some(done) = entry.take()
        {
            out.push(Unit::Entry(done));
        }
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with("### ") {
            out.push(Unit::Heading(line));
        } else if let Some(open) = entry.as_mut() {
            open.push(line);
        } else {
            entry = Some(vec![line]);
        }
    }
    if let Some(done) = entry {
        out.push(Unit::Entry(done));
    }
    out
}

/// The entries of `main_body` that `shipped` does not carry, each shipped entry
/// consuming at most one match, rendered under their headings with a blank line
/// after every heading and every entry.
fn unshipped_entries<'a>(main_body: &[&'a str], shipped: &[&'a str]) -> Vec<&'a str> {
    let mut remaining: std::collections::HashMap<Vec<&'a str>, usize> =
        std::collections::HashMap::new();
    for unit in units(shipped) {
        if let Unit::Entry(lines) = unit {
            *remaining.entry(lines).or_default() += 1;
        }
    }
    let mut out = Vec::new();
    let mut heading: Option<&'a str> = None;
    let mut heading_written = false;
    for unit in units(main_body) {
        match unit {
            Unit::Heading(line) => {
                heading = Some(line);
                heading_written = false;
            }
            Unit::Entry(lines) => {
                if let Some(count) = remaining.get_mut(lines.as_slice())
                    && *count > 0
                {
                    *count -= 1;
                    continue;
                }
                if let Some(line) = heading
                    && !heading_written
                {
                    out.push(line);
                    out.push("");
                    heading_written = true;
                }
                out.extend_from_slice(&lines);
                out.push("");
            }
        }
    }
    out
}

/// Extract a rolled section's body verbatim, for the manifest, the release body and the in-app notes: the raw lines between `## <version>` and the next `## ` heading, trimmed of leading and trailing blank lines only.
pub fn rolled_body(text: &str, version: &str) -> Result<String> {
    let mut grab = false;
    let mut lines: Vec<&str> = Vec::new();
    for line in text.lines() {
        if !grab {
            grab = is_section_header(line, version);
            continue;
        }
        if line.starts_with("## ") {
            break;
        }
        lines.push(line);
    }
    if !grab {
        return Err(Error::new(format!(
            "{CHANGELOG_FILE} has no \"## [{version}]\" section to extract"
        )));
    }
    let start = lines
        .iter()
        .position(|l| !l.trim().is_empty())
        .unwrap_or(lines.len());
    let end = lines[start..]
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(start, |p| start + p + 1);
    let trimmed = &lines[start..end];
    if trimmed.is_empty() {
        return Err(Error::new(format!(
            "the \"## [{version}]\" section of {CHANGELOG_FILE} is empty — the roll \
             should have made that impossible; refusing to ship note-less"
        )));
    }
    Ok(trimmed.join("\n"))
}

/// The GitHub release body: a standing newcomer preamble, then this release's changelog verbatim.
pub fn release_notes_document(version: &str, changelog_body: &str) -> String {
    bound_release_body(&format!(
        "**aterm** is the terminal for AI. New here? What each file is:\n\
         \n\
         - `aterm-{version}.dmg` — the signed, notarized app as a drag-install DMG. \
         The ALab toolchain installs itself on first launch (or on demand via \
         `aterm pkg install --default-set`).\n\
         - `aterm-{version}-mac.zip` — the same app as a zip; this is the \
         container the in-app updater and Homebrew stage from.\n\
         - `aterm.dmg` / `aterm-mac.zip` — permanent `releases/latest/download/` names \
         for the DMG and the zip above (byte-identical copies).\n\
         - `.sha256` files verify a download: `shasum -a 256 -c <asset>.sha256`.\n\
         - `aterm-appcast.toml` / `aterm-machines.toml` (and their `.sig`) are consumed by \
         the in-app self-updater — not for humans.\n\
         - Releases named `atpkg-index-N` are machine-readable package indexes, not app \
         releases.\n\
         \n\
         ---\n\
         \n\
         {body}\n",
        body = changelog_body,
    ))
}

/// GitHub refuses a release body over this many characters ([`github_length`]), as an opaque `422` on the create POST; the rolled section grows every release.
pub const GITHUB_RELEASE_BODY_LIMIT: usize = 125_000;

/// What GitHub counts against "maximum is 125000 characters": UTF-16 code units, the safe
/// reading for a JSON API.
pub(crate) fn github_length(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The changelog body as GitHub will accept it: verbatim when it fits, otherwise cut at the last section boundary that does, with a tail naming what was dropped and where to read it (a body ending mid-sentence reads as corruption).
#[must_use]
pub fn bound_release_body(body: &str) -> String {
    if github_length(body) <= GITHUB_RELEASE_BODY_LIMIT {
        return body.to_string();
    }
    let tail = "\n\n---\n\n*These notes were longer than GitHub accepts in a release body \
                (125,000 characters). The entries above are complete as far as they go; the \
                rest of this release's changelog is in `CHANGELOG.md` in the source tree at \
                this tag.*\n";
    let budget_units = GITHUB_RELEASE_BODY_LIMIT.saturating_sub(github_length(tail));
    let mut budget = body.len();
    let mut spent = 0usize;
    for (at, ch) in body.char_indices() {
        if spent + ch.len_utf16() > budget_units {
            budget = at;
            break;
        }
        spent += ch.len_utf16();
    }
    let head = &body[..budget];
    let cut = head
        .rfind("\n### ")
        .or_else(|| head.rfind("\n## "))
        .unwrap_or(budget);
    format!("{}{tail}", &body[..cut])
}

#[cfg(test)]
mod release_body_bound_tests {
    use super::{GITHUB_RELEASE_BODY_LIMIT, bound_release_body, release_notes_document};

    /// One that does not fit ends on a whole entry, under the limit, saying so.
    #[test]
    fn an_oversized_body_is_cut_at_a_section_and_says_it_was() {
        let entry = "### Section\n\n- an entry long enough to matter, repeated to overflow\n\n";
        let body = entry.repeat(4000);
        assert!(
            body.len() > GITHUB_RELEASE_BODY_LIMIT,
            "fixture must actually overflow: {} bytes",
            body.len()
        );
        let bounded = bound_release_body(&body);
        assert!(
            bounded.len() <= GITHUB_RELEASE_BODY_LIMIT,
            "bounded body is still {} bytes, over the {GITHUB_RELEASE_BODY_LIMIT} GitHub takes",
            bounded.len()
        );
        let kept = bounded
            .split("\n\n---\n\n*These notes")
            .next()
            .unwrap_or_default();
        assert!(
            kept.ends_with("\n\n") || kept.ends_with('\n'),
            "the kept text must end on a whole entry"
        );
    }

    /// A budget that lands inside a multi-byte character does not panic: em dashes cover
    /// the whole budget neighbourhood across several pads, rather than one guessed offset.
    #[test]
    fn a_cut_that_falls_inside_a_character_does_not_panic() {
        for pad in 0..8usize {
            let body = format!(
                "### Section\n\n{}{}\n",
                "a".repeat(pad),
                "—x".repeat(GITHUB_RELEASE_BODY_LIMIT)
            );
            let bounded = bound_release_body(&body);
            assert!(
                super::github_length(&bounded) <= GITHUB_RELEASE_BODY_LIMIT,
                "pad {pad}: bounded body is {} units",
                super::github_length(&bounded)
            );
        }
    }

    /// The limit counts characters, not bytes: em dashes are three bytes each.
    #[test]
    fn the_limit_counts_characters_not_bytes() {
        let body = "—".repeat(GITHUB_RELEASE_BODY_LIMIT - 1);
        assert!(
            body.len() > GITHUB_RELEASE_BODY_LIMIT,
            "fixture must overflow a BYTE budget: {} bytes",
            body.len()
        );
        assert_eq!(
            bound_release_body(&body),
            body,
            "a body inside the character limit must survive whole"
        );
    }

    /// The whole document, preamble plus body, lands under the limit: GitHub counts the
    /// document, not the section inside it.
    #[test]
    fn the_assembled_document_fits_what_github_accepts() {
        let body = "### Section\n\n- entry — with em dashes — in UTF-8\n\n".repeat(4000);
        let doc = release_notes_document("0.87.0", &body);
        assert!(
            super::github_length(&doc) <= GITHUB_RELEASE_BODY_LIMIT,
            "assembled release notes are {} UTF-16 units ({} bytes), over the \
             {GITHUB_RELEASE_BODY_LIMIT} GitHub takes",
            super::github_length(&doc),
            doc.len()
        );
    }

    /// Bounding an already-bounded body is a no-op, byte for byte, so the POST can re-bound
    /// whatever it reads off disk.
    #[test]
    fn bounding_an_already_bounded_body_changes_nothing() {
        let overflowing = "### Section\n\n- an entry long enough to matter\n\n".repeat(5000);
        assert!(overflowing.len() > GITHUB_RELEASE_BODY_LIMIT);
        let once = bound_release_body(&overflowing);
        let twice = bound_release_body(&once);
        assert_eq!(once, twice, "the bound is not idempotent");
        let doc = release_notes_document("0.87.0", &overflowing);
        assert_eq!(doc, bound_release_body(&doc));
    }
}

/// Today's date in America/Los_Angeles as `YYYY-MM-DD`, from the zone's TZif file.
pub fn today_la() -> Result<String> {
    const ZONE: &str = "America/Los_Angeles";
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .ok_or_else(|| Error::new("the clock is before 1970"))?;
    let offset = aterm_uds::zone::zone_offset_at(ZONE, now)
        .ok_or_else(|| Error::new(format!("no {ZONE} zone file under /usr/share/zoneinfo")))?;
    Ok(la_date(now, offset))
}

/// `unix` on a clock `offset` seconds east of UTC, as `YYYY-MM-DD`.
fn la_date(unix: i64, offset: i64) -> String {
    let (y, m, d) = aterm_types::rfc3339::civil_from_days((unix + offset).div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// 0-based (start, end) line indices of a section: `start` is the `## [name]` header, `end`
/// the next `## ` heading (or one past the last line).
pub(crate) fn section_span(text: &str, section: &str) -> Option<(usize, usize)> {
    let mut start = None;
    let mut count = 0;
    for (idx, line) in text.lines().enumerate() {
        count = idx + 1;
        if let Some(s) = start {
            if line.starts_with("## ") {
                return Some((s, idx));
            }
        } else if is_section_header(line, section) {
            start = Some(idx);
        }
    }
    start.map(|s| (s, count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_notes_repeated_subsections_refuse_before_cut() {
        let doc = "## [Unreleased]\n\n### Fixed\n\n- first fix\n\n### Changed\n\n- a change\n\n### Fixed\n\n- another fix\n\n## [0.1.0] - date\n\n### Fixed\n\n- older fix\n";
        let err = gate_unreleased(doc).unwrap_err().to_string();
        assert!(
            err.contains("repeats ### fixed") && err.contains("lines"),
            "{err}"
        );
        let coalesced = doc.replace("### Fixed\n\n- another fix\n\n", "").replacen(
            "- first fix",
            "- first fix\n\n- another fix",
            1,
        );
        assert!(gate_unreleased(&coalesced).is_ok());
        let comment = doc.replacen(
            "### Fixed\n\n- another fix",
            "<!-- ### Fixed -->\n\n- another fix",
            1,
        );
        assert!(gate_unreleased(&comment).is_ok());
        let recut = doc.replacen("## [Unreleased]", "## [0.2.0]", 1);
        assert!(gate_section(&recut, "0.2.0").is_err());
    }

    #[test]
    fn release_notes_harness_window_refuses_stale_announcements() {
        let doc = "## [Unreleased]\n\n### Changed\n\n- a new fix\n\n- **Harness versions since v0.193.0: `alab claude harness 0.31`.**\n\n## [0.194.0] - date\n\n- `alab claude harness 0.31` records the upgrade changes.\n\n## [0.193.0] - date\n\n- older change\n";
        let err = gate_unreleased(doc).unwrap_err().to_string();
        assert!(
            err.contains("immediately preceding") && err.contains("0.194.0"),
            "{err}"
        );
    }

    #[test]
    fn release_notes_harness_version_must_be_new() {
        let doc = "## [Unreleased]\n\n- a new fix\n\n- **Harness versions since v0.193.0: `alab claude harness 0.31`.**\n\n## [0.194.0] - date\n\n- `alab claude harness 0.31` records the upgrade changes.\n";
        let dated = doc.replacen("since v0.193.0", "since v0.194.0", 1);
        let err = gate_unreleased(&dated).unwrap_err().to_string();
        assert!(
            err.contains("already announced") && err.contains("0.31"),
            "{err}"
        );
        let fresh = dated.replacen("harness 0.31", "harness 0.32", 1);
        assert!(gate_unreleased(&fresh).is_ok());
        assert!(gate_unreleased(&fresh.replace("harness 0.32", "harness\n  0.32")).is_ok());
        let recut = dated.replacen("## [Unreleased]", "## [0.195.0]", 1);
        assert!(gate_section(&recut, "0.195.0").is_err());
    }

    /// The original gate accepted this merge artifact. Each refusal assertion is
    /// a negative control against removing the duplicate check at that boundary.
    #[test]
    fn repeated_notes_cannot_pass_the_gate_or_roll() {
        let doc = "## [Unreleased]\n\n- **Fix.** Same  words.\n  Continued here.\n\n## [0.2.0] - date\n\n- newer unrelated fix\n\n## [0.1.0] - date\n\n* **Fix.** Same words. Continued here.\n";
        let err = gate_unreleased(doc).unwrap_err().to_string();
        assert!(
            err.contains("[Unreleased]") && err.contains("[0.1.0]"),
            "{err}"
        );
        assert!(roll(doc, "0.3.0", "date").is_err());
        assert!(release_changelog(doc, "0.3.0", "date").is_err());
        assert!(roll_shipped(doc, doc, "0.3.0", "date").is_err());
        let recut = doc.replace("## [Unreleased]", "## [0.3.0] - date");
        assert!(gate_section(&recut, "0.3.0").is_err());
        assert!(release_changelog(&recut, "0.3.0", "date").is_err());

        // Same bold lead, new description: a legitimate follow-up survives.
        let followup = doc.replacen("Same  words.", "A further fix.", 1);
        assert!(gate_unreleased(&followup).is_ok());
        assert!(roll(&followup, "0.3.0", "date").is_ok());
        // Removing the older entry also changes the verdict.
        assert!(
            gate_unreleased(&doc.replace(
                "* **Fix.** Same words. Continued here.",
                "- distinct older entry"
            ))
            .is_ok()
        );
    }

    #[test]
    fn harness_entries_keep_their_own_rule_and_recuts_ignore_newer_notes() {
        let harness = format!("{HARNESS_ENTRY_HEAD}0.2.0: `alab claude harness 0.32`.**");
        let old_harness = format!("{HARNESS_ENTRY_HEAD}0.1.0: `alab claude harness 0.31`.**");
        let doc = format!(
            "## [Unreleased]\n\n- fresh note\n\n{harness}\n\n## [0.2.0] - date\n\n- old note\n\n{old_harness}\n\n## [0.1.0] - date\n\n- oldest note\n"
        );
        assert!(gate_unreleased(&doc).is_ok());
        let duplicate = doc.replace("fresh note", "old note");
        assert!(gate_unreleased(&duplicate).is_err());
        assert!(gate_section(&duplicate, "0.2.0").is_ok());
    }

    /// TODAY IN LOS ANGELES IS WHAT `date` SAYS under `TZ=America/Los_Angeles` (the reader
    /// [`today_la`] replaced, as its oracle; read between two of ours, so a midnight between
    /// them is no failure). NEGATIVE CONTROL: the zone's offset moves the day, so a midnight
    /// UTC is still the day before there.
    #[test]
    fn today_in_los_angeles_is_what_date_says() {
        assert_eq!(la_date(1_792_627_200, -7 * 3600), "2026-10-21");
        assert_eq!(la_date(1_792_627_200, 0), "2026-10-22");
        let before = today_la().expect("the zone file");
        let out = aterm_uds::spawnfd::command(aterm_uds::os_path!("date"))
            .env("TZ", "America/Los_Angeles")
            .arg("+%Y-%m-%d")
            .output()
            .expect("run date");
        let after = today_la().expect("the zone file");
        let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert!(
            said == before || said == after,
            "{said} vs {before}..{after}"
        );
    }

    const SOURCE: &str = "# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n- **One** — shipped.\n  A continuation line.\n\n- **Two** — shipped.\n\n## [0.91.0] - 2026-09-22\n\n- older.\n";

    /// Main that has not moved: the roll is exactly [`roll`], byte for byte.
    #[test]
    fn roll_shipped_of_an_unmoved_main_is_the_plain_roll() {
        assert_eq!(
            roll_shipped(SOURCE, SOURCE, "0.92.0", "2026-09-23").unwrap(),
            roll(SOURCE, "0.92.0", "2026-09-23").unwrap()
        );
    }

    /// Main moved after the publish: what the published commit carried ships, what was
    /// added after stays unreleased, and an edited entry's new text stays while its old
    /// text ships.
    #[test]
    fn roll_shipped_keeps_what_peers_added_after_the_publish_unreleased() {
        let main = SOURCE
            .replace(
                "### Fixed\n\n- **One**",
                "### Added\n\n- **New** — a peer's.\n\n### Fixed\n\n- **Peer fix** — after the publish.\n\n- **One**",
            )
            .replace("- **Two** — shipped.", "- **Two** — shipped, then reworded.");
        let rolled = roll_shipped(&main, SOURCE, "0.92.0", "2026-09-23").unwrap();
        assert_eq!(
            rolled,
            "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- **New** — a peer's.\n\n\
             ### Fixed\n\n- **Peer fix** — after the publish.\n\n\
             - **Two** — shipped, then reworded.\n\n\
             ## [0.92.0] - 2026-09-23\n\n### Fixed\n\n- **One** — shipped.\n  A continuation line.\n\n\
             - **Two** — shipped.\n\n## [0.91.0] - 2026-09-22\n\n- older.\n"
        );
        let plain = roll(&main, "0.92.0", "2026-09-23").unwrap();
        assert!(
            rolled_body(&plain, "0.92.0").unwrap().contains("a peer's"),
            "{plain}"
        );
        assert!(
            !rolled_body(&rolled, "0.92.0").unwrap().contains("a peer's"),
            "{rolled}"
        );
    }

    #[test]
    fn claim_changelogs_takes_an_existing_section_as_it_stands() {
        let rolled = roll(SOURCE, "0.92.0", "2026-09-23").unwrap();
        let (release, landing) = claim_changelogs(SOURCE, &rolled, "0.92.0", "2026-09-24").unwrap();
        assert_eq!(release, roll(SOURCE, "0.92.0", "2026-09-24").unwrap());
        assert_eq!(landing, rolled, "main is never rolled twice");
        let (release, landing) =
            claim_changelogs(&rolled, &rolled, "0.92.0", "2026-09-24").unwrap();
        assert_eq!(
            (release.as_str(), landing.as_str()),
            (rolled.as_str(), rolled.as_str())
        );
        claim_changelogs(&rolled, SOURCE, "0.92.0", "2026-09-24").unwrap_err();
    }

    /// The release body carries the changelog VERBATIM below the rule — the "used
    /// once, verbatim" contract (spec §3) is about the notes themselves, and the
    /// preamble must never edit them.
    #[test]
    fn the_release_body_is_preamble_then_the_changelog_verbatim() {
        let body = "### Fixed\n- a thing\n- another";
        let doc = release_notes_document("0.44.0", body);
        assert!(doc.ends_with(&format!("\n---\n\n{body}\n")), "{doc}");
    }

    /// NEGATIVE CONTROL: non-bracketed `## ` headings end a section consistently across
    /// `real_body`, `rolled_body`, and `gate_section`, and suffix collisions on `## [0.1.0]extra`
    /// do not match `0.1.0`.
    #[test]
    fn section_headers_are_exact_and_stop_at_any_h2() {
        let doc = "# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n\
                   <!-- a -->- **Kept** — real note.\n\n\
                   ## [0.1.0]extra\n\n- wrong\n\n\
                   ## [0.1.0] - 2026-10-01\n\n### Fixed\n\n- shipped\n\n\
                   ## Appendix\n\n'''\n";
        assert_eq!(
            real_body(doc, "Unreleased"),
            vec!["- **Kept** — real note."]
        );
        assert_eq!(gate_unreleased(doc).unwrap().entries, 1);
        assert!(has_section(doc, "0.1.0"));
        assert!(!has_section(doc, "0.2.0"));
        assert_eq!(rolled_body(doc, "0.1.0").unwrap(), "### Fixed\n\n- shipped");
        assert!(gate_section(doc, "0.1.0").is_ok());
    }
}
