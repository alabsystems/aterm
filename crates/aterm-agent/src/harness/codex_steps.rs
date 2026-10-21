// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CODEX STEP FOLD: turns, tool calls, turn ends and the usage wall from
//! a Codex rollout, for the session log, with no text kept (session log 5G-2;
//! rulings T9, T33, S6). It folds into the Claude step fold's own types
//! ([`Step`], [`StepEvent`], [`StepBatch`]), so the log takes a Codex
//! rollout's steps as it takes a transcript's. One reader feeds it: 5G-3's
//! rollout tap ([`super::codex_tap`]), which binds only a codex-alab
//! session's own thread on a build [`CODEX_MEASURED_BUILDS`] names, and
//! never a vendor Codex.
//!
//! WHAT IT READS is what the lab's Codex 0.163.0 rollouts show (the 5G lab
//! run, 2026-10-19: a plain turn, a tool call, an approval, a question, Esc,
//! a usage wall, a compaction and a subagent against the lab's stub),
//! committed reduced as `testdata/steps/codex-0.163.0.jsonl` and its
//! `-subagent` leg, and every key it reads is a literal of `aterm-phase`'s
//! `ROLLOUT_KEYS`, spelled from there ([`anchor_text`]) so the drift canary
//! that greps the store's Codex build for them guards this fold too:
//! * A TURN OPENS AT ITS `turn_context`, the one turn-open key, once per
//!   turn id (the opened ids are remembered, [`TURN_IDS_MAX`] of them): a
//!   `turn_context` naming a turn already opened (Codex writes one again for
//!   a running turn) opens nothing. `task_started` opens nothing: every turn
//!   the lab ran wrote both, but a compaction turn's `turn_context` comes
//!   after its compaction (measured), and the turn id and model are the
//!   `turn_context`'s. The model and effort are said ([`StepEvent::Model`])
//!   when they move, the build from the thread's own `session_meta` (the
//!   writer's, where the reader knows it: [`CodexStepFold::set_writer`]).
//! * A turn ENDS at the `task_complete` naming the open turn
//!   ([`StepEvent::TurnEnd`], its `duration_ms`), on the usage wall when its
//!   error's closed word is `usage_limit_exceeded` ([`StepEvent::EndWall`],
//!   [`WallKind::UsageSession`]: Codex names no window), and is interrupted
//!   at the `turn_aborted` naming it ([`StepEvent::Interrupt`]). A turn's
//!   tokens are the `last_token_usage` of each `token_count` whose running
//!   total grew since the last turn ended or was interrupted (a compaction
//!   turn's count comes before its `turn_context`, measured), in OpenAI's
//!   classes: input holds cached input, output holds reasoning.
//! * A CALL is a `function_call` or `custom_tool_call` item
//!   ([`StepEvent::ToolUse`]: its `call_id`, its bare `name`), its result the
//!   item's `_output` ([`StepEvent::ToolResult`], never failed: the output's
//!   text is not read). Its class is [`tool_class`]'s, from the calls the
//!   lab's rollouts showed a class for.
//! * A `compacted` row is a compaction ([`StepEvent::Compact`]).
//!
//! A COPY IS NO ONE'S TURN. A subagent's rollout opens with its own
//! `session_meta` (naming `forked_from_id`), then a copy of its parent's
//! rows — the parent's `session_meta`, its last turns' `turn_context`s,
//! `task_complete`s and messages — in one burst, then its own
//! `thread_settings_applied` naming its own thread (measured on the lab's
//! subagent, committed). A legacy fork copies the same way, its parent's
//! counts among them, and ends the copy with the same line (measured on
//! 0.163.0, `super::codex_rollout`'s header); a paginated `/fork` writes the
//! line in its `session_meta`'s millisecond. So, as the spend fold reads a
//! fork ([`super::codex_rollout::TurnReader`]), the rows from a fork's
//! `session_meta` up to its own `thread_settings_applied` — or, where none
//! comes, each stamped within [`FORK_BURST_MS`] of the newest before it —
//! step nothing; a copied count's total stands, so the fork's own count
//! that repeats it is no turn's. A rollout whose first row is no
//! `session_meta` is read the same way. Unlike the spend fold, a paginated
//! fork is no exception: its copy holds no count, but it holds turns.
//!
//! THE SEED AND THE FLOOR. The fold is bound to a rollout ([`Self::bind`])
//! and fed its lines from its FIRST byte, since a copy is known only from
//! the first row. Rows ending at or below the bind's seed, and rows older
//! than the floor, are history: folded into the copy, the open turn, the
//! running total and the turn's tokens, never queued. History names no
//! model: the first turn the log takes says its own, however many turns of
//! history ran on it (a resumed thread's, read by the rollout tap).
//!
//! FAIL CLOSED. A `turn_context` with no turn id, or one that is no log id
//! ([`StepId`]: at most 40 bytes of its alphabet), a `task_complete` or
//! `turn_aborted` with no turn id, the open turn's `task_complete` with no
//! `duration_ms`, a call or a result with no `call_id`, and a row that would
//! step with no readable stamp latch the fold off for the bound rollout
//! ([`StepEvent::Off`]) until the next bind. A call id that is no log id is
//! counted lost; a row of a kind it does not read is skipped.
//!
//! NO TEXT (ruling S6). A step holds ids, a tool's bounded name, class words
//! and numbers. A row is opened only when its raw bytes hold a key it reads
//! (or a `\u` escape, which could spell one), and read through
//! `super::transcript`'s `codex_steps` projection, which keeps only those
//! keys: a call's arguments and input, a result's output, messages,
//! instructions and a compaction's history are validated, never
//! materialized (`tests/transcript_allocations.rs`).
//!
//! MEASURED BUILDS. [`CODEX_MEASURED_BUILDS`] names each codex-alab build
//! whose rollouts the lab measured: 0.164.0-dev.12521 and 0.164.0-dev.12479
//! (session log 6D-3a, `lab codex-alab-proof` on the package built from its
//! source and on the owner's managed one), and 0.164.0-dev.12672 (B5c, the
//! forever-port package under the twelve-leg proof), each leg's rollout
//! committed reduced as `codex-alab-<release>-<leg>.jsonl`.
//! A row is [`StepState::On`] only on a build named there, read off the
//! thread's own `session_meta` (or handed to [`CodexStepFold::bind_build`]
//! by the tap that read it), and, where its reader knows the build writing
//! the rollout now ([`CodexStepFold::set_writer`]: a resumed or re-exec'd
//! thread never restates its `session_meta`), only while that one is named
//! there too; else [`StepState::Partial`] (ruling T8's rule): the 5G lab
//! run's 0.163.0 was a vendor Codex, which is never bound. An
//! ALab build's version carries its commit as build metadata
//! (`0.164.0-dev.12521+g3d3eb12d8`), whose `+` no log id holds, so the
//! build is said (`ver=`) as its release, the version before the `+`
//! ([`log_build`]); it is matched whole.

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

