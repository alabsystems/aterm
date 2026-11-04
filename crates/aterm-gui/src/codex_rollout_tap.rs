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
mod tests {
    use super::*;

    fn target(thread: &str) -> TapTarget {
        crate::self_claim::RolloutTarget {
            revision: 1,
            pid: 4242,
            start: "Mon Oct 20 16:37:58 2026".to_string(),
            thread: thread.to_string(),
            home: std::path::PathBuf::from("/private/tmp/x/codex-home"),
        }
        .into()
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
        assert_eq!(due(&mut asked, 7, None, None, (at(0), None)), Ask::Nothing);
        assert_eq!(
            due(&mut asked, 7, Some(target(THREAD)), None, (at(0), None)),
            Ask::Read(target(THREAD))
        );
        assert_eq!(asked.get(&7).map(|a| a.owed), Some(false));
        for ms in [1, 250, 999] {
            assert_eq!(
                due(
                    &mut asked,
                    7,
                    Some(target(THREAD)),
                    None,
                    (at(ms), Some(at(ms)))
                ),
                Ask::Nothing,
                "{ms}"
            );
            assert_eq!(asked.get(&7).map(|a| a.owed), Some(true), "{ms}");
        }
        assert_eq!(
            due(&mut asked, 8, Some(target(THREAD)), None, (at(500), None)),
            Ask::Read(target(THREAD))
        );
        assert_eq!(
            due(&mut asked, 7, Some(target(THREAD)), None, (at(1_000), None)),
            Ask::Read(target(THREAD))
        );
        assert_eq!(asked.get(&7).map(|a| a.owed), Some(false));
        assert_eq!(
            due(&mut asked, 7, Some(target(THREAD)), None, (at(1_400), None)),
            Ask::Nothing
        );
        assert_eq!(due(&mut asked, 7, None, None, (at(1_500), None)), Ask::Stop);
        assert_eq!(
            due(&mut asked, 7, None, None, (at(1_600), None)),
            Ask::Nothing
        );
        assert_eq!(
            due(&mut asked, 7, Some(target(THREAD)), None, (at(1_700), None)),
            Ask::Read(target(THREAD)),
            "back in front: asked at once"
        );
        assert_eq!(due(&mut asked, 8, None, None, (at(1_800), None)), Ask::Stop);
    }

    /// AT REST, NO TRAILING READ: a claimant whose last output was older than [`RECHECK`] at its
    /// read, and wrote nothing since, swept again inside [`RECHECK`] by wakes not its own, owes no
    /// read, so the sweep's `next_wake` arms no timer for it (`lab codex-alab-proof
    /// --window-only`'s END: `timer_wakes=0`). CONTROL: output since the read owes one.
    #[test]
    fn a_claimant_at_rest_owes_no_trailing_read() {
        const THREAD: &str = "019a0000-0000-7000-8000-00000000000b";
        let mut asked = HashMap::new();
        let t0 = Instant::now() + Duration::from_secs(10);
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let wrote = Some(t0 - Duration::from_secs(5));
        assert_eq!(
            due(&mut asked, 7, Some(target(THREAD)), None, (at(0), wrote)),
            Ask::Read(target(THREAD))
        );
        for ms in [300, 600, 900] {
            assert_eq!(
                due(&mut asked, 7, Some(target(THREAD)), None, (at(ms), wrote)),
                Ask::Nothing
            );
            assert_eq!(
                asked.get(&7).map(|a| a.owed),
                Some(false),
                "{ms}: nothing moved"
            );
        }
        assert_eq!(
            due(
                &mut asked,
                7,
                Some(target(THREAD)),
                None,
                (at(950), Some(at(940)))
            ),
            Ask::Nothing
        );
        assert_eq!(
            asked.get(&7).map(|a| a.owed),
            Some(true),
            "CONTROL: output since the read"
        );
    }

