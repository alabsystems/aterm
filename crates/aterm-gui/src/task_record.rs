// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Worker session task records (`task`, `task mark`, `await task`, `result`).

use std::path::{Component, Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::SessionCtx;
use crate::control::Scope;
use crate::turn_ledger::TurnBy;

/// State of a worker session's task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TaskState {
    /// Task is active; nothing to read yet.
    Working,
    /// Worker landed its work.
    Done,
    /// Agent exited with no relaunch coming.
    Exited,
    /// Worker stalled before showing its composer or is held without progress.
    Stalled,
}

impl TaskState {
    /// All states in usage-line order.
    pub(crate) const ALL: [Self; 4] = [Self::Working, Self::Done, Self::Exited, Self::Stalled];

    /// Wire word for this state.
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Done => "done",
            Self::Exited => "exited",
            Self::Stalled => "stalled",
        }
    }

    /// Parses a wire word into a [`TaskState`].
    #[must_use]
    pub(crate) fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == word)
    }

    /// Whether a waiter has a terminal state to read (every state except `working`).
    #[must_use]
    pub(crate) const fn is_terminal(self) -> bool {
        !matches!(self, Self::Working)
    }
}

/// `why=` value written when Autopilot Land lands the work.
pub(crate) const WHY_LANDED: &str = "landed";

/// Closed `why=` vocabulary for task state transitions.
pub(crate) const TASK_WHY: &[&str] = &[
    WHY_LANDED,
    "marker",
    "done-check",
    "exit",
    "shell",
    "startup",
    "held",
    "turn",
    "moved",
];

/// Default `from=` value when a task is started by `ctl` without a caller session ID.
pub(crate) const FROM_CTL: &str = "ctl";

/// Whether `from` is `ctl` or a valid global session ID (`s-<hex>`).
#[must_use]
pub(crate) fn valid_from(from: &str) -> bool {
    from == FROM_CTL
        || (from.len() <= 64
            && from
                .strip_prefix("s-")
                .is_some_and(|hex| !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit())))
}

/// Directory where task result files live (`<state>/tasks/`).
#[must_use]
pub(crate) fn tasks_dir() -> Option<PathBuf> {
    aterm_types::dirs::state_dir().map(|state| tasks_dir_under(&state))
}

/// [`tasks_dir`] under `state`.
#[must_use]
pub(crate) fn tasks_dir_under(state: &Path) -> PathBuf {
    state.join("tasks")
}

/// The wall clock in unix milliseconds (`0` before the epoch).
fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Per-session task record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskRecord {
    /// Monotonic revision incremented on record or supervisor attention changes.
    pub(crate) rev: u64,
    /// Current task state.
    pub(crate) state: TaskState,
    /// Origin of the task (a global session ID or [`FROM_CTL`]).
    pub(crate) from: String,
    /// Reason the task entered its current state ([`TASK_WHY`]).
    pub(crate) why: Option<&'static str>,
    /// Agent exit code for an `exited` task, if known.
    pub(crate) exit: Option<i32>,
    /// Size in bytes of the worker's result file (`0` when absent).
    pub(crate) result_bytes: u64,
    /// Path to the result file under [`tasks_dir`], if any.
    pub(crate) result_path: Option<PathBuf>,
    /// Whether supervisor attention stands on the session.
    pub(crate) attention: bool,
    /// Wall-clock timestamp (unix ms) when the current state was entered.
    pub(crate) since_unix_ms: u64,
    /// Monotonic instant when the current state was entered.
    pub(crate) since: Instant,
    /// Revision at which the current state was entered.
    pub(crate) entered: u64,
    /// Agent program started by `spawn agent=<word>`, if spawned as a worker.
    pub(crate) agent: Option<aterm_phase::Program>,
    /// Canonical `cwd` trusted by the starter when spawning the worker.
    pub(crate) trusted_cwd: Option<String>,
}

