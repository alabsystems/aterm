// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Changelog gate/roll/extract (release spec §3): the one Rust port of `changelog_real_body` semantics (strip HTML comments, `###` scaffolds, blanks) over the hand-written Keep-a-Changelog `CHANGELOG.md`.

use crate::ledger::{Error, Result};

/// The changelog's repo-root-relative path: the one hand-written source of release notes.
pub const CHANGELOG_FILE: &str = "CHANGELOG.md";

/// What the pre-claim gate learned about a section: `entries`, the count of top-level bullets in the real body, for the transcript only.
#[derive(Debug)]
pub struct GateSummary {
    pub entries: usize,
}

/// The real body of a section: the lines between `## [<name>]` and the next `## ` heading, with HTML comments (multi-line included), `###` scaffold headings and blank lines removed.
pub fn real_body(text: &str, section: &str) -> Vec<String> {
    let header = format!("## [{section}]");
    let mut in_section = false;
    let mut in_comment = false;
    let mut out = Vec::new();
    for raw in text.lines() {
        if !in_section {
            in_section = raw.starts_with(&header);
            continue;
        }
        if raw.starts_with("## ") {
            break;
        }
        let mut line = raw.to_string();
        if in_comment {
            match line.find("-->") {
                Some(p) => {
                    line = line[p + 3..].to_string();
                    in_comment = false;
                }
                None => continue,
            }
        }
        while let Some(start) = line.find("<!--") {
            match line.rfind("-->").filter(|&end| end >= start + 4) {
                Some(end) => line = format!("{}{}", &line[..start], &line[end + 3..]),
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
    let header = format!("## [{version}]");
    text.lines().any(|l| l.starts_with(&header))
}

/// Pre-claim gates, in transcript order: `[Unreleased]` exists, its real body is non-empty, and the raw section carries no `'''` or raw control character.
pub fn gate_unreleased(text: &str) -> Result<GateSummary> {
    gate_section(text, "Unreleased")
}

/// The same gate over any section: a recut re-cuts a version whose body an earlier wedged cut already rolled into `## [X.Y.Z]`, so it judges that section (the fresh `[Unreleased]` above it is legitimately empty).
pub fn gate_section(text: &str, section: &str) -> Result<GateSummary> {
    if !text
        .lines()
        .any(|l| l.starts_with(&format!("## [{section}]")))
    {
        return Err(Error::new(format!(
            "{CHANGELOG_FILE} has no \"## [{section}]\" section"
        )));
    }
    let body = real_body(text, section);
    if body.is_empty() {
        return Err(Error::new(format!(
            "the [{section}] section of {CHANGELOG_FILE} has no release notes — add a \
             hand-written \"### Added/Changed/Fixed\" entry before cutting (comments, \
             blank lines and bare ### scaffolds do not count)"
        )));
    }
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
    // Double-roll guard: a second `## [X.Y.Z]` would split the notes across two headings,
    // and recut detection keys on this section's presence.
    if has_section(text, version) {
        return Err(Error::new(format!(
            "{CHANGELOG_FILE} already has a \"## [{version}]\" section — already \
             rolled; a recut reuses it instead of rolling again"
        )));
    }
    if !text.lines().any(|l| l.starts_with("## [Unreleased]")) {
        return Err(Error::new(format!(
            "{CHANGELOG_FILE} has no \"## [Unreleased]\" section"
        )));
    }
    let mut out = Vec::new();
    let mut done = false;
    for line in text.lines() {
        if !done && line.starts_with("## [Unreleased]") {
            out.push(line.to_string());
            out.push(String::new());
            out.push(format!("## [{version}] - {date}"));
            done = true;
            continue;
        }
        out.push(line.to_string());
    }
    let mut rolled = out.join("\n");
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
    let header = format!("## [{version}] - ");
    text.lines()
        .find_map(|l| l.strip_prefix(header.as_str()))
        .map(str::trim)
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
        .position(|l| l.starts_with("## [Unreleased]"))
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
    out.extend(leftovers.iter().map(String::as_str));
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
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.starts_with("## [Unreleased]"))
        .ok_or_else(|| {
            Error::new(format!(
                "{CHANGELOG_FILE} has no \"## [Unreleased]\" section"
            ))
        })?;
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.starts_with("## "))
        .map_or(lines.len(), |p| start + 1 + p);
    Ok(lines[start + 1..end].to_vec())
}

/// One unit of a changelog section body: a `###` heading, or an entry — a line
/// that starts a bullet (or a paragraph after a blank or a heading) plus every
/// following non-blank line that does not start another.
enum Unit<'a> {
    Heading(&'a str),
    Entry(Vec<&'a str>),
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
fn unshipped_entries(main_body: &[&str], shipped: &[&str]) -> Vec<String> {
    let mut remaining: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for unit in units(shipped) {
        if let Unit::Entry(lines) = unit {
            *remaining.entry(lines.join("\n")).or_default() += 1;
        }
    }
    let mut out = Vec::new();
    let mut heading: Option<&str> = None;
    let mut heading_written = false;
    for unit in units(main_body) {
        match unit {
            Unit::Heading(line) => {
                heading = Some(line);
                heading_written = false;
            }
            Unit::Entry(lines) => {
                let key = lines.join("\n");
                if let Some(count) = remaining.get_mut(&key)
                    && *count > 0
                {
                    *count -= 1;
                    continue;
                }
                if let Some(line) = heading
                    && !heading_written
                {
                    out.push(line.to_string());
                    out.push(String::new());
                    heading_written = true;
                }
                out.extend(lines.iter().map(|l| (*l).to_string()));
                out.push(String::new());
            }
        }
    }
    out
}

/// Extract a rolled section's body verbatim, for the manifest, the release body and the in-app notes: the raw lines between `## <version>` and the next `## ` heading, trimmed of leading and trailing blank lines only.
pub fn rolled_body(text: &str, version: &str) -> Result<String> {
    let header = format!("## [{version}]");
    let mut grab = false;
    let mut lines: Vec<&str> = Vec::new();
    for line in text.lines() {
        if !grab {
            grab = line.starts_with(&header);
            continue;
        }
        if line.starts_with("## [") {
            break;
        }
        lines.push(line);
    }
    if !grab {
        return Err(Error::new(format!(
            "{CHANGELOG_FILE} has no \"## [{version}]\" section to extract"
        )));
    }
    while lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return Err(Error::new(format!(
            "the \"## [{version}]\" section of {CHANGELOG_FILE} is empty — the roll \
             should have made that impossible; refusing to ship note-less"
        )));
    }
    Ok(lines.join("\n"))
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
/// the next `## [` heading (or one past the last line).
fn section_span(text: &str, section: &str) -> Option<(usize, usize)> {
    let header = format!("## [{section}]");
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.iter().position(|l| l.starts_with(&header))?;
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.starts_with("## ["))
        .map_or(lines.len(), |p| start + 1 + p);
    Some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
