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

/// The two changelogs a release claim writes (2026-09-23, owner ruling R2), from
/// the PUBLISHED commit's `source` and main's current `main`:
///
/// * the RELEASE commit's — `source` rolled ([`roll`]): its `[Unreleased]` becomes
///   `## [version] - date`, so the notes that ship are exactly the entries the
///   published commit carried. A `source` that already has the section (published
///   after an earlier roll) is taken as it stands.
/// * MAIN's — [`roll_shipped`]: the same section, carrying the same shipped body,
///   and main's `[Unreleased]` keeps every entry peers added after the publish,
///   because those ship in the NEXT release. A `main` that already has the section
///   (an earlier claim of this version whose cut wedged) is taken as it stands.
pub fn claim_changelogs(
    source: &str,
    main: &str,
    version: &str,
    date: &str,
) -> Result<(String, String)> {
    let source_rolled = has_section(source, version);
    let release = if source_rolled {
        source.to_string()
    } else {
        roll(source, version, date)?
    };
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

/// Roll `main` for a release whose notes are `source`'s `[Unreleased]` body — the
/// published commit's — rather than main's own.
///
/// The result is `main` with `## [version] - date` inserted below `[Unreleased]`,
/// carrying `source`'s `[Unreleased]` body verbatim, and main's `[Unreleased]`
/// reduced to the entries that body does not contain (compared as whole entries —
/// a bullet with its continuation lines — each shipped entry matching at most one),
/// each under its own `###` heading. When main's `[Unreleased]` IS `source`'s, the
/// result is byte-identical to [`roll`]`(main, …)`. An entry a peer EDITED after the
/// publish is a different entry: its new text stays unreleased, its old text ships.
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
/// after every heading and every entry. Empty when everything shipped.
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
    //
    // THE BOUND IS TAKEN ON THE WHOLE DOCUMENT, not on `changelog_body` alone:
    // the preamble is ~1.3 KB that the POST carries too, so bounding the body
    // and then prefixing the preamble yields a document OVER the limit by the
    // preamble's length — which is what the POST would then have to cut a
    // second time. One bound, over exactly the bytes that get posted.
    bound_release_body(&format!(
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
         {body}\n",
        body = changelog_body,
    ))
}

/// **GITHUB REFUSES A RELEASE BODY OVER THIS**, in characters, and says so only
/// as an opaque `422` on the create POST.
///
/// Measured 2026-09-17, when the v0.87.0 cut failed at the draft step with
/// `curl: (22) … error: 422` and nothing else to read. The tag was valid, the
/// target commit was on the remote, and the identical request with a SHORT body
/// succeeded — the only difference was the body, at 225,061 bytes. The rolled
/// section is pasted verbatim, and it has been growing: 30 KB at v0.84.0,
/// 52 KB at v0.85.0, 141 KB at v0.86.0, 225 KB here. v0.86.0's cut hit the same
/// refusal and it was recorded as "transient GitHub convergence"; it was not
/// transient, it was this, one release earlier.
///
/// COMPARED AGAINST [`github_length`], which counts UTF-16 code units — the
/// safe reading of "characters" for an API that speaks JSON. It was compared
/// against BYTES until the boundary panic below was found: bytes are merely
/// conservative (UTF-8 never spends fewer bytes than characters, so nothing
/// over-long could slip through) but they cut notes GitHub would have taken,
/// and a byte budget cannot serve as a slice index at all. `publish.rs` reads
/// this too: the bound is taken at the POST as well as at the write, and a 422
/// is diagnosed against it.
pub const GITHUB_RELEASE_BODY_LIMIT: usize = 125_000;