impl TaskRecord {
    /// Milliseconds elapsed since the current state was entered (`since_ms=`).
    #[must_use]
    pub(crate) fn since_ms(&self) -> u64 {
        u64::try_from(self.since.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Whether a waiter has news (a terminal state or standing attention).
    #[must_use]
    pub(crate) const fn has_news(&self) -> bool {
        self.state.is_terminal() || self.attention
    }

    /// Formats the record's wire reply fields.
    #[must_use]
    pub(crate) fn wire_fields(&self) -> String {
        let mut out = format!(
            "rev={} state={} from={} since_ms={} result_bytes={}",
            self.rev,
            self.state.as_str(),
            self.from,
            self.since_ms(),
            self.result_bytes
        );
        self.push_optional(&mut out);
        out
    }

    /// ` why= exit= result_path=`, each only when set.
    fn push_optional(&self, out: &mut String) {
        if let Some(why) = self.why {
            out.push_str(" why=");
            out.push_str(why);
        }
        if let Some(exit) = self.exit {
            out.push_str(&format!(" exit={exit}"));
        }
        if let Some(path) = &self.result_path {
            out.push_str(" result_path=");
            out.push_str(&crate::control::pct_encode(&path.to_string_lossy()));
        }
    }

    /// Formats the record for seamless-update handoff ([`crate::session_store::SessionRecord::task`]).
    #[must_use]
    #[cfg(any(unix, test))]
    pub(crate) fn carry_row(&self) -> String {
        let mut out = format!(
            "rev={} state={} from={} since={} result_bytes={} attention={}",
            self.rev,
            self.state.as_str(),
            self.from,
            self.since_unix_ms,
            self.result_bytes,
            u8::from(self.attention)
        );
        self.push_optional(&mut out);
        if let Some(agent) = self.agent {
            out.push_str(" agent=");
            out.push_str(agent.name());
        }
        if let Some(folder) = &self.trusted_cwd {
            out.push_str(" trusted_cwd=");
            out.push_str(&crate::control::pct_encode(folder));
        }
        out
    }

    /// Restores and validates a [`TaskRecord`] from a seamless-update carry row.
    #[must_use]
    pub(crate) fn from_carry_row(
        row: &str,
        tasks: Option<&Path>,
        now: Instant,
        now_unix_ms: u64,
    ) -> Option<Self> {
        let mut fields: Vec<(&str, &str)> = Vec::new();
        for (key, value) in row.split_whitespace().filter_map(|t| t.split_once('=')) {
            if fields.iter().any(|(k, _)| *k == key) {
                return None;
            }
            fields.push((key, value));
        }
        let field = |key: &str| fields.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        let since_unix_ms = field("since")?.parse::<u64>().ok()?;
        let age = std::time::Duration::from_millis(now_unix_ms.saturating_sub(since_unix_ms));
        let rev = field("rev")?.parse::<u64>().ok().filter(|rev| *rev > 0)?;
        Some(Self {
            rev,
            state: TaskState::parse(field("state")?)?,
            from: Some(field("from")?).filter(|f| valid_from(f))?.to_string(),
            why: match field("why") {
                Some(word) => Some(why_word(word)?),
                None => None,
            },
            exit: match field("exit") {
                Some(code) => Some(code.parse::<i32>().ok()?),
                None => None,
            },
            result_bytes: field("result_bytes")?.parse::<u64>().ok()?,
            result_path: match field("result_path") {
                Some(path) => Some(result_path(path, tasks)?),
                None => None,
            },
            attention: match field("attention")? {
                "0" => false,
                "1" => true,
                _ => return None,
            },
            since_unix_ms,
            since: now.checked_sub(age).unwrap_or(now),
            entered: rev,
            agent: field("agent").and_then(worker_agent),
            trusted_cwd: field("trusted_cwd").and_then(trusted_folder),
        })
    }
}

/// The folder a carried `trusted_cwd=` names: pct-decoded, absolute, spelled
/// as its own parts ([`spelled_as_its_parts`]: no `.`, `..`, doubled or
/// trailing separator), no control character, at most [`RESULT_PATH_MAX`]
/// bytes — the canonical form the spawn stored.
fn trusted_folder(value: &str) -> Option<String> {
    let decoded = aterm_codec::pct::decode(value);
    let path = Path::new(&decoded);
    (decoded.len() <= RESULT_PATH_MAX
        && !decoded.chars().any(char::is_control)
        && path.has_root()
        && path
            .components()
            .all(|c| !matches!(c, Component::ParentDir | Component::CurDir))
        && spelled_as_its_parts(path))
    .then(|| decoded.clone())
}

/// The agent a worker's `agent=` word names: one a spawn can start
/// (`aterm_agent::harness::worker::worker_cli`), else none.
fn worker_agent(word: &str) -> Option<aterm_phase::Program> {
    aterm_phase::Program::ALL
        .into_iter()
        .find(|p| p.name() == word)
        .filter(|p| aterm_agent::harness::worker::worker_cli(*p).is_some())
}

/// The [`TASK_WHY`] word `value` is, as its `'static` spelling.
fn why_word(value: &str) -> Option<&'static str> {
    TASK_WHY.iter().copied().find(|w| *w == value)
}

/// Parsed `task mark` transition and its associated metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskMark {
    /// Target state.
    pub(crate) state: TaskState,
    /// Task origin (`None` preserves the existing record's `from` or defaults to [`FROM_CTL`]).
    pub(crate) from: Option<String>,
    /// Transition reason ([`TASK_WHY`]).
    pub(crate) why: Option<&'static str>,
    /// Agent exit code.
    pub(crate) exit: Option<i32>,
    /// Result size in bytes.
    pub(crate) result_bytes: u64,
    /// Result file path under [`tasks_dir`].
    pub(crate) result_path: Option<PathBuf>,
}

