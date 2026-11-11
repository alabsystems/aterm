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
    pub const fn word(self) -> &'static str {
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

/// The words of a subagent spawn's row: agy 1.4.0's `invoke_subagent` took a
/// list and wrote `subagents` even for one; agy 1.4.10's makes one and writes
/// `subagent` (its binary holds both templates).
const SUBAGENTS_CREATED: [&str; 2] = [
    "Created the following subagents:",
    "Created the following subagent:",
];

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
    let rest = content
        .lines()
        .take(3)
        .find_map(|l| l.split_once(TASK_STARTED))?
        .1;
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
    let rest = content
        .lines()
        .take(4)
        .find_map(|l| l.trim().strip_prefix("Task \""))?;
    let (id, tail) = rest.split_once('"')?;
    tail.trim_start().starts_with("cancelled").then_some(id)
}

/// The first message's sender and first content line in a system envelope. Message bodies
/// are unescaped: later `[Message]` lines can be tool output, so they cannot retire other work.
fn message_header(content: &str) -> Option<(&str, &str)> {
    let messages = content.strip_prefix("<SYSTEM_MESSAGE>\n").or_else(|| {
        content
            .split_once("\n<SYSTEM_MESSAGE>\n")
            .map(|(_, rest)| rest)
    })?;
    let first = messages
        .lines()
        .next()?
        .strip_prefix("[Message] timestamp=")?;
    let (fields, body) = first.split_once(" content=")?;
    let (timestamp, rest) = fields.split_once(" sender=")?;
    let (sender, priority) = rest.split_once(" priority=")?;
    if timestamp.is_empty()
        || sender.is_empty()
        || sender.contains(char::is_whitespace)
        || priority.is_empty()
        || priority.contains(char::is_whitespace)
    {
        return None;
    }
    Some((sender, body))
}

/// The system sender's own idle notice, before any multiline payload.
fn gone_idle(sender: &str, body: &str) -> Option<String> {
    if sender != "system" {
        return None;
    }
    let (id, _) = body
        .strip_prefix("Subagent ")?
        .split_once(" has gone idle")?;
    super::codex_rollout::is_thread_id(id).then(|| id.to_owned())
}

/// One background task open in the transcript.
#[derive(Debug)]
struct Task {
    /// A cron's `MaxIterations` (its schedule call's), `Some(None)` for a cron
    /// with none: it runs until it is cancelled.
    cron: Option<Option<u64>>,
    /// A cron's firings read so far.
    fired: u64,
}

/// A `manage_subagents` kill waiting on its result row.
enum Kill {
    All,
    These(Vec<String>),
}

#[derive(Default)]
struct OpenWorkFold {
    tasks: BTreeMap<String, Task>,
    subagents: BTreeSet<String>,
    schedules: Vec<(String, String, Option<u64>)>,
    kill: Option<Kill>,
}