/// The codex-alab builds whose rollouts the lab measured, each with its
/// key-only reductions committed (`harness/testdata/steps/<stem>-<leg>.jsonl`,
/// `aterm_phase::anchors::rollout_reduction_stem`'s stem): exact one by one
/// (ruling T8's rule), the whole build string matched: 0.164.0-dev.12521,
/// built from its source for the run, 0.164.0-dev.12479, the owner's
/// managed codex-alab, read from its store, and 0.164.0-dev.12672, the
/// forever-port build (codex-alab `089291d37`) the FOREVER CRUISE / LAND
/// proof runs (B5c). `lab codex-alab-proof`'s legs
/// on each (session log 6D-3a: a turn, a continued turn, an approved
/// `exec_command`, a `request_user_input` answered, the usage wall waited
/// out, a crash relaunched and a turn carried on), each step the fold makes
/// exact against what the leg did and the rollout's own per-turn token
/// record (`codex_steps_tests`).
pub const CODEX_MEASURED_BUILDS: &[&str] = &[
    "0.164.0-dev.12521+g3d3eb12d8",
    "0.164.0-dev.12479+g07a25d86a",
    "0.164.0-dev.12672+g089291d37",
];

/// The build `build` as the log says it (`ver=`): the build when it is a
/// log id ([`StepId`]), else its release, the version before a `+` (an ALab
/// build's commit is build metadata), when that is one.
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

/// The class of a Codex call, by its namespace and NAME (addendum A11), and
/// whether it starts a subagent: one of the log's `CLS_WORDS`. Only what a
/// lab rollout showed is classed: `spawn_agent` in the `collaboration`
/// namespace starts a subagent (0.163.0, the lab's subagent leg), and
/// `exec_command` in no namespace runs a shell command, `shell` as Claude
/// Code's `Bash` is (codex-alab 0.164.0-dev.12521 and 12479, the lab's
/// approval leg: the command its model ran, approved). Every other call is
/// `other`: 0.163.0's `exec` (`functions`) runs a script that may call any
/// tool, so its name says no class, and `request_user_input` (in no
/// namespace on codex-alab, in `functions` on 0.163.0) asks the person.
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
fn fingerprint(id: &str) -> u64 {
    aterm_hash::fnv1a_64(id.as_bytes())
}