/// Usage error line for `task`.
pub(crate) const TASK_USAGE: &str = "ERR usage: task [mark <working|done|exited|stalled> \
     [from=<s-…|ctl>] [why=<word>] [exit=<n>] [result_bytes=<n>] [result_path=<pct>]]\n";

impl TaskMark {
    /// Creates a mark to `state` with default fields.
    #[must_use]
    pub(crate) const fn new(state: TaskState) -> Self {
        Self {
            state,
            from: None,
            why: None,
            exit: None,
            result_bytes: 0,
            result_path: None,
        }
    }

    /// Parses `task mark` arguments against `tasks` directory.
    pub(crate) fn parse(args: &str, tasks: Option<&Path>) -> Result<Self, String> {
        let usage = || TASK_USAGE.to_string();
        let mut words = args.split_whitespace();
        let state = words.next().and_then(TaskState::parse).ok_or_else(usage)?;
        let mut mark = Self::new(state);
        let mut seen: Vec<&str> = Vec::new();
        for token in words {
            let (key, value) = token.split_once('=').ok_or_else(usage)?;
            if seen.contains(&key) {
                return Err(usage());
            }
            seen.push(key);
            match key {
                "from" if valid_from(value) => mark.from = Some(value.to_string()),
                "from" => {
                    return Err("ERR task mark: from= is a session id (s-<hex>) or ctl\n".into());
                }
                "why" => {
                    mark.why = Some(why_word(value).ok_or_else(|| {
                        format!("ERR task mark: why= is one of {}\n", TASK_WHY.join("|"))
                    })?);
                }
                "exit" => mark.exit = Some(value.parse::<i32>().map_err(|_| usage())?),
                "result_bytes" => mark.result_bytes = value.parse::<u64>().map_err(|_| usage())?,
                "result_path" => {
                    mark.result_path = Some(result_path(value, tasks).ok_or_else(|| {
                        "ERR task mark: result_path= names a file under <state>/tasks/\n"
                            .to_string()
                    })?);
                }
                _ => return Err(usage()),
            }
        }
        Ok(mark)
    }

