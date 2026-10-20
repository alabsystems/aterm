// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE MUSE CODE LIVE UPGRADE, ITS PURE HALF: the QUIET FLOW only (`[harness]
//! upgrade_ask = false`, the default, and the one flow Muse Code ever takes).
//! When atpkg lands a newer Muse Code build under a supervised Muse, the
//! window's host ends it at a genuinely idle point — SIGTERM, which Muse
//! answers with `143` and a clean `session.end` — and relaunches it through the
//! managed twin on the same session (the relaunch lane's own line, `resume
//! <uuid>`), and NOTHING is typed after. The I/O half is `upgrade_drive`'s
//! `muse` module (`upgrade_muse_drive.rs`, its `upgrade_visit`). Every fact
//! here was measured on 1.4.10-R6427.1 in the lab (`crates/aterm-lab`, the
//! real binary against its stub, no account; the 2026-10-17 measurements and
//! their critic):
//!
//! * **A running Muse sees nothing of a landing.** For a minute after atpkg's
//!   twin moved, the running process drew nothing, kept its argv and mapped
//!   binary, and made no request; Muse Code has no self-updater. The new build
//!   runs only once the process is ended and started again through the twin
//!   (which names the new build), and a `resume <uuid>` through it came back on
//!   the same session with every prior turn in the next request.
//! * **The order of two builds** ([`order`]): Muse's versions are the revision
//!   form `x.y.z-R<n>[.<s>]`, which the Claude lane's `upgrade::Version`
//!   refuses; atpkg's own vendor version (`atpkg::vendor_direct::Version`) is
//!   revision-aware, and a store build's directory is that version's build id,
//!   so a record whose directory names another build is no version at all
//!   ([`build_version`]). The version is the store's `<build>.vendor` record,
//!   never `--version` (which takes Muse's auth lock).
//! * **The status substitute, G-M1** ([`quiet_log`]): Muse keeps no status file
//!   as Claude Code does. Its session's own log does better: every process's
//!   start writes a `runtime.session.route_facts` record naming its `pid`, and
//!   from there every run `started` has a run `terminal` once its turn is over
//!   (a cancelled one too). The reader's idle reading (Idle, authoritative, an
//!   empty composer) is required beside it, by the drive.
//! * **No running work is interrupted, G-M2** ([`open_work`]): a backgrounded
//!   shell (`bash` with `yield_time_ms`) leaves its turn ended (`terminal:
//!   completed`) while its task runs on — a task `accepted`/`started` with no
//!   end, its last words `status 'terminal process running'` and an `output`
//!   whose `execution_state` is `background_running`. When it ends, Muse queues
//!   an inbox item (`inbox_item_queued`, `background_task_terminal`), ends the
//!   task, and only then starts a run of its own that drains it
//!   (`inbox_item_drained`): between the two the screen may read idle. So in
//!   this process's part of the log every task accepted or started has an end —
//!   `completed`, `cancelled`, `failed` or `rejected` (measured over 702 kept
//!   lab logs: nothing ever follows one of the four, and `failed` is as common
//!   as `cancelled` is rare, so a gate that knew only two would hold a session
//!   whose tool failed for ever), but for a task whose kind carries no work a
//!   restart could lose (`session_name.allocate`, [`NO_WORK_TASKS`], which the
//!   next process fails and redoes) — and every queued item a later drain; and the
//!   same of every subagent log of the session (`subagent/<id>/session.jsonl`,
//!   whose records carry no pid: read from the part's start by their
//!   `recorded_at`). An unreadable or torn log is no proof ([`Open::Unreadable`]),
//!   nor a log with no start of this process in it ([`Open::Unproved`]).
//! * **The process rule, G-M3**: an idle Muse has NO child process at all
//!   (fresh, 10, 30 and 60 s after a turn that ran a shell, and resumed); a
//!   tool shell is a `/bin/sh -c` child in a session of its own. So the gate
//!   requires no descendant at all, never Claude's shell-name list, which a
//!   shell that `exec`s its last command slips past.
//! * **Its SIGTERM is the restart's own, G-M4**: `143` and a clean
//!   `session.end`, which the relaunch on exit reads as someone's end and never
//!   relaunches. So the record that ends it is written BEFORE the signal (its
//!   cause the upgrade's, [`super::relaunch::ends_its_agent`]), and the exit is
//!   carried as the harness's own restart (`relaunch::ended_by_restart`), onto
//!   the twin, adopted by the `.session.lock` its new process holds; quiet, so
//!   nothing is typed after (`done:resumed`).
//! * **The ask flow stays OFF for Muse Code.** Its notice says "Claude Code"
//!   and `claude --resume`; Muse's screen half of a READY reads a soft-wrapped
//!   marker as a line of its own and replays the old marker after a resume
//!   (both measured), and its login and limit walls under a notice are
//!   unmeasured. `[harness] upgrade_ask` moves nothing here.
//!
//! [`gate`] is the order the drive takes these in, one wait word each; the
//! derived model is `HarnessMuseQuietUpgrade`
//! (`aterm_models_harness::harness_muse_quiet_upgrade_model`), bound to this
//! gate, the order and the claim in `upgrade_muse_live_tests.rs`.

