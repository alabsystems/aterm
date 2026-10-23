// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Subagent in-flight tool-call tracking and panel-row placement.

use std::sync::Arc;

use aterm_json::Value;
use aterm_phase::agents_panel::{PANEL_ROWS_MAX, PanelRow, RowKind, Span, text_hash};

/// Grace period past a tool call's budget before it reads overdue.
pub const CALL_SLACK_MS: u64 = 30_000;
/// Default budget for a tool call that declares no timeout.
pub const DEFAULT_BUDGET_MS: u64 = 120_000;
/// Maximum budget granted to a declared tool timeout.
pub const MAX_BUDGET_MS: u64 = 600_000;
/// Maximum in-flight tool calls tracked per assistant message.
pub const FLIGHT_MAX: usize = 8;
/// Allowed start-time slack past clock resolution when matching a panel row to a transcript.
pub const START_SLACK_MS: u64 = 2_000;
/// Maximum inline byte length of [`ToolName`].
pub const TOOL_NAME_MAX: usize = 31;
/// The tool whose `timeout` input is a budget Claude Code enforces.
const TIMED_TOOL: &str = "Bash";
/// Tool whose non-error result ends a workflow subagent.
pub const FINAL_TOOL: &str = "StructuredOutput";
/// The stop reasons, besides `end_turn`, a turn ends on.
const TURN_ENDS: [&str; 3] = ["stop_sequence", "max_tokens", "refusal"];

/// Inline tool name truncated at [`TOOL_NAME_MAX`] bytes with `…` when needed.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ToolName {
    len: u8,
    bytes: [u8; TOOL_NAME_MAX],
}

impl ToolName {
    /// Constructs a [`ToolName`], truncating with `…` if `name` exceeds [`TOOL_NAME_MAX`].
    #[must_use]
    pub fn of(name: &str) -> Self {
        const CUT: &str = "\u{2026}";
        let mut bytes = [0u8; TOOL_NAME_MAX];
        let len = if name.len() <= TOOL_NAME_MAX {
            bytes[..name.len()].copy_from_slice(name.as_bytes());
            name.len()
        } else {
            let mut end = TOOL_NAME_MAX - CUT.len();
            while !name.is_char_boundary(end) {
                end -= 1;
            }
            bytes[..end].copy_from_slice(&name.as_bytes()[..end]);
            bytes[end..end + CUT.len()].copy_from_slice(CUT.as_bytes());
            end + CUT.len()
        };
        Self {
            len: u8::try_from(len).unwrap_or(0),
            bytes,
        }
    }

    /// Returns the stored tool name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or("")
    }
}

impl std::fmt::Debug for ToolName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

/// One call in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Call {
    /// Hash of the `tool_use` ID.
    pub id: u64,
    pub tool: ToolName,
    /// Unix timestamp (ms) when the call row was written.
    pub at_ms: i64,
    /// Allowed execution budget in milliseconds.
    pub budget_ms: u64,
}

impl Call {
    /// Unix timestamp (ms) after which the call exceeds its budget plus slack.
    #[must_use]
    pub fn until_ms(&self) -> i64 {
        let more = i64::try_from(self.budget_ms + CALL_SLACK_MS).unwrap_or(i64::MAX);
        self.at_ms.saturating_add(more)
    }
}

/// In-flight state of a subagent transcript.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FileFlight {
    /// Indeterminate state (no rows yet, too many calls, or an oversized line).
    #[default]
    Unknown,
    /// No tool call in flight; waiting on the model's turn.
    Model,
    /// Newest assistant row ended the turn with nothing in flight.
    Done,
    /// Waiting on in-flight tool calls (carries the earliest-expiring [`Call`]).
    Tool(Call),
}

/// Incremental fold over a subagent transcript's in-flight tool calls.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlightFold {
    /// Unix ms of its first row with a time.
    start_ms: Option<i64>,
    /// The newest assistant message's id hash.
    message: u64,
    calls: [Option<Call>; FLIGHT_MAX],
    /// The newest message had more calls than kept, or a row could not be
    /// read: [`FileFlight::Unknown`] until the next message.
    lost: bool,
    /// The newest assistant row ended its turn.
    done: bool,
    /// An assistant row or a result was read.
    known: bool,
    /// Moved with every change of [`Self::flight`] or [`Self::start_ms`].
    generation: u64,
}

/// A `toolu_…` id's hash.
fn id_hash(id: &str) -> u64 {
    text_hash(id)
}