    /// Applies this mark to `current` at `now` and `now_unix_ms`, returning `(record, moved)`.
    #[must_use]
    pub(crate) fn apply(
        &self,
        current: Option<&TaskRecord>,
        attention: bool,
        now: Instant,
        now_unix_ms: u64,
    ) -> (TaskRecord, bool) {
        let from = self
            .from
            .clone()
            .or_else(|| current.map(|r| r.from.clone()))
            .unwrap_or_else(|| FROM_CTL.to_string());
        let Some(current) = current else {
            let record = TaskRecord {
                rev: 1,
                state: self.state,
                from,
                why: self.why,
                exit: self.exit,
                result_bytes: self.result_bytes,
                result_path: self.result_path.clone(),
                attention,
                since_unix_ms: now_unix_ms,
                since: now,
                entered: 1,
                agent: None,
                trusted_cwd: None,
            };
            return (record, true);
        };
        let same = current.state == self.state
            && current.from == from
            && current.why == self.why
            && current.exit == self.exit
            && current.result_bytes == self.result_bytes
            && current.result_path == self.result_path
            && current.attention == attention;
        let mut next = current.clone();
        if same {
            return (next, false);
        }
        next.rev = next.rev.saturating_add(1);
        if next.state != self.state {
            next.since = now;
            next.since_unix_ms = now_unix_ms;
            next.entered = next.rev;
        }
        next.state = self.state;
        next.from = from;
        next.why = self.why;
        next.exit = self.exit;
        next.result_bytes = self.result_bytes;
        next.result_path.clone_from(&self.result_path);
        next.attention = attention;
        (next, true)
    }
}

/// Maximum decoded byte length of `result_path=` (`PATH_MAX`).
pub(crate) const RESULT_PATH_MAX: usize = 1024;

/// A `result_path=` value as a path: pct-decoded, at most
/// [`RESULT_PATH_MAX`] bytes, absolute, spelled the one way its components
/// spell it (no `.`, no `//`, no trailing `/`; [`spelled_as_its_parts`]), and
/// naming a file strictly inside `tasks` with no `..` or control character on
/// the way.
fn result_path(value: &str, tasks: Option<&Path>) -> Option<PathBuf> {
    let tasks = tasks?;
    let decoded = aterm_codec::pct::decode(value);
    if decoded.len() > RESULT_PATH_MAX || decoded.chars().any(char::is_control) {
        return None;
    }
    let path = PathBuf::from(decoded);
    if !spelled_as_its_parts(&path) {
        return None;
    }
    let inside = path.strip_prefix(tasks).ok()?;
    let mut parts = inside.components().peekable();
    parts.peek()?;
    parts
        .all(|c| matches!(c, Component::Normal(_)))
        .then_some(path)
}

/// Whether `path` is spelled as the path its own components rebuild: byte for
/// byte off Windows, so a `.`, a `//` or a trailing `/` is a second spelling
/// of one file.
fn spelled_as_its_parts(path: &Path) -> bool {
    let rebuilt: PathBuf = path.components().collect();
    #[cfg(windows)]
    {
        let one_separator = |p: &Path| p.to_string_lossy().replace('/', "\\");
        one_separator(&rebuilt) == one_separator(path)
    }
    #[cfg(not(windows))]
    {
        rebuilt.as_os_str() == path.as_os_str()
    }
}

/// Whether an attention entry written by `owner` updates the task record's attention flag.
pub(crate) fn owner_moves_task(owner: &str) -> bool {
    owner == crate::harness_host::ATTENTION_OWNER
        || owner == crate::harness_host::NEEDS_ATTENTION_OWNER
}

/// Whether supervisor or worker `NEEDS YOU:` attention stands in `meta`.
pub(crate) fn supervisor_attention(meta: &crate::session_timeline::SessionMeta) -> bool {
    [
        crate::harness_host::ATTENTION_OWNER,
        crate::harness_host::NEEDS_ATTENTION_OWNER,
    ]
    .iter()
    .any(|owner| meta.attention_owners.get(owner).is_some())
}

