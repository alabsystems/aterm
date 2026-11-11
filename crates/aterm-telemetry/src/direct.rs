// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ONE DIRECT WRITER: a row a process writes the moment it knows it, from a process that has
//! no [`crate::Sink`] (a build's front or holder, the capacity ledger's claimant, `aterm ctl`).
//!
//! The write never holds its caller up for long: the host's facts are read once a process
//! ([`facts`]), and the append waits at most [`LOCK_WAIT`] for another writer's lock on the log.
//! A row that misses it is lost and counted ([`Lost`]) in a loss record the folder keeps
//! ([`crate::log::mark_lost`]), which the next append of any writer says in a gap line: a
//! process that writes one row and exits (a build's holder) has no next row of its own. Where
//! no record could be left, the process's next row goes after a gap line that says it, as the
//! window's writer does. Telemetry never fails the action it reports.
//!
//! A writer that keeps a row it could not write, to write it again later (the fleet board's
//! task rows), writes with [`write_kept`] instead: a row it did not write is not counted, so a
//! row written late is never also said lost. It counts the row only once it gives it up
//! ([`give_up`]), exactly as [`write_now`] counts a row it could not write.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::{Digest, HostFacts, Row};

/// How long a direct write waits for another writer's lock on the log.
pub const LOCK_WAIT: Duration = Duration::from_millis(50);

/// This process's host facts and their digest, read at its first direct row.
static FACTS: OnceLock<(HostFacts, Digest)> = OnceLock::new();

/// The rows this process's direct writes lost.
static LOST: Lost = Lost::new();

/// Rows a process lost (a missed lock, a failed write): the loss record it keeps in a log's
/// folder, and those it could leave none of, with how many of those a gap line said.
#[derive(Debug)]
pub struct Lost {
    lost: AtomicU64,
    unmarked: AtomicU64,
    told: AtomicU64,
    marked: Mutex<Option<Marked>>,
}

/// The loss record a [`Lost`] last left ([`crate::log::mark_lost`]).
#[derive(Debug)]
struct Marked {
    dir: PathBuf,
    writer: String,
    rows: u64,
}

impl Lost {
    /// None lost.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            lost: AtomicU64::new(0),
            unmarked: AtomicU64::new(0),
            told: AtomicU64::new(0),
            marked: Mutex::new(None),
        }
    }

    /// How many rows were lost.
    #[must_use]
    pub fn lost(&self) -> u64 {
        self.lost.load(Ordering::Acquire)
    }

    /// The rows no loss record holds that no gap line has said, claimed for one write's gap line
    /// (`(told, had)`: it says `had - told`): two threads of one process never both say one loss.
    fn claim(&self) -> Option<(u64, u64)> {
        let had = self.unmarked.load(Ordering::Acquire);
        let told = self.told.load(Ordering::Acquire);
        (had > told
            && self
                .told
                .compare_exchange(told, had, Ordering::AcqRel, Ordering::Acquire)
                .is_ok())
        .then_some((told, had))
    }

    /// A [`Self::claim`] whose write failed, given back for the next where no other write has
    /// claimed since (where one has, its rows go unsaid: missing, never said twice).
    fn unclaim(&self, (told, had): (u64, u64)) {
        let _ = self
            .told
            .compare_exchange(had, told, Ordering::AcqRel, Ordering::Acquire);
    }

    /// Count `rows` rows lost from the log in `dir`: in the folder's loss record where one can be
    /// left (its own renamed to the new count, or a new one), else for this process's next row.
    pub fn lose(&self, dir: &Path, rows: u64) {
        if rows == 0 {
            return;
        }
        self.lost.fetch_add(rows, Ordering::AcqRel);
        let mut marked = self
            .marked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (writer, was) = match marked.take() {
            Some(m) if m.dir == dir => (m.writer, Some(m.rows)),
            _ => (crate::log::fresh_writer("direct"), None),
        };
        match crate::log::mark_lost(dir, &writer, was, rows) {
            Ok(rows) => {
                *marked = Some(Marked {
                    dir: dir.to_owned(),
                    writer,
                    rows,
                });
            }
            Err(_) => {
                self.unmarked.fetch_add(rows, Ordering::AcqRel);
            }
        }
    }
}

