// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dedicated thread and cadence for tapping `codex-alab` rollout steps into the session log.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use aterm_agent::harness::codex_facts::CodexFactsTail;
use aterm_agent::harness::codex_tap::{Refusal, RolloutTap, rollout_path};
use aterm_agent::harness::footer::{FOLD_BUDGET, GitHeadRead, read_git_head};

use crate::claude_footer::LogWake;
use crate::self_claim::RolloutTarget;
use crate::session_log::transcript::{Applied, RolloutWord, Source, TapRun, apply_rollout_batch};
use crate::session_timeline::SessionTimeline;

/// Minimum interval between rollout reads while a `codex-alab` session is in front.
pub(crate) const RECHECK: Duration = Duration::from_secs(1);

/// Per-session rollout cadence state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RolloutAsk {
    /// When the last rollout read was dispatched.
    pub(crate) at: Instant,
    /// Whether a sweep arrived before [`RECHECK`] elapsed and still owes a trailing read.
    pub(crate) owed: bool,
}

/// Rollout action selected by the status sweep for one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ask {
    /// Not due, or nothing to request.
    Nothing,
    /// Request a rollout read for the target.
    Read(RolloutTarget),
    /// Stop the session's tap once after its claim disappears.
    Stop,
}

/// Returns the next rollout [`Ask`] for `session` at `now`.
pub(crate) fn due(
    asked: &mut HashMap<u64, RolloutAsk>,
    session: u64,
    target: Option<RolloutTarget>,
    now: Instant,
) -> Ask {
    let Some(target) = target else {
        return if asked.remove(&session).is_some() {
            Ask::Stop
        } else {
            Ask::Nothing
        };
    };
    if let Some(prior) = asked.get_mut(&session)
        && now.saturating_duration_since(prior.at) < RECHECK
    {
        prior.owed = true;
        return Ask::Nothing;
    }
    asked.insert(
        session,
        RolloutAsk {
            at: now,
            owed: false,
        },
    );
    Ask::Read(target)
}

/// Process-inspection callbacks for the rollout claimant.
#[derive(Clone, Copy)]
pub(crate) struct Image {
    /// Kernel start identity for `pid`.
    pub(crate) start: fn(u32) -> Option<String>,
    /// Process start timestamp in wall milliseconds.
    pub(crate) born_ms: fn(u32) -> Option<u64>,
    /// Running ALab package version for `pid`.
    pub(crate) build: fn(u32) -> Option<String>,
    /// Process group of `pid`.
    pub(crate) group: fn(u32) -> Option<i32>,
    /// The worker task `pid` was spawned on ([`crate::survey::task_prompt_of_env`]).
    pub(crate) task: fn(u32) -> Option<String>,
}

impl Image {
    /// Live kernel callbacks for process birth and running ALab build.
    pub(crate) const KERNEL: Image = Image {
        start: aterm_agent::harness::restart_stamp::kernel_start,
        born_ms,
        build: running_build,
        group: live_group,
        task: spawned_task,
    };
}

/// One ask: the session, its timeline, what its claim names, what a moved
/// log wakes, and how the claimant's process is looked at.
struct Job {
    session: u64,
    ctx: Weak<crate::SessionCtx>,
    target: RolloutTarget,
    run: TapRun,
    wake: LogWake,
    image: Image,
}

impl Job {
    /// Named `capture_job` because a bare `capture` merges in the lock-order
    /// census with `aterm-a11y`'s `capture`.
    fn capture_job(
        session: u64,
        ctx: &Arc<crate::SessionCtx>,
        target: RolloutTarget,
        wake: &LogWake,
        image: Image,
    ) -> Option<Self> {
        let _claim = crate::self_claim::hold_rollout(ctx, &target)?;
        let run = ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .slog()
            .transcript_arm(Source::Rollout)
            .filter(|arm| arm.run.claimed_by(target.pid))?
            .run;
        Some(Self {
            session,
            ctx: Arc::downgrade(ctx),
            target,
            run,
            wake: wake.clone(),
            image,
        })
    }
}

