// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The tooling cache's share of the reclaim.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::{Planned, RECLAIM_IDLE, Unused};

const DEPTH: usize = 2;
/// What [`reclaim`] renames an entry `<name>` to before removing it: the stage names from
/// `.provision-reclaim-<name>` ([`super::stage_aside`]), each read as a stage by this prefix.
const STAGE: &str = ".provision-reclaim-";

/// The base of the stage names entry `name` takes: `.provision-reclaim-<name>`.
fn stage_base(name: &std::ffi::OsStr) -> std::ffi::OsString {
    let mut base = std::ffi::OsString::from(STAGE);
    base.push(name);
    base
}

/// The tooling cache under `home`, on macOS's rule (`aterm_uds::host::user_cache`):
/// `~/Library/Caches/aterm/tooling.noindex`.
#[must_use]
pub fn root(home: &Path) -> PathBuf {
    aterm_uds::host::user_cache("macos", Some(home), None, aterm_uds::host::TOOLING)
        .unwrap_or_else(|| home.join("Library/Caches/aterm/tooling.noindex"))
}

fn meta_used(meta: &std::fs::Metadata) -> Option<SystemTime> {
    let mut newest = meta.modified().ok()?;
    if meta.is_file()
        && let Ok(read) = meta.accessed()
    {
        newest = newest.max(read);
    }
    Some(newest)
}

fn last_used(path: &Path, depth: usize) -> Option<SystemTime> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    let mut newest = meta_used(&meta)?;
    if meta.is_dir() && depth > 0 {
        for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
            if let Ok(emeta) = entry.metadata() {
                if let Some(used) = meta_used(&emeta) {
                    newest = newest.max(used);
                }
                if emeta.is_dir()
                    && depth > 1
                    && let Some(used) = last_used(&entry.path(), depth - 1)
                {
                    newest = newest.max(used);
                }
            }
        }
    }
    Some(newest)
}

// Scratch rows are reclaimed only when both EXEC_STAMP and RAN exist as regular files, since
// elapsed time or a reboot cannot prove an image ran.
fn why(entry: &Path, now: SystemTime) -> Option<Unused> {
    let name = entry.file_name()?.to_str()?;
    if !std::fs::symlink_metadata(entry).is_ok_and(|m| m.is_dir()) {
        return None;
    }
    if let Some(original) = name.strip_prefix(STAGE) {
        return (!original.starts_with(aterm_uds::host::TOOLING_SCRATCH))
            .then_some(Unused::Unfinished);
    }
    if name.starts_with(aterm_uds::host::TOOLING_SCRATCH) {
        let given_up = [aterm_uds::deadline::EXEC_STAMP, aterm_uds::deadline::RAN]
            .iter()
            .all(|stamp| std::fs::symlink_metadata(entry.join(stamp)).is_ok_and(|m| m.is_file()));
        return given_up.then_some(Unused::GivenUp);
    }
    let idle = now
        .duration_since(last_used(entry, DEPTH)?)
        .unwrap_or(Duration::ZERO);
    let after = RECLAIM_IDLE;
    (idle >= after).then_some(Unused::Tooling { idle, after })
}

