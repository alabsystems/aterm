// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::board::{Board, Row as BoardRow};

const NODE: &str = "0123456789abcdef0123456789abcdef";

/// A worker spawned at `started_ms`.
fn worker(sid: &str, started_ms: u64) -> BoardRow {
    BoardRow {
        node: Some(NODE.to_owned()),
        program: Some("claude".to_owned()),
        cwd: Some("/Users//me/secret-project".to_owned()),
        brief: Some(format!("sha256:{}", "ab".repeat(32))),
        ..BoardRow::started(sid, started_ms)
    }
}

/// `worker`'s row closed by its Mac's record `seen`, at 61 s: what its task row says.
fn closed(seen: &str) -> (Outcome, Stop, Option<i32>) {
    let mut row = worker("s-0a1", 1_000);
    row.merge(seen, 61_000);
    let r = task_row(
        &row,
        Some(seen),
        61_000,
        "m101.local",
        Digest::dir("/facts"),
    );
    (r.outcome, r.stop, r.code)
}

#[test]
fn a_task_row_says_its_conclusion_and_why_in_the_rows_words() {
    let close = |rest: &str| format!("@s-0a1 rev=4 state=closed {rest} task_news=1");
    assert_eq!(
        closed(&close("conclusion=finished why=landed")),
        (Outcome::Ok, Stop::Unknown, None)
    );
    assert_eq!(
        closed(&close("conclusion=failed why=exit")),
        (Outcome::Fail, Stop::Exit, None)
    );
    assert_eq!(
        closed(&close("conclusion=failed why=exit-status exit=3")),
        (Outcome::Fail, Stop::Exit, Some(3))
    );
    for stall in ["held", "startup", "shell", "signin", "install", "cancelled"] {
        assert_eq!(
            closed(&close(&format!("conclusion=failed why={stall}"))),
            (Outcome::Fail, Stop::Stalled, None),
            "{stall}"
        );
    }
    assert_eq!(
        closed(&close("conclusion=failed why=could-not-finish")),
        (Outcome::Fail, Stop::Unknown, None),
        "any other word"
    );
    assert_eq!(
        closed(&close("conclusion=later-word")),
        (Outcome::Unknown, Stop::Unknown, None)
    );
    assert_eq!(
        closed(&close("")),
        (Outcome::Fail, Stop::Unknown, None),
        "a closed tab with no conclusion is failed, as the board reads it"
    );

    let mut row = worker("s-0a1", 1_000);
    let seen = close("conclusion=finished");
    row.merge(&seen, 61_000);
    let r = task_row(
        &row,
        Some(&seen),
        61_000,
        "m101.local",
        Digest::dir("/facts"),
    );
    assert_eq!(r.kind, Kind::Task);
    assert_eq!(
        (
            r.sid.as_deref(),
            r.on.as_deref(),
            r.label.as_str(),
            r.host.as_str()
        ),
        (Some("s-0a1"), Some(NODE), "claude", "m101")
    );
    assert_eq!(r.wall_ms, Fig::Known(60_000), "from its spawn");
    assert_eq!((r.wait_ms.clone(), r.cache.clone()), (Fig::Na, Fig::Na));
    let line = r.line();
    assert!(
        !line.contains("secret-project") && !line.contains("abab"),
        "{line}"
    );
}

/// A board in a scratch state folder, its workers started an hour ago (fresh, never pruned):
/// `apply` merges one seen row, `tasks` reads the task rows the state's log holds.
struct Scratch {
    _tmp: aterm_tempfile::TempDir,
    board: Board,
    log: std::path::PathBuf,
    t0: u64,
}

impl Scratch {
    fn new(sids: &[&str]) -> Self {
        let tmp = aterm_tempfile::tempdir().expect("scratch");
        let board = Board::in_state(tmp.path());
        let t0 = crate::board::now_ms() - 3_600_000;
        for sid in sids {
            board.record(&worker(sid, t0)).expect("recorded");
            assert!(board.row(sid).is_some(), "the board holds {sid}");
        }
        let log = aterm_telemetry::log::dir_in(tmp.path());
        Self {
            _tmp: tmp,
            board,
            log,
            t0,
        }
    }

    fn apply(&self, seen: &str, at: u64) {
        self.board
            .apply(&[seen.to_owned()], self.t0 + at)
            .expect("applied");
    }

    fn tasks(&self, sid: &str) -> Vec<Row> {
        aterm_telemetry::log::read(&self.log, 4, None).map_or_else(
            |_| Vec::new(),
            |read| {
                read.rows
                    .into_iter()
                    .filter(|r| r.kind == Kind::Task && r.sid.as_deref() == Some(sid))
                    .collect()
            },
        )
    }

    fn open(&self, sid: &str) -> bool {
        self.board.row(sid).expect("a row").is_open()
    }

    /// The board row of `sid`: how many task rows it owes, and the end it told.
    fn owes(&self, sid: &str) -> (usize, Option<String>) {
        let row = self.board.row(sid).expect("a row");
        (row.owed.len(), row.told)
    }