enum Command {
    Request(Job),
    /// The session's tap goes; with its timeline, `status rollout=` says
    /// `stopped` ([`stop_tap`]).
    Stop {
        session: u64,
        timeline: Option<Arc<Mutex<SessionTimeline>>>,
    },
}

/// The one thread rollouts are read on, started on first use.
static TAP: Mutex<Option<Sender<Command>>> = Mutex::new(None);

/// Dispatches `ask` for `session` using [`Image::KERNEL`].
pub(crate) fn follow_ask(ask: Ask, session: u64, ctx: &Arc<crate::SessionCtx>, wake: &LogWake) {
    follow_ask_as(ask, session, ctx, wake, Image::KERNEL);
}

/// Dispatches `ask` for `session` using `image`.
pub(crate) fn follow_ask_as(
    ask: Ask,
    session: u64,
    ctx: &Arc<crate::SessionCtx>,
    wake: &LogWake,
    image: Image,
) {
    match ask {
        Ask::Read(target) => request_as(session, ctx, target, wake, image),
        Ask::Stop => stop_session(session, &ctx.timeline),
        Ask::Nothing => {}
    }
}

/// Queues an off-thread rollout read for `target` into `ctx`'s session log.
pub(crate) fn request_as(
    session: u64,
    ctx: &Arc<crate::SessionCtx>,
    target: RolloutTarget,
    wake: &LogWake,
    image: Image,
) {
    let Some(job) = Job::capture_job(session, ctx, target, wake, image) else {
        return;
    };
    let mut slot = TAP.lock().unwrap_or_else(PoisonError::into_inner);
    if slot.is_none() {
        let (tx, rx) = channel::<Command>();
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-codex-rollout",
            crate::qos::Role::Responsive,
            move || run(&rx),
        );
        match spawned {
            Ok(_) => *slot = Some(tx),
            Err(e) => {
                aterm_log::warn!("codex rollout tap: could not start its thread: {e}");
                return;
            }
        }
    }
    if slot
        .as_ref()
        .is_some_and(|tx| tx.send(Command::Request(job)).is_err())
    {
        // The thread is gone (a panic in a read): a fresh one next time.
        *slot = None;
    }
}

/// Drops `session`'s tap and marks `status rollout=stopped`.
pub(crate) fn stop_session(session: u64, timeline: &Arc<Mutex<SessionTimeline>>) {
    send_stop(session, Some(Arc::clone(timeline)));
}

/// Drops a retired session's tap.
pub(crate) fn retire_session(session: u64) {
    send_stop(session, None);
}

fn send_stop(session: u64, timeline: Option<Arc<Mutex<SessionTimeline>>>) {
    let slot = TAP.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(tx) = slot.as_ref() {
        let _ = tx.send(Command::Stop { session, timeline });
    }
}

/// One session's tap, what it was made for, and the refusal last logged.
struct Held {
    target: RolloutTarget,
    run: TapRun,
    tap: RolloutTap,
    said: Option<Refusal>,
    /// The survey's reader of the same rollout, once its file is found ([`publish_facts`]).
    facts: Option<CodexFactsTail>,
    /// The claimant's worker task, read once.
    task: Option<String>,
    /// What [`publish_facts`] last published from it.
    shown: Option<crate::survey::CodexSurvey>,
}

/// The taps by session.
type Taps = HashMap<u64, Held>;

fn run(rx: &Receiver<Command>) {
    let mut taps = Taps::new();
    while let Ok(cmd) = rx.recv() {
        match cmd {
            Command::Request(job) => {
                if let Some(applied) = read_once(&mut taps, &job) {
                    job.wake.after(job.session, &applied);
                }
            }
            Command::Stop { session, timeline } => {
                stop_tap(&mut taps, session, timeline.as_ref());
            }
        }
    }
}

