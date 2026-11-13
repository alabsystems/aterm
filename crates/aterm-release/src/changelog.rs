// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Changelog gate/roll/extract (release spec §3): the one Rust port of `changelog_real_body` semantics (strip HTML comments, `###` scaffolds, blanks) over the Keep-a-Changelog `CHANGELOG.md`, whose `[Unreleased]` a release writes from the entry files under `changes/` ([`Window`]).

use crate::ledger::{Error, GitRunner, Result, git_read_ok};

use aterm_census::changelog::is_section_header;
pub use aterm_census::changelog::{
    CHANGELOG_FILE, GateSummary, HARNESS_ENTRY_HEAD, has_section, real_body,
};
pub use aterm_verify::changelog_fragments::{FRAGMENT_DIR, Fragment};
use aterm_verify::changelog_fragments::{
    MAX_FRAGMENT_BYTES, MAX_FRAGMENTS, MAX_TOTAL_BYTES, Section, assemble, entries, entry_key,
    unreleased_entries,
};

#[path = "changelog_published.rs"]
mod published;
pub(crate) use published::gate as gate_published;

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

/// The date of `version`'s section, `## [version] - <date>`: what the release commit's roll was
/// dated, read back so a claim on a later day lands the roll it was made with.
#[must_use]
pub fn section_date<'a>(text: &'a str, version: &str) -> Option<&'a str> {
    text.lines().find_map(|l| {
        l.strip_prefix("## [")?
            .strip_prefix(version)?
            .strip_prefix("] - ")
            .map(str::trim)
    })
}

/// One commit's release-note sources: `CHANGELOG.md`'s text and each entry file under
/// `changes/` with its blob id, read from objects alone.
#[derive(Debug, Clone)]
pub struct Window {
    /// `CHANGELOG.md` as the commit holds it.
    pub raw: String,
    /// Every file under `changes/`, by path, with its blob id.
    pub fragments: Vec<(Fragment, String)>,
}

impl Window {
    /// The release-note sources `commit` holds for releasing `version`. A recut (its changelog
    /// already carries `version`'s section) ships that section as it stands and deletes no file,
    /// so it reads none: the files under `changes/` belong to the next window, whose own step 1
    /// judges them, and a stray one never blocks the recovery release. Otherwise strict:
    /// `changes/` (spelled exactly so; a root entry another case of it is refused) holds only
    /// regular, non-executable files within the bounds, each UTF-8.
    ///
    /// # Errors
    /// Any of those refusals, or git cannot read the objects.
    pub fn at(git: &dyn GitRunner, commit: &str, version: &str) -> Result<Window> {
        let raw = blob_text(git, &format!("{commit}:{CHANGELOG_FILE}"))?;
        if has_section(&raw, version) {
            return Ok(Window {
                raw,
                fragments: Vec::new(),
            });
        }
        let Some(tree) = fragment_tree(git, commit, true)? else {
            return Ok(Window {
                raw,
                fragments: Vec::new(),
            });
        };
        let listed = git_read_ok(
            git,
            &[
                "--no-lazy-fetch",
                "--no-replace-objects",
                "ls-tree",
                "-r",
                "-l",
                "-z",
                &tree,
            ],
        )?
        .stdout;
        let mut fragments = Vec::new();
        let mut total = 0usize;
        for record in listed.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            let (meta, rel) = split_record(record)?;
            let path = format!("{FRAGMENT_DIR}/{rel}");
            let fields: Vec<&str> = meta.split_whitespace().collect();
            let [mode, kind, oid, size] = fields.as_slice() else {
                return Err(Error::new(format!(
                    "{path}: unreadable tree entry {meta:?}"
                )));
            };
            if *mode != "100644" || *kind != "blob" {
                return Err(Error::new(format!(
                    "{path}: is not a regular, non-executable file (git mode {mode} {kind}) — a \
                     changelog entry is a plain file the release ships as written"
                )));
            }
            let size: usize = size.parse().unwrap_or(usize::MAX);
            total = total.saturating_add(size);
            if size > MAX_FRAGMENT_BYTES {
                return Err(Error::new(format!(
                    "{path}: is {size} bytes, over the {MAX_FRAGMENT_BYTES} one entry may hold"
                )));
            }
            if fragments.len() >= MAX_FRAGMENTS || total > MAX_TOTAL_BYTES {
                return Err(Error::new(format!(
                    "{FRAGMENT_DIR}/ holds more than {MAX_FRAGMENTS} entries or {MAX_TOTAL_BYTES} \
                     bytes, more than one release may ship"
                )));
            }
            let text = blob_text(git, oid)
                .map_err(|_| Error::new(format!("{path}: is not readable UTF-8 text")))?;
            fragments.push((Fragment { path, text }, (*oid).to_string()));
        }
        Ok(Window { raw, fragments })
    }

    /// `CHANGELOG.md` with `[Unreleased]` written from this window's raw entries and files
    /// ([`assemble`]), `announcement` (the derived harness-versions line) leading `### Changed`.
    ///
    /// # Errors
    /// Every finding [`assemble`] has, one per line.
    pub fn assembled(&self, announcement: Option<&str>) -> Result<String> {
        let fragments: Vec<Fragment> = self.fragments.iter().map(|(f, _)| f.clone()).collect();
        assemble(&self.raw, &fragments, announcement).map_err(|found| Error::new(found.join("\n")))
    }

    /// Whether this window's changelog already carries `version`'s section: a recut, whose
    /// release commit ships that section as it stands and deletes no file (and whose window
    /// [`Window::at`] read none).
    #[must_use]
    pub fn is_recut(&self, version: &str) -> bool {
        has_section(&self.raw, version)
    }
}