    /// Hold the log's lock, as another writer's long append does, until the guard drops.
    fn hold_log(&self) -> std::fs::File {
        std::fs::create_dir_all(&self.log).expect("the log's folder");
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.log.join(".lock"))
            .expect("lock file");
        aterm_uds::file_lock::lock_exclusive_until(
            &held,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
            "the test holds the telemetry lock",
        )
        .expect("hold the lock");
        held
    }

    /// The rows the log's gap lines say were lost.
    fn dropped(&self) -> u64 {
        aterm_telemetry::log::read(&self.log, 4, None).map_or(0, |read| read.dropped)
    }

    /// The loss records the log's folder holds, no append has said yet.
    fn unsaid(&self) -> Vec<String> {
        std::fs::read_dir(&self.log).map_or_else(
            |_| Vec::new(),
            |dir| {
                dir.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.starts_with("lost-"))
                    .collect()
            },
        )
    }
}

/// A task row the log's lock refused (another writer held it past
/// [`aterm_telemetry::direct::LOCK_WAIT`]) is owed by its board row, not lost: a later apply
/// that reads the row, or the keeper's settle, writes it once the lock is free, with its own
/// close's time and `wall_ms`, and no gap line says it lost. A reversed end owed so is the
/// worker's last row. One settle writes what two rows owe, and each tells its end.
/// NEGATIVE CONTROL: once written, it is owed no more and written no second time.
#[test]
fn a_task_row_the_logs_lock_refused_is_written_later_as_it_was_and_never_said_lost() {
    let at = Scratch::new(&["s-7a1", "s-7b2", "s-7c3"]);
    let record = "@s-7a1 rev=3 state=closed task_state=exited agent=claude \
                  conclusion=finished why=exit-status exit=0 task_news=1";
    let held = at.hold_log();
    at.apply(record, 61_000);
    at.apply(
        "@s-7c3 rev=2 state=closed task_state=failed agent=claude conclusion=failed \
         why=could-not-finish task_news=1",
        62_000,
    );
    assert!(!at.open("s-7a1"));
    assert_eq!(at.owes("s-7a1"), (1, None), "the row its log refused");
    assert_eq!(at.owes("s-7c3"), (1, None));
    assert!(
        at.unsaid().is_empty(),
        "a row the board keeps is not counted lost"
    );
    drop(held);
    // A later answer of the worker, which moves nothing, writes what its row owes.
    at.apply(record, 70_000);
    let rows = at.tasks("s-7a1");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        (rows[0].at_ms, rows[0].wall_ms.clone()),
        (at.t0 + 61_000, Fig::Known(61_000)),
        "its own close's time and wall_ms"
    );
    assert_eq!(
        (rows[0].outcome, rows[0].stop, rows[0].code),
        (Outcome::Ok, Stop::Exit, Some(0))
    );
    assert_eq!(at.owes("s-7a1"), (0, Some("ok.exit".to_owned())));
    assert_eq!(
        at.dropped(),
        0,
        "a row written late is said lost by no gap line"
    );
    assert!(at.unsaid().is_empty());
    at.apply(record, 80_000);
    assert_eq!(at.tasks("s-7a1").len(), 1, "written once");

    // A close that reversed and came to another end, its write refused: the keeper's settle
    // writes it, the worker's last row, in the one append that writes `s-7c3`'s.
    let done = "@s-7b2 rev=3 state=closed task_state=done agent=claude conclusion=finished \
                why=landed task_news=1";
    at.apply(done, 1_000);
    at.apply(
        "@s-7b2 rev=3 state=done agent=claude conclusion=finished why=landed task_news=1",
        2_000,
    );
    assert!(at.open("s-7b2"), "a row at the close's rev reopens it");
    let held = at.hold_log();
    at.apply(
        "@s-7b2 rev=4 state=closed task_state=stalled agent=claude conclusion=failed \
         why=held task_news=1",
        3_000,
    );
    assert_eq!(at.owes("s-7b2"), (1, Some("ok.unknown".to_owned())));
    drop(held);
    at.board
        .settle(&["s-7b2".to_owned(), "s-7c3".to_owned()], at.t0 + 9_000)
        .expect("settled");
    assert_eq!(
        at.tasks("s-7b2")
            .iter()
            .map(|r| (r.outcome, r.stop, r.at_ms))
            .collect::<Vec<_>>(),
        [
            (Outcome::Ok, Stop::Unknown, at.t0 + 1_000),
            (Outcome::Fail, Stop::Stalled, at.t0 + 3_000)
        ],
        "its last row is the end it came to"
    );
    assert_eq!(at.owes("s-7b2"), (0, Some("fail.stalled".to_owned())));
    assert_eq!(
        at.tasks("s-7c3")
            .iter()
            .map(|r| (r.outcome, r.at_ms))
            .collect::<Vec<_>>(),
        [(Outcome::Fail, at.t0 + 62_000)]
    );
    assert_eq!(at.owes("s-7c3"), (0, Some("fail.unknown".to_owned())));
    assert_eq!(at.dropped(), 0);
}

