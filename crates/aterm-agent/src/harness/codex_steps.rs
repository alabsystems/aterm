// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CODEX STEP FOLD.

use std::collections::VecDeque;
use std::path::Path;

use aterm_json::Value;
use aterm_phase::WallKind;
use aterm_phase::anchors::anchor_text;

use super::codex_rollout::{
    FORK_BURST_MS, Prefix, RolloutRead, Streamed, counts_of, is_thread_id, stamp_ms,
    stream_lines_ends,
};
use super::steps::{
    End, STEP_QUEUE, Step, StepBatch, StepEvent, StepId, StepPos, StepState, TURN_IDS_MAX,
    ToolName, number, quoted, state_for,
};
use super::transcript;

/// The codex-alab builds whose rollouts the lab measured and reduced (`harness/testdata/steps/<stem>-<leg>.jsonl`).
pub const CODEX_MEASURED_BUILDS: &[&str] = &[
    "0.164.0-dev.12521+g3d3eb12d8",
    "0.164.0-dev.12479+g07a25d86a",
    "0.164.0-dev.12672+g089291d37",
];

/// The build `build` as the log says it (`ver=`).
#[must_use]
pub fn log_build(build: &str) -> Option<StepId> {
    StepId::new(build).or_else(|| {
        build
            .split_once('+')
            .and_then(|(release, _)| StepId::new(release))
    })
}

const SESSION_META: &str = anchor_text("rollout.session_meta");
const TURN_CONTEXT: &str = anchor_text("rollout.turn_context");
const EVENT_MSG: &str = anchor_text("rollout.event_msg");
const RESPONSE_ITEM: &str = anchor_text("rollout.response_item");
const COMPACTED: &str = anchor_text("rollout.compacted");
const TASK_COMPLETE: &str = anchor_text("rollout.task_complete");
const TURN_ABORTED: &str = anchor_text("rollout.turn_aborted");
const SETTINGS_APPLIED: &str = anchor_text("rollout.thread_settings_applied");
const TOKEN_COUNT: &str = anchor_text("rollout.token_count");
const FUNCTION_CALL: &str = anchor_text("rollout.function_call");
const FUNCTION_OUTPUT: &str = anchor_text("rollout.function_call_output");
const CUSTOM_CALL: &str = anchor_text("rollout.custom_tool_call");
const CUSTOM_OUTPUT: &str = anchor_text("rollout.custom_tool_call_output");
const KEY_ID: &str = anchor_text("rollout.id");
const KEY_FORKED_FROM: &str = anchor_text("rollout.forked_from_id");
const KEY_CLI_VERSION: &str = anchor_text("rollout.cli_version");
const KEY_THREAD_ID: &str = anchor_text("rollout.thread_id");
const KEY_TURN_ID: &str = anchor_text("rollout.turn_id");
const KEY_CALL_ID: &str = anchor_text("rollout.call_id");
const KEY_NAME: &str = anchor_text("rollout.name");
const KEY_NAMESPACE: &str = anchor_text("rollout.namespace");
const KEY_MODEL: &str = anchor_text("rollout.model");
const KEY_EFFORT: &str = anchor_text("rollout.effort");
const KEY_DURATION_MS: &str = anchor_text("rollout.duration_ms");
const KEY_ERROR: &str = anchor_text("rollout.error");
const KEY_ERROR_INFO: &str = anchor_text("rollout.codex_error_info");
const USAGE_LIMIT: &str = anchor_text("rollout.usage_limit_exceeded");
const COLLABORATION: &str = anchor_text("rollout.collaboration");
const SPAWN_AGENT: &str = anchor_text("rollout.spawn_agent");
const EXEC_COMMAND: &str = anchor_text("rollout.exec_command");

/// The kinds of row past a copy the fold opens: any other is skipped by its
/// raw bytes, unparsed (a quote inside a JSON string is escaped, so a
/// quoted match is a key or a closed word).
const OPENED: [&str; 9] = [
    TURN_CONTEXT,
    TASK_COMPLETE,
    TURN_ABORTED,
    TOKEN_COUNT,
    FUNCTION_CALL,
    FUNCTION_OUTPUT,
    CUSTOM_CALL,
    CUSTOM_OUTPUT,
    COMPACTED,
];

