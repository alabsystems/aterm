// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CODEX STEP FOLD.

use std::collections::VecDeque;
use std::path::Path;

use aterm_json::Value;
use aterm_phase::WallKind;

use super::codex_rollout::{
    FORK_BURST_MS, Prefix, RolloutRead, Streamed, counts_of, is_thread_id, stamp_ms,
    stream_lines_ends,
};
use super::steps::{
    End, STEP_QUEUE, Step, StepBatch, StepEvent, StepId, StepPos, StepState, TURN_IDS_MAX,
    ToolName, number, quoted, state_for,
};
use super::transcript;

/// The codex-alab builds whose rollouts the lab measured and reduced (`harness/testdata/steps/<stem>-<leg>.jsonl`):
/// exactly the builds those reductions' own `# codex <build>` lines name, read by `build.rs` from the files the
/// checkout holds. A release builds from a clean clone, so there a build is listed when its reductions are committed
/// (`lab codex-measure`) and by nothing else; in a working tree an uncommitted reduction counts too, which is why
/// `lab codex-measure` refuses one HEAD does not hold. Sorted, matched whole. Each is named as its
/// rollouts name it (`session_meta`'s `cli_version`): by its client version
/// (`atpkg::alab::client_version`), never by codex-alab's own version.
pub const CODEX_MEASURED_BUILDS: &[&str] =
    include!(concat!(env!("OUT_DIR"), "/codex_measured_builds.rs"));

/// What each build [`CODEX_MEASURED_BUILDS`] names showed in its committed reductions: its keys
/// (`exit_code`) and each kept closed word beside its key (`turn_trigger=alab_autopilot`), read by
/// `build.rs` from the files the checkout holds, as [`CODEX_MEASURED_BUILDS`] is
/// ([`super::codex_shown::words`]). Sorted by build.
pub const CODEX_SHOWN: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/codex_shown.rs"));

/// codex-alab's Autopilot's own `turn_trigger` (codex-alab `TURN_TRIGGER`,
/// `codex-rs/alab-autopilot/src/extension.rs`), set on every submission its Autopilot makes.
pub const AUTOPILOT_TRIGGER: &str = "alab_autopilot";

/// The closed words a gated read reads, beside their key: what a measurement notes when a build
/// drops one (`lab codex-measure`'s drift).
pub const GATED_WORDS: [(&str, &str); 1] = [(KEY_TURN_TRIGGER, AUTOPILOT_TRIGGER)];

/// THE GATE OF THE FOLD'S LATER READS (gaps 20, 25): each applies only while the rows are read
/// [`StepState::On`] and both the rollout's build and its writer's show what it reads
/// ([`CODEX_SHOWN`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Gate {
    /// `exit_code`: a command's completion fails its step.
    pub exit: bool,
    /// `turn_trigger=alab_autopilot`: the turn is the harness's.
    pub autopilot: bool,
}

impl Gate {
    /// No gated read: an unmeasured build's, and every build's while its rows read `Partial`.
    pub const NONE: Gate = Gate {
        exit: false,
        autopilot: false,
    };
    /// Every gated read: the lab reducer's fold ([`CodexStepFold::ungated`]).
    pub const ALL: Gate = Gate {
        exit: true,
        autopilot: true,
    };

    /// The gate `build`'s committed reductions open ([`Gate::NONE`] for a build [`CODEX_SHOWN`]
    /// does not name).
    #[must_use]
    pub fn of(build: Option<&str>) -> Gate {
        build
            .and_then(|b| CODEX_SHOWN.iter().find(|(n, _)| *n == b))
            .map_or(Gate::NONE, |(_, w)| Gate::from_words(w))
    }

    /// The gate a build whose reductions show `words` ([`super::codex_shown::words`]) opens.
    #[must_use]
    pub fn from_words(words: &str) -> Gate {
        Gate {
            exit: shows(words, KEY_EXIT_CODE, None),
            autopilot: shows(words, GATED_WORDS[0].0, Some(GATED_WORDS[0].1)),
        }
    }

    const fn and(self, o: Gate) -> Gate {
        Gate {
            exit: self.exit && o.exit,
            autopilot: self.autopilot && o.autopilot,
        }
    }
}

