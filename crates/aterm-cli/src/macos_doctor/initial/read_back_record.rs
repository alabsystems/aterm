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
//! It is fenced, never trusted: each row belongs to a run revision and carries the journal's
//! latest-change stamp for items that may write a private row. The bounded index keeps the
//! latest sequence of each item independently of journal rotation; its random incarnation
//! changes when its binding to the journal is lost. A same-incarnation stamp changes whenever
//! any selected item changes, even when its journal line has aged out. The selection excludes
//! only items known to write no private row (`aterm_machines::init::writes_no_private_row`).
//!
//! Where no usable index spans a reading, the current journal and its one rotation must still
//! hold every line since its file fence. Unknown or malformed lines, missing history, and a
//! changed revision leave the row unread. A successful check advances its fence before saving;
//! an unchanged record is not rewritten. A saved row keeps its original reading and age.

use super::*;
use aterm_types::macos_setup::{Observation, Read, Standing};

/// The record, beside the run's records in the state root.
pub(in crate::macos_doctor) const READ_BACK_RECORD: &str = "setup-read-back.json";

/// The format this build writes; any other is a record this build does not read (format 1 fenced
/// every row by the journal as the record was written, not as each row was read, so it carried
/// readings across a change made between them).
const READ_BACK_FORMAT: u64 = 2;

/// The change journal at one moment: its file identity, size and optional retained history stamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::macos_doctor) struct JournalMark {
    inode: u64,
    len: u64,
    history: Option<aterm_converge::HistoryStamp>,
}

/// The change journal as a row's read or its last successful check started (`None`
/// inside: no journal file then), which fences that reading ([`no_change_since`]): it stands
/// only while the journal holds no change
/// written since that may be a change to a privately read row. A read whose journal could not
/// be read has none, and nothing keeps it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fence(Option<JournalMark>);

impl Fence {
    /// The journal file `inode` at `len` bytes (tests of the crates that carry a fence).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub const fn at(inode: u64, len: u64) -> Self {
        Self(Some(JournalMark {
            inode,
            len,
            history: None,
        }))
    }
}

/// The account's change journal as it stands now, fencing a read that starts now ([`Fence`]);
/// `None` where it cannot be read or its initial history checkpoint cannot be published.
#[must_use]
pub fn journal_fence() -> Option<Fence> {
    let state = aterm_types::dirs::account_state_dir()?;
    let journal = aterm_converge::FileJournal::under(&state);
    journal.prepare_history().ok()?;
    let mark = JournalMark::of(&journal.path()).ok()??;
    mark.history?;
    Some(Fence(Some(mark)))
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
                history: aterm_converge::FileJournal::history_stamp_at(path, |id| {
                    !aterm_machines::init::writes_no_private_row(id)
                }),
            })),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn record(mark: Option<Self>) -> Value {
        mark.map_or(
            Value::Null,
            |mark| json!({ "inode": mark.inode, "len": mark.len, "history": mark.history }),
        )
    }

    fn parse(value: &Value) -> Option<Option<Self>> {
        if value.is_null() {
            return Some(None);
        }
        Some(Some(Self {
            inode: value["inode"].as_u64()?,
            len: value["len"].as_u64()?,
            history: match value.get("history") {
                None | Some(Value::Null) => None,
                Some(history) => Some(aterm_json::from_value(history.clone()).ok()?),
            },
        }))
    }
}

/// Whether the journal at `path` holds no change written since it stood as `then` that may be a
/// change to a privately read row ([`leaves_private_rows`] of every line since).
///
/// A matching retained-history incarnation answers first, independent of rotations. Otherwise,
/// the lines since are those past `then`'s length in the file it names. That file is the
/// journal still (grown since), or the one rotation kept beside it
/// ([`aterm_converge::FileJournal::ROTATED`]), which nothing appends to, and then every line of
/// the journal that took its place counts too. With no file then, they are every line of the
/// journal, which stands for all of it only while no rotation is kept beside it; none now, none
/// then and no rotation is no line at all. A file that is neither (rotated twice, or replaced), one that
/// changed places while it was read, and anything that cannot be read promise nothing.
pub(in crate::macos_doctor) fn no_change_since(then: Option<JournalMark>, path: &Path) -> bool {
    let Ok(now) = JournalMark::of(path) else {
        return false;
    };
    if let (Some(then), Some(now)) = (then, now)
        && let (Some(before), Some(after)) = (then.history, now.history)
        && before.same_history(after)
    {
        return before == after;
    }
    let rotated = path.with_file_name(aterm_converge::FileJournal::ROTATED);
    match (then, now) {
        (None, None) => matches!(JournalMark::of(&rotated), Ok(None)),
        (Some(_), None) => false,
        (None, Some(now)) => {
            matches!(JournalMark::of(&rotated), Ok(None)) && no_change_past(path, now.inode, 0)
        }
        (Some(then), Some(now)) if now.inode == then.inode => {
            no_change_past(path, then.inode, then.len)
        }
        (Some(then), Some(now)) => {
            no_change_past(&rotated, then.inode, then.len) && no_change_past(path, now.inode, 0)
        }
    }
}

