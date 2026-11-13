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
use aterm_agent::harness::steps::StepState;

use crate::claude_footer::LogWake;
use crate::self_claim::{TapTarget, Tapped, Untapped};
use crate::session_log::transcript::{
    Applied, ReadWait, RolloutWord, Source, TapRun, apply_rollout_batch,
};
use crate::session_timeline::SessionTimeline;

/// Minimum interval between rollout reads while a `codex-alab` session is in front: a read-only
/// target's record is read again on the same cadence ([`crate::self_claim::tapped`]).
pub(crate) const RECHECK: Duration = Duration::from_secs(1);

/// How long after a codex-alab tab's screen last moved its rollout and record are read once more,
/// while its record names a count ([`Look::counted`]): codex-alab writes a change it drew up to its
/// record writer's interval (1 s, CONTRACT §4) after the frame, so the read the move itself caused
/// may precede the write; a further [`RECHECK`] covers the sweep's own lag.
pub(crate) const SETTLE: Duration = Duration::from_secs(2);

/// What the status sweep saw of a tab when it asked ([`due`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Look {
    /// The tab's screen generation at the sweep.
    pub(crate) screen: crate::control::ScreenGen,
    /// Its codex-alab's record named a count at the last read the timeline took
    /// ([`SessionTimeline::codex_terminals`]).
    pub(crate) counted: bool,
}

/// Per-session rollout cadence state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RolloutAsk {
    /// When the last rollout read was dispatched.
    pub(crate) at: Instant,
    /// Whether a sweep arrived before [`RECHECK`] elapsed and still owes a trailing read: the
    /// session wrote since that read, or that read went out just after its output ([`due`]).
    pub(crate) owed: bool,
    /// The session's last output (`latest_output_activity_ns` on the sweep's clock) as that read
    /// was dispatched; `None` where it had written nothing.
    pub(crate) output: Option<Instant>,
    /// The screen generation the last call saw ([`Look::screen`]); `None` when asked with no look.
    pub(crate) screen: Option<crate::control::ScreenGen>,
    /// The settle read owed ([`SETTLE`]): the screen moved while the record named a count.
    pub(crate) settle: Option<Instant>,
    /// The harness pairing's read wait the session's log last named ([`pair_wait`]).
    pub(crate) wait: Option<ReadWait>,
    /// The pair read owed ([`pair_wait`]): a read to the rollout's end begun at or after this
    /// instant settles a harness turn's read side.
    pub(crate) pair: Option<Instant>,
}

impl RolloutAsk {
    /// When this ask wants the sweep again: its trailing read, its settle read or its pair read,
    /// the earliest, and never before [`RECHECK`] after the last read, since [`due`] reads no
    /// sooner: a settle or a pair kept across a late trailing read can lie inside that window, and
    /// a wake there would only be refused at every sweep until the window ends.
    pub(crate) fn wake(&self) -> Option<Instant> {
        let recheck = self.at.checked_add(RECHECK);
        let trailing = recheck.filter(|_| self.owed);
        let after = |at: Instant| recheck.map_or(at, |r| at.max(r));
        let settle = self.settle.map(after);
        let pair = self.pair.map(after);
        [trailing, settle, pair].into_iter().flatten().min()
    }
}

/// THE PAIR READ (gap 25): `session`'s log names its run's harness read wait `wait`
/// ([`crate::session_log::SessionLog::harness_wait`]) with the instant a read to the rollout's end
/// settles it from, or none. A wait the ask has not taken owes one read at or after that instant
/// ([`RolloutAsk::pair`]): [`RolloutAsk::wake`] carries it to the loop's deadline
/// ([`crate::session_status::StatusObserver::next_wake`]) and it stands, through sweeps that skip
/// the session or come too soon, until [`due`] dispatches a read at or after it. A wait the ask
/// took already owes nothing more: a read that fails or stops short of the file's end leaves the
/// wait's [`ReadWait::read_ms`] as it was, so unless the rows it applied or an act logged since
/// move the wait, only the tap's own cadence reads again. No wait clears the pair read, and a
/// session with no ask takes none.
pub(crate) fn pair_wait(
    asked: &mut HashMap<u64, RolloutAsk>,
    session: u64,
    wait: Option<(ReadWait, Instant)>,
) {
    let Some(ask) = asked.get_mut(&session) else {
        return;
    };
    ask.pair = match wait {
        Some((key, at)) if ask.wait != Some(key) => Some(at),
        Some(_) => ask.pair,
        None => None,
    };
    ask.wait = wait.map(|(key, _)| key);
}

