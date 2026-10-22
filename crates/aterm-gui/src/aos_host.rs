// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `aos` verb's main-thread half (`docs/DESIGN-aos-hosted-execution-2026-10-21.md`
//! §5): the front window's momentum glow (`ws.fx.cursor_mut().momentum`, an
//! [`aterm_effects::aos_momentum::MomentumHost`]) attached, paused, repaired, resumed,
//! detached, read and traced. Every method takes the instant it acts at, so a test pins
//! it; the `Wake::Aos` handler passes `Instant::now()`, which is no earlier than any
//! instant the host already handed the glow (every key, frame and scheduler query reads
//! the clock where it is made, on this thread).
//!
//! # Reconciling a publication
//!
//! A commit ([`App::aos_repair_at`]) installs the candidate's code and replayed state
//! as one value, and delivers each key queued during the pause to that pair once, in
//! order, before it returns: all inside this one main-thread call, so no frame and no
//! scheduler query sees anything in between. The host then:
//!
//! * **requests a redraw** of the window, so the next frame is ticked by the accepted
//!   pair. Its fingerprint joins the frame's repaint key, so a frame that changes the
//!   glow presents (the previous overlay's rows are damaged with the new one's), and a
//!   glow past its cold instant clears on that frame even where no lane would wake it
//!   (an unfocused window past its typed wake);
//! * **re-plans the effect lane from the accepted state** on the same turn: winit's
//!   `about_to_wait` follows every user event and plans each window's lane
//!   (`plan_terminal_effect_lane_with_recording`, the lane's one planner) from the glow
//!   as it now is. A settling glow under the published code names its wake there, so the
//!   lane a stranded glow parked is armed again and runs to the clearing frame. This
//!   module never asks the glow for a wake itself: each such question is a recorded
//!   scheduler observation (design §2 item 6), owned by the planner.
//!
//! A resume (the pending update rejected, the queue delivered to the old pair) and a
//! detach reconcile the same way.

use std::time::Instant;

use aterm_aos::Digest;
use aterm_aos::checkpoint::Checkpoint;
use aterm_aos::log::{EventLog, Tail};
use aterm_aos::publish::{Commit, Head, PublishRefusal};
use aterm_aos::task::StepRefusal;
use aterm_effects::aos_momentum::{
    EventCounts, MOMENTUM_MAX_EVENTS, MomentumAosStatus, MomentumCode, MomentumObservation,
    MomentumSchema, TraceRefusal,
};

use crate::{App, WindowId};

/// What the verb keeps per window between its calls.
#[derive(Debug, Default)]
pub(crate) struct AosWindow {
    /// The recording epoch the live attach was given (`None` unattached).
    epoch: Option<Instant>,
    /// Attaches so far; each attach's log epoch is the count including it.
    attaches: u64,
}

/// Why an `aos` ask did nothing. Nothing is changed by any of them, with one
/// exception [`App::aos_repair_at`] names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AosRefusal {
    /// No window is in front.
    NoWindow,
    /// The glow's boundary refused.
    Publish(PublishRefusal),
    /// The trace did not export.
    Trace(TraceRefusal),
    /// The exported trace did not decode back: a fault of the export.
    Decode(String),
}

impl std::fmt::Display for AosRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AosRefusal::NoWindow => f.write_str("no window is in front"),
            AosRefusal::Publish(r) => write!(f, "{r}"),
            AosRefusal::Trace(r) => write!(f, "{r}"),
            AosRefusal::Decode(why) => write!(f, "the exported trace does not decode: {why}"),
        }
    }
}

impl std::error::Error for AosRefusal {}

impl From<PublishRefusal> for AosRefusal {
    fn from(r: PublishRefusal) -> Self {
        AosRefusal::Publish(r)
    }
}

/// A committed repair.
pub(crate) struct Repaired {
    /// The window whose glow it published over.
    pub(crate) window: WindowId,
    /// Whether the repair paused a running glow first.
    pub(crate) paused_first: bool,
    /// The new revision, its witness and what the queued keys returned.
    pub(crate) commit: Commit<MomentumSchema>,
}

/// A rejected update.
pub(crate) struct Resumed {
    /// The window whose glow resumed.
    pub(crate) window: WindowId,
    /// What each queued input returned from the old code, once each, in order.
    pub(crate) delivered: Vec<Result<MomentumObservation, StepRefusal>>,
}

