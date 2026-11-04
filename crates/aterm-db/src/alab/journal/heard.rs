// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The heard archive (design §7, §8): every `ev` batch this Mac read from the ring, kept on this
//! Mac for 120 days or 64 MiB, so an account's login and logout log reaches back past the ring's
//! two weeks, labelled "heard by this Mac". One file a UTC day, `<spool>/heard/<day>.jsonl`, one
//! batch a line: `{m, n, p, e}` (the writer's Commons machine uuid, its batch counter, the week of
//! the table it was read from, the events). Local only: nothing here is ever written to Commons.

use super::event::Event;
use aterm_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::{
    io::{self, Write as _},
    path::Path,
};

/// The archive's folder in the spool folder.
pub(crate) const HEARD: &str = "heard";
/// Days a day's file is kept.
pub(crate) const HEARD_DAYS: u64 = 120;
/// The most the archive holds; the oldest days go first.
pub(crate) const HEARD_MAX: u64 = 64 * 1024 * 1024;
const DAY: u64 = 86_400;

/// One batch read from the ring: its writer machine (32 hex), counter, week, and events.
pub(crate) type Batch = (String, u64, u64, Vec<Event>);

/// Append `batches` to today's file, then drop days past [`HEARD_DAYS`] and the oldest past
/// [`HEARD_MAX`].
///
/// # Errors
/// The folder cannot be made private, or a file cannot be written, listed or removed.
pub(crate) fn archive(spool: &Path, now: u64, batches: &[Batch]) -> io::Result<()> {
    let dir = spool.join(HEARD);
    aterm_uds::private_file::ensure_private_dir(&dir)?;
    if !batches.is_empty() {
        let mut text = Vec::new();
        for (m, n, p, events) in batches {
            let e: Vec<Value> = events.iter().map(Event::to_value).collect();
            let line = aterm_json::to_vec(&json!({"m": m, "n": n, "p": p, "e": e}))
                .map_err(|_| io::Error::other("a heard batch could not be encoded"))?;
            text.extend_from_slice(&line);
            text.push(b'\n');
        }
        let (mut file, _) =
            super::spool::open_private_append(&dir.join(format!("{}.jsonl", now / DAY)))?;
        file.write_all(&text)?;
    }
    prune(&dir, now)
}

/// The batches the archive already holds from `since` (Unix seconds) on, by writer machine: each
/// line's `(m, n)` in the day files from `since`'s day. A batch first read from a week's table is
/// archived on a day of that week or later, so `since` at the window's first week finds every
/// batch the window still holds, and a book read as lost (whose own list is empty) archives none
/// of them twice. A line that does not read is skipped.
///
/// # Errors
/// The archive's folder exists and cannot be listed, or a day file cannot be read.
pub(crate) fn archived(spool: &Path, since: u64) -> io::Result<BTreeMap<String, BTreeSet<u64>>> {
    let dir = spool.join(HEARD);
    let mut held: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(held),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else { continue };
        if canonical_day(name).is_none_or(|day| day < since / DAY) {
            continue;
        }
        let bytes = match aterm_uds::private_file::read_private(
            &dir,
            name,
            usize::try_from(HEARD_MAX).unwrap_or(usize::MAX),
        ) {
            Ok(bytes) => bytes,
            Err(aterm_uds::private_file::PrivateFileError::Missing) => continue,
            Err(aterm_uds::private_file::PrivateFileError::Io(error)) => return Err(error),
            Err(other) => return Err(io::Error::other(other.to_string())),
        };
        for line in bytes.split(|b| *b == b'\n') {
            let Ok(value) = aterm_json::from_slice::<Value>(line) else {
                continue;
            };
            if let (Some(m), Some(n)) = (
                value.get("m").and_then(Value::as_str),
                value.get("n").and_then(Value::as_u64),
            ) {
                held.entry(m.to_owned()).or_default().insert(n);
            }
        }
    }
    Ok(held)
}

/// Every batch the archive holds from `since`'s day on, each whose events all decode (a line that
/// does not read, or holds an event the vocabulary refuses, is skipped whole), oldest day first:
/// the log's "heard by this Mac" part (design §7).
///
/// # Errors
/// The archive's folder exists and cannot be listed, or a day file cannot be read.
pub(crate) fn batches(spool: &Path, since: u64) -> io::Result<Vec<Batch>> {
    let dir = spool.join(HEARD);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut days: Vec<(u64, String)> = Vec::new();
    for entry in entries {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some(day) = canonical_day(name).filter(|day| *day >= since / DAY) {
            days.push((day, name.to_owned()));
        }
    }
    days.sort();
    let mut out = Vec::new();
    for (_, name) in days {
        let bytes = match aterm_uds::private_file::read_private(
            &dir,
            &name,
            usize::try_from(HEARD_MAX).unwrap_or(usize::MAX),
        ) {
            Ok(bytes) => bytes,
            Err(aterm_uds::private_file::PrivateFileError::Missing) => continue,
            Err(aterm_uds::private_file::PrivateFileError::Io(error)) => return Err(error),
            Err(other) => return Err(io::Error::other(other.to_string())),
        };
        for line in bytes.split(|b| *b == b'\n') {
            let Ok(value) = aterm_json::from_slice::<Value>(line) else {
                continue;
            };
            let read = (|| {
                let m = value.get("m")?.as_str()?;
                aterm_codec::hex::is_lower(m, 32).then_some(())?;
                let events = value
                    .get("e")?
                    .as_array()?
                    .iter()
                    .map(Event::from_value)
                    .collect::<Option<Vec<Event>>>()?;
                Some((
                    m.to_owned(),
                    value.get("n")?.as_u64()?,
                    value.get("p")?.as_u64()?,
                    events,
                ))
            })();
            out.extend(read);
        }
    }
    Ok(out)
}

fn canonical_day(name: &str) -> Option<u64> {
    let digits = name.strip_suffix(".jsonl")?;
    if digits.is_empty()
        || (digits.len() > 1 && digits.starts_with('0'))
        || !digits.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    digits.parse::<u64>().ok()
}

/// Remove days older than [`HEARD_DAYS`], then the oldest while the archive passes
/// [`HEARD_MAX`].
fn prune(dir: &Path, now: u64) -> io::Result<()> {
    let mut days: Vec<(u64, String, u64)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        let Some(day) = canonical_day(name_str) else {
            continue;
        };
        let meta = match std::fs::symlink_metadata(entry.path()) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if !meta.is_file() {
            continue;
        }
        days.push((day, name_str.to_owned(), meta.len()));
    }
    days.sort_unstable();
    let today = now / DAY;
    let mut total: u64 = days.iter().map(|(_, _, len)| len).sum();
    for (day, file_name, len) in days {
        if day.saturating_add(HEARD_DAYS) >= today && total <= HEARD_MAX {
            break;
        }
        match std::fs::remove_file(dir.join(file_name)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        total = total.saturating_sub(len);
    }
    Ok(())
}
