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

/// A sender's row goes to this Mac's state folder's log, the one its board's task rows go to:
/// with no [`crate::board::HereGuard`], [`here_log`] is the log [`log_of`] finds beside
/// [`Board::here`]'s folder (a test binary's own state). NEGATIVE CONTROL: under a guard, it
/// is the log beside the guard's board, and none for a folder not named `board`.
#[cfg(unix)]
#[test]
fn a_senders_row_goes_to_the_log_its_boards_task_rows_go_to() {
    let state = aterm_types::dirs::state_dir().expect("a state folder");
    assert_eq!(here_log(), Some(aterm_telemetry::log::dir_in(&state)));
    let board = Board::here().expect("this Mac's board");
    assert_eq!(here_log(), log_of(board.dir()));
    let tmp = aterm_tempfile::tempdir().expect("scratch");
    {
        let _here = crate::board::HereGuard::at(&tmp.path().join(crate::board::DIR));
        assert_eq!(here_log(), Some(aterm_telemetry::log::dir_in(tmp.path())));
    }
    let _here = crate::board::HereGuard::at(&tmp.path().join("elsewhere"));
    assert_eq!(here_log(), None);
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
        hold_lock(&self.log)
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
/// The row whose write failed keeps its file as it was, its close unkept and its row not
/// appended, so the next apply tells its end once. NEGATIVE CONTROL: the failed row is the
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
    assert_eq!(at.tasks("s-9b1").len(), 1, "told once its row was kept");
    assert_eq!(at.dropped(), 0);
    assert!(at.unsaid().is_empty());
}

/// Hold the lock of the log in `log`, as another writer's long append does, until the guard
/// drops.
fn hold_lock(log: &std::path::Path) -> std::fs::File {
    std::fs::create_dir_all(log).expect("the log's folder");
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(log.join(".lock"))
        .expect("lock file");
    aterm_uds::file_lock::lock_exclusive_until(
        &held,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
        "the test holds the telemetry lock",
    )
    .expect("hold the lock");
    held
}

/// Fault the `nth` write (from 1) of the board row of `sid` on this thread, until the guard
/// drops ([`crate::board::fault_writes`]).
fn fault_write(
    sid: &'static str,
    nth: usize,
    fault: crate::board::WriteFault,
) -> crate::board::FaultedWrites {
    let mut writes = 0;
    crate::board::fault_writes(move |row| {
        if row.sid != sid {
            return None;
        }
        writes += 1;
        (writes == nth).then_some(fault)
    })
}