impl FlightFold {
    /// Creates an empty fold.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Unix timestamp (ms) of the transcript's first timestamped row.
    #[must_use]
    pub fn start_ms(&self) -> Option<i64> {
        self.start_ms
    }

    /// Generation counter incremented whenever the fold's output changes.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Current [`FileFlight`] state of the transcript.
    #[must_use]
    pub fn flight(&self) -> FileFlight {
        if self.lost || !self.known {
            return FileFlight::Unknown;
        }
        let first = self.calls.iter().flatten().min_by_key(|c| c.until_ms());
        match first {
            Some(call) => FileFlight::Tool(*call),
            None if self.done => FileFlight::Done,
            None => FileFlight::Model,
        }
    }

    /// Resets the fold when the transcript is rewritten and advances the generation.
    pub fn restart(&mut self) {
        let generation = self.generation.wrapping_add(1);
        *self = Self {
            generation,
            ..Self::default()
        };
    }

    /// Marks the transcript unknown until the next assistant message after an oversized line.
    pub fn long_line(&mut self) {
        let before = (self.flight(), self.start_ms);
        self.lost = true;
        self.moved(before);
    }

    /// Folds one complete JSONL row.
    pub fn fold_bytes(&mut self, line: &[u8]) {
        let Ok(line) = std::str::from_utf8(line) else {
            return;
        };
        if self.start_ms.is_some() && !wants(line) {
            // An assistant row not read whole: the model's turn, known.
            if !self.known && super::steps::quoted(line, "assistant") {
                let before = (self.flight(), self.start_ms);
                self.known = true;
                self.moved(before);
            }
            return;
        }
        let Ok(row) = super::transcript::flight(line) else {
            return;
        };
        let before = (self.flight(), self.start_ms);
        if self.start_ms.is_none() {
            self.start_ms = row
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(super::codex_rollout::stamp_ms);
        }
        let at = row
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(super::codex_rollout::stamp_ms);
        let message = row.get("message");
        let parts = message
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array);
        match row.get("type").and_then(Value::as_str) {
            Some("assistant") => {
                self.known = true;
                let id = message
                    .and_then(|m| m.get("id"))
                    .and_then(Value::as_str)
                    .map_or(0, id_hash);
                if id != self.message {
                    self.message = id;
                    self.calls = [None; FLIGHT_MAX];
                    self.lost = false;
                }
                let stop = message
                    .and_then(|m| m.get("stop_reason"))
                    .and_then(Value::as_str);
                self.done = matches!(stop, Some(s) if s != "tool_use");
                for part in parts.into_iter().flatten() {
                    if part.get("type").and_then(Value::as_str) == Some("tool_use") {
                        self.call(part, at);
                    }
                }
            }
            Some("user") => {
                for part in parts.into_iter().flatten() {
                    if part.get("type").and_then(Value::as_str) == Some("tool_result")
                        && let Some(id) = part.get("tool_use_id").and_then(Value::as_str)
                    {
                        self.known = true;
                        let id = id_hash(id);
                        let error = part.get("is_error").and_then(Value::as_bool) == Some(true);
                        for slot in &mut self.calls {
                            if let Some(call) = slot.filter(|c| c.id == id) {
                                *slot = None;
                                if call.tool.as_str() == FINAL_TOOL && !error {
                                    self.done = true;
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        self.moved(before);
    }

    /// Keep the call `part` (a `tool_use`), written at `at`.
    fn call(&mut self, part: &Value, at: Option<i64>) {
        let input = part.get("input");
        if input
            .and_then(|i| i.get("run_in_background"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            return;
        }
        let (Some(id), Some(at)) = (part.get("id").and_then(Value::as_str), at) else {
            self.lost = true;
            return;
        };
        let name = part.get("name").and_then(Value::as_str).unwrap_or("");
        let declared = (name == TIMED_TOOL)
            .then(|| input.and_then(|i| i.get("timeout")).and_then(Value::as_u64))
            .flatten()
            .filter(|&ms| ms > 0);
        let call = Call {
            id: id_hash(id),
            tool: ToolName::of(name),
            at_ms: at,
            budget_ms: declared.map_or(DEFAULT_BUDGET_MS, |ms| ms.min(MAX_BUDGET_MS)),
        };
        if self.calls.iter().flatten().any(|c| c.id == call.id) {
            return;
        }
        match self.calls.iter_mut().find(|slot| slot.is_none()) {
            Some(slot) => *slot = Some(call),
            None => self.lost = true,
        }
    }

    fn moved(&mut self, before: (FileFlight, Option<i64>)) {
        if (self.flight(), self.start_ms) != before {
            self.generation = self.generation.wrapping_add(1);
        }
    }
}

/// Whether a row's bytes may hold a call, a result or a turn's end: what the
/// fold parses once it has its start.
fn wants(line: &str) -> bool {
    super::steps::wants_parts(line)
        || super::steps::quoted(line, "end_turn")
        || TURN_ENDS.iter().any(|end| super::steps::quoted(line, end))
}

/// Normalizes panel row text by dropping control characters and collapsing whitespace.
#[must_use]
pub fn panel_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut space = false;
    for c in raw.chars().filter(|c| !c.is_control() || c.is_whitespace()) {
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    out
}

/// Expands wide glyphs with a trailing space to match one char per terminal cell.
#[must_use]
pub fn cells(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        out.push(c);
        if aterm_grapheme::char_width(c) == 2 {
            out.push(' ');
        }
    }
    out
}

/// Converts a workflow name into Claude Code's lowercase hyphenated script slug.
#[must_use]
pub fn workflow_slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut dash = false;
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(c);
        } else {
            dash = true;
        }
    }
    if out.is_empty() {
        out.push_str("workflow");
    }
    out
}

/// Candidate agent or workflow run for matching against an agents-panel row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Flyer {
    /// [`RowKind::Agent`] or [`RowKind::Workflow`].
    pub kind: RowKind,
    /// Expected [`RowKey::name_hash`] for the panel row.
    pub name_hash: u64,
    /// Normalized cell activity string for an agent (`None` for a workflow).
    pub activity: Option<Arc<str>>,
    /// Unix timestamp (ms) when the run started.
    pub start_ms: i64,
    pub flight: FileFlight,
}

impl Flyer {
    /// Builds a [`Flyer`] for an agent from its metadata and start time.
    #[must_use]
    pub fn agent(name: &str, description: Option<&str>, start_ms: i64, flight: FileFlight) -> Self {
        Self {
            kind: RowKind::Agent,
            name_hash: text_hash(&cells(&panel_text(name))),
            activity: description
                .map(|d| cells(&panel_text(d)))
                .filter(|d| !d.is_empty())
                .map(Arc::from),
            start_ms,
            flight,
        }
    }

    /// Builds a [`Flyer`] for a workflow run from its script slug and combined flight.
    #[must_use]
    pub fn workflow(slug: &str, start_ms: i64, flight: FileFlight) -> Self {
        Self {
            kind: RowKind::Workflow,
            name_hash: text_hash(slug),
            activity: None,
            start_ms,
            flight,
        }
    }
}

/// Combines subagent [`FileFlight`] states into a workflow run's aggregate state.
#[must_use]
pub fn combine(flights: impl IntoIterator<Item = FileFlight>) -> FileFlight {
    let mut first: Option<Call> = None;
    let mut model = false;
    for flight in flights {
        match flight {
            FileFlight::Unknown => return FileFlight::Unknown,
            FileFlight::Model => model = true,
            FileFlight::Done => {}
            FileFlight::Tool(call) => {
                if first.is_none_or(|c| call.until_ms() < c.until_ms()) {
                    first = Some(call);
                }
            }
        }
    }
    match (model, first) {
        (true, _) => FileFlight::Model,
        (false, Some(call)) => FileFlight::Tool(call),
        (false, None) => FileFlight::Done,
    }
}

/// Snapshot of a session's active agents and workflow runs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlightTable {
    pub flyers: Vec<Flyer>,
}

/// Parsed activity text of an agents-panel row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Activity {
    /// No activity drawn.
    #[default]
    None,
    /// Uncut activity text hash.
    Whole(u64),
    /// Ellipsis-truncated activity prefix length and hash.
    Cut { len: u16, hash: u64 },
}