    /// JUST AFTER A BURST, ONE TRAILING READ: codex-alab's rollout writer can put a record in the
    /// file after the frame that ended its burst, so a read sent within [`RECHECK`] of the last
    /// output owes one trailing read though nothing was written since; that trailing read, past
    /// [`RECHECK`] of the output, owes none. CONTROL: the same sweeps after a read sent at rest owe
    /// nothing ([`a_claimant_at_rest_owes_no_trailing_read`]).
    #[test]
    fn a_read_just_after_a_burst_owes_one_trailing_read() {
        const THREAD: &str = "019a0000-0000-7000-8000-00000000000c";
        let mut asked = HashMap::new();
        let t0 = Instant::now() + Duration::from_secs(10);
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let wrote = Some(at(0));
        assert_eq!(
            due(&mut asked, 7, Some(target(THREAD)), None, (at(250), wrote)),
            Ask::Read(target(THREAD))
        );
        assert_eq!(
            due(&mut asked, 7, Some(target(THREAD)), None, (at(500), wrote)),
            Ask::Nothing
        );
        assert_eq!(
            asked.get(&7).map(|a| a.owed),
            Some(true),
            "a read 250 ms after the burst owes a trailing read"
        );
        assert_eq!(
            due(
                &mut asked,
                7,
                Some(target(THREAD)),
                None,
                (at(1_250), wrote)
            ),
            Ask::Read(target(THREAD)),
            "the trailing read"
        );
        assert_eq!(
            due(
                &mut asked,
                7,
                Some(target(THREAD)),
                None,
                (at(1_500), wrote)
            ),
            Ask::Nothing
        );
        assert_eq!(
            asked.get(&7).map(|a| a.owed),
            Some(false),
            "the trailing read, 1.25 s after the burst, owes none"
        );
        let mut rest = HashMap::new();
        let old = Some(t0 - Duration::from_secs(5));
        let _ = due(&mut rest, 7, Some(target(THREAD)), None, (at(250), old));
        let _ = due(&mut rest, 7, Some(target(THREAD)), None, (at(500), old));
        assert_eq!(
            rest.get(&7).map(|a| a.owed),
            Some(false),
            "CONTROL: a read sent at rest"
        );
    }

