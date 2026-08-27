// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Changelog gate/roll/extract (release spec §3): the one Rust port of
//! `changelog_real_body` semantics (strip HTML comments, `###` scaffolds,
//! blanks) over the hand-written Keep-a-Changelog `CHANGELOG.md`. Gates run
//! before the claim (`[Unreleased]` body non-empty; `'''` anywhere in the body
//! is a hard abort with the offending line number — never a silent collapse);
//! the roll (`[Unreleased]` → `[0.2.0] - <local date>` + fresh scaffold) lands
//! inside the claim commit; the extracted section body is used verbatim, once,
//! for manifest + GitHub notes + in-app notes.
//!
//! Ported from the retired shell pipeline: `changelog_real_body` and the roll
//! awk from `tools/prepare-release.sh`, the section/trim extraction from
//! `tools/extract-changelog.sh`. The integration tests keep the original awk
//! programs as an oracle and prove line-for-line parity, real CHANGELOG.md
//! included.

use std::process::Command;

use crate::ledger::{Error, Result};

/// The changelog's repo-root-relative path — the single hand-written source
/// of release notes (there is no generator).
pub const CHANGELOG_FILE: &str = "CHANGELOG.md";

/// What the pre-claim gate learned about `[Unreleased]` — `entries` (the
/// count of top-level bullets in the real body) feeds the cut transcript's
/// "N entries, no '''" line; it is informational, never load-bearing.
#[derive(Debug)]
pub struct GateSummary {
    pub entries: usize,
}