/// Error returned when a mark times out waiting on a committing update's session fence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MarkFenced;

/// Applies `mark` to `ctx`'s task record, waking waiters and recording timeline events.
pub(crate) fn mark_session_task(
    ctx: &SessionCtx,
    local_id: u64,
    mark: &TaskMark,
) -> Result<(TaskRecord, bool), MarkFenced> {
    mark_session_task_within(ctx, local_id, mark, ctx.fabric.fence_wait())
}

/// [`mark_session_task`] waiting at most `fence_wait` on a committing update's fence.
pub(crate) fn mark_session_task_within(
    ctx: &SessionCtx,
    local_id: u64,
    mark: &TaskMark,
    fence_wait: std::time::Duration,
) -> Result<(TaskRecord, bool), MarkFenced> {
    let marked = mark_session_task_if(ctx, local_id, mark, fence_wait, |_| true)?;
    Ok(marked.expect("an unconditional mark is applied"))
}

/// [`mark_session_task_within`] applied only when `when` holds on the current record.
pub(crate) fn mark_session_task_if(
    ctx: &SessionCtx,
    local_id: u64,
    mark: &TaskMark,
    fence_wait: std::time::Duration,
    when: impl FnOnce(Option<&TaskRecord>) -> bool,
) -> Result<Option<(TaskRecord, bool)>, MarkFenced> {
    let marked = mark_session_task_with(
        ctx,
        local_id,
        Some(mark),
        AttentionEdit::Keep,
        fence_wait,
        |record, _| when(record),
    )?;
    Ok(marked.and_then(|m| m.record.map(|record| (record, m.moved))))
}

/// Edit to apply to the supervisor's attention entry alongside a task mark.
#[derive(Clone, Copy)]
pub(crate) enum AttentionEdit<'a> {
    /// Leave the attention entry unchanged.
    Keep,
    /// Raise the attention entry with the given text.
    Raise(&'a str),
    /// Clear the attention entry if the predicate holds on the standing text.
    TakeDown(fn(Option<&str>) -> bool),
}

/// Outcome of [`mark_session_task_with`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MarkedWith {
    /// Resulting task record, if any.
    pub(crate) record: Option<TaskRecord>,
    /// Whether the task record revision advanced.
    pub(crate) moved: bool,
    /// Whether the supervisor's attention entry changed.
    pub(crate) attention_moved: bool,
}

/// Atomically applies `mark` and `attention` under the session timeline lock when `when` holds.
pub(crate) fn mark_session_task_with(
    ctx: &SessionCtx,
    local_id: u64,
    mark: Option<&TaskMark>,
    edit: AttentionEdit<'_>,
    fence_wait: std::time::Duration,
    when: impl FnOnce(Option<&TaskRecord>, Option<&str>) -> bool,
) -> Result<Option<MarkedWith>, MarkFenced> {
    let outcome = ctx
        .fabric
        .run_unfenced(fence_wait, || {
            let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let owner = crate::harness_host::ATTENTION_OWNER;
            if !when(timeline.task(), meta.attention_owners.get(owner)) {
                return None;
            }
            let attention_moved = match edit {
                AttentionEdit::Keep => false,
                AttentionEdit::Raise(text) => crate::session_timeline::put_supervisor_attention(
                    &mut meta,
                    &mut timeline,
                    Some(text),
                ),
                AttentionEdit::TakeDown(stands) => {
                    stands(meta.attention_owners.get(owner))
                        && crate::session_timeline::put_supervisor_attention(
                            &mut meta,
                            &mut timeline,
                            None,
                        )
                }
            };
            let attention = supervisor_attention(&meta);
            let (record, moved) = match mark {
                Some(mark) => {
                    let (record, moved) =
                        timeline.mark_task(mark, attention, Instant::now(), unix_ms_now());
                    (Some(record), moved)
                }
                None => {
                    let moved = timeline.note_task_attention(attention);
                    (timeline.task().cloned(), moved)
                }
            };
            timeline.slog_take_wake();
            drop(timeline);
            drop(meta);
            Some(MarkedWith {
                record,
                moved,
                attention_moved,
            })
        })
        .ok_or(MarkFenced)?;
    let record_moved = outcome
        .as_ref()
        .is_some_and(|m| m.moved || m.attention_moved);
    if record_moved {
        crate::presence::post_task_changed(local_id);
    }
    if outcome
        .as_ref()
        .is_some_and(|m| m.moved && m.record.as_ref().is_some_and(|r| r.entered == r.rev))
    {
        crate::harness_host::ring();
    }
    Ok(outcome)
}