/// The class of a Codex call, by its namespace and NAME (addendum A11), and whether it starts a subagent.
#[must_use]
pub fn tool_class(namespace: Option<&str>, name: &str) -> (&'static str, bool) {
    match (namespace, name) {
        (Some(COLLABORATION), SPAWN_AGENT) => ("agent", true),
        (None, EXEC_COMMAND) => ("shell", false),
        _ => ("other", false),
    }
}

/// A copy that has read no stamp yet ([`Prefix::Copy`]): its burst starts
/// at the first stamp read.
const NO_STAMP: i64 = i64::MAX;

/// An id as the fold compares it (never kept).
const fn fingerprint(id: &str) -> u64 {
    aterm_hash::fnv1a_64(id.as_bytes())
}

fn word<'a>(v: Option<&'a Value>, key: &str) -> Option<&'a str> {
    v?.get(key)?.as_str()
}

/// THE CODEX STEP FOLD of one rollout.
#[derive(Clone, Debug)]
pub struct CodexStepFold {
    /// The process's start (wall ms): older rows are history.
    floor_ms: u64,
    /// The rollout bound: its path's fingerprint and identity.
    bound: Option<(u64, Option<(u64, u64)>)>,
    /// Rows ending at or below this are history.
    seed: u64,
    /// Where the fold stands against a copy.
    prefix: Prefix,
    /// While a fork's copy is read, its own thread's fingerprint: its own
    /// `thread_settings_applied` ends the copy.
    thread: Option<u64>,
    /// The build its own `session_meta` names, as the log says it
    /// ([`log_build`]).
    version: Option<StepId>,
    /// THE WRITER: the build of the process writing the rollout now, where
    /// its reader knows it ([`Self::set_writer`]), its state and how the
    /// log says it.
    writer: Option<(StepState, Option<StepId>)>,
    /// The state its rows are read at: [`StepState::On`] only on a build
    /// [`CODEX_MEASURED_BUILDS`] names, matched whole.
    state: StepState,
    /// The largest running total read, a copy's included.
    top: u64,
    /// The open turn's id's fingerprint.
    open: Option<u64>,
    /// The turn ids opened, newest last, at most [`TURN_IDS_MAX`].
    opened: VecDeque<u64>,
    /// Tokens since the last turn ended or was interrupted.
    tok_in: u64,
    tok_out: u64,
    /// The model and effort last said.
    model: Option<(StepId, Option<StepId>)>,
    /// Latched off for the bound rollout.
    broken: bool,
    queue: VecDeque<Step>,
    overflow: u64,
    lost: u64,
}

impl CodexStepFold {
    /// A fold whose rows older than `floor_ms` (wall ms) are history.
    #[must_use]
    pub const fn new(floor_ms: u64) -> CodexStepFold {
        CodexStepFold {
            floor_ms,
            bound: None,
            seed: 0,
            prefix: Prefix::Start,
            thread: None,
            version: None,
            writer: None,
            state: StepState::Partial,
            top: 0,
            open: None,
            opened: VecDeque::new(),
            tok_in: 0,
            tok_out: 0,
            model: None,
            broken: false,
            queue: VecDeque::new(),
            overflow: 0,
            lost: 0,
        }
    }

    /// Bind the rollout at `fp` (its identity `id`).
    pub fn bind(&mut self, fp: u64, id: Option<(u64, u64)>, seed: u64) {
        self.bind_build(fp, id, seed, None);
    }

    /// [`Self::bind`] a rollout whose `session_meta` the binder read already names `build` (the tap's gate, `super::codex_tap`).
    pub fn bind_build(&mut self, fp: u64, id: Option<(u64, u64)>, seed: u64, build: Option<&str>) {
        *self = CodexStepFold {
            queue: std::mem::take(&mut self.queue),
            overflow: self.overflow,
            lost: self.lost,
            bound: Some((fp, id)),
            seed,
            state: state_for(CODEX_MEASURED_BUILDS, build),
            writer: self.writer,
            ..CodexStepFold::new(self.floor_ms)
        };
        let at_ms = self.floor_ms;
        self.enqueue(
            StepPos {
                fp,
                id,
                off: seed,
                at_ms,
            },
            StepEvent::Bind { len: seed },
        );
    }