/// One apply that reads a sid twice (a close, then the row reopened) merges the second answer
/// into what the first left: the row its close owes stays owed, though the row stands open.
/// NEGATIVE CONTROL: the rows are read and written once, so the reopen is kept too.
#[test]
fn a_sid_answered_twice_in_one_apply_keeps_what_its_first_answer_owed() {
    let at = Scratch::new(&["s-9a1"]);
    let held = at.hold_log();
    at.board
        .apply(
            &[
                "@s-9a1 rev=3 state=closed task_state=done agent=claude conclusion=finished \
                 why=landed task_news=1"
                    .to_owned(),
                "@s-9a1 rev=3 state=done agent=claude conclusion=finished why=landed \
                 task_news=1"
                    .to_owned(),
            ],
            at.t0 + 1_000,
        )
        .expect("applied");
    assert!(at.open("s-9a1"), "the second answer reopened it");
    assert_eq!(at.owes("s-9a1"), (1, None), "the first answer's row");
    drop(held);
    at.board
        .settle(&["s-9a1".to_owned()], at.t0 + 2_000)
        .expect("settled");
    assert_eq!(at.tasks("s-9a1").len(), 1);
}

/// A board row file made immutable (`chflags uchg`) until the guard drops: it reads as it
/// did, and a rename over it is refused (`EPERM`), so one row's write fails and no other's.
#[cfg(target_os = "macos")]
struct Immutable(std::path::PathBuf);

#[cfg(target_os = "macos")]
impl Immutable {
    fn set(path: std::path::PathBuf) -> Self {
        let made = std::process::Command::new("/usr/bin/chflags")
            .arg("uchg")
            .arg(&path)
            .status()
            .expect("chflags ran");
        assert!(made.success(), "chflags uchg {}", path.display());
        Self(path)
    }
}

#[cfg(target_os = "macos")]
impl Drop for Immutable {
    fn drop(&mut self) {
        let _ = std::process::Command::new("/usr/bin/chflags")
            .arg("nouchg")
            .arg(&self.0)
            .status();
    }
}

/// One board row's failed write leaves every other row the apply paid written: a row that
/// owed a task row the apply's append wrote no longer owes it, so no later write says it a
/// second time, and no give-up says lost a row the log holds. The apply answers the failure.
/// The row whose write failed keeps its file as it was, so the next apply tells its end
/// again: a second row of one end, never none. NEGATIVE CONTROL: the failed row is the
/// first the apply holds, so stopping at it writes no other.
#[cfg(target_os = "macos")]
#[test]
fn one_board_rows_failed_write_leaves_every_other_row_the_apply_paid_written() {
    let at = Scratch::new(&["s-9b1", "s-9b2"]);
    let close = |sid: &str| {
        format!(
            "@{sid} rev=3 state=closed task_state=exited agent=claude conclusion=finished \
             why=exit-status exit=0 task_news=1"
        )
    };
    let held = at.hold_log();
    at.apply(&close("s-9b2"), 1_000);
    assert_eq!(at.owes("s-9b2"), (1, None), "refused by the log's lock");
    drop(held);
    let fixed = Immutable::set(at.board.dir().join("s-9b1"));
    let answer = at
        .board
        .apply(&[close("s-9b1"), close("s-9b2")], at.t0 + 2_000);
    assert!(answer.is_err(), "the failed write is the answer");
    assert!(at.open("s-9b1"), "its file kept as it was");
    assert_eq!(
        at.owes("s-9b2"),
        (0, Some("ok.exit".to_owned())),
        "the row the append wrote is owed no more"
    );
    drop(fixed);
    at.board
        .settle(&["s-9b2".to_owned()], at.t0 + 3_000)
        .expect("settled");
    at.apply(&close("s-9b1"), 4_000);
    assert!(!at.open("s-9b1"));
    assert_eq!(at.tasks("s-9b2").len(), 1, "written once");
    assert_eq!(at.tasks("s-9b1").len(), 2, "told again, never lost");
    assert_eq!(at.dropped(), 0);
    assert!(at.unsaid().is_empty());
}

