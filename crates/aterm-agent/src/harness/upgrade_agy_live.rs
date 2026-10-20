// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE AGY LIVE UPGRADE, ITS PURE HALF: the QUIET FLOW only (`[harness]
//! upgrade_ask = false`, the default, and the one flow agy ever takes). When
//! atpkg lands a newer agy build under a supervised agy, the window's host ends
//! it at a genuinely idle point — SIGTERM, which agy answers with `0` (mid-turn
//! too: measured) — and relaunches it through the managed twin on the same
//! conversation (the relaunch lane's own line, `--conversation=<uuid>`), and
//! NOTHING is typed after. The I/O half is `upgrade_drive`'s `agy::live`
//! module (`upgrade_agy_live_drive.rs`, its `upgrade_visit`). Every fact here
//! was measured on 1.4.0 in the lab (`crates/aterm-lab`, the real binary
//! against its Gemini API stub, no account; the round-2 measurements of
//! 2026-10-17 and their critic, and the measurements of 2026-10-18):
//!
//! * **A running agy sees nothing of a landing** (30 s with no frame, the same
//!   inode; the twin's `AGY_CLI_DISABLE_AUTO_UPDATE` keeps its own updater
//!   off). The new build runs once the process is ended and started again
//!   through the twin, and `--conversation=<uuid>` through it came back on the
//!   same conversation, every prior turn in the next request.
//! * **The order of two builds** ([`order`]): agy's versions are plain
//!   `x.y.z`, read in atpkg's own grammar (`atpkg::vendor_direct::Version`,
//!   the order Muse Code's lane reads its revision form in); a store build's
//!   directory is its version's build id (`1.4.0` is `1000001000004000000`),
//!   so a record whose directory names another build is no version
//!   ([`build_version`]). Never `--version`.
//! * **The status substitute, G-A1**: agy keeps no status file as Claude Code
//!   does. Its reader's idle reading (Idle, authoritative, an empty composer,
//!   no wall; the drive's) and the conversation's row in
//!   `conversation_summaries.db` ([`summary_rows`]) reading
//!   `CASCADE_RUN_STATUS_IDLE` both stand for it. The row reads `RUNNING` from
//!   a turn's first moment to a few hundred milliseconds AFTER the reader
//!   reads it over (measured: the screen idle at …674.6, the row `RUNNING`
//!   until …675.4), so at a turn's end the row says the turn is still open
//!   (`wait:turn-open`), the point the restart then owns
//!   (`upgrade_drive::log_settles`). The database is READ ONLY, live: with
//!   agy's connection open (its `-wal` and `-shm` beside it) through SQLite's
//!   own read-only open; with it closed — agy closes it some 30 s after its
//!   last write, deleting both, so an idle agy's is closed most of the time —
//!   as an immutable file whose identity is the same before and after the read
//!   with no `-wal` made meanwhile (the drive's). 2 179 live read-only reads
//!   over 881 s of turns, tasks, a timer, subagents, kills, a cron and a
//!   resume left agy's own logs without a database error and every one of its
//!   writes in place.
//! * **No running work is interrupted, G-A2.** agy's background work — a
//!   `run_command` past its `WaitMsBeforeAsync` (a daemon one too), a
//!   `schedule` timer or cron, an `invoke_subagent` subagent — runs on after
//!   the turn that started it ended, and agy's SIGTERM kills it all silently
//!   (and nothing is revived on resume). So five witnesses must each say none
//!   runs, every one failing closed:
//!   (a) THE SCREEN: no panel row and no `task(s)`/`subagent(s)` count
//!   (`aterm_phase::agy::background_shown`; the count alone is drawn with the
//!   panel turned off, `hideActiveItems`: measured);
//!   (b) THE TRANSCRIPT ([`open_work`]): of this process's rows (created at or
//!   after its start, less [`START_SLACK_S`]), every background task's
//!   `RUNNING` row has its end — the `SYSTEM_MESSAGE` its `sender=` names, or
//!   `Task "<id>" cancelled.` (a `manage_task` kill: measured) — a cron's
//!   only once it fired its `MaxIterations` (each firing is a `sender=` row,
//!   `(iteration N)`, while it runs on: measured), and every subagent
//!   `Created the following subagents` names has gone idle (`Subagent <id> has
//!   gone idle`) or was killed (`manage_subagents`, then `Successfully killed`:
//!   measured, its row naming roles alone);
//!   (c) THE SUMMARY DATABASE, reinforcement: `not_fully_idle` 0, and no row
//!   whose parent is this conversation reads other than `IDLE` (blind to a
//!   daemon task, and stale after a kill until agy opens the conversation
//!   again: reinforcement only);
//!   (d) THE PROCESSES: no descendant at all (an idle agy has none; a command
//!   task is a `zsh -c` session leader on its own pty, a daemon task's simple
//!   command a bare `sleep`, which Claude Code's shell-name list misses: the
//!   drive's);
//!   (e) A PENDING WAKE ([`wakes`]): no message in
//!   `brain/<conv>/.system_generated/messages/` that wakes agy (every one
//!   but `MESSAGE_PRIORITY_LOW`) unread by `read.json`, nor one written
//!   within [`WAKE_SETTLE_MS`]. A task's end, a timer's or cron's firing and a
//!   subagent going idle each write one, HIGH, and agy takes a turn on it by
//!   itself: idle, the message is marked read 12 ms after it is written and
//!   its `SYSTEM_MESSAGE` row starts that turn; busy, it waits unread (and in
//!   `undelivered/`) until the running turn ends, and agy's turn on it
//!   follows at once (both measured). A LOW one (a cancelled task's, the
//!   restart notice) wakes nothing and waits for the next person's message.
//! * **Its SIGTERM is the restart's own, G-A3**: `0`, the crash file removed —
//!   every graceful way out's status, which the relaunch on exit reads as
//!   someone's end and never relaunches. So the record that ends it is written
//!   BEFORE the signal (its cause the upgrade's,
//!   [`super::relaunch::ends_its_agent`]), and the exit is carried as the
//!   harness's own restart (`relaunch::ended_by_restart`), onto the twin,
//!   adopted by the new process holding `presence/<uuid>.lock`; quiet, so
//!   nothing is typed after (`done:resumed`).
//! * **Its relaunch must be able to type, G-A4**: the relaunch lane types its
//!   line only once the conversation is free — no other live agy names it in
//!   its argv, and none holds its database with no presence lock: one opened
//!   on it while this agy held it (`--conversation`, `-c`, an in-TUI
//!   `/resume`) holds no lock and keeps holding that database once this agy is
//!   gone (measured). So the restart is never begun while another agy has the
//!   conversation open ([`Look::shared`], `wait:conversation-shared`): its
//!   SIGTERM would end an idle agy that nothing could bring back. And a launch
//!   that set its environment on its own line (`GEMINI_API_KEY=… agy`,
//!   [`inline_env`]) is never restarted (`skip:env-not-carried`): the relaunch
//!   line runs in the shell's environment and carries only `HOME`, so its key,
//!   endpoint or proxy would be gone.
//! * **The ask flow stays OFF for agy.** Its transcript has no model id and no
//!   process identity (measured), its notice would say "Claude Code", and its
//!   resumed screen replays an old READY. `[harness] upgrade_ask` moves
//!   nothing here.
//!
//! [`gate`] is the order the drive takes these in, one wait word each; the
//! derived model is `HarnessAgyQuietUpgrade`
//! (`aterm_models_harness::harness_agy_quiet_upgrade_model`), bound to this
//! gate, the order and the claim in `upgrade_agy_live_drive_tests.rs`.

