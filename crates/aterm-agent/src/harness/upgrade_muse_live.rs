// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE MUSE CODE LIVE UPGRADE, ITS PURE HALF.

use std::cmp::Ordering;

use aterm_json::Value;
use atpkg::vendor_direct::Version as BuildVersion;

/// A Muse Code build's version as its store record names it (`version`), in atpkg's revision-aware grammar.
#[must_use]
pub fn build_version(version: &str, build: Option<&str>) -> Option<BuildVersion> {
    let v = BuildVersion::parse(version.trim())?;
    match build {
        Some(dir) => (dir.parse::<u64>().ok() == Some(v.build_id())).then_some(v),
        None => Some(v),
    }
}

/// THE ORDER OF THE TWIN'S BUILD AGAINST THE ONE THAT RUNS.
#[must_use]
pub fn order(running: &str, target: &str) -> Option<Ordering> {
    let (r, t) = (
        BuildVersion::parse(running.trim())?,
        BuildVersion::parse(target.trim())?,
    );
    (r.has_revision() == t.has_revision()).then(|| t.cmp(&r))
}

/// Whether a session running `running` is moved onto `target`.
#[must_use]
pub fn moves_onto(running: &str, target: &str) -> bool {
    order(running, target) == Some(Ordering::Greater)
}

/// What in a session's log holds a restart off (G-M1, G-M2), or why the log proves nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Open {
    /// No `runtime.session.route_facts` record names the process (written since it started) in what was read.
    Unproved,
    /// A line of the log does not parse (a torn tail, a log written over), or the log could not be read at all.
    Unreadable,
    /// A run started with no terminal.
    Turn,
    /// A task accepted or started with no end (G-M2); `background` where its
    /// last word says it runs in the background (`terminal process running`,
    /// an output `background_running`).
    Task {
        /// Its last word names background work.
        /// Its last word names background work.
        background: bool,
    },
    /// An inbox item queued with no later drain.
    Queued,
}

impl Open {
    /// The wait word the drive says (`wait:<word>`).
    #[must_use]
    pub const fn word(self) -> &'static str {
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
/// (microseconds), where it has them.
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

/// THIS PROCESS'S PART OF THE SESSION'S LOG.
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
/// holds — fail closed).
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

/// An inbox item's key: its source's kind and the task it tells of.
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

/// NO RUNNING WORK IN `log` FROM `from_us` ON (G-M1 and G-M2 over one log).
pub fn open_work(log: &str, from_us: u64) -> Result<(), Open> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut runs: BTreeMap<String, bool> = BTreeMap::new();
    let mut tasks: BTreeMap<String, bool> = BTreeMap::new();
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

/// G-M1 AND G-M2 OVER THE SESSION'S OWN LOG `log` (as read: whole, or a tail that reaches the start).
pub fn quiet_log(log: &str, pid: u32, since_us: u64) -> Result<u64, Open> {
    let (at, from_us) = part_start(log, pid, since_us).ok_or(Open::Unproved)?;
    open_work(&log[at..], 0)?;
    Ok(from_us)
}

/// One look at a supervised Muse Code that is behind its twin's build, everything the quiet flow's restart waits on, read by the drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Look {
    /// A hold, a hand other than the harness's, unread input, or a person's keystroke within the grace (`upgrade_drive::held`).
    pub held: bool,
    /// Muse's reader reads the screen Idle, authoritatively, with no wall (its own background work reads busy, `aterm_phase::muse::phase`).
    pub idle: bool,
    /// Its composer holds nothing (a person's draft is theirs).
    pub composer_empty: bool,
    /// The session's log and its subagents' ([`quiet_log`], [`open_work`]).
    pub log: Result<(), Open>,
    /// The process's descendants.
    pub children: Option<usize>,
}

/// THE QUIET FLOW'S GATE.
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