    /// THE SETTLE: while the record names a count, each sweep that sees the screen move owes one
    /// more read [`SETTLE`] after it, re-armed by each later move and kept across a read
    /// dispatched before it (that read may precede codex-alab's coalesced write); the first read at
    /// or after it discharges it, and nothing more is owed while the screen rests. An uncounted
    /// look, or none, arms nothing. The wake never falls inside the [`RECHECK`] after a read,
    /// where `due` would refuse it.
    #[test]
    fn a_settle_read_follows_the_screens_last_move_while_the_record_counts() {
        const THREAD: &str = "019a0000-0000-7000-8000-00000000000b";
        let g = |seq: u64| crate::control::ScreenGen { epoch: 1, seq };
        let look = |seq: u64, counted: bool| {
            Some(Look {
                screen: g(seq),
                counted,
            })
        };
        let mut asked = HashMap::new();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let ask = |asked: &HashMap<u64, RolloutAsk>| *asked.get(&7).expect("asked");

        assert_eq!(
            due(
                &mut asked,
                7,
                Some(target(THREAD)),
                look(1, true),
                (t0, Some(t0))
            ),
            Ask::Read(target(THREAD))
        );
        assert_eq!(
            ask(&asked).settle,
            Some(t0 + SETTLE),
            "the first look arms it"
        );
        assert_eq!(
            due(
                &mut asked,
                7,
                Some(target(THREAD)),
                look(1, true),
                (at(500), Some(at(500)))
            ),
            Ask::Nothing
        );
        assert!(ask(&asked).owed);
        assert_eq!(
            ask(&asked).settle,
            Some(t0 + SETTLE),
            "a still screen keeps it"
        );
        assert_eq!(
            due(
                &mut asked,
                7,
                Some(target(THREAD)),
                look(2, true),
                (at(800), Some(at(800)))
            ),
            Ask::Nothing
        );
        assert_eq!(
            ask(&asked).settle,
            Some(at(800) + SETTLE),
            "a move re-arms it"
        );
        assert_eq!(
            ask(&asked).wake(),
            Some(t0 + RECHECK),
            "the trailing read first"
        );
        assert_eq!(
            due(
                &mut asked,
                7,
                Some(target(THREAD)),
                look(2, true),
                (t0 + RECHECK, Some(t0 + RECHECK))
            ),
            Ask::Read(target(THREAD))
        );
        assert_eq!(
            ask(&asked).settle,
            Some(at(800) + SETTLE),
            "a read before the settle keeps it"
        );
        assert_eq!(ask(&asked).wake(), Some(at(800) + SETTLE));
        assert_eq!(
            due(
                &mut asked,
                7,
                Some(target(THREAD)),
                look(2, true),
                (at(800) + SETTLE, Some(at(800) + SETTLE))
            ),
            Ask::Read(target(THREAD))
        );
        assert_eq!(ask(&asked).settle, None, "the settle read discharges it");
        assert_eq!(
            ask(&asked).wake(),
            None,
            "nothing more while the screen rests"
        );
        assert_eq!(
            due(
                &mut asked,
                7,
                Some(target(THREAD)),
                look(3, false),
                (at(6_000), Some(at(6_000)))
            ),
            Ask::Read(target(THREAD))
        );
        assert_eq!(
            ask(&asked).settle,
            None,
            "a record naming no count arms nothing"
        );
        assert_eq!(
            due(
                &mut asked,
                7,
                Some(target(THREAD)),
                None,
                (at(8_000), Some(at(8_000)))
            ),
            Ask::Read(target(THREAD))
        );
        assert_eq!(ask(&asked).settle, None, "no look arms nothing");
        assert_eq!(ask(&asked).screen, None);

        let base = RolloutAsk {
            at: t0,
            owed: false,
            output: None,
            screen: None,
            settle: None,
            wait: None,
            pair: None,
        };
        assert_eq!(base.wake(), None, "neither");
        assert_eq!(
            RolloutAsk { owed: true, ..base }.wake(),
            Some(t0 + RECHECK),
            "owed only"
        );
        assert_eq!(
            RolloutAsk {
                settle: Some(at(1_500)),
                ..base
            }
            .wake(),
            Some(at(1_500)),
            "settle only"
        );
        assert_eq!(
            RolloutAsk {
                settle: Some(at(300)),
                ..base
            }
            .wake(),
            Some(t0 + RECHECK),
            "settle only, inside the recheck: due reads no sooner"
        );
        assert_eq!(
            RolloutAsk {
                owed: true,
                settle: Some(at(300)),
                ..base
            }
            .wake(),
            Some(t0 + RECHECK),
            "both, the settle inside the recheck"
        );
        assert_eq!(
            RolloutAsk {
                owed: true,
                settle: Some(at(3_000)),
                ..base
            }
            .wake(),
            Some(t0 + RECHECK),
            "both: the trailing read is earlier"
        );

        // A late trailing read keeps a settle that lies within RECHECK of it: the wake names the
        // first instant due reads, not the settle, which due would refuse.
        let mut late = HashMap::new();
        assert_eq!(
            due(
                &mut late,
                9,
                Some(target(THREAD)),
                look(1, true),
                (t0, Some(t0))
            ),
            Ask::Read(target(THREAD))
        );
        assert_eq!(
            due(
                &mut late,
                9,
                Some(target(THREAD)),
                look(2, true),
                (at(100), Some(at(100)))
            ),
            Ask::Nothing
        );
        assert_eq!(
            due(
                &mut late,
                9,
                Some(target(THREAD)),
                look(2, true),
                (at(1_500), Some(at(1_500)))
            ),
            Ask::Read(target(THREAD)),
            "the trailing read, late"
        );
        let kept = *late.get(&9).expect("asked");
        assert_eq!(kept.settle, Some(at(2_100)), "the settle stands past it");
        let mut refused = late.clone();
        assert_eq!(
            due(
                &mut refused,
                9,
                Some(target(THREAD)),
                look(2, true),
                (at(2_100), Some(at(2_100)))
            ),
            Ask::Nothing,
            "premise: due refuses a read at the settle itself"
        );
        assert_eq!(
            kept.wake(),
            Some(at(1_500) + RECHECK),
            "the wake waits out the recheck"
        );
        assert_eq!(
            due(
                &mut late,
                9,
                Some(target(THREAD)),
                look(2, true),
                (at(1_500) + RECHECK, Some(at(1_500) + RECHECK))
            ),
            Ask::Read(target(THREAD))
        );
        assert_eq!(
            late.get(&9).expect("asked").settle,
            None,
            "that read discharges the settle"
        );
    }