/// Reopens a finished worker's task to `working` when a new turn is sent to it.
pub(crate) fn reopen_at_turn(ctx: &SessionCtx, local_id: u64, by: &TurnBy, found: Option<u64>) {
    let is_done = |r: Option<&TaskRecord>| {
        r.is_some_and(|r| r.state == TaskState::Done && Some(r.entered) == found)
    };
    if *by == TurnBy::Harness
        || !is_done(
            ctx.timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .task(),
        )
    {
        return;
    }
    let mut mark = TaskMark::new(TaskState::Working);
    mark.why = Some("turn");
    let fence = ctx.fabric.fence_wait();
    if mark_session_task_if(ctx, local_id, &mark, fence, is_done).is_err() {
        aterm_log::warn!(
            "task: a turn into a finished worker was not marked working: an update is committing"
        );
    }
}

/// Usage error line for `result`.
pub(crate) const RESULT_USAGE: &str = "ERR usage: result [full]\n";

/// Handles `result [full]` on `ctx` (session `local_id`).
pub(crate) fn cmd_result(ctx: &SessionCtx, local_id: u64, rest: &str) -> String {
    let full = match rest.trim() {
        "" => false,
        "full" => true,
        _ => return RESULT_USAGE.to_string(),
    };
    let record = ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .task()
        .cloned();
    let Some(record) = record else {
        return "ERR no task\n".to_string();
    };
    match (&record.result_path, record.state) {
        (Some(path), TaskState::Done | TaskState::Exited) => {
            match aterm_agent::harness::result::reread(path) {
                Some(found) => {
                    repair_result(ctx, local_id, &record, path, &found);
                    found.answer(path, full)
                }
                None => aterm_agent::harness::result::answer(path, full),
            }
        }
        (_, TaskState::Done) => "ERR result unavailable why=no-snapshot\n".to_string(),
        (_, state) => format!("ERR result unavailable why={}\n", state.as_str()),
    }
}

/// THE MISSED RESULT, KEPT: the record `record` (as `result` read it) marked
/// again with `found`'s size as its `result_bytes=` — every other fact as it
/// stands, a new rev, its waiters woken — then the snapshot at `path` and its
/// sidecar written over with it, so `task`, `await task` and every later
/// `result` agree.
fn repair_result(
    ctx: &SessionCtx,
    local_id: u64,
    record: &TaskRecord,
    path: &Path,
    found: &aterm_agent::harness::result::Reread,
) {
    let mark = TaskMark {
        state: record.state,
        from: Some(record.from.clone()),
        why: record.why,
        exit: record.exit,
        result_bytes: found.bytes(),
        result_path: record.result_path.clone(),
    };
    let same = |now: Option<&TaskRecord>| now.is_some_and(|now| now.rev == record.rev);
    match mark_session_task_if(ctx, local_id, &mark, std::time::Duration::ZERO, same) {
        Ok(Some(_)) => {
            if let Err(e) = found.write(path) {
                aterm_log::warn!(
                    "task: the result read again was not written over {}: {e}",
                    path.display()
                );
            }
        }
        Ok(None) => {}
        Err(MarkFenced) => aterm_log::warn!(
            "task: the result read again is not kept while an update commits; the next `result` \
             reads it again"
        ),
    }
}

