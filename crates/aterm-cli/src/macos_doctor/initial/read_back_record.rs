// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The last read-back of the rows System State reads privately, kept across launches (System
//! State design §3.5).
//!
//! Such a row (another app's data: the Mac settings, the sign-outs' account state, Rust in
//! Spotlight) is read only while System State is in front with a person, or at a stage's end,
//! whose read-back is its reading. A process keeps its readings in memory alone, so without this
//! record a relaunch (an update, Quit & Reopen, a restart of the Mac) would leave every such row
//! with no reading at all until a person opened System State: an unattended finished Mac would
//! read `unread:` forever. The record keeps each such row's latest reading while it reads the
//! row settled ([`Observation::read_back_settled`]: as wanted, or changed since aterm set it),
//! with the time it was read, so the next launch says the row set up as it was last read (or
//! changed since aterm set it, as it read then), with its age, never as in place now.
//!
//! It is fenced, never trusted: it holds for the run revision it was written under, and each row
//! holds while the change journal ([`aterm_converge::FileJournal`], where every pass of any aterm
//! process writes each change before it makes it) holds no change written since that row's read
//! started ([`Fence`], taken as the read started, or as the stage that read it back ended, after
//! its own changes): the same file, grown by nothing but failure lines. Any change line counts,
//! whichever item it names, so a fence can only leave a row unread, never carry one across a
//! change to it. The fence is checked as the record is written (a reading this process holds,
//! another process's change since its read started included) and again as the next launch reads
//! it. A change since, a journal rotated or replaced, a newer plan, an unreadable or other
//! format's record each leave the rows unread, which says nothing done. A row that reads
//! otherwise drops out of the record with its next write, so a row not settled is never
//! carried forward as settled.

use super::*;
use aterm_types::macos_setup::{Observation, Read, Standing};

/// The record, beside the run's records in the state root.
pub(in crate::macos_doctor) const READ_BACK_RECORD: &str = "setup-read-back.json";

/// The format this build writes; any other is a record this build does not read (format 1 fenced
/// every row by the journal as the record was written, not as each row was read, so it carried
/// readings across a change made between them).
const READ_BACK_FORMAT: u64 = 2;

/// The change journal at one moment: its file's identity and size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::macos_doctor) struct JournalMark {
    inode: u64,
    len: u64,
}

/// The change journal as a row's read started (`None` inside: no journal file then), which
/// fences that reading ([`no_change_since`]): it stands only while the journal holds no change
/// written since. A read whose journal could not be read has none, and nothing keeps it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fence(Option<JournalMark>);

impl Fence {
    /// The journal file `inode` at `len` bytes (tests of the crates that carry a fence).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub const fn at(inode: u64, len: u64) -> Self {
        Self(Some(JournalMark { inode, len }))
    }
}

/// The account's change journal as it stands now, fencing a read that starts now ([`Fence`]);
/// `None` where it cannot be read.
#[must_use]
pub fn journal_fence() -> Option<Fence> {
    let (_, journal) = places().ok()?;
    JournalMark::of(&journal).ok().map(Fence)
}

