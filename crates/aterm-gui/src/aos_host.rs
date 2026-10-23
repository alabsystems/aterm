// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Main-thread host operations for the `aos` verb on the front window's momentum glow.

use std::time::Instant;

use aterm_aos::Digest;
use aterm_aos::checkpoint::Checkpoint;
use aterm_aos::log::{EventLog, Tail};
use aterm_aos::publish::{Commit, Head, PublishRefusal};
use aterm_aos::task::StepRefusal;
#[cfg(debug_assertions)]
use aterm_effects::aos_momentum::Sched;
#[cfg(debug_assertions)]
use aterm_effects::aos_momentum::bundle::CandidateBundle;
#[cfg(debug_assertions)]
use aterm_effects::aos_momentum::m4::Admitted;
#[cfg(debug_assertions)]
use aterm_effects::aos_momentum::publish::{PlanPublishRefusal, Published};
use aterm_effects::aos_momentum::{
    EventCounts, MOMENTUM_MAX_EVENTS, MomentumAosStatus, MomentumCode, MomentumObservation,
    MomentumSchema, MomentumTrace, TraceRefusal,
};

use crate::{App, WindowId};

/// What the verb keeps per window between its calls.
#[derive(Debug, Default)]
pub(crate) struct AosWindow {
    /// The recording epoch the live attach was given (`None` unattached).
    epoch: Option<Instant>,
    /// Attaches so far; each attach's log epoch is the count including it.
    attaches: u64,
    /// The last publication's bundle while its plan may be live: a later publication replaces it, and an attach, a detach or a committed repair drops it.
    #[cfg(debug_assertions)]
    published: Option<LivePlan>,
}

/// A plan's publication as the window keeps it, since its bundle cannot be recomputed once the commit has replaced the log: the bundle, and the code and revision [`App::aos_bundle_now`] checks the glow still runs.
#[cfg(debug_assertions)]
#[derive(Debug)]
struct LivePlan {
    /// The code the publication made live.
    sched: Sched,
    /// The revision it minted.
    revision: u64,
    /// Its bundle, the publication part filled.
    bundle: CandidateBundle,
}

/// Reason an `aos` operation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AosRefusal {
    /// No window is in front.
    NoWindow,
    /// The glow's boundary refused.
    Publish(PublishRefusal),
    /// The plan's publication refused (a debug build only).
    #[cfg(debug_assertions)]
    Plan(PlanPublishRefusal),
    /// No published plan is live in the front window, so there is no bundle to export.
    #[cfg(debug_assertions)]
    NoPublication,
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
            #[cfg(debug_assertions)]
            AosRefusal::Plan(r) => write!(f, "{r}"),
            #[cfg(debug_assertions)]
            AosRefusal::NoPublication => f.write_str(
                "no published plan is live in the front window (a repair, an attach or a \
                 detach since its publication drops its bundle)",
            ),
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

/// A published plan (a debug build only).
#[cfg(debug_assertions)]
pub(crate) struct PlanPublished {
    /// The window whose glow it published over.
    pub(crate) window: WindowId,
    /// Whether the publication paused a running glow first.
    pub(crate) paused_first: bool,
    /// The revision, the bundle with its publication part, and what the queued keys returned.
    pub(crate) published: Published,
}

/// A rejected update.
pub(crate) struct Resumed {
    /// The window whose glow resumed.
    pub(crate) window: WindowId,
    /// What each queued input returned from the old code, once each, in order.
    pub(crate) delivered: Vec<Result<MomentumObservation, StepRefusal>>,
}