/// Matching key extracted from an agents-panel task row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowKey {
    pub kind: RowKind,
    /// Hash of the agent's cell name or workflow's slugged name.
    pub name_hash: u64,
    pub activity: Activity,
    /// Drawn elapsed clock `(ms, resolution_ms)`.
    pub clock: Option<(u64, u32)>,
}

/// The text of columns `span` of `row` (one char per cell).
fn columns(row: &str, span: Span) -> Option<&str> {
    let mut at = row.char_indices().map(|(i, _)| i).chain([row.len()]);
    let start = at.nth(usize::from(span.start))?;
    let end = if span.is_empty() {
        start
    } else {
        at.nth(usize::from(span.len()) - 1)?
    };
    row.get(start..end)
}

impl RowKey {
    /// Extracts the [`RowKey`] for `parsed` from `row`, if it is an agent or workflow row.
    #[must_use]
    pub fn of(row: &str, parsed: &PanelRow) -> Option<Self> {
        if !matches!(parsed.kind, RowKind::Agent | RowKind::Workflow) {
            return None;
        }
        let name_hash = match parsed.kind {
            RowKind::Workflow => text_hash(&workflow_slug(columns(row, parsed.name)?)),
            _ => parsed.name_hash,
        };
        let activity = match parsed.activity {
            None => Activity::None,
            Some(span) => {
                let text = columns(row, span)?;
                match text.strip_suffix('\u{2026}') {
                    Some(head) => Activity::Cut {
                        len: u16::try_from(head.len()).ok()?,
                        hash: text_hash(head),
                    },
                    None => Activity::Whole(text_hash(text)),
                }
            }
        };
        Some(Self {
            kind: parsed.kind,
            name_hash,
            activity,
            clock: parsed.elapsed.and_then(|(_, e)| e.clock()),
        })
    }