    /// Whether a rollout is bound.
    #[must_use]
    pub const fn is_bound(&self) -> bool {
        self.bound.is_some()
    }

    /// Raise the floor to `floor_ms` (wall ms; it only rises).
    pub fn raise_floor(&mut self, floor_ms: u64) {
        self.floor_ms = self.floor_ms.max(floor_ms);
    }

    /// Record the `build` that the process writing the rollout runs now (`super::codex_alab::alab_build`).
    pub fn set_writer(&mut self, build: &str) {
        let writer = (
            state_for(CODEX_MEASURED_BUILDS, Some(build)),
            log_build(build),
        );
        if self.writer.is_some_and(|w| w != writer) {
            self.model = None;
        }
        self.writer = Some(writer);
    }

    /// The reader stops reading the bound rollout at `end`, the end of the last line it pushed (its writer runs a build nobody measured).
    pub fn stop(&mut self, end: u64) {
        if let Some((fp, id)) = self.bound
            && !self.broken
        {
            self.latch_as(fp, id, end, "unmeasured");
        }
    }

    /// Whether the fold latched off.
    #[must_use]
    pub const fn broken(&self) -> bool {
        self.broken
    }

    /// Everything queued since the last take.
    pub fn take(&mut self) -> StepBatch {
        StepBatch {
            steps: self.queue.drain(..).collect(),
            overflow: std::mem::take(&mut self.overflow),
            lost: std::mem::take(&mut self.lost),
            end: End::default(),
        }
    }

    /// A line the reader could not hand on, ending at `end`.
    pub const fn lost_line(&mut self, end: u64) {
        if self.bound.is_some() && end > self.seed {
            self.lost = self.lost.saturating_add(1);
        }
    }

    /// Fold one complete line of the bound rollout, ending at offset `end` (past its newline).
    pub fn push(&mut self, line: &[u8], end: u64) {
        let Some((fp, id)) = self.bound else {
            return;
        };
        if self.broken {
            return;
        }
        let fresh = end > self.seed;
        let Ok(text) = std::str::from_utf8(line) else {
            self.lost_line(end);
            return;
        };
        let t = text.trim();
        if t.is_empty()
            || (self.prefix == Prefix::Own
                && !OPENED.iter().any(|k| quoted(t, k))
                && !t.contains("\\u"))
        {
            return;
        }
        let Ok(v) = transcript::codex_steps(t) else {
            self.lost_line(end);
            return;
        };
        let stamp = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(stamp_ms);
        if !self.past_copy(&v, stamp) {
            return;
        }
        let at_ms = stamp.and_then(|s| u64::try_from(s).ok());
        let live = fresh && at_ms.is_some_and(|t| t >= self.floor_ms);
        let mut out: Vec<StepEvent> = Vec::new();
        let bad = self.row(&v, fresh, live, &mut out);
        if !fresh {
            return;
        }
        if bad || (!out.is_empty() && at_ms.is_none()) {
            self.latch(fp, id, end);
            return;
        }
        let Some(at_ms) = at_ms.filter(|&t| t >= self.floor_ms) else {
            return;
        };
        let state = self.state();
        let k = out.len() as u64;
        for (i, ev) in out.into_iter().enumerate() {
            let off = end.saturating_sub(k - 1 - i as u64);
            self.enqueue_as(StepPos { fp, id, off, at_ms }, state, ev);
        }
    }

