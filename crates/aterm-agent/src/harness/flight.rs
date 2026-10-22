// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! IN-FLIGHT TOOL CALLS: what a quiet dot of Claude Code's agents panel waits
//! on (`docs/DESIGN-harness-autopilot-2026-10-15.md` §10.6.13).
//!
//! The panel's fold ([`super::agents`]) reads work off the screen alone, and
//! a row whose tokens have not moved for [`super::agents::STUCK_MS`] reads
//! stuck. A subagent inside one long tool call is working, not stuck.
//! MEASURED on Claude Code 2.1.304 (the lab, 2026-10-20; the screens are
//! `aterm-phase`'s `claude-2.1.304-agents-panel-*long-bash*` fixtures, the
//! rows `harness/testdata/flight/`): a background agent's 300 s Bash call
//! (`timeout: 400000`) held its row at `↓ 6.2k tokens` from its second
//! second to its fifth minute, a workflow's 200 s call held the workflow's
//! row at `↓ 12.7k tokens`, and neither transcript grew during its call (no
//! progress rows): the call's row, then its result's 300.26 s later.
//!
//! THE FOLD. Every subagent transcript the session's usage fold follows
//! (`super::session_usage`: one read of what each file appends, no other
//! file read for it and no watcher) feeds a [`FlightFold`] too: the time of
//! its first row (the agent's start: within 0.2 s of the start its panel
//! clock implies, measured) and the calls of its newest assistant message
//! that no `tool_result` has answered. A new message answers every call of
//! the ones before it (the API takes the next turn only after a result for
//! each), so at most one message's calls are kept, [`FLIGHT_MAX`] of them; a
//! message with more, or a row too long to read, leaves the file
//! [`FileFlight::Unknown`] until the next message. A call that went to the
//! background (`run_in_background: true`; its result is written at once,
//! 33 ms measured) is never in flight. Only rows whose bytes hold a call's or
//! a result's key, a `\u` escape or a turn's end (`end_turn`, and the other
//! stop reasons a turn ends on: `stop_sequence`, `max_tokens`, `refusal`)
//! are parsed, by a projection that keeps no argument but the two below and
//! no result's content; any other assistant row says, by its kind alone,
//! that the model's turn is known. A workflow's agent ENDS on its answered
//! [`FINAL_TOOL`] call, with no assistant row after it: MEASURED on one run
//! of 22 agents, 18 ended so and 4 on a `stop_sequence` text row, and before
//! either was read every one of them read the model's turn for good, which
//! held its run's row stuck beside a sibling's long call.
//!
//! ITS OWN BUDGET. A call may run for its declared timeout: Bash's `timeout`
//! (ms, at most [`MAX_BUDGET_MS`], Claude Code's `BASH_MAX_TIMEOUT_MS`
//! default), else [`DEFAULT_BUDGET_MS`] (Bash's default, and every other
//! tool's: none declares one Claude Code enforces), plus [`CALL_SLACK_MS`].
//! A call with no declared timeout outlives its budget at 150 s, where the
//! screen's rule already turns a dot stuck: the budget only ever buys a call
//! the time it asked for.
//!
//! THE MAPPING ([`place`]): a panel row is a file's only by EXACT agreement
//! — its kind; its name, `name ?? agentType` of the file's `meta.json` for
//! an agent (a workflow's: the run's script, `<slug>-<run>.js`, the panel's
//! name slugged as Claude Code slugs it, [`workflow_slug`]); its activity,
//! the meta's `description` whole or cut with `…` (Claude Code's own
//! normalisation, [`panel_text`], in the pane's cells, [`cells`]); and its
//! clock's implied start within the clock's resolution plus
//! [`START_SLACK_MS`] of the file's first row (a workflow's script's mtime).
//! Exactly one file must agree, and no other row may take it; else the row
//! is [`Flight::Unknown`] and the screen's rules stand alone — no row reads
//! busier than its screen shows unless its own file says so.

use std::sync::Arc;

use aterm_json::Value;
use aterm_phase::agents_panel::{PANEL_ROWS_MAX, PanelRow, RowKind, Span, text_hash};

/// The slack past a call's own budget before it reads overdue: the 30 s the
/// screen's rule gives Bash's two-minute default (`agents::STUCK_MS`).
pub const CALL_SLACK_MS: u64 = 30_000;
/// The budget of a call that declares none: Bash's default timeout.
pub const DEFAULT_BUDGET_MS: u64 = 120_000;
/// The most a declared timeout buys: Bash's maximum.
pub const MAX_BUDGET_MS: u64 = 600_000;
/// The calls of one message a file keeps.
pub const FLIGHT_MAX: usize = 8;
/// The slack, past the clock's resolution, a row's implied start may differ
/// from its file's and still be that file's: measured within 0.2 s, and a
/// panel read up to a repaint after Claude drew it.
pub const START_SLACK_MS: u64 = 2_000;
/// The bytes of a tool's name kept ([`ToolName`]).
pub const TOOL_NAME_MAX: usize = 31;
/// The tool whose `timeout` input is a budget Claude Code enforces.
const TIMED_TOOL: &str = "Bash";
/// The tool whose answered call (not an error) ends a workflow's agent: its
/// schema output, the run's result.
pub const FINAL_TOOL: &str = "StructuredOutput";
/// The stop reasons, besides `end_turn`, a turn ends on.
const TURN_ENDS: [&str; 3] = ["stop_sequence", "max_tokens", "refusal"];

/// A tool's name, inline: at most [`TOOL_NAME_MAX`] bytes. A longer name (an
/// MCP tool's `mcp__<server>__<tool>`) is cut at a char boundary and ends
/// with `…`, so the title that names it (`in mcp__claude_ai_Claude_Docs__… 4m`)
/// never shows a cut name as if it were whole, and a cut name never equals a
/// whole one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ToolName {
    len: u8,
    bytes: [u8; TOOL_NAME_MAX],
}