/// A row whose write fails after its pay's append wrote what it owed (as an exit there
/// would leave it) is paying, never owing: the next pay asks the log, which holds the row,
/// so it is told and written no second time; and where the log's lock refuses every ask
/// until [`OWED_KEPT`] after its close, it is dropped unsaid, never said lost while the log
/// holds it, and its end, told by the paying write, stays told, so a reopen and a second
/// close to it write no second row. A row whose append was refused and whose write after it
/// failed too is paying a row the log does not hold: the next pay writes it once. NEGATIVE
/// CONTROL: a row whose paying write fails keeps its file as it was, and the append writes
/// none of it.
#[test]
fn a_row_paying_when_its_write_failed_is_settled_by_what_the_log_holds() {
    use crate::board::WriteFault::Before;
    let at = Scratch::new(&["s-9d1", "s-9d2", "s-9d3", "s-9d4"]);
    let close = |sid: &str| {
        format!(
            "@{sid} rev=3 state=closed task_state=exited agent=claude conclusion=finished \
             why=exit-status exit=0 task_news=1"
        )
    };
    let paying = |sid: &str| {
        let row = at.board.row(sid).expect("a row");
        (row.owed.len(), row.paying.len(), row.told)
    };
    let kept = u64::try_from(OWED_KEPT.as_millis()).expect("ms");
    for sid in ["s-9d1", "s-9d2"] {
        let faulted = fault_write(sid, 2, Before);
        assert!(
            at.board.apply(&[close(sid)], at.t0 + 1_000).is_err(),
            "the write after the append failed"
        );
        drop(faulted);
        assert_eq!(at.tasks(sid).len(), 1, "the append wrote it");
        assert!(!at.open(sid), "the paying write kept the close");
        assert_eq!(
            paying(sid),
            (0, 1, Some("ok.exit".to_owned())),
            "paying, never owing, its end told"
        );
    }
    at.board
        .settle(&["s-9d1".to_owned()], at.t0 + 2_000)
        .expect("settled");
    assert_eq!(at.tasks("s-9d1").len(), 1, "written no second time");
    assert_eq!(paying("s-9d1"), (0, 0, Some("ok.exit".to_owned())));
    let held = at.hold_log();
    // Reopened at its close's rev and closed again while paying: the end it pays is its end.
    at.apply(
        "@s-9d2 rev=3 state=exited agent=claude conclusion=finished why=exit-status exit=0 \
         task_news=1",
        1_500,
    );
    assert!(at.open("s-9d2"));
    at.apply(&close("s-9d2"), 1_600);
    assert_eq!(
        paying("s-9d2"),
        (0, 1, Some("ok.exit".to_owned())),
        "no second row of its end"
    );
    at.board
        .settle(&["s-9d2".to_owned()], at.t0 + kept)
        .expect("settled");
    assert_eq!(
        paying("s-9d2"),
        (0, 1, Some("ok.exit".to_owned())),
        "kept while unjudged"
    );
    at.board
        .settle(&["s-9d2".to_owned()], at.t0 + 1_000 + kept)
        .expect("settled");
    assert_eq!(
        paying("s-9d2"),
        (0, 0, Some("ok.exit".to_owned())),
        "dropped unsaid, its end told"
    );

    // Refused by the log's lock, and the write after it failed: paying, though not written.
    let faulted = fault_write("s-9d3", 2, Before);
    assert!(at.board.apply(&[close("s-9d3")], at.t0 + 3_000).is_err());
    drop(faulted);
    assert_eq!(paying("s-9d3"), (0, 1, Some("ok.exit".to_owned())));
    drop(held);
    assert!(at.tasks("s-9d3").is_empty());
    let faulted = fault_write("s-9d4", 1, Before);
    assert!(at.board.apply(&[close("s-9d4")], at.t0 + 3_000).is_err());
    drop(faulted);
    assert!(at.open("s-9d4"), "its file kept as it was");
    assert!(at.tasks("s-9d4").is_empty(), "the append wrote none of it");
    // Reopened and closed again once its paying row was dropped unsaid: the end it told.
    at.apply(
        "@s-9d2 rev=3 state=exited agent=claude conclusion=finished why=exit-status exit=0 \
         task_news=1",
        kept + 2_000,
    );
    assert!(at.open("s-9d2"));
    at.apply(&close("s-9d2"), kept + 3_000);
    at.board
        .settle(&["s-9d3".to_owned()], at.t0 + 4_000)
        .expect("settled");
    at.apply(&close("s-9d4"), 5_000);
    for sid in ["s-9d1", "s-9d2", "s-9d3", "s-9d4"] {
        assert_eq!(at.tasks(sid).len(), 1, "{sid} written once");
    }
    assert_eq!(paying("s-9d3"), (0, 0, Some("ok.exit".to_owned())));
    assert_eq!(at.dropped(), 0, "none said lost");
    assert!(at.unsaid().is_empty(), "{:?}", at.unsaid());
}

