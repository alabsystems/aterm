// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Run numbers survive the progress record whose recovery retires it. Every
//! publication of progress reserves its number first, under the profile lock;
//! previews only read this floor, and an owned retry keeps its reserved number.
//!
//! A counter that does not parse is rebuilt, never fatal ([`rebuilt`]): reads take the rebuilt
//! floor, and the next reservation keeps the damaged bytes beside the record and writes it
//! again. One a newer aterm wrote is [`NEWER_SETUP`].

use super::*;

const RECORD: &str = "run-revision.json";

/// Legacy aterm allocated from one. A legacy recovery whose only run number
/// was lost enters a new era rather than reusing those approvals and caches.
/// Manually authored legacy upper-half run numbers are outside that migration.
const LEGACY_RECOVERY: u64 = 1 << 63;

/// The counter as it stands.
enum Counter {
    Missing,
    Kept(u64),
    /// It does not parse: the floor rebuilt from the records ([`rebuilt`]).
    Damaged(u64),
}

fn counter(root: &Path) -> Result<Counter, String> {
    let path = root.join(RECORD);
    let bytes = match crate::macos_doctor::read_record_bytes(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Counter::Missing),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let Some(value) = aterm_json::from_slice::<Value>(&bytes)
        .ok()
        .filter(|value| value["profile"].as_str() == Some(PROFILE))
    else {
        return Ok(Counter::Damaged(rebuilt(root)));
    };
    match known_version(&value["version"], "") {
        Err(why) if setup_is_newer(&why) => return Err(why),
        Err(_) => return Ok(Counter::Damaged(rebuilt(root))),
        Ok(()) => {}
    }
    Ok(value["revision"]
        .as_u64()
        .map_or_else(|| Counter::Damaged(rebuilt(root)), Counter::Kept))
}

fn read(root: &Path) -> Result<Option<u64>, String> {
    Ok(match counter(root)? {
        Counter::Missing => None,
        Counter::Kept(revision) | Counter::Damaged(revision) => Some(revision),
    })
}

/// The floor a counter that does not parse is rebuilt to: the highest run number any record in
/// `root` still names (a run's progress and the run it continues, a Stop asked for, a Restore's
/// run, a set-aside's checkpoint, an upgrade's or put-back's checkpoints) or the window's record
/// of access steps beside it names, in the recovery era ([`LEGACY_RECOVERY`]) at least, as a
/// recovery whose counter was lost enters it: no number a lost reservation took is used again.
fn rebuilt(root: &Path) -> u64 {
    fn named(value: &Value) -> u64 {
        match value {
            Value::Object(fields) => fields
                .iter()
                .map(|(key, field)| match (key.as_str(), field.as_u64()) {
                    ("revision" | "run" | "access_revision" | "continues_revision", Some(run)) => {
                        run
                    }
                    _ => named(field),
                })
                .max()
                .unwrap_or(0),
            Value::Array(items) => items.iter().map(named).max().unwrap_or(0),
            _ => 0,
        }
    }
    let records = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            std::path::Path::new(&entry.file_name())
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .map(|entry| entry.path())
        .filter_map(|path| crate::macos_doctor::read_record_bytes(&path).ok())
        .filter_map(|bytes| aterm_json::from_slice::<Value>(&bytes).ok())
        .map(|value| named(&value));
    let access = root
        .parent()
        .and_then(|dir| {
            crate::macos_doctor::read_record_text(
                &dir.join(aterm_types::macos_setup::access_record::FILE),
            )
            .ok()
        })
        .into_iter()
        .flat_map(|text| {
            text.lines()
                .filter_map(|line| {
                    let s = line.split_whitespace().next()?;
                    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                        .then(|| s.parse::<u64>().ok())
                        .flatten()
                })
                .collect::<Vec<_>>()
        });
    records
        .chain(access)
        .fold(LEGACY_RECOVERY, std::cmp::Ord::max)
}

/// The last reserved number; reading never allocates or creates state.
pub(super) fn floor(root: &Path) -> Result<u64, String> {
    Ok(read(root)?.unwrap_or(0))
}

/// Preview the next run after both a readable predecessor and the durable floor.
pub(super) fn next(root: &Path, observed: u64) -> Result<u64, String> {
    next_revision(floor(root)?.max(observed))
}

/// Whether an embedded run with a gap after its predecessor was reserved.
pub(super) fn reserved(root: &Path, revision: u64) -> Result<bool, String> {
    Ok(read(root)?.is_some_and(|floor| floor >= revision))
}

/// Reserve before publishing progress, including a checkpoint embedded in a
/// plan. A retry or put-back may still publish an earlier owned run: the floor
/// prevents reuse by a new admission and grants no authority to write one. A counter that does
/// not parse is kept beside itself first ([`set_aside`]) and written again, never below its
/// rebuilt floor.
pub(super) fn reserve(root: &Path, revision: u64) -> Result<(), String> {
    let revision = match counter(root)? {
        Counter::Kept(current) if current >= revision => {
            return flush_record_with(&root.join(RECORD), durable_name);
        }
        Counter::Damaged(rebuilt) => {
            set_aside::keep_aside(root, RECORD)?;
            revision.max(rebuilt)
        }
        Counter::Missing | Counter::Kept(_) => revision,
    };
    write_json(
        &root.join(RECORD),
        &json!({"profile": PROFILE, "version": 1, "revision": revision}),
    )
}

/// Under profile.lock: a counter that does not parse is kept beside itself and written again at
/// its rebuilt floor; whether it was.
pub(super) fn repair(root: &Path) -> Result<bool, String> {
    if !damaged(root)? {
        return Ok(false);
    }
    reserve(root, 0)?;
    Ok(true)
}

/// Whether the counter is there and does not parse.
pub(super) fn damaged(root: &Path) -> Result<bool, String> {
    Ok(matches!(counter(root)?, Counter::Damaged(_)))
}

/// Preserve the allocation namespace before a damaged setup loses progress.
/// An existing counter is never reset; only legacy recovery enters a new era.
pub(super) fn recover(root: &Path, previous: Option<u64>) -> Result<(), String> {
    let floor = read(root)?.unwrap_or(LEGACY_RECOVERY);
    reserve(root, floor.max(previous.unwrap_or(0)))
}

#[cfg(test)]
#[path = "revision_tests.rs"]
mod tests;