use std::collections::{BTreeMap, BTreeSet};

use aterm_json::Value;

pub use super::upgrade_muse_live::{build_version, moves_onto, order};

// ---------------------------------------------------------------- the transcript

/// What in the conversation's transcript holds a restart off (G-A2(b)), or
/// why it proves nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Open {
    /// No process start to read the window from, or a tail of the transcript
    /// that does not reach back to it.
    Unproved,
    /// A row that does not parse (a torn tail), a row with no time, or the
    /// transcript could not be read at all.
    Unreadable,
    /// A background task (a command, a timer, a cron) started with no end.
    Task,
    /// A subagent spawned that neither went idle nor was killed.
    Subagent,
}

impl Open {
    /// The wait word the drive says (`wait:<word>`).
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Open::Unproved => "log-unproved",
            Open::Unreadable => "log-unreadable",
            Open::Task => "background-work",
            Open::Subagent => "subagent-working",
        }
    }
}

/// The slack on a row's time against the process's start: `created_at` is
/// whole seconds (truncated), and the kernel's start as the drive reads it is
/// to the second too.
pub const START_SLACK_S: u64 = 2;

/// The text a task's `RUNNING` row names its id after.
const TASK_STARTED: &str = "background task with task id: ";

/// The words of a subagent spawn's row.
const SUBAGENTS_CREATED: &str = "Created the following subagents:";

