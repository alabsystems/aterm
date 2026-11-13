// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dev-set changelog judgment of what a change adds, on the merged checkout.
//!
//! A change's changelog entry is its own file under `changes/`
//! ([`crate::changelog_fragments`]), so the rules judge what the change adds against its base,
//! never the whole tree: a stray entry someone pushed to main is the release's step 1 to name
//! (which opens one repair), not a red every later landing inherits and many sessions race to fix.
//!
//! (a) A top-level entry in `CHANGELOG.md` outside a released `## [X.Y.Z]` section that the base
//!     did not hold is red: it belongs in a file. So is a line the change adds under
//!     `[Unreleased]` that the release's roll refuses ([`unreleased_findings`]). Moving or
//!     removing an entry the base held passes.
//! (b) Every path the change adds or changes under `changes/` (in any case of the name, or a
//!     file named `changes`) passes [`fragment_findings`], as a regular, non-executable file
//!     within the one-entry bound, which is checked before the file is read.
//! (c) Such a fragment whose entry (by [`entry_key`]) a release's own tag already holds, that a
//!     raw `[Unreleased]` entry holds, or that another fragment of the tree holds, is red: the
//!     next release would ship it twice. A released section whose tag this clone lacks (a
//!     version abandoned before it shipped) shipped nothing.
//! (d) A released section that gains or loses an entry against the base is red: it records what
//!     its release shipped, and git's line merge across a release is what leaves a branch's raw
//!     entry inside the section main just rolled, or applies a branch's removal of a raw entry
//!     inside it, both as clean merges.

use crate::changelog_fragments::{
    FRAGMENT_DIR, MAX_FRAGMENT_BYTES, MAX_TOTAL_BYTES, Section, entries, entry_key,
    fragment_findings, in_entry_dir, unreleased_findings,
};
use crate::{Ctx, Scope};
use aterm_census::changelog::{CHANGELOG_FILE, KINDS, has_section};
use std::path::Path;

/// The paths this change touches that the rules read, and the commit it is judged against:
/// `None` when it touches neither `CHANGELOG.md` nor the entry directory. Listing failures
/// remain failures to run, rather than silently omitting the check.
fn selection(ctx: &Ctx) -> Result<Option<(String, Vec<String>)>, String> {
    // The release owns its window's judgment: its step 1 judges the whole assembled notes.
    if ctx.release {
        return Ok(None);
    }
    let base = match &ctx.scope {
        Scope::Changed(changed) => changed.base.as_str(),
        _ => "origin/main",
    };
    let (merge_base, listed) = crate::changed::changed_paths(&ctx.root, &ctx.path_env, base)?;
    let paths = ctx.input_paths.clone().unwrap_or(listed);
    let read: Vec<String> = paths
        .into_iter()
        .filter(|path| path == CHANGELOG_FILE || in_entry_dir(path))
        .collect();
    Ok((!read.is_empty()).then_some((merge_base, read)))
}

/// Whether the rules judge this change at all.
///
/// # Errors
/// The change's paths could not be listed.
pub fn selected(ctx: &Ctx) -> Result<bool, String> {
    selection(ctx).map(|found| found.is_some())
}