/// A task row its board gives up is said lost exactly once: a write that fails
/// [`OWED_KEPT`] after its close (and not one a second before it), its board row pruned or
/// written over, a fifth row owed past [`OWED_MAX`] (the oldest), or a line that holds no
/// task row. NEGATIVE CONTROLS: the failed writes of other rows after each was given up, a
/// later prune and later appends say none of them again; the rows still owed are written, in
/// order.
#[test]
fn a_task_row_its_board_gives_up_is_said_lost_exactly_once() {
    let at = Scratch::new(&["s-8a1", "s-8b2", "s-8c3", "s-8d4", "s-8e5", "s-8f6"]);
    let record = |sid: &str| {
        format!(
            "@{sid} rev=3 state=closed task_state=done agent=claude conclusion=finished \
             why=landed task_news=1"
        )
    };
    let kept = u64::try_from(OWED_KEPT.as_millis()).expect("ms");
    let held = at.hold_log();
    at.apply(&record("s-8a1"), 1_000);
    at.apply(&record("s-8b2"), 1_000);
    at.apply(&record("s-8a1"), kept);
    assert_eq!(at.owes("s-8a1").0, 1, "a write that fails within its time");
    at.apply(&record("s-8a1"), 1_000 + kept);
    assert_eq!(at.owes("s-8a1"), (0, None), "retries exhausted: given up");
    // Its board row written over (a row recorded again for its sid).
    at.apply(&record("s-8e5"), 1_000);
    assert_eq!(at.owes("s-8e5").0, 1);
    at.board
        .record(&worker("s-8e5", at.t0))
        .expect("recorded over");
    assert_eq!(at.owes("s-8e5"), (0, None));
    // A line that holds no task row (a build's), given up at its first write.
    let mut no_task = worker("s-8f6", at.t0);
    let mut build = task_row(&no_task, None, at.t0, "m101", Digest::dir("/facts"));
    build.kind = Kind::Build;
    no_task.owed = vec![build.line()];
    at.board.record(&no_task).expect("recorded");
    // Its board row pruned, a week after it closed.
    at.board.prune_at(at.t0 + 8 * 24 * 3_600_000);
    assert!(at.board.row("s-8b2").is_none(), "pruned");
    at.board.prune_at(at.t0 + 9 * 24 * 3_600_000);
    // A worker whose closes reversed five times, each end owed: the oldest given up.
    for n in 0..5u64 {
        let rev = 4 + n;
        let end = if n % 2 == 0 {
            "task_state=done conclusion=finished why=landed"
        } else {
            "task_state=exited conclusion=failed why=exit-status exit=1"
        };
        at.apply(
            &format!("@s-8c3 rev={rev} state=closed {end} agent=claude task_news=1"),
            10_000 + 2 * n,
        );
        at.apply(
            &format!("@s-8c3 rev={rev} state=working agent=claude task_news=1"),
            10_001 + 2 * n,
        );
    }
    assert_eq!(at.owes("s-8c3").0, OWED_MAX);
    drop(held);
    at.board
        .settle(&["s-8c3".to_owned(), "s-8f6".to_owned()], at.t0 + 20_000)
        .expect("settled");
    assert_eq!(at.owes("s-8f6"), (0, None));
    assert_eq!(
        (at.dropped(), at.unsaid().len()),
        (4, 1),
        "each row given up before this settle said once by its append, and the line it gave \
         up left for the next"
    );
    assert!(
        ["s-8a1", "s-8b2", "s-8e5", "s-8f6"]
            .iter()
            .all(|sid| at.tasks(sid).is_empty())
    );
    assert_eq!(
        at.tasks("s-8c3")
            .iter()
            .map(|r| (r.outcome, r.at_ms))
            .collect::<Vec<_>>(),
        [
            (Outcome::Fail, at.t0 + 10_002),
            (Outcome::Ok, at.t0 + 10_004),
            (Outcome::Fail, at.t0 + 10_006),
            (Outcome::Ok, at.t0 + 10_008)
        ],
        "the rows still owed, in order"
    );
    at.apply(&record("s-8a1"), 30_000);
    at.apply(&record("s-8d4"), 40_000);
    at.board.prune_at(at.t0 + 10 * 24 * 3_600_000);
    assert_eq!(at.tasks("s-8d4").len(), 1);
    assert_eq!(at.dropped(), 5, "said once");
    assert!(
        at.unsaid().is_empty(),
        "and left unsaid by none: {:?}",
        at.unsaid()
    );
}