/// The words a `manage_subagents` kill's result row says.
const SUBAGENTS_KILLED: &str = "Successfully killed";

/// A tool call's argument as agy's transcript keeps it: each a JSON value
/// written as a string (`"Action": "\"kill\""`, `"MaxIterations": "1"`).
fn arg(call: &Value, name: &str) -> Option<Value> {
    let raw = call.get("args")?.get(name)?;
    match raw.as_str() {
        Some(text) => aterm_json::from_str::<Value>(text)
            .ok()
            .or_else(|| Some(Value::from(text))),
        None => Some(raw.clone()),
    }
}

/// Every UUID-shaped word in `text` (8-4-4-4-12 hex, any case).
fn uuids(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_ascii_hexdigit() || c == '-'))
        .filter(|w| super::codex_rollout::is_thread_id(w))
        .map(str::to_owned)
        .collect()
}

/// A task's id in a `RUNNING` row's content (`…task id: <conv>/task-N\n…`).
fn started_task(content: &str) -> Option<&str> {
    let rest = content.split_once(TASK_STARTED)?.1;
    let id = rest.split(char::is_whitespace).next()?;
    (!id.is_empty()).then_some(id)
}

/// A cron's expression and prompt in a `RUNNING` row's description (`Task
/// Description: Cron: */1 * * * *, Prompt: lab cron fired`); `None` for any
/// other task.
fn cron_of(content: &str) -> Option<(String, String)> {
    let line = content
        .lines()
        .find_map(|l| l.strip_prefix("Task Description: "))?;
    let (expr, prompt) = line.strip_prefix("Cron: ")?.split_once(", Prompt: ")?;
    Some((expr.to_owned(), prompt.to_owned()))
}

/// The id a `manage_task` kill's result row names cancelled (`Task "<id>"
/// cancelled.`).
fn cancelled_task(content: &str) -> Option<&str> {
    let rest = content.split_once("Task \"")?.1;
    let (id, tail) = rest.split_once('"')?;
    tail.trim_start().starts_with("cancelled").then_some(id)
}

/// The sender a `SYSTEM_MESSAGE` row names (`… sender=<id> priority=…`).
fn sender(content: &str) -> Option<&str> {
    content
        .split_once("sender=")?
        .1
        .split(char::is_whitespace)
        .next()
}

/// The subagent a `SYSTEM_MESSAGE` row says went idle (`Subagent <id> has gone
/// idle`).
fn gone_idle(content: &str) -> Option<String> {
    let (head, _) = content.split_once(" has gone idle")?;
    let id = head.rsplit("Subagent ").next()?;
    super::codex_rollout::is_thread_id(id).then(|| id.to_owned())
}