/// Restores a carried task record onto `ctx` during seamless-update adoption.
pub(crate) fn seed_carried(ctx: &SessionCtx, row: Option<&str>) {
    let Some(row) = row else {
        return;
    };
    let Some(mut record) =
        TaskRecord::from_carry_row(row, tasks_dir().as_deref(), Instant::now(), unix_ms_now())
    else {
        aterm_log::warn!(
            "task: the carried task record does not read ({row:?}); the session crosses without one"
        );
        return;
    };
    record.attention = supervisor_attention(&ctx.meta.lock().unwrap_or_else(|p| p.into_inner()));
    ctx.timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .seed_task(record);
    crate::session_timeline::keep_worker_autopilot(ctx);
}

/// Returns the session's published agent word and percent-encoded attention text.
pub(crate) fn agent_and_attention(ctx: &SessionCtx) -> (&'static str, String) {
    let attention = ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .attention
        .as_deref()
        .map_or_else(|| "-".to_string(), crate::control::pct_encode);
    let agent = ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .agent()
        .word;
    (agent, attention)
}

/// The `task` reply line for `record`, unterminated: `OK task -` with none,
/// else `OK task <fields> agent= attention=`.
fn task_reply(ctx: &SessionCtx, record: Option<&TaskRecord>) -> String {
    let Some(record) = record else {
        return "OK task -".to_string();
    };
    let (agent, attention) = agent_and_attention(ctx);
    format!(
        "OK task {} agent={agent} attention={attention}",
        record.wire_fields()
    )
}

/// Handles `task` and `task mark …` on `ctx` (session `local_id`).
pub(crate) fn cmd_task(ctx: &SessionCtx, local_id: u64, scope: Scope, rest: &str) -> String {
    let rest = rest.trim();
    if rest.is_empty() {
        let record = ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .task()
            .cloned();
        return format!("{}\n", task_reply(ctx, record.as_ref()));
    }
    let Some(args) = rest
        .strip_prefix("mark")
        .filter(|a| a.is_empty() || a.starts_with(' '))
    else {
        return TASK_USAGE.to_string();
    };
    if !scope.is_owner_class() {
        return "ERR denied\n".to_string();
    }
    let parsed = match TaskMark::parse(args, tasks_dir().as_deref()) {
        Ok(parsed) => parsed,
        Err(reply) => return reply,
    };
    let deadline_stall =
        parsed.state == TaskState::Stalled && matches!(parsed.why, Some("startup" | "held"));
    let edit = if deadline_stall {
        AttentionEdit::Keep
    } else {
        AttentionEdit::TakeDown(crate::harness_host::deadline_attention_stands)
    };
    let Ok(marked) = mark_session_task_with(
        ctx,
        local_id,
        Some(&parsed),
        edit,
        ctx.fabric.fence_wait(),
        |_, _| true,
    ) else {
        return crate::fabric::HOLD_BUSY.to_string();
    };
    let marked = marked.expect("an unconditional mark is applied");
    if marked.attention_moved {
        crate::presence::post_meta_changed(local_id);
    }
    format!(
        "{} changed={}\n",
        task_reply(ctx, marked.record.as_ref()),
        u8::from(marked.moved)
    )
}

/// How an `await task` over multiple targets completes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Combine {
    /// Completes when the first target has news.
    Any,
    /// Completes when every target has news.
    All,
}

/// Maximum number of targets allowed in a single `await task` request.
pub(crate) const MAX_AWAIT_TARGETS: usize = 64;

/// Usage error line for `await task`.
pub(crate) const AWAIT_TASK_USAGE: &str = "ERR usage: await task [@<sel>[:<rev>][,@<sel>…]] \
     [any|all] [since=<rev>] [timeout=<ms>]\n";