impl OpenWorkFold {
    fn step(&mut self, trimmed: &str, from_s: u64) -> Result<(), Open> {
        let row: Value = aterm_json::from_str(trimmed).map_err(|_| Open::Unreadable)?;
        let created = row
            .get("created_at")
            .and_then(Value::as_str)
            .and_then(super::unix_time::parse_utc)
            .ok_or(Open::Unreadable)?;
        if created < from_s {
            return Ok(());
        }
        let kind = row.get("type").and_then(Value::as_str).unwrap_or_default();
        let content = row
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match kind {
            "PLANNER_RESPONSE" => {
                self.kill = None;
                let calls = row.get("tool_calls").and_then(Value::as_array);
                for call in calls.into_iter().flatten() {
                    match call.get("name").and_then(Value::as_str) {
                        Some("schedule") => {
                            let text_arg =
                                |n: &str| arg(call, n).and_then(|v| v.as_str().map(str::to_owned));
                            if let (Some(expr), Some(prompt)) =
                                (text_arg("CronExpression"), text_arg("Prompt"))
                            {
                                let max = arg(call, "MaxIterations").and_then(|v| v.as_u64());
                                self.schedules.push((expr, prompt, max));
                            }
                        }
                        // Generic result rows carry no call identity. Only one tool call can
                        // be paired with its next result; a batch stays open until idle notices.
                        Some("manage_subagents") if calls.is_some_and(|calls| calls.len() == 1) => {
                            let action = arg(call, "Action");
                            self.kill = match action.as_ref().and_then(Value::as_str) {
                                Some("kill_all") => Some(Kill::All),
                                Some("kill") => Some(Kill::These(
                                    arg(call, "ConversationIds")
                                        .map(|v| {
                                            uuids(&aterm_json::to_string(&v).unwrap_or_default())
                                        })
                                        .unwrap_or_default(),
                                )),
                                _ => None,
                            };
                        }
                        _ => {}
                    }
                }
            }
            "GENERIC" => {
                // A failed result spends the pending call too: a later unrelated success
                // cannot answer it. RUNNING rows have not supplied a terminal result yet.
                let pending = if row.get("status").and_then(Value::as_str) == Some("RUNNING") {
                    None
                } else {
                    self.kill.take()
                };
                if row.get("status").and_then(Value::as_str) == Some("RUNNING")
                    && let Some(id) = started_task(content)
                {
                    let cron = cron_of(content).map(|(expr, prompt)| {
                        self.schedules
                            .iter()
                            .position(|(e, p, _)| *e == expr && *p == prompt)
                            .and_then(|at| self.schedules.remove(at).2)
                    });
                    self.tasks.insert(id.to_owned(), Task { cron, fired: 0 });
                }
                if let Some(id) = cancelled_task(content) {
                    self.tasks.remove(id);
                }
                if let Some(head) = content
                    .lines()
                    .take(4)
                    .map(str::trim)
                    .find(|l| SUBAGENTS_CREATED.contains(l))
                    && let Some((_, spawned)) = content.split_once(head)
                {
                    self.subagents.extend(
                        spawned
                            .lines()
                            .filter(|l| l.contains("\"conversationId\""))
                            .flat_map(uuids),
                    );
                }
                if content
                    .lines()
                    .take(4)
                    .any(|l| l.starts_with(SUBAGENTS_KILLED))
                {
                    match pending {
                        Some(Kill::All) => self.subagents.clear(),
                        Some(Kill::These(ids)) => {
                            for id in ids {
                                self.subagents.remove(&id);
                            }
                        }
                        None => {}
                    }
                }
            }
            "SYSTEM_MESSAGE" => {
                if let Some((from, body)) = message_header(content) {
                    if let Some(task) = self.tasks.get_mut(from) {
                        let ended = match task.cron {
                            None => true,
                            Some(max) => {
                                task.fired += 1;
                                max.is_some_and(|m| task.fired >= m)
                            }
                        };
                        if ended {
                            self.tasks.remove(from);
                        }
                    }
                    if let Some(id) = gone_idle(from, body) {
                        self.subagents.remove(&id);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(self) -> Result<(), Open> {
        if !self.tasks.is_empty() {
            return Err(Open::Task);
        }
        if !self.subagents.is_empty() {
            return Err(Open::Subagent);
        }
        Ok(())
    }
}

/// THE PROMPTS `text`, A TRANSCRIPT OR ITS TAIL, WROTE AT OR AFTER `from_s`, oldest first: what was
/// said to the conversation since, in the words it was said (`<USER_REQUEST>`'s), a line of the
/// harness's among them, as the excerpt's reader drops it. A torn line says nothing.
#[must_use]
pub fn prompts_since(text: &str, from_s: u64) -> Vec<String> {
    use super::excerpt::{agy_user_request, clean_prompt_words};
    use super::upgrade::{HARNESS_MARK, typed_words};
    text.lines()
        .filter(|line| line.contains("USER_INPUT"))
        .filter_map(|line| aterm_json::from_str::<Value>(line.trim()).ok())
        .filter_map(|row| {
            let at = super::unix_time::parse_utc(row.get("created_at")?.as_str()?)?;
            if at < from_s
                || row.get("type")?.as_str()? != "USER_INPUT"
                || row.get("source")?.as_str()? != "USER_EXPLICIT"
            {
                return None;
            }
            let request = agy_user_request(row.get("content")?.as_str()?).trim();
            if typed_words(request).starts_with(HARNESS_MARK) {
                Some(typed_words(request).to_owned())
            } else {
                clean_prompt_words(request)
            }
        })
        .collect()
}

/// Check that no background task or subagent remains open in `text` at or after `from_s`.
pub fn open_work(text: &str, from_s: u64) -> Result<(), Open> {
    let mut fold = OpenWorkFold::default();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        fold.step(line, from_s)?;
    }
    fold.finish()
}

/// Evaluate transcript quietness over `text` (full or `truncated` tail) for a process started at `started_ms`.
pub fn quiet_transcript(text: &str, truncated: bool, started_ms: u64) -> Result<(), Open> {
    quiet_transcript_reader(
        std::io::Cursor::new(text.as_bytes()),
        usize::MAX,
        truncated,
        started_ms,
    )
}

/// Evaluate transcript quietness by streaming lines from `reader` (full or `truncated` tail, with
/// each raw line bounded to `max_line_bytes`) for a process started at `started_ms`.
pub fn quiet_transcript_reader<R: std::io::BufRead>(
    mut reader: R,
    max_line_bytes: usize,
    truncated: bool,
    started_ms: u64,
) -> Result<(), Open> {
    use std::io::{BufRead as _, Read as _};
    if started_ms == 0 {
        return Err(Open::Unproved);
    }
    let from_s = (started_ms / 1000).saturating_sub(START_SLACK_S);
    let mut fold = OpenWorkFold::default();
    let mut checked_first = !truncated;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let limit = u64::try_from(max_line_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let n = (&mut reader)
            .take(limit)
            .read_until(b'\n', &mut buf)
            .map_err(|_| Open::Unreadable)?;
        if n == 0 {
            break;
        }
        if buf.len() > max_line_bytes && buf.last() != Some(&b'\n') {
            return Err(Open::Unreadable);
        }
        let line = String::from_utf8_lossy(&buf);
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !checked_first {
            let first = aterm_json::from_str::<Value>(trimmed).ok().and_then(|row| {
                row.get("created_at")
                    .and_then(Value::as_str)
                    .and_then(super::unix_time::parse_utc)
            });
            if first.is_none_or(|at| at >= from_s) {
                return Err(Open::Unproved);
            }
            checked_first = true;
        }
        fold.step(trimmed, from_s)?;
    }
    if !checked_first {
        return Err(Open::Unproved);
    }
    fold.finish()
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
/// `None` for anything else.
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
/// cancelled task's end): it waits for the person's next message.
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
/// or backtick holds.
fn simple_commands(line: &str) -> Vec<Vec<String>> {
    let mut commands: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut nest: Vec<Nest> = Vec::new();
    let mut escaped = false;
    let mut chars = line.chars();
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
            '\\' => {
                if chars.as_str().starts_with('\n') {
                    chars.next();
                    continue;
                }
                if chars.as_str().starts_with("\r\n") {
                    chars.next();
                    chars.next();
                    continue;
                }
                escaped = true;
            }
            '"' if quoted => {
                nest.pop();
            }
            '"' => nest.push(Nest::Double),
            '\'' if !quoted => nest.push(Nest::Single),
            '$' if chars.as_str().starts_with('(') => {
                word.push(ch);
                word.extend(chars.next());
                nest.push(Nest::Paren);
                continue;
            }
            '$' if chars.as_str().starts_with('{') => {
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

fn is_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The name a word assigns (`NAME=…`, with an unquoted `=` and its name unquoted), or `None`.
fn assigned_name(word: &str) -> Option<String> {
    let (mut single, mut double, mut escaped) = (false, false, false);
    let mut eq_at = None;
    for (idx, ch) in word.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if !single => escaped = true,
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '$' | '`' if !single => return None,
            '=' if !single && !double => {
                eq_at = Some(idx);
                break;
            }
            _ => {}
        }
    }
    let lhs = unquoted(&word[..eq_at?]);
    is_env_name(&lhs).then_some(lhs)
}

/// `word` with its quotes and escapes taken off.
fn unquoted(word: &str) -> String {
    let mut out = String::new();
    let (mut single, mut double, mut escaped) = (false, false, false);
    let mut chars = word.chars();
    while let Some(ch) = chars.next() {
        match ch {
            _ if escaped => {
                out.push(ch);
                escaped = false;
            }
            '\\' if !single => {
                if chars.as_str().starts_with('\n') {
                    chars.next();
                } else if chars.as_str().starts_with("\r\n") {
                    chars.next();
                    chars.next();
                } else {
                    escaped = true;
                }
            }
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            c => out.push(c),
        }
    }
    out
}

/// Return the first inline `NAME=VALUE` assignment prefixing `program` in `cmdline`
/// whose variable name is not in `carried`.
#[must_use]
pub(super) fn inline_env_for(cmdline: &str, program: &str, carried: &[&str]) -> Option<String> {
    for words in simple_commands(cmdline) {
        let mut set: Option<String> = None;
        let mut under_env = false;
        for word in &words {
            let bare = unquoted(word);
            let name = assigned_name(word).or_else(|| {
                under_env
                    .then(|| {
                        bare.split_once('=')
                            .map(|(n, _)| n)
                            .filter(|n| is_env_name(n))
                            .map(str::to_owned)
                    })
                    .flatten()
            });
            if let Some(name) = name {
                if !carried.contains(&name.as_str()) && set.is_none() {
                    set = Some(name);
                }
                continue;
            }
            let base = bare.rsplit('/').next().unwrap_or(&bare);
            if base == program {
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

/// Return the first inline `NAME=VALUE` assignment prefixing the `agy` command in `cmdline`.
#[must_use]
pub fn inline_env(cmdline: &str) -> Option<String> {
    inline_env_for(
        cmdline,
        super::upgrade_agy::PROGRAM,
        &[super::upgrade_agy::HOME_ENV],
    )
}

#[cfg(test)]
#[path = "upgrade_agy_live_tests.rs"]
mod tests;
