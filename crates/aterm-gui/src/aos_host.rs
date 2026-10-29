// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Main-thread host operations for the `aos` verb on the front window's momentum glow.

use std::collections::VecDeque;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use aterm_aos::Digest;
use aterm_aos::checkpoint::Checkpoint;
use aterm_aos::log::{EventLog, Tail};
use aterm_aos::publish::{Commit, Head, PublishRefusal};
use aterm_aos::registry::{Listing, Registry, Row, TaskKind, Timeline};
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
    MomentumSchema, MomentumTrace, PreparedRepair, RepairJob, Segment, SegmentJudgement, TASK_NAME,
    TraceRefusal,
};

use crate::{App, WindowId};

/// The most kept faults a window holds before it drops the oldest.
pub(crate) const KEPT_CAP: usize = 4;

/// Why a press detached the recording: the tty does not echo it.
pub(crate) const NO_ECHO_KEY: &str = "no-echo key";

/// Why a press or a config change detached the recording: Secure Keyboard Entry is on.
pub(crate) const SECURE_ENTRY: &str = "secure keyboard entry";

/// Why a press detached the recording: a program reads it raw, or the tty's mode is unreadable, and it is not typed at a prompt of aterm's own integrated shell, so it may be a password.
pub(crate) const RAW_KEY: &str = "raw key";

/// Why the watcher left a segment unjudged: its host worker did not start.
pub(crate) const WATCHER_DID_NOT_RUN: &str = "the watcher's host worker did not run";

/// The name of the one-shot thread a repair's prepare runs on.
pub(crate) const REPAIR_THREAD: &str = "aterm-aos-repair";

/// The watcher's host worker thread's name.
pub(crate) const WATCH_THREAD: &str = "aterm-aos-watch";

/// Why a typed key may expose a secret if recorded, or `None` when aterm sees it go where it is echoed (the tty echoes it, or it is typed at an open prompt of aterm's own shell); `echo` reads the tty only when that decides.
pub(crate) fn key_privacy(
    swallows: bool,
    secure: bool,
    at_prompt: bool,
    echo: impl FnOnce() -> Option<aterm_pty::TtyEcho>,
) -> Option<&'static str> {
    if swallows {
        Some(NO_ECHO_KEY)
    } else if secure {
        Some(SECURE_ENTRY)
    } else if at_prompt || echo().is_some_and(|e| e.echo) {
        None
    } else {
        Some(RAW_KEY)
    }
}

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
    /// The watcher's judgements of retired segments and the privacy detaches, oldest first.
    timeline: Timeline,
    /// Failing segments kept as trace files, oldest first, at most [`KEPT_CAP`].
    kept: VecDeque<Kept>,
    /// The id the next kept fault gets.
    next_kept: u64,
    /// Retired segments a privacy detach took from the recording it dropped, for the watcher.
    orphans: Vec<Segment>,
    /// Whether a host worker is judging this window's segments.
    watching: bool,
    /// The repair whose prepare is out on a host worker.
    repairing: Option<PendingRepair>,
    /// Repairs started so far; each one's token is the count including it.
    repairs: u64,
    /// Whether a key the recording may not hold reached the glow since it last started from rest, so the next attach starts it from rest.
    tainted: bool,
    /// The prompt each session last submitted a line from, by session: a key typed there since goes to whatever reads after the line, not to the prompt.
    submitted: std::collections::HashMap<u64, u64>,
}

impl AosWindow {
    /// Notes that `session` submitted a line from its prompt `prompt`.
    pub(crate) fn note_submitted(&mut self, session: u64, prompt: u64) {
        self.submitted.insert(session, prompt);
    }

    /// Whether a key typed to `session` while its prompt `prompt` is in front reaches that prompt's line editor: no line was submitted from it; a submission from an earlier prompt is forgotten.
    pub(crate) fn prompt_open(&mut self, session: u64, prompt: u64) -> bool {
        match self.submitted.get(&session) {
            Some(&p) if p == prompt => false,
            Some(_) => {
                self.submitted.remove(&session);
                true
            }
            None => true,
        }
    }
}

/// Where an `aos` ask's answer goes.
type Reply = Sender<Result<String, String>>;

/// A failing segment the watcher kept.
#[derive(Debug)]
struct Kept {
    /// Its id, which `aos export kept=<id>` names.
    id: u64,
    log_epoch: u64,
    /// The segment as a trace file's contents.
    trace: MomentumTrace,
}