/// A row paying a line a later aterm wrote (a word this aterm's rows do not write) asks the
/// log for it as its file holds it: the log holds it, so it is told and written no second
/// time, though this aterm would write it otherwise. NEGATIVE CONTROL (by hand: the line
/// asked for as this aterm writes it: not found, written again).
#[test]
fn a_paying_line_a_later_aterm_wrote_is_asked_for_as_it_was_written() {
    let at = Scratch::new(&["s-9f1"]);
    let mut row = worker("s-9f1", at.t0);
    let mine = task_row(&row, None, at.t0 + 1_000, "m101", Digest::dir("/facts")).line();
    let (body, _) = mine
        .strip_suffix('\n')
        .and_then(|line| line.rsplit_once(" sum="))
        .expect("a sealed line");
    let later = aterm_telemetry::row::sealed(format!("{body} later=1"));
    let read = owed_row(&later).expect("this aterm reads it");
    assert_ne!(read.line(), later, "and writes it otherwise");
    assert_eq!(
        aterm_telemetry::direct::write_kept(at.log.clone(), std::slice::from_ref(&later)),
        aterm_telemetry::direct::Kept::Written,
        "the later aterm's append took it"
    );
    // Its paying write told its end, as this aterm's does.
    row.paying = vec![later];
    row.told = Some(told_of(&read));
    at.board
        .record(&row)
        .expect("its write after failed: paying");
    at.board
        .settle(&["s-9f1".to_owned()], at.t0 + 2_000)
        .expect("settled");
    assert_eq!(at.tasks("s-9f1").len(), 1, "written no second time");
    let row = at.board.row("s-9f1").expect("a row");
    assert_eq!(
        (row.owed.len(), row.paying.len(), row.told),
        (0, 0, Some(told_of(&read)))
    );
}

/// An append that failed and could not be cut back leaves its rows paying, never owing: the
/// next pay asks the log, which holds what the write left, so none is written twice and none
/// said lost. NEGATIVE CONTROL (by hand: such an append taken as refused, its rows owed
/// again: a second row).
#[test]
fn an_append_left_uncut_leaves_its_rows_paying_for_the_log_to_settle() {
    let at = Scratch::new(&["s-9a7"]);
    crate::board::pay_says(aterm_telemetry::direct::Kept::Unsure);
    at.apply(
        "@s-9a7 rev=3 state=closed task_state=exited agent=claude conclusion=finished \
         why=exit-status exit=0 task_news=1",
        1_000,
    );
    let row = at.board.row("s-9a7").expect("a row");
    assert_eq!((row.owed.len(), row.paying.len()), (0, 1), "paying");
    assert_eq!(at.tasks("s-9a7").len(), 1, "what the write left");
    at.board
        .settle(&["s-9a7".to_owned()], at.t0 + 2_000)
        .expect("settled");
    assert_eq!(at.tasks("s-9a7").len(), 1, "written no second time");
    assert_eq!(at.owes("s-9a7"), (0, Some("ok.exit".to_owned())));
    assert_eq!(at.dropped(), 0);
    assert!(at.unsaid().is_empty(), "none said lost: {:?}", at.unsaid());
}