/// THE TASK ROW (Continuum TM-1): a worker writes its task row once its row is closed and
/// the board holds the conclusion its Mac reported, whichever came first: a close built from
/// the Mac's last row naming it, or the Mac's report while the row was open and then any
/// close (a stall's only while it stands: `a_stall_its_record_took_back_is_no_end`).
/// NEGATIVE CONTROLS: a close with no conclusion of its Mac's writes none, though the
/// board reads it `failed` and the close carries the Mac's rev; nothing writes a second row
/// of the same end (the record arriving after a close that wrote, a reopen at the close's
/// rev and a second close). A close that reversed and came to another end writes that end,
/// the worker's last row.
#[test]
fn a_worker_writes_its_task_row_once_it_closed_and_its_macs_conclusion_was_heard() {
    let at = Scratch::new(&["s-0a1", "s-0b2", "s-0c3", "s-0d4"]);

    // A conclusion heard while open, then the keeper's close of a tab its Mac no longer hosts.
    at.apply("@s-0a1 rev=2 state=working agent=claude task_news=0", 1_000);
    at.apply("@s-0a1 rev=2 state=closed agent=-", 2_000);
    assert!(!at.open("s-0a1"));
    assert!(
        at.tasks("s-0a1").is_empty(),
        "a close with no conclusion of its Mac's writes none"
    );
    at.apply("@s-0a1 rev=2 state=working agent=claude task_news=0", 3_000);
    assert!(at.open("s-0a1"), "the worker's own answer reopens the row");
    at.apply(
        "@s-0a1 rev=3 state=done agent=claude conclusion=finished why=landed task_news=1",
        4_000,
    );
    assert!(at.tasks("s-0a1").is_empty(), "an open row writes none");
    at.apply("@s-0a1 rev=0 state=closed agent=-", 5_000);
    let rows = at.tasks("s-0a1");
    assert_eq!(rows.len(), 1, "the close of a concluded worker: {rows:?}");
    assert_eq!(
        (rows[0].outcome, rows[0].stop, rows[0].code),
        (Outcome::Ok, Stop::Unknown, None),
        "its Mac's conclusion; no record, no exit code"
    );
    assert_eq!(rows[0].wall_ms, Fig::Known(5_000), "from its spawn");
    // The record the close was read from, applied late (a dispatcher's wait), and a reopen
    // by the Mac's row at the close's rev with a second close: no second row.
    let record =
        "@s-0a1 rev=3 state=closed agent=claude conclusion=finished why=landed task_news=1";
    at.apply(record, 6_000);
    at.apply(
        "@s-0a1 rev=3 state=done agent=claude conclusion=finished why=landed task_news=1",
        7_000,
    );
    assert!(at.open("s-0a1"), "a row at the close's rev reopens it");
    at.apply(record, 8_000);
    assert!(!at.open("s-0a1"));
    assert_eq!(at.tasks("s-0a1").len(), 1, "one row of one end");

    // The Mac's record of the close, never heard concluded before: its words and exit code.
    at.apply(
        "@s-0b2 rev=4 state=closed task_state=exited agent=claude conclusion=failed \
         why=exit-status exit=3 task_news=1",
        2_000,
    );
    let rows = at.tasks("s-0b2");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        (rows[0].outcome, rows[0].stop, rows[0].code),
        (Outcome::Fail, Stop::Exit, Some(3))
    );

    // A wait's close built from the Mac's last row (`closed_row`), the worker still working:
    // it carries the Mac's rev and `task_news=`, and no conclusion of the Mac's.
    at.apply("@s-0c3 rev=1 state=working agent=claude task_news=0", 1_000);
    at.apply("@s-0c3 rev=1 state=closed agent=claude task_news=1", 2_000);
    assert!(!at.open("s-0c3"));
    assert!(
        at.tasks("s-0c3").is_empty(),
        "the board's `failed` for a gone tab is no end its Mac reported"
    );
    // Then that Mac's record of the close, naming its conclusion: the record writes.
    at.apply(
        "@s-0c3 rev=2 state=closed agent=claude conclusion=finished why=landed task_news=1",
        3_000,
    );
    let rows = at.tasks("s-0c3");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].outcome, Outcome::Ok);

    // A stalled worker (open, its Mac's conclusion `failed`) read gone, as a Mac with no window
    // open makes every worker; its tab comes back, it finishes, and closes for good.
    at.apply(
        "@s-0d4 rev=2 state=stalled agent=claude conclusion=failed why=held task_news=1",
        1_000,
    );
    at.apply("@s-0d4 rev=0 state=closed agent=-", 2_000);
    let rows = at.tasks("s-0d4");
    assert_eq!(
        rows.iter().map(|r| (r.outcome, r.stop)).collect::<Vec<_>>(),
        [(Outcome::Fail, Stop::Stalled)],
        "the end the board read"
    );
    at.apply("@s-0d4 rev=3 state=working agent=claude task_news=1", 3_000);
    assert!(at.open("s-0d4"), "its tab came back");
    at.apply(
        "@s-0d4 rev=4 state=done agent=claude conclusion=finished why=landed task_news=1",
        4_000,
    );
    at.apply("@s-0d4 rev=0 state=closed agent=-", 5_000);
    let rows = at.tasks("s-0d4");
    assert_eq!(rows.len(), 2, "the end it came to: {rows:?}");
    assert_eq!(
        (rows[1].outcome, rows[1].wall_ms.clone()),
        (Outcome::Ok, Fig::Known(5_000)),
        "its last row is its end"
    );
}

/// A Mac's own close of a tab gone while its task was `working` says `conclusion=failed` (its
/// reading of the closed tab, `task_state=working`): no end its task came to, so no row,
/// whether the board took it from the Mac's row or from a wait's `closed_row`. NEGATIVE
/// CONTROLS: the same close with the record's own terminal state writes its row; a Mac too
/// old to say `task_state=` is believed only for `finished`; a close below the board's rev
/// is an older answer, no record of the Mac's last row.
#[test]
fn a_macs_reading_of_a_closed_working_tab_is_no_end_its_task_came_to() {
    let at = Scratch::new(&["s-1a1", "s-1b2", "s-1c3", "s-1d4", "s-1e5"]);

    // The Mac's own closed row of a working tab, the keeper's leg parked when it closed.
    at.apply("@s-1a1 rev=3 state=working agent=claude task_news=0", 1_000);
    at.apply(
        "@s-1a1 rev=3 state=closed task_state=working agent=claude conclusion=failed \
         task_news=1",
        2_000,
    );
    assert!(!at.open("s-1a1"));
    assert!(
        at.tasks("s-1a1").is_empty(),
        "the closed tab's reading is no end: {:?}",
        at.tasks("s-1a1")
    );
    // The same close after the record came to its end: the row.
    at.apply(
        "@s-1b2 rev=3 state=closed task_state=failed agent=claude conclusion=failed \
         why=could-not-finish task_news=1",
        2_000,
    );
    assert_eq!(
        at.tasks("s-1b2")
            .iter()
            .map(|r| (r.outcome, r.stop))
            .collect::<Vec<_>>(),
        [(Outcome::Fail, Stop::Unknown)]
    );
    // A Mac too old to say `task_state=`: `failed` is what it says of every closed tab.
    at.apply(
        "@s-1c3 rev=3 state=closed agent=claude conclusion=failed task_news=1",
        2_000,
    );
    assert!(at.tasks("s-1c3").is_empty(), "{:?}", at.tasks("s-1c3"));
    at.apply(
        "@s-1d4 rev=3 state=closed agent=claude conclusion=finished why=landed task_news=1",
        2_000,
    );
    assert_eq!(
        at.tasks("s-1d4").len(),
        1,
        "a record's `finished` is its end"
    );
    // An answer below the board's rev is no record of the Mac's last row.
    at.apply("@s-1e5 rev=5 state=working agent=claude task_news=0", 1_000);
    at.apply(
        "@s-1e5 rev=4 state=closed task_state=done agent=claude conclusion=finished \
         why=landed task_news=1",
        2_000,
    );
    assert!(!at.open("s-1e5"), "the close is taken");
    assert!(at.tasks("s-1e5").is_empty(), "{:?}", at.tasks("s-1e5"));
}