    /// Where the row stands against a copy: `true` when it is
    /// the thread's own and is read on, `false` when it steps nothing (a
    /// copied row, the first row, the line that ends a copy).
    fn past_copy(&mut self, v: &Value, stamp: Option<i64>) -> bool {
        let kind = v.get("type").and_then(Value::as_str);
        let payload = v.get("payload");
        let ptype = word(payload, "type");
        match self.prefix {
            Prefix::Own => return true,
            Prefix::Start => {
                if kind == Some(SESSION_META) {
                    let build = word(payload, KEY_CLI_VERSION);
                    self.state = state_for(CODEX_MEASURED_BUILDS, build);
                    self.version = build.and_then(log_build);
                    let fork = word(payload, KEY_FORKED_FROM).is_some_and(|p| !p.is_empty());
                    if !fork {
                        self.prefix = Prefix::Own;
                        return false;
                    }
                    self.thread = word(payload, KEY_ID)
                        .filter(|t| is_thread_id(t))
                        .map(fingerprint);
                }
                self.prefix = Prefix::Copy(stamp.unwrap_or(NO_STAMP));
            }
            Prefix::Copy(newest) => {
                let own = kind == Some(EVENT_MSG)
                    && ptype == Some(SETTINGS_APPLIED)
                    && self.thread.is_some()
                    && word(payload, KEY_THREAD_ID).map(fingerprint) == self.thread;
                if own {
                    self.prefix = Prefix::Own;
                    self.thread = None;
                    return false;
                }
                match stamp {
                    Some(at) if newest != NO_STAMP && at.saturating_sub(newest) > FORK_BURST_MS => {
                        self.prefix = Prefix::Own;
                        self.thread = None;
                        return true;
                    }
                    Some(at) if newest == NO_STAMP => self.prefix = Prefix::Copy(at),
                    Some(at) => self.prefix = Prefix::Copy(newest.max(at)),
                    None => {}
                }
            }
        }
        if kind == Some(EVENT_MSG)
            && ptype == Some(TOKEN_COUNT)
            && let Some((_, total, _)) = counts_of(payload)
        {
            self.top = self.top.max(total);
        }
        false
    }

    /// The steps of one of the thread's own rows, into `out`; `true` when a
    /// key it reads has the wrong shape.
    fn row(&mut self, v: &Value, fresh: bool, live: bool, out: &mut Vec<StepEvent>) -> bool {
        let kind = v.get("type").and_then(Value::as_str);
        let payload = v.get("payload");
        let ptype = word(payload, "type");
        match (kind, ptype) {
            (Some(TURN_CONTEXT), _) => {
                let Some(turn) = word(payload, KEY_TURN_ID) else {
                    return true;
                };
                let Some(vref) = StepId::new(turn) else {
                    return true;
                };
                let f = fingerprint(turn);
                if !self.opened.contains(&f) {
                    if self.opened.len() == TURN_IDS_MAX {
                        self.opened.pop_front();
                    }
                    self.opened.push_back(f);
                    self.open = Some(f);
                    out.push(StepEvent::Prompt { vref });
                }
                if let Some(model) = word(payload, KEY_MODEL)
                    .filter(|m| live && super::upgrade::is_model_id(m))
                    .and_then(StepId::new)
                {
                    let effort = word(payload, KEY_EFFORT).and_then(StepId::new);
                    if self.model != Some((model, effort)) {
                        self.model = Some((model, effort));
                        out.push(StepEvent::Model {
                            model,
                            effort,
                            ver: self.writer.map_or(self.version, |(_, ver)| ver),
                        });
                    }
                }
            }
            (Some(EVENT_MSG), Some(TASK_COMPLETE)) => {
                let Some(turn) = word(payload, KEY_TURN_ID) else {
                    return true;
                };
                if self.open != Some(fingerprint(turn)) {
                    return false;
                }
                let Some(vdur_ms) = number(payload.and_then(|p| p.get(KEY_DURATION_MS))) else {
                    return true;
                };
                out.push(StepEvent::TurnEnd {
                    vdur_ms,
                    tok_in: self.tok_in,
                    tok_out: self.tok_out,
                });
                let error = payload.and_then(|p| p.get(KEY_ERROR));
                if word(error, KEY_ERROR_INFO) == Some(USAGE_LIMIT) {
                    out.push(StepEvent::EndWall {
                        kind: WallKind::UsageSession,
                    });
                }
                self.ended();
            }
            (Some(EVENT_MSG), Some(TURN_ABORTED)) => {
                let Some(turn) = word(payload, KEY_TURN_ID) else {
                    return true;
                };
                if self.open == Some(fingerprint(turn)) {
                    out.push(StepEvent::Interrupt);
                    self.ended();
                }
            }
            (Some(EVENT_MSG), Some(TOKEN_COUNT)) => {
                if let Some((_, total, last)) = counts_of(payload)
                    && total > self.top
                {
                    self.top = total;
                    self.tok_in = self.tok_in.saturating_add(last.input);
                    self.tok_out = self.tok_out.saturating_add(last.output);
                }
            }
            (Some(RESPONSE_ITEM), Some(FUNCTION_CALL | CUSTOM_CALL)) => {
                let Some(call) = word(payload, KEY_CALL_ID) else {
                    return true;
                };
                let Some(id) = StepId::new(call) else {
                    self.lost = self.lost.saturating_add(u64::from(fresh));
                    return false;
                };
                let name = word(payload, KEY_NAME).unwrap_or("");
                let (cls, subagent) = tool_class(word(payload, KEY_NAMESPACE), name);
                out.push(StepEvent::ToolUse {
                    id,
                    tool: ToolName::new(name),
                    cls,
                    subagent,
                });
            }
            (Some(RESPONSE_ITEM), Some(FUNCTION_OUTPUT | CUSTOM_OUTPUT)) => {
                let Some(call) = word(payload, KEY_CALL_ID) else {
                    return true;
                };
                match StepId::new(call) {
                    Some(id) => out.push(StepEvent::ToolResult { id, error: false }),
                    None => self.lost = self.lost.saturating_add(u64::from(fresh)),
                }
            }
            (Some(COMPACTED), _) => out.push(StepEvent::Compact),
            _ => {}
        }
        false
    }

