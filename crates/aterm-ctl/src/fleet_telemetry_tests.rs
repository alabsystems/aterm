// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[cfg(unix)]
use crate::board::Board;
use crate::board::Row as BoardRow;

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
    let mut row = worker("t0a1xxxxxxx", 1_000);
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
    let close = |rest: &str| format!("@t0a1xxxxxxx rev=4 state=closed {rest} task_news=1");
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

    let mut row = worker("t0a1xxxxxxx", 1_000);
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
        (Some("t0a1xxxxxxx"), Some(NODE), "claude", "m101")
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
/// `apply` merges one seen row, `tasks` reads the task rows the state's log holds. Only Unix
/// keeps a board (`Board::bind`), so the tests on one are Unix's.
#[cfg(unix)]
struct Scratch {
    _tmp: aterm_tempfile::TempDir,
    board: Board,
    log: std::path::PathBuf,
    t0: u64,
}

#[cfg(unix)]
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
#[cfg(unix)]
#[test]
fn a_task_row_the_logs_lock_refused_is_written_later_as_it_was_and_never_said_lost() {
    let at = Scratch::new(&["t7a1xxxxxxx", "t7b2xxxxxxx", "t7c3xxxxxxx"]);
    let record = "@t7a1xxxxxxx rev=3 state=closed task_state=exited agent=claude \
                  conclusion=finished why=exit-status exit=0 task_news=1";
    let held = at.hold_log();
    at.apply(record, 61_000);
    at.apply(
        "@t7c3xxxxxxx rev=2 state=closed task_state=failed agent=claude conclusion=failed \
         why=could-not-finish task_news=1",
        62_000,
    );
    assert!(!at.open("t7a1xxxxxxx"));
    assert_eq!(at.owes("t7a1xxxxxxx"), (1, None), "the row its log refused");
    assert_eq!(at.owes("t7c3xxxxxxx"), (1, None));
    assert!(
        at.unsaid().is_empty(),
        "a row the board keeps is not counted lost"
    );
    drop(held);
    // A later answer of the worker, which moves nothing, writes what its row owes.
    at.apply(record, 70_000);
    let rows = at.tasks("t7a1xxxxxxx");
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
    assert_eq!(at.owes("t7a1xxxxxxx"), (0, Some("ok.exit".to_owned())));
    assert_eq!(
        at.dropped(),
        0,
        "a row written late is said lost by no gap line"
    );
    assert!(at.unsaid().is_empty());
    at.apply(record, 80_000);
    assert_eq!(at.tasks("t7a1xxxxxxx").len(), 1, "written once");

    // A close that reversed and came to another end, its write refused: the keeper's settle
    // writes it, the worker's last row, in the one append that writes `t7c3xxxxxxx`'s.
    let done = "@t7b2xxxxxxx rev=3 state=closed task_state=done agent=claude conclusion=finished \
                why=landed task_news=1";
    at.apply(done, 1_000);
    at.apply(
        "@t7b2xxxxxxx rev=3 state=done agent=claude conclusion=finished why=landed task_news=1",
        2_000,
    );
    assert!(
        at.open("t7b2xxxxxxx"),
        "a row at the close's rev reopens it"
    );
    let held = at.hold_log();
    at.apply(
        "@t7b2xxxxxxx rev=4 state=closed task_state=stalled agent=claude conclusion=failed \
         why=held task_news=1",
        3_000,
    );
    assert_eq!(at.owes("t7b2xxxxxxx"), (1, Some("ok.unknown".to_owned())));
    drop(held);
    at.board
        .settle(
            &["t7b2xxxxxxx".to_owned(), "t7c3xxxxxxx".to_owned()],
            at.t0 + 9_000,
        )
        .expect("settled");
    assert_eq!(
        at.tasks("t7b2xxxxxxx")
            .iter()
            .map(|r| (r.outcome, r.stop, r.at_ms))
            .collect::<Vec<_>>(),
        [
            (Outcome::Ok, Stop::Unknown, at.t0 + 1_000),
            (Outcome::Fail, Stop::Stalled, at.t0 + 3_000)
        ],
        "its last row is the end it came to"
    );
    assert_eq!(at.owes("t7b2xxxxxxx"), (0, Some("fail.stalled".to_owned())));
    assert_eq!(
        at.tasks("t7c3xxxxxxx")
            .iter()
            .map(|r| (r.outcome, r.at_ms))
            .collect::<Vec<_>>(),
        [(Outcome::Fail, at.t0 + 62_000)]
    );
    assert_eq!(at.owes("t7c3xxxxxxx"), (0, Some("fail.unknown".to_owned())));
    assert_eq!(at.dropped(), 0);
}

/// One apply that reads a sid twice (a close, then the row reopened) merges the second answer
/// into what the first left: the row its close owes stays owed, though the row stands open.
/// NEGATIVE CONTROL: the rows are read and written once, so the reopen is kept too.
#[cfg(unix)]
#[test]
fn a_sid_answered_twice_in_one_apply_keeps_what_its_first_answer_owed() {
    let at = Scratch::new(&["t9a1xxxxxxx"]);
    let held = at.hold_log();
    at.board
        .apply(
            &[
                "@t9a1xxxxxxx rev=3 state=closed task_state=done agent=claude conclusion=finished \
                 why=landed task_news=1"
                    .to_owned(),
                "@t9a1xxxxxxx rev=3 state=done agent=claude conclusion=finished why=landed \
                 task_news=1"
                    .to_owned(),
            ],
            at.t0 + 1_000,
        )
        .expect("applied");
    assert!(at.open("t9a1xxxxxxx"), "the second answer reopened it");
    assert_eq!(at.owes("t9a1xxxxxxx"), (1, None), "the first answer's row");
    drop(held);
    at.board
        .settle(&["t9a1xxxxxxx".to_owned()], at.t0 + 2_000)
        .expect("settled");
    assert_eq!(at.tasks("t9a1xxxxxxx").len(), 1);
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
    let at = Scratch::new(&["t9b1xxxxxxx", "t9b2xxxxxxx"]);
    let close = |sid: &str| {
        format!(
            "@{sid} rev=3 state=closed task_state=exited agent=claude conclusion=finished \
             why=exit-status exit=0 task_news=1"
        )
    };
    let held = at.hold_log();
    at.apply(&close("t9b2xxxxxxx"), 1_000);
    assert_eq!(
        at.owes("t9b2xxxxxxx"),
        (1, None),
        "refused by the log's lock"
    );
    drop(held);
    let fixed = Immutable::set(at.board.dir().join("t9b1xxxxxxx"));
    let answer = at
        .board
        .apply(&[close("t9b1xxxxxxx"), close("t9b2xxxxxxx")], at.t0 + 2_000);
    assert!(answer.is_err(), "the failed write is the answer");
    assert!(at.open("t9b1xxxxxxx"), "its file kept as it was");
    assert_eq!(
        at.owes("t9b2xxxxxxx"),
        (0, Some("ok.exit".to_owned())),
        "the row the append wrote is owed no more"
    );
    drop(fixed);
    at.board
        .settle(&["t9b2xxxxxxx".to_owned()], at.t0 + 3_000)
        .expect("settled");
    at.apply(&close("t9b1xxxxxxx"), 4_000);
    assert!(!at.open("t9b1xxxxxxx"));
    assert_eq!(at.tasks("t9b2xxxxxxx").len(), 1, "written once");
    assert_eq!(
        at.tasks("t9b1xxxxxxx").len(),
        1,
        "told once its row was kept"
    );
    assert_eq!(at.dropped(), 0);
    assert!(at.unsaid().is_empty());
}

