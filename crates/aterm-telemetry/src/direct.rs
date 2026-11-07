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
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::{Fig, Kind, Meter, Outcome, Stop};

    fn host() -> HostFacts {
        HostFacts {
            host: "m14".to_owned(),
            model: "Mac15,14".to_owned(),
            cpu: "Apple M3 Ultra".to_owned(),
            cores: 32,
            mem_mib: 98_304,
            os: "macOS 27.0.1".to_owned(),
            aterm: "0.155.0+gtest".to_owned(),
        }
    }

    fn row(at_ms: u64) -> Row {
        Row {
            at_ms,
            kind: Kind::Build,
            key: None,
            dir: None,
            label: "targo".to_owned(),
            sid: None,
            on: None,
            host: "m14".to_owned(),
            facts: host().digest(),
            wall_ms: Fig::Known(1),
            cpu_ms: Fig::Na,
            peak_bytes: Fig::Na,
            wait_ms: Fig::Known(1),
            cache: Fig::Na,
            outcome: Outcome::Ok,
            stop: Stop::Exit,
            code: Some(0),
            sig: None,
            meter: Meter::Exact,
        }
    }

    #[test]
    fn a_held_lock_costs_one_row_that_the_next_gap_line_says() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("telemetry");
        std::fs::create_dir(&dir).expect("dir");
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(".lock"))
            .expect("lock file");
        aterm_uds::file_lock::lock_exclusive_until(
            &held,
            Instant::now() + Duration::from_secs(60),
            "the test holds the telemetry lock",
        )
        .expect("hold the lock");
        let lost = Lost::new();
        let one = |at| write(dir.clone(), &host(), &row(at), &lost);
        assert!(!one(1), "a row written past another writer's lock");
        assert_eq!(lost.lost(), 1);
        drop(held);
        assert!(one(2), "the row once the lock is free");
        let read = crate::log::read(&dir, 4, None).expect("read");
        assert_eq!(
            read.rows.iter().map(|r| r.at_ms).collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(read.dropped, 1, "the lost row is said in a gap line");
        assert!(one(3));
        let again = crate::log::read(&dir, 4, None).expect("read");
        assert_eq!(again.dropped, 1, "a lost row is said once");
    }

    /// A row its writer keeps ([`write_kept_with`]) is not counted when the lock refuses it:
    /// written once the lock is free, it is in the log and no gap line says it lost; rows its
    /// writer gives up ([`Lost::lose`]) are said once, as many as it gave up.
    /// NEGATIVE CONTROL: the same refusal through [`write`] is said lost.
    #[test]
    fn a_kept_row_is_said_lost_only_once_its_writer_gives_it_up() {
        let (_tmp, dir, held) = held_log();
        let lost = Lost::new();
        let kept = |at| write_kept_with(dir.clone(), &host(), &[row(at).line()], &lost);
        assert!(!kept(1), "refused by the held lock");
        assert_eq!(lost.lost(), 0, "a kept row is not counted");
        assert!(
            !write(dir.clone(), &host(), &row(2), &Lost::new()),
            "NEGATIVE CONTROL: refused too"
        );
        drop(held);
        assert!(kept(1), "written once the lock is free");
        let read = crate::log::read(&dir, 4, None).expect("read");
        assert_eq!(
            read.rows.iter().map(|r| r.at_ms).collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(
            read.dropped, 1,
            "only the control's row, which `write` counted"
        );
        lost.lose(&dir, 2);
        assert_eq!(lost.lost(), 2);
        assert!(kept(3));
        assert_eq!(
            crate::log::read(&dir, 4, None).expect("read").dropped,
            3,
            "the two given up, said once"
        );
        assert!(kept(4));
        assert_eq!(crate::log::read(&dir, 4, None).expect("read").dropped, 3);
    }

    /// The log in a scratch folder, its lock held by the test until the guard drops.
    fn held_log() -> (aterm_tempfile::TempDir, PathBuf, std::fs::File) {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("telemetry");
        std::fs::create_dir(&dir).expect("dir");
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(".lock"))
            .expect("lock file");
        aterm_uds::file_lock::lock_exclusive_until(
            &held,
            Instant::now() + Duration::from_secs(60),
            "the test holds the telemetry lock",
        )
        .expect("hold the lock");
        (tmp, dir, held)
    }

    /// A process that writes one row and exits (a build's holder) has no next row to say its
    /// loss in: the folder's loss record says it, at the next append of any writer.
    /// NEGATIVE CONTROL: the record is said once, and an append with none says no gap.
    #[test]
    fn a_row_lost_by_a_process_that_writes_no_other_is_said_by_the_next_writer() {
        let (_tmp, dir, held) = held_log();
        let holder = Lost::new();
        let lost_twice = Lost::new();
        assert!(!write(dir.clone(), &host(), &row(1), &holder));
        assert!(!write(dir.clone(), &host(), &row(2), &lost_twice));
        assert!(!write(dir.clone(), &host(), &row(3), &lost_twice));
        drop(held);
        let window = crate::Log::new(dir.clone(), crate::Limits::DEFAULT, host());
        window
            .append(&[row(4).line()])
            .expect("the window's append");
        let read = crate::log::read(&dir, 4, None).expect("read");
        assert_eq!(
            read.rows.iter().map(|r| r.at_ms).collect::<Vec<_>>(),
            vec![4]
        );
        assert_eq!(
            read.dropped, 3,
            "each writer's loss, said by another's append"
        );
        assert!(write(dir.clone(), &host(), &row(5), &Lost::new()));
        let again = crate::log::read(&dir, 4, None).expect("read");
        assert_eq!(again.dropped, 3, "a loss is said once");
        assert!(
            std::fs::read_dir(&dir)
                .expect("dir")
                .flatten()
                .all(|e| !e.file_name().to_string_lossy().starts_with("lost-")),
            "no record is left"
        );
    }

    /// Where no loss record can be left (the log's folder cannot be made), the process's next
    /// row says its losses: each once, a failed write's claim given back to the next.
    #[test]
    fn a_loss_no_record_holds_is_said_once_by_the_processes_next_row() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let blocked = tmp.path().join("not-a-folder");
        std::fs::write(&blocked, b"").expect("a file where the folder would be");
        let bad = blocked.join("telemetry");
        let good = tmp.path().join("telemetry");
        let lost = Lost::new();
        assert!(!write(bad.clone(), &host(), &row(1), &lost));
        assert_eq!(lost.unmarked.load(Ordering::Acquire), 1, "no record left");
        assert!(!write(bad, &host(), &row(2), &lost), "a claim that failed");
        assert!(write(good.clone(), &host(), &row(3), &lost));
        let read = crate::log::read(&good, 4, None).expect("read");
        assert_eq!(read.dropped, 2, "both losses, said once");
        assert!(write(good.clone(), &host(), &row(4), &lost));
        let again = crate::log::read(&good, 4, None).expect("read");
        assert_eq!(
            again.rows.iter().map(|r| r.at_ms).collect::<Vec<_>>(),
            vec![3, 4]
        );
        assert_eq!(again.dropped, 2, "a loss is said once");
    }

    /// A row that fails after its gap line was written (its append would rotate the log, and the
    /// rotation fails) never has the line's rows given back: the next write says no second gap,
    /// only the row it lost.
    #[test]
    fn a_row_that_fails_after_its_gap_line_never_has_the_line_said_twice() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("telemetry");
        std::fs::create_dir(&dir).expect("dir");
        let nearly_full = std::fs::File::create(crate::log::file_path(&dir, 0)).expect("rows.log");
        nearly_full
            .set_len(crate::Limits::DEFAULT.file_bytes - 120)
            .expect("nearly full");
        drop(nearly_full);
        let blocks_rotation = crate::log::file_path(&dir, crate::Limits::DEFAULT.files - 1);
        std::fs::create_dir(&blocks_rotation).expect("a folder where the oldest file goes");
        let lost = Lost::new();
        lost.unmarked.store(2, Ordering::Release);
        assert!(
            !write(dir.clone(), &host(), &row(1), &lost),
            "the rotation fails"
        );
        std::fs::remove_dir(&blocks_rotation).expect("unblocked");
        assert!(write(dir.clone(), &host(), &row(2), &lost));
        let read = crate::log::read(&dir, crate::Limits::DEFAULT.files, None).expect("read");
        assert_eq!(
            read.rows.iter().map(|r| r.at_ms).collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(read.dropped, 3, "the two said once, and the row lost");
    }

    /// An append whose gap line ended a torn line says so, as one write of both did.
    #[test]
    fn an_append_says_it_mended_a_torn_line_its_gap_line_ended() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("telemetry");
        let log = crate::Log::new(dir.clone(), crate::Limits::DEFAULT, host());
        log.append(&[row(1).line()]).expect("first");
        let mut torn = std::fs::OpenOptions::new()
            .append(true)
            .open(crate::log::file_path(&dir, 0))
            .expect("rows.log");
        std::io::Write::write_all(&mut torn, b"schema=1 at=2 kind=bu").expect("a cut line");
        crate::log::mark_lost(&dir, "test", None, 1).expect("a loss record");
        let done = log.append(&[row(3).line()]).expect("appended");
        assert!(done.mended, "{done:?}");
        let read = crate::log::read(&dir, 4, None).expect("read");
        assert_eq!((read.dropped, read.torn), (1, 1));
    }

    /// One write says its own losses and the folder's loss records in one gap line, under one
    /// wait for the lock.
    #[test]
    fn one_write_says_its_own_losses_and_the_folders_in_one_gap_line() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("telemetry");
        std::fs::create_dir(&dir).expect("dir");
        crate::log::mark_lost(&dir, "other", None, 1).expect("a loss record");
        let lost = Lost::new();
        lost.unmarked.store(2, Ordering::Release);
        let waited = crate::log::locks_waited();
        assert!(write(dir.clone(), &host(), &row(1), &lost));
        assert_eq!(
            crate::log::locks_waited() - waited,
            1,
            "one wait for the lock"
        );
        let text = std::fs::read_to_string(crate::log::file_path(&dir, 0)).expect("rows.log");
        let gaps: Vec<&str> = text.lines().filter(|l| l.contains(" kind=gap ")).collect();
        assert_eq!(gaps.len(), 1, "{text}");
        assert!(gaps[0].contains(" dropped=3 "), "{text}");
    }

    /// An append whose gap line rotated the log says so, though its row then fit the new file.
    #[test]
    fn an_append_says_it_rotated_where_its_gap_line_did() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("telemetry");
        std::fs::create_dir(&dir).expect("dir");
        let full = std::fs::File::create(crate::log::file_path(&dir, 0)).expect("rows.log");
        full.set_len(crate::Limits::DEFAULT.file_bytes - 10)
            .expect("all but full");
        drop(full);
        crate::log::mark_lost(&dir, "test", None, 1).expect("a loss record");
        let log = crate::Log::new(dir.clone(), crate::Limits::DEFAULT, host());
        let done = log.append(&[row(1).line()]).expect("appended");
        assert!(done.rotated, "{done:?}");
        let read = crate::log::read(&dir, crate::Limits::DEFAULT.files, None).expect("read");
        assert_eq!(
            read.rows.iter().map(|r| r.at_ms).collect::<Vec<_>>(),
            vec![1]
        );
    }

    /// A loss no record holds is claimed by one write's gap line: a second write that starts
    /// before the first is done says none of it, and a failed write gives its claim back.
    #[test]
    fn two_writes_of_one_process_never_both_say_one_loss() {
        let lost = Lost::new();
        lost.unmarked.store(2, Ordering::Release);
        let first = lost.claim();
        assert_eq!(first, Some((0, 2)));
        assert_eq!(lost.claim(), None, "the first write holds it");
        lost.unclaim(first.expect("claimed"));
        assert_eq!(lost.claim(), Some((0, 2)), "a failed write gave it back");
    }

    #[test]
    fn a_folder_is_itself_by_any_path_and_no_other() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let a = tmp.path().join("a");
        std::fs::create_dir(&a).expect("a");
        let b = tmp.path().join("b");
        std::fs::create_dir(&b).expect("b");
        assert!(same_folder(&a, &a));
        assert!(same_folder(&a, &tmp.path().join("b/../a")), "resolved");
        assert!(!same_folder(&a, &b));
        assert!(!same_folder(&a, &tmp.path().join("gone")), "unresolvable");
    }
}