/// What GitHub is counting when it says "maximum is 125000 characters".
///
/// NOT bytes: the first cut of this bound measured `str::len()`, which is
/// merely conservative on a body of prose (every non-ASCII character costs
/// more than it scores) but is wrong in the direction that matters — it cut
/// notes GitHub would have taken. And a raw byte budget cannot be used as a
/// slice index at all: `&body[..124_767]` PANICS when that byte is in the
/// middle of an em dash, which this repo's changelog is full of.
///
/// UTF-16 code units are the safe reading of "characters" for an API that
/// speaks JSON: it equals the scalar count for everything in the BMP and
/// counts an astral scalar (an emoji) as the surrogate pair a UTF-16 counter
/// sees. Where the two readings differ this one is the larger, so a body that
/// passes here passes under either.
fn github_length(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The changelog body as GitHub will accept it: verbatim when it fits, and
/// otherwise cut at the last SECTION BOUNDARY that does, with a pointer to the
/// full text in the repository.
///
/// Cutting at a boundary rather than a character count is the point — a release
/// body that ends mid-sentence reads as corruption, and a reader cannot tell a
/// truncation from a bug. The tail names what was dropped and where to read it,
/// so the body is honest about being partial.
///
/// IDEMPOTENT, and that is load-bearing: `publish.rs` bounds again at the POST,
/// over a file this function already wrote. A body within the limit is returned
/// verbatim, so the second application changes nothing — otherwise every
/// resumed cut would saw another section off its own notes.
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
    // Walk to the byte index that spends the budget. `char_indices` only ever
    // yields a boundary, so every index below is a legal slice point — the
    // panic the byte budget could reach is unreachable by construction.
    let mut budget = body.len();
    let mut spent = 0usize;
    for (at, ch) in body.char_indices() {
        if spent + ch.len_utf16() > budget_units {
            budget = at;
            break;
        }
        spent += ch.len_utf16();
    }
    // The last section heading that still fits, so the body ends on a whole
    // entry. `rfind` returns the index OF the newline, itself a boundary.
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

    /// **A BODY THAT FITS IS VERBATIM**, to the byte — the bound must not touch
    /// the ordinary release.
    #[test]
    fn a_body_within_the_limit_is_returned_unchanged() {
        let body = "### Added\n\n- a thing\n\n### Fixed\n\n- another\n";
        assert_eq!(bound_release_body(body), body);
    }

    /// **AND ONE THAT DOES NOT FIT ENDS ON A WHOLE ENTRY**, under the limit,
    /// saying so.
    ///
    /// THE TWIN: return `body.to_string()` unconditionally and the length
    /// assertion goes red — which is exactly what shipped until 2026-09-17, and
    /// what made the v0.87.0 draft POST fail with an opaque `422` at 225,061
    /// bytes (and v0.86.0's before it, at 141 KB, misrecorded as transient).
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
        assert!(
            bounded.contains("longer than GitHub accepts"),
            "a truncated body must say it is truncated, or it reads as corruption"
        );
        // It ends on a section boundary, not mid-sentence.
        let kept = bounded
            .split("\n\n---\n\n*These notes")
            .next()
            .unwrap_or_default();
        assert!(
            kept.ends_with("\n\n") || kept.ends_with('\n'),
            "the kept text must end on a whole entry"
        );
    }

    /// **THE BUDGET INDEX LANDS INSIDE A CHARACTER AND NOTHING PANICS.**
    ///
    /// The first cut of this bound sliced `&body[..budget]` at a raw byte
    /// count. `&str` indexing panics off a char boundary, so a release whose
    /// notes happened to carry a multi-byte character across that one index
    /// would have aborted the cut — in the POST path, after the claim.
    ///
    /// THE TWIN: restore the byte slice and this goes from green to a panic,
    /// which is why the fixture pads with a single ASCII run and then lays em
    /// dashes across the whole budget neighbourhood rather than trusting one
    /// guessed offset.
    #[test]
    fn a_cut_that_falls_inside_a_character_does_not_panic() {
        for pad in 0..8usize {
            // Em dashes are 3 bytes and 1 UTF-16 unit, so the byte index the
            // old code computed drifts off a boundary for most `pad` values.
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

    /// **THE LIMIT IS COUNTED THE WAY GITHUB COUNTS IT.** A body of em dashes
    /// is three times its character count in bytes; bounding on bytes would
    /// throw away roughly two thirds of notes GitHub would have accepted.
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

    /// **BOUNDING TWICE CHANGES NOTHING.** The POST bounds what it reads from
    /// disk, and that file was already bounded when this binary wrote it — so
    /// the second application has to be a no-op or every resumed cut would
    /// saw another section off its own notes.
    #[test]
    fn bounding_an_already_bounded_body_is_a_no_op() {
        let body = "### Section\n\n- entry\n\n".repeat(20_000);
        let once = bound_release_body(&body);
        let twice = bound_release_body(&once);
        assert_eq!(once, twice, "the bound must be idempotent");
    }

    /// The WHOLE document — preamble plus body — also lands under the limit,
    /// because the preamble is what the POST carries too.
    ///
    /// EXACTLY the limit, with no slack: bounding `changelog_body` alone left
    /// the assembled document over by the preamble's ~1.3 KB, and this
    /// assertion used to have to allow `+ 2_000` to pass. That slack WAS the
    /// defect — GitHub counts the document, not the section inside it.
    #[test]
    fn the_assembled_document_fits_what_github_accepts() {
        let body = "### Section\n\n- entry\n\n".repeat(6000);
        let doc = release_notes_document("0.87.0", &body);
        assert!(
            doc.len() <= GITHUB_RELEASE_BODY_LIMIT,
            "assembled release notes are {} bytes, over the {GITHUB_RELEASE_BODY_LIMIT} \
             GitHub takes",
            doc.len()
        );
        assert!(
            doc.starts_with("**aterm** is the terminal for AI."),
            "the preamble must survive the bound"
        );
    }

    /// **BOUNDING AN ALREADY-BOUNDED BODY IS A NO-OP**, byte for byte.
    ///
    /// This is what lets the guard sit at the POST as well as at the write
    /// without the two fighting: the POST re-bounds whatever it reads off disk
    /// — a file written by an older binary, a resumed cut, a hand-edited one —
    /// and a file this binary wrote passes through untouched.
    #[test]
    fn bounding_an_already_bounded_body_changes_nothing() {
        let overflowing = "### Section\n\n- an entry long enough to matter\n\n".repeat(5000);
        assert!(overflowing.len() > GITHUB_RELEASE_BODY_LIMIT);
        let once = bound_release_body(&overflowing);
        let twice = bound_release_body(&once);
        assert_eq!(once, twice, "the bound is not idempotent");
        // And the same for the assembled document, which is what dist/notes-*.md holds.
        let doc = release_notes_document("0.87.0", &overflowing);
        assert_eq!(doc, bound_release_body(&doc));
    }
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

    const SOURCE: &str = "# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n- **One** — shipped.\n  A continuation line.\n\n- **Two** — shipped.\n\n## [0.91.0] - 2026-09-22\n\n- older.\n";

    /// Main that has not moved: the roll is exactly [`roll`], byte for byte.
    #[test]
    fn roll_shipped_of_an_unmoved_main_is_the_plain_roll() {
        assert_eq!(
            roll_shipped(SOURCE, SOURCE, "0.92.0", "2026-09-23").unwrap(),
            roll(SOURCE, "0.92.0", "2026-09-23").unwrap()
        );
    }

    /// Main moved after the publish: what the published commit carried ships,
    /// what peers added stays unreleased under its own heading — an entry whose
    /// text a peer EDITED is a new entry, so its new text stays and its old ships.
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
        // NEGATIVE CONTROL: the plain roll of main would ship the peers' entries.
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
        // Main already rolled by an earlier claim of this version (a recut).
        let (release, landing) = claim_changelogs(SOURCE, &rolled, "0.92.0", "2026-09-24").unwrap();
        assert_eq!(release, roll(SOURCE, "0.92.0", "2026-09-24").unwrap());
        assert_eq!(landing, rolled, "main is never rolled twice");
        // A published commit that was itself rolled ships its section as it stands.
        let (release, landing) =
            claim_changelogs(&rolled, &rolled, "0.92.0", "2026-09-24").unwrap();
        assert_eq!(
            (release.as_str(), landing.as_str()),
            (rolled.as_str(), rolled.as_str())
        );
        // …but main having dropped that section is refused, not papered over.
        let error = claim_changelogs(&rolled, SOURCE, "0.92.0", "2026-09-24")
            .unwrap_err()
            .to_string();
        assert!(error.contains("dropped a released section"), "{error}");
    }

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