/// Hold the lock of the log in `log`, as another writer's long append does, until the guard
/// drops.
#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
#[test]
fn a_row_paying_when_its_write_failed_is_settled_by_what_the_log_holds() {
    use crate::board::WriteFault::Before;
    let at = Scratch::new(&["t9d1xxxxxxx", "t9d2xxxxxxx", "t9d3xxxxxxx", "t9d4xxxxxxx"]);
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
    for sid in ["t9d1xxxxxxx", "t9d2xxxxxxx"] {
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
        .settle(&["t9d1xxxxxxx".to_owned()], at.t0 + 2_000)
        .expect("settled");
    assert_eq!(at.tasks("t9d1xxxxxxx").len(), 1, "written no second time");
    assert_eq!(paying("t9d1xxxxxxx"), (0, 0, Some("ok.exit".to_owned())));
    let held = at.hold_log();
    // Reopened at its close's rev and closed again while paying: the end it pays is its end.
    at.apply(
        "@t9d2xxxxxxx rev=3 state=exited agent=claude conclusion=finished why=exit-status exit=0 \
         task_news=1",
        1_500,
    );
    assert!(at.open("t9d2xxxxxxx"));
    at.apply(&close("t9d2xxxxxxx"), 1_600);
    assert_eq!(
        paying("t9d2xxxxxxx"),
        (0, 1, Some("ok.exit".to_owned())),
        "no second row of its end"
    );
    at.board
        .settle(&["t9d2xxxxxxx".to_owned()], at.t0 + kept)
        .expect("settled");
    assert_eq!(
        paying("t9d2xxxxxxx"),
        (0, 1, Some("ok.exit".to_owned())),
        "kept while unjudged"
    );
    at.board
        .settle(&["t9d2xxxxxxx".to_owned()], at.t0 + 1_000 + kept)
        .expect("settled");
    assert_eq!(
        paying("t9d2xxxxxxx"),
        (0, 0, Some("ok.exit".to_owned())),
        "dropped unsaid, its end told"
    );

    // Refused by the log's lock, and the write after it failed: paying, though not written.
    let faulted = fault_write("t9d3xxxxxxx", 2, Before);
    assert!(
        at.board
            .apply(&[close("t9d3xxxxxxx")], at.t0 + 3_000)
            .is_err()
    );
    drop(faulted);
    assert_eq!(paying("t9d3xxxxxxx"), (0, 1, Some("ok.exit".to_owned())));
    drop(held);
    assert!(at.tasks("t9d3xxxxxxx").is_empty());
    let faulted = fault_write("t9d4xxxxxxx", 1, Before);
    assert!(
        at.board
            .apply(&[close("t9d4xxxxxxx")], at.t0 + 3_000)
            .is_err()
    );
    drop(faulted);
    assert!(at.open("t9d4xxxxxxx"), "its file kept as it was");
    assert!(
        at.tasks("t9d4xxxxxxx").is_empty(),
        "the append wrote none of it"
    );
    // Reopened and closed again once its paying row was dropped unsaid: the end it told.
    at.apply(
        "@t9d2xxxxxxx rev=3 state=exited agent=claude conclusion=finished why=exit-status exit=0 \
         task_news=1",
        kept + 2_000,
    );
    assert!(at.open("t9d2xxxxxxx"));
    at.apply(&close("t9d2xxxxxxx"), kept + 3_000);
    at.board
        .settle(&["t9d3xxxxxxx".to_owned()], at.t0 + 4_000)
        .expect("settled");
    at.apply(&close("t9d4xxxxxxx"), 5_000);
    for sid in ["t9d1xxxxxxx", "t9d2xxxxxxx", "t9d3xxxxxxx", "t9d4xxxxxxx"] {
        assert_eq!(at.tasks(sid).len(), 1, "{sid} written once");
    }
    assert_eq!(paying("t9d3xxxxxxx"), (0, 0, Some("ok.exit".to_owned())));
    assert_eq!(at.dropped(), 0, "none said lost");
    assert!(at.unsaid().is_empty(), "{:?}", at.unsaid());
}

/// A row paying a line a later aterm wrote (a word this aterm's rows do not write) asks the
/// log for it as its file holds it: the log holds it, so it is told and written no second
/// time, though this aterm would write it otherwise. NEGATIVE CONTROL (by hand: the line
/// asked for as this aterm writes it: not found, written again).
#[cfg(unix)]
#[test]
fn a_paying_line_a_later_aterm_wrote_is_asked_for_as_it_was_written() {
    let at = Scratch::new(&["t9f1xxxxxxx"]);
    let mut row = worker("t9f1xxxxxxx", at.t0);
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
        .settle(&["t9f1xxxxxxx".to_owned()], at.t0 + 2_000)
        .expect("settled");
    assert_eq!(at.tasks("t9f1xxxxxxx").len(), 1, "written no second time");
    let row = at.board.row("t9f1xxxxxxx").expect("a row");
    assert_eq!(
        (row.owed.len(), row.paying.len(), row.told),
        (0, 0, Some(told_of(&read)))
    );
}

/// An append that failed and could not be cut back leaves its rows paying, never owing: the
/// next pay asks the log, which holds what the write left, so none is written twice and none
/// said lost. NEGATIVE CONTROL (by hand: such an append taken as refused, its rows owed
/// again: a second row).
#[cfg(unix)]
#[test]
fn an_append_left_uncut_leaves_its_rows_paying_for_the_log_to_settle() {
    let at = Scratch::new(&["t9a7xxxxxxx"]);
    crate::board::pay_says(aterm_telemetry::direct::Kept::Unsure);
    at.apply(
        "@t9a7xxxxxxx rev=3 state=closed task_state=exited agent=claude conclusion=finished \
         why=exit-status exit=0 task_news=1",
        1_000,
    );
    let row = at.board.row("t9a7xxxxxxx").expect("a row");
    assert_eq!((row.owed.len(), row.paying.len()), (0, 1), "paying");
    assert_eq!(at.tasks("t9a7xxxxxxx").len(), 1, "what the write left");
    at.board
        .settle(&["t9a7xxxxxxx".to_owned()], at.t0 + 2_000)
        .expect("settled");
    assert_eq!(at.tasks("t9a7xxxxxxx").len(), 1, "written no second time");
    assert_eq!(at.owes("t9a7xxxxxxx"), (0, Some("ok.exit".to_owned())));
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
#[cfg(unix)]
#[test]
fn a_paying_rows_end_is_told_before_its_append_and_a_settle_keeps_a_newer_one() {
    use crate::board::WriteFault::Before;
    let at = Scratch::new(&["t9b1xxxxxxx"]);
    let faulted = fault_write("t9b1xxxxxxx", 2, Before);
    assert!(
        at.board
            .apply(
                &[
                    "@t9b1xxxxxxx rev=3 state=closed task_state=exited agent=claude \
                   conclusion=finished why=exit-status exit=0 task_news=1"
                        .to_owned()
                ],
                at.t0 + 1_000,
            )
            .is_err(),
        "the write after the append failed"
    );
    drop(faulted);
    let mut row = at.board.row("t9b1xxxxxxx").expect("a row");
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
        "@t9b1xxxxxxx rev=4 state=stalled agent=claude conclusion=failed why=held task_news=1",
        2_000,
    );
    at.apply(
        "@t9b1xxxxxxx rev=5 state=closed task_state=stalled agent=claude conclusion=failed \
         why=held task_news=1",
        3_000,
    );
    assert_eq!(
        at.board.row("t9b1xxxxxxx").expect("a row").told.as_deref(),
        Some("fail.stalled")
    );
    // ... and a reopen at that close's rev, before this aterm reads the row again.
    at.apply(
        "@t9b1xxxxxxx rev=5 state=stalled agent=claude conclusion=failed why=held task_news=1",
        3_500,
    );
    let mut row = at.board.row("t9b1xxxxxxx").expect("a row");
    assert!(row.is_open());
    row.paying = paid;
    row.was_told = was;
    at.board.record(&row).expect("recorded");
    at.apply(
        "@t9b1xxxxxxx rev=5 state=closed task_state=stalled agent=claude conclusion=failed \
         why=held task_news=1",
        4_000,
    );
    let row = at.board.row("t9b1xxxxxxx").expect("a row");
    assert_eq!(
        (row.owed.len(), row.paying.len(), row.told.as_deref()),
        (0, 0, Some("fail.stalled")),
        "no second row of the newer end owed, and the newer end kept"
    );
    at.apply(
        "@t9b1xxxxxxx rev=5 state=stalled agent=claude conclusion=failed why=held task_news=1",
        5_000,
    );
    assert!(
        at.open("t9b1xxxxxxx"),
        "a row at the close's rev reopens it"
    );
    at.apply(
        "@t9b1xxxxxxx rev=5 state=closed task_state=stalled agent=claude conclusion=failed \
         why=held task_news=1",
        6_000,
    );
    assert_eq!(
        at.tasks("t9b1xxxxxxx")
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
#[cfg(unix)]
#[test]
fn a_paying_row_the_log_lacks_past_a_newer_end_told_since_is_given_up() {
    use crate::board::WriteFault::Before;
    let at = Scratch::new(&["t9c1xxxxxxx"]);
    let held = at.hold_log();
    let faulted = fault_write("t9c1xxxxxxx", 2, Before);
    assert!(
        at.board
            .apply(
                &[
                    "@t9c1xxxxxxx rev=3 state=closed task_state=exited agent=claude \
                   conclusion=finished why=exit-status exit=0 task_news=1"
                        .to_owned()
                ],
                at.t0 + 1_000,
            )
            .is_err(),
        "the append refused, and the write after it failed"
    );
    drop(faulted);
    drop(held);
    let mut row = at.board.row("t9c1xxxxxxx").expect("a row");
    assert_eq!(
        (
            row.paying.len(),
            row.told.as_deref(),
            row.was_told.as_deref()
        ),
        (1, Some("ok.exit"), None),
        "paying a row the log does not hold, its end told"
    );
    assert!(at.tasks("t9c1xxxxxxx").is_empty());
    // An older aterm's writes, which leave `paying=` and `was_told=` as they found them: a
    // reopen, and a close to a newer end, which it writes and tells.
    let paid = std::mem::take(&mut row.paying);
    let was = row.was_told.take();
    at.board.record(&row).expect("recorded");
    at.apply(
        "@t9c1xxxxxxx rev=4 state=stalled agent=claude conclusion=failed why=held task_news=1",
        2_000,
    );
    at.apply(
        "@t9c1xxxxxxx rev=5 state=closed task_state=stalled agent=claude conclusion=failed \
         why=held task_news=1",
        3_000,
    );
    let mut row = at.board.row("t9c1xxxxxxx").expect("a row");
    assert_eq!(row.told.as_deref(), Some("fail.stalled"));
    row.paying = paid;
    row.was_told = was;
    at.board.record(&row).expect("recorded");
    at.board
        .settle(&["t9c1xxxxxxx".to_owned()], at.t0 + 4_000)
        .expect("settled");
    let row = at.board.row("t9c1xxxxxxx").expect("a row");
    assert_eq!(
        (row.owed.len(), row.paying.len(), row.told.as_deref()),
        (0, 0, Some("fail.stalled")),
        "given up, the newer end kept"
    );
    assert_eq!(at.unsaid().len(), 1, "the row it gave up, said lost");
    at.apply(
        "@t9c1xxxxxxx rev=5 state=stalled agent=claude conclusion=failed why=held task_news=1",
        5_000,
    );
    assert!(
        at.open("t9c1xxxxxxx"),
        "a row at the close's rev reopens it"
    );
    at.apply(
        "@t9c1xxxxxxx rev=5 state=closed task_state=stalled agent=claude conclusion=failed \
         why=held task_news=1",
        6_000,
    );
    assert_eq!(
        at.tasks("t9c1xxxxxxx")
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
#[cfg(unix)]
#[test]
fn a_paying_row_the_log_lacks_refused_again_tells_the_end_before_it() {
    use crate::board::WriteFault::Before;
    let at = Scratch::new(&["t9c2xxxxxxx"]);
    let close = "@t9c2xxxxxxx rev=3 state=closed task_state=exited agent=claude \
                 conclusion=finished why=exit-status exit=0 task_news=1";
    let held = at.hold_log();
    let faulted = fault_write("t9c2xxxxxxx", 2, Before);
    assert!(at.board.apply(&[close.to_owned()], at.t0 + 1_000).is_err());
    drop((faulted, held));
    assert!(at.tasks("t9c2xxxxxxx").is_empty());
    // The log's lock taken by another writer once the settle's ask found the row absent, at
    // the paying write of its pay.
    let lock = std::rc::Rc::new(std::cell::RefCell::new(None));
    let taken = std::rc::Rc::clone(&lock);
    let log = at.log.clone();
    let faulted = crate::board::fault_writes(move |row| {
        if row.sid == "t9c2xxxxxxx" && !row.paying.is_empty() {
            taken.borrow_mut().get_or_insert_with(|| hold_lock(&log));
        }
        None
    });
    at.board
        .settle(&["t9c2xxxxxxx".to_owned()], at.t0 + 2_000)
        .expect("settled");
    drop(faulted);
    assert!(lock.borrow().is_some(), "the append refused");
    let row = at.board.row("t9c2xxxxxxx").expect("a row");
    assert_eq!(
        (row.owed.len(), row.paying.len(), row.told, row.was_told),
        (1, 0, None, None),
        "owed again, told the end before it"
    );
    let kept = u64::try_from(OWED_KEPT.as_millis()).expect("ms");
    at.board
        .settle(&["t9c2xxxxxxx".to_owned()], at.t0 + 1_000 + kept)
        .expect("settled");
    assert_eq!(at.owes("t9c2xxxxxxx"), (0, None), "given up");
    drop(lock);
    at.apply(
        "@t9c2xxxxxxx rev=3 state=exited agent=claude conclusion=finished why=exit-status exit=0 \
         task_news=1",
        kept + 2_000,
    );
    assert!(at.open("t9c2xxxxxxx"));
    at.apply(close, kept + 3_000);
    assert_eq!(
        at.tasks("t9c2xxxxxxx")
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
#[cfg(unix)]
#[test]
fn a_paying_row_the_log_lacks_tells_again_the_end_the_log_held_before_it() {
    use crate::board::WriteFault::Before;
    let at = Scratch::new(&["t9c3xxxxxxx"]);
    at.apply(
        "@t9c3xxxxxxx rev=3 state=closed task_state=exited agent=claude conclusion=finished \
         why=exit-status exit=0 task_news=1",
        1_000,
    );
    assert_eq!(at.owes("t9c3xxxxxxx"), (0, Some("ok.exit".to_owned())));
    at.apply(
        "@t9c3xxxxxxx rev=4 state=stalled agent=claude conclusion=failed why=held task_news=1",
        2_000,
    );
    assert!(at.open("t9c3xxxxxxx"));
    let held = at.hold_log();
    let faulted = fault_write("t9c3xxxxxxx", 2, Before);
    assert!(
        at.board
            .apply(
                &[
                    "@t9c3xxxxxxx rev=5 state=closed task_state=stalled agent=claude \
                   conclusion=failed why=held task_news=1"
                        .to_owned()
                ],
                at.t0 + 3_000,
            )
            .is_err()
    );
    drop((faulted, held));
    let row = at.board.row("t9c3xxxxxxx").expect("a row");
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
        if row.sid == "t9c3xxxxxxx" && !row.paying.is_empty() {
            taken.borrow_mut().get_or_insert_with(|| hold_lock(&log));
        }
        None
    });
    at.board
        .settle(&["t9c3xxxxxxx".to_owned()], at.t0 + 4_000)
        .expect("settled");
    drop(faulted);
    assert!(lock.borrow().is_some(), "the append refused");
    let row = at.board.row("t9c3xxxxxxx").expect("a row");
    assert_eq!(
        (row.owed.len(), row.paying.len(), row.told, row.was_told),
        (1, 0, Some("ok.exit".to_owned()), None),
        "owed again, told the end the log held before it"
    );
    let kept = u64::try_from(OWED_KEPT.as_millis()).expect("ms");
    at.board
        .settle(&["t9c3xxxxxxx".to_owned()], at.t0 + 3_000 + kept)
        .expect("settled");
    assert_eq!(
        at.owes("t9c3xxxxxxx"),
        (0, Some("ok.exit".to_owned())),
        "given up"
    );
    drop(lock);
    at.apply(
        "@t9c3xxxxxxx rev=5 state=exited agent=claude conclusion=finished why=exit-status exit=0 \
         task_news=1",
        kept + 4_000,
    );
    assert!(at.open("t9c3xxxxxxx"));
    at.apply(
        "@t9c3xxxxxxx rev=6 state=closed task_state=exited agent=claude conclusion=finished \
         why=exit-status exit=0 task_news=1",
        kept + 5_000,
    );
    assert_eq!(
        at.tasks("t9c3xxxxxxx")
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
#[cfg(unix)]
#[test]
fn a_paying_row_dropped_unsaid_beside_one_still_paying_is_told() {
    use crate::board::WriteFault::Before;
    let at = Scratch::new(&["t9c4xxxxxxx"]);
    let kept = u64::try_from(OWED_KEPT.as_millis()).expect("ms");
    at.apply(
        "@t9c4xxxxxxx rev=3 state=closed task_state=exited agent=claude conclusion=finished \
         why=exit-status exit=0 task_news=1",
        1_000,
    );
    at.apply(
        "@t9c4xxxxxxx rev=4 state=stalled agent=claude conclusion=failed why=held task_news=1",
        2_000,
    );
    let held = at.hold_log();
    at.apply(
        "@t9c4xxxxxxx rev=5 state=closed task_state=stalled agent=claude conclusion=failed \
         why=held task_news=1",
        3_000,
    );
    assert_eq!(
        at.owes("t9c4xxxxxxx"),
        (1, Some("ok.exit".to_owned())),
        "A owed"
    );
    let b_at = kept - 60_000;
    at.apply(
        "@t9c4xxxxxxx rev=6 state=working agent=claude task_news=1",
        b_at - 1_000,
    );
    let faulted = fault_write("t9c4xxxxxxx", 2, Before);
    assert!(
        at.board
            .apply(
                &[
                    "@t9c4xxxxxxx rev=7 state=closed task_state=failed agent=claude \
                   conclusion=failed why=could-not-finish task_news=1"
                        .to_owned()
                ],
                at.t0 + b_at,
            )
            .is_err()
    );
    drop((faulted, held));
    let row = at.board.row("t9c4xxxxxxx").expect("a row");
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
        .settle(&["t9c4xxxxxxx".to_owned()], at.t0 + 3_000 + kept + 1_000)
        .expect("settled");
    drop(held);
    let row = at.board.row("t9c4xxxxxxx").expect("a row");
    assert_eq!(
        (row.paying.len(), row.was_told.as_deref()),
        (1, Some("fail.stalled")),
        "A dropped unsaid, its end told before B"
    );
    let lock = std::rc::Rc::new(std::cell::RefCell::new(None));
    let taken = std::rc::Rc::clone(&lock);
    let log = at.log.clone();
    let faulted = crate::board::fault_writes(move |row| {
        if row.sid == "t9c4xxxxxxx" && !row.paying.is_empty() {
            taken.borrow_mut().get_or_insert_with(|| hold_lock(&log));
        }
        None
    });
    at.board
        .settle(&["t9c4xxxxxxx".to_owned()], at.t0 + 3_000 + kept + 2_000)
        .expect("settled");
    drop(faulted);
    assert!(lock.borrow().is_some(), "B's append refused");
    assert_eq!(
        at.owes("t9c4xxxxxxx"),
        (1, Some("fail.stalled".to_owned())),
        "B owed again, told A's end"
    );
    at.board
        .settle(&["t9c4xxxxxxx".to_owned()], at.t0 + b_at + kept)
        .expect("settled");
    assert_eq!(
        at.owes("t9c4xxxxxxx"),
        (0, Some("fail.stalled".to_owned())),
        "B given up"
    );
    drop(lock);
    at.apply(
        "@t9c4xxxxxxx rev=7 state=exited agent=claude conclusion=finished why=exit-status exit=0 \
         task_news=1",
        2 * kept,
    );
    assert!(at.open("t9c4xxxxxxx"));
    at.apply(
        "@t9c4xxxxxxx rev=8 state=closed task_state=exited agent=claude conclusion=finished \
         why=exit-status exit=0 task_news=1",
        2 * kept + 1_000,
    );
    assert_eq!(
        at.tasks("t9c4xxxxxxx")
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
#[cfg(unix)]
#[test]
fn a_write_its_rename_published_says_what_its_row_gave_up() {
    use crate::board::WriteFault::{After, Before};
    let at = Scratch::new(&["t9e1xxxxxxx", "t9e2xxxxxxx"]);
    let given_up = |sid: &str| {
        let mut row = worker(sid, at.t0);
        let mut build = task_row(&row, None, at.t0, "m101", Digest::dir("/facts"));
        build.kind = Kind::Build;
        row.owed = vec![build.line()];
        at.board.record(&row).expect("recorded");
    };
    given_up("t9e1xxxxxxx");
    let faulted = fault_write("t9e1xxxxxxx", 1, Before);
    assert!(at.board.settle(&["t9e1xxxxxxx".to_owned()], at.t0).is_err());
    drop(faulted);
    assert_eq!(at.owes("t9e1xxxxxxx").0, 1, "its file as it was");
    assert!(at.unsaid().is_empty(), "nothing said");
    let faulted = fault_write("t9e1xxxxxxx", 1, After);
    at.board
        .settle(&["t9e1xxxxxxx".to_owned()], at.t0)
        .expect("its row published");
    drop(faulted);
    assert_eq!(at.owes("t9e1xxxxxxx").0, 0);
    assert_eq!(at.unsaid().len(), 1, "the line it gave up, said lost");

    let held = at.hold_log();
    at.apply(
        "@t9e2xxxxxxx rev=3 state=closed task_state=done agent=claude conclusion=finished \
         why=landed task_news=1",
        1_000,
    );
    drop(held);
    let faulted = crate::board::fault_writes(|row| (row.sid == "t9e2xxxxxxx").then_some(After));
    at.board
        .settle(&["t9e2xxxxxxx".to_owned()], at.t0 + 2_000)
        .expect("each write published");
    drop(faulted);
    assert_eq!(at.tasks("t9e2xxxxxxx").len(), 1);
    assert_eq!(at.owes("t9e2xxxxxxx"), (0, Some("ok.unknown".to_owned())));
}

/// A task row its board gives up is said lost exactly once: a write that fails
/// [`OWED_KEPT`] after its close (and not one a second before it), its board row pruned or
/// written over, a fifth row owed past [`OWED_MAX`] (the oldest), or a line that holds no
/// task row. NEGATIVE CONTROLS: the failed writes of other rows after each was given up, a
/// later prune and later appends say none of them again; the rows still owed are written, in
/// order.
#[cfg(unix)]
#[test]
fn a_task_row_its_board_gives_up_is_said_lost_exactly_once() {
    let at = Scratch::new(&[
        "t8a1xxxxxxx",
        "t8b2xxxxxxx",
        "t8c3xxxxxxx",
        "t8d4xxxxxxx",
        "t8e5xxxxxxx",
        "t8f6xxxxxxx",
    ]);
    let record = |sid: &str| {
        format!(
            "@{sid} rev=3 state=closed task_state=done agent=claude conclusion=finished \
             why=landed task_news=1"
        )
    };
    let kept = u64::try_from(OWED_KEPT.as_millis()).expect("ms");
    let held = at.hold_log();
    at.apply(&record("t8a1xxxxxxx"), 1_000);
    at.apply(&record("t8b2xxxxxxx"), 1_000);
    at.apply(&record("t8a1xxxxxxx"), kept);
    assert_eq!(
        at.owes("t8a1xxxxxxx").0,
        1,
        "a write that fails within its time"
    );
    at.apply(&record("t8a1xxxxxxx"), 1_000 + kept);
    assert_eq!(
        at.owes("t8a1xxxxxxx"),
        (0, None),
        "retries exhausted: given up"
    );
    // Its board row written over (a row recorded again for its sid).
    at.apply(&record("t8e5xxxxxxx"), 1_000);
    assert_eq!(at.owes("t8e5xxxxxxx").0, 1);
    at.board
        .record(&worker("t8e5xxxxxxx", at.t0))
        .expect("recorded over");
    assert_eq!(at.owes("t8e5xxxxxxx"), (0, None));
    // A line that holds no task row (a build's), given up at its first write.
    let mut no_task = worker("t8f6xxxxxxx", at.t0);
    let mut build = task_row(&no_task, None, at.t0, "m101", Digest::dir("/facts"));
    build.kind = Kind::Build;
    no_task.owed = vec![build.line()];
    at.board.record(&no_task).expect("recorded");
    // Its board row pruned, a week after it closed.
    at.board.prune_at(at.t0 + 8 * 24 * 3_600_000);
    assert!(at.board.row("t8b2xxxxxxx").is_none(), "pruned");
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
            &format!("@t8c3xxxxxxx rev={rev} state=closed {end} agent=claude task_news=1"),
            10_000 + 2 * n,
        );
        at.apply(
            &format!("@t8c3xxxxxxx rev={rev} state=working agent=claude task_news=1"),
            10_001 + 2 * n,
        );
    }
    assert_eq!(at.owes("t8c3xxxxxxx").0, OWED_MAX);
    drop(held);
    at.board
        .settle(
            &["t8c3xxxxxxx".to_owned(), "t8f6xxxxxxx".to_owned()],
            at.t0 + 20_000,
        )
        .expect("settled");
    assert_eq!(at.owes("t8f6xxxxxxx"), (0, None));
    assert_eq!(
        (at.dropped(), at.unsaid().len()),
        (4, 1),
        "each row given up before this settle said once by its append, and the line it gave \
         up left for the next"
    );
    assert!(
        ["t8a1xxxxxxx", "t8b2xxxxxxx", "t8e5xxxxxxx", "t8f6xxxxxxx"]
            .iter()
            .all(|sid| at.tasks(sid).is_empty())
    );
    assert_eq!(
        at.tasks("t8c3xxxxxxx")
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
    at.apply(&record("t8a1xxxxxxx"), 30_000);
    at.apply(&record("t8d4xxxxxxx"), 40_000);
    at.board.prune_at(at.t0 + 10 * 24 * 3_600_000);
    assert_eq!(at.tasks("t8d4xxxxxxx").len(), 1);
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
#[cfg(unix)]
#[test]
fn a_worker_writes_its_task_row_once_it_closed_and_its_macs_conclusion_was_heard() {
    let at = Scratch::new(&["t0a1xxxxxxx", "t0b2xxxxxxx", "t0c3xxxxxxx", "t0d4xxxxxxx"]);

    // A conclusion heard while open, then the keeper's close of a tab its Mac no longer hosts.
    at.apply(
        "@t0a1xxxxxxx rev=2 state=working agent=claude task_news=0",
        1_000,
    );
    at.apply("@t0a1xxxxxxx rev=2 state=closed agent=-", 2_000);
    assert!(!at.open("t0a1xxxxxxx"));
    assert!(
        at.tasks("t0a1xxxxxxx").is_empty(),
        "a close with no conclusion of its Mac's writes none"
    );
    at.apply(
        "@t0a1xxxxxxx rev=2 state=working agent=claude task_news=0",
        3_000,
    );
    assert!(
        at.open("t0a1xxxxxxx"),
        "the worker's own answer reopens the row"
    );
    at.apply(
        "@t0a1xxxxxxx rev=3 state=done agent=claude conclusion=finished why=landed task_news=1",
        4_000,
    );
    assert!(
        at.tasks("t0a1xxxxxxx").is_empty(),
        "an open row writes none"
    );
    at.apply("@t0a1xxxxxxx rev=0 state=closed agent=-", 5_000);
    let rows = at.tasks("t0a1xxxxxxx");
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
        "@t0a1xxxxxxx rev=3 state=closed agent=claude conclusion=finished why=landed task_news=1";
    at.apply(record, 6_000);
    at.apply(
        "@t0a1xxxxxxx rev=3 state=done agent=claude conclusion=finished why=landed task_news=1",
        7_000,
    );
    assert!(
        at.open("t0a1xxxxxxx"),
        "a row at the close's rev reopens it"
    );
    at.apply(record, 8_000);
    assert!(!at.open("t0a1xxxxxxx"));
    assert_eq!(at.tasks("t0a1xxxxxxx").len(), 1, "one row of one end");

    // The Mac's record of the close, never heard concluded before: its words and exit code.
    at.apply(
        "@t0b2xxxxxxx rev=4 state=closed task_state=exited agent=claude conclusion=failed \
         why=exit-status exit=3 task_news=1",
        2_000,
    );
    let rows = at.tasks("t0b2xxxxxxx");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        (rows[0].outcome, rows[0].stop, rows[0].code),
        (Outcome::Fail, Stop::Exit, Some(3))
    );

    // A wait's close built from the Mac's last row (`closed_row`), the worker still working:
    // it carries the Mac's rev and `task_news=`, and no conclusion of the Mac's.
    at.apply(
        "@t0c3xxxxxxx rev=1 state=working agent=claude task_news=0",
        1_000,
    );
    at.apply(
        "@t0c3xxxxxxx rev=1 state=closed agent=claude task_news=1",
        2_000,
    );
    assert!(!at.open("t0c3xxxxxxx"));
    assert!(
        at.tasks("t0c3xxxxxxx").is_empty(),
        "the board's `failed` for a gone tab is no end its Mac reported"
    );
    // Then that Mac's record of the close, naming its conclusion: the record writes.
    at.apply(
        "@t0c3xxxxxxx rev=2 state=closed agent=claude conclusion=finished why=landed task_news=1",
        3_000,
    );
    let rows = at.tasks("t0c3xxxxxxx");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].outcome, Outcome::Ok);

    // A stalled worker (open, its Mac's conclusion `failed`) read gone, as a Mac with no window
    // open makes every worker; its tab comes back, it finishes, and closes for good.
    at.apply(
        "@t0d4xxxxxxx rev=2 state=stalled agent=claude conclusion=failed why=held task_news=1",
        1_000,
    );
    at.apply("@t0d4xxxxxxx rev=0 state=closed agent=-", 2_000);
    let rows = at.tasks("t0d4xxxxxxx");
    assert_eq!(
        rows.iter().map(|r| (r.outcome, r.stop)).collect::<Vec<_>>(),
        [(Outcome::Fail, Stop::Stalled)],
        "the end the board read"
    );
    at.apply(
        "@t0d4xxxxxxx rev=3 state=working agent=claude task_news=1",
        3_000,
    );
    assert!(at.open("t0d4xxxxxxx"), "its tab came back");
    at.apply(
        "@t0d4xxxxxxx rev=4 state=done agent=claude conclusion=finished why=landed task_news=1",
        4_000,
    );
    at.apply("@t0d4xxxxxxx rev=0 state=closed agent=-", 5_000);
    let rows = at.tasks("t0d4xxxxxxx");
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
#[cfg(unix)]
#[test]
fn a_macs_reading_of_a_closed_working_tab_is_no_end_its_task_came_to() {
    let at = Scratch::new(&[
        "t1a1xxxxxxx",
        "t1b2xxxxxxx",
        "t1c3xxxxxxx",
        "t1d4xxxxxxx",
        "t1e5xxxxxxx",
    ]);

    // The Mac's own closed row of a working tab, the keeper's leg parked when it closed.
    at.apply(
        "@t1a1xxxxxxx rev=3 state=working agent=claude task_news=0",
        1_000,
    );
    at.apply(
        "@t1a1xxxxxxx rev=3 state=closed task_state=working agent=claude conclusion=failed \
         task_news=1",
        2_000,
    );
    assert!(!at.open("t1a1xxxxxxx"));
    assert!(
        at.tasks("t1a1xxxxxxx").is_empty(),
        "the closed tab's reading is no end: {:?}",
        at.tasks("t1a1xxxxxxx")
    );
    // The same close after the record came to its end: the row.
    at.apply(
        "@t1b2xxxxxxx rev=3 state=closed task_state=failed agent=claude conclusion=failed \
         why=could-not-finish task_news=1",
        2_000,
    );
    assert_eq!(
        at.tasks("t1b2xxxxxxx")
            .iter()
            .map(|r| (r.outcome, r.stop))
            .collect::<Vec<_>>(),
        [(Outcome::Fail, Stop::Unknown)]
    );
    // A Mac too old to say `task_state=`: `failed` is what it says of every closed tab.
    at.apply(
        "@t1c3xxxxxxx rev=3 state=closed agent=claude conclusion=failed task_news=1",
        2_000,
    );
    assert!(
        at.tasks("t1c3xxxxxxx").is_empty(),
        "{:?}",
        at.tasks("t1c3xxxxxxx")
    );
    at.apply(
        "@t1d4xxxxxxx rev=3 state=closed agent=claude conclusion=finished why=landed task_news=1",
        2_000,
    );
    assert_eq!(
        at.tasks("t1d4xxxxxxx").len(),
        1,
        "a record's `finished` is its end"
    );
    // An answer below the board's rev is no record of the Mac's last row.
    at.apply(
        "@t1e5xxxxxxx rev=5 state=working agent=claude task_news=0",
        1_000,
    );
    at.apply(
        "@t1e5xxxxxxx rev=4 state=closed task_state=done agent=claude conclusion=finished \
         why=landed task_news=1",
        2_000,
    );
    assert!(!at.open("t1e5xxxxxxx"), "the close is taken");
    assert!(
        at.tasks("t1e5xxxxxxx").is_empty(),
        "{:?}",
        at.tasks("t1e5xxxxxxx")
    );
}

/// A conclusion other than a stall's (`a_stall_its_record_took_back_is_no_end`) the board
/// heard while the row was open is the worker's end whatever close with no record of another
/// end follows: the Mac's own close of a tab working again (a turn
/// after it finished), an old Mac's `failed` with no `task_state=`, or a close with no record
/// of the Mac's (the keeper's), in either order, and a live row of the turn read before or
/// after either. The close's words, a closed tab's reading, are never its outcome or stop.
/// NEGATIVE CONTROL: a record close naming a concluding state is its end, though the board
/// heard another (`t5e5xxxxxxx`), and what a closed row heard is no later close's end (`t5f6xxxxxxx`).
#[cfg(unix)]
#[test]
fn a_heard_conclusion_is_the_end_whatever_close_follows_and_in_either_order() {
    let at = Scratch::new(&[
        "t4a1xxxxxxx",
        "t4b2xxxxxxx",
        "t4c3xxxxxxx",
        "t4e5xxxxxxx",
        "t5a1xxxxxxx",
        "t5b2xxxxxxx",
        "t5e5xxxxxxx",
        "t5f6xxxxxxx",
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
    at.apply(&finished("t4a1xxxxxxx"), 1_000);
    at.apply(&macs("t4a1xxxxxxx"), 2_000);
    at.apply(&keepers("t4a1xxxxxxx"), 3_000);
    at.apply(&finished("t4b2xxxxxxx"), 1_000);
    at.apply(&keepers("t4b2xxxxxxx"), 2_000);
    at.apply(&macs("t4b2xxxxxxx"), 3_000);
    assert_eq!(ends("t4a1xxxxxxx"), [(Outcome::Ok, Stop::Unknown)]);
    assert_eq!(
        ends("t4b2xxxxxxx"),
        ends("t4a1xxxxxxx"),
        "either order, one end"
    );
    // A Mac too old to say `task_state=`: its closed tab's `failed` is not the end.
    at.apply(&finished("t4c3xxxxxxx"), 1_000);
    at.apply(
        "@t4c3xxxxxxx rev=3 state=closed agent=claude conclusion=failed why=turn task_news=1",
        2_000,
    );
    assert_eq!(ends("t4c3xxxxxxx"), [(Outcome::Ok, Stop::Unknown)]);
    // A wait's close built from a row older than the board's: the heard end stands.
    at.apply(
        "@t4e5xxxxxxx rev=3 state=done agent=claude conclusion=finished why=landed task_news=1",
        1_000,
    );
    at.apply(
        "@t4e5xxxxxxx rev=2 state=closed task_state=working agent=claude task_news=1",
        2_000,
    );
    assert_eq!(ends("t4e5xxxxxxx"), [(Outcome::Ok, Stop::Unknown)]);
    // A live row of the turn typed after it (a wait's leg answered while it works), read
    // before the Mac's close, and after it with the keeper's close next.
    let turn = |sid: &str| format!("@{sid} rev=3 state=working agent=claude task_news=0");
    at.apply(&finished("t5a1xxxxxxx"), 1_000);
    at.apply(&turn("t5a1xxxxxxx"), 2_000);
    at.apply(&macs("t5a1xxxxxxx"), 3_000);
    at.apply(&finished("t5b2xxxxxxx"), 1_000);
    at.apply(&macs("t5b2xxxxxxx"), 2_000);
    at.apply(&turn("t5b2xxxxxxx"), 3_000);
    assert!(
        at.open("t5b2xxxxxxx"),
        "a live row at the close's rev reopens it"
    );
    at.apply(&keepers("t5b2xxxxxxx"), 4_000);
    assert_eq!(ends("t5a1xxxxxxx"), [(Outcome::Ok, Stop::Unknown)]);
    assert_eq!(
        ends("t5b2xxxxxxx"),
        ends("t5a1xxxxxxx"),
        "either order, one end"
    );
    // NEGATIVE CONTROL: the Mac's record of another end is the end.
    at.apply(&finished("t5e5xxxxxxx"), 1_000);
    at.apply(
        "@t5e5xxxxxxx rev=3 state=closed task_state=stalled agent=claude conclusion=failed \
         why=held task_news=1",
        2_000,
    );
    assert_eq!(ends("t5e5xxxxxxx"), [(Outcome::Fail, Stop::Stalled)]);
    // NEGATIVE CONTROL: a failure heard (an Autopilot set to off), a turn, then the Mac's
    // record that it finished, then a turn after that closed while working: the failure is
    // no end of the second close.
    at.apply(
        "@t5f6xxxxxxx rev=2 state=failed agent=claude conclusion=failed why=off task_news=1",
        1_000,
    );
    at.apply(&turn("t5f6xxxxxxx"), 2_000);
    at.apply(
        "@t5f6xxxxxxx rev=4 state=closed task_state=done agent=claude conclusion=finished \
         why=landed task_news=1",
        3_000,
    );
    at.apply(
        "@t5f6xxxxxxx rev=5 state=working agent=claude task_news=0",
        4_000,
    );
    assert!(at.open("t5f6xxxxxxx"), "the turn reopens it");
    at.apply(
        "@t5f6xxxxxxx rev=5 state=closed task_state=working agent=claude task_news=1",
        5_000,
    );
    assert_eq!(ends("t5f6xxxxxxx"), [(Outcome::Ok, Stop::Unknown)]);
}

/// A stall's `failed` stands only while the stall does: its record takes it back when the
/// stall moves on (the agent started after all, a held stall made progress), with no turn. A
/// stall the board heard and then read moving on, before the Mac's close of the tab working or
/// after it, is no end; an end heard before the stall still is. NEGATIVE CONTROL: a stall that
/// stood when a recordless close came is the end the board last heard (`t6d4xxxxxxx`).
#[cfg(unix)]
#[test]
fn a_stall_its_record_took_back_is_no_end() {
    let at = Scratch::new(&["t6a1xxxxxxx", "t6b2xxxxxxx", "t6c3xxxxxxx", "t6d4xxxxxxx"]);
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
    at.apply(&stalled("t6a1xxxxxxx", 2), 1_000);
    at.apply(&moved_on("t6a1xxxxxxx", 3), 2_000);
    at.apply(&gone("t6a1xxxxxxx", 3), 3_000);
    assert!(ends("t6a1xxxxxxx").is_empty(), "{:?}", ends("t6a1xxxxxxx"));
    // The Mac's close first, then the stall moving on and the keeper's close.
    at.apply(&stalled("t6b2xxxxxxx", 2), 1_000);
    at.apply(&gone("t6b2xxxxxxx", 3), 2_000);
    at.apply(&moved_on("t6b2xxxxxxx", 3), 3_000);
    at.apply(&keepers("t6b2xxxxxxx"), 4_000);
    assert!(ends("t6b2xxxxxxx").is_empty(), "{:?}", ends("t6b2xxxxxxx"));
    // It finished, was typed into, stalled and moved on, then closed while working: it
    // finished.
    at.apply(
        "@t6c3xxxxxxx rev=2 state=done agent=claude conclusion=finished why=landed task_news=1",
        1_000,
    );
    at.apply(&moved_on("t6c3xxxxxxx", 3), 2_000);
    at.apply(&stalled("t6c3xxxxxxx", 4), 3_000);
    at.apply(&moved_on("t6c3xxxxxxx", 5), 4_000);
    at.apply(&gone("t6c3xxxxxxx", 5), 5_000);
    assert_eq!(ends("t6c3xxxxxxx"), [(Outcome::Ok, Stop::Unknown)]);
    // NEGATIVE CONTROL: the stall stood as far as the board heard.
    at.apply(&stalled("t6d4xxxxxxx", 2), 1_000);
    at.apply(&keepers("t6d4xxxxxxx"), 2_000);
    assert_eq!(ends("t6d4xxxxxxx"), [(Outcome::Fail, Stop::Stalled)]);
}

/// A record close that names no `why=` says no stop: the `why=held` the board kept from an
/// earlier report (the worker stalled, then moved on) is not why this end came.
#[cfg(unix)]
#[test]
fn a_record_close_with_no_why_takes_no_stop_from_an_earlier_report() {
    let at = Scratch::new(&["t3a1xxxxxxx"]);
    at.apply(
        "@t3a1xxxxxxx rev=2 state=stalled agent=claude conclusion=failed why=held task_news=1",
        1_000,
    );
    at.apply(
        "@t3a1xxxxxxx rev=3 state=closed task_state=failed agent=claude conclusion=failed \
         task_news=1",
        2_000,
    );
    assert_eq!(
        at.tasks("t3a1xxxxxxx")
            .iter()
            .map(|r| (r.outcome, r.stop))
            .collect::<Vec<_>>(),
        [(Outcome::Fail, Stop::Unknown)]
    );
}

/// The board keeps the end a worker's last task row told, so no read of the log decides a
/// second: a log rotated or removed between two closes of one end writes no second row.
/// NEGATIVE CONTROL: a close that reversed and came to another end writes it.
#[cfg(unix)]
#[test]
fn the_board_keeps_the_end_its_last_task_row_told_past_the_log() {
    let at = Scratch::new(&["t2a1xxxxxxx"]);
    let record = "@t2a1xxxxxxx rev=3 state=closed task_state=done agent=claude conclusion=finished \
                  why=landed task_news=1";
    at.apply(record, 1_000);
    assert_eq!(at.tasks("t2a1xxxxxxx").len(), 1);
    assert_eq!(
        at.board.row("t2a1xxxxxxx").and_then(|r| r.told).as_deref(),
        Some("ok.unknown")
    );
    std::fs::remove_dir_all(&at.log).expect("the log removed");
    at.apply(
        "@t2a1xxxxxxx rev=3 state=done agent=claude conclusion=finished why=landed task_news=1",
        2_000,
    );
    assert!(
        at.open("t2a1xxxxxxx"),
        "a row at the close's rev reopens it"
    );
    at.apply(record, 3_000);
    assert!(!at.open("t2a1xxxxxxx"));
    assert!(
        at.tasks("t2a1xxxxxxx").is_empty(),
        "no second row of one end: {:?}",
        at.tasks("t2a1xxxxxxx")
    );
    at.apply(
        "@t2a1xxxxxxx rev=4 state=done agent=claude conclusion=finished why=landed task_news=1",
        4_000,
    );
    at.apply(
        "@t2a1xxxxxxx rev=5 state=closed task_state=stalled agent=claude conclusion=failed \
         why=held task_news=1",
        5_000,
    );
    assert_eq!(
        at.tasks("t2a1xxxxxxx")
            .iter()
            .map(|r| (r.outcome, r.stop))
            .collect::<Vec<_>>(),
        [(Outcome::Fail, Stop::Stalled)],
        "the other end it came to"
    );
    assert_eq!(
        at.board.row("t2a1xxxxxxx").and_then(|r| r.told).as_deref(),
        Some("fail.stalled")
    );
}

#[test]
fn a_spawn_row_names_its_worker_and_the_mac_it_was_sent_to() {
    let args: Vec<String> = ["agent=claude", "label=dx", "cwd=/Users//me/secret-project"]
        .map(str::to_owned)
        .into();
    let facts = Digest::dir("/facts");
    let sent: Dialled<()> = Ok(Ok(((), Some("t0a1xxxxxxx".to_owned()))));
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
            Some("t0a1xxxxxxx"),
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
        "t0a1xxxxxxx",
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