/// A ROW TELLS THE END IT PAYS BEFORE ITS APPEND, AND A SETTLE KEEPS A NEWER ONE (TM-1
/// review): a board writer that knows no `paying=` (an older aterm, which keeps it and
/// `was_told=` as words it does not know) reads the paying row's `told=` as the end it pays,
/// and one that reopened the row and told a newer end since keeps that end: a close to it
/// read while the row still pays the older writes no second row, the settle its apply makes
/// keeps it, and a reopen and a further close to it write none either. NEGATIVE CONTROLS (by
/// hand): the settle telling the paying line's end: `told=` back at the older end, and the
/// newer written twice; the close's check reading the paying line's end before `told=`: the
/// newer written twice.
#[test]
fn a_paying_rows_end_is_told_before_its_append_and_a_settle_keeps_a_newer_one() {
    use crate::board::WriteFault::Before;
    let at = Scratch::new(&["s-9b1"]);
    let faulted = fault_write("s-9b1", 2, Before);
    assert!(
        at.board
            .apply(
                &["@s-9b1 rev=3 state=closed task_state=exited agent=claude \
                   conclusion=finished why=exit-status exit=0 task_news=1"
                    .to_owned()],
                at.t0 + 1_000,
            )
            .is_err(),
        "the write after the append failed"
    );
    drop(faulted);
    let mut row = at.board.row("s-9b1").expect("a row");
    assert_eq!(
        (row.paying.len(), row.told.as_deref()),
        (1, Some("ok.exit")),
        "its end told before the append"
    );
    // An older aterm's writes, which leave `paying=` and `was_told=` as they found them: a
    // reopen, and a close to a newer end, which it writes and tells.
    let paid = std::mem::take(&mut row.paying);
    let was = row.was_told.take();
    at.board.record(&row).expect("recorded");
    at.apply(
        "@s-9b1 rev=4 state=stalled agent=claude conclusion=failed why=held task_news=1",
        2_000,
    );
    at.apply(
        "@s-9b1 rev=5 state=closed task_state=stalled agent=claude conclusion=failed \
         why=held task_news=1",
        3_000,
    );
    assert_eq!(
        at.board.row("s-9b1").expect("a row").told.as_deref(),
        Some("fail.stalled")
    );
    // ... and a reopen at that close's rev, before this aterm reads the row again.
    at.apply(
        "@s-9b1 rev=5 state=stalled agent=claude conclusion=failed why=held task_news=1",
        3_500,
    );
    let mut row = at.board.row("s-9b1").expect("a row");
    assert!(row.is_open());
    row.paying = paid;
    row.was_told = was;
    at.board.record(&row).expect("recorded");
    at.apply(
        "@s-9b1 rev=5 state=closed task_state=stalled agent=claude conclusion=failed \
         why=held task_news=1",
        4_000,
    );
    let row = at.board.row("s-9b1").expect("a row");
    assert_eq!(
        (row.owed.len(), row.paying.len(), row.told.as_deref()),
        (0, 0, Some("fail.stalled")),
        "no second row of the newer end owed, and the newer end kept"
    );
    at.apply(
        "@s-9b1 rev=5 state=stalled agent=claude conclusion=failed why=held task_news=1",
        5_000,
    );
    assert!(at.open("s-9b1"), "a row at the close's rev reopens it");
    at.apply(
        "@s-9b1 rev=5 state=closed task_state=stalled agent=claude conclusion=failed \
         why=held task_news=1",
        6_000,
    );
    assert_eq!(
        at.tasks("s-9b1")
            .iter()
            .map(|r| (r.outcome, r.stop))
            .collect::<Vec<_>>(),
        [(Outcome::Ok, Stop::Exit), (Outcome::Fail, Stop::Stalled)],
        "each end written once"
    );
}

/// A PAYING ROW THE LOG LACKS, PAST A NEWER END TOLD SINCE, IS GIVEN UP (TM-1 review): a
/// board writer that knows no `paying=` (an older aterm) reopened the row and wrote a newer
/// end while it paid a row the log never took, which written now would follow the newer one:
/// it is said lost once, the newer end stays told and the log's last task row, and a reopen
/// and a second close to that end write no second row. NEGATIVE CONTROL (by hand: the row
/// the log lacks owed again whatever was told since): the older end written after the newer,
/// `told=` back at it, and the newer end written twice.
#[test]
fn a_paying_row_the_log_lacks_past_a_newer_end_told_since_is_given_up() {
    use crate::board::WriteFault::Before;
    let at = Scratch::new(&["s-9c1"]);
    let held = at.hold_log();
    let faulted = fault_write("s-9c1", 2, Before);
    assert!(
        at.board
            .apply(
                &["@s-9c1 rev=3 state=closed task_state=exited agent=claude \
                   conclusion=finished why=exit-status exit=0 task_news=1"
                    .to_owned()],
                at.t0 + 1_000,
            )
            .is_err(),
        "the append refused, and the write after it failed"
    );
    drop(faulted);
    drop(held);
    let mut row = at.board.row("s-9c1").expect("a row");
    assert_eq!(
        (
            row.paying.len(),
            row.told.as_deref(),
            row.was_told.as_deref()
        ),
        (1, Some("ok.exit"), None),
        "paying a row the log does not hold, its end told"
    );
    assert!(at.tasks("s-9c1").is_empty());
    // An older aterm's writes, which leave `paying=` and `was_told=` as they found them: a
    // reopen, and a close to a newer end, which it writes and tells.
    let paid = std::mem::take(&mut row.paying);
    let was = row.was_told.take();
    at.board.record(&row).expect("recorded");
    at.apply(
        "@s-9c1 rev=4 state=stalled agent=claude conclusion=failed why=held task_news=1",
        2_000,
    );
    at.apply(
        "@s-9c1 rev=5 state=closed task_state=stalled agent=claude conclusion=failed \
         why=held task_news=1",
        3_000,
    );
    let mut row = at.board.row("s-9c1").expect("a row");
    assert_eq!(row.told.as_deref(), Some("fail.stalled"));
    row.paying = paid;
    row.was_told = was;
    at.board.record(&row).expect("recorded");
    at.board
        .settle(&["s-9c1".to_owned()], at.t0 + 4_000)
        .expect("settled");
    let row = at.board.row("s-9c1").expect("a row");
    assert_eq!(
        (row.owed.len(), row.paying.len(), row.told.as_deref()),
        (0, 0, Some("fail.stalled")),
        "given up, the newer end kept"
    );
    assert_eq!(at.unsaid().len(), 1, "the row it gave up, said lost");
    at.apply(
        "@s-9c1 rev=5 state=stalled agent=claude conclusion=failed why=held task_news=1",
        5_000,
    );
    assert!(at.open("s-9c1"), "a row at the close's rev reopens it");
    at.apply(
        "@s-9c1 rev=5 state=closed task_state=stalled agent=claude conclusion=failed \
         why=held task_news=1",
        6_000,
    );
    assert_eq!(
        at.tasks("s-9c1")
            .iter()
            .map(|r| (r.outcome, r.stop))
            .collect::<Vec<_>>(),
        [(Outcome::Fail, Stop::Stalled)],
        "the newer end the last, written once"
    );
}