impl Default for Lost {
    fn default() -> Self {
        Self::new()
    }
}

/// This process's host facts and their digest ([`HostFacts::read`] once a process).
#[must_use]
pub fn facts() -> &'static (HostFacts, Digest) {
    FACTS.get_or_init(|| {
        let facts = HostFacts::read();
        let digest = facts.digest();
        (facts, digest)
    })
}

/// Append `row` to the log in `dir` with this process's [`facts`] and losses ([`write`]).
pub fn write_now(dir: PathBuf, row: &Row) -> bool {
    write(dir, &facts().0, row, &LOST)
}

/// Append `lines` (rows' lines, [`Row::line`]) to the log in `dir` in one write, with this
/// process's [`facts`] and losses ([`write_kept_with`]): whether they were written. Lines it did
/// not write are not counted lost: the caller keeps them, to write again or to give up
/// ([`give_up`]).
pub fn write_kept(dir: PathBuf, lines: &[String]) -> bool {
    write_kept_with(dir, &facts().0, lines, &LOST)
}

/// Count `rows` rows a writer kept ([`write_kept`]) and gave up on as lost from the log in `dir`,
/// in this process's losses, as [`write_now`] counts a row it could not write.
pub fn give_up(dir: &Path, rows: u64) {
    LOST.lose(dir, rows);
}

/// Append `row` to the log in `dir` within [`LOCK_WAIT`], after a gap line for the rows `lost`
/// holds that no line has said yet: `false` (and the row counted in `lost`) when it was not
/// written.
pub fn write(dir: PathBuf, facts: &HostFacts, row: &Row, lost: &Lost) -> bool {
    match append(dir, facts, &[row.line()], lost) {
        Ok(()) => true,
        Err(dir) => {
            lost.lose(&dir, 1);
            false
        }
    }
}

/// Append `lines` to the log in `dir` in one write within [`LOCK_WAIT`], after a gap line for
/// the rows `lost` holds that no line has said yet: whether they were written. A line it did not
/// write is not counted in `lost`: its caller keeps it, and counts it once it gives it up
/// ([`Lost::lose`]), so a row written late is never also said lost.
pub fn write_kept_with(dir: PathBuf, facts: &HostFacts, lines: &[String], lost: &Lost) -> bool {
    append(dir, facts, lines, lost).is_ok()
}

/// Append `lines` to the log in `dir` under one wait of at most [`LOCK_WAIT`] for its lock,
/// after a gap line for the rows `lost` holds that no line has said: `Err` with the log's folder
/// where they were not written, which counts nothing of them.
fn append(dir: PathBuf, facts: &HostFacts, lines: &[String], lost: &Lost) -> Result<(), PathBuf> {
    let log = crate::Log::new(dir, crate::Limits::DEFAULT, facts.clone()).waiting(LOCK_WAIT);
    // One wait for the lock; the gap line is a write of its own, so a row that fails after it
    // never has the line's rows given back and said twice.
    let claim = lost.claim();
    let said = claim.map_or(0, |(told, had)| had - told);
    match log.append_saying(said, lines) {
        Ok(_) => Ok(()),
        Err((_, gap_written)) => {
            if let Some(claim) = claim
                && !gap_written
            {
                lost.unclaim(claim);
            }
            Err(log.dir().to_owned())
        }
    }
}

/// Whether `a` and `b` name one folder: the same path, or the same once both resolve. A writer
/// whose rows belong to one folder alone (this user's capacity ledger) holds its action's folder
/// to it, so a scratch folder (a test's) writes no row.
#[must_use]
pub fn same_folder(a: &Path, b: &Path) -> bool {
    a == b
        || std::fs::canonicalize(a)
            .ok()
            .zip(std::fs::canonicalize(b).ok())
            .is_some_and(|(a, b)| a == b)
}

#[cfg(test)]
#[path = "direct_tests.rs"]
mod tests;
