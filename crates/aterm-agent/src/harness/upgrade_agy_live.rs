// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pure quiet-flow gate and transcript/summary/wake inspection for live `agy` upgrades.

use std::collections::{BTreeMap, BTreeSet};

use aterm_json::Value;

pub use super::upgrade_muse_live::{build_version, moves_onto, order};

/// Condition in the conversation transcript that holds off a live `agy` restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Open {
    /// Missing process start timestamp or truncated transcript tail.
    Unproved,
    /// Unparseable transcript line, missing timestamp, or unreadable file.
    Unreadable,
    /// Background command, timer, or cron task still open.
    Task,
    /// Spawned subagent has neither gone idle nor been killed.
    Subagent,
}

impl Open {
    /// Wait reason word (`wait:<word>`).
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

/// Timestamp slack in seconds between `created_at` and process start.
pub const START_SLACK_S: u64 = 2;

/// The text a task's `RUNNING` row names its id after.
const TASK_STARTED: &str = "background task with task id: ";

/// The words of a subagent spawn's row.
const SUBAGENTS_CREATED: &str = "Created the following subagents:";

/// The words a `manage_subagents` kill's result row says.
const SUBAGENTS_KILLED: &str = "Successfully killed";

/// A tool call's argument as agy's transcript keeps it: each a JSON value
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
fn cron_of(content: &str) -> Option<(String, String)> {
    let line = content
        .lines()
        .find_map(|l| l.strip_prefix("Task Description: "))?;
    let (expr, prompt) = line.strip_prefix("Cron: ")?.split_once(", Prompt: ")?;
    Some((expr.to_owned(), prompt.to_owned()))
}

/// The id a `manage_task` kill's result row names cancelled (`Task "<id>"
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
fn gone_idle(content: &str) -> Option<String> {
    let (head, _) = content.split_once(" has gone idle")?;
    let id = head.rsplit("Subagent ").next()?;
    super::codex_rollout::is_thread_id(id).then(|| id.to_owned())
}

/// One background task open in the transcript.
#[derive(Debug)]
struct Task {
    /// A cron's `MaxIterations` (its schedule call's), `Some(None)` for a cron
    cron: Option<Option<u64>>,
    /// A cron's firings read so far.
    fired: u64,
}

/// A `manage_subagents` kill waiting on its result row.
enum Kill {
    All,
    These(Vec<String>),
}

/// Check that no background task or subagent remains open in `text` at or after `from_s`.
pub fn open_work(text: &str, from_s: u64) -> Result<(), Open> {
    let mut tasks: BTreeMap<String, Task> = BTreeMap::new();
    let mut subagents: BTreeSet<String> = BTreeSet::new();
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

/// Evaluate transcript quietness over `text` (full or `truncated` tail) for a process started at `started_ms`.
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

/// Summary row status value when a conversation is idle between turns.
pub const STATUS_IDLE: &str = "CASCADE_RUN_STATUS_IDLE";

/// Conversation summary row and child-subagent busy count from `conversation_summaries.db`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    /// Conversation row `status` string.
    pub status: String,
    /// Conversation row `not_fully_idle` flag.
    pub not_fully_idle: bool,
    /// Number of child subagent rows not in idle status.
    pub children_busy: usize,
}

/// Error returned when an `agy` summary query output, message file, or `read.json` is malformed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unreadable;

/// A `numeric` boolean column as sqlite prints it (`0`/`1`, `false`/`true`);
fn flag(text: &str) -> Option<bool> {
    match text.trim() {
        "0" | "false" => Some(false),
        "1" | "true" => Some(true),
        _ => None,
    }
}

/// Parse `sqlite3` summary query output `out` for `conversation`.
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

/// Evaluate the summary idle gate (`G-A1` / `G-A2(c)`), returning the wait reason on failure.
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

/// Settle window in milliseconds after a waking message file is written.
pub const WAKE_SETTLE_MS: u64 = 5_000;

/// The priority of a message that wakes nothing (the restart notice, a
const QUIET_PRIORITY: &str = "MESSAGE_PRIORITY_LOW";

/// Single inbox message file metadata and contents.
#[derive(Clone, Debug)]
pub struct Message {
    /// Message ID (filename stem referenced in `read.json`).
    pub id: String,
    /// Raw JSON file contents.
    pub body: String,
    /// File modification timestamp in Unix milliseconds.
    pub written_ms: u64,
}

/// Count unread or newly settled waking messages in `messages` against `read.json` (`read`).
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

/// Snapshot of all signals inspected by the live `agy` quiet-upgrade [`gate`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Look {
    /// Whether the session is held by a user or non-harness actor.
    pub held: bool,
    /// Background work indicator shown on screen (`aterm_phase::agy::background_shown`).
    pub shown: Option<&'static str>,
    /// Whether the screen phase reader reports `agy` authoritatively idle.
    pub idle: bool,
    /// Whether the prompt composer is empty.
    pub composer_empty: bool,
    /// Parsed conversation summary result ([`summary_rows`]).
    pub summary: Result<Option<Summary>, Unreadable>,
    /// Transcript open-work check result ([`quiet_transcript`]).
    pub transcript: Result<(), Open>,
    /// Number of live child processes (`None` if process table read failed).
    pub children: Option<usize>,
    /// Number of pending waking messages (`None` if inbox read failed).
    pub wakes: Option<usize>,
    /// Number of other live `agy` processes sharing the conversation (`None` if unknown).
    pub shared: Option<usize>,
}

/// Evaluate the quiet-upgrade gate over `l`, returning the first blocking wait word on `Err`.
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

/// What a character of a word sits inside, innermost last in
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

/// Return the first inline `NAME=VALUE` assignment prefixing the `agy` command in `cmdline`.
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