/// A PAYING ROW THE LOG LACKS, REFUSED AGAIN, TELLS THE END BEFORE IT (TM-1 review): its
/// paying write told its end, which the log never took, so where the log's lock refuses the
/// append that pays it again, the row owes it told the end it told before its first paying
/// write (`was_told=`), and once it is given up, a later close to that end is written.
/// NEGATIVE CONTROL (by hand: the end that pay's paying write found told put back): `told=`
/// left at an end the log never held, and the later close neither written nor said lost.
#[test]
fn a_paying_row_the_log_lacks_refused_again_tells_the_end_before_it() {
    use crate::board::WriteFault::Before;
    let at = Scratch::new(&["s-9c2"]);
    let close = "@s-9c2 rev=3 state=closed task_state=exited agent=claude \
                 conclusion=finished why=exit-status exit=0 task_news=1";
    let held = at.hold_log();
    let faulted = fault_write("s-9c2", 2, Before);
    assert!(at.board.apply(&[close.to_owned()], at.t0 + 1_000).is_err());
    drop((faulted, held));
    assert!(at.tasks("s-9c2").is_empty());
    // The log's lock taken by another writer once the settle's ask found the row absent, at
    // the paying write of its pay.
    let lock = std::rc::Rc::new(std::cell::RefCell::new(None));
    let taken = std::rc::Rc::clone(&lock);
    let log = at.log.clone();
    let faulted = crate::board::fault_writes(move |row| {
        if row.sid == "s-9c2" && !row.paying.is_empty() {
            taken.borrow_mut().get_or_insert_with(|| hold_lock(&log));
        }
        None
    });
    at.board
        .settle(&["s-9c2".to_owned()], at.t0 + 2_000)
        .expect("settled");
    drop(faulted);
    assert!(lock.borrow().is_some(), "the append refused");
    let row = at.board.row("s-9c2").expect("a row");
    assert_eq!(
        (row.owed.len(), row.paying.len(), row.told, row.was_told),
        (1, 0, None, None),
        "owed again, told the end before it"
    );
    let kept = u64::try_from(OWED_KEPT.as_millis()).expect("ms");
    at.board
        .settle(&["s-9c2".to_owned()], at.t0 + 1_000 + kept)
        .expect("settled");
    assert_eq!(at.owes("s-9c2"), (0, None), "given up");
    drop(lock);
    at.apply(
        "@s-9c2 rev=3 state=exited agent=claude conclusion=finished why=exit-status exit=0 \
         task_news=1",
        kept + 2_000,
    );
    assert!(at.open("s-9c2"));
    at.apply(close, kept + 3_000);
    assert_eq!(
        at.tasks("s-9c2")
            .iter()
            .map(|r| (r.outcome, r.stop))
            .collect::<Vec<_>>(),
        [(Outcome::Ok, Stop::Exit)],
        "the later close written"
    );
    assert_eq!(at.dropped(), 1, "the row given up, said lost once");
    assert!(at.unsaid().is_empty(), "{:?}", at.unsaid());
}