/// Rollout action selected by the status sweep for one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ask {
    /// Not due, or nothing to request.
    Nothing,
    /// Request a rollout read for the target.
    Read(TapTarget),
    /// Stop the session's tap once after its target disappears.
    Stop,
}

/// Returns the next rollout [`Ask`] for `session` at `now`, its last output at `output`, given
/// what the sweep saw of the tab (`look`; `None` asks on the output's cadence alone).
/// A sweep inside [`RECHECK`] owes the trailing read where the session wrote since the last read,
/// or where that read went out within [`RECHECK`] of the session's last output: codex-alab hands
/// its rollout items to a writer task of its own (`RolloutCmd::AddItems`), so a record can reach
/// the file just after the frame a burst ended on, and the read that frame's sweep sent. A
/// codex-alab at rest, its output older than [`RECHECK`] at the read, swept again by a wake of
/// someone else's (its own `aterm ctl` reads, the fabric bridge's `sessions status`), owes none and
/// arms no timer for a rollout nothing moved. Apart from that, a screen that moved while the
/// record names a count owes one more read [`SETTLE`] after the move, re-armed by each later move
/// and discharged by the first read at or after it; a pair read ([`pair_wait`]) is kept, and
/// discharged, the same way.
pub(crate) fn due(
    asked: &mut HashMap<u64, RolloutAsk>,
    session: u64,
    target: Option<TapTarget>,
    look: Option<Look>,
    (now, output): (Instant, Option<Instant>),
) -> Ask {
    let Some(target) = target else {
        return if asked.remove(&session).is_some() {
            Ask::Stop
        } else {
            Ask::Nothing
        };
    };
    let prior = asked.get(&session).copied();
    let screen = look.map(|l| l.screen);
    let settle = match look {
        Some(l) if l.counted && prior.is_none_or(|p| p.screen != screen) => now.checked_add(SETTLE),
        Some(l) if l.counted => prior.and_then(|p| p.settle),
        _ => None,
    };
    if let Some(prior) = asked.get_mut(&session)
        && now.saturating_duration_since(prior.at) < RECHECK
    {
        prior.owed |= output != prior.output
            || prior
                .output
                .is_some_and(|wrote| prior.at.saturating_duration_since(wrote) < RECHECK);
        prior.screen = screen;
        prior.settle = settle;
        return Ask::Nothing;
    }
    asked.insert(
        session,
        RolloutAsk {
            at: now,
            owed: false,
            output,
            screen,
            settle: settle.filter(|at| *at > now),
            wait: prior.and_then(|p| p.wait),
            pair: prior.and_then(|p| p.pair).filter(|at| *at > now),
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
    /// Client build for rollout binding ([`aterm_agent::harness::codex_alab::alab_build`]).
    pub(crate) build: fn(u32) -> Option<String>,
    /// Product version from the running image's package, rechecked after a rollout read.
    pub(crate) version: fn(u32) -> Option<String>,
    /// Process group of `pid`.
    pub(crate) group: fn(u32) -> Option<i32>,
    /// The worker task `pid` was spawned on ([`crate::survey::task_prompt_of_env`]).
    pub(crate) task: fn(u32) -> Option<String>,
    /// The session record of `(pid, start)` under a `$CODEX_HOME`, read while that identity is
    /// live ([`aterm_agent::harness::self_claimed::read_seen`]): a read-only target's, at each ask.
    pub(crate) record:
        fn(&std::path::Path, u32, &str) -> Option<aterm_agent::harness::self_claimed::Seen>,
}

impl Image {
    /// Live kernel callbacks for process birth and running ALab build.
    pub(crate) const KERNEL: Image = Image {
        start: aterm_agent::harness::restart_stamp::kernel_start,
        born_ms,
        build: running_build,
        version: running_version,
        group: live_group,
        task: spawned_task,
        record: aterm_agent::harness::self_claimed::read_seen,
    };
}

/// One ask: the session, its timeline, what its claim or read-only copy names, what a moved
/// log wakes, and how the claimant's process is looked at.
struct Job {
    session: u64,
    ctx: Weak<crate::SessionCtx>,
    target: TapTarget,
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
        target: TapTarget,
        wake: &LogWake,
        image: Image,
    ) -> Option<Self> {
        let _claim = crate::self_claim::hold_target(ctx, &target)?;
        let run = ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .slog()
            .transcript_arm(Source::Rollout)
            .filter(|arm| arm.run.led_by(target.pid()))?
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
    target: TapTarget,
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

/// One session's tap, the rollout it was made for, and the refusal last logged.
struct Held {
    tapped: Tapped,
    run: TapRun,
    tap: RolloutTap,
    said: Option<Refusal>,
    /// The survey's reader of the same rollout, once its file is found ([`publish_facts`]).
    facts: Option<CodexFactsTail>,
    /// The claimant's worker task, read once.
    task: Option<String>,
    /// What [`publish_facts`] last published from it.
    shown: Option<crate::survey::CodexSurvey>,
    /// The background terminals [`publish_terminals`] last published from the claimant's record.
    terminals: Option<u32>,
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
        .is_some_and(|held| held.shown.is_some() || held.terminals.is_some())
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

/// The product version of the package whose image `pid` runs; never the selected package.
fn running_version(pid: u32) -> Option<String> {
    let exe = aterm_uds::sudo::exe_path(i32::try_from(pid).ok()?)?;
    aterm_agent::harness::codex_alab::product_version(&exe)
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

/// [`read_once`]'s guarded arm, read and apply, and the answer ([`RolloutWord`]): bound (read
/// `on`) or partial, the tap's refusal, unbound, no thread (a read-only target's record naming
/// none among them), or stale when the queued claim or copy, the thread it names, the run or the
/// sampled process image no longer matches.
fn read_on(
    taps: &mut Taps,
    job: &Job,
    after_read: impl FnOnce(),
) -> (Option<Applied>, RolloutWord) {
    let tapped = job.ctx.upgrade().map_or(Err(Untapped::Moved), |ctx| {
        crate::self_claim::tapped(&ctx, &job.target, job.image.record)
    });
    let tapped = match tapped {
        Ok(tapped) => tapped,
        Err(why) => {
            drop_held(taps, job.session, job.ctx.upgrade().as_deref());
            return (
                None,
                match why {
                    Untapped::Moved => RolloutWord::Stale,
                    Untapped::Unnamed => RolloutWord::NoThread,
                },
            );
        }
    };
    let (pid, start) = (job.target.pid(), job.target.start());
    let read = || {
        let ctx = job.ctx.upgrade()?;
        let (arm, began) = {
            let _claim = crate::self_claim::hold_rollout(&ctx, &tapped)?;
            let timeline = ctx.timeline.lock().unwrap_or_else(PoisonError::into_inner);
            let arm = timeline
                .slog()
                .transcript_arm(Source::Rollout)
                .filter(|arm| arm.run == job.run)?;
            (arm, timeline.slog().now().wall_ms)
        };
        ((job.image.start)(pid).as_deref() == Some(start)).then_some((ctx, arm, began))
    };
    let Some((ctx, arm, began)) = read() else {
        drop_held(taps, job.session, job.ctx.upgrade().as_deref());
        return (None, RolloutWord::Stale);
    };
    if taps
        .get(&job.session)
        .is_some_and(|held| held.tapped != tapped || held.run != job.run)
    {
        drop_held(taps, job.session, Some(&ctx));
    }
    let held = match taps.entry(job.session) {
        Entry::Occupied(e) => e.into_mut(),
        Entry::Vacant(e) => {
            let Some(tap) = RolloutTap::new(&tapped.thread, tapped.target.home()) else {
                return (None, RolloutWord::NoThread);
            };
            e.insert(Held {
                tapped: tapped.clone(),
                run: job.run,
                tap,
                said: None,
                facts: None,
                task: (job.image.task)(pid),
                shown: None,
                terminals: None,
            })
        }
    };
    let floor = arm.floor_ms.max((job.image.born_ms)(pid).unwrap_or(0));
    let running = (job.image.build)(pid);
    let version = (job.image.version)(pid);
    held.tap.arm(floor, arm.given, running.as_deref());
    // A read that reached the rollout's end read every row written before it began (`began`, on
    // the log's clock): the harness pairing's watermark (`SessionLog::harness_pair`). A read that
    // failed or stopped short gives none.
    let eof = held
        .tap
        .read(FOLD_BUDGET)
        .filter(|read| read.complete)
        .map(|_| began);
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
    let word = match (held.tap.reads_at(), refused) {
        (Some(StepState::On), _) => RolloutWord::Bound,
        (Some(StepState::Partial), _) => RolloutWord::Partial,
        (None, Some(refusal)) => RolloutWord::Refused(refusal),
        (None, None) => RolloutWord::Unbound,
    };
    let batch = held.tap.take();
    let record = aterm_agent::harness::self_claimed::read_record_facts(
        tapped.target.home(),
        tapped.target.pid(),
        &tapped.thread,
    );
    let survey = read_facts(held, &tapped, record.goal);
    after_read();
    if (job.image.start)(pid).as_deref() != Some(start)
        || (job.image.build)(pid) != running
        || (job.image.version)(pid) != version
    {
        drop_held(taps, job.session, Some(&ctx));
        return (None, RolloutWord::Stale);
    }
    let applied = apply_rollout_batch(&ctx, &tapped, job.run, &batch, (held.tap.is_bound(), eof));
    if applied.disarm {
        drop_held(taps, job.session, Some(&ctx));
        return (Some(applied), RolloutWord::Stale);
    }
    if let Some(survey) = survey {
        publish_facts(&ctx, job, &tapped, held, survey);
    }
    if held.terminals != record.background_terminals {
        publish_terminals(&ctx, job, &tapped, held, record.background_terminals);
    }
    (Some(applied), word)
}

/// The survey's facts from `held`'s rollout, whatever build wrote it (the tap's steps refuse a
/// `session_meta` of a shape the fold does not know, these facts are any codex's), with the `goal`
/// the claimant's own record names ([`read_on`] reads it), when they differ from those last shown.
fn read_facts(
    held: &mut Held,
    tapped: &Tapped,
    goal: Option<String>,
) -> Option<crate::survey::CodexSurvey> {
    if held.facts.is_none()
        && let Some(path) = rollout_path(tapped.target.home(), &tapped.thread)
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

/// Publishes `survey` for the claimant's group while its claim or read-only copy still names
/// `tapped`; kept as shown only once the timeline took it, so a group not in front is offered it
/// again at the next read.
fn publish_facts(
    ctx: &crate::SessionCtx,
    job: &Job,
    tapped: &Tapped,
    held: &mut Held,
    survey: crate::survey::CodexSurvey,
) {
    if publish(ctx, job, tapped, |timeline, group| {
        timeline.set_codex_survey(group, survey.clone())
    }) {
        held.shown = Some(survey);
    }
}

/// Publishes the background terminals codex-alab's own record counts for the claimant's thread on
/// their own slot: a count alone is no survey fact, so a tab with no rollout keeps its screen's
/// read. Kept as shown only once the timeline took it, as [`publish_facts`] keeps its facts.
fn publish_terminals(
    ctx: &crate::SessionCtx,
    job: &Job,
    tapped: &Tapped,
    held: &mut Held,
    count: Option<u32>,
) {
    if publish(ctx, job, tapped, |timeline, group| {
        timeline.set_codex_terminals(group, count)
    }) {
        held.terminals = count;
    }
}

/// Runs `set` on the timeline for the claimant's group while its claim or read-only copy still
/// names `tapped`, and says the footer changed when it took: whether it did.
fn publish(
    ctx: &crate::SessionCtx,
    job: &Job,
    tapped: &Tapped,
    set: impl FnOnce(&mut SessionTimeline, i32) -> bool,
) -> bool {
    let Some(group) = (job.image.group)(job.target.pid()) else {
        return false;
    };
    let took = {
        let Some(_claim) = crate::self_claim::hold_rollout(ctx, tapped) else {
            return false;
        };
        set(
            &mut ctx.timeline.lock().unwrap_or_else(PoisonError::into_inner),
            group,
        )
    };
    if took {
        crate::claude_footer::post_changed(job.session);
    }
    took
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
        target: impl Into<TapTarget>,
        image: Image,
    ) -> Option<Applied> {
        let job = Job::capture_job(session, ctx, target.into(), &LogWake::default(), image)?;
        read_once(&mut self.0, &job)
    }

    pub(crate) fn queued(
        &mut self,
        session: u64,
        ctx: &Arc<crate::SessionCtx>,
        target: impl Into<TapTarget>,
        image: Image,
        schedule: (impl FnOnce(), impl FnOnce()),
    ) -> Option<Applied> {
        let job = Job::capture_job(session, ctx, target.into(), &LogWake::default(), image)?;
        schedule.0();
        read_once_after(&mut self.0, &job, schedule.1)
    }

    /// Runs one rollout read/apply pass and returns the answer's word.
    pub(crate) fn read_on(
        &mut self,
        session: u64,
        ctx: &Arc<crate::SessionCtx>,
        target: impl Into<TapTarget>,
        image: Image,
    ) -> &'static str {
        let Some(job) = Job::capture_job(session, ctx, target.into(), &LogWake::default(), image)
        else {
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
#[path = "codex_rollout_tap_tests.rs"]
mod tests;