/// Decoded summary of an exported trace.
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

    /// An attach's, repair's, publication's, resume's or detach's host half: the window's
    /// next frame is ticked by the pair now live. It asks the glow for no wake, as each
    /// query is a recorded scheduler observation: `about_to_wait`'s lane planner re-plans
    /// on this turn, and a pause's final query is the only other one on these paths.
    fn aos_reconcile(&self, id: WindowId) {
        if let Some(w) = self.windows.get(&id).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
    }

    /// Starts hosting the front window's glow under `code` at `now`.
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
        #[cfg(debug_assertions)]
        {
            ws.aos.published = None;
        }
        let momentum = &mut ws.fx.cursor_mut().momentum;
        momentum.aos_attach(now, code, &build, ws.aos.attaches);
        let status = momentum.aos_status();
        self.aos_reconcile(id);
        Ok((id, status))
    }

    /// Pauses the front window's glow at `now` and returns the expected head.
    pub(crate) fn aos_pause_at(&mut self, now: Instant) -> Result<(WindowId, Head), AosRefusal> {
        let id = self.aos_front()?;
        let ws = self.windows.get_mut(&id).ok_or(AosRefusal::NoWindow)?;
        let lane = ws.effect_present_interval();
        let head = ws.fx.cursor_mut().momentum.aos_pause(now, lane)?;
        Ok((id, head))
    }

    /// Publishes `code` over the front window's glow, pausing it at `now` first if running.
    ///
    /// # Errors
    ///
    /// Returns [`AosRefusal`] if no window is in front or the publish boundary refuses.
    /// When this call paused the glow itself, a refused repair resumes it (the old code
    /// runs on; nothing can have queued within this call), so a refusal leaves the glow
    /// as the call found it except for the pause's recorded query. A glow the caller
    /// paused stays paused, its queue whole.
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
                #[cfg(debug_assertions)]
                {
                    ws.aos.published = None;
                }
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

    /// `aos publish`: publishes `admitted`'s plan over the front window's glow, pausing it at `now` first if it runs, then reconciles the host as [`Self::aos_repair_at`] does and keeps the bundle.
    ///
    /// # Errors
    /// The publication's refusal, after which a glow this call paused resumes on the old code (nothing can have queued within the call) and one the caller paused stays paused, its queue whole.
    #[cfg(debug_assertions)]
    pub(crate) fn aos_publish_at(
        &mut self,
        admitted: Admitted,
        now: Instant,
    ) -> Result<PlanPublished, AosRefusal> {
        let id = self.aos_front()?;
        let ws = self.windows.get_mut(&id).ok_or(AosRefusal::NoWindow)?;
        let lane = ws.effect_present_interval();
        let momentum = &mut ws.fx.cursor_mut().momentum;
        let paused_first = momentum.aos_attached() && !momentum.aos_paused();
        if paused_first {
            momentum.aos_pause(now, lane)?;
        }
        match momentum.aos_publish(admitted) {
            Ok(published) => {
                ws.aos.published = Some(LivePlan {
                    sched: momentum.aos_sched(),
                    revision: published.revision().0,
                    bundle: published.bundle().clone(),
                });
                self.aos_reconcile(id);
                Ok(PlanPublished {
                    window: id,
                    paused_first,
                    published,
                })
            }
            Err(refusal) => {
                if paused_first {
                    let _ = momentum.aos_resume();
                }
                Err(AosRefusal::Plan(refusal))
            }
        }
    }

    /// Rejects a pending update and resumes the previous code over queued inputs.
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

    /// Stops hosting the front window's glow and returns its status.
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
        #[cfg(debug_assertions)]
        {
            ws.aos.published = None;
        }
        self.aos_reconcile(id);
        Ok((id, status))
    }

    /// `aos status`: the front window's glow as plain data.
    pub(crate) fn aos_status_now(&self) -> Result<(WindowId, MomentumAosStatus), AosRefusal> {
        let id = self.aos_front()?;
        let ws = self.windows.get(&id).ok_or(AosRefusal::NoWindow)?;
        Ok((id, ws.fx.cursor().momentum.aos_status()))
    }

    /// `aos bundle`: the bundle the front window's live plan was published with, and the revision that publication minted.
    ///
    /// # Errors
    /// [`AosRefusal::NoPublication`] unless the glow is still attached (a horizon detaches it on its own) and runs the code and revision the window's last publication made live.
    #[cfg(debug_assertions)]
    pub(crate) fn aos_bundle_now(&self) -> Result<(WindowId, u64, &CandidateBundle), AosRefusal> {
        let id = self.aos_front()?;
        let ws = self.windows.get(&id).ok_or(AosRefusal::NoWindow)?;
        let momentum = &ws.fx.cursor().momentum;
        ws.aos
            .published
            .as_ref()
            .filter(|p| {
                momentum.aos_attached()
                    && momentum.aos_sched() == p.sched
                    && momentum.aos_status().revision == p.revision
            })
            .map(|p| (id, p.revision, &p.bundle))
            .ok_or(AosRefusal::NoPublication)
    }

    /// Exports, decodes, and summarizes the front window's trace.
    pub(crate) fn aos_trace_now(&self) -> Result<TraceSummary, AosRefusal> {
        self.aos_trace_export().map(|(summary, _)| summary)
    }

    /// `aos trace hex`: [`Self::aos_trace_now`]'s summary and the trace it summarizes as one file ([`MomentumTrace::encode`]) a lab replays in its own process, refused exactly as `aos trace` is.
    pub(crate) fn aos_trace_file_now(&self) -> Result<(TraceSummary, Vec<u8>), AosRefusal> {
        self.aos_trace_export()
            .map(|(summary, trace)| (summary, trace.encode()))
    }

    /// The exported trace, decoded back and summarized as [`Self::aos_trace_now`] does, with the trace itself.
    fn aos_trace_export(&self) -> Result<(TraceSummary, MomentumTrace), AosRefusal> {
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
        let summary = TraceSummary {
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
        };
        Ok((summary, trace))
    }

    /// The `Wake::Aos` handler: `ask` at `now`, answered as the verb's line, where a plan to publish arrives evaluated and a bare [`crate::aos_verb::Asked::Publish`] is refused.
    pub(crate) fn control_aos(
        &mut self,
        ask: impl Into<crate::aos_verb::Ask>,
        now: Instant,
    ) -> Result<String, String> {
        use crate::aos_verb::{
            Ask, Asked, UNEVALUATED, pause_line, repair_line, resume_line, status_line,
            trace_file_line, trace_line,
        };
        if !cfg!(any(test, debug_assertions)) {
            return Err(crate::aos_verb::NEEDS_DEBUG.to_string());
        }
        #[cfg_attr(
            not(debug_assertions),
            expect(
                clippy::infallible_destructuring_match,
                reason = "without debug assertions `Ask` is only `Asked`; the publish arm is a debug build's"
            )
        )]
        let asked = match ask.into() {
            Ask::Asked(asked) => asked,
            #[cfg(debug_assertions)]
            Ask::Publish(admitted) => {
                return self
                    .aos_publish_at(*admitted, now)
                    .map(|r| crate::aos_verb::publish_line(&r))
                    .map_err(|refusal| refusal.to_string());
            }
        };
        let answer = match asked {
            Asked::Status => self.aos_status_now().map(|(id, s)| status_line(id.0, &s)),
            Asked::Attach(code) => self
                .aos_attach_at(code, now)
                .map(|(id, s)| status_line(id.0, &s)),
            Asked::Pause => self.aos_pause_at(now).map(|(id, h)| pause_line(id.0, &h)),
            Asked::Repair(code) => self.aos_repair_at(code, now).map(|r| repair_line(&r)),
            Asked::Publish(_) => return Err(UNEVALUATED.to_string()),
            #[cfg(debug_assertions)]
            Asked::Bundle => self
                .aos_bundle_now()
                .map(|(id, revision, b)| crate::aos_verb::bundle_line(id.0, revision, b)),
            #[cfg(not(debug_assertions))]
            Asked::Bundle => return Err(crate::aos_verb::NEEDS_DEBUG.to_string()),
            Asked::Resume => self.aos_resume_now().map(|r| resume_line(&r)),
            Asked::Detach => self.aos_detach_now().map(|(id, s)| status_line(id.0, &s)),
            Asked::Trace => self.aos_trace_now().map(|t| trace_line(&t)),
            Asked::TraceHex => self
                .aos_trace_file_now()
                .map(|(t, file)| trace_file_line(&t, &file)),
        };
        answer.map_err(|refusal| refusal.to_string())
    }
}