/// A repair waiting for its prepare from a host worker.
#[derive(Debug)]
struct PendingRepair {
    /// Which repair of the window it is.
    token: u64,
    /// Whether the repair paused a running glow first.
    paused_first: bool,
    /// The head the job was copied at.
    head: Head,
    /// Main-thread time spent on it so far.
    main: Duration,
    /// Where the verb's answer goes.
    reply: Reply,
}

/// What a host worker hands back to the main thread (`Wake::AosWorked`).
#[derive(Debug)]
pub(crate) enum Worked {
    /// A repair's prepare, made on a copy of the paused boundary.
    Repair {
        window: WindowId,
        token: u64,
        prepared: Box<PreparedRepair>,
    },
    /// The watcher's judgements of a window's retired segments, oldest first.
    Watch {
        window: WindowId,
        judged: Vec<SegmentJudgement>,
    },
}

/// A trace file for `aos export`, which the control thread writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Exported {
    pub(crate) window: WindowId,
    /// The kept fault it is, or `None` for the live recording.
    pub(crate) kept: Option<u64>,
    pub(crate) log_epoch: u64,
    /// The task identity it was recorded under.
    pub(crate) identity: Digest,
    /// [`MomentumTrace::encode`]'s bytes.
    pub(crate) bytes: Vec<u8>,
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
    /// A repair of the front window is already being prepared.
    Repairing,
    /// The host worker for the prepare did not run.
    Worker(String),
    /// Secure Keyboard Entry is on, so nothing is recorded.
    SecureEntry,
    /// No kept fault has this id.
    NoKept(u64),
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
            AosRefusal::Repairing => {
                f.write_str("a repair of the front window is already being prepared")
            }
            AosRefusal::Worker(why) => write!(f, "the repair's host worker did not run: {why}"),
            AosRefusal::SecureEntry => f.write_str(
                "Secure Keyboard Entry is on, so the glow is not recorded (turn it off to attach)",
            ),
            AosRefusal::NoKept(id) => write!(f, "no kept fault {id} in the front window"),
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
    /// Main-thread time the repair took.
    pub(crate) main: Duration,
    /// Whether its prepare ran on a host worker.
    pub(crate) off_main: bool,
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
        if self.config.secure_keyboard_entry_or_default() {
            return Err(AosRefusal::SecureEntry);
        }
        let build = aterm_types::version::build_id();
        let ws = self.windows.get_mut(&id).ok_or(AosRefusal::NoWindow)?;
        if std::mem::take(&mut ws.aos.tainted) {
            ws.fx.cursor_mut().momentum.reset();
        }
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
        let started = Instant::now();
        let id = self.aos_front()?;
        let ws = self.windows.get_mut(&id).ok_or(AosRefusal::NoWindow)?;
        if ws.aos.repairing.is_some() {
            return Err(AosRefusal::Repairing);
        }
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
                    main: started.elapsed(),
                    off_main: false,
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

    /// `aos repair` from the `Wake::Aos` handler: pauses the front window's glow at `now` if it runs, hands a copy of the paused boundary to a host worker for the prepare, and answers `reply` once [`Self::aos_worked`] commits it; with no event-loop proxy (a test app) it repairs in place ([`Self::aos_repair_at`]).
    pub(crate) fn aos_repair_begin(&mut self, code: MomentumCode, now: Instant, reply: Reply) {
        let post = self.proxy.clone().map(|proxy| {
            move |worked: Worked| {
                let _ = proxy.send_event(crate::Wake::AosWorked(Box::new(worked)));
            }
        });
        self.aos_repair_via(code, now, reply, post);
    }

    /// [`Self::aos_repair_begin`] with the worker's prepare handed to `post` for the main thread, or with no `post` the repair made in place.
    pub(crate) fn aos_repair_via<P>(
        &mut self,
        code: MomentumCode,
        now: Instant,
        reply: Reply,
        post: Option<P>,
    ) where
        P: FnOnce(Worked) + Send + 'static,
    {
        let started = Instant::now();
        let Some(post) = post else {
            let answer = self
                .aos_repair_at(code, now)
                .map(|r| crate::aos_verb::repair_line(&r))
                .map_err(|refusal| refusal.to_string());
            let _ = reply.send(answer);
            return;
        };
        match self.aos_repair_stage(code, now, reply) {
            Ok((id, token, job)) => {
                let spawned = std::thread::Builder::new()
                    .name(REPAIR_THREAD.into())
                    .spawn(move || {
                        crate::qos::set_self(crate::qos::Role::Responsive);
                        let prepared = job.prepare();
                        post(Worked::Repair {
                            window: id,
                            token,
                            prepared: Box::new(prepared),
                        });
                    });
                match spawned {
                    Ok(_) => {
                        if let Some(p) = self
                            .windows
                            .get_mut(&id)
                            .and_then(|ws| ws.aos.repairing.as_mut())
                        {
                            p.main = started.elapsed();
                        }
                    }
                    Err(e) => self.aos_repair_finish(id, token, Err(e.to_string())),
                }
            }
            Err((reply, refusal)) => {
                let _ = reply.send(Err(refusal.to_string()));
            }
        }
    }

    /// [`Self::aos_repair_begin`]'s main-thread half before the worker: the pause, the copy and the pending repair, whose token it returns with the job.
    /// # Errors
    /// The refusal with `reply` handed back, after which a glow this call paused resumes on the old code.
    pub(crate) fn aos_repair_stage(
        &mut self,
        code: MomentumCode,
        now: Instant,
        reply: Reply,
    ) -> Result<(WindowId, u64, RepairJob), (Reply, AosRefusal)> {
        let id = match self.aos_front() {
            Ok(id) => id,
            Err(refusal) => return Err((reply, refusal)),
        };
        let Some(ws) = self.windows.get_mut(&id) else {
            return Err((reply, AosRefusal::NoWindow));
        };
        if ws.aos.repairing.is_some() {
            return Err((reply, AosRefusal::Repairing));
        }
        let lane = ws.effect_present_interval();
        let momentum = &mut ws.fx.cursor_mut().momentum;
        let paused_first = momentum.aos_attached() && !momentum.aos_paused();
        if paused_first && let Err(refusal) = momentum.aos_pause(now, lane) {
            return Err((reply, refusal.into()));
        }
        let staged = momentum.aos_repair_job(code).and_then(|job| {
            momentum
                .aos_paused_head()
                .cloned()
                .map(|head| (job, head))
                .ok_or(PublishRefusal::NotPaused)
        });
        let (job, head) = match staged {
            Ok(staged) => staged,
            Err(refusal) => {
                if paused_first {
                    let _ = momentum.aos_resume();
                }
                return Err((reply, refusal.into()));
            }
        };
        ws.aos.repairs = ws.aos.repairs.saturating_add(1);
        let token = ws.aos.repairs;
        ws.aos.repairing = Some(PendingRepair {
            token,
            paused_first,
            head,
            main: Duration::ZERO,
            reply,
        });
        Ok((id, token, job))
    }

    /// Commits window `id`'s pending repair `token` from its worker's prepare (every check `commit` makes, on the original boundary) and answers the verb; a refusal resumes a glow the repair paused while it is still paused at the head the job was copied at.
    pub(crate) fn aos_repair_finish(
        &mut self,
        id: WindowId,
        token: u64,
        prepared: Result<PreparedRepair, String>,
    ) {
        let started = Instant::now();
        let Some(ws) = self.windows.get_mut(&id) else {
            return;
        };
        let Some(pending) = ws.aos.repairing.take_if(|p| p.token == token) else {
            return;
        };
        let momentum = &mut ws.fx.cursor_mut().momentum;
        let committed = match prepared {
            Ok(prepared) => momentum.aos_commit(prepared).map_err(AosRefusal::from),
            Err(why) => Err(AosRefusal::Worker(why)),
        };
        let answer = match committed {
            Ok(commit) => {
                #[cfg(debug_assertions)]
                {
                    ws.aos.published = None;
                }
                Ok(Repaired {
                    window: id,
                    paused_first: pending.paused_first,
                    commit,
                    main: pending.main.saturating_add(started.elapsed()),
                    off_main: true,
                })
            }
            Err(refusal) => {
                if pending.paused_first && momentum.aos_paused_head() == Some(&pending.head) {
                    let _ = momentum.aos_resume();
                }
                Err(refusal)
            }
        };
        self.aos_reconcile(id);
        let _ = pending.reply.send(
            answer
                .map(|r| crate::aos_verb::repair_line(&r))
                .map_err(|refusal| refusal.to_string()),
        );
    }

    /// The `Wake::AosWorked` handler.
    pub(crate) fn aos_worked(&mut self, worked: Worked) {
        match worked {
            Worked::Repair {
                window,
                token,
                prepared,
            } => self.aos_repair_finish(window, token, Ok(*prepared)),
            Worked::Watch { window, judged } => self.aos_watch_fold(window, judged),
        }
    }

    /// The online watcher's main-thread half, run as the loop parks: notes each detach a glow made by itself as a timeline row, takes each window's retired segments and hands them to a host worker to judge, one worker per window at a time; with no event-loop proxy (a test app) it judges them in place.
    pub(crate) fn aos_watch(&mut self) {
        let post = self.proxy.clone().map(|proxy| {
            move |worked: Worked| {
                let _ = proxy.send_event(crate::Wake::AosWorked(Box::new(worked)));
            }
        });
        self.aos_watch_via(post);
    }

    /// [`Self::aos_watch`] with each worker's judgements handed to `post` for the main thread, or with no `post` judged in place; segments a worker that did not run leaves are timeline rows unjudged, never lost.
    pub(crate) fn aos_watch_via<P>(&mut self, post: Option<P>)
    where
        P: Fn(Worked) + Clone + Send + 'static,
    {
        for ws in self.windows.values_mut() {
            if let Some(why) = ws.fx.cursor_mut().momentum.aos_take_detached() {
                ws.aos.epoch = None;
                #[cfg(debug_assertions)]
                {
                    ws.aos.published = None;
                }
                ws.aos.timeline.push(Row::Detached { why });
            }
        }
        let due: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(_, ws)| {
                !ws.aos.watching
                    && (!ws.aos.orphans.is_empty()
                        || ws.fx.cursor().momentum.aos_retired_waiting() > 0)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in due {
            let Some(ws) = self.windows.get_mut(&id) else {
                continue;
            };
            let mut segments = std::mem::take(&mut ws.aos.orphans);
            segments.extend(ws.fx.cursor_mut().momentum.aos_take_retired());
            let Some(post) = post.clone() else {
                let judged = segments.into_iter().map(Segment::judge).collect();
                self.aos_watch_fold(id, judged);
                continue;
            };
            let (tx, rx) = std::sync::mpsc::channel::<Vec<Segment>>();
            let spawned = spawn_watcher(move || {
                crate::qos::set_self(crate::qos::Role::Background);
                let Ok(segments) = rx.recv() else {
                    return;
                };
                let judged = segments.into_iter().map(Segment::judge).collect();
                post(Worked::Watch { window: id, judged });
            });
            let unsent = match spawned {
                Ok(_) => tx.send(segments).err().map(|e| e.0),
                Err(_) => Some(segments),
            };
            match unsent {
                None => ws.aos.watching = true,
                Some(segments) => {
                    let judged = segments
                        .into_iter()
                        .map(|s| s.unjudged(WATCHER_DID_NOT_RUN))
                        .collect();
                    self.aos_watch_fold(id, judged);
                }
            }
        }
    }

    /// Adds window `id`'s judged segments to its timeline, keeping each failing one as a fault (the oldest dropped past [`KEPT_CAP`]); no alert is raised.
    pub(crate) fn aos_watch_fold(&mut self, id: WindowId, judged: Vec<SegmentJudgement>) {
        let Some(ws) = self.windows.get_mut(&id) else {
            return;
        };
        let aos = &mut ws.aos;
        aos.watching = false;
        for j in judged {
            let kept = j.fault.map(|trace| {
                let kid = aos.next_kept;
                aos.next_kept = aos.next_kept.saturating_add(1);
                if aos.kept.len() >= KEPT_CAP {
                    aos.kept.pop_front();
                }
                aos.kept.push_back(Kept {
                    id: kid,
                    log_epoch: j.log_epoch,
                    trace,
                });
                kid
            });
            aos.timeline.push(Row::Segment {
                cause: j.cause,
                code: j.code.to_owned(),
                log_epoch: j.log_epoch,
                from: j.from,
                to: j.to,
                judged: j.judged,
                kept,
            });
        }
    }

    /// `aos ls`: every window's hosted task with its timeline, from the pure registry.
    pub(crate) fn aos_ls_now(&self) -> Vec<String> {
        let mut reg = Registry::new();
        for (id, ws) in &self.windows {
            let m = &ws.fx.cursor().momentum;
            let s = m.aos_status();
            reg.insert(Listing {
                slot: id.0,
                kind: TaskKind::Effect,
                task: TASK_NAME.to_owned(),
                code: s.code.to_owned(),
                attached: s.attached,
                paused: s.paused,
                revision: s.revision,
                log_epoch: s.log_epoch,
                log_len: s.log_len,
                waiting: m.aos_retired_waiting() + ws.aos.orphans.len(),
                kept: ws.aos.kept.len(),
                timeline: ws.aos.timeline.clone(),
            });
        }
        reg.lines()
    }

    /// `aos export [kept=<n>]`'s main-thread half: the front window's live trace, or its kept fault `kept`, as a trace file's bytes.
    pub(crate) fn aos_export_now(&self, kept: Option<u64>) -> Result<Exported, AosRefusal> {
        let id = self.aos_front()?;
        let ws = self.windows.get(&id).ok_or(AosRefusal::NoWindow)?;
        let (log_epoch, trace) = match kept {
            Some(n) => ws
                .aos
                .kept
                .iter()
                .find(|k| k.id == n)
                .map(|k| (k.log_epoch, k.trace.clone()))
                .ok_or(AosRefusal::NoKept(n))?,
            None => {
                let (summary, trace) = self.aos_trace_export()?;
                (summary.log_epoch, trace)
            }
        };
        Ok(Exported {
            window: id,
            kept,
            log_epoch,
            identity: trace.identity.digest(),
            bytes: trace.encode(),
        })
    }

    /// Stops recording every window's glow for privacy (`why` names the no-echo key or Secure Keyboard Entry), each detach a timeline row whose recording's retired segments go to the watcher.
    pub(crate) fn aos_privacy_detach_all(&mut self, why: &'static str) {
        for ws in self.windows.values_mut() {
            privacy_detach(ws, why);
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
        let retired = momentum.aos_take_retired();
        momentum.aos_detach();
        ws.aos.orphans.extend(retired);
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
            Asked::Ls => Ok(self.aos_ls_now().join("\n")),
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
            Asked::Bundle => return Err(crate::aos_verb::needs_debug("bundle")),
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

/// Starts the watcher's host worker thread running `work`.
fn spawn_watcher<F: FnOnce() + Send + 'static>(
    work: F,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    #[cfg(test)]
    if WATCH_SPAWN_REFUSED.with(std::cell::Cell::get) {
        return Err(std::io::Error::other("a test refused the watcher's thread"));
    }
    std::thread::Builder::new()
        .name(WATCH_THREAD.into())
        .spawn(work)
}

#[cfg(test)]
thread_local! {
    /// Whether a test makes the watcher's thread fail to start, as a host out of threads would.
    pub(crate) static WATCH_SPAWN_REFUSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
thread_local! {
    /// Whether a test turned the privacy detach off, the negative control's switch.
    pub(crate) static PRIVACY_DETACH_OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Notes a typed key the recording may not hold (`why`, from [`key_privacy`]) before the glow sees it: the glow is tainted until an attach starts it from rest, and a recording of it stops ([`privacy_detach`]).
pub(crate) fn private_key(ws: &mut crate::WindowState, why: &'static str) {
    ws.aos.tainted = true;
    privacy_detach(ws, why);
}

/// Stops recording `ws`'s glow for privacy before the press `why` names reaches it, so no event of the press is logged: a timeline row, and the recording's retired segments go to the watcher.
pub(crate) fn privacy_detach(ws: &mut crate::WindowState, why: &'static str) {
    #[cfg(test)]
    if PRIVACY_DETACH_OFF.with(std::cell::Cell::get) {
        return;
    }
    let momentum = &mut ws.fx.cursor_mut().momentum;
    if !momentum.aos_attached() {
        return;
    }
    let retired = momentum.aos_take_retired();
    momentum.aos_detach();
    ws.aos.orphans.extend(retired);
    ws.aos.epoch = None;
    #[cfg(debug_assertions)]
    {
        ws.aos.published = None;
    }
    ws.aos.timeline.push(Row::Detached { why });
    if let Some(w) = ws.os_window.as_ref() {
        w.request_redraw();
    }
}