    /// The open turn ended or was interrupted: its tokens start over.
    const fn ended(&mut self) {
        self.open = None;
        self.tok_in = 0;
        self.tok_out = 0;
    }

    fn latch(&mut self, fp: u64, id: Option<(u64, u64)>, end: u64) {
        self.latch_as(fp, id, end, "unknown");
    }

    fn latch_as(&mut self, fp: u64, id: Option<(u64, u64)>, end: u64, why: &'static str) {
        self.broken = true;
        let at_ms = self.floor_ms;
        self.enqueue(
            StepPos {
                fp,
                id,
                off: end,
                at_ms,
            },
            StepEvent::Off { why },
        );
    }

    /// The state of the rows now (rule): the thread's build's,
    /// and never `On` while its writer runs a build nobody measured
    /// ([`Self::set_writer`]).
    const fn state(&self) -> StepState {
        match self.writer {
            Some((StepState::Partial, _)) => StepState::Partial,
            _ => self.state,
        }
    }

    fn enqueue(&mut self, pos: StepPos, ev: StepEvent) {
        let state = self.state();
        self.enqueue_as(pos, state, ev);
    }

    /// Queue one step; a full queue drops its oldest step that is no
    /// [`StepEvent::Bind`] or [`StepEvent::Off`], counting it in
    /// [`StepBatch::overflow`] (the Claude fold's rule).
    fn enqueue_as(&mut self, pos: StepPos, state: StepState, ev: StepEvent) {
        self.queue.push_back(Step { pos, state, ev });
        while self.queue.len() > STEP_QUEUE {
            let pinned = |s: &Step| matches!(s.ev, StepEvent::Bind { .. } | StepEvent::Off { .. });
            let i = self.queue.iter().position(|s| !pinned(s)).unwrap_or(0);
            self.queue.remove(i);
            self.overflow = self.overflow.saturating_add(1);
        }
    }
}

/// Fold the rollout at `path` through `fold` from its first byte, at most `cap` bytes.
pub fn fold_rollout(path: &Path, fold: &mut CodexStepFold, cap: u64) -> Option<RolloutRead> {
    stream_lines_ends(path, cap, |_, end, line| {
        match line {
            Streamed::Line(Some(text)) => fold.push(text.as_bytes(), end),
            Streamed::Line(None) | Streamed::Long => fold.lost_line(end),
        }
        std::ops::ControlFlow::Continue(())
    })
}

#[cfg(test)]
#[path = "codex_steps_tests.rs"]
mod tests;