/// Single target selector and optional per-target `since` revision for `await task`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TargetArg {
    /// Selector body after `@`.
    pub(crate) selector: String,
    /// Per-target `since` revision override.
    pub(crate) since: Option<u64>,
}

/// Parsed `await task` arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AwaitTaskArgs {
    /// Target list (empty for the single-session form).
    pub(crate) targets: Vec<TargetArg>,
    /// Completion mode (`any` or `all`).
    pub(crate) combine: Combine,
    /// Default `since` revision for targets without an explicit `:rev`.
    pub(crate) since: Option<u64>,
    /// Optional timeout in milliseconds.
    pub(crate) timeout_ms: Option<u64>,
}

/// Parses `await task` arguments after the `task` word.
pub(crate) fn parse_await_task(words: &[&str], list: bool) -> Result<AwaitTaskArgs, String> {
    let usage = || AWAIT_TASK_USAGE.to_string();
    let mut args = AwaitTaskArgs {
        targets: Vec::new(),
        combine: Combine::Any,
        since: None,
        timeout_ms: None,
    };
    let (mut combine_seen, mut list_seen) = (false, false);
    let mut words = words.iter().copied();
    while let Some(word) = words.next() {
        if word.starts_with('@') {
            if !list || list_seen {
                return Err(usage());
            }
            list_seen = true;
            for element in word.split(',') {
                let body = element.strip_prefix('@').ok_or_else(usage)?;
                let (selector, since) = match body.split_once(':') {
                    Some((selector, rev)) => {
                        (selector, Some(rev.parse::<u64>().map_err(|_| usage())?))
                    }
                    None => (body, None),
                };
                if !aterm_types::control_verbs::selector_body_names_something(selector) {
                    return Err(format!("{}\n", aterm_types::control_verbs::EMPTY_SELECTOR));
                }
                if args.targets.len() == MAX_AWAIT_TARGETS {
                    return Err(format!(
                        "ERR too many await task targets (max {MAX_AWAIT_TARGETS})\n"
                    ));
                }
                args.targets.push(TargetArg {
                    selector: selector.to_string(),
                    since,
                });
            }
        } else if matches!(word, "any" | "all") {
            if combine_seen {
                return Err(usage());
            }
            combine_seen = true;
            args.combine = if word == "all" {
                Combine::All
            } else {
                Combine::Any
            };
        } else if let Some(rev) = word.strip_prefix("since=") {
            if args.since.is_some() {
                return Err(usage());
            }
            args.since = Some(rev.parse::<u64>().map_err(|_| usage())?);
        } else {
            let value = match word.strip_prefix("timeout=") {
                Some(value) => value,
                None if word == "timeout" => words.next().ok_or_else(usage)?,
                None => return Err(usage()),
            };
            if args.timeout_ms.is_some() {
                return Err(usage());
            }
            args.timeout_ms = Some(aterm_control::wire::wait_ms(value).ok_or_else(usage)?);
        }
    }
    Ok(args)
}

/// Formats a single `await task` reply row.
#[must_use]
pub(crate) fn await_row(
    sid: &str,
    record: Option<&TaskRecord>,
    agent: &str,
    attention: &str,
    closed: bool,
) -> String {
    let (rev, state, result_bytes) =
        record.map_or((0, "-", 0), |r| (r.rev, r.state.as_str(), r.result_bytes));
    let state = if closed { "closed" } else { state };
    format!(
        "@{sid} rev={rev} state={state} agent={agent} attention={attention} result_bytes={result_bytes}"
    )
}

/// Formats `task_state=` and `task_rev=` tokens for `status` and `sessions`.
#[must_use]
pub(crate) fn status_tokens(record: Option<&TaskRecord>) -> String {
    match record {
        Some(r) => format!("task_state={} task_rev={}", r.state.as_str(), r.rev),
        None => "task_state=- task_rev=0".to_string(),
    }
}

#[cfg(test)]
#[path = "task_record_tests.rs"]
mod tests;