/// One background task open in the transcript.
#[derive(Debug)]
struct Task {
    /// A cron's `MaxIterations` (its schedule call's), `Some(None)` for a cron
    /// with none: it runs until it is cancelled. `None` for a command or a
    /// timer, which its first end row ends.
    cron: Option<Option<u64>>,
    /// A cron's firings read so far.
    fired: u64,
}

/// A `manage_subagents` kill waiting on its result row.
enum Kill {
    All,
    These(Vec<String>),
}

/// NO RUNNING WORK IN THE TRANSCRIPT `text` FROM `from_s` ON (G-A2(b)):
/// every row created at or after `from_s` (unix seconds) read in order —
/// every background task's `RUNNING` row ended by a `SYSTEM_MESSAGE` its
/// `sender=` names or by `Task "<id>" cancelled.`, a cron only once it fired
/// the `MaxIterations` its `schedule` call named (none: never by firing);
/// every subagent a spawn row names gone idle or killed (a `manage_subagents`
/// call naming it, or `kill_all`, then `Successfully killed`). A line that
/// does not parse, or a row with no time, is [`Open::Unreadable`]: the gate
/// fails closed. A task open first ([`Open::Task`]), then a subagent
/// ([`Open::Subagent`]).
pub fn open_work(text: &str, from_s: u64) -> Result<(), Open> {
    let mut tasks: BTreeMap<String, Task> = BTreeMap::new();
    let mut subagents: BTreeSet<String> = BTreeSet::new();
    // Cron schedule calls not yet matched to their task: expression, prompt,
    // `MaxIterations`.
    let mut schedules: Vec<(String, String, Option<u64>)> = Vec::new();
    let mut kill: Option<Kill> = None;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let row: Value = aterm_json::from_str(line.trim()).map_err(|_| Open::Unreadable)?;
        let created = row
            .get("created_at")
            .and_then(Value::as_str)
            .and_then(super::upgrade_models::parse_utc)
            .ok_or(Open::Unreadable)?;
        if created < from_s {
            continue;
        }
        let kind = row.get("type").and_then(Value::as_str).unwrap_or_default();
        let content = row
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match kind {
            "PLANNER_RESPONSE" => {
                for call in row
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    match call.get("name").and_then(Value::as_str) {
                        Some("schedule") => {
                            let text_arg =
                                |n: &str| arg(call, n).and_then(|v| v.as_str().map(str::to_owned));
                            if let (Some(expr), Some(prompt)) =
                                (text_arg("CronExpression"), text_arg("Prompt"))
                            {
                                let max = arg(call, "MaxIterations").and_then(|v| v.as_u64());
                                schedules.push((expr, prompt, max));
                            }
                        }
                        Some("manage_subagents") => {
                            let action = arg(call, "Action");
                            kill = match action.as_ref().and_then(Value::as_str) {
                                Some("kill_all") => Some(Kill::All),
                                Some("kill") => Some(Kill::These(
                                    arg(call, "ConversationIds")
                                        .map(|v| {
                                            uuids(&aterm_json::to_string(&v).unwrap_or_default())
                                        })
                                        .unwrap_or_default(),
                                )),
                                _ => kill,
                            };
                        }
                        _ => {}
                    }
                }
            }
            "GENERIC" => {
                if row.get("status").and_then(Value::as_str) == Some("RUNNING")
                    && let Some(id) = started_task(content)
                {
                    let cron = cron_of(content).map(|(expr, prompt)| {
                        schedules
                            .iter()
                            .position(|(e, p, _)| *e == expr && *p == prompt)
                            .and_then(|at| schedules.remove(at).2)
                    });
                    tasks.insert(id.to_owned(), Task { cron, fired: 0 });
                }
                if let Some(id) = cancelled_task(content) {
                    tasks.remove(id);
                }
                if let Some((_, spawned)) = content.split_once(SUBAGENTS_CREATED) {
                    subagents.extend(uuids(spawned));
                }
                if content.contains(SUBAGENTS_KILLED) {
                    match kill.take() {
                        Some(Kill::All) => subagents.clear(),
                        Some(Kill::These(ids)) => {
                            for id in ids {
                                subagents.remove(&id);
                            }
                        }
                        None => {}
                    }
                }
            }
            "SYSTEM_MESSAGE" => {
                if let Some(from) = sender(content)
                    && let Some(task) = tasks.get_mut(from)
                {
                    let ended = match task.cron {
                        None => true,
                        Some(max) => {
                            task.fired += 1;
                            max.is_some_and(|m| task.fired >= m)
                        }
                    };
                    if ended {
                        tasks.remove(from);
                    }
                }
                if let Some(id) = gone_idle(content) {
                    subagents.remove(&id);
                }
            }
            _ => {}
        }
    }
    if !tasks.is_empty() {
        return Err(Open::Task);
    }
    if !subagents.is_empty() {
        return Err(Open::Subagent);
    }
    Ok(())
}