/// ONE STOP: `session`'s tap goes and, with its `timeline`, its answer's
/// word becomes `stopped` (`SessionLog::rollout_stopped`): nothing reads its
/// rollout until the sweep asks again.
fn stop_tap(taps: &mut Taps, session: u64, timeline: Option<&Arc<Mutex<SessionTimeline>>>) {
    taps.remove(&session);
    let cleared = if let Some(timeline) = timeline {
        let mut timeline = timeline.lock().unwrap_or_else(PoisonError::into_inner);
        let cleared = timeline.clear_codex_survey();
        timeline.slog_mut().rollout_stopped();
        cleared
    } else {
        false
    };
    if cleared {
        crate::claude_footer::post_changed(session);
    }
}

/// `session`'s tap goes, and the facts it published with it: a tap made again publishes afresh.
fn drop_held(taps: &mut Taps, session: u64, ctx: Option<&crate::SessionCtx>) {
    if taps
        .remove(&session)
        .is_some_and(|held| held.shown.is_some())
        && let Some(ctx) = ctx
        && ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear_codex_survey()
    {
        crate::claude_footer::post_changed(session);
    }
}

/// The start of process `pid`, wall ms, where the kernel names it.
fn born_ms(pid: u32) -> Option<u64> {
    let birth = aterm_uds::process::read_process_birth(i32::try_from(pid).ok()?)?;
    birth.seconds.checked_mul(1000)
}

/// The process group `pid` runs in.
fn live_group(pid: u32) -> Option<i32> {
    crate::consent_observer::live_pgid_of(i32::try_from(pid).ok()?)
}

/// The worker task file process `pid`'s environment names.
fn spawned_task(pid: u32) -> Option<String> {
    crate::survey::task_prompt_of_env(&atpkg::caller_shell::process_args(pid)?.env)
}

/// The build process `pid`'s image runs, when it is an ALab build's.
fn running_build(pid: u32) -> Option<String> {
    let exe = aterm_uds::sudo::exe_path(i32::try_from(pid).ok()?)?;
    aterm_agent::harness::codex_alab::alab_build(&exe)
}

/// ONE ASK: arm, read on, apply, and then, in a hold of its
/// own, count the ask answered with its word
/// (`SessionLog::rollout_answered`, `status rollout= rollout_asks=`): every
/// record the ask wrote is in the log before the count moves.
fn read_once(taps: &mut Taps, job: &Job) -> Option<Applied> {
    read_once_after(taps, job, || {})
}

/// `after_read` is the scheduling seam used by the captured regression:
/// the queued owner can change while file IO holds neither gate nor timeline.
fn read_once_after(taps: &mut Taps, job: &Job, after_read: impl FnOnce()) -> Option<Applied> {
    let (applied, word) = read_on(taps, job, after_read);
    if let Some(ctx) = job.ctx.upgrade() {
        ctx.timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .slog_mut()
            .rollout_answered(word);
    }
    applied
}