fn word<'a>(v: Option<&'a Value>, key: &str) -> Option<&'a str> {
    v?.get(key)?.as_str()
}

/// THE CODEX STEP FOLD of one rollout (module docs).
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
    pub fn new(floor_ms: u64) -> CodexStepFold {
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

    /// Bind the rollout at `fp` (its identity `id`): its lines are pushed
    /// from its first byte on, and those ending at or below `seed` are
    /// history (module docs). Everything read of another binding is dropped
    /// but the queue, and the latch lifts.
    pub fn bind(&mut self, fp: u64, id: Option<(u64, u64)>, seed: u64) {
        self.bind_build(fp, id, seed, None);
    }

    /// [`Self::bind`] a rollout whose `session_meta` the binder read already
    /// names `build` (the tap's gate, `super::codex_tap`): the bind is said
    /// at that build's state, so a measured thread's source is never said
    /// partial before its first row. The thread's own `session_meta` decides
    /// the rows' state.
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
    pub fn is_bound(&self) -> bool {
        self.bound.is_some()
    }

    /// Raise the floor to `floor_ms` (wall ms; it only rises): rows older
    /// than it, read from now on, are history.
    pub fn raise_floor(&mut self, floor_ms: u64) {
        self.floor_ms = self.floor_ms.max(floor_ms);
    }

    /// THE WRITER (session log 5G-3): `build` is what the process writing
    /// the rollout runs now (the rollout tap reads it off the claimant's
    /// image, `super::codex_alab::alab_build`). A thread's `session_meta`
    /// names the build that STARTED it, and a thread resumed or re-exec'd
    /// onto another build appends without restating it, so its rows are
    /// [`StepState::On`] only while the writer's build, too, is one
    /// [`CODEX_MEASURED_BUILDS`] names, and the build said (`ver=`) is the
    /// writer's. A writer that moves says the model again on its next turn.
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

    /// The reader stops reading the bound rollout at `end`, the end of the
    /// last line it pushed (its writer runs a build nobody measured): the
    /// source says `off` once ([`StepEvent::Off`]), and nothing more is
    /// folded until the next bind.
    pub fn stop(&mut self, end: u64) {
        if let Some((fp, id)) = self.bound
            && !self.broken
        {
            self.latch_as(fp, id, end, "unmeasured");
        }
    }

    /// Whether the fold latched off.
    #[must_use]
    pub fn broken(&self) -> bool {
        self.broken
    }

    /// Everything queued since the last take. The end is the Claude fold's
    /// and says nothing here: no Codex wall is judged by it.
    pub fn take(&mut self) -> StepBatch {
        StepBatch {
            steps: self.queue.drain(..).collect(),
            overflow: std::mem::take(&mut self.overflow),
            lost: std::mem::take(&mut self.lost),
            end: End::default(),
        }
    }

    /// A line the reader could not hand on, ending at `end`: longer than
    /// the line bound, or not UTF-8. Counted lost past the seed.
    pub fn lost_line(&mut self, end: u64) {
        if self.bound.is_some() && end > self.seed {
            self.lost = self.lost.saturating_add(1);
        }
    }

    /// Fold one complete line of the bound rollout, ending at offset `end`
    /// (past its newline).
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
        // A row the log takes: past the seed, and no older than the floor.
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

    /// Where the row stands against a copy (module docs): `true` when it is
    /// the thread's own and is read on, `false` when it steps nothing (a
    /// copied row, the first row, the line that ends a copy). A copied
    /// count's total stands.
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
                // A fork's `session_meta`, or a first row naming no thread:
                // the copy starts at its stamp, else at the first one read.
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
    /// key it reads has the wrong shape (module docs). `fresh`: past the
    /// seed; `live`: past the seed and no older than the floor, a row the
    /// log takes.
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
                // The model is said from the first row the log takes on:
                // the history's (below the seed or the floor) is the run's
                // to name, so a later turn on the same model still says it.
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
    fn ended(&mut self) {
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

    /// The state of the rows now (ruling T8's rule): the thread's build's,
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

/// Fold the rollout at `path` through `fold` from its first byte, at most
/// `cap` bytes: each complete line pushed with its end, a line longer than
/// the line bound or not UTF-8 counted lost, a torn last line left unread
/// ([`super::codex_rollout`]'s `stream_lines`). `None` when the file cannot
/// be opened.
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