/// G-A2(b) OVER THE TRANSCRIPT AS READ: `text` the whole file, or a tail of
/// it (`truncated`, its partial first line dropped), the process started at
/// `started_ms` (the kernel's, unix ms; `0`: unread). Its window starts
/// [`START_SLACK_S`] before that start; a tail must reach back before it
/// (its first row older), else [`Open::Unproved`], as is no start at all.
pub fn quiet_transcript(text: &str, truncated: bool, started_ms: u64) -> Result<(), Open> {
    if started_ms == 0 {
        return Err(Open::Unproved);
    }
    let from_s = (started_ms / 1000).saturating_sub(START_SLACK_S);
    if truncated {
        let first = text
            .lines()
            .find(|l| !l.trim().is_empty())
            .and_then(|l| aterm_json::from_str::<Value>(l.trim()).ok())
            .and_then(|row| {
                row.get("created_at")
                    .and_then(Value::as_str)
                    .and_then(super::upgrade_models::parse_utc)
            });
        if first.is_none_or(|at| at >= from_s) {
            return Err(Open::Unproved);
        }
    }
    open_work(text, from_s)
}

// ---------------------------------------------------------------- the summary

/// The status a conversation's summary row reads between turns.
pub const STATUS_IDLE: &str = "CASCADE_RUN_STATUS_IDLE";

/// The conversation's row in `conversation_summaries.db` and its subagents'
/// (rows whose parent it is), as the drive's read-only query answers them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    /// Its own row's `status` (`CASCADE_RUN_STATUS_IDLE` between turns).
    pub status: String,
    /// Its own row's `not_fully_idle`: background work (not a daemon task's)
    /// still runs, or ran when agy was killed.
    pub not_fully_idle: bool,
    /// Its subagents' rows that read other than idle.
    pub children_busy: usize,
}

/// AN ANSWER OR A FILE OF AGY'S THAT DOES NOT READ AS MEASURED (a summary
/// line of another shape, a torn message, a `read.json` that does not parse):
/// the witness reading it fails closed, never an idle point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unreadable;

/// A `numeric` boolean column as sqlite prints it (`0`/`1`, `false`/`true`);
/// `None` for anything else.
fn flag(text: &str) -> Option<bool> {
    match text.trim() {
        "0" | "false" => Some(false),
        "1" | "true" => Some(true),
        _ => None,
    }
}

/// THE SUMMARY OF `conversation` FROM THE QUERY'S ANSWER `out`: one line per
/// row, `conversation_id|status|not_fully_idle|parent_conversation_id`.
/// `Ok(None)` where no row is the conversation's own (agy has not summarized
/// it: nothing proved); `Err` for a line of any other shape (fail closed).
pub fn summary_rows(conversation: &str, out: &str) -> Result<Option<Summary>, Unreadable> {
    let mut own: Option<(String, bool)> = None;
    let mut children_busy = 0;
    for line in out.lines().filter(|l| !l.trim().is_empty()) {
        let fields: Vec<&str> = line.split('|').collect();
        let [id, status, nfi, parent] = fields.as_slice() else {
            return Err(Unreadable);
        };
        let nfi = flag(nfi).ok_or(Unreadable)?;
        if *id == conversation {
            own = Some(((*status).to_owned(), nfi));
        } else if *parent == conversation {
            if *status != STATUS_IDLE {
                children_busy += 1;
            }
        } else {
            return Err(Unreadable);
        }
    }
    Ok(own.map(|(status, not_fully_idle)| Summary {
        status,
        not_fully_idle,
        children_busy,
    }))
}