/// The exported trace, decoded back ([`App::aos_trace_at`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TraceSummary {
    pub(crate) window: WindowId,
    /// Events in the log since its checkpoint.
    pub(crate) log_len: usize,
    pub(crate) log_epoch: u64,
    /// The log's first cursor and the cursor after its last event.
    pub(crate) first: u64,
    pub(crate) end: u64,
    /// The trace digest over the whole log (`None` when the log is broken).
    pub(crate) trace: Option<Digest>,
    /// The ancestor checkpoint's digest.
    pub(crate) checkpoint: Digest,
    /// The task identity the trace was recorded under.
    pub(crate) identity: Digest,
    /// Why the log is broken, if it is.
    pub(crate) broken: Option<&'static str>,
    pub(crate) checkpoint_bytes: usize,
    pub(crate) log_bytes: usize,
    /// How many keys, resets, ticks and queries the decoded log holds.
    pub(crate) counts: EventCounts,
}

impl App {
    /// The window the verb acts on: the frontmost.
    fn aos_front(&self) -> Result<WindowId, AosRefusal> {
        self.frontmost_window
            .filter(|id| self.windows.contains_key(id))
            .ok_or(AosRefusal::NoWindow)
    }

    /// A publication's (or resume's, or detach's) host half: the window's next frame is
    /// ticked by the pair now live. The lane is re-planned by `about_to_wait` on this
    /// turn (module docs).
    fn aos_reconcile(&self, id: WindowId) {
        if let Some(w) = self.windows.get(&id).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
    }

    /// `aos attach`: starts hosting the front window's glow under `code` at `now` (the
    /// recording epoch: checkpoint C0 and an empty log whose epoch counts this window's
    /// attaches). Answers the glow's status after it.
    pub(crate) fn aos_attach_at(
        &mut self,
        code: MomentumCode,
        now: Instant,
    ) -> Result<(WindowId, MomentumAosStatus), AosRefusal> {
        let id = self.aos_front()?;
        let build = aterm_types::version::build_id();
        let ws = self.windows.get_mut(&id).ok_or(AosRefusal::NoWindow)?;
        ws.aos.attaches = ws.aos.attaches.saturating_add(1);
        ws.aos.epoch = Some(now);
        let momentum = &mut ws.fx.cursor_mut().momentum;
        momentum.aos_attach(now, code, &build, ws.aos.attaches);
        let status = momentum.aos_status();
        // A re-attach over a pending update rejected it: its queue went to the old pair.
        self.aos_reconcile(id);
        Ok((id, status))
    }

    /// `aos pause`: pauses the glow at this step boundary, recording a final scheduler
    /// query at `now` on the window's effect lane interval. Answers the expected head.
    pub(crate) fn aos_pause_at(&mut self, now: Instant) -> Result<(WindowId, Head), AosRefusal> {
        let id = self.aos_front()?;
        let ws = self.windows.get_mut(&id).ok_or(AosRefusal::NoWindow)?;
        let lane = ws.effect_present_interval();
        let head = ws.fx.cursor_mut().momentum.aos_pause(now, lane)?;
        Ok((id, head))
    }

    /// `aos repair`: publishes `code` over the front window's glow, pausing it at `now`
    /// first if it runs, then reconciles the host (module docs).
    ///
    /// # Errors
    ///
    /// The boundary's refusal. When this call paused the glow itself, a refused repair
    /// resumes it (the old code runs on; nothing can have queued within this call), so a
    /// refusal leaves the glow as the call found it except for the pause's recorded
    /// query. A glow the caller paused stays paused, its queue whole.
    pub(crate) fn aos_repair_at(
        &mut self,
        code: MomentumCode,
        now: Instant,
    ) -> Result<Repaired, AosRefusal> {
        let id = self.aos_front()?;
        let ws = self.windows.get_mut(&id).ok_or(AosRefusal::NoWindow)?;
        let lane = ws.effect_present_interval();
        let momentum = &mut ws.fx.cursor_mut().momentum;
        let paused_first = momentum.aos_attached() && !momentum.aos_paused();
        if paused_first {
            momentum.aos_pause(now, lane)?;
        }
        match momentum.aos_repair(code) {
            Ok(commit) => {
                self.aos_reconcile(id);
                Ok(Repaired {
                    window: id,
                    paused_first,
                    commit,
                })
            }
            Err(refusal) => {
                if paused_first {
                    let _ = momentum.aos_resume();
                }
                Err(refusal.into())
            }
        }
    }

    /// `aos resume`: rejects the pending update; the old pair resumes and is handed each
    /// queued input once, in order.
    pub(crate) fn aos_resume_now(&mut self) -> Result<Resumed, AosRefusal> {
        let id = self.aos_front()?;
        let ws = self.windows.get_mut(&id).ok_or(AosRefusal::NoWindow)?;
        let momentum = &mut ws.fx.cursor_mut().momentum;
        if !momentum.aos_attached() {
            return Err(PublishRefusal::NotAttached.into());
        }
        if !momentum.aos_paused() {
            return Err(PublishRefusal::NotPaused.into());
        }
        let delivered = momentum.aos_resume();
        self.aos_reconcile(id);
        Ok(Resumed {
            window: id,
            delivered,
        })
    }