/// A PAYING ROW THE LOG LACKS TELLS AGAIN THE END THE LOG HELD BEFORE IT (TM-1 review):
/// a worker's close to `ok.exit` is written; a later close to `fail.stalled` is refused by the
/// log's lock and the write after it fails, so its row is paying a row the log does not hold,
/// `told=` that row's end and `was_told=` `ok.exit`. The settle that finds it absent, its
/// append refused again, owes it told `ok.exit`; once it is given up, a reopen and a close
/// back to `ok.exit` write no second row, since the log's last task row is that end; the
/// row given up is left for the next append to say lost. NEGATIVE CONTROL (by hand: the settle restoring no end, `None`): `told=` lost, and the
/// close back to `ok.exit` written twice.
#[test]
fn a_paying_row_the_log_lacks_tells_again_the_end_the_log_held_before_it() {
    use crate::board::WriteFault::Before;
    let at = Scratch::new(&["s-9c3"]);
    at.apply(
        "@s-9c3 rev=3 state=closed task_state=exited agent=claude conclusion=finished \
         why=exit-status exit=0 task_news=1",
        1_000,
    );
    assert_eq!(at.owes("s-9c3"), (0, Some("ok.exit".to_owned())));
    at.apply(
        "@s-9c3 rev=4 state=stalled agent=claude conclusion=failed why=held task_news=1",
        2_000,
    );
    assert!(at.open("s-9c3"));
    let held = at.hold_log();
    let faulted = fault_write("s-9c3", 2, Before);
    assert!(
        at.board
            .apply(
                &["@s-9c3 rev=5 state=closed task_state=stalled agent=claude \
                   conclusion=failed why=held task_news=1"
                    .to_owned()],
                at.t0 + 3_000,
            )
            .is_err()
    );
    drop((faulted, held));
    let row = at.board.row("s-9c3").expect("a row");
    assert_eq!(
        (row.owed.len(), row.paying.len(), row.told, row.was_told),
        (
            0,
            1,
            Some("fail.stalled".to_owned()),
            Some("ok.exit".to_owned())
        ),
        "paying a row the log does not hold"
    );
    // The log's lock taken by another writer once the settle's ask found the row absent.
    let lock = std::rc::Rc::new(std::cell::RefCell::new(None));
    let taken = std::rc::Rc::clone(&lock);
    let log = at.log.clone();
    let faulted = crate::board::fault_writes(move |row| {
        if row.sid == "s-9c3" && !row.paying.is_empty() {
            taken.borrow_mut().get_or_insert_with(|| hold_lock(&log));
        }
        None
    });
    at.board
        .settle(&["s-9c3".to_owned()], at.t0 + 4_000)
        .expect("settled");
    drop(faulted);
    assert!(lock.borrow().is_some(), "the append refused");
    let row = at.board.row("s-9c3").expect("a row");
    assert_eq!(
        (row.owed.len(), row.paying.len(), row.told, row.was_told),
        (1, 0, Some("ok.exit".to_owned()), None),
        "owed again, told the end the log held before it"
    );
    let kept = u64::try_from(OWED_KEPT.as_millis()).expect("ms");
    at.board
        .settle(&["s-9c3".to_owned()], at.t0 + 3_000 + kept)
        .expect("settled");
    assert_eq!(
        at.owes("s-9c3"),
        (0, Some("ok.exit".to_owned())),
        "given up"
    );
    drop(lock);
    at.apply(
        "@s-9c3 rev=5 state=exited agent=claude conclusion=finished why=exit-status exit=0 \
         task_news=1",
        kept + 4_000,
    );
    assert!(at.open("s-9c3"));
    at.apply(
        "@s-9c3 rev=6 state=closed task_state=exited agent=claude conclusion=finished \
         why=exit-status exit=0 task_news=1",
        kept + 5_000,
    );
    assert_eq!(
        at.tasks("s-9c3")
            .iter()
            .map(|r| (r.outcome, r.stop))
            .collect::<Vec<_>>(),
        [(Outcome::Ok, Stop::Exit)],
        "the close back to the end the log holds written no second time"
    );
    assert_eq!(
        (at.dropped(), at.unsaid().len()),
        (0, 1),
        "the row given up left for the next append to say lost"
    );
}