/// G-A1'S AND G-A2(c)'S RULE over the summary as read: `Ok` where its row
/// reads idle with nothing not fully idle and no subagent busy; else the
/// wait word — the database unread (`summary-unreadable`), no row of it
/// (`summary-unproved`), a turn's status (`turn-open`), background work by
/// its flag (`background-work`), a subagent's row (`subagent-working`).
pub fn summary_gate(s: &Result<Option<Summary>, Unreadable>) -> Result<(), &'static str> {
    match s {
        Err(Unreadable) => Err("summary-unreadable"),
        Ok(None) => Err("summary-unproved"),
        Ok(Some(s)) if s.status != STATUS_IDLE => Err("turn-open"),
        Ok(Some(s)) if s.not_fully_idle => Err("background-work"),
        Ok(Some(s)) if s.children_busy > 0 => Err("subagent-working"),
        Ok(Some(_)) => Ok(()),
    }
}

// ---------------------------------------------------------------- the messages

/// How long after a waking message is WRITTEN the restart still waits, read
/// or not: idle, agy marks it read 12 ms after writing it and its turn on it
/// starts there (measured); the restart never lands between the mark and the
/// turn showing on the screen and in the summary row.
pub const WAKE_SETTLE_MS: u64 = 5_000;

/// The priority of a message that wakes nothing (the restart notice, a
/// cancelled task's end): it waits for the person's next message.
const QUIET_PRIORITY: &str = "MESSAGE_PRIORITY_LOW";

/// One message file of the conversation: its id (the file's name without
/// `.json`), its text, and when it was written (unix ms).
#[derive(Clone, Debug)]
pub struct Message {
    /// The id `read.json` marks.
    pub id: String,
    /// The file's text.
    pub body: String,
    /// Its modification time, unix ms.
    pub written_ms: u64,
}

/// THE WAKES PENDING (G-A2(e)): of `messages`, those that wake agy (every
/// priority but [`QUIET_PRIORITY`], an unknown one too) that `read`
/// (`read.json`'s text: `{"<id>": true, …}`; `None`, no such file: nothing
/// read) does not mark read, or that were written within [`WAKE_SETTLE_MS`]
/// of `now_ms`. `Err` where a message or `read.json` does not parse: fail
/// closed.
pub fn wakes(read: Option<&str>, messages: &[Message], now_ms: u64) -> Result<usize, Unreadable> {
    let read: Value = match read {
        Some(text) => aterm_json::from_str(text).map_err(|_| Unreadable)?,
        None => Value::Object(aterm_json::Map::new()),
    };
    let mut pending = 0;
    for m in messages {
        let body: Value = aterm_json::from_str(&m.body).map_err(|_| Unreadable)?;
        if body.get("priority").and_then(Value::as_str) == Some(QUIET_PRIORITY) {
            continue;
        }
        let marked = read.get(&m.id).and_then(Value::as_bool) == Some(true);
        let recent = now_ms.saturating_sub(m.written_ms) < WAKE_SETTLE_MS;
        if !marked || recent {
            pending += 1;
        }
    }
    Ok(pending)
}

// ---------------------------------------------------------------- the gate

