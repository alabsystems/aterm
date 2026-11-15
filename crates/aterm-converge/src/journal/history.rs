// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A bounded latest-change index. Its journal identity binds every snapshot to bytes the
//! writer flushed. An older writer, a torn publication or a replaced journal breaks that
//! binding. The next writer starts a new random incarnation, invalidating old witnesses.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

pub(super) const FILE: &str = "change-heads.json";
const BOUND: u64 = 4 << 20;

/// The newest selected change in one incarnation of the journal's retained history.
/// Unselected changes leave this stamp equal, even across arbitrarily many rotations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryStamp {
    incarnation: [u8; 16],
    latest: u64,
}

impl HistoryStamp {
    /// Whether both stamps refer to the same uninterrupted history.
    #[must_use]
    pub fn same_history(self, other: Self) -> bool {
        self.incarnation == other.incarnation
    }
}

#[derive(Serialize, Deserialize)]
pub(super) struct History {
    format: u8,
    incarnation: [u8; 16],
    sequence: u64,
    device: u64,
    inode: u64,
    len: u64,
    last_nonce: String,
    changes: BTreeMap<String, u64>,
}

impl History {
    pub(super) fn read(journal: &Path) -> Option<Self> {
        let path = journal.with_file_name(FILE);
        let (bytes, _) = aterm_uds::atomic_file::read_regular(&path, BOUND).ok()?;
        let history: Self = aterm_json::from_slice(&bytes).ok()?;
        let (journal_bytes, meta) =
            aterm_uds::atomic_file::read_regular(journal, super::FileJournal::BOUND).ok()?;
        (history.format == 1
            && last_nonce(&journal_bytes).as_deref() == Some(history.last_nonce.as_str())
            && meta.is_file()
            && (meta.dev(), meta.ino(), meta.len()) == (history.device, history.inode, history.len)
            && history
                .changes
                .values()
                .all(|seq| *seq > 0 && *seq <= history.sequence))
        .then_some(history)
    }

    pub(super) fn begin(journal: &Path) -> io::Result<Self> {
        if let Some(history) = Self::read(journal) {
            return Ok(history);
        }
        let mut incarnation = [0; 16];
        aterm_uds::rand::fill(&mut incarnation)?;
        Ok(Self {
            format: 1,
            incarnation,
            sequence: 0,
            device: 0,
            inode: 0,
            len: 0,
            last_nonce: String::new(),
            changes: BTreeMap::new(),
        })
    }

    pub(super) fn note(&mut self, id: &str) -> io::Result<()> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| io::Error::other("journal history sequence exhausted"))?;
        self.changes.insert(id.to_owned(), self.sequence);
        Ok(())
    }

    pub(super) fn publish(mut self, journal: &Path) -> io::Result<()> {
        let (journal_bytes, meta) =
            aterm_uds::atomic_file::read_regular(journal, super::FileJournal::BOUND)?;
        self.last_nonce = last_nonce(&journal_bytes)
            .ok_or_else(|| io::Error::other("journal has no final nonce"))?;
        self.device = meta.dev();
        self.inode = meta.ino();
        self.len = meta.len();
        let bytes = aterm_json::to_vec(&self).map_err(io::Error::other)?;
        if bytes.len() as u64 > BOUND {
            return Err(io::Error::other("journal history exceeds its 4 MiB bound"));
        }
        aterm_uds::atomic_file::write(&journal.with_file_name(FILE), &bytes, 0o600)
    }

    pub(super) fn stamp(&self, includes: impl Fn(&str) -> bool) -> HistoryStamp {
        HistoryStamp {
            incarnation: self.incarnation,
            latest: self
                .changes
                .iter()
                .filter(|(id, _)| includes(id))
                .map(|(_, seq)| *seq)
                .max()
                .unwrap_or(0),
        }
    }
}

/// Only the exact metadata-only checkpoint shape can bypass a change reader. Extra fields
/// (especially an item id or from/to values) cannot be hidden behind the checkpoint marker.
pub(super) fn is_checkpoint(line: &[u8]) -> bool {
    let Ok(value) = aterm_json::from_slice::<aterm_json::Value>(line) else {
        return false;
    };
    value.as_object().is_some_and(|object| object.len() == 3)
        && value["checkpoint"].as_bool() == Some(true)
        && value["time"].as_u64().is_some()
        && value["nonce"].as_str().is_some_and(|nonce| {
            nonce.len() == 32 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

// Each new line carries fresh randomness. Matching it as well as inode and length keeps
// a stale index from binding to a new file whose filesystem reused an old inode.
fn last_nonce(bytes: &[u8]) -> Option<String> {
    let line = bytes
        .split(|byte| *byte == b'\n')
        .rfind(|line| !line.is_empty())?;
    let value: aterm_json::Value = aterm_json::from_slice(line).ok()?;
    let nonce = value.get("nonce")?.as_str()?;
    (nonce.len() == 32 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| nonce.to_owned())
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