/// [`read_once`]'s guarded arm, read and apply, and the answer ([`RolloutWord`]): bound, the
/// tap's refusal, unbound, no thread, or stale when the queued claim, copied target, run or
/// sampled process image no longer matches.
fn read_on(
    taps: &mut Taps,
    job: &Job,
    after_read: impl FnOnce(),
) -> (Option<Applied>, RolloutWord) {
    let read = || {
        let ctx = job.ctx.upgrade()?;
        let arm = {
            let _claim = crate::self_claim::hold_rollout(&ctx, &job.target)?;
            let timeline = ctx.timeline.lock().unwrap_or_else(PoisonError::into_inner);
            timeline
                .slog()
                .transcript_arm(Source::Rollout)
                .filter(|arm| arm.run == job.run)?
        };
        ((job.image.start)(job.target.pid).as_deref() == Some(job.target.start.as_str()))
            .then_some((ctx, arm))
    };
    let Some((ctx, arm)) = read() else {
        drop_held(taps, job.session, job.ctx.upgrade().as_deref());
        return (None, RolloutWord::Stale);
    };
    if taps
        .get(&job.session)
        .is_some_and(|held| held.target != job.target || held.run != job.run)
    {
        drop_held(taps, job.session, Some(&ctx));
    }
    let held = match taps.entry(job.session) {
        Entry::Occupied(e) => e.into_mut(),
        Entry::Vacant(e) => {
            let Some(tap) = RolloutTap::new(&job.target.thread, &job.target.home) else {
                return (None, RolloutWord::NoThread);
            };
            e.insert(Held {
                target: job.target.clone(),
                run: job.run,
                tap,
                said: None,
                facts: None,
                task: (job.image.task)(job.target.pid),
                shown: None,
            })
        }
    };
    let floor = arm
        .floor_ms
        .max((job.image.born_ms)(job.target.pid).unwrap_or(0));
    let running = (job.image.build)(job.target.pid);
    held.tap.arm(floor, arm.given, running.as_deref());
    let _ = held.tap.read(FOLD_BUDGET);
    let refused = held.tap.refused();
    if refused != held.said {
        held.said = refused;
        if let Some(refusal) = refused {
            aterm_log::info!(
                "codex rollout tap #{}: binds nothing: {}",
                job.session,
                refusal.word()
            );
        }
    }
    let word = match (held.tap.is_bound(), refused) {
        (true, _) => RolloutWord::Bound,
        (false, Some(refusal)) => RolloutWord::Refused(refusal),
        (false, None) => RolloutWord::Unbound,
    };
    let batch = held.tap.take();
    let survey = read_facts(held, &job.target);
    after_read();
    if (job.image.start)(job.target.pid).as_deref() != Some(job.target.start.as_str())
        || (job.image.build)(job.target.pid) != running
    {
        drop_held(taps, job.session, Some(&ctx));
        return (None, RolloutWord::Stale);
    }
    let applied = apply_rollout_batch(&ctx, &job.target, job.run, &batch, held.tap.is_bound());
    if applied.disarm {
        drop_held(taps, job.session, Some(&ctx));
        return (Some(applied), RolloutWord::Stale);
    }
    if let Some(survey) = survey {
        publish_facts(&ctx, job, held, survey);
    }
    (Some(applied), word)
}

/// The survey's facts from `held`'s rollout, whatever build wrote it (the tap's steps are
/// measured builds' only, these facts any codex's), when they differ from those last shown.
fn read_facts(held: &mut Held, target: &RolloutTarget) -> Option<crate::survey::CodexSurvey> {
    if held.facts.is_none()
        && let Some(path) = rollout_path(&target.home, &target.thread)
    {
        held.facts = Some(CodexFactsTail::new(path));
    }
    let facts = match held.facts.as_mut() {
        Some(tail) => {
            let _ = tail.read();
            tail.facts()
        }
        None => Default::default(),
    };
    let branch = match facts.cwd.as_deref().map(read_git_head) {
        Some(GitHeadRead::Found(branch, _)) => Some(branch),
        _ => None,
    };
    let goal = aterm_agent::harness::self_claimed::read_record_goal(
        &target.home,
        target.pid,
        &target.thread,
    );
    if held.facts.is_none() && held.task.is_none() && goal.is_none() && held.shown.is_none() {
        return None;
    }
    let survey = crate::survey::CodexSurvey {
        facts,
        branch,
        task: held.task.clone(),
        goal,
    };
    (held.shown.as_ref() != Some(&survey)).then_some(survey)
}

/// Publishes `survey` for the claimant's group while its claim stands; kept as shown only once
/// the timeline took it, so a group not in front is offered it again at the next read.
fn publish_facts(
    ctx: &crate::SessionCtx,
    job: &Job,
    held: &mut Held,
    survey: crate::survey::CodexSurvey,
) {
    let Some(group) = (job.image.group)(job.target.pid) else {
        return;
    };
    let took = {
        let Some(_claim) = crate::self_claim::hold_rollout(ctx, &job.target) else {
            return;
        };
        ctx.timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set_codex_survey(group, survey.clone())
    };
    if took {
        held.shown = Some(survey);
        crate::claude_footer::post_changed(job.session);
    }
}

/// Synchronous rollout tap harness for tests.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct Host(Taps);

