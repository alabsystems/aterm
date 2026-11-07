// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FLEET ROWS (Continuum TM-1, `aterm_telemetry`): what a Mac that spawns workers over the
//! fleet writes of them in its own telemetry log.
//!
//! - `kind=spawn`, once a fleet spawn's dial and exchange end, refused or lost too
//!   ([`spawned`]): `sid=` the worker the answer named ([`crate::spawned_sid_from_answer`]), `on=`
//!   the node id of the Mac it was sent to, `wall_ms` from the dial to the answer. A dial that
//!   failed, or an answer naming no worker, is `refused`; an exchange lost after its head was
//!   sent `unknown`, since its task may have reached that Mac. The receiving Mac writes its own
//!   spawn row, with no `on=` ([`Spawn::row`] is that row's shape too).
//! - `kind=task`, when a worker's board row closes and the board holds the worker's conclusion
//!   as its Mac reported it ([`task_end`], from [`crate::board::Board::apply`], in whichever
//!   process applied the close): the close is built from that Mac's last row and names a
//!   conclusion its record came to (`task_state=` other than `working`, or `finished`), or the
//!   Mac reported one while the row was open, which is then the row's end whatever the close
//!   says. A close with neither writes nothing: the board's
//!   `failed` there, and a Mac's own `conclusion=failed` for a tab closed while still `working`,
//!   is the reading of whoever closed a gone tab, not an end its task came to. A worker writes
//!   one, and another only where a close reversed and it came to another end: the board row keeps
//!   the end its last row told (`told=`), so its last task row is its end. `outcome` is the
//!   conclusion (`finished` ok, `failed` fail), `stop` its `why=` (`exit` or `exit-status` exit, a
//!   stall stalled, any other word unknown), `code` the record's `exit=` (none from an earlier
//!   report), and `wall_ms` the time from its spawn to when this Mac's board took the close. A
//!   board row is any worker this Mac's board holds, spawned here or over the fleet; `on=` is its
//!   Mac's node, none for a worker here.
//!
//! No row holds a task's text, a label or a path: a key is a digest of the agent or the task's
//! own digest and its folder, the label one word (the agent, or the program a worker runs).
//! Neither row has a cache tier, an admission wait, CPU or memory of its own (`na`).

use aterm_telemetry::{Digest, Fig, Kind, Meter, Outcome, Row, Stop};

/// A worker spawn's row ([`Self::row`]): the sender's after a fleet spawn, or the receiver's.
#[derive(Clone, Copy, Debug)]
pub struct Spawn<'a> {
    /// The agent it starts (`claude`): `shell` for a plain shell's spawn (the receiving Mac's
    /// row; a fleet spawn of one writes no sender's row), `-` where none was named.
    pub agent: &'a str,
    /// Its working folder, where the spawn named one: digested.
    pub cwd: Option<&'a str>,
    /// How long the spawn took, ms.
    pub wall_ms: u64,
    /// `ok` once a worker was started, `refused` where it was not (the dial failed, or the
    /// answer named none), `unknown` where the exchange was lost after its head was sent.
    pub outcome: Outcome,
    /// The worker's sid, where the answer named one.
    pub sid: Option<&'a str>,
    /// The node id of the Mac it was sent to (the sender's row alone).
    pub on: Option<&'a str>,
}

impl Spawn<'_> {
    /// The `kind=spawn` row, written at `at_ms` on `host` with facts `facts`.
    #[must_use]
    pub fn row(&self, at_ms: u64, host: &str, facts: Digest) -> Row {
        Row {
            at_ms,
            kind: Kind::Spawn,
            key: Some(Digest::key(&Kind::Spawn, self.agent, self.cwd)),
            dir: self.cwd.map(Digest::dir),
            label: aterm_telemetry::row::label(self.agent),
            sid: self.sid.map(str::to_owned),
            on: self.on.map(str::to_owned),
            host: aterm_telemetry::row::host_word(host),
            facts,
            wall_ms: Fig::Known(self.wall_ms),
            cpu_ms: Fig::Na,
            peak_bytes: Fig::Na,
            wait_ms: Fig::Na,
            cache: Fig::Na,
            outcome: self.outcome,
            stop: if self.outcome == Outcome::Ok {
                Stop::Exit
            } else {
                Stop::Unknown
            },
            code: None,
            sig: None,
            meter: Meter::Stamped,
        }
    }
}

