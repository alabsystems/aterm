// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CODEX ROLLOUT TAP'S HOST (session log 5G-3; plan amendment A1, step
//! 6D-3b; ruling T15): a codex-alab session's rollout steps reach its
//! session log on one named thread of their own (`aterm-codex-rollout`), as
//! a Claude Code transcript's reach it on the footer resolver's.
//!
//! - WHO IS TAPPED. The status sweep asks
//!   (`SessionStatus::request_rollout_if_due`), at most every [`RECHECK`],
//!   while the program in front is the one a proven self-claim speaks for
//!   (`self_claim::fronts_claimant`) and that claim's copy of its record
//!   names a thread (`self_claim::rollout_target`: the gate's claim half).
//!   The build half is the tap's own (`aterm_agent::harness::codex_tap`):
//!   the thread's rollout binds only on a build `CODEX_MEASURED_BUILDS`
//!   names, and only while the claimant's image runs one it names too. A
//!   vendor Codex is never tapped, nor an unmeasured build.
//! - EACH ASK ([`read_once`]). One short hold of the timeline lock arms the
//!   tap from the log's open Codex run (`SessionLog::transcript_arm`,
//!   [`Source::Rollout`]): with none, the session's tap goes and nothing is
//!   read. Then, no lock held, the claimant's process is looked at
//!   ([`Image`]: its start and the build its image runs now, which a thread
//!   resumed or re-exec'd onto another build never restates in its
//!   rollout) and the tap reads on, at most `FOLD_BUDGET` bytes, its floor
//!   the latest of the run's and the process's start, and only while that
//!   build is a measured one; the batch is applied through the transcript glue
//!   (`session_log::transcript::apply_batch`: chunked holds, every record
//!   `src=transcript`), and the loop is woken as the footer resolver wakes
//!   it (`claude_footer::LogWake`).
//! - OWNERSHIP AT APPLY. Every job keeps its claim revision and exact run
//!   (log epoch, node and group). The live claim and copied thread/home are
//!   fenced at capture, dequeue and every apply chunk, in meta → copies →
//!   timeline order. Process start/build reads stay off-lock, before and
//!   after file IO; a failed look binds nothing. A queued stop is not the
//!   authority that prevents an older job from entering a newer run.
//! - A NEW IDENTITY (another pid or start, another thread or home) is a new
//!   tap; a sweep switched off and the claimant gone from the front drop it
//!   and say so (`status rollout=stopped`, [`stop_session`]), and a retired
//!   session's goes with it ([`retire_session`]).
//!
//! Nothing is held here but the tap (ids and numbers: ruling S6), and
//! nothing is logged but a refusal's word.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use aterm_agent::harness::codex_tap::RolloutTap;
use aterm_agent::harness::footer::FOLD_BUDGET;

use crate::claude_footer::LogWake;
use crate::self_claim::RolloutTarget;
use crate::session_log::transcript::{Applied, Source, TapRun, apply_rollout_batch};
use crate::session_timeline::SessionTimeline;

/// How often a codex-alab session's rollout is read on while it is in
/// front: one open and the bytes appended since the last read (the
/// rollout's folder is looked in only while none is bound).
pub(crate) const RECHECK: Duration = Duration::from_secs(1);

/// What the status sweep does for one session at one look ([`due`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ask {
    /// Nothing: not due, or never asked and nothing to ask.
    Nothing,
    /// Ask for the target's rollout to be read on ([`request_as`]).
    Read(RolloutTarget),
    /// The claim behind the asks is gone: drop the session's tap
    /// ([`stop_session`]), once.
    Stop,
}

/// THE CADENCE (pure; `SessionStatus::request_rollout_if_due`): with a
/// `target` (a proven claimant in front whose copy names a thread), a read
/// is asked at once and then at most every [`RECHECK`], `asked` keeping
/// when; with none (the program in front moved, the claim or its copy
/// gone), a session that was asked for is stopped once and forgotten.
pub(crate) fn due(
    asked: &mut HashMap<u64, Instant>,
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
    let due = asked
        .get(&session)
        .is_none_or(|at| now.saturating_duration_since(*at) >= RECHECK);
    if !due {
        return Ask::Nothing;
    }
    asked.insert(session, now);
    Ask::Read(target)
}