/// Whether `words` ([`super::codex_shown::words`]) hold the key `key`, or, with `value`, the
/// closed word `value` beside it.
fn shows(words: &str, key: &str, value: Option<&str>) -> bool {
    words.split('\n').any(|t| match value {
        None => t == key,
        Some(v) => t.split_once('=') == Some((key, v)),
    })
}

/// The build `build` as the log says it (`ver=`).
#[must_use]
pub fn log_build(build: &str) -> Option<StepId> {
    StepId::new(build).or_else(|| {
        build
            .split_once('+')
            .and_then(|(release, _)| StepId::new(release))
    })
}

const SESSION_META: &str = aterm_phase::anchor!("rollout.session_meta");
const TURN_CONTEXT: &str = aterm_phase::anchor!("rollout.turn_context");
const EVENT_MSG: &str = aterm_phase::anchor!("rollout.event_msg");
const RESPONSE_ITEM: &str = aterm_phase::anchor!("rollout.response_item");
const COMPACTED: &str = aterm_phase::anchor!("rollout.compacted");
const TASK_COMPLETE: &str = aterm_phase::anchor!("rollout.task_complete");
const TURN_ABORTED: &str = aterm_phase::anchor!("rollout.turn_aborted");
const SETTINGS_APPLIED: &str = aterm_phase::anchor!("rollout.thread_settings_applied");
const TOKEN_COUNT: &str = aterm_phase::anchor!("rollout.token_count");
const FUNCTION_CALL: &str = aterm_phase::anchor!("rollout.function_call");
const FUNCTION_OUTPUT: &str = aterm_phase::anchor!("rollout.function_call_output");
const CUSTOM_CALL: &str = aterm_phase::anchor!("rollout.custom_tool_call");
const CUSTOM_OUTPUT: &str = aterm_phase::anchor!("rollout.custom_tool_call_output");
const KEY_TIMESTAMP: &str = aterm_phase::anchor!("rollout.timestamp");
const KEY_TYPE: &str = aterm_phase::anchor!("rollout.type");
const KEY_PAYLOAD: &str = aterm_phase::anchor!("rollout.payload");
const KEY_ID: &str = aterm_phase::anchor!("rollout.id");
const KEY_FORKED_FROM: &str = aterm_phase::anchor!("rollout.forked_from_id");
const KEY_CLI_VERSION: &str = aterm_phase::anchor!("rollout.cli_version");
const KEY_THREAD_ID: &str = aterm_phase::anchor!("rollout.thread_id");
const KEY_TURN_ID: &str = aterm_phase::anchor!("rollout.turn_id");
const KEY_CALL_ID: &str = aterm_phase::anchor!("rollout.call_id");
const KEY_NAME: &str = aterm_phase::anchor!("rollout.name");
const KEY_NAMESPACE: &str = aterm_phase::anchor!("rollout.namespace");
const KEY_MODEL: &str = aterm_phase::anchor!("rollout.model");
const KEY_EFFORT: &str = aterm_phase::anchor!("rollout.effort");
const KEY_DURATION_MS: &str = aterm_phase::anchor!("rollout.duration_ms");
const KEY_ERROR: &str = aterm_phase::anchor!("rollout.error");
const KEY_ERROR_INFO: &str = aterm_phase::anchor!("rollout.codex_error_info");
const USAGE_LIMIT: &str = aterm_phase::anchor!("rollout.usage_limit_exceeded");
const COLLABORATION: &str = aterm_phase::anchor!("rollout.collaboration");
const SPAWN_AGENT: &str = aterm_phase::anchor!("rollout.spawn_agent");
const EXEC_COMMAND: &str = aterm_phase::anchor!("rollout.exec_command");
const ITEM_COMPLETED: &str = aterm_phase::anchor!("rollout.item_completed");
const KEY_ITEM: &str = aterm_phase::anchor!("rollout.item");
const KEY_EXIT_CODE: &str = aterm_phase::anchor!("rollout.exit_code");
const TASK_STARTED: &str = aterm_phase::anchor!("rollout.task_started");
const KEY_TURN_ATTRIBUTION: &str = aterm_phase::anchor!("rollout.turn_attribution");
const KEY_TURN_TRIGGER: &str = aterm_phase::anchor!("rollout.turn_trigger");

