// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    let kept =
        |at| write_kept_with(dir.clone(), &host(), &[row(at).line()], &lost) == Kept::Written;
    assert_eq!(
        write_kept_with(dir.clone(), &host(), &[row(1).line()], &lost),
        Kept::Refused,
        "refused by the held lock, none of it left"
    );
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

/// AN APPEND ITS LOG COULD NOT CUT BACK IS NOT TAKEN AS REFUSED (TM-1 review): where a
/// failed write's cut back fails too ([`crate::Log::never_cut_back`]), the log may hold what
/// it wrote. Rows so left answer [`Kept::Unsure`], which their writer settles by asking the
/// log, and [`write`] counts no row so left as lost; a gap line so left is taken as said, its
/// records not left again nor the caller's own unrecorded rows it said given back, and the
/// rows after it, never written, are refused. Every loss is said once. NEGATIVE CONTROL: the
/// same writes, cut back, answer [`Kept::Refused`], count the row lost, and leave the gap's
/// records and the caller's rows to say again.
#[test]
fn an_append_its_log_could_not_cut_back_is_unsure_and_its_gap_said_once() {
    use std::sync::{Arc, atomic::AtomicU64};
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let lines = [row(2).line(), row(3).line()];
    for uncut in [false, true] {
        let dir = tmp.path().join(format!("telemetry-{uncut}"));
        let log = |room: usize| {
            let log = crate::Log::new(dir.clone(), crate::Limits::DEFAULT, host())
                .waiting(LOCK_WAIT)
                .cut_after(Arc::new(AtomicU64::new(room as u64)));
            if uncut { log.never_cut_back() } else { log }
        };
        assert_eq!(
            write_kept_with(dir.clone(), &host(), &[row(1).line()], &Lost::new()),
            Kept::Written
        );
        let lost = Lost::new();
        assert_eq!(
            kept(append_to(&log(lines[0].len()), &lines, &lost)),
            if uncut { Kept::Unsure } else { Kept::Refused },
            "two rows, the disk full after the first"
        );
        let holds = crate::Log::new(dir.clone(), crate::Limits::DEFAULT, host())
            .holds(&lines)
            .expect("read");
        assert_eq!(holds, [uncut, false]);
        let wrote_it = wrote(append_to(&log(4), &[row(4).line()], &lost), &lost);
        assert!(!wrote_it);
        assert_eq!(
            lost.lost(),
            u64::from(!uncut),
            "a row the log may hold is not lost"
        );

        let dir = tmp.path().join(format!("gap-{uncut}"));
        assert_eq!(
            write_kept_with(dir.clone(), &host(), &[row(1).line()], &Lost::new()),
            Kept::Written
        );
        crate::log::mark_lost(&dir, "w", None, 3).expect("a loss record");
        // Two rows of the writer's own it could leave no record of, its gap line's to say.
        let lost = Lost::new();
        lost.unmarked.store(2, std::sync::atomic::Ordering::Release);
        let gap = crate::row::gap_line(crate::now_ms(), 5);
        assert!(gap.ends_with('\n'), "{gap:?}");
        let log = |room: usize| {
            let log = crate::Log::new(dir.clone(), crate::Limits::DEFAULT, host())
                .waiting(LOCK_WAIT)
                .cut_after(Arc::new(AtomicU64::new(room as u64)));
            if uncut { log.never_cut_back() } else { log }
        };
        assert_eq!(
            kept(append_to(&log(gap.len() - 1), &[row(5).line()], &lost)),
            Kept::Refused,
            "the gap line cut of its line feed: the row never written"
        );
        assert_eq!(
            write_kept_with(dir.clone(), &host(), &[row(6).line()], &lost),
            Kept::Written
        );
        let read = crate::log::read(&dir, 4, None).expect("read");
        assert_eq!(
            read.dropped, 5,
            "the 3 recorded rows and the writer's 2, said once (uncut: {uncut})"
        );
        assert_eq!(
            read.rows.iter().map(|r| r.at_ms).collect::<Vec<_>>(),
            [1, 6]
        );
    }
}

/// A TORN LINE'S MEND THAT FAILS WRITES NONE OF ITS APPEND (TM-1 review): an append to a log
/// whose last line is torn (a crash's, or a write that could not be cut back) first ends that
/// line, and where that line feed's write fails and cannot be cut back either, none of the
/// rows or the gap line was written: [`write`] counts the row lost, and the gap's records are
/// left again, said once by the next write. NEGATIVE CONTROL (by hand: the mend's error
/// passed on as one that may have left the lines): the row counted nowhere, and the records
/// taken and never said.
#[test]
fn a_torn_lines_mend_that_fails_writes_none_of_its_append() {
    use std::sync::{Arc, atomic::AtomicU64};
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    for gap in [false, true] {
        let dir = tmp.path().join(format!("torn-{gap}"));
        assert_eq!(
            write_kept_with(dir.clone(), &host(), &[row(1).line()], &Lost::new()),
            Kept::Written
        );
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(crate::log::file_path(&dir, 0))
            .expect("the log's file");
        std::io::Write::write_all(&mut file, b"kind=task torn").expect("a torn tail");
        drop(file);
        let log = crate::Log::new(dir.clone(), crate::Limits::DEFAULT, host())
            .waiting(LOCK_WAIT)
            .cut_after(Arc::new(AtomicU64::new(0)))
            .never_cut_back();
        let lost = Lost::new();
        if gap {
            crate::log::mark_lost(&dir, "w", None, 3).expect("a loss record");
            assert_eq!(
                kept(append_to(&log, &[row(5).line()], &lost)),
                Kept::Refused
            );
        } else {
            assert!(!wrote(append_to(&log, &[row(5).line()], &lost), &lost));
            assert_eq!(lost.lost(), 1, "the row, never written, counted lost");
        }
        let holds = crate::Log::new(dir.clone(), crate::Limits::DEFAULT, host())
            .holds(&[row(5).line()])
            .expect("read");
        assert_eq!(holds, [false]);
        assert_eq!(
            write_kept_with(dir.clone(), &host(), &[row(6).line()], &lost),
            Kept::Written
        );
        let read = crate::log::read(&dir, 4, None).expect("read");
        assert_eq!(
            (
                read.dropped,
                read.rows.iter().map(|r| r.at_ms).collect::<Vec<_>>()
            ),
            (if gap { 3 } else { 1 }, vec![1, 6]),
            "each loss said once (gap: {gap})"
        );
    }
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