    /// THE PAIR READ (gap 25, [`pair_wait`]): a harness read wait the log names owes one read at
    /// or after the instant it gives, carried by [`RolloutAsk::wake`] and kept across a read
    /// dispatched before it; the first read at or after it discharges it. The same wait handed
    /// again (the read failed or stopped short of the file's end) owes nothing more, so the ask
    /// arms no wake for it, however long the turn waits; a wait a read to the end moved (its
    /// watermark rose, the read begun before the wait's time) owes one more read, no sooner than
    /// the recheck. No wait clears it, and a session with no ask takes none.
    #[test]
    fn a_harness_read_wait_owes_one_read_at_or_after_it() {
        const THREAD: &str = "019a0000-0000-7000-8000-00000000000e";
        let mut asked = HashMap::new();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let ask = |asked: &HashMap<u64, RolloutAsk>| *asked.get(&7).expect("asked");
        let wait = |read_ms: u64| ReadWait {
            since: 9_000,
            read_ms,
        };
        let read = |asked: &mut HashMap<u64, RolloutAsk>, ms: u64| {
            due(asked, 7, Some(target(THREAD)), None, (at(ms), None))
        };

        pair_wait(&mut asked, 7, Some((wait(0), at(3_000))));
        assert!(asked.is_empty(), "no ask, nothing taken");
        assert_eq!(read(&mut asked, 0), Ask::Read(target(THREAD)));
        assert_eq!(
            ask(&asked).wake(),
            None,
            "premise: a read at rest owes nothing"
        );
        pair_wait(&mut asked, 7, Some((wait(0), at(3_000))));
        assert_eq!(
            ask(&asked).wake(),
            Some(at(3_000)),
            "a new wait owes its read"
        );
        assert_eq!(read(&mut asked, 1_500), Ask::Read(target(THREAD)));
        assert_eq!(
            ask(&asked).wake(),
            Some(at(3_000)),
            "a read before it keeps it"
        );
        pair_wait(&mut asked, 7, Some((wait(0), at(3_000))));
        assert_eq!(
            ask(&asked).wake(),
            Some(at(3_000)),
            "the same wait again keeps it"
        );
        assert_eq!(read(&mut asked, 2_000), Ask::Nothing, "inside the recheck");
        assert_eq!(
            ask(&asked).wake(),
            Some(at(3_000)),
            "a refused sweep keeps it"
        );
        assert_eq!(read(&mut asked, 3_000), Ask::Read(target(THREAD)));
        assert_eq!(ask(&asked).pair, None, "the read at it discharges it");
        assert_eq!(ask(&asked).wake(), None);
        pair_wait(&mut asked, 7, Some((wait(0), at(3_000))));
        assert_eq!(
            ask(&asked).wake(),
            None,
            "a failed read leaves the wait as it was: nothing more is owed"
        );
        assert_eq!(read(&mut asked, 4_000), Ask::Read(target(THREAD)));
        pair_wait(&mut asked, 7, Some((wait(0), at(4_000))));
        assert_eq!(ask(&asked).wake(), None, "nor at the next read's");
        pair_wait(&mut asked, 7, Some((wait(1_900), at(4_000))));
        assert_eq!(
            ask(&asked).wake(),
            Some(at(4_000) + RECHECK),
            "a read to the end that left it waiting owes one more, after the recheck"
        );
        pair_wait(&mut asked, 7, None);
        assert_eq!((ask(&asked).wait, ask(&asked).pair), (None, None));
        assert_eq!(ask(&asked).wake(), None, "no wait clears it");
    }
}
