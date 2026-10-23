// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retention pruning for `<aterm state>/drive` ([`prune`], [`DRIVE_BUDGET`],
//! [`tab_ids_in`], [`registry_tabs`], [`crate::supervise::approvals::bound`],
//! [`super::upgrade_drive::STATE_KEEP_S`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::tap::is_tab_id;

/// How far the drive directory's groups may grow before the oldest go: files AND bytes ([`prune`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    /// The most group files kept.
    pub files: usize,
    /// The most bytes those files hold.
    pub bytes: u64,
}

/// Default retention budget for `<root>/drive`.
pub const DRIVE_BUDGET: Budget = Budget {
    files: 2048,
    bytes: 64 << 20,
};

/// The per-tab file kinds, the longest suffix first (`.journal.jsonl` before `.jsonl`).
const SUFFIXES: [&str; 4] = [
    ".journal.jsonl",
    ".goal-hold.json",
    ".earlier-tabs",
    ".jsonl",
];

/// Maximum directory entries visited in one retention pass.
const MAX_NAMES: usize = 65_536;

/// The largest record or layout read for the tabs it names: past it, nothing is proven unnamed.
const MAX_NAMING_BYTES: u64 = 16 << 20;

/// The tab a drive file's name belongs to ([`SUFFIXES`]).
fn group_of(name: &str) -> Option<&str> {
    SUFFIXES
        .iter()
        .find_map(|suffix| name.strip_suffix(suffix))
        .filter(|stem| is_tab_id(stem))
}

/// One tab's files.
#[derive(Default)]
struct Group {
    files: Vec<(PathBuf, u64)>,
    /// The newest mtime of its files, in unix seconds.
    newest: u64,
    /// One of its names is not a plain file, or its metadata cannot be read: kept.
    unreadable: bool,
}

/// Prune unprotected tab groups in `<root>/drive` ([`super::upgrade_drive::STATE_KEEP_S`]).
pub fn prune(
    root: &Path,
    named: impl FnOnce() -> Option<BTreeSet<String>>,
    now: u64,
    budget: Budget,
) -> Vec<String> {
    prune_with_limit(root, named, now, budget, MAX_NAMES)
}

fn prune_with_limit(
    root: &Path,
    named: impl FnOnce() -> Option<BTreeSet<String>>,
    now: u64,
    budget: Budget,
    max_names: usize,
) -> Vec<String> {
    let drive = root.join("drive");
    let Ok(entries) = std::fs::read_dir(&drive) else {
        return Vec::new();
    };
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for (at, entry) in entries.enumerate() {
        if at >= max_names {
            return Vec::new();
        }
        let Ok(entry) = entry else {
            return Vec::new();
        };
        let name = entry.file_name();
        let Some(stem) = name.to_str().and_then(group_of) else {
            continue;
        };
        let group = groups.entry(stem.to_string()).or_default();
        match entry.metadata() {
            Ok(meta) if meta.is_file() => {
                let at = meta
                    .modified()
                    .ok()
                    .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok());
                match at {
                    Some(at) => group.newest = group.newest.max(at.as_secs()),
                    None => group.unreadable = true,
                }
                group.files.push((entry.path(), meta.len()));
            }
            _ => group.unreadable = true,
        }
    }
    let mut files: usize = groups.values().map(|g| g.files.len()).sum();
    let mut bytes: u64 = groups
        .values()
        .flat_map(|g| g.files.iter().map(|(_, len)| *len))
        .fold(0_u64, u64::saturating_add);
    let within = |files: usize, bytes: u64| files <= budget.files && bytes <= budget.bytes;
    if within(files, bytes) {
        return Vec::new();
    }
    let Some(named) = named() else {
        return Vec::new();
    };
    let mut kept: BTreeSet<String> = groups
        .iter()
        .filter(|(stem, g)| {
            named.contains(stem.as_str())
                || g.unreadable
                || now.saturating_sub(g.newest) < super::upgrade_drive::STATE_KEEP_S
        })
        .map(|(stem, _)| stem.clone())
        .collect();
    let mut visit: Vec<String> = kept.iter().cloned().collect();
    while let Some(stem) = visit.pop() {
        let Some(earlier) = noted(&drive, &stem) else {
            return Vec::new();
        };
        for was in earlier {
            if groups.contains_key(&was) && kept.insert(was.clone()) {
                visit.push(was);
            }
        }
    }
    let mut oldest: Vec<(&String, &Group)> = groups
        .iter()
        .filter(|(stem, _)| !kept.contains(stem.as_str()))
        .collect();
    oldest.sort_by_key(|(stem, g)| (g.newest, *stem));
    let mut gone = Vec::new();
    for (stem, g) in oldest {
        if within(files, bytes) {
            break;
        }
        for (path, len) in &g.files {
            if std::fs::remove_file(path).is_ok() {
                files -= 1;
                bytes = bytes.saturating_sub(*len);
            }
        }
        gone.push(stem.clone());
    }
    gone
}