impl JournalMark {
    /// The journal file at `path` as it stands: `Ok(None)` with no file there.
    ///
    /// # Errors
    /// Its metadata cannot be read.
    pub(in crate::macos_doctor) fn of(path: &Path) -> std::io::Result<Option<Self>> {
        use std::os::unix::fs::MetadataExt as _;
        match std::fs::symlink_metadata(path) {
            Ok(meta) => Ok(Some(Self {
                inode: meta.ino(),
                len: meta.len(),
            })),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn record(mark: Option<Self>) -> Value {
        mark.map_or(
            Value::Null,
            |mark| json!({ "inode": mark.inode, "len": mark.len }),
        )
    }

    fn parse(value: &Value) -> Option<Option<Self>> {
        if value.is_null() {
            return Some(None);
        }
        Some(Some(Self {
            inode: value["inode"].as_u64()?,
            len: value["len"].as_u64()?,
        }))
    }
}

/// Whether the journal at `path` holds no change written since it stood as `then`: the same
/// file (with none then, no file now, or one whose every line is a failure line), grown by
/// failure lines alone. A rotation or a replaced file is a new file, whose lines may be changes:
/// it holds no such promise. Anything that cannot be read promises nothing.
pub(in crate::macos_doctor) fn no_change_since(then: Option<JournalMark>, path: &Path) -> bool {
    let Ok(now) = JournalMark::of(path) else {
        return false;
    };
    let from = match (then, now) {
        (_, None) => return then.is_none(),
        (None, Some(_)) => 0,
        (Some(then), Some(now)) if now.inode == then.inode && now.len >= then.len => then.len,
        (Some(_), Some(_)) => return false,
    };
    let Ok((bytes, _)) =
        aterm_uds::atomic_file::read_regular(path, aterm_converge::FileJournal::BOUND)
    else {
        return false;
    };
    let Some(since) = usize::try_from(from)
        .ok()
        .and_then(|from| bytes.get(from..))
    else {
        return false;
    };
    since
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .all(|line| {
            aterm_json::from_slice::<Value>(line).is_ok_and(|line| {
                line.get("failed").and_then(Value::as_str).is_some()
                    && line.get("to").is_none()
                    && line.get("from").is_none()
            })
        })
}

/// Whether `seen` is a row this record keeps: read privately, read back settled, dated.
fn kept(seen: &Observation) -> bool {
    seen.subject.reads_privately() && seen.read_back_settled()
}

/// The record of `rows` under `revision`: those it keeps ([`kept`]) whose fence still holds at
/// the journal at `journal` ([`no_change_since`]), each with its fence.
pub(in crate::macos_doctor) fn record(
    revision: u64,
    rows: &[(Observation, Fence)],
    journal: &Path,
) -> Value {
    let rows: Vec<Value> = rows
        .iter()
        .filter(|(seen, fence)| kept(seen) && no_change_since(fence.0, journal))
        .filter_map(|(seen, fence)| {
            Some(json!({
                "row": seen.subject.setup_word(),
                "standing": seen.standing.word(),
                "now": seen.now,
                "after_login": seen.after_login,
                "wanted": seen.wanted,
                "note": seen.note,
                "detail": seen.detail,
                "read_at": seen.read_at()?,
                "journal": JournalMark::record(fence.0),
            }))
        })
        .collect();
    json!({
        "format": READ_BACK_FORMAT,
        "revision": revision,
        "rows": rows,
    })
}

/// The rows `record` keeps, each with its fence, where it was written under `revision` and the
/// journal at `journal` holds no change since that row's read started ([`no_change_since`]);
/// none otherwise, and none of a row this build does not know or would not keep.
pub(in crate::macos_doctor) fn parse(
    record: &Value,
    revision: u64,
    journal: &Path,
) -> Vec<(Observation, Fence)> {
    if record["format"].as_u64() != Some(READ_BACK_FORMAT)
        || record["revision"].as_u64() != Some(revision)
    {
        return Vec::new();
    }
    let text = |value: &Value| -> Option<Option<String>> {
        match value {
            Value::Null => Some(None),
            Value::String(text) => Some(Some(text.clone())),
            _ => None,
        }
    };
    let mut rows: Vec<(Observation, Fence)> = Vec::new();
    for entry in record["rows"].as_array().into_iter().flatten() {
        let Some(fence) = JournalMark::parse(&entry["journal"])
            .map(Fence)
            .filter(|fence| no_change_since(fence.0, journal))
        else {
            continue;
        };
        let Some(seen) = (|| {
            Some(Observation {
                subject: Subject::from_setup_word(entry["row"].as_str()?)?,
                standing: Standing::from_word(entry["standing"].as_str()?)?,
                now: text(&entry["now"])?,
                after_login: text(&entry["after_login"])?,
                wanted: entry["wanted"].as_str()?.to_owned(),
                note: text(&entry["note"])?,
                detail: text(&entry["detail"])?,
                read: Read::At(entry["read_at"].as_u64()?),
            })
        })() else {
            continue;
        };
        if kept(&seen) && !rows.iter().any(|(row, _)| row.subject == seen.subject) {
            rows.push((seen, fence));
        }
    }
    rows
}

/// Where the record and the journal it is fenced by live: the doctor's state root, and the
/// account's change journal.
fn places() -> Result<(PathBuf, PathBuf), String> {
    let journal = aterm_types::dirs::account_state_dir()
        .map(|root| aterm_converge::FileJournal::under(&root).path())
        .ok_or_else(|| "this account has no state directory".to_owned())?;
    Ok((state_root()?.join(READ_BACK_RECORD), journal))
}

/// Keep `rows`, this process's latest readings, each with the journal as its read started, as
/// the private rows' last read-back: those read back settled ([`kept`]) whose fence still
/// holds now (nothing journaled since that read started, by any aterm process), under the
/// current run revision.
///
/// # Errors
/// The record cannot be written, or the revision read: the next launch's rows are then unread,
/// which says nothing done.
pub fn remember_private_read_back(rows: &[(Observation, Fence)]) -> Result<(), String> {
    let (path, journal) = places()?;
    let revision = initial_setup_revision()?;
    write_json(&path, &record(revision, rows, &journal))
}

/// The private rows' last read-back a previous launch kept ([`remember_private_read_back`]),
/// each with its fence, where it still holds: written under the current run revision, with
/// nothing journaled since that row's read started. None otherwise.
#[must_use]
pub fn remembered_private_read_back() -> Vec<(Observation, Fence)> {
    let Ok((path, journal)) = places() else {
        return Vec::new();
    };
    let Ok(revision) = initial_setup_revision() else {
        return Vec::new();
    };
    crate::macos_doctor::read_record_bytes(&path)
        .ok()
        .and_then(|bytes| aterm_json::from_slice::<Value>(&bytes).ok())
        .map_or_else(Vec::new, |record| parse(&record, revision, &journal))
}

#[cfg(test)]
#[path = "read_back_record_tests.rs"]
mod tests;