/// The most open calls the exit read keeps (codex_facts' `PENDING_KEPT`): past it the oldest is
/// dropped, and reads `ok` at its output.
const CALLS_MAX: usize = 64;

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
    /// THE EXIT READ'S CALLS (gap 20): each open call's `call_id` fingerprint, and whether a
    /// completion read before its output failed, oldest first, at most [`CALLS_MAX`].
    calls: VecDeque<(u64, bool)>,
    /// What the rollout's own build showed ([`Gate::of`]).
    shown: Gate,
    /// What the writer's build showed ([`Gate::of`], [`Self::set_writer`]).
    writer_shown: Gate,
    /// Every gated read applies whatever the build ([`Self::ungated`]).
    ungated: bool,
    /// THE HARNESS'S TURN (gap 25): the turn whose `task_started` said the Autopilot began it,
    /// read before its `turn_context`.
    harness: Option<u64>,
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
            calls: VecDeque::new(),
            shown: Gate::NONE,
            writer_shown: Gate::NONE,
            ungated: false,
            harness: None,
            queue: VecDeque::new(),
            overflow: 0,
            lost: 0,
        }
    }

    /// A fold whose gated reads ([`Gate`]) read on every build: the lab reducer's, since a build
    /// is measured from what this reads; never the tap's.
    #[must_use]
    pub const fn ungated(floor_ms: u64) -> CodexStepFold {
        let mut f = CodexStepFold::new(floor_ms);
        f.ungated = true;
        f
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
            shown: Gate::of(build),
            writer: self.writer,
            writer_shown: self.writer_shown,
            ungated: self.ungated,
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

    /// Record the `build` that the process writing the rollout runs now (`super::codex_alab::alab_build`;
    /// `None`: its reader names none, so its rows are read [`StepState::Partial`]).
    pub fn set_writer(&mut self, build: Option<&str>) {
        let writer = (
            state_for(CODEX_MEASURED_BUILDS, build),
            build.and_then(log_build),
        );
        if self.writer.is_some_and(|w| w != writer) {
            self.model = None;
        }
        self.writer = Some(writer);
        self.writer_shown = Gate::of(build);
    }

    /// THE STATE ITS ROWS ARE READ AT NOW: [`StepState::On`] only while the rollout's own build and
    /// its writer's are both builds [`CODEX_MEASURED_BUILDS`] names, else [`StepState::Partial`]
    /// (Claude Code's unmeasured builds' rule: strong; it adds, opens a turn none runs and ends
    /// only its own, fix E1).
    #[must_use]
    pub const fn reads_at(&self) -> StepState {
        self.state()
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
        let gate = self.gate();
        let opens = OPENED.iter().any(|k| quoted(t, k))
            || (gate.exit
                && !self.calls.is_empty()
                && quoted(t, ITEM_COMPLETED)
                && quoted(t, KEY_EXIT_CODE))
            || (gate.autopilot && quoted(t, TASK_STARTED));
        if t.is_empty() || (self.prefix == Prefix::Own && !opens && !t.contains("\\u")) {
            return;
        }
        let Ok(v) = transcript::codex_steps(t) else {
            self.lost_line(end);
            return;
        };
        let stamp = v
            .get(KEY_TIMESTAMP)
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
        let kind = v.get(KEY_TYPE).and_then(Value::as_str);
        let payload = v.get(KEY_PAYLOAD);
        let ptype = word(payload, KEY_TYPE);
        match self.prefix {
            Prefix::Own => return true,
            Prefix::Start => {
                if kind == Some(SESSION_META) {
                    let build = word(payload, KEY_CLI_VERSION);
                    self.state = state_for(CODEX_MEASURED_BUILDS, build);
                    self.shown = Gate::of(build);
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
        let kind = v.get(KEY_TYPE).and_then(Value::as_str);
        let payload = v.get(KEY_PAYLOAD);
        let ptype = word(payload, KEY_TYPE);
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
                    if self.harness.take() == Some(f) {
                        out.push(StepEvent::HarnessTurn { vref });
                    }
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
            (Some(EVENT_MSG), Some(TASK_STARTED)) => self.task_started(payload, out),
            (Some(EVENT_MSG), Some(ITEM_COMPLETED)) => self.item_completed(payload),
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
                let f = fingerprint(call);
                if self.gate().exit && !self.calls.iter().any(|(c, _)| *c == f) {
                    if self.calls.len() == CALLS_MAX {
                        self.calls.pop_front();
                    }
                    self.calls.push_back((f, false));
                }
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
                let f = fingerprint(call);
                let failed = self
                    .calls
                    .iter()
                    .position(|(c, _)| *c == f)
                    .and_then(|i| self.calls.remove(i))
                    .is_some_and(|(_, failed)| failed);
                match StepId::new(call) {
                    Some(id) => out.push(StepEvent::ToolResult {
                        id,
                        error: failed && self.gate().exit,
                    }),
                    None => self.lost = self.lost.saturating_add(u64::from(fresh)),
                }
            }
            (Some(COMPACTED), _) => out.push(StepEvent::Compact),
            _ => {}
        }
        false
    }

    /// THE EXIT READ (gap 20): a command's completion (`item_completed`) whose `item.id` is an
    /// open call's `call_id`, read before that call's output, fails the call when its
    /// `item.exit_code` is a non-zero integer (`-1` for a decline), and clears it otherwise: the
    /// last completion before the output wins. One read after the output finds no open call and
    /// changes nothing. It never latches and emits nothing.
    fn item_completed(&mut self, payload: Option<&Value>) {
        if !self.gate().exit {
            return;
        }
        let item = payload.and_then(|p| p.get(KEY_ITEM));
        let Some(f) = word(item, KEY_ID).map(fingerprint) else {
            return;
        };
        let failed = item
            .and_then(|i| i.get(KEY_EXIT_CODE))
            .and_then(Value::as_i64)
            .is_some_and(|x| x != 0);
        if let Some(call) = self.calls.iter_mut().find(|(c, _)| *c == f) {
            call.1 = failed;
        }
    }

    /// THE HARNESS'S TURN (gap 25): a `task_started` whose `turn_attribution.turn_trigger` is
    /// [`AUTOPILOT_TRIGGER`] says the agent's own harness began its turn: a
    /// [`StepEvent::HarnessTurn`] after that turn's `Prompt`. Where the `task_started` comes
    /// first it is held and follows the `Prompt` directly, at its `turn_context`; where the
    /// `turn_context` came first it is pushed now, at the `task_started` row, after the `Prompt`
    /// and anything read between (the turn's `Model`). Any other trigger, `null`, or a missing
    /// attribution says nothing, and so does a turn that opened and ended already. It never
    /// latches.
    fn task_started(&mut self, payload: Option<&Value>, out: &mut Vec<StepEvent>) {
        if !self.gate().autopilot {
            return;
        }
        let trigger = word(
            payload.and_then(|p| p.get(KEY_TURN_ATTRIBUTION)),
            KEY_TURN_TRIGGER,
        );
        if trigger != Some(AUTOPILOT_TRIGGER) {
            return;
        }
        let Some(turn) = word(payload, KEY_TURN_ID) else {
            return;
        };
        let f = fingerprint(turn);
        if self.open == Some(f) {
            if let Some(vref) = StepId::new(turn) {
                out.push(StepEvent::HarnessTurn { vref });
            }
        } else if !self.opened.contains(&f) {
            self.harness = Some(f);
        }
    }

    /// The open turn ended or was interrupted: its tokens start over, and its calls close.
    fn ended(&mut self) {
        self.open = None;
        self.tok_in = 0;
        self.tok_out = 0;
        self.calls.clear();
    }

    /// THE GATE THE ROWS ARE READ AT NOW ([`Gate`]): every read for an ungated fold; none while
    /// [`Self::state`] is not [`StepState::On`]; else what the rollout's build showed and, once
    /// its writer is known, what the writer's showed too (as [`Self::state`] ignores a writer
    /// none named yet).
    fn gate(&self) -> Gate {
        if self.ungated {
            Gate::ALL
        } else if !matches!(self.state(), StepState::On) {
            Gate::NONE
        } else if self.writer.is_some() {
            self.shown.and(self.writer_shown)
        } else {
            self.shown
        }
    }

    fn latch(&mut self, fp: u64, id: Option<(u64, u64)>, end: u64) {
        self.broken = true;
        let at_ms = self.floor_ms;
        self.enqueue(
            StepPos {
                fp,
                id,
                off: end,
                at_ms,
            },
            StepEvent::Off { why: "unknown" },
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