/// A conclusion other than a stall's (`a_stall_its_record_took_back_is_no_end`) the board
/// heard while the row was open is the worker's end whatever close with no record of another
/// end follows: the Mac's own close of a tab working again (a turn
/// after it finished), an old Mac's `failed` with no `task_state=`, or a close with no record
/// of the Mac's (the keeper's), in either order, and a live row of the turn read before or
/// after either. The close's words, a closed tab's reading, are never its outcome or stop.
/// NEGATIVE CONTROL: a record close naming a concluding state is its end, though the board
/// heard another (`s-5e5`), and what a closed row heard is no later close's end (`s-5f6`).
#[test]
fn a_heard_conclusion_is_the_end_whatever_close_follows_and_in_either_order() {
    let at = Scratch::new(&[
        "s-4a1", "s-4b2", "s-4c3", "s-4e5", "s-5a1", "s-5b2", "s-5e5", "s-5f6",
    ]);
    let ends = |sid: &str| {
        at.tasks(sid)
            .iter()
            .map(|r| (r.outcome, r.stop))
            .collect::<Vec<_>>()
    };
    let finished = |sid: &str| {
        format!("@{sid} rev=2 state=done agent=claude conclusion=finished why=landed task_news=1")
    };
    let macs = |sid: &str| {
        format!(
            "@{sid} rev=3 state=closed task_state=working agent=claude conclusion=failed \
             why=turn task_news=1"
        )
    };
    let keepers = |sid: &str| format!("@{sid} rev=2 state=closed agent=-");
    // The Mac's own close first, then the keeper's; and the other way round.
    at.apply(&finished("s-4a1"), 1_000);
    at.apply(&macs("s-4a1"), 2_000);
    at.apply(&keepers("s-4a1"), 3_000);
    at.apply(&finished("s-4b2"), 1_000);
    at.apply(&keepers("s-4b2"), 2_000);
    at.apply(&macs("s-4b2"), 3_000);
    assert_eq!(ends("s-4a1"), [(Outcome::Ok, Stop::Unknown)]);
    assert_eq!(ends("s-4b2"), ends("s-4a1"), "either order, one end");
    // A Mac too old to say `task_state=`: its closed tab's `failed` is not the end.
    at.apply(&finished("s-4c3"), 1_000);
    at.apply(
        "@s-4c3 rev=3 state=closed agent=claude conclusion=failed why=turn task_news=1",
        2_000,
    );
    assert_eq!(ends("s-4c3"), [(Outcome::Ok, Stop::Unknown)]);
    // A wait's close built from a row older than the board's: the heard end stands.
    at.apply(
        "@s-4e5 rev=3 state=done agent=claude conclusion=finished why=landed task_news=1",
        1_000,
    );
    at.apply(
        "@s-4e5 rev=2 state=closed task_state=working agent=claude task_news=1",
        2_000,
    );
    assert_eq!(ends("s-4e5"), [(Outcome::Ok, Stop::Unknown)]);
    // A live row of the turn typed after it (a wait's leg answered while it works), read
    // before the Mac's close, and after it with the keeper's close next.
    let turn = |sid: &str| format!("@{sid} rev=3 state=working agent=claude task_news=0");
    at.apply(&finished("s-5a1"), 1_000);
    at.apply(&turn("s-5a1"), 2_000);
    at.apply(&macs("s-5a1"), 3_000);
    at.apply(&finished("s-5b2"), 1_000);
    at.apply(&macs("s-5b2"), 2_000);
    at.apply(&turn("s-5b2"), 3_000);
    assert!(at.open("s-5b2"), "a live row at the close's rev reopens it");
    at.apply(&keepers("s-5b2"), 4_000);
    assert_eq!(ends("s-5a1"), [(Outcome::Ok, Stop::Unknown)]);
    assert_eq!(ends("s-5b2"), ends("s-5a1"), "either order, one end");
    // NEGATIVE CONTROL: the Mac's record of another end is the end.
    at.apply(&finished("s-5e5"), 1_000);
    at.apply(
        "@s-5e5 rev=3 state=closed task_state=stalled agent=claude conclusion=failed \
         why=held task_news=1",
        2_000,
    );
    assert_eq!(ends("s-5e5"), [(Outcome::Fail, Stop::Stalled)]);
    // NEGATIVE CONTROL: a failure heard (an Autopilot set to off), a turn, then the Mac's
    // record that it finished, then a turn after that closed while working: the failure is
    // no end of the second close.
    at.apply(
        "@s-5f6 rev=2 state=failed agent=claude conclusion=failed why=off task_news=1",
        1_000,
    );
    at.apply(&turn("s-5f6"), 2_000);
    at.apply(
        "@s-5f6 rev=4 state=closed task_state=done agent=claude conclusion=finished \
         why=landed task_news=1",
        3_000,
    );
    at.apply("@s-5f6 rev=5 state=working agent=claude task_news=0", 4_000);
    assert!(at.open("s-5f6"), "the turn reopens it");
    at.apply(
        "@s-5f6 rev=5 state=closed task_state=working agent=claude task_news=1",
        5_000,
    );
    assert_eq!(ends("s-5f6"), [(Outcome::Ok, Stop::Unknown)]);
}