/// What a tap reads of the claimant's process (module docs): injected, the
/// kernel's in the app ([`Image::KERNEL`]) and stand-ins in tests, which
/// have no such process.
#[derive(Clone, Copy)]
pub(crate) struct Image {
    /// The kernel identity named by the proof (unreadable is not a match).
    pub(crate) start: fn(u32) -> Option<String>,
    /// The process's start, wall ms, where the kernel names it.
    pub(crate) born_ms: fn(u32) -> Option<u64>,
    /// The build its image runs now (its package's version, whole), when
    /// that is an ALab build's.
    pub(crate) build: fn(u32) -> Option<String>,
}

impl Image {
    /// The kernel's: the process's birth, and its image's package
    /// (`proc_pidpath`, then `codex_alab::alab_build`). A syscall and a few
    /// small file reads at each ask, on the tap's own thread, never under a
    /// lock.
    pub(crate) const KERNEL: Image = Image {
        start: aterm_agent::harness::restart_stamp::kernel_start,
        born_ms,
        build: running_build,
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

/// WHAT THE STATUS SWEEP DOES with one look's [`Ask`] for `session`
/// (`SessionStatus::request_rollout_if_due`): read on ([`request_as`]),
/// stop and say so ([`stop_session`]), or nothing.
pub(crate) fn follow_ask(ask: Ask, session: u64, ctx: &Arc<crate::SessionCtx>, wake: &LogWake) {
    follow_ask_as(ask, session, ctx, wake, Image::KERNEL);
}

/// [`follow_ask`], the claimant's process looked at through `image`.
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

/// Read `target`'s rollout on into `ctx`'s captured session-log run, off-thread
/// ([`read_once`]), the claimant's process looked at through `image`. If
/// the thread cannot be started nothing is read.
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
        // It takes the session timeline lock the UI thread contends, so the
        // lock-holder floor applies.
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

/// Drop `session`'s tap, the sweep off or the claimant gone from the front,
/// and say so: once every ask sent before it is answered (the thread's queue
/// is in order), `status rollout=` says `stopped` for a session the tap
/// answered, its `rollout_asks=` kept ([`stop_tap`]).
pub(crate) fn stop_session(session: u64, timeline: &Arc<Mutex<SessionTimeline>>) {
    send_stop(session, Some(Arc::clone(timeline)));
}

/// Drop a retired session's tap: nothing reads its status any more.
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
    said: Option<&'static str>,
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
    if let Some(timeline) = timeline {
        timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .slog_mut()
            .rollout_stopped();
    }
}

/// The start of process `pid`, wall ms, where the kernel names it.
fn born_ms(pid: u32) -> Option<u64> {
    let birth = aterm_uds::process::read_process_birth(i32::try_from(pid).ok()?)?;
    birth.seconds.checked_mul(1000)
}

/// The build process `pid`'s image runs, when it is an ALab build's.
fn running_build(pid: u32) -> Option<String> {
    let exe = aterm_uds::sudo::exe_path(i32::try_from(pid).ok()?)?;
    aterm_agent::harness::codex_alab::alab_build(&exe)
}

/// ONE ASK (module docs): arm, read on, apply, and then, in a hold of its
/// own, count the ask answered with its word
/// (`SessionLog::rollout_answered`, `status rollout= rollout_asks=`): every
/// record the ask wrote is in the log before the count moves. What the
/// applied batch did; `None` when no run takes the rollout's steps (its tap
/// dropped) or the thread is no thread id.
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

/// [`read_once`]'s guarded arm, read and apply, and the answer's word:
/// `bound`, the tap's refusal, `no-thread`, or `stale` when the queued
/// claim, copied target, run or sampled process image no longer matches.
fn read_on(
    taps: &mut Taps,
    job: &Job,
    after_read: impl FnOnce(),
) -> (Option<Applied>, &'static str) {
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
        taps.remove(&job.session);
        return (None, "stale");
    };
    if taps
        .get(&job.session)
        .is_some_and(|held| held.target != job.target || held.run != job.run)
    {
        taps.remove(&job.session);
    }
    let held = match taps.entry(job.session) {
        Entry::Occupied(e) => e.into_mut(),
        Entry::Vacant(e) => {
            let Some(tap) = RolloutTap::new(&job.target.thread, &job.target.home) else {
                return (None, "no-thread");
            };
            e.insert(Held {
                target: job.target.clone(),
                run: job.run,
                tap,
                said: None,
            })
        }
    };
    let floor = arm
        .floor_ms
        .max((job.image.born_ms)(job.target.pid).unwrap_or(0));
    let running = (job.image.build)(job.target.pid);
    held.tap.arm(floor, arm.given, running.as_deref());
    let _ = held.tap.read(FOLD_BUDGET);
    let refused = held.tap.refused().map(|r| r.word());
    if refused != held.said {
        held.said = refused;
        if let Some(word) = refused {
            aterm_log::info!("codex rollout tap #{}: binds nothing: {word}", job.session);
        }
    }
    let word = match (held.tap.is_bound(), refused) {
        (true, _) => "bound",
        (false, Some(word)) => word,
        (false, None) => "unbound",
    };
    let batch = held.tap.take();
    after_read();
    // Kernel reads stay off-lock. The claim/copy/run fence is checked AGAIN
    // by the writer under every chunk's locks, after all off-lock IO.
    if (job.image.start)(job.target.pid).as_deref() != Some(job.target.start.as_str())
        || (job.image.build)(job.target.pid) != running
    {
        taps.remove(&job.session);
        return (None, "stale");
    }
    let applied = apply_rollout_batch(&ctx, &job.target, job.run, &batch, held.tap.is_bound());
    if applied.disarm {
        taps.remove(&job.session);
        return (Some(applied), "stale");
    }
    (Some(applied), word)
}