/// The REAL body of a section — the lines between `## [<name>]` and the next
/// `## ` heading with HTML comments (multi-line included), `###` scaffold
/// headings and blank/whitespace lines removed. What survives is actual
/// release-note prose/bullets; empty output means "no changelog", so a guard
/// built on it fails a truly-empty section, a whitespace-only one, a
/// comment-only one, AND a bare `### ` scaffold with no bullets.
///
/// Line-for-line port of `changelog_real_body` (tools/prepare-release.sh) —
/// including the awk's LEFTMOST-LONGEST `<!--.*-->` match, which strips from
/// the FIRST `<!--` to the LAST `-->` of a line (so `a <!-- x --> b <!-- y
/// --> c` yields `a  c`, not `a  b  c`). The tests hold this function to the
/// original awk as an oracle; do not "fix" that greediness.
pub fn real_body(text: &str, section: &str) -> Vec<String> {
    let header = format!("## [{section}]");
    let mut in_section = false;
    let mut in_comment = false;
    let mut out = Vec::new();
    for raw in text.lines() {
        if !in_section {
            // Comments cannot carry across the section boundary: the awk
            // skips pre-section lines before its comment tracking runs.
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
        // Greedy same-line comment removal (see the doc comment): first
        // `<!--` through the last `-->` that still closes it. At most one
        // effective round, but keep the awk's `while` shape for fidelity.
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

/// Does the changelog already carry a `## [<version>]` section? Used by the
/// roll's double-roll guard here and by the claim's "cut elsewhere" abort
/// (ledger.rs) — one definition so the two can never disagree.
pub fn has_section(text: &str, version: &str) -> bool {
    let header = format!("## [{version}]");
    text.lines().any(|l| l.starts_with(&header))
}

/// Pre-claim gates (spec §3), in transcript order: `[Unreleased]` exists, its
/// real body is non-empty, and the RAW section carries no `'''`. Runs before
/// the claim so a note-less or manifest-poisoning changelog costs seconds,
/// not a burned ledger number.
pub fn gate_unreleased(text: &str) -> Result<GateSummary> {
    gate_section(text, "Unreleased")
}

/// The same gate over an arbitrary section — the RECUT path (spec §5) re-cuts
/// a version whose body was already rolled into `## [X.Y.Z]` by the earlier
/// wedged cut, so the gate must judge THAT section (the fresh `[Unreleased]`
/// scaffold above it is legitimately empty then).
pub fn gate_section(text: &str, section: &str) -> Result<GateSummary> {
    if !text
        .lines()
        .any(|l| l.starts_with(&format!("## [{section}]")))
    {
        return Err(Error::new(format!(
            "{CHANGELOG_FILE} has no \"## [{section}]\" section"
        )));
    }
    // HARD REQUIREMENT (carried from prepare-release.sh): every release ships
    // a hand-written changelog — the Software Update window and the manifest
    // surface these notes, and there is no generator to lean on.
    let body = real_body(text, section);
    if body.is_empty() {
        return Err(Error::new(format!(
            "the [{section}] section of {CHANGELOG_FILE} has no release notes — add a \
             hand-written \"### Added/Changed/Fixed\" entry before cutting (comments, \
             blank lines and bare ### scaffolds do not count)"
        )));
    }
    // `'''` scan over the RAW section — raw is what ships verbatim inside the
    // manifest's `changelog = '''…'''` TOML literal, so a `'''` anywhere in it
    // (comments included) would terminate the literal early and every fleet
    // client's Manifest::parse would reject the release. Hard abort with the
    // file line number — never gen-appcast.sh's silent quote collapse (spec
    // decision 12).
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
            // Raw control characters (a stray \r, ESC, DEL, …) are refused by
            // Manifest::to_toml at emission time — but emission runs in
            // step_build, AFTER the claim commit is pushed and the whole
            // build completed, so that refusal alone burns a ledger number.
            // Mirror the emitter's exact rule here (spec decision 12
            // sequences representability BEFORE the claim): \t is TOML-legal,
            // and \n never appears inside a lines() item.
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
    // Informational entry count: top-level bullets of the real body.
    let entries = body
        .iter()
        .filter(|l| l.starts_with("- ") || l.starts_with("* "))
        .count();
    Ok(GateSummary { entries })
}

/// Roll `## [Unreleased]` → `## [<version>] - <date>` (spec §3): the new
/// heading is inserted right below the Unreleased header with one blank line
/// between — the whole current body changes ownership to the release and a
/// fresh EMPTY `[Unreleased]` scaffold is what remains on top. Exact
/// behavioral port of prepare-release.sh's roll awk (the tests hold it to
/// that oracle), so historical rolls and this one are byte-compatible.
///
/// `date` is `YYYY-MM-DD` — pass [`today_la`] for a real cut; injected so
/// tests are deterministic.
pub fn roll(text: &str, version: &str, date: &str) -> Result<String> {
    // Double-roll guard: a second `## [X.Y.Z]` section would split the release
    // notes across two headings and desync the recut detection (spec §5
    // derives "recut" from this section's presence).
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
    // `lines()` drops the final newline; restore it iff the input had one so
    // the roll is a pure insertion (byte-identical everywhere else).
    if text.ends_with('\n') {
        rolled.push('\n');
    }
    Ok(rolled)
}

/// Extract a rolled section's body VERBATIM (spec §3: used once, verbatim,
/// for manifest + `gh release create --notes-file` + the in-app notes): the
/// raw lines between `## [<version>]` and the next `## [` heading, trimmed of
/// leading/trailing blank lines only. Behavioral port of
/// tools/extract-changelog.sh — minus its fall-back-to-Unreleased lane, which
/// existed to mask a missing section; here a missing or empty section is a
/// hard error because the gate + roll already guaranteed real notes.
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

/// The GitHub release BODY: a standing newcomer preamble, then this release's
/// changelog verbatim.
///
/// The changelog alone addresses people who already run aterm; the /releases
/// page is also the front door for people who have never heard of it and land
/// on a wall of unexplained assets (a DMG next to a zip next to TOML nobody
/// should open). The preamble is part of the TEMPLATE, not prose written per
/// cut, so every future release carries it without anyone remembering to.
///
/// Only the release body gets the preamble. The manifest's `changelog` and the
/// in-app Software Update notes stay the rolled section verbatim (spec §3) —
/// an installed copy already knows what aterm is.
///
/// ONE macOS download (RETIRED 2026-08-26: the batteries-included seed, the
/// Intel `-x86_64` DMG, the `-lite` twin and the `aterm-offline.dmg` alias):
/// the guide names exactly the lean DMG, the zip, the evergreen aliases and
/// the sidecars — nothing this release does not carry.
pub fn release_notes_document(version: &str, changelog_body: &str) -> String {
    // Sizes are ballpark labels for a reader scanning the asset list, not
    // records (the `.sha256` sidecars are the records).
    format!(
        "**aterm** is the terminal for AI. New here? What each file is:\n\
         \n\
         - `aterm-{version}.dmg` — the signed, notarized app as a drag-install DMG \
         (~28 MB). The ALab toolchain installs itself on first launch (or on demand via \
         `aterm pkg install --default-set`).\n\
         - `aterm-{version}-mac.zip` — the same app as a zip (~26 MB); this is the \
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
         {changelog_body}\n"
    )
}

/// Today's date in America/Los_Angeles as `YYYY-MM-DD` — exact parity with
/// prepare-release.sh (`TZ=America/Los_Angeles date +%Y-%m-%d`). Shelling out
/// to `date(1)` is deliberate: the alternative is hand-rolling US DST rules
/// in-process, a whole failure class for one cosmetic heading date.
pub fn today_la() -> Result<String> {
    let out = Command::new("/bin/date")
        .env("TZ", "America/Los_Angeles")
        .arg("+%Y-%m-%d")
        .output()
        .map_err(|e| Error::new(format!("failed to spawn /bin/date: {e}")))?;
    if !out.status.success() {
        return Err(Error::new(format!(
            "/bin/date failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let date = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // Shape-check the tool output before it lands in a permanent heading.
    let shaped = date.len() == 10
        && date.bytes().enumerate().all(|(i, b)| match i {
            4 | 7 => b == b'-',
            _ => b.is_ascii_digit(),
        });
    if !shaped {
        return Err(Error::new(format!(
            "/bin/date produced {date:?}, not YYYY-MM-DD"
        )));
    }
    Ok(date)
}

/// 0-based (start, end) line indices of a section: `start` is the `## [name]`
/// header line, `end` the next `## [` heading (or one past the last line).
/// The `'''` gate uses it to attribute file line numbers; the span matches
/// [`rolled_body`]'s extraction span — the scan must cover exactly the bytes
/// that would ship.
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

    /// The release body opens for NEWCOMERS and still carries the changelog
    /// VERBATIM below the rule — the "used once, verbatim" contract (spec §3)
    /// is about the notes themselves, and the preamble must never edit them.
    #[test]
    fn the_release_body_is_preamble_then_the_changelog_verbatim() {
        let body = "### Fixed\n- a thing\n- another";
        let doc = release_notes_document("0.44.0", body);
        // The preamble names THIS release's exact asset names, so a reader can
        // match the guide against the asset list one screen below it.
        assert!(doc.starts_with("**aterm** is the terminal for AI."));
        assert!(doc.contains("`aterm-0.44.0.dmg`"), "{doc}");
        assert!(doc.contains("`aterm-0.44.0-mac.zip`"), "{doc}");
        assert!(doc.contains("`aterm.dmg` / `aterm-mac.zip`"), "{doc}");
        assert!(doc.contains("shasum -a 256 -c"), "{doc}");
        assert!(doc.contains("atpkg-index-N"), "{doc}");
        // ONE macOS download: the guide must never again advertise a
        // container the release does not carry (RETIRED 2026-08-26).
        for retired in [
            "x86_64",
            "lite.dmg",
            "aterm-offline",
            "batteries",
            "offline",
        ] {
            assert!(!doc.contains(retired), "{retired:?} resurfaced: {doc}");
        }
        // Changelog below the rule, byte-for-byte, newline-terminated.
        assert!(doc.ends_with(&format!("\n---\n\n{body}\n")), "{doc}");
    }
}