/// `rev:path`'s text, `None` where `rev` holds no such file.
fn text_at(ctx: &Ctx, rev: &str, path: &str) -> Result<Option<String>, String> {
    let out = crate::changed::git(&ctx.root, &ctx.path_env)
        .args(["cat-file", "-e", &format!("{rev}:{path}")])
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("git cat-file: {e}"))?;
    if !out.success() {
        return Ok(None);
    }
    let out = crate::changed::git(&ctx.root, &ctx.path_env)
        .args(["cat-file", "blob", &format!("{rev}:{path}")])
        .output()
        .map_err(|e| format!("git cat-file: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git cat-file blob {rev}:{path}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    String::from_utf8(out.stdout)
        .map(Some)
        .map_err(|_| format!("{rev}:{path} is not UTF-8"))
}

/// The kinds' directory names, for messages.
fn kinds() -> String {
    KINDS
        .iter()
        .map(|kind| kind.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether release `version`'s own tag, as this clone holds it, shipped an entry whose words are
/// `key`: its `CHANGELOG.md`'s `[version]` section (or its `[Unreleased]`, for a tag cut before
/// the roll wrote the section), the shipment evidence the release's own audit reads.
fn tag_shipped(ctx: &Ctx, version: &str, key: &str) -> Result<bool, String> {
    let Some(text) = text_at(ctx, &format!("refs/tags/v{version}"), CHANGELOG_FILE)? else {
        return Ok(false);
    };
    let section = if has_section(&text, version) {
        Section::Released(version)
    } else {
        Section::Unreleased
    };
    Ok(entries(&text)
        .into_iter()
        .any(|e| e.section == section && entry_key(&e.text) == key))
}

/// (a) and (d): what the change does to `CHANGELOG.md` against its base's `was`.
fn changelog_findings(was: &str, changelog: &str, found: &mut Vec<String>) {
    let mut held: Vec<String> = entries(was)
        .into_iter()
        .filter(|e| e.section == Section::Unreleased)
        .map(|e| e.text)
        .collect();
    for entry in entries(changelog)
        .into_iter()
        .filter(|e| e.section == Section::Unreleased)
    {
        if let Some(at) = held.iter().position(|text| *text == entry.text) {
            held.swap_remove(at);
            continue;
        }
        found.push(format!(
            "{CHANGELOG_FILE} line {}: an entry outside every released section — a change's \
             changelog entry is its own file: move it to \
             changes/<kind>/<session>-<slug>.md (kind: {}) and leave {CHANGELOG_FILE} as \
             its base holds it, less any entry the new one replaces; `ship release` writes \
             [Unreleased] from those files",
            entry.line,
            kinds()
        ));
    }
    // Serves the drain of entries written before fragments: the roll's raw arm refuses these
    // lines, so a branch that adds one is told now; it goes once [Unreleased] holds none.
    let was_lines: Vec<&str> = was.lines().collect();
    let mut odd: Vec<&str> = unreleased_findings(was)
        .into_iter()
        .filter_map(|(n, _)| was_lines.get(n - 1).copied())
        .collect();
    let lines: Vec<&str> = changelog.lines().collect();
    for (n, message) in unreleased_findings(changelog) {
        let line = lines.get(n - 1).copied().unwrap_or_default();
        if let Some(at) = odd.iter().position(|held| *held == line) {
            odd.swap_remove(at);
            continue;
        }
        found.push(message);
    }
    let released = |text: &str| -> Vec<(String, usize, String)> {
        entries(text)
            .into_iter()
            .filter_map(|e| match e.section {
                Section::Released(version) => Some((version.to_string(), e.line, e.text)),
                Section::Unreleased => None,
            })
            .collect()
    };
    let mut base = released(was);
    for (version, line, text) in released(changelog) {
        if let Some(at) = base
            .iter()
            .position(|(v, _, held)| *v == version && *held == text)
        {
            base.remove(at);
            continue;
        }
        found.push(format!(
            "{CHANGELOG_FILE} line {line}: [{version}] gains an entry its base's [{version}] \
             lacks — a released section records what v{version} shipped, and a merge across \
             the release can leave a branch's entry inside it: keep [{version}] as the base \
             holds it, and write the entry as its own file, \
             changes/<kind>/<session>-<slug>.md (kind: {}), which the next release ships",
            kinds()
        ));
    }
    for (version, line, _) in base {
        found.push(format!(
            "{CHANGELOG_FILE}: [{version}] loses the entry its base holds at line {line} — a \
             released section records what v{version} shipped, and a merge across the release \
             can apply a branch's removal of an unreleased entry inside it: keep [{version}] as \
             the base holds it"
        ));
    }
}

/// Judge the working tree, including staged and unstaged changes. The land driver merges and
/// rehomes before constructing this selection, so this is the tree it will push.
///
/// # Errors
/// The change's paths, its base's `CHANGELOG.md`, a release tag's or a file it names could not
/// be read.
pub fn judge(ctx: &Ctx) -> Result<Option<Vec<String>>, String> {
    let Some((base, paths)) = selection(ctx)? else {
        return Ok(None);
    };
    let changelog = match std::fs::read(ctx.root.join(CHANGELOG_FILE)) {
        Ok(bytes) => {
            String::from_utf8(bytes).map_err(|_| format!("{CHANGELOG_FILE} is not UTF-8"))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("read {CHANGELOG_FILE}: {e}")),
    };
    let mut found = Vec::new();
    if paths.iter().any(|path| path == CHANGELOG_FILE) {
        let was = text_at(ctx, &base, CHANGELOG_FILE)?.unwrap_or_default();
        changelog_findings(&was, &changelog, &mut found);
    }
    let all = entries(&changelog);
    // Serves the drain of entries written before fragments: a raw [Unreleased] entry ships as
    // it stands, so a file repeating it ships twice; it goes once [Unreleased] holds none.
    let raw: Vec<(String, usize)> = all
        .iter()
        .filter(|e| e.section == Section::Unreleased)
        .map(|e| (entry_key(&e.text), e.line))
        .collect();
    let released: Vec<(String, &str)> = all
        .iter()
        .filter_map(|e| match e.section {
            Section::Released(version) => Some((entry_key(&e.text), version)),
            Section::Unreleased => None,
        })
        .collect();
    let tree = fragments_in(&ctx.root)?;
    for path in paths.iter().filter(|path| in_entry_dir(path)) {
        let full = ctx.root.join(path);
        let Ok(meta) = std::fs::symlink_metadata(&full) else {
            continue;
        };
        if !regular(&meta) {
            found.push(format!(
                "{path}: is not a regular, non-executable file — a changelog entry is a plain \
                 file the release reads as it ships"
            ));
            continue;
        }
        if meta.len() > MAX_FRAGMENT_BYTES as u64 {
            found.push(format!(
                "{path}: is {} bytes, over the {MAX_FRAGMENT_BYTES} one entry may hold",
                meta.len()
            ));
            continue;
        }
        let bytes = std::fs::read(&full).map_err(|e| format!("read {path}: {e}"))?;
        let Ok(text) = String::from_utf8(bytes) else {
            found.push(format!(
                "{path}: is not UTF-8 text — a changelog entry ships as written"
            ));
            continue;
        };
        let findings = fragment_findings(path, &text);
        if !findings.is_empty() {
            found.extend(findings);
            continue;
        }
        let key = entry_key(&text);
        if let Some((_, line)) = raw.iter().find(|(held, _)| *held == key) {
            found.push(format!(
                "{path}: {CHANGELOG_FILE} line {line} holds this entry under [Unreleased] too, \
                 and the next release ships both: delete this file or the raw copy"
            ));
        }
        let mut judged: Vec<&str> = Vec::new();
        for (_, version) in released.iter().filter(|(held, _)| *held == key) {
            if judged.contains(version) {
                continue;
            }
            judged.push(version);
            if tag_shipped(ctx, version, &key)? {
                found.push(format!(
                    "{path}: v{version}'s tag holds this entry (`git show \
                     v{version}:CHANGELOG.md`) — v{version} shipped it, so the next release \
                     would ship it again: delete this file. If v{version} was abandoned and \
                     origin's tag deleted, this clone's tag is stale: `git tag -d v{version}`"
                ));
                break;
            }
        }
        if let Some((twin, _)) = tree.iter().find(|(other, bytes)| {
            other != path
                && bytes
                    .as_ref()
                    .is_some_and(|b| entry_key(&String::from_utf8_lossy(b)) == key)
        }) {
            found.push(format!(
                "{path}: {twin} holds this entry too, so the next release would ship it \
                 twice: delete one"
            ));
        }
    }
    Ok(Some(found))
}

/// Whether `meta` is a regular file no one may execute.
fn regular(meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.file_type().is_file() && meta.permissions().mode() & 0o111 == 0
    }
    #[cfg(not(unix))]
    {
        meta.file_type().is_file()
    }
}

/// One file under `changes/`: its repository-relative path and its bytes where it is within the
/// one-entry bound (`None` past it, unread).
type Listed = (String, Option<Vec<u8>>);

/// Every regular file under `root/changes/`, sorted by path. Reading stops once the files read
/// pass the window's bound, which the release refuses whole.
fn fragments_in(root: &Path) -> Result<Vec<Listed>, String> {
    let mut out = Vec::new();
    let mut total = 0u64;
    let mut stack = vec![FRAGMENT_DIR.to_string()];
    while let Some(dir) = stack.pop() {
        let listing = match std::fs::read_dir(root.join(&dir)) {
            Ok(listing) => listing,
            // A file named `changes` is (b)'s to name, not a failure to run.
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                continue;
            }
            Err(e) => return Err(format!("read {dir}/: {e}")),
        };
        for item in listing {
            let item = item.map_err(|e| format!("read {dir}/: {e}"))?;
            let path = format!("{dir}/{}", item.file_name().to_string_lossy());
            let kind = item.file_type().map_err(|e| format!("read {path}: {e}"))?;
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() {
                let len = item
                    .metadata()
                    .map_err(|e| format!("read {path}: {e}"))?
                    .len();
                total = total.saturating_add(len);
                let bytes = if len > MAX_FRAGMENT_BYTES as u64 || total > MAX_TOTAL_BYTES as u64 {
                    None
                } else {
                    Some(std::fs::read(item.path()).map_err(|e| format!("read {path}: {e}"))?)
                };
                out.push((path, bytes));
            }
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
#[path = "changelog_guard_tests.rs"]
mod tests;