#[cfg(test)]
impl Host {
    /// Runs one rollout request and records its answer.
    pub(crate) fn ask(
        &mut self,
        session: u64,
        ctx: &Arc<crate::SessionCtx>,
        target: RolloutTarget,
        image: Image,
    ) -> Option<Applied> {
        let job = Job::capture_job(session, ctx, target, &LogWake::default(), image)?;
        read_once(&mut self.0, &job)
    }

    pub(crate) fn queued(
        &mut self,
        session: u64,
        ctx: &Arc<crate::SessionCtx>,
        target: RolloutTarget,
        image: Image,
        schedule: (impl FnOnce(), impl FnOnce()),
    ) -> Option<Applied> {
        let job = Job::capture_job(session, ctx, target, &LogWake::default(), image)?;
        schedule.0();
        read_once_after(&mut self.0, &job, schedule.1)
    }

    /// Runs one rollout read/apply pass and returns the answer's word.
    pub(crate) fn read_on(
        &mut self,
        session: u64,
        ctx: &Arc<crate::SessionCtx>,
        target: RolloutTarget,
        image: Image,
    ) -> &'static str {
        let Some(job) = Job::capture_job(session, ctx, target, &LogWake::default(), image) else {
            return RolloutWord::Stale.word();
        };
        read_on(&mut self.0, &job, || {}).1.word()
    }

    /// Stops `session`'s tap as the sweep's [`Ask::Stop`] does.
    pub(crate) fn stop(&mut self, session: u64, ctx: &crate::SessionCtx) {
        stop_tap(&mut self.0, session, Some(&ctx.timeline));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(thread: &str) -> RolloutTarget {
        RolloutTarget {
            revision: 1,
            pid: 4242,
            start: "Mon Oct 20 16:37:58 2026".to_string(),
            thread: thread.to_string(),
            home: std::path::PathBuf::from("/private/tmp/x/codex-home"),
        }
    }

    /// THE CADENCE: a proven claimant in front is asked for at once, then
    /// once per [`RECHECK`] however often the sweep looks; once no target
    /// stands (the program in front moved, the claim gone) its tap is
    /// stopped once, and a target again is asked for at once.
    #[test]
    fn a_claimant_is_asked_once_a_recheck_and_stopped_once_gone() {
        const THREAD: &str = "019a0000-0000-7000-8000-00000000000a";
        let mut asked = HashMap::new();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        assert_eq!(due(&mut asked, 7, None, at(0)), Ask::Nothing);
        assert_eq!(
            due(&mut asked, 7, Some(target(THREAD)), at(0)),
            Ask::Read(target(THREAD))
        );
        assert_eq!(asked.get(&7).map(|a| a.owed), Some(false));
        for ms in [1, 250, 999] {
            assert_eq!(
                due(&mut asked, 7, Some(target(THREAD)), at(ms)),
                Ask::Nothing,
                "{ms}"
            );
            assert_eq!(asked.get(&7).map(|a| a.owed), Some(true), "{ms}");
        }
        assert_eq!(
            due(&mut asked, 8, Some(target(THREAD)), at(500)),
            Ask::Read(target(THREAD))
        );
        assert_eq!(
            due(&mut asked, 7, Some(target(THREAD)), at(1_000)),
            Ask::Read(target(THREAD))
        );
        assert_eq!(asked.get(&7).map(|a| a.owed), Some(false));
        assert_eq!(
            due(&mut asked, 7, Some(target(THREAD)), at(1_400)),
            Ask::Nothing
        );
        assert_eq!(due(&mut asked, 7, None, at(1_500)), Ask::Stop);
        assert_eq!(due(&mut asked, 7, None, at(1_600)), Ask::Nothing);
        assert_eq!(
            due(&mut asked, 7, Some(target(THREAD)), at(1_700)),
            Ask::Read(target(THREAD)),
            "back in front: asked at once"
        );
        assert_eq!(due(&mut asked, 8, None, at(1_800)), Ask::Stop);
    }
}