/// A blob's text by object name (`<rev>:<path>` or an id), strict UTF-8.
fn blob_text(git: &dyn GitRunner, spec: &str) -> Result<String> {
    let out = git_read_ok(
        git,
        &[
            "--no-lazy-fetch",
            "--no-replace-objects",
            "cat-file",
            "blob",
            spec,
        ],
    )?;
    String::from_utf8(out.stdout).map_err(|_| Error::new(format!("{spec} is not valid UTF-8")))
}

/// One `ls-tree -z` record's metadata and path.
fn split_record(record: &[u8]) -> Result<(&str, &str)> {
    let text = std::str::from_utf8(record)
        .map_err(|_| Error::new(format!("a {FRAGMENT_DIR}/ path is not UTF-8")))?;
    text.split_once('\t')
        .ok_or_else(|| Error::new(format!("unreadable tree entry {text:?}")))
}

/// The id of `commit`'s `changes/` tree, `None` where it holds none. `strict` refuses a root
/// entry whose name is another case of `changes` and a `changes` that is not a directory; the
/// landing's read is not strict, since it never judges main's tip.
fn fragment_tree(git: &dyn GitRunner, commit: &str, strict: bool) -> Result<Option<String>> {
    let listed = git_read_ok(
        git,
        &[
            "--no-lazy-fetch",
            "--no-replace-objects",
            "ls-tree",
            "-z",
            &format!("{commit}^{{tree}}"),
        ],
    )?
    .stdout;
    let mut found = None;
    for record in listed.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let Ok((meta, name)) = split_record(record) else {
            continue;
        };
        if !name.eq_ignore_ascii_case(FRAGMENT_DIR) {
            continue;
        }
        let fields: Vec<&str> = meta.split_whitespace().collect();
        match fields.as_slice() {
            [_, "tree", oid] if name == FRAGMENT_DIR => found = Some((*oid).to_string()),
            _ if strict => {
                return Err(Error::new(format!(
                    "{name}: changelog entries live in a directory spelled exactly \
                     {FRAGMENT_DIR}/, which no release would otherwise read"
                )));
            }
            _ => {}
        }
    }
    Ok(found)
}

/// Every blob under `commit`'s `changes/`, by path, with its id: a plain listing with no rules,
/// for the landing on main's tip.
///
/// # Errors
/// git cannot list the tree.
pub fn fragment_listing(git: &dyn GitRunner, commit: &str) -> Result<Vec<(String, String)>> {
    let Some(tree) = fragment_tree(git, commit, false)? else {
        return Ok(Vec::new());
    };
    let listed = git_read_ok(
        git,
        &[
            "--no-lazy-fetch",
            "--no-replace-objects",
            "ls-tree",
            "-r",
            "-z",
            &tree,
        ],
    )?
    .stdout;
    let mut out = Vec::new();
    for record in listed.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let Ok((meta, rel)) = split_record(record) else {
            continue;
        };
        let fields: Vec<&str> = meta.split_whitespace().collect();
        if let [_, "blob", oid] = fields.as_slice() {
            out.push((format!("{FRAGMENT_DIR}/{rel}"), (*oid).to_string()));
        }
    }
    Ok(out)
}