/// A stall's `failed` stands only while the stall does: its record takes it back when the
/// stall moves on (the agent started after all, a held stall made progress), with no turn. A
/// stall the board heard and then read moving on, before the Mac's close of the tab working or
/// after it, is no end; an end heard before the stall still is. NEGATIVE CONTROL: a stall that
/// stood when a recordless close came is the end the board last heard (`s-6d4`).
#[test]
fn a_stall_its_record_took_back_is_no_end() {
    let at = Scratch::new(&["s-6a1", "s-6b2", "s-6c3", "s-6d4"]);
    let ends = |sid: &str| {
        at.tasks(sid)
            .iter()
            .map(|r| (r.outcome, r.stop))
            .collect::<Vec<_>>()
    };
    let stalled = |sid: &str, rev: u64| {
        format!(
            "@{sid} rev={rev} state=stalled agent=claude conclusion=failed why=startup \
             task_news=1"
        )
    };
    let moved_on =
        |sid: &str, rev: u64| format!("@{sid} rev={rev} state=working agent=claude task_news=0");
    let gone = |sid: &str, rev: u64| {
        format!("@{sid} rev={rev} state=closed task_state=working agent=claude task_news=1")
    };
    let keepers = |sid: &str| format!("@{sid} rev=2 state=closed agent=-");
    // The stall moving on read first, then the Mac's close of the tab working.
    at.apply(&stalled("s-6a1", 2), 1_000);
    at.apply(&moved_on("s-6a1", 3), 2_000);
    at.apply(&gone("s-6a1", 3), 3_000);
    assert!(ends("s-6a1").is_empty(), "{:?}", ends("s-6a1"));
    // The Mac's close first, then the stall moving on and the keeper's close.
    at.apply(&stalled("s-6b2", 2), 1_000);
    at.apply(&gone("s-6b2", 3), 2_000);
    at.apply(&moved_on("s-6b2", 3), 3_000);
    at.apply(&keepers("s-6b2"), 4_000);
    assert!(ends("s-6b2").is_empty(), "{:?}", ends("s-6b2"));
    // It finished, was typed into, stalled and moved on, then closed while working: it
    // finished.
    at.apply(
        "@s-6c3 rev=2 state=done agent=claude conclusion=finished why=landed task_news=1",
        1_000,
    );
    at.apply(&moved_on("s-6c3", 3), 2_000);
    at.apply(&stalled("s-6c3", 4), 3_000);
    at.apply(&moved_on("s-6c3", 5), 4_000);
    at.apply(&gone("s-6c3", 5), 5_000);
    assert_eq!(ends("s-6c3"), [(Outcome::Ok, Stop::Unknown)]);
    // NEGATIVE CONTROL: the stall stood as far as the board heard.
    at.apply(&stalled("s-6d4", 2), 1_000);
    at.apply(&keepers("s-6d4"), 2_000);
    assert_eq!(ends("s-6d4"), [(Outcome::Fail, Stop::Stalled)]);
}

/// A record close that names no `why=` says no stop: the `why=held` the board kept from an
/// earlier report (the worker stalled, then moved on) is not why this end came.
#[test]
fn a_record_close_with_no_why_takes_no_stop_from_an_earlier_report() {
    let at = Scratch::new(&["s-3a1"]);
    at.apply(
        "@s-3a1 rev=2 state=stalled agent=claude conclusion=failed why=held task_news=1",
        1_000,
    );
    at.apply(
        "@s-3a1 rev=3 state=closed task_state=failed agent=claude conclusion=failed \
         task_news=1",
        2_000,
    );
    assert_eq!(
        at.tasks("s-3a1")
            .iter()
            .map(|r| (r.outcome, r.stop))
            .collect::<Vec<_>>(),
        [(Outcome::Fail, Stop::Unknown)]
    );
}

