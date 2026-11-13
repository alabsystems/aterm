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
//!   conclusion its record came to (`task_state=` other than `working`, or `finished`), whose
//!   words are the row's end; or else the Mac reported one while the row stood open (the last it
//!   named, past a later live row that names none: a turn typed after it), which is the row's
//!   end whatever words the close merged over it. A stall's `failed` stands only while the stall
//!   does: a live row of it moving on, or the Mac's close at a later rev saying `working`, takes
//!   it back. A close with neither writes nothing: the board's
//!   `failed` there, and a Mac's own `conclusion=failed` for a tab closed while still `working`,
//!   is the reading of whoever closed a gone tab, not an end its task came to. A worker writes
//!   one, and another only where a close reversed and it came to another end: the board row keeps
//!   the end its last row told (`told=`), so its last task row is its end. A row the log's lock
//!   (or its disk) refused is owed (`owed=`) under the board's lock, and written by a later apply
//!   of its row or the keeper's settle at its own close's time and `wall_ms`, never also said
//!   lost; one the board gives up is said lost once. A board row records what it writes to the
//!   log as paying (`paying=`) before the append, with the end it told before (`was_told=`), so
//!   a row whose write after it failed asks the log, and writes again only what the log does not
//!   hold, unless a writer that knows no `paying=` told a newer end since. `outcome` is the
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
/// exchange's result (what it answered, before anything of it is printed, and the sid its answer
/// named; `Err` where it was lost).
pub(crate) type Dialled<C> = std::io::Result<std::io::Result<(C, Option<String>)>>;

/// This Mac's telemetry log, which a sender's row goes to: its state folder's
/// ([`aterm_telemetry::log::default_dir`]), the one its board's task rows go to; in a test, the
/// log [`log_of`] finds beside the board a [`crate::board::HereGuard`] points at (none for a
/// folder not named [`crate::board::DIR`]).
pub(crate) fn here_log() -> Option<std::path::PathBuf> {
    #[cfg(test)]
    if let Some(board) = crate::board::here_override() {
        return log_of(&board);
    }
    aterm_telemetry::log::default_dir()
}