/// Advance only over a tail proved to leave the private rows unchanged. Capture the end
/// before checking: bytes appended during the check must remain ahead of the returned fence.
fn advanced_fence(fence: Fence, journal: &Path) -> Option<Fence> {
    let next = JournalMark::of(journal).ok()?;
    no_change_since(fence.0, journal).then_some(Fence(next))
}

/// Whether the file at `path` is the journal file `inode` and every line past its first `from`
/// bytes leaves the privately read rows as they were ([`leaves_private_rows`]). Another file
/// there, one shorter than `from`, or one that cannot be read whole promises nothing.
fn no_change_past(path: &Path, inode: u64, from: u64) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let Ok((bytes, meta)) =
        aterm_uds::atomic_file::read_regular(path, aterm_converge::FileJournal::BOUND)
    else {
        return false;
    };
    if meta.ino() != inode {
        return false;
    }
    let Some(since) = usize::try_from(from)
        .ok()
        .and_then(|from| bytes.get(from..))
    else {
        return false;
    };
    since
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .all(leaves_private_rows)
}

/// Whether the journal line `line` records nothing that may have changed a privately read row:
/// a metadata-only history checkpoint, a failure line (no change was made), or a line of an item that writes no such row
/// ([`aterm_machines::init::writes_no_private_row`]). A change under any other id, a line with
/// no id, and one that is not a JSON object each may have.
fn leaves_private_rows(line: &[u8]) -> bool {
    if aterm_converge::FileJournal::is_history_checkpoint(line) {
        return true;
    }
    let Ok(line) = aterm_json::from_slice::<Value>(line) else {
        return false;
    };
    let Some(id) = line.get("id").and_then(Value::as_str) else {
        return false;
    };
    let failure = line.get("failed").and_then(Value::as_str).is_some()
        && line.get("to").is_none()
        && line.get("from").is_none();
    failure || aterm_machines::init::writes_no_private_row(id)
}

/// Whether `seen` is a row this record keeps: read privately, read back settled, dated.
fn kept(seen: &Observation) -> bool {
    seen.subject.reads_privately() && seen.read_back_settled()
}

/// The record of `rows` under `revision`: those it keeps ([`kept`]) whose fence still holds at
/// the journal at `journal` ([`no_change_since`]), each with its advanced fence.
pub(in crate::macos_doctor) fn record(
    revision: u64,
    rows: &[(Observation, Fence)],
    journal: &Path,
) -> Value {
    let rows: Vec<Value> = rows
        .iter()
        .filter(|(seen, _)| kept(seen))
        .filter_map(|(seen, fence)| {
            let fence = advanced_fence(*fence, journal)?;
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
/// journal at `journal` holds no change since that row's read started that may be to such a
/// row ([`no_change_since`]); none otherwise, and none of a row this build does not know or
/// would not keep. Each returned fence starts at the end captured before its check.
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
        let Some(fence) = entry.get("journal").and_then(JournalMark::parse).map(Fence) else {
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
        // Discard unusable or duplicate cache rows before taking the history reader's lock.
        if !kept(&seen) || rows.iter().any(|(row, _)| row.subject == seen.subject) {
            continue;
        }
        let Some(fence) = advanced_fence(fence, journal) else {
            continue;
        };
        rows.push((seen, fence));
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
/// holds now (no change journaled since that read started, by any aterm process, that may be
/// to such a row: [`no_change_since`]), under the current run revision. A successful write
/// returns the advanced fences in `rows`; an unchanged record is not rewritten. Each subject
/// may occur at most once: an ambiguous snapshot is refused.
///
/// # Errors
/// The input repeats a subject, the record cannot be written, or the revision cannot be read.
/// An error leaves the caller's fences unchanged.
pub fn remember_private_read_back(rows: &mut [(Observation, Fence)]) -> Result<(), String> {
    let (path, journal) = places()?;
    let revision = initial_setup_revision()?;
    remember_in(&path, &journal, revision, rows)
}

fn remember_in(
    path: &Path,
    journal: &Path,
    revision: u64,
    rows: &mut [(Observation, Fence)],
) -> Result<(), String> {
    let mut subjects = std::collections::HashSet::with_capacity(rows.len());
    for (seen, _) in rows.iter() {
        let subject = seen.subject.setup_word();
        if !subjects.insert(subject) {
            return Err(format!(
                "private read-back snapshot repeats subject {subject}"
            ));
        }
    }
    let value = record(revision, rows, journal);
    // A pass with no new journal bytes or readings has nothing to publish.
    let previous = crate::macos_doctor::read_record_bytes(path)
        .ok()
        .and_then(|bytes| aterm_json::from_slice::<Value>(&bytes).ok());
    if previous.as_ref() != Some(&value) {
        write_json(path, &value)?;
    }
    // Only hand back a fence the durable record holds. A failed write leaves the caller's
    // original fences intact, and a row omitted because its fence failed never advances.
    for (seen, fence) in rows {
        if let Some(entry) = value["rows"].as_array().and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry["row"].as_str() == Some(seen.subject.setup_word()))
        }) && let Some(mark) = JournalMark::parse(&entry["journal"])
        {
            *fence = Fence(mark);
        }
    }
    Ok(())
}

/// The private rows' last read-back a previous launch kept ([`remember_private_read_back`]),
/// each with its fence, where it still holds: written under the current run revision, with no
/// change journaled since that row's read started that may be to such a row
/// ([`no_change_since`]). None otherwise.
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