/// A spawn's `outcome` by its answer: `ok` where it named the worker's sid, `refused` where it
/// answered without one, `unknown` where none came (`sid` is `Err`): the task may have reached
/// the other Mac before the exchange was cut.
#[must_use]
pub fn spawn_outcome<T, E>(sid: Result<&Option<T>, E>) -> Outcome {
    match sid {
        Ok(Some(_)) => Outcome::Ok,
        Ok(None) => Outcome::Refused,
        Err(_) => Outcome::Unknown,
    }
}

/// A fleet spawn's dial and exchange: `Err` where the dial failed (nothing was sent), else the
/// exchange's result (its exit code and the sid its answer named; `Err` where it was lost).
pub(crate) type Dialled<C> = std::io::Result<std::io::Result<(C, Option<String>)>>;

/// After a fleet spawn's dial and exchange: write the sender's row of the worker `args` asked
/// for (`agent=`, `cwd=`), dialled at `began` to the node `on` and answered `exchanged`.
pub(crate) fn spawned<C>(
    args: &[String],
    began: std::time::Instant,
    exchanged: &Dialled<C>,
    on: Option<&str>,
) {
    let Some(dir) = aterm_telemetry::log::default_dir() else {
        return;
    };
    let wall_ms = u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX);
    let (facts, digest) = aterm_telemetry::direct::facts();
    let row =
        spawn_of(args, wall_ms, exchanged, on).row(aterm_telemetry::now_ms(), &facts.host, *digest);
    let _ = aterm_telemetry::direct::write_now(dir, &row);
}

/// [`spawned`]'s spawn, `wall_ms` long. Its words are read as the board reads them
/// (`board::spawned`): the last of a key, its quotes taken off, an empty one none; so its
/// folder's digest is the one its task row has.
fn spawn_of<'a, C>(
    args: &'a [String],
    wall_ms: u64,
    exchanged: &'a Dialled<C>,
    on: Option<&'a str>,
) -> Spawn<'a> {
    let arg = |key: &str| {
        args.iter()
            .rev()
            .find_map(|a| a.strip_prefix(key)?.strip_prefix('='))
            .map(|v| {
                v.strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .unwrap_or(v)
            })
            .filter(|v| !v.is_empty())
    };
    let (outcome, sid) = match exchanged {
        Err(_) => (Outcome::Refused, None),
        Ok(exchange) => {
            let sid = exchange.as_ref().map(|(_, sid)| sid);
            (spawn_outcome(sid), sid.ok().and_then(Option::as_deref))
        }
    };
    Spawn {
        agent: arg("agent").unwrap_or("-"),
        cwd: arg("cwd"),
        wall_ms,
        outcome,
        sid,
        on,
    }
}