/// One look at a supervised agy that is behind its twin's build, everything
/// the quiet flow's restart waits on, read by the drive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Look {
    /// A hold, a hand other than the harness's, unread input, or a person's
    /// keystroke within the grace (`upgrade_drive::held`).
    pub held: bool,
    /// What the screen shows of agy's own background work (G-A2(a),
    /// `aterm_phase::agy::background_shown`).
    pub shown: Option<&'static str>,
    /// agy's reader reads the screen Idle, authoritatively, with no wall (its
    /// own background work reads busy).
    pub idle: bool,
    /// Its composer holds nothing (a person's draft is theirs).
    pub composer_empty: bool,
    /// The conversation's summary rows (G-A1, G-A2(c); [`summary_rows`]).
    pub summary: Result<Option<Summary>, Unreadable>,
    /// The transcript (G-A2(b), [`quiet_transcript`]).
    pub transcript: Result<(), Open>,
    /// The process's descendants (G-A2(d)): `None` where the table could not
    /// be read.
    pub children: Option<usize>,
    /// The wakes pending (G-A2(e), [`wakes`]): `None` where the messages
    /// could not be read.
    pub wakes: Option<usize>,
    /// The OTHER live agys that have the conversation open (G-A4): one whose
    /// argv names it, or one holding its database with no presence lock —
    /// never the agy the look is of. `None` where they cannot be read.
    pub shared: Option<usize>,
}

/// THE QUIET FLOW'S GATE: `Ok` where the restart may go now; else the wait,
/// the first of — a person's hold (`held`), background work the screen shows
/// (`background-work`), a screen not idle (`screen-not-idle`), a draft
/// (`draft`), the summary ([`summary_gate`]), the transcript ([`Open::word`]),
/// an unreadable process table (`process-table`) or any descendant at all
/// (`child-process`), another agy on the conversation or none readable
/// (`conversation-shared`), unreadable messages (`messages-unreadable`) or a
/// wake pending (`queued-work`). Nothing past the gate is decided here: the
/// build's proof to start, the claim and the last look are the drive's.
pub fn gate(l: &Look) -> Result<(), &'static str> {
    if l.held {
        return Err("held");
    }
    if l.shown.is_some() {
        return Err("background-work");
    }
    if !l.idle {
        return Err("screen-not-idle");
    }
    if !l.composer_empty {
        return Err("draft");
    }
    summary_gate(&l.summary)?;
    l.transcript.map_err(Open::word)?;
    match l.children {
        None => return Err("process-table"),
        Some(0) => {}
        Some(_) => return Err("child-process"),
    }
    if l.shared != Some(0) {
        return Err("conversation-shared");
    }
    match l.wakes {
        None => Err("messages-unreadable"),
        Some(0) => Ok(()),
        Some(_) => Err("queued-work"),
    }
}

// ---------------------------------------------------------------- the launch

/// What a character of a word sits inside, innermost last in
/// [`simple_commands`]' stack.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Nest {
    Single,
    Double,
    /// `$(…)`, `$((…))`, or a bare `(…)` inside one.
    Paren,
    /// `${…}`.
    Brace,
    Backtick,
}

