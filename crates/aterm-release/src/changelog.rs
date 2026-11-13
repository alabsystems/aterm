// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Changelog gate/roll/extract (release spec §3): the one Rust port of `changelog_real_body` semantics (strip HTML comments, `###` scaffolds, blanks) over the hand-written Keep-a-Changelog `CHANGELOG.md`.

use crate::ledger::{Error, Result};

pub use aterm_census::changelog::{
    CHANGELOG_FILE, GateSummary, HARNESS_ENTRY_HEAD, has_section, real_body,
};
use aterm_census::changelog::{body_line, is_section_header};

#[path = "changelog_history.rs"]
mod history;
pub use history::gate as gate_history;
#[path = "changelog_published.rs"]
mod published;
#[cfg(test)]
pub(crate) use history::gate_with_keys as gate_history_with_keys;
pub(crate) use published::gate as gate_published;

/// Count each [`note_keys`] key across sections, excluding the next release's
/// Unreleased after a recut's roll. A move preserves the count; a surviving
/// misplaced copy cannot hide the removal and restoration of another copy.
#[derive(Default)]
pub(crate) struct NoteCounter {
    sections: std::collections::BTreeMap<String, usize>,
    bodies: Vec<(String, std::collections::BTreeMap<String, usize>)>,
    snapshots: std::collections::BTreeMap<String, Vec<usize>>,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct NoteMembership {
    pub(crate) anywhere: usize,
    pub(crate) copies: usize,
    pub(crate) active: usize,
}

impl NoteCounter {
    /// Decode each distinct section's bytes once. Most release sections repeat
    /// unchanged across thousands of snapshots; only their small IDs repeat here.
    pub(crate) fn counts(
        &mut self,
        oid: &str,
        snapshot: Option<&str>,
        section: &str,
        candidates: &[String],
    ) -> Option<Vec<NoteMembership>> {
        if !self.snapshots.contains_key(oid) {
            let text = snapshot?;
            let mut starts = Vec::new();
            let mut offset = 0;
            for line in text.split_inclusive('\n') {
                if line.starts_with("## ") {
                    starts.push(offset);
                }
                offset += line.len();
            }
            starts.push(text.len());
            let mut ids = Vec::new();
            for span in starts.windows(2) {
                let source = &text[span[0]..span[1]];
                let header = source.lines().next().unwrap_or("");
                if !header.starts_with("## [") {
                    continue;
                }
                let id = if let Some(id) = self.sections.get(source) {
                    *id
                } else {
                    let mut in_comment = false;
                    let body: Vec<_> = source
                        .lines()
                        .skip(1)
                        .filter_map(|line| body_line(line, &mut in_comment))
                        .collect();
                    let mut counts = std::collections::BTreeMap::new();
                    for key in keys_of(&body) {
                        *counts.entry(key).or_default() += 1;
                    }
                    let id = self.bodies.len();
                    self.bodies.push((header.to_owned(), counts));
                    self.sections.insert(source.to_owned(), id);
                    id
                };
                ids.push(id);
            }
            self.snapshots.insert(oid.to_owned(), ids);
        }
        let ids = &self.snapshots[oid];
        let rolled = ids
            .iter()
            .any(|id| is_section_header(&self.bodies[*id].0, section));
        let active = if rolled { section } else { "Unreleased" };
        let skip = section != "Unreleased" && rolled;
        let mut held = vec![NoteMembership::default(); candidates.len()];
        for id in ids {
            let (header, counts) = &self.bodies[*id];
            let excluded = skip && is_section_header(header, "Unreleased");
            for (index, key) in candidates.iter().enumerate() {
                let count = counts.get(key).copied().unwrap_or(0);
                held[index].anywhere += count;
                if excluded {
                    continue;
                }
                held[index].copies += count;
                if is_section_header(header, active) {
                    held[index].active += count;
                }
            }
        }
        Some(held)
    }
}

/// Judge the notes the next release will ship.
pub fn gate_unreleased(text: &str) -> Result<GateSummary> {
    gate_section(text, "Unreleased")
}

/// Judge a release or recut through the shared structural rules.
pub fn gate_section(text: &str, section: &str) -> Result<GateSummary> {
    let found = aterm_census::changelog::gate_section(text, section).map_err(Error::new)?;
    crate::harness_versions::notes_gate(text, section)?;
    Ok(found)
}

/// Roll `## [Unreleased]` → `## [<version>] - <date>` (spec §3): the new heading is inserted right below the Unreleased header with one blank line between — the whole current body changes ownership to the release and a fresh EMPTY `[Unreleased]` scaffold is what remains on top.
pub fn roll(text: &str, version: &str, date: &str) -> Result<String> {
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
/// [`roll`]'s.
pub fn release_changelog(source: &str, version: &str, date: &str) -> Result<String> {
    if has_section(source, version) {
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
    keys_of(&real_body(text, section))
}

/// [`note_keys`] of one section's real body.
fn keys_of(body: &[String]) -> Vec<String> {
    entries_of(body)
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
    entries_of(&real_body(text, section))
}

/// [`note_entries`] of one section's real body.
fn entries_of(body: &[String]) -> Vec<String> {
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
#[path = "changelog_release_body_bound_tests.rs"]
mod release_body_bound_tests;

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

#[cfg(test)]
#[path = "changelog_tests.rs"]
mod tests;