impl ToolName {
    /// `name`, whole where it fits, else cut to fit with `…` after the cut.
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

    /// The name as kept.
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
    /// Its `tool_use` id's hash.
    pub id: u64,
    pub tool: ToolName,
    /// Unix ms its row was written.
    pub at_ms: i64,
    /// What it may run for (module docs: ITS OWN BUDGET).
    pub budget_ms: u64,
}

impl Call {
    /// Unix ms past which it has outlived its budget and the slack.
    #[must_use]
    pub fn until_ms(&self) -> i64 {
        let more = i64::try_from(self.budget_ms + CALL_SLACK_MS).unwrap_or(i64::MAX);
        self.at_ms.saturating_add(more)
    }
}

/// What one subagent's transcript says it is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FileFlight {
    /// Nothing certain: no call row read yet, a message with more calls than
    /// kept, a row too long to read.
    #[default]
    Unknown,
    /// No call of its own in flight: the model's turn.
    Model,
    /// Its newest assistant row ended its turn, nothing in flight.
    Done,
    /// Waiting on calls of its own: the first to outlive its budget.
    Tool(Call),
}

/// One subagent transcript's calls in flight (module docs: THE FOLD).
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
    /// A fold that has read nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Unix ms of the file's first row with a time.
    #[must_use]
    pub fn start_ms(&self) -> Option<i64> {
        self.start_ms
    }

    /// Moved with every change of what the fold says.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// What the file says as of the rows read.
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

    /// The fold starts over (the file was rewritten): what it said is gone,
    /// and its generation moves on.
    pub fn restart(&mut self) {
        let generation = self.generation.wrapping_add(1);
        *self = Self {
            generation,
            ..Self::default()
        };
    }

    /// A row too long to read: whatever it was, the file is unknown until its
    /// next message.
    pub fn long_line(&mut self) {
        let before = (self.flight(), self.start_ms);
        self.lost = true;
        self.moved(before);
    }

    /// Fold one complete row.
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
                    // A new message: every call of the ones before it was
                    // answered (or abandoned) before it was asked for.
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
                                // The agent's result, answered: it is done.
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

/// Claude Code's own normalisation of a panel row's text (2.1.304's `Fr`):
/// control characters dropped, every run of white space one space, trimmed.
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

/// `text` as the pane's text holds it: one char per cell, a wide glyph's
/// right half a space of its own.
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

/// A workflow's name as Claude Code names its script (2.1.304's `Xae`):
/// lower case, every run of characters other than `a-z0-9` one `-`, none at
/// either end; `workflow` when nothing is left.
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

/// One agent or workflow run a panel row may be (module docs: THE MAPPING).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Flyer {
    /// [`RowKind::Agent`] or [`RowKind::Workflow`].
    pub kind: RowKind,
    /// The hash its row's name must have: an agent's name in cells, a
    /// workflow's slug ([`RowKey::name_hash`]).
    pub name_hash: u64,
    /// An agent's activity in cells ([`panel_text`], [`cells`]); `None` for
    /// a workflow, whose row draws a bar.
    pub activity: Option<Arc<str>>,
    /// Unix ms it started.
    pub start_ms: i64,
    pub flight: FileFlight,
}

impl Flyer {
    /// An agent's: its meta's `name ?? agentType` and `description` as
    /// written (none, or one that normalises to nothing: a row with no
    /// activity), started at `start_ms`.
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

    /// A workflow run's: its script's slug, started at `start_ms`, its
    /// agents' flights combined ([`combine`]).
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

/// A workflow run's flight from its agents': unknown if any is; the model's
/// while any is in its turn; else waiting on the first call to outlive its
/// budget; else done.
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

/// A session's agents and workflow runs, as its usage fold last read them
/// (`session_usage::SessionUsage::flights`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlightTable {
    pub flyers: Vec<Flyer>,
}

/// What a row's activity reads as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Activity {
    /// None drawn.
    #[default]
    None,
    /// Whole: its hash.
    Whole(u64),
    /// Cut with `…`: the bytes before it, and their hash.
    Cut { len: u16, hash: u64 },
}

/// What [`place`] matches one task row by, read off the pane's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowKey {
    pub kind: RowKind,
    /// An agent's name's hash; a workflow's slugged name's.
    pub name_hash: u64,
    pub activity: Activity,
    /// The clock as drawn, `(ms, res_ms)`.
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
    /// The key of `parsed`, read off `row` (the pane's text, one char per
    /// cell, as `aterm_phase::agents_panel::one_cell` reads it): `None` for
    /// a row that is no agent or workflow.
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

/// What the host tells the panel's fold of one row's own calls
/// (`agents::PanelTrack::fold_flown`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Flight {
    /// No file is certainly the row's, or its file is unknown: the screen's
    /// rules alone.
    #[default]
    Unknown,
    /// Its file has no call in flight.
    Model,
    /// Its file waits on `call` (its id's hash), of `tool`, written `age_ms`
    /// ago, which outlives its budget `left_ms` from now (`0`: already).
    Tool {
        call: u64,
        tool: ToolName,
        age_ms: u64,
        left_ms: u64,
    },
}

/// Each row's [`Flight`] (`keys[i]` the key of the panel's row `i`, `None`
/// for a row that is no task; past [`PANEL_ROWS_MAX`] rows none), from
/// `table` at `wall_now_ms` (module docs: THE MAPPING). Fixed arrays,
/// nothing allocated.
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
        // Two rows that agree with one file: neither is certainly its.
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