use std::cmp::Ordering;

use aterm_json::Value;
use atpkg::vendor_direct::Version as BuildVersion;

// ---------------------------------------------------------------- the order

/// A Muse Code build's version as its store record names it (`version`), in
/// atpkg's revision-aware grammar — and, where the build's directory name is
/// known (`build`, `store/muse/<build>/`), only where that directory IS the
/// version's build id (atpkg lays every vendor build under it): a record
/// naming another version than its directory is no version. `None` for a
/// version atpkg's grammar refuses.
#[must_use]
pub fn build_version(version: &str, build: Option<&str>) -> Option<BuildVersion> {
    let v = BuildVersion::parse(version.trim())?;
    match build {
        Some(dir) => (dir.parse::<u64>().ok() == Some(v.build_id())).then_some(v),
        None => Some(v),
    }
}

/// THE ORDER OF THE TWIN'S BUILD AGAINST THE ONE THAT RUNS: `target` against
/// `running`, both atpkg vendor versions (the revision form `x.y.z-R<n>[.<s>]`
/// or plain `x.y.z`), where both parse in the SAME grammar (a program speaks
/// one, and the two never compare across). `None` where either does not
/// parse, or they speak two grammars: no order, so no move.
#[must_use]
pub fn order(running: &str, target: &str) -> Option<Ordering> {
    let (r, t) = (
        BuildVersion::parse(running.trim())?,
        BuildVersion::parse(target.trim())?,
    );
    (r.has_revision() == t.has_revision()).then(|| t.cmp(&r))
}

/// Whether a session running `running` is moved onto `target`: strictly
/// newer by [`order`]. Never sideways, never down, never on no order.
#[must_use]
pub fn moves_onto(running: &str, target: &str) -> bool {
    order(running, target) == Some(Ordering::Greater)
}

// ---------------------------------------------------------------- the log

/// What in a session's log holds a restart off (G-M1, G-M2), or why the log
/// proves nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Open {
    /// No `runtime.session.route_facts` record names the process (written
    /// since it started) in what was read: its part of the log is not found.
    Unproved,
    /// A line of the log does not parse (a torn tail, a log written over), or
    /// the log could not be read at all.
    Unreadable,
    /// A run started with no terminal: a turn runs (G-M1).
    Turn,
    /// A task accepted or started with no end (G-M2); `background` where its
    /// last word says it runs in the background (`terminal process running`,
    /// an output `background_running`).
    Task {
        /// Its last word names background work.
        background: bool,
    },
    /// An inbox item queued with no later drain: the work's end is told and
    /// Muse's own run on it is still to start (G-M2).
    Queued,
}

impl Open {
    /// The wait word the drive says (`wait:<word>`).
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Open::Unproved => "log-unproved",
            Open::Unreadable => "log-unreadable",
            Open::Turn => "turn-open",
            Open::Task { background: true } => "background-work",
            Open::Task { background: false } => "open-task",
            Open::Queued => "queued-work",
        }
    }
}

/// The record's `payload_type`, its `payload`, and its `recorded_at`
/// (microseconds), where it has them. A retained frame or marker (no
/// `payload_type`) is no record of a run or a task.
fn record(v: &Value) -> Option<(&str, &Value, Option<u64>)> {
    Some((
        v.get("payload_type")?.as_str()?,
        v.get("payload")?,
        v.get("recorded_at").and_then(Value::as_u64),
    ))
}

/// The pid a `runtime.session.route_facts` record names.
fn route_pid(payload: &Value) -> Option<u64> {
    payload.get("record")?.get("pid")?.as_u64()
}

/// THIS PROCESS'S PART OF THE SESSION'S LOG: where in `log` the LAST
/// `runtime.session.route_facts` record naming `pid` begins (a byte offset
/// past that record's line), and its `recorded_at` (microseconds) — only one
/// written no earlier than `since_us` (the process's start, by the kernel): a
/// record of an earlier process that had the same pid number is no part of
/// this one's. `None` where `log` holds none.
#[must_use]
pub fn part_start(log: &str, pid: u32, since_us: u64) -> Option<(usize, u64)> {
    let mut at = 0;
    let mut found = None;
    for line in log.split_inclusive('\n') {
        let next = at + line.len();
        if line.contains("route_facts")
            && let Ok(v) = aterm_json::from_str::<Value>(line.trim())
            && let Some(("runtime.session.route_facts", payload, recorded)) = record(&v)
            && route_pid(payload) == Some(u64::from(pid))
            && let Some(recorded) = recorded.filter(|r| *r >= since_us)
        {
            found = Some((next, recorded));
        }
        at = next;
    }
    found
}