/// The release commit's changelog: `source`'s window assembled with `announcement` and rolled
/// for `version` on `date`, or `source`'s changelog as it stands when it already carries the
/// section (a recut).
///
/// # Errors
/// [`Window::assembled`]'s and [`roll`]'s.
pub fn release_text(
    source: &Window,
    announcement: Option<&str>,
    version: &str,
    date: &str,
) -> Result<String> {
    if source.is_recut(version) {
        return Ok(source.raw.clone());
    }
    roll(&source.assembled(announcement)?, version, date)
}

/// What a roll ships, in words: `entries` entries in its section and `removed` entry files the
/// release commit deletes.
#[must_use]
pub fn roll_words(entries: usize, removed: usize) -> String {
    let plural =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    format!(
        "its roll ships {} and deletes {}",
        plural(entries, "entry", "entries"),
        plural(removed, "entry file", "entry files")
    )
}

/// The files the release commit deletes: every one `source` holds, or none for a recut, whose
/// files belong to the next window.
#[must_use]
pub fn release_removals(source: &Window, version: &str) -> Vec<(String, String)> {
    if source.is_recut(version) {
        return Vec::new();
    }
    source
        .fragments
        .iter()
        .map(|(f, oid)| (f.path.clone(), oid.clone()))
        .collect()
}

/// Main's changelog once the claim lands on its tip `main`, from the published commit's
/// changelog `source` and the release commit's `release`. Main's text is never judged here:
/// its lines up to and including `## [Unreleased]`, then its `[Unreleased]` body less the raw
/// entries `source` carried (each consuming one match), then `release`'s `## [version]` section
/// exactly as the release commit holds it, then the rest of main. So the section main records
/// is byte for byte the one that shipped, and anything else on main stays for the dev check and
/// the next cut's step 1. Main as it stands where it already carries the section (an earlier
/// wedged claim); a main without `[Unreleased]` takes the section above its first `## `.
///
/// # Errors
/// `release` has no `## [version]` section, or `source` carries it while main does not (main
/// dropped a released section).
pub fn landing_text(main: &str, source: &str, release: &str, version: &str) -> Result<String> {
    if has_section(main, version) {
        return Ok(main.to_string());
    }
    if has_section(source, version) {
        return Err(Error::new(format!(
            "the published commit's {CHANGELOG_FILE} already carries \"## [{version}]\" and \
             main's does not — main has dropped a released section; restore it before cutting"
        )));
    }
    let section = section_lines(release, version).ok_or_else(|| {
        Error::new(format!(
            "the release commit's {CHANGELOG_FILE} has no \"## [{version}]\" section"
        ))
    })?;
    let lines: Vec<&str> = main.lines().collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len() + section.len() + 2);
    let leftovers;
    match lines
        .iter()
        .position(|l| is_section_header(l, "Unreleased"))
    {
        Some(start) => {
            let end = lines[start + 1..]
                .iter()
                .position(|l| l.starts_with("## "))
                .map_or(lines.len(), |p| start + 1 + p);
            // Serves the drain of entries written before fragments: what the source carried
            // raw shipped, so main's copy of it goes; it goes once [Unreleased] holds none.
            let shipped = unreleased_lines(source);
            leftovers = unshipped_entries(&lines[start + 1..end], &shipped);
            out.extend_from_slice(&lines[..=start]);
            out.push("");
            out.extend_from_slice(&leftovers);
            out.extend_from_slice(&section);
            out.extend_from_slice(&lines[end..]);
        }
        None => {
            let first = lines
                .iter()
                .position(|l| l.starts_with("## "))
                .unwrap_or(lines.len());
            out.extend_from_slice(&lines[..first]);
            out.extend_from_slice(&section);
            out.extend_from_slice(&lines[first..]);
        }
    }
    let mut text = out.join("\n");
    if main.ends_with('\n') || main.is_empty() {
        text.push('\n');
    }
    Ok(text)
}