/// The rollout thread's taps, for a test that asks by hand ([`read_once`]
/// on this thread, the claimant looked at through a stand-in [`Image`]).
#[cfg(test)]
#[derive(Default)]
pub(crate) struct Host(Taps);

#[cfg(test)]
impl Host {
    /// One ask, as the rollout thread answers it.
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

    /// One ask's read and apply alone, as the rollout thread runs them
    /// before it counts the ask ([`read_on`]): the answer's word.
    pub(crate) fn read_on(
        &mut self,
        session: u64,
        ctx: &Arc<crate::SessionCtx>,
        target: RolloutTarget,
        image: Image,
    ) -> &'static str {
        let Some(job) = Job::capture_job(session, ctx, target, &LogWake::default(), image) else {
            return "stale";
        };
        read_on(&mut self.0, &job, || {}).1
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
    /// stopped once, and a target again is asked for at once. A session
    /// never asked for is never stopped. NEGATIVE CONTROLS (scratch, the
    /// unit's notes): a cadence that asks at every look, and a sweep that
    /// forgets without stopping, each fail it.
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
        for ms in [1, 250, 999] {
            assert_eq!(
                due(&mut asked, 7, Some(target(THREAD)), at(ms)),
                Ask::Nothing,
                "{ms}"
            );
        }
        // Another session keeps its own cadence.
        assert_eq!(
            due(&mut asked, 8, Some(target(THREAD)), at(500)),
            Ask::Read(target(THREAD))
        );
        assert_eq!(
            due(&mut asked, 7, Some(target(THREAD)), at(1_000)),
            Ask::Read(target(THREAD))
        );
        assert_eq!(
            due(&mut asked, 7, Some(target(THREAD)), at(1_400)),
            Ask::Nothing
        );
        // Gone: stopped once.
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