/// The board keeps the end a worker's last task row told, so no read of the log decides a
/// second: a log rotated or removed between two closes of one end writes no second row.
/// NEGATIVE CONTROL: a close that reversed and came to another end writes it.
#[test]
fn the_board_keeps_the_end_its_last_task_row_told_past_the_log() {
    let at = Scratch::new(&["s-2a1"]);
    let record = "@s-2a1 rev=3 state=closed task_state=done agent=claude conclusion=finished \
                  why=landed task_news=1";
    at.apply(record, 1_000);
    assert_eq!(at.tasks("s-2a1").len(), 1);
    assert_eq!(
        at.board.row("s-2a1").and_then(|r| r.told).as_deref(),
        Some("ok.unknown")
    );
    std::fs::remove_dir_all(&at.log).expect("the log removed");
    at.apply(
        "@s-2a1 rev=3 state=done agent=claude conclusion=finished why=landed task_news=1",
        2_000,
    );
    assert!(at.open("s-2a1"), "a row at the close's rev reopens it");
    at.apply(record, 3_000);
    assert!(!at.open("s-2a1"));
    assert!(
        at.tasks("s-2a1").is_empty(),
        "no second row of one end: {:?}",
        at.tasks("s-2a1")
    );
    at.apply(
        "@s-2a1 rev=4 state=done agent=claude conclusion=finished why=landed task_news=1",
        4_000,
    );
    at.apply(
        "@s-2a1 rev=5 state=closed task_state=stalled agent=claude conclusion=failed \
         why=held task_news=1",
        5_000,
    );
    assert_eq!(
        at.tasks("s-2a1")
            .iter()
            .map(|r| (r.outcome, r.stop))
            .collect::<Vec<_>>(),
        [(Outcome::Fail, Stop::Stalled)],
        "the other end it came to"
    );
    assert_eq!(
        at.board.row("s-2a1").and_then(|r| r.told).as_deref(),
        Some("fail.stalled")
    );
}

#[test]
fn a_spawn_row_names_its_worker_and_the_mac_it_was_sent_to() {
    let args: Vec<String> = ["agent=claude", "label=dx", "cwd=/Users//me/secret-project"]
        .map(str::to_owned)
        .into();
    let facts = Digest::dir("/facts");
    let sent: Dialled<()> = Ok(Ok(((), Some("s-0a1".to_owned()))));
    let r = spawn_of(&args, 120, &sent, Some(NODE)).row(9, "m101.local", facts);
    assert_eq!(
        (
            r.kind.clone(),
            r.outcome,
            r.stop,
            r.sid.as_deref(),
            r.on.as_deref()
        ),
        (
            Kind::Spawn,
            Outcome::Ok,
            Stop::Exit,
            Some("s-0a1"),
            Some(NODE)
        )
    );
    assert_eq!(
        (r.label.as_str(), r.wall_ms.clone()),
        ("claude", Fig::Known(120))
    );
    assert_eq!(
        (
            r.wait_ms.clone(),
            r.cache.clone(),
            r.cpu_ms.clone(),
            r.peak_bytes.clone()
        ),
        (Fig::Na, Fig::Na, Fig::Na, Fig::Na)
    );
    assert!(!r.line().contains("secret-project"), "{}", r.line());
    let refused: Dialled<()> = Ok(Ok(((), None)));
    let r = spawn_of(&args, 5, &refused, Some(NODE)).row(9, "m101", facts);
    assert_eq!(
        (r.outcome, r.stop, r.sid),
        (Outcome::Refused, Stop::Unknown, None)
    );
    let lost: Dialled<()> = Ok(Err(std::io::Error::other("the link closed")));
    let r = spawn_of(&args, 5, &lost, Some(NODE)).row(9, "m101", facts);
    assert_eq!(
        (r.outcome, r.on.as_deref()),
        (Outcome::Unknown, Some(NODE)),
        "its task may have reached that Mac"
    );
    let shell: Vec<String> = Vec::new();
    assert_eq!(
        spawn_of(&shell, 5, &refused, None)
            .row(9, "m101", facts)
            .label,
        "-"
    );
    // A dial that failed (no grant, no route) sent nothing: refused, with the Mac it was for.
    let undialled: Dialled<()> = Err(std::io::Error::other("drive not granted"));
    let r = spawn_of(&args, 5, &undialled, Some(NODE)).row(9, "m101", facts);
    assert_eq!(
        (r.outcome, r.sid, r.on.as_deref()),
        (Outcome::Refused, None, Some(NODE))
    );
    // Its folder is read as the board reads it, so the spawn row's folder is the task row's.
    let quoted: Vec<String> = ["agent=claude", "cwd=/first", "cwd=\"/Users//me/a b\""]
        .map(str::to_owned)
        .into();
    let board = crate::board::spawned(
        "s-0a1",
        crate::board::Placed::default(),
        None,
        (&quoted, b"task"),
        None,
        9,
    );
    let r = spawn_of(&quoted, 5, &sent, Some(NODE)).row(9, "m101", facts);
    assert_eq!(board.cwd.as_deref(), Some("/Users//me/a b"));
    assert_eq!(r.dir, board.cwd.as_deref().map(Digest::dir));
}