/// A task event's id and kind, of either shape Muse writes: `runtime.session`
/// with payload `kind: task`, or `runtime.session.task`.
fn task_event<'a>(payload_type: &str, payload: &'a Value) -> Option<(&'a str, &'a Value)> {
    let task = match payload_type {
        "runtime.session" => payload.get("kind")?.as_str()? == "task",
        "runtime.session.task" => true,
        _ => false,
    };
    if !task {
        return None;
    }
    let event = payload.get("event")?;
    let id = payload
        .get("task_id")
        .and_then(Value::as_str)
        .or_else(|| event.get("task_id").and_then(Value::as_str))?;
    Some((id, event))
}

/// A run event of `runtime.session` (payload `kind: run`).
fn run_event<'a>(payload_type: &str, payload: &'a Value) -> Option<(&'a str, &'a Value)> {
    if payload_type != "runtime.session" || payload.get("kind")?.as_str()? != "run" {
        return None;
    }
    Some((payload.get("run_id")?.as_str()?, payload.get("event")?))
}

/// The task events that end a task: nothing follows one (module header).
const TASK_ENDS: [&str; 4] = ["completed", "cancelled", "failed", "rejected"];

/// THE TASK KINDS THAT CARRY NO WORK A RESTART COULD LOSE, by the `task_kind`
/// their `proposed` event names: an open task of one holds nothing (G-M2's
/// open-task rule reads every other kind, so a kind first seen is work and
/// holds — fail closed). Only `session_name.allocate`, the reservation of the
/// session's display name in Muse's session-name authority (the account's
/// `~/Library/Application Support/Muse/session-name-authority`, whatever
/// `HOME` says), measured over the 72 such tasks in the kept lab logs of
/// 1.4.10-R6427.1: each is proposed, accepted, scheduled and started as its
/// process starts, BEFORE any run; 55 never end, and the other 17 are ended
/// `failed` ("session name reservation failed") only by the NEXT process, at
/// its resume, which then proposes an allocation of its own (every resume of
/// `b7muse`'s session did: thirteen in a row). So a SIGTERM cuts off nothing
/// the next process does not redo, while a gate that read it as work would
/// hold such a session for its process's whole life (the review of
/// 2026-10-18; 70 of the 339 kept process parts ended with one open). The
/// task appears only where the authority's database could be opened but the
/// reservation never came (the labs before 2026-10-17 02:53 opened the
/// account's read-only; since the jail denies it, none is proposed); where a
/// reservation does complete, its task ends `completed` and holds nothing
/// either way.
const NO_WORK_TASKS: [&str; 1] = ["session_name.allocate"];

/// Whether a task's event says its work runs in the background: its status
/// `terminal process running`, or an output whose chunk names
/// `execution_state` `background_running`.
fn says_background(event: &Value) -> bool {
    match event.get("kind").and_then(Value::as_str) {
        Some("status") => {
            event.get("message").and_then(Value::as_str) == Some("terminal process running")
        }
        Some("output") => event
            .get("chunk")
            .and_then(Value::as_str)
            .is_some_and(|c| c.contains("background_running")),
        _ => false,
    }
}