    /// Whether `flyer` is this row's by name, activity and start at
    /// `wall_now_ms`.
    fn fits(&self, flyer: &Flyer, wall_now_ms: i64) -> bool {
        if flyer.kind != self.kind || flyer.name_hash != self.name_hash {
            return false;
        }
        let activity = match (self.activity, flyer.activity.as_deref()) {
            (Activity::None, None) => true,
            (Activity::Whole(hash), Some(text)) => text_hash(text) == hash,
            (Activity::Cut { len, hash }, Some(text)) => {
                let len = usize::from(len);
                len > 0
                    && len < text.len()
                    && text.is_char_boundary(len)
                    && text_hash(&text[..len]) == hash
            }
            _ => false,
        };
        let Some((ms, res)) = self.clock else {
            return false;
        };
        let start = i128::from(wall_now_ms) - i128::from(ms);
        let err = (start - i128::from(flyer.start_ms)).unsigned_abs();
        activity && err <= u128::from(u64::from(res) + START_SLACK_MS)
    }
}

/// Resolved in-flight state reported to the agents-panel tracker for one row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Flight {
    /// No unique transcript match or transcript state is unknown.
    #[default]
    Unknown,
    /// Matched transcript has no tool call in flight.
    Model,
    /// Matched transcript is waiting on `tool` call `call`.
    Tool {
        call: u64,
        tool: ToolName,
        age_ms: u64,
        left_ms: u64,
    },
}

/// Matches panel row `keys` against `table` at `wall_now_ms`.
#[must_use]
pub fn place(
    keys: &[Option<RowKey>],
    table: &FlightTable,
    wall_now_ms: i64,
) -> [Flight; PANEL_ROWS_MAX] {
    let mut found: [Option<usize>; PANEL_ROWS_MAX] = [None; PANEL_ROWS_MAX];
    for (slot, key) in found.iter_mut().zip(keys) {
        let Some(key) = key else { continue };
        let mut fits = table
            .flyers
            .iter()
            .enumerate()
            .filter(|(_, f)| key.fits(f, wall_now_ms))
            .map(|(i, _)| i);
        if let Some(one) = fits.next()
            && fits.next().is_none()
        {
            *slot = Some(one);
        }
    }
    let mut out = [Flight::Unknown; PANEL_ROWS_MAX];
    for (flight, &at) in out.iter_mut().zip(&found) {
        let Some(at) = at else { continue };
        if found.iter().filter(|&&other| other == Some(at)).count() > 1 {
            continue;
        }
        *flight = match table.flyers[at].flight {
            FileFlight::Unknown => Flight::Unknown,
            FileFlight::Model | FileFlight::Done => Flight::Model,
            FileFlight::Tool(call) => Flight::Tool {
                call: call.id,
                tool: call.tool,
                age_ms: u64::try_from(wall_now_ms.saturating_sub(call.at_ms)).unwrap_or(0),
                left_ms: u64::try_from(call.until_ms().saturating_sub(wall_now_ms)).unwrap_or(0),
            },
        };
    }
    out
}

#[cfg(test)]
#[path = "flight_tests.rs"]
mod tests;