/// After a fleet spawn's dial and exchange: write the sender's row of the worker `args` asked for
/// (`agent=`, `cwd=`), dialled at `began` to the node `on` and answered `exchanged`, to the log in
/// `dir`; with no `dir` (no state folder), none.
pub(crate) fn spawned<C>(
    dir: Option<std::path::PathBuf>,
    args: &[String],
    began: std::time::Instant,
    exchanged: &Dialled<C>,
    on: Option<&str>,
) {
    let Some(dir) = dir else {
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

/// How long a board keeps a task row it owes its log ([`crate::board::Row::owed`]), from the
/// close it is the row of: a write that fails once it is this old gives it up, said lost once.
pub(crate) const OWED_KEPT: std::time::Duration = std::time::Duration::from_secs(3600);

/// The most task rows one board row owes its log at once: a close past it gives up the oldest.
pub(crate) const OWED_MAX: usize = 4;

/// The task row a board's close owes the log ([`task_end`]).
pub(crate) struct TaskEnd {
    row: Row,
}

impl TaskEnd {
    /// Owe the row on the board row `on` ([`crate::board::Row::owed`]), after any it owes
    /// already, so its board writes it with them, its own close's time and `wall_ms` kept: how
    /// many of the oldest it gave up to hold at most [`OWED_MAX`].
    pub(crate) fn owe(&self, on: &mut crate::board::Row) -> u64 {
        on.owed.push(self.row.line());
        let over = on.owed.len().saturating_sub(OWED_MAX);
        on.owed.drain(..over);
        over as u64
    }
}

/// The task row an owed line holds ([`crate::board::Row::owed`]): `None` for a line that is no
/// whole `kind=task` row.
pub(crate) fn owed_row(line: &str) -> Option<Row> {
    match aterm_telemetry::row::parse_line(line)? {
        aterm_telemetry::row::Line::Row(row) if row.kind == Kind::Task => Some(row),
        _ => None,
    }
}

/// The end a task row tells, `<outcome>.<stop>`: what its board keeps of it
/// ([`crate::board::Row::told`]).
pub(crate) fn told_of(row: &Row) -> String {
    format!("{}.{}", row.outcome.word(), row.stop.word())
}

/// The log of the state folder a board in `board_dir` sits in (`<state>/board`,
/// [`crate::board::Board::in_state`]): `None` for a board elsewhere, which has no state of its
/// own and writes no row.
pub(crate) fn log_of(board_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let state = board_dir.parent().filter(|_| {
        board_dir
            .file_name()
            .is_some_and(|n| n == crate::board::DIR)
    })?;
    Some(aterm_telemetry::log::dir_in(state))
}

/// The board in `board_dir` merged `seen` into `was`, changing it to `row`, at `now_ms`: where
/// `row` is closed and the board holds its worker's conclusion as its Mac reported it, the
/// worker's task row, for the log of the state folder the board sits in (`<state>/board`,
/// [`crate::board::Board::in_state`]). A board elsewhere has no state of its own and writes none.
///
/// The conclusion is its Mac's where `seen` is a close built from that Mac's last row (`task_news=`
/// at the board's rev or above: a row the Mac wrote, or a wait's `closed_row` of the last one it
/// answered) and names a conclusion its record came to: that record's words are the end. Without
/// one, it is the conclusion `was` heard while it stood open: the one it holds (an open row's
/// conclusion is only ever the last its Mac reported), or, where a later live row named none (a
/// turn typed after it finished), the last one its Mac named ([`crate::board::Row::ended`]). A
/// stall's `failed` is heard only while the stall stands: its record takes it back when the stall
/// moves on, which a live row naming none or the Mac's close at a later rev saying `working`
/// reports, and `ended` never keeps it. That heard conclusion and its `why=` are then the end,
/// never the words the close merged over them: so a close of a tab working again, from any Mac, a
/// recordless close, and a live row of the turn read before or after either, in any order, write
/// the same end. A stall is the one end the board cannot see past where nothing it reads says the
/// stall moved on: then it writes the stall it last heard, unless a live row of the stall moving
/// on was read first (and then the end heard before the stall, if any). Such closes include a
/// recordless close read before the Mac's close that says so, a wait's close built from the
/// stalled row it last read (the Mac's aterm relaunched without the tab), and a `failed` close
/// from a Mac too old to say `task_state=`, whose rev a move of a standing stall's ask raises too;
/// that Mac's `finished` close is a record of its own. A Mac's close
/// says its record's own state (`task_state=`): `finished` is only ever a record's, but `failed`
/// is also what a Mac answers for a tab closed on a task still `working`, which is that close's
/// reading and no end its task came to. So a close's `failed` counts only beside a `task_state=`
/// other than `working`, and a close from a Mac too old to say one counts only for `finished`. A
/// close with neither writes none: the board's `failed` for a tab gone with no conclusion is the reading of
/// whoever closed it, never the Mac's.
///
/// A close is the board's reading of a tab it no longer finds, which a later answer can reverse
/// ([`crate::board::Row::merge`] reopens the row). So a worker writes a row again only where its
/// end changed: none where its outcome and stop are those of the newest row its board row owes
/// ([`crate::board::Row::owed`]), or, with none, of the row it last told
/// ([`crate::board::Row::told`]: while it pays rows, [`crate::board::Row::paying`], theirs, unless
/// a writer that knows no `paying=` told a newer one since), all of which the board
/// keeps under its lock with the row, past every rotation of the log. A worker's
/// last task row is its end. One that closed once, the usual flow, writes one; a record applied
/// after the close that wrote, or a reopen at the close's rev and a second close, writes no
/// second; a close that reversed and ended otherwise writes the end it came to. The board's lock
/// holds the merge, the row's write and the board's, so of the processes that read one close
/// (the window's keeper, a dispatcher's wait) only one writes.
///
/// The row is owed, not written here: the board writes it with every row it owes, and keeps
/// those its log refused (its lock held past [`aterm_telemetry::direct::LOCK_WAIT`], a disk that
/// filled) in the board row until a later write takes them, each at its own close's time and
/// `wall_ms`, so the worker's last task row is still its end. A row written late is never also
/// said lost; one the board gives up (its board row pruned or written over, past [`OWED_KEPT`],
/// a line that holds no row, or past [`OWED_MAX`]) is said lost once, and one it was paying when
/// a write after the append failed is settled by what the log holds, never written twice
/// ([`crate::board::BoundBoard::pay`]).
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
    // What the board heard while the row stood open: the conclusion it holds, unless that is a
    // stall the Mac's close says moved on, else the last end its Mac named before a live row
    // that named none.
    let standing =
        was.conclusion.is_some() && !(was.state == "stalled" && macs_moved_on(seen, was.rev));
    let heard = if standing {
        (&was.conclusion, &was.why)
    } else {
        (&was.ended, &was.ended_why)
    };
    let heard = (was.is_open() && heard.0.is_some()).then_some(heard);
    // Without a record, the end is what the board heard: the close's own words (a closed tab's
    // `failed`, an old Mac's or a new one's) are never its outcome or stop, so whichever close of
    // the worker comes first, the row says the same end.
    let ended = match (record, heard) {
        (Some(_), _) => row.clone(),
        (None, Some((conclusion, why))) => crate::board::Row {
            conclusion: conclusion.clone(),
            why: why.clone(),
            ..row.clone()
        },
        (None, None) => return None,
    };
    log_of(board_dir)?;
    let (facts, digest) = aterm_telemetry::direct::facts();
    let task = task_row(&ended, record, now_ms, &facts.host, *digest);
    // The end the board last told or owes: the newest row it owes is the one its log will say
    // last, else the end it told, which while it pays rows is theirs, or a newer one a writer
    // that knows no `paying=` told since.
    let last = row
        .owed
        .iter()
        .rev()
        .find_map(|line| owed_row(line))
        .map(|owed| told_of(&owed))
        .or_else(|| row.told.clone());
    if last.as_deref() == Some(told_of(&task).as_str()) {
        return None;
    }
    Some(TaskEnd { row: task })
}

/// Whether `seen` is a close built from its Mac's last row at a rev above the board's `rev`
/// (`task_news=`) whose record was `working`: a stall the board heard was taken back.
fn macs_moved_on(seen: &str, rev: u64) -> bool {
    let field = |key: &str| crate::await_task::row_field(seen, key);
    field("state") == Some("closed")
        && field("task_news").is_some()
        && field("task_state") == Some("working")
        && field("rev")
            .and_then(|r| r.parse::<u64>().ok())
            .is_some_and(|r| r > rev)
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
#[path = "fleet_telemetry_tests.rs"]
mod tests;