/// The `kind=task` row of the worker `row`, closed, read at `now_ms`. Where the close is its
/// Mac's own record (`record`), its `stop` and `code` are that record's `why=` and `exit=`, none
/// where it says none: the board keeps an earlier report's `why=` past a close that names none,
/// which is no stop this end came to.
#[must_use]
pub(crate) fn task_row(
    row: &crate::board::Row,
    record: Option<&str>,
    now_ms: u64,
    host: &str,
    facts: Digest,
) -> Row {
    let outcome = match row.conclusion.as_deref() {
        Some("finished") => Outcome::Ok,
        Some("failed") => Outcome::Fail,
        _ => Outcome::Unknown,
    };
    let why = match record {
        Some(seen) => {
            crate::await_task::row_field(seen, "why").filter(|w| !w.is_empty() && *w != "-")
        }
        None => row.why.as_deref(),
    };
    let stop = match why {
        Some("exit" | "exit-status") => Stop::Exit,
        Some(why) if aterm_types::control_verbs::is_stall_why(why) => Stop::Stalled,
        _ => Stop::Unknown,
    };
    let code = record
        .and_then(|seen| crate::await_task::row_field(seen, "exit"))
        .and_then(|c| c.parse().ok());
    let label = row.program.as_deref().unwrap_or("-");
    Row {
        at_ms: now_ms,
        kind: Kind::Task,
        key: row
            .brief
            .as_deref()
            .map(|brief| Digest::key(&Kind::Task, brief, row.cwd.as_deref())),
        dir: row.cwd.as_deref().map(Digest::dir),
        label: aterm_telemetry::row::label(label),
        sid: Some(row.sid.clone()),
        on: row.node.clone(),
        host: aterm_telemetry::row::host_word(host),
        facts,
        wall_ms: Fig::Known(now_ms.saturating_sub(row.started_ms)),
        cpu_ms: Fig::Na,
        peak_bytes: Fig::Na,
        wait_ms: Fig::Na,
        cache: Fig::Na,
        outcome,
        stop,
        code,
        sig: None,
        meter: Meter::Stamped,
    }
}

/// The task row a board's close owes the log ([`task_end`]), and the end it tells.
pub(crate) struct TaskEnd {
    dir: std::path::PathBuf,
    row: Row,
    /// `<outcome>.<stop>`: what the board keeps of it ([`crate::board::Row::told`]).
    pub(crate) told: String,
}

impl TaskEnd {
    /// Append the row to its log ([`aterm_telemetry::direct::write_now`]): whether it was written.
    pub(crate) fn write(&self) -> bool {
        aterm_telemetry::direct::write_now(self.dir.clone(), &self.row)
    }
}

/// The board in `board_dir` merged `seen` into `was`, changing it to `row`, at `now_ms`: where
/// `row` is closed and the board holds its worker's conclusion as its Mac reported it, the
/// worker's task row, for the log of the state folder the board sits in (`<state>/board`,
/// [`crate::board::Board::in_state`]). A board elsewhere has no state of its own and writes none.
///
/// The conclusion is its Mac's where `seen` is a close built from that Mac's last row (`task_news=`
/// at the board's rev or above: a row the Mac wrote, or a wait's `closed_row` of the last one it
/// answered) and names a conclusion its record came to, or where `was` was open and held one (an
/// open row's conclusion is only ever the last its Mac reported). That heard conclusion and its
/// `why=` are then the row's end, never the words the close merged over them: so a close of a tab
/// working again (a turn after it finished), from any Mac, and a recordless close, in either
/// order, write the same end. A Mac's close says its record's
/// own state (`task_state=`): `finished` is only ever a record's, but `failed` is also what a Mac
/// answers for a tab closed on a task still `working`, which is that close's reading and no end
/// its task came to. So a close's `failed` counts only beside a `task_state=` other than
/// `working`, and a close from a Mac too old to say one counts only for `finished`. A close with
/// neither writes none: the board's `failed` for a tab gone with no conclusion is the reading of
/// whoever closed it, never the Mac's.
///
/// A close is the board's reading of a tab it no longer finds, which a later answer can reverse
/// ([`crate::board::Row::merge`] reopens the row). So a worker writes a row again only where its
/// end changed: none where the board row says it told the same outcome and stop
/// ([`crate::board::Row::told`]), which the board keeps under its lock with the row, past every
/// rotation of the log. A worker's last task row is its end. One that closed once, the usual flow,
/// writes one; a record applied after the close that wrote, or a reopen at the close's rev and a
/// second close, writes no second; a close that reversed and ended otherwise writes the end it
/// came to. The board's lock holds the merge, the row's write and the board's, so of the processes
/// that read one close (the window's keeper, a dispatcher's wait) only one writes.
pub(crate) fn task_end(
    board_dir: &std::path::Path,
    was: &crate::board::Row,
    row: &crate::board::Row,
    seen: &str,
    now_ms: u64,
) -> Option<TaskEnd> {
    if row.is_open() {
        return None;
    }
    let record = macs_close(seen, was.rev).then_some(seen);
    let heard = was.is_open() && was.conclusion.is_some();
    if record.is_none() && !heard {
        return None;
    }
    // Without a record, the end is what the board heard while the row was open: the close's own
    // words (a closed tab's `failed`, an old Mac's or a new one's) are never its outcome or stop,
    // so whichever close of the worker comes first, the row says the same end.
    let ended = if record.is_some() {
        row.clone()
    } else {
        crate::board::Row {
            conclusion: was.conclusion.clone(),
            why: was.why.clone(),
            ..row.clone()
        }
    };
    let state = board_dir.parent().filter(|_| {
        board_dir
            .file_name()
            .is_some_and(|n| n == crate::board::DIR)
    })?;
    let (facts, digest) = aterm_telemetry::direct::facts();
    let task = task_row(&ended, record, now_ms, &facts.host, *digest);
    let told = format!("{}.{}", task.outcome.word(), task.stop.word());
    if row.told.as_deref() == Some(told.as_str()) {
        return None;
    }
    Some(TaskEnd {
        dir: aterm_telemetry::log::dir_in(state),
        row: task,
        told,
    })
}