/// A PAYING ROW DROPPED UNSAID BESIDE ONE STILL PAYING IS TOLD (TM-1 review, reproduced by
/// the sixth review): a close to `ok.exit` is written; a close to `fail.stalled` (A) is owed
/// under the log's held lock; a close to `fail.unknown` (B) close to [`OWED_KEPT`] later
/// pays both, its append refused and the write after it failed, and the log comes to hold A
/// but not B (an append cut after its first line). A settle that cannot read the log once A
/// is past its time drops A unsaid and keeps B, and takes A's end as the one told before B.
/// The settle that finds B absent, its append refused, owes it told A's end, and once B is
/// given up a reopen and a close to `ok.exit` are written: the log's last task row is the
/// worker's end. NEGATIVE CONTROL (by hand: the drop leaving `was_told=` as it was, as
/// before the sixth review): B's settle tells `ok.exit`, an end older than A, and the close
/// to `ok.exit` writes nothing, so the log ends at A.
#[test]
fn a_paying_row_dropped_unsaid_beside_one_still_paying_is_told() {
    use crate::board::WriteFault::Before;
    let at = Scratch::new(&["s-9c4"]);
    let kept = u64::try_from(OWED_KEPT.as_millis()).expect("ms");
    at.apply(
        "@s-9c4 rev=3 state=closed task_state=exited agent=claude conclusion=finished \
         why=exit-status exit=0 task_news=1",
        1_000,
    );
    at.apply(
        "@s-9c4 rev=4 state=stalled agent=claude conclusion=failed why=held task_news=1",
        2_000,
    );
    let held = at.hold_log();
    at.apply(
        "@s-9c4 rev=5 state=closed task_state=stalled agent=claude conclusion=failed \
         why=held task_news=1",
        3_000,
    );
    assert_eq!(at.owes("s-9c4"), (1, Some("ok.exit".to_owned())), "A owed");
    let b_at = kept - 60_000;
    at.apply(
        "@s-9c4 rev=6 state=working agent=claude task_news=1",
        b_at - 1_000,
    );
    let faulted = fault_write("s-9c4", 2, Before);
    assert!(
        at.board
            .apply(
                &["@s-9c4 rev=7 state=closed task_state=failed agent=claude \
                   conclusion=failed why=could-not-finish task_news=1"
                    .to_owned()],
                at.t0 + b_at,
            )
            .is_err()
    );
    drop((faulted, held));
    let row = at.board.row("s-9c4").expect("a row");
    assert_eq!(
        (row.paying.len(), row.was_told.as_deref()),
        (2, Some("ok.exit")),
        "paying A and B"
    );
    assert_eq!(
        aterm_telemetry::direct::write_kept(at.log.clone(), &row.paying[..1]),
        aterm_telemetry::direct::Kept::Written,
        "the log holds A"
    );
    let held = at.hold_log();
    at.board
        .settle(&["s-9c4".to_owned()], at.t0 + 3_000 + kept + 1_000)
        .expect("settled");
    drop(held);
    let row = at.board.row("s-9c4").expect("a row");
    assert_eq!(
        (row.paying.len(), row.was_told.as_deref()),
        (1, Some("fail.stalled")),
        "A dropped unsaid, its end told before B"
    );
    let lock = std::rc::Rc::new(std::cell::RefCell::new(None));
    let taken = std::rc::Rc::clone(&lock);
    let log = at.log.clone();
    let faulted = crate::board::fault_writes(move |row| {
        if row.sid == "s-9c4" && !row.paying.is_empty() {
            taken.borrow_mut().get_or_insert_with(|| hold_lock(&log));
        }
        None
    });
    at.board
        .settle(&["s-9c4".to_owned()], at.t0 + 3_000 + kept + 2_000)
        .expect("settled");
    drop(faulted);
    assert!(lock.borrow().is_some(), "B's append refused");
    assert_eq!(
        at.owes("s-9c4"),
        (1, Some("fail.stalled".to_owned())),
        "B owed again, told A's end"
    );
    at.board
        .settle(&["s-9c4".to_owned()], at.t0 + b_at + kept)
        .expect("settled");
    assert_eq!(
        at.owes("s-9c4"),
        (0, Some("fail.stalled".to_owned())),
        "B given up"
    );
    drop(lock);
    at.apply(
        "@s-9c4 rev=7 state=exited agent=claude conclusion=finished why=exit-status exit=0 \
         task_news=1",
        2 * kept,
    );
    assert!(at.open("s-9c4"));
    at.apply(
        "@s-9c4 rev=8 state=closed task_state=exited agent=claude conclusion=finished \
         why=exit-status exit=0 task_news=1",
        2 * kept + 1_000,
    );
    assert_eq!(
        at.tasks("s-9c4")
            .iter()
            .map(|r| (r.outcome, r.stop))
            .collect::<Vec<_>>(),
        [
            (Outcome::Ok, Stop::Exit),
            (Outcome::Fail, Stop::Stalled),
            (Outcome::Ok, Stop::Exit)
        ],
        "the log's last task row is the worker's end"
    );
    assert_eq!(at.dropped(), 1, "B given up, said lost once");
}