/// `text`'s `## [version]` heading and every line after it up to the next `## ` heading.
fn section_lines<'a>(text: &'a str, version: &str) -> Option<Vec<&'a str>> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.iter().position(|l| is_section_header(l, version))?;
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.starts_with("## "))
        .map_or(lines.len(), |p| start + 1 + p);
    Some(lines[start..end].to_vec())
}

/// The raw lines of `text`'s `[Unreleased]` body, empty where it has none.
fn unreleased_lines(text: &str) -> Vec<&str> {
    let mut lines = text.lines();
    if !lines.any(|l| is_section_header(l, "Unreleased")) {
        return Vec::new();
    }
    lines.take_while(|l| !l.starts_with("## ")).collect()
}

/// The files the claim deletes from main's tip, given the files the release shipped
/// (`shipped`, path and blob id) and main's `listing`: each shipped blob removes at most one of
/// main's files with the same id, the same path first, so a shipped file renamed or moved to
/// another kind since the publish still goes, and one edited since stays and ships next. Where
/// main already carries the section (an earlier wedged claim), only files whose entry that
/// section holds go.
#[must_use]
pub fn landing_removals(
    shipped: &[(Fragment, String)],
    main: &str,
    listing: &[(String, String)],
    version: &str,
) -> Vec<(String, String)> {
    let recorded: Option<Vec<String>> = has_section(main, version).then(|| {
        entries(main)
            .into_iter()
            .filter(|e| e.section == Section::Released(version))
            .map(|e| e.text)
            .collect()
    });
    let mut taken = vec![false; listing.len()];
    let mut out = Vec::new();
    let mut pending = Vec::new();
    for (fragment, oid) in shipped {
        if let Some(held) = &recorded {
            let entry = fragment.text.strip_suffix('\n').unwrap_or(&fragment.text);
            if !held.iter().any(|text| text == entry) {
                continue;
            }
        }
        match listing
            .iter()
            .enumerate()
            .position(|(i, (path, id))| !taken[i] && *path == fragment.path && id == oid)
        {
            Some(i) => {
                taken[i] = true;
                out.push(listing[i].clone());
            }
            None => pending.push(oid),
        }
    }
    for oid in pending {
        if let Some(i) = listing
            .iter()
            .enumerate()
            .position(|(i, (_, id))| !taken[i] && id == oid)
        {
            taken[i] = true;
            out.push(listing[i].clone());
        }
    }
    out
}

/// The files the claim deletes from main's tip because they carry, word for word or
/// re-wrapped ([`entry_key`]), an entry the release shipped raw from `source`'s `[Unreleased]`:
/// main moved it into a file after the publish, and left there the next release would refuse it
/// as a repeat. Each shipped entry removes at most one of main's files, none that `removed`
/// already holds; `text_of` reads a listed blob's text (`None` where it cannot).
/// Serves the drain of entries written before fragments; it goes once `[Unreleased]` holds none.
#[must_use]
pub fn landing_raw_removals(
    source: &str,
    listing: &[(String, String)],
    removed: &[(String, String)],
    text_of: &dyn Fn(&str) -> Option<String>,
) -> Vec<(String, String)> {
    let mut shipped: Vec<String> = unreleased_entries(source)
        .iter()
        .map(|text| entry_key(text))
        .collect();
    let mut out = Vec::new();
    for listed in listing {
        if shipped.is_empty() {
            break;
        }
        if removed.contains(listed) {
            continue;
        }
        let Some(text) = text_of(&listed.1) else {
            continue;
        };
        let key = entry_key(&text);
        if let Some(at) = shipped.iter().position(|held| *held == key) {
            shipped.swap_remove(at);
            out.push(listed.clone());
        }
    }
    out
}

/// A blob's text by id where it is UTF-8 within the one-entry bound, for the landing's
/// reads of main's files, which never refuse.
#[must_use]
pub fn small_blob_text(git: &dyn GitRunner, oid: &str) -> Option<String> {
    let size = git_read_ok(
        git,
        &[
            "--no-lazy-fetch",
            "--no-replace-objects",
            "cat-file",
            "-s",
            oid,
        ],
    )
    .ok()?
    .stdout;
    let size: usize = std::str::from_utf8(&size).ok()?.trim().parse().ok()?;
    if size > MAX_FRAGMENT_BYTES {
        return None;
    }
    blob_text(git, oid).ok()
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