/// Whether `seen` is a close built from its Mac's last row, at or above the board's rev `rev`
/// (`task_news=`), naming a conclusion that Mac's record came to ([`task_end`]).
fn macs_close(seen: &str, rev: u64) -> bool {
    let field = |key: &str| crate::await_task::row_field(seen, key);
    let concluded = match field("conclusion") {
        Some("finished") => true,
        Some(c) if !c.is_empty() && c != "-" => {
            field("task_state").is_some_and(|s| !matches!(s, "" | "-" | "working" | "closed"))
        }
        _ => false,
    };
    field("state") == Some("closed")
        && field("task_news").is_some()
        && field("rev")
            .and_then(|r| r.parse::<u64>().ok())
            .is_some_and(|r| r >= rev)
        && concluded
}

#[cfg(test)]
mod tests {
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
    }

    /// THE TASK ROW (Continuum TM-1): a worker writes its task row once its row is closed and
    /// the board holds the conclusion its Mac reported, whichever came first: a close built from
    /// the Mac's last row naming it, or the Mac's report while the row was open and then any
    /// close. NEGATIVE CONTROLS: a close with no conclusion of its Mac's writes none, though the
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

    /// A conclusion the board heard while the row was open is the worker's end whatever close
    /// follows: the Mac's own close of a tab working again (a turn after it finished), an old
    /// Mac's `failed` with no `task_state=`, or a close with no record of the Mac's (the
    /// keeper's), in either order. The close's words, a closed tab's reading, are never its
    /// outcome or stop. NEGATIVE CONTROL: a record close naming a concluding state is its end
    /// (`s-1b2`).
    #[test]
    fn a_heard_conclusion_is_the_end_whatever_close_follows_and_in_either_order() {
        let at = Scratch::new(&["s-4a1", "s-4b2", "s-4c3", "s-4d4", "s-4e5"]);
        let ends = |sid: &str| {
            at.tasks(sid)
                .iter()
                .map(|r| (r.outcome, r.stop))
                .collect::<Vec<_>>()
        };
        let finished = |sid: &str| {
            format!(
                "@{sid} rev=2 state=done agent=claude conclusion=finished why=landed task_news=1"
            )
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
        // A stall heard, then the Mac's close of the tab working again with no conclusion.
        at.apply(
            "@s-4d4 rev=2 state=stalled agent=claude conclusion=failed why=startup task_news=1",
            1_000,
        );
        at.apply(
            "@s-4d4 rev=3 state=closed task_state=working agent=claude task_news=1",
            2_000,
        );
        assert_eq!(ends("s-4d4"), [(Outcome::Fail, Stop::Stalled)]);
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
}