fn entries(root: &Path) -> Vec<PathBuf> {
    if !std::fs::symlink_metadata(root).is_ok_and(|m| m.is_dir()) {
        return Vec::new();
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    found.sort();
    found
}

/// Whether [`reclaim`] removes `entry`, judged `why`, in place: a stage, or a given-up scratch.
/// Every other entry is staged first under a name of its own.
const fn in_place(why: Unused) -> bool {
    matches!(why, Unused::Unfinished | Unused::GivenUp)
}

/// Advisory tooling cleanup candidates at `now`. An entry that needs staging is listed only
/// while it can be staged ([`super::stage_for`]), and a stage an earlier pass left with the
/// forecast of what of it will stay ([`Planned::stage`]). [`reclaim`] checks active use before
/// removal; this read-only observation does not probe process ownership.
#[must_use]
pub fn plan(root: &Path, now: SystemTime) -> Vec<Planned> {
    entries(root)
        .into_iter()
        .filter_map(|entry| {
            let why = why(&entry, now)?;
            if !in_place(why) {
                super::stage_for(&entry, root, &stage_base(entry.file_name()?))?;
            }
            if why == Unused::Unfinished {
                return Some(Planned::stage(entry));
            }
            Some(Planned {
                path: entry.clone(),
                why,
                paths: vec![entry],
                held: false,
                stays: None,
            })
        })
        .collect()
}

/// Remove what nobody uses from the tooling cache `root` at `now` ([`why`]), `record` asked first
/// for each (`false` keeps it); each removed whole ([`super::remove::tree`]) is returned with why,
/// a stage some entry of which stayed with what stayed ([`Unused::Stayed`]), for the next pass,
/// and an entry whose intent is on record and nothing of which went (its rename aside refused
/// after the check, or a process came to use it since; one in a stage by then is returned as the
/// stage, [`super::Refusal::Staged`]) with [`Unused::Refused`], so every intent on record closes
/// with an outcome. Each stage is visited once a pass, as the one listing of
/// `root` reads it, and an entry is staged under the lowest free stage name from
/// `.provision-reclaim-<name>`, never over another entry: a stage that keeps an entry costs only
/// itself, never the next entry of its name.
pub fn reclaim(
    root: &Path,
    now: SystemTime,
    record: &mut dyn FnMut(&Path, Unused) -> bool,
) -> Vec<(PathBuf, Unused)> {
    reclaim_on(root, now, record, &super::fence::Fence::default())
}

pub(super) fn reclaim_on(
    root: &Path,
    now: SystemTime,
    record: &mut dyn FnMut(&Path, Unused) -> bool,
    fence: &super::fence::Fence<'_>,
) -> Vec<(PathBuf, Unused)> {
    let mut intents = super::Intents::new(record);
    for entry in entries(root) {
        let Some(why) = why(&entry, now) else {
            continue;
        };
        let base = entry.file_name().map(stage_base);
        if !in_place(why)
            && base
                .as_ref()
                .is_none_or(|b| super::stage_for(&entry, root, b).is_none())
        {
            continue;
        }
        // An interrupted stage may still be reported by the executable's old spelling.
        let originals = if in_place(why) {
            originals_of_stage(root, &entry)
        } else {
            vec![entry.clone()]
        };
        let clear = |current: &Path| {
            let paths: Vec<&Path> = originals
                .iter()
                .map(PathBuf::as_path)
                .chain([current])
                .collect();
            fence.clear_all(current, &paths)
        };
        // A stage an earlier pass left is in its stage from its intent on.
        let asked = if why == Unused::Unfinished {
            clear(&entry) && intents.ask_staged(&entry, why)
        } else {
            clear(&entry) && intents.ask(&entry, why)
        };
        if !asked {
            continue;
        }
        if !clear(&entry) {
            intents.stop();
            continue;
        }
        let stage = match &base {
            Some(base) if !in_place(why) => {
                super::seams::staging(&entry);
                match super::stage_aside(&entry, root, base) {
                    Ok(stage) => {
                        intents.moved(&stage);
                        stage
                    }
                    Err(e) => {
                        intents.refused(super::Refusal::Rename(e.kind()));
                        continue;
                    }
                }
            }
            _ => entry.clone(),
        };
        if !clear(&stage) {
            intents.stop();
            continue;
        }
        let left = super::remove::tree(&stage);
        if left.whole() {
            intents.outcome(entry, why);
        } else {
            intents.outcome(
                stage,
                Unused::Stayed {
                    stayed: left.stayed,
                    freed: left.freed,
                },
            );
        }
    }
    intents.finish()
}

/// The original tooling row a stage name records, including a later unique stage's suffix.
fn originals_of_stage(root: &Path, stage: &Path) -> Vec<PathBuf> {
    let Some(name) = stage
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix(STAGE))
    else {
        return vec![stage.to_path_buf()];
    };
    // `fish-3` may be the original row's name or the third stage of `fish`: both aliases
    // retain ownership when an old executable spelling cannot distinguish the two.
    let mut originals = vec![root.join(name)];
    if let Some((head, tail)) = name.rsplit_once('-')
        && !tail.is_empty()
        && tail.bytes().all(|byte| byte.is_ascii_digit())
    {
        originals.push(root.join(head));
    }
    originals
}

#[cfg(test)]
#[path = "build_cache_tooling_tests.rs"]
mod tests;