/// The words of `line` (a command line as the shell's integration recorded
/// it, as typed), one list per simple command: split at every blank and every
/// `;`, `&`, `|`, `(`, `)`, `{`, `}` or newline that no quote, `$(…)`, `${…}`
/// or backtick holds. A word keeps its quotes, escapes and substitutions whole
/// (`KEY=$(security … -w)` is one word); an open one runs to the line's end.
fn simple_commands(line: &str) -> Vec<Vec<String>> {
    let mut commands: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut nest: Vec<Nest> = Vec::new();
    let mut escaped = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if escaped {
            word.push(ch);
            escaped = false;
            continue;
        }
        let top = nest.last().copied();
        if top == Some(Nest::Single) {
            if ch == '\'' {
                nest.pop();
            }
            word.push(ch);
            continue;
        }
        let quoted = top == Some(Nest::Double);
        match ch {
            '\\' => escaped = true,
            '"' if quoted => {
                nest.pop();
            }
            '"' => nest.push(Nest::Double),
            '\'' if !quoted => nest.push(Nest::Single),
            '$' if chars.peek() == Some(&'(') => {
                word.push(ch);
                word.extend(chars.next());
                nest.push(Nest::Paren);
                continue;
            }
            '$' if chars.peek() == Some(&'{') => {
                word.push(ch);
                word.extend(chars.next());
                nest.push(Nest::Brace);
                continue;
            }
            '`' if top == Some(Nest::Backtick) => {
                nest.pop();
            }
            '`' => nest.push(Nest::Backtick),
            '(' if top == Some(Nest::Paren) => nest.push(Nest::Paren),
            ')' if top == Some(Nest::Paren) => {
                nest.pop();
            }
            '}' if top == Some(Nest::Brace) => {
                nest.pop();
            }
            c if top.is_none() && (c == ' ' || c == '\t' || ";&|(){}\n\r".contains(c)) => {
                if !word.is_empty() {
                    current.push(std::mem::take(&mut word));
                }
                if c != ' ' && c != '\t' && !current.is_empty() {
                    commands.push(std::mem::take(&mut current));
                }
                continue;
            }
            _ => {}
        }
        word.push(ch);
    }
    if !word.is_empty() {
        current.push(word);
    }
    if !current.is_empty() {
        commands.push(current);
    }
    commands
}

/// The name a word assigns (`NAME=…`, its name unquoted), or `None`.
fn assigned_name(word: &str) -> Option<&str> {
    let (name, _) = word.split_once('=')?;
    let mut bytes = name.bytes();
    let first = bytes.next()?;
    ((first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_'))
    .then_some(name)
}

/// `word` with its quotes and escapes taken off.
fn unquoted(word: &str) -> String {
    let mut out = String::new();
    let (mut single, mut double, mut escaped) = (false, false, false);
    for ch in word.chars() {
        match ch {
            _ if escaped => {
                out.push(ch);
                escaped = false;
            }
            '\\' if !single => escaped = true,
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            c => out.push(c),
        }
    }
    out
}

/// WHAT A TYPED LAUNCH SET IN AGY'S ENVIRONMENT ON ITS OWN LINE, which no
/// relaunch line carries (G-A4): in `cmdline` — the shell integration's record
/// of the command the tab runs, as typed — the first `NAME=…` word other than
/// `HOME` (which the relaunch carries: `upgrade_agy::relaunch_env`), or the
/// first option of an `env` (`-u NAME`, `-i`), ahead of a word naming agy
/// (`agy`, or a path whose last part is `agy`, quoted or not) in the same
/// simple command. The relaunch line runs in the shell's environment, so a
/// `GEMINI_API_KEY=k GOOGLE_GEMINI_BASE_URL=… agy` restarted would come back
/// with no key and no endpoint. `None` for a launch that set nothing there —
/// an `export` of its own, a `cd … &&` before it, the relaunch's own
/// `HOME='…' '<twin>' …` — and for a line naming no agy: a function, an alias
/// or a script that sets a variable is beyond this witness, and so is the rest
/// of a bash line its history kept no record of (`ignorespace`), whose
/// integration names only its first simple command.
#[must_use]
pub fn inline_env(cmdline: &str) -> Option<String> {
    for words in simple_commands(cmdline) {
        let mut set: Option<String> = None;
        let mut under_env = false;
        for word in &words {
            if let Some(name) = assigned_name(word) {
                if name != super::upgrade_agy::HOME_ENV && set.is_none() {
                    set = Some(name.to_owned());
                }
                continue;
            }
            let bare = unquoted(word);
            let base = bare.rsplit('/').next().unwrap_or(&bare);
            if base == super::upgrade_agy::PROGRAM {
                if set.is_some() {
                    return set;
                }
                break;
            }
            if base == "env" {
                under_env = true;
            } else if under_env && bare.starts_with('-') && bare != "--" && set.is_none() {
                set = Some(format!("env {bare}"));
            }
        }
    }
    None
}

#[cfg(test)]
#[path = "upgrade_agy_live_tests.rs"]
mod tests;