    /// `aos detach`: stops hosting; the live state moves back whole and the shipped
    /// engine runs it directly again (a pending update is rejected first).
    pub(crate) fn aos_detach_now(&mut self) -> Result<(WindowId, MomentumAosStatus), AosRefusal> {
        let id = self.aos_front()?;
        let ws = self.windows.get_mut(&id).ok_or(AosRefusal::NoWindow)?;
        let momentum = &mut ws.fx.cursor_mut().momentum;
        if !momentum.aos_attached() {
            return Err(PublishRefusal::NotAttached.into());
        }
        momentum.aos_detach();
        let status = momentum.aos_status();
        ws.aos.epoch = None;
        self.aos_reconcile(id);
        Ok((id, status))
    }

    /// `aos status`: the front window's glow as plain data.
    pub(crate) fn aos_status_now(&self) -> Result<(WindowId, MomentumAosStatus), AosRefusal> {
        let id = self.aos_front()?;
        let ws = self.windows.get(&id).ok_or(AosRefusal::NoWindow)?;
        Ok((id, ws.fx.cursor().momentum.aos_status()))
    }

    /// `aos trace`: the exported trace (identity, encoded checkpoint, encoded log),
    /// decoded back under the window's recording epoch and beside the checkpoint it extends,
    /// and summarized, so the answer is what a replay elsewhere would read. A log that is
    /// torn or does not end with its seal is refused; the counts are of the decoded log.
    pub(crate) fn aos_trace_now(&self) -> Result<TraceSummary, AosRefusal> {
        let id = self.aos_front()?;
        let ws = self.windows.get(&id).ok_or(AosRefusal::NoWindow)?;
        let trace = ws
            .fx
            .cursor()
            .momentum
            .aos_trace()
            .map_err(AosRefusal::Trace)?;
        let epoch = ws
            .aos
            .epoch
            .ok_or_else(|| AosRefusal::Decode("no recording epoch for this attach".into()))?;
        let schema = MomentumSchema::new(epoch);
        let checkpoint = Checkpoint::decode(&trace.checkpoint)
            .map_err(|e| AosRefusal::Decode(format!("checkpoint: {e}")))?;
        let (log, tail) = EventLog::decode(
            &schema,
            &trace.log,
            &trace.identity,
            &checkpoint.digest(),
            MOMENTUM_MAX_EVENTS,
        )
        .map_err(|e| AosRefusal::Decode(format!("log: {e}")))?;
        match tail {
            Tail::Clean => {}
            Tail::Torn { at, dropped } => {
                return Err(AosRefusal::Decode(format!(
                    "log: a torn tail at byte {at} ({dropped} bytes)"
                )));
            }
            Tail::Unsealed { events } => {
                return Err(AosRefusal::Decode(format!(
                    "log: no seal after its {events} events"
                )));
            }
        }
        let (first, end) = (log.header().first_cursor, log.end_cursor());
        Ok(TraceSummary {
            window: id,
            log_len: log.len(),
            log_epoch: log.header().log_epoch,
            first,
            end,
            trace: log.trace_digest(&schema, first, end).ok(),
            checkpoint: checkpoint.digest(),
            identity: trace.identity.digest(),
            broken: log.broken(),
            checkpoint_bytes: trace.checkpoint.len(),
            log_bytes: trace.log.len(),
            counts: EventCounts::of(log.inputs()),
        })
    }

    /// The `Wake::Aos` handler: `asked` at `now`, answered as the verb's line.
    pub(crate) fn control_aos(
        &mut self,
        asked: crate::aos_verb::Asked,
        now: Instant,
    ) -> Result<String, String> {
        use crate::aos_verb::{
            Asked, pause_line, repair_line, resume_line, status_line, trace_line,
        };
        if !cfg!(any(test, debug_assertions)) {
            return Err(crate::aos_verb::NEEDS_DEBUG.to_string());
        }
        let answer = match asked {
            Asked::Status => self.aos_status_now().map(|(id, s)| status_line(id.0, &s)),
            Asked::Attach(code) => self
                .aos_attach_at(code, now)
                .map(|(id, s)| status_line(id.0, &s)),
            Asked::Pause => self.aos_pause_at(now).map(|(id, h)| pause_line(id.0, &h)),
            Asked::Repair(code) => self.aos_repair_at(code, now).map(|r| repair_line(&r)),
            Asked::Resume => self.aos_resume_now().map(|r| resume_line(&r)),
            Asked::Detach => self.aos_detach_now().map(|(id, s)| status_line(id.0, &s)),
            Asked::Trace => self.aos_trace_now().map(|t| trace_line(&t)),
        };
        answer.map_err(|refusal| refusal.to_string())
    }
}