/// A board write whose folder's flush fails after its rename published the row is kept:
/// its file holds the row, so what the row gave up is said lost, and its apply or settle
/// answers no failure. NEGATIVE CONTROL: a write refused before its rename keeps the file as
/// it was and says nothing.
#[test]
fn a_write_its_rename_published_says_what_its_row_gave_up() {
    use crate::board::WriteFault::{After, Before};
    let at = Scratch::new(&["s-9e1", "s-9e2"]);
    let given_up = |sid: &str| {
        let mut row = worker(sid, at.t0);
        let mut build = task_row(&row, None, at.t0, "m101", Digest::dir("/facts"));
        build.kind = Kind::Build;
        row.owed = vec![build.line()];
        at.board.record(&row).expect("recorded");
    };
    given_up("s-9e1");
    let faulted = fault_write("s-9e1", 1, Before);
    assert!(at.board.settle(&["s-9e1".to_owned()], at.t0).is_err());
    drop(faulted);
    assert_eq!(at.owes("s-9e1").0, 1, "its file as it was");
    assert!(at.unsaid().is_empty(), "nothing said");
    let faulted = fault_write("s-9e1", 1, After);
    at.board
        .settle(&["s-9e1".to_owned()], at.t0)
        .expect("its row published");
    drop(faulted);
    assert_eq!(at.owes("s-9e1").0, 0);
    assert_eq!(at.unsaid().len(), 1, "the line it gave up, said lost");

    let held = at.hold_log();
    at.apply(
        "@s-9e2 rev=3 state=closed task_state=done agent=claude conclusion=finished \
         why=landed task_news=1",
        1_000,
    );
    drop(held);
    let faulted = crate::board::fault_writes(|row| (row.sid == "s-9e2").then_some(After));
    at.board
        .settle(&["s-9e2".to_owned()], at.t0 + 2_000)
        .expect("each write published");
    drop(faulted);
    assert_eq!(at.tasks("s-9e2").len(), 1);
    assert_eq!(at.owes("s-9e2"), (0, Some("ok.unknown".to_owned())));
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