/// An inbox item's key: its source's kind and the task it tells of. A queued
/// item's source is the event's own; a drained one's its delivery snapshot's.
fn inbox_key(source: Option<&Value>) -> (String, String) {
    let field = |k: &str| {
        source
            .and_then(|s| s.get(k))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    (field("source"), field("task_id"))
}

/// NO RUNNING WORK IN `log` FROM `from_us` ON (G-M1 and G-M2 over one log):
/// every record recorded at or after `from_us` (microseconds; a record with no
/// time counts; `0` reads them all) read in order — every run `started` has a
/// later `terminal` ([`Open::Turn`]), every task `accepted` or `started` a
/// later end ([`TASK_ENDS`]; [`Open::Task`]) unless its kind carries no work
/// ([`NO_WORK_TASKS`]), and every run `inbox_item_queued` a later
/// `inbox_item_drained` of the same source ([`Open::Queued`]). A line that
/// does not parse is [`Open::Unreadable`]: the gate fails closed. The first
/// that stands, in that order (a turn, then a task, then a queued item).
pub fn open_work(log: &str, from_us: u64) -> Result<(), Open> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut runs: BTreeMap<String, bool> = BTreeMap::new();
    // A task's id → whether its last word said background (open tasks only).
    let mut tasks: BTreeMap<String, bool> = BTreeMap::new();
    // The tasks whose `proposed` named a kind that carries no work.
    let mut no_work: BTreeSet<String> = BTreeSet::new();
    let mut queued: Vec<(String, String)> = Vec::new();
    for line in log.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = aterm_json::from_str(line.trim()).map_err(|_| Open::Unreadable)?;
        let Some((kind, payload, recorded)) = record(&v) else {
            continue;
        };
        if recorded.is_some_and(|r| r < from_us) {
            continue;
        }
        if let Some((id, event)) = task_event(kind, payload) {
            match event.get("kind").and_then(Value::as_str) {
                Some("proposed") => {
                    if event
                        .get("task_kind")
                        .and_then(Value::as_str)
                        .is_some_and(|k| NO_WORK_TASKS.contains(&k))
                    {
                        no_work.insert(id.to_owned());
                    }
                }
                Some("accepted" | "started") if !no_work.contains(id) => {
                    tasks.entry(id.to_owned()).or_insert(false);
                }
                Some(end) if TASK_ENDS.contains(&end) => {
                    tasks.remove(id);
                }
                _ => {
                    if let Some(background) = tasks.get_mut(id) {
                        *background = says_background(event);
                    }
                }
            }
            continue;
        }
        if let Some((run, event)) = run_event(kind, payload) {
            match event.get("kind").and_then(Value::as_str) {
                Some("started") => {
                    runs.insert(run.to_owned(), true);
                }
                Some("terminal") => {
                    runs.insert(run.to_owned(), false);
                }
                Some("inbox_item_queued") => queued.push(inbox_key(event.get("source"))),
                Some("inbox_item_drained") => {
                    let key =
                        inbox_key(event.get("delivery_snapshot").and_then(|d| d.get("source")));
                    if let Some(at) = queued.iter().position(|q| *q == key) {
                        queued.remove(at);
                    }
                }
                _ => {}
            }
        }
    }
    if runs.values().any(|open| *open) {
        return Err(Open::Turn);
    }
    if let Some(background) = tasks.values().copied().reduce(|a, b| a || b) {
        return Err(Open::Task { background });
    }
    if !queued.is_empty() {
        return Err(Open::Queued);
    }
    Ok(())
}

/// G-M1 AND G-M2 OVER THE SESSION'S OWN LOG `log` (as read: whole, or a tail
/// that reaches the start): the part of process `pid` ([`part_start`], its
/// start at or after `since_us`, the kernel's), then [`open_work`] over EVERY
/// record of it. The part is bounded by where its route record lies in the
/// file, never by the records' wall clock: a record written after the route
/// but stamped before it (the clock stepped back) is this process's all the
/// same, and dropping it would read open work closed. `Ok` with the part's
/// start (microseconds), from which every subagent log of the session — whose
/// records carry no pid, so only their time places them — is read
/// ([`open_work`]).
pub fn quiet_log(log: &str, pid: u32, since_us: u64) -> Result<u64, Open> {
    let (at, from_us) = part_start(log, pid, since_us).ok_or(Open::Unproved)?;
    open_work(&log[at..], 0)?;
    Ok(from_us)
}

// ---------------------------------------------------------------- the gate

/// One look at a supervised Muse Code that is behind its twin's build,
/// everything the quiet flow's restart waits on, read by the drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Look {
    /// A hold, a hand other than the harness's, unread input, or a person's
    /// keystroke within the grace (`upgrade_drive::held`).
    pub held: bool,
    /// Muse's reader reads the screen Idle, authoritatively, with no wall
    /// (its own background work reads busy, `aterm_phase::muse::phase`).
    pub idle: bool,
    /// Its composer holds nothing (a person's draft is theirs).
    pub composer_empty: bool,
    /// The session's log and its subagents' ([`quiet_log`], [`open_work`]).
    pub log: Result<(), Open>,
    /// The process's descendants: `None` where the table could not be read.
    pub children: Option<usize>,
}

/// THE QUIET FLOW'S GATE: `Ok` where the restart may go now; else the wait,
/// the first of — a person's hold (`held`), a screen not idle
/// (`screen-not-idle`), a draft (`draft`), the log ([`Open::word`]), an
/// unreadable process table (`process-table`) or any descendant at all
/// (`child-process`). Nothing past the gate is decided here: the build's
/// proof to start, the claim and the last look are the drive's.
pub fn gate(l: &Look) -> Result<(), &'static str> {
    if l.held {
        return Err("held");
    }
    if !l.idle {
        return Err("screen-not-idle");
    }
    if !l.composer_empty {
        return Err("draft");
    }
    l.log.map_err(Open::word)?;
    match l.children {
        None => Err("process-table"),
        Some(0) => Ok(()),
        Some(_) => Err("child-process"),
    }
}

#[cfg(test)]
#[path = "upgrade_muse_live_tests.rs"]
mod tests;