/// The tab ids present in one rendezvous registry listing (`<control dir>/graph`) ([`MAX_NAMES`]).
pub fn registry_tabs(graph: &Path, known_live: &[String]) -> Option<BTreeSet<String>> {
    registry_tabs_with_limit(graph, known_live, MAX_NAMES)
}

fn registry_tabs_with_limit(
    graph: &Path,
    known_live: &[String],
    max_names: usize,
) -> Option<BTreeSet<String>> {
    if known_live.is_empty() {
        return None;
    }
    let mut out = BTreeSet::new();
    for (at, entry) in std::fs::read_dir(graph).ok()?.enumerate() {
        if at >= max_names {
            return None;
        }
        let name = entry.ok()?.file_name();
        if let Some(sid) = name.to_str().filter(|n| is_tab_id(n)) {
            out.insert(sid.to_string());
        }
    }
    known_live
        .iter()
        .all(|sid| out.contains(sid))
        .then_some(out)
}

/// Read tab IDs from `<drive>/<stem>.earlier-tabs` ([`super::upgrade_drive::note_earlier_tab`]).
fn noted(drive: &Path, stem: &str) -> Option<Vec<String>> {
    match super::cli::read_complete_text(
        &drive.join(format!("{stem}.earlier-tabs")),
        MAX_NAMING_BYTES,
    ) {
        Ok(body) => Some(
            body.lines()
                .filter(|t| is_tab_id(t))
                .map(str::to_string)
                .collect(),
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(Vec::new()),
        Err(_) => None,
    }
}

/// Extract every tab ID (`s-<hex>`, [`is_tab_id`]) occurring in `text`.
pub fn tab_ids_in(text: &str, out: &mut BTreeSet<String>) {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(at) = text.get(from..).and_then(|rest| rest.find("s-")) {
        let start = from + at;
        let hex = bytes.get(start + 2..).map_or(0, |rest| {
            rest.iter().take_while(|b| b.is_ascii_hexdigit()).count()
        });
        if let Some(id) = text.get(start..start + 2 + hex.min(64))
            && is_tab_id(id)
        {
            out.insert(id.to_string());
        }
        from = start + 2;
    }
}

/// Collect [`tab_ids_in`] from `dir` files matching `wanted` (up to [`MAX_NAMING_BYTES`]).
pub fn tabs_named_in(dir: &Path, wanted: impl Fn(&str) -> bool) -> Option<BTreeSet<String>> {
    tabs_named_with_limit(dir, wanted, MAX_NAMES)
}

fn tabs_named_with_limit(
    dir: &Path,
    wanted: impl Fn(&str) -> bool,
    max_names: usize,
) -> Option<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(out),
        Err(_) => return None,
    };
    for (at, entry) in entries.enumerate() {
        if at >= max_names {
            return None;
        }
        let entry = entry.ok()?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !wanted(name) {
            continue;
        }
        let kind = entry.file_type().ok()?;
        if kind.is_dir() {
            continue;
        }
        if !kind.is_file() {
            return None;
        }
        let text = super::cli::read_complete_text(&entry.path(), MAX_NAMING_BYTES).ok()?;
        tab_ids_in(&text, &mut out);
    }
    Some(out)
}

#[cfg(test)]
#[path = "drive_retention_tests.rs"]
mod tests;
