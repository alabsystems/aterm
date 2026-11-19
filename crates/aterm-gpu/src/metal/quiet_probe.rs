// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The first commit after the GPU went quiet (ATERM-KI-1027-4; the reading frame's submit
//! burst of the 2026-11-17 profile). It runs planned takes (the bench's `benches/quiet/plan.rs`
//! makes the plan): each sleeps its quiet, with or without a keep, then commits one arm and
//! waits for it, telling the caller's [`QuietHost`] at each [`QuietMark`] so the caller can read
//! the process's counters there (this crate reads none). The arms are the renderer's
//! overlay-only present through a headless [`GpuRenderer`]'s public present calls (into the
//! virtual window's offscreen target, waited on: no drawable is acquired and no layer
//! presents, so whatever re-taking a drawable costs is not in it), hand-built clear, empty,
//! blit and two-buffer command buffers on the probe's own session, and (with
//! `--features compute`) a compute job on `compute`'s job path. It opens no window and reaches
//! no window server: `benches/quiet_commit.rs` runs it.

use std::sync::Arc;

use aterm_core::terminal::Terminal;
use aterm_render::Theme;
use aterm_time::{Duration, Instant};

use super::encoder::{EncodeSession, RenderPassDesc, StoreAction, Submitted};
use super::ffi::{
    AutoreleasePool, ClearColor, Device, LoadAction, PixelFormat, TEXTURE_USAGE_RENDER_TARGET,
};
use super::loss::{CbOutcome, LossLatch};
use super::resources::{MetalResourceDevice, SealedTexture};
use crate::{GpuRenderer, PresentCrop, TrayDelta, TrayQuad, WindowGpu};

/// What a take commits once its quiet ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum QuietArm {
    /// The renderer's overlay-only present: a headless [`GpuRenderer`]'s
    /// `present_overlay_virtual` of a stats card whose one row band changed (Submit B), into
    /// the virtual window's offscreen target and waited on. Not the windowed present: no
    /// drawable, no `CAMetalLayer`.
    P,
    /// One whole-target clear pass (Clear, Store), one command buffer.
    Clear,
    /// One 1x1 clear pass, one command buffer.
    Dot,
    /// [`Self::Clear`]'s command buffer and an empty second one on the same queue, production's
    /// CB1+CB2 shape.
    TwoCb,
    /// A command buffer committed with no encoder.
    EmptyCb,
    /// One blit command buffer copying a whole kept texture onto the target.
    Blit,
    /// One finite compute job through `compute`'s job path, on its own queue (built only with
    /// `--features compute`).
    Compute,
}

impl QuietArm {
    /// Every arm, in the order the summary lists them.
    pub const ALL: [Self; 7] = [
        Self::P,
        Self::Clear,
        Self::Dot,
        Self::TwoCb,
        Self::EmptyCb,
        Self::Blit,
        Self::Compute,
    ];

    /// The arm's name in `QUIET_COMMIT_ARMS` and in every output line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::P => "p",
            Self::Clear => "clear",
            Self::Dot => "dot",
            Self::TwoCb => "two_cb",
            Self::EmptyCb => "empty_cb",
            Self::Blit => "blit",
            Self::Compute => "compute",
        }
    }

    /// The arm named `name`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.name() == name)
    }
}

/// What keeps the GPU busy during a take's quiet: one piece of work every `period`. A keep in
/// this process starts at the take's [`QuietMark::QuietStart`] mark, and its last starts
/// before the quiet ends; [`Self::OtherProcessDot`] says when its child runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum QuietKeep {
    /// A 1x1 clear on the take's own queue: the renderer's for [`QuietArm::P`], the probe's
    /// session otherwise.
    SameQueueDot(Duration),
    /// A 1x1 clear on a second encode session of this process, with its own loss latch.
    OtherQueueDot(Duration),
    /// One compute job on `compute`'s own queue in this process. The job path cannot commit on
    /// the renderer's queue, so this stands where a same-queue compute keep cannot.
    OtherQueueCompute(Duration),
    /// A child process (the bench run again as `QUIET_COMMIT_ROLE=keeper`) committing a 1x1
    /// clear on its own device. It is spawned after the quiet began, just before the take's
    /// [`QuietMark::QuietStart`] mark (its first clear completed 15-61 ms after its spawn in
    /// the grid's first full run), and clears until the take settled and its host closed its
    /// stdin.
    OtherProcessDot(Duration),
}

impl QuietKeep {
    /// The keep's name in every output line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::SameQueueDot(_) => "same_queue_dot",
            Self::OtherQueueDot(_) => "other_queue_dot",
            Self::OtherQueueCompute(_) => "other_queue_compute",
            Self::OtherProcessDot(_) => "other_process_dot",
        }
    }

    /// How often it commits.
    #[must_use]
    pub const fn period(self) -> Duration {
        match self {
            Self::SameQueueDot(p)
            | Self::OtherQueueDot(p)
            | Self::OtherQueueCompute(p)
            | Self::OtherProcessDot(p) => p,
        }
    }
}

/// One take's plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct QuietTake {
    /// How long this process commits no work of its own (keeps aside) before the take. It
    /// starts at the last of: the previous take's completion, the previous take's keeper
    /// child's exit, the previous take's ballast release and its settle, and this take's
    /// ballast clear. Zero is back to back: the take follows an untimed commit of the same arm
    /// with no gap.
    pub quiet: Duration,
    /// What it commits.
    pub arm: QuietArm,
    /// What keeps the GPU busy during the quiet, or `None`.
    pub keep: Option<QuietKeep>,
    /// MiB of render targets, allocated and cleared on the take's own queue before the quiet
    /// and referenced by nothing the take commits, released after it settled.
    pub ballast_mib: u32,
}

impl QuietTake {
    /// A take of `arm` after `quiet_ms`, with no keep and no ballast.
    #[must_use]
    pub const fn bare(arm: QuietArm, quiet_ms: u64) -> Self {
        Self {
            quiet: Duration::from_millis(quiet_ms),
            arm,
            keep: None,
            ballast_mib: 0,
        }
    }

    /// The keep's name, `none` for none.
    #[must_use]
    pub fn keep_name(&self) -> &'static str {
        self.keep.map_or("none", QuietKeep::name)
    }
}

/// Where a take is, as its host is told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuietMark {
    /// The take's ballast is cleared and its keeper child started, if it has them; its quiet's
    /// sleep (and in-process keeps) come next. The quiet itself began at
    /// [`QuietCommit::quiet_from`], at or before this mark.
    QuietStart,
    /// The quiet just ended; nothing of the take is committed yet (a back-to-back take's
    /// untimed predecessor comes after this mark).
    QuietEnd,
    /// The take's first encode call is next.
    Submit,
    /// Its last commit returned. Not told for [`QuietArm::P`], whose commits are inside the
    /// renderer's present call.
    Committed,
    /// Its work completed and the waiting thread woke.
    Completed,
    /// [`SETTLE`] after [`Self::Completed`]: late accounting on Metal's threads has had time
    /// to land.
    Settled,
}

/// How long after a take completed its host is told [`QuietMark::Settled`], and how long the
/// probe waits after releasing a take's ballast before the next quiet starts.
pub const SETTLE: Duration = Duration::from_millis(50);

/// What the probe asks of its caller.
pub trait QuietHost {
    /// Take `take` (its index in the plan) reached `mark`.
    fn mark(&mut self, take: usize, mark: QuietMark);
    /// Take `take` is done and measured `commit`: its keeper child has exited and its ballast
    /// was released [`SETTLE`] ago. The next take's ballast and quiet follow.
    fn took(&mut self, take: usize, commit: &QuietCommit);
    /// Start a keeper child that commits a 1x1 clear every `period` on its own device.
    ///
    /// # Errors
    /// The child could not start.
    fn keeper_start(&mut self, period: Duration) -> Result<(), String>;
    /// End the keeper child and say whether it exited cleanly.
    ///
    /// # Errors
    /// The child could not be ended or reaped.
    fn keeper_stop(&mut self) -> Result<bool, String>;
}

/// What the probe needs besides the plan.
#[derive(Clone, Copy, Debug)]
pub struct QuietSetup {
    /// The hand-built arms' Bgra8 target, `(width, height)`.
    pub size: (usize, usize),
    /// [`QuietArm::P`]'s font size in device px.
    pub px: f32,
}

/// What one take measured, each instant and span on the CPU's monotonic clock.
#[derive(Clone, Copy, Debug)]
pub struct QuietCommit {
    /// The take's plan.
    pub take: QuietTake,
    /// When its quiet began ([`QuietTake::quiet`] says from what).
    pub quiet_from: Instant,
    /// When its first encode call began.
    pub began: Instant,
    /// From this process's last completed work of its own (keeps aside; a ballast's clear, or
    /// the previous take's keeper child's exit or ballast release where that came later) to the
    /// take's first encode call.
    pub idle: Duration,
    /// From the last completion on the take's own queue (whatever this probe committed there:
    /// setup, a same-queue keep, a ballast's clear, a predecessor, an earlier take) to its
    /// first encode call; `None` where nothing completed there yet.
    pub queue_idle: Option<Duration>,
    /// Encode through the last commit (P: the renderer's encode span, parks excluded).
    pub submit: Duration,
    /// From the last commit to the waiting thread's wake (P: its present's work span less the
    /// encode).
    pub complete: Duration,
    /// `GPUEndTime - GPUStartTime` of the work's command buffer; `None` for P.
    pub gpu: Option<Duration>,
    /// In-process keep commits made during the quiet.
    pub keeps: u32,
    /// From the last in-process keep's completion to the first encode call; `None` with no
    /// in-process keep made.
    pub last_keep_to_submit: Option<Duration>,
    /// Whether the keeper child exited cleanly; `None` with no child.
    pub keep_ok: Option<bool>,
}

/// [`QuietArm::P`]'s grid, rows by columns.
const P_GRID: (u16, u16) = (32, 110);
/// [`QuietArm::P`]'s card, px: the stats deck's 560x360.
const CARD: (u32, u32) = (560, 360);
/// The card rows each take changes: one row band.
const CARD_BAND: (u32, u32) = (48, 66);
/// Each ballast texture: 4096x4096 Bgra8, 64 MiB.
const BALLAST_SIDE: usize = 4096;

/// The queues the probe commits on, each with its last completion kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Queue {
    /// The probe's own session: the hand-built arms, their same-queue keep and ballast.
    Probe,
    /// The renderer's: P, its same-queue keep and ballast.
    Renderer,
    /// `compute`'s device queue: the compute arm and keep.
    Compute,
    /// The second session of [`QuietKeep::OtherQueueDot`].
    Other,
}

impl Queue {
    /// The queue `arm` commits on.
    const fn of(arm: QuietArm) -> Self {
        match arm {
            QuietArm::P => Self::Renderer,
            QuietArm::Compute => Self::Compute,
            QuietArm::Clear
            | QuietArm::Dot
            | QuietArm::TwoCb
            | QuietArm::EmptyCb
            | QuietArm::Blit => Self::Probe,
        }
    }

    /// The queue `keep` commits on during a take of `arm`; `None` for another process's.
    const fn of_keep(keep: QuietKeep, arm: QuietArm) -> Option<Self> {
        match keep {
            QuietKeep::SameQueueDot(_) => Some(if matches!(arm, QuietArm::P) {
                Self::Renderer
            } else {
                Self::Probe
            }),
            QuietKeep::OtherQueueDot(_) => Some(Self::Other),
            QuietKeep::OtherQueueCompute(_) => Some(Self::Compute),
            QuietKeep::OtherProcessDot(_) => None,
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// Run `takes` in order, telling `host` at each mark. Setup runs every arm the plan names once,
/// untimed, so no take pays a queue's, a texture's or a pipeline's first use.
/// # Errors
/// No Metal device, an allocation or present that fails or is refused, a pass that does not
/// complete, a keep or arm this build lacks, or a keeper child that does not start or end.
#[doc(hidden)]
pub fn quiet_commit_probe(
    setup: QuietSetup,
    takes: &[QuietTake],
    host: &mut dyn QuietHost,
) -> Result<Vec<QuietCommit>, String> {
    let _pool = AutoreleasePool::new();
    let mut probe = Probe::open(setup, takes)?;
    let mut last_done = Instant::now();
    let mut out = Vec::with_capacity(takes.len());
    for (i, &take) in takes.iter().enumerate() {
        let _pool = AutoreleasePool::new();
        let ballast = if take.ballast_mib > 0 {
            let held = probe.ballast(take)?;
            last_done = Instant::now();
            Some(held)
        } else {
            None
        };
        let child = match take.keep {
            Some(QuietKeep::OtherProcessDot(period)) => {
                host.keeper_start(period)?;
                true
            }
            _ => false,
        };
        let quiet_from = last_done;
        let quiet_end = quiet_from + take.quiet;
        host.mark(i, QuietMark::QuietStart);
        let mut keeps = 0;
        let mut last_keep = None;
        if let Some(keep) = take
            .keep
            .filter(|k| !matches!(k, QuietKeep::OtherProcessDot(_)))
        {
            let mut next = Instant::now();
            while next < quiet_end {
                sleep_until(next);
                last_keep = Some(probe.keep_once(keep, take.arm)?);
                keeps += 1;
                next += keep.period();
            }
        }
        sleep_until(quiet_end);
        host.mark(i, QuietMark::QuietEnd);
        let idle_from = if take.quiet.is_zero() {
            probe.prepare(take.arm)?;
            probe.run(take.arm, &mut || {})?.done
        } else {
            last_done
        };
        probe.prepare(take.arm)?;
        host.mark(i, QuietMark::Submit);
        let queue_done = probe.last_done(Queue::of(take.arm));
        let began = Instant::now();
        let timing = probe.run(take.arm, &mut || host.mark(i, QuietMark::Committed))?;
        host.mark(i, QuietMark::Completed);
        last_done = timing.done;
        std::thread::sleep(SETTLE);
        host.mark(i, QuietMark::Settled);
        let keep_ok = if child {
            let ok = host.keeper_stop()?;
            // The next take's quiet is the GPU's: it starts once the keeper's last clear and
            // its device's teardown are over, not at this take's own completion.
            last_done = Instant::now();
            Some(ok)
        } else {
            None
        };
        if let Some(held) = ballast {
            // Released before the next quiet starts, and given the settle to land, so the next
            // take's quiet holds no unmapping of this one's ballast that the settle saw.
            drop(held);
            std::thread::sleep(SETTLE);
            last_done = Instant::now();
        }
        let commit = QuietCommit {
            take,
            quiet_from,
            began,
            idle: began.saturating_duration_since(idle_from),
            queue_idle: queue_done.map(|d| began.saturating_duration_since(d)),
            submit: timing.submit,
            complete: timing.complete,
            gpu: timing.gpu,
            keeps,
            last_keep_to_submit: last_keep.map(|k| began.saturating_duration_since(k)),
            keep_ok,
        };
        host.took(i, &commit);
        out.push(commit);
    }
    Ok(out)
}

/// The keeper child's body (`QUIET_COMMIT_ROLE=keeper`): a 1x1 clear every `period` on its own
/// device, session and loss latch, each waited on, until `stop` answers true; `done` runs once
/// each clear completed. Returns how many it committed.
/// # Errors
/// No Metal device, an allocation that fails, or a clear that does not complete.
#[doc(hidden)]
pub fn quiet_keeper(
    period: Duration,
    stop: &dyn Fn() -> bool,
    done: &mut dyn FnMut(),
) -> Result<u64, String> {
    let _pool = AutoreleasePool::new();
    let dev = Device::preferred_listed().ok_or_else(|| "no listed Metal device".to_owned())?;
    let lane = Lane::open(&dev)?;
    let mut n = 0u64;
    let mut next = Instant::now();
    while !stop() {
        let _pool = AutoreleasePool::new();
        completed(&clear(&lane.session, &lane.dot)?)?;
        done();
        n += 1;
        next = (next + period).max(Instant::now());
        sleep_until(next);
    }
    Ok(n)
}

/// One take's spans.
struct Timing {
    submit: Duration,
    complete: Duration,
    gpu: Option<Duration>,
    done: Instant,
}

/// A queue of this process's own: its session, its mint (one loss latch for both) and a 1x1
/// render target.
struct Lane {
    session: EncodeSession,
    mint: MetalResourceDevice,
    dot: SealedTexture,
}

impl Lane {
    fn open(dev: &Device) -> Result<Self, String> {
        let latch = Arc::new(LossLatch::new());
        let session = EncodeSession::new(dev, Arc::clone(&latch))?;
        let mint = MetalResourceDevice::new(dev, latch);
        let dot = mint.texture_2d(PixelFormat::Bgra8Unorm, 1, 1, TEXTURE_USAGE_RENDER_TARGET)?;
        Ok(Self { session, mint, dot })
    }
}

/// Everything the plan's takes draw on.
struct Probe {
    /// The hand-built arms' queue, with its whole-size target and the blit's source.
    lane: Lane,
    frame: SealedTexture,
    src: SealedTexture,
    size: (usize, usize),
    /// A second queue for [`QuietKeep::OtherQueueDot`], opened if the plan names it.
    other: Option<Lane>,
    /// Production's present, opened if the plan names [`QuietArm::P`].
    production: Option<Production>,
    /// `compute`'s job path, opened if the plan names it.
    #[cfg(feature = "compute")]
    compute: Option<compute_lane::ComputeLane>,
    /// Each [`Queue`]'s last completion, by [`Queue::index`].
    last: [Option<Instant>; 4],
}

impl Probe {
    fn open(setup: QuietSetup, takes: &[QuietTake]) -> Result<Self, String> {
        let dev = Device::preferred_listed().ok_or_else(|| "no listed Metal device".to_owned())?;
        let lane = Lane::open(&dev)?;
        let (w, h) = setup.size;
        let frame =
            lane.mint
                .texture_2d(PixelFormat::Bgra8Unorm, w, h, TEXTURE_USAGE_RENDER_TARGET)?;
        let src =
            lane.mint
                .texture_2d(PixelFormat::Bgra8Unorm, w, h, TEXTURE_USAGE_RENDER_TARGET)?;
        let names = |f: &dyn Fn(&QuietTake) -> bool| takes.iter().any(f);
        let other = names(&|t| matches!(t.keep, Some(QuietKeep::OtherQueueDot(_))))
            .then(|| Lane::open(&dev))
            .transpose()?;
        let wants_compute = names(&|t| {
            t.arm == QuietArm::Compute || matches!(t.keep, Some(QuietKeep::OtherQueueCompute(_)))
        });
        #[cfg(not(feature = "compute"))]
        if wants_compute {
            return Err(
                "the plan names a compute arm or keep: build with --features compute".to_owned(),
            );
        }
        let production = names(&|t| t.arm == QuietArm::P)
            .then(|| Production::open(setup.px))
            .transpose()?;
        let mut probe = Self {
            lane,
            frame,
            src,
            size: setup.size,
            other,
            production,
            #[cfg(feature = "compute")]
            compute: wants_compute
                .then(compute_lane::ComputeLane::open)
                .transpose()?,
            last: [None; 4],
        };
        for target in [&probe.src, &probe.frame, &probe.lane.dot] {
            completed(&clear(&probe.lane.session, target)?)?;
        }
        probe.done_on(Queue::Probe, Instant::now());
        if let Some(other) = &probe.other {
            completed(&clear(&other.session, &other.dot)?)?;
            probe.done_on(Queue::Other, Instant::now());
        }
        if probe.production.is_some() {
            // `Production::open` presented once and cleared its keep target there.
            probe.done_on(Queue::Renderer, Instant::now());
        }
        for arm in QuietArm::ALL {
            if takes.iter().any(|t| t.arm == arm) || (arm == QuietArm::Compute && wants_compute) {
                probe.prepare(arm)?;
                probe.run(arm, &mut || {})?;
            }
        }
        Ok(probe)
    }

    /// Note that work on `queue` completed at `at`.
    fn done_on(&mut self, queue: Queue, at: Instant) {
        self.last[queue.index()] = Some(at);
    }

    /// `queue`'s last completion.
    fn last_done(&self, queue: Queue) -> Option<Instant> {
        self.last[queue.index()]
    }

    /// Ready `arm`'s next take outside its timed span (the compute job's batch load).
    fn prepare(&mut self, arm: QuietArm) -> Result<(), String> {
        match arm {
            #[cfg(feature = "compute")]
            QuietArm::Compute => self.compute_lane()?.load(),
            _ => Ok(()),
        }
    }

    /// One commit of `arm`, waited on; `committed` runs once its last commit returned.
    fn run(&mut self, arm: QuietArm, committed: &mut dyn FnMut()) -> Result<Timing, String> {
        let timing = self.run_on(arm, committed)?;
        self.done_on(Queue::of(arm), timing.done);
        Ok(timing)
    }

    fn run_on(&mut self, arm: QuietArm, committed: &mut dyn FnMut()) -> Result<Timing, String> {
        let session = &self.lane.session;
        let began = Instant::now();
        let subs = match arm {
            QuietArm::P => {
                return self
                    .production
                    .as_mut()
                    .ok_or_else(|| "P was not opened".to_owned())?
                    .present();
            }
            QuietArm::Compute => return self.run_compute(committed),
            QuietArm::Clear | QuietArm::Dot => {
                let target = if arm == QuietArm::Clear {
                    &self.frame
                } else {
                    &self.lane.dot
                };
                let mut cb = session.begin()?;
                drop(cb.render_pass(&pass(target))?);
                vec![cb.commit()]
            }
            QuietArm::TwoCb => {
                let mut cb = session.begin()?;
                drop(cb.render_pass(&pass(&self.frame))?);
                let first = cb.commit();
                let second = session.begin()?.commit();
                vec![first, second]
            }
            QuietArm::EmptyCb => {
                vec![session.begin()?.commit()]
            }
            QuietArm::Blit => {
                let mut cb = session.begin()?;
                let (w, h) = self.size;
                cb.copy_texture_sub_rect(&self.src, (0, 0), &self.frame, (0, 0), w, h)?;
                vec![cb.commit()]
            }
        };
        let after_commit = Instant::now();
        committed();
        for sub in &subs {
            completed(sub)?;
        }
        let done = Instant::now();
        Ok(Timing {
            submit: after_commit.saturating_duration_since(began),
            complete: done.saturating_duration_since(after_commit),
            gpu: subs.first().map(Submitted::gpu_span),
            done,
        })
    }

    #[cfg(feature = "compute")]
    fn compute_lane(&mut self) -> Result<&mut compute_lane::ComputeLane, String> {
        self.compute
            .as_mut()
            .ok_or_else(|| "the compute lane was not opened".to_owned())
    }

    #[cfg(feature = "compute")]
    fn run_compute(&mut self, committed: &mut dyn FnMut()) -> Result<Timing, String> {
        self.compute_lane()?.run(committed)
    }

    #[cfg(not(feature = "compute"))]
    fn run_compute(&mut self, _committed: &mut dyn FnMut()) -> Result<Timing, String> {
        Err("the compute arm needs --features compute".to_owned())
    }

    /// One keep commit, waited on: when it completed.
    fn keep_once(&mut self, keep: QuietKeep, arm: QuietArm) -> Result<Instant, String> {
        self.keep_on(keep, arm)?;
        let done = Instant::now();
        if let Some(queue) = Queue::of_keep(keep, arm) {
            self.done_on(queue, done);
        }
        Ok(done)
    }

    fn keep_on(&mut self, keep: QuietKeep, arm: QuietArm) -> Result<(), String> {
        match keep {
            QuietKeep::SameQueueDot(_) if arm == QuietArm::P => self
                .production
                .as_mut()
                .ok_or_else(|| "P was not opened".to_owned())?
                .keep_dot(),
            QuietKeep::SameQueueDot(_) => completed(&clear(&self.lane.session, &self.lane.dot)?),
            QuietKeep::OtherQueueDot(_) => {
                let other = self
                    .other
                    .as_ref()
                    .ok_or_else(|| "the second queue was not opened".to_owned())?;
                completed(&clear(&other.session, &other.dot)?)
            }
            QuietKeep::OtherQueueCompute(_) => {
                self.prepare(QuietArm::Compute)?;
                self.run_compute(&mut || {}).map(drop)
            }
            QuietKeep::OtherProcessDot(_) => Err("the keeper child is the host's".to_owned()),
        }
    }

    /// `take`'s ballast: render targets on its own queue's mint, cleared there once and waited
    /// on, then referenced by nothing the take commits.
    fn ballast(&mut self, take: QuietTake) -> Result<Vec<SealedTexture>, String> {
        let count = take.ballast_mib.div_ceil(64) as usize;
        let (session, mint) = if take.arm == QuietArm::P {
            self.production
                .as_mut()
                .ok_or_else(|| "P was not opened".to_owned())?
                .lane()?
        } else {
            (&self.lane.session, &self.lane.mint)
        };
        let held = (0..count)
            .map(|_| {
                mint.texture_2d(
                    PixelFormat::Bgra8Unorm,
                    BALLAST_SIDE,
                    BALLAST_SIDE,
                    TEXTURE_USAGE_RENDER_TARGET,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut cb = session.begin()?;
        for t in &held {
            drop(cb.render_pass(&pass(t))?);
        }
        completed(&cb.commit())?;
        self.done_on(Queue::of(take.arm), Instant::now());
        Ok(held)
    }
}

/// The renderer's overlay-only present: a headless renderer's virtual window showing a 32x110
/// grid under a 560x360 card, presented once in full, then once per take with the card's
/// serial advanced and one row band changed ([`TrayDelta`]), as the stats deck's reading frame
/// presents, but into the virtual window's offscreen target and waited on, with no drawable.
struct Production {
    gpu: GpuRenderer,
    win: WindowGpu,
    crop: PresentCrop,
    destination: (u32, u32),
    card: Vec<u8>,
    at: (u32, u32),
    serial: u64,
    /// The same-queue keep's target, on the renderer's own mint once its first present armed it.
    dot: Option<SealedTexture>,
}

impl Production {
    fn open(px: f32) -> Result<Self, String> {
        let mut gpu = GpuRenderer::new_headless(px, Theme::default())?;
        gpu.set_bloom(false);
        gpu.set_shimmer(false);
        let (rows, cols) = P_GRID;
        let mut terminal = Terminal::new(rows, cols);
        terminal.process(b"\x1b[?25l");
        for line in 0..rows - 1 {
            terminal.process(
                format!("{line:>3}  drwxr-xr-x  14 user  staff   448 Nov 17 21:30 crates\r\n")
                    .as_bytes(),
            );
        }
        let input = terminal.cell_frame(usize::from(rows), usize::from(cols));
        let (fw, fh) = gpu.frame_size(usize::from(rows), usize::from(cols));
        let (fw, fh) = (
            u32::try_from(fw).map_err(|e| e.to_string())?,
            u32::try_from(fh).map_err(|e| e.to_string())?,
        );
        let at = (fw.saturating_sub(CARD.0 + 24), 24);
        let mut p = Self {
            gpu,
            win: WindowGpu::new(),
            crop: PresentCrop {
                source_y: 0,
                height: fh,
            },
            destination: (fw, fh),
            card: [36, 40, 52, 255].repeat((CARD.0 * CARD.1) as usize),
            at,
            serial: 1,
            dot: None,
        };
        let quad = Self::quad(&p.card, p.at, p.serial, None);
        if !p.gpu.present_virtual_cropped_with_effects_transport(
            &mut p.win,
            &input,
            false,
            None,
            Some(quad),
            p.crop,
            0,
            p.destination,
        ) {
            return Err("the full present of P's first frame failed".to_owned());
        }
        let (_, mint) = p
            .gpu
            .metal_probe_lane()
            .ok_or_else(|| "the renderer's Metal arm is not live".to_owned())?;
        p.dot =
            Some(mint.texture_2d(PixelFormat::Bgra8Unorm, 1, 1, TEXTURE_USAGE_RENDER_TARGET)?);
        p.keep_dot()?;
        Ok(p)
    }

    /// The card `card` at `at`, of `serial`, with `delta` since the card before.
    fn quad(card: &[u8], at: (u32, u32), serial: u64, delta: Option<TrayDelta>) -> TrayQuad<'_> {
        TrayQuad {
            rgba: card,
            pw: CARD.0,
            ph: CARD.1,
            dx: at.0,
            dy: at.1,
            scrim: None,
            serial,
            delta,
            above: &[],
        }
    }

    /// The renderer's own queue and mint.
    fn lane(&mut self) -> Result<(&EncodeSession, &MetalResourceDevice), String> {
        self.gpu
            .metal_probe_lane()
            .ok_or_else(|| "the renderer's Metal arm is not live".to_owned())
    }

    /// The next card: serial advanced, its band repainted, presented overlay-only.
    fn present(&mut self) -> Result<Timing, String> {
        self.serial += 1;
        let shade = u8::try_from(self.serial % 200).unwrap_or(0) + 40;
        let stride = CARD.0 as usize * 4;
        let band = CARD_BAND.0 as usize * stride..CARD_BAND.1 as usize * stride;
        let (pixels, _) = self.card[band].as_chunks_mut::<4>();
        pixels.fill([shade, 90, 200 - shade / 2, 255]);
        let delta = TrayDelta {
            since: self.serial - 1,
            rows: CARD_BAND,
        };
        let quad = Self::quad(&self.card, self.at, self.serial, Some(delta));
        let shown = self.gpu.present_overlay_virtual(
            &mut self.win,
            false,
            None,
            Some(quad),
            Some(self.crop),
            self.destination,
        );
        let done = Instant::now();
        match shown {
            Ok(true) => {}
            Ok(false) => return Err("P's overlay-only present failed".to_owned()),
            Err(refusal) => {
                return Err(format!("P's overlay-only present was refused: {refusal:?}"));
            }
        }
        let encode = Duration::from_nanos(self.win.last_present_encode_ns());
        let work = Duration::from_nanos(self.win.last_present_work_ns());
        Ok(Timing {
            submit: encode,
            complete: work.saturating_sub(encode),
            gpu: None,
            done,
        })
    }

    /// One 1x1 clear on the renderer's own queue, waited on.
    fn keep_dot(&mut self) -> Result<(), String> {
        let dot = self
            .dot
            .as_ref()
            .ok_or_else(|| "P's keep target was not minted".to_owned())?;
        let (session, _) = self
            .gpu
            .metal_probe_lane()
            .ok_or_else(|| "the renderer's Metal arm is not live".to_owned())?;
        completed(&clear(session, dot)?)
    }
}

/// `compute`'s job path: one tone-int job of one row, loaded before its take and recycled
/// after, on the compute device's own queue.
#[cfg(feature = "compute")]
mod compute_lane {
    use std::sync::Arc;

    use aterm_effects::int_bag_mlp::Batch;
    use aterm_effects::tone_int;
    use aterm_time::{Duration, Instant};

    use super::Timing;
    use crate::compute::{ComputeDevice, Job, Params, Pipeline, Prepared};

    pub(super) struct ComputeLane {
        dev: ComputeDevice,
        params: Arc<Params>,
        pipeline: Pipeline,
        batch: Batch,
        job: Option<Job<Prepared>>,
        loaded: bool,
    }

    impl ComputeLane {
        pub(super) fn open() -> Result<Self, String> {
            let dev = ComputeDevice::open().map_err(|e| format!("compute device: {e:?}"))?;
            let blob = tone_int::v1().map_err(|e| format!("tone-int v1: {e:?}"))?;
            let params = Params::new(&dev, blob).map_err(|e| format!("params: {e:?}"))?;
            let pipeline =
                Pipeline::build(&dev, &params).map_err(|e| format!("pipeline: {e:?}"))?;
            let batch = tone_int::pack(&params.view(), &[tone_int::B1_TEXT])
                .map_err(|e| format!("batch: {e:?}"))?;
            let job = Some(Job::new(&dev, &params));
            Ok(Self {
                dev,
                params,
                pipeline,
                batch,
                job,
                loaded: false,
            })
        }

        /// Load the batch into the job's shared buffers.
        pub(super) fn load(&mut self) -> Result<(), String> {
            let job = match self.job.as_mut() {
                Some(job) => job,
                None => self.job.insert(Job::new(&self.dev, &self.params)),
            };
            job.load(&self.batch).map_err(|e| format!("load: {e:?}"))?;
            self.loaded = true;
            Ok(())
        }

        /// Submit the loaded job and wait for it.
        pub(super) fn run(&mut self, committed: &mut dyn FnMut()) -> Result<Timing, String> {
            if !std::mem::take(&mut self.loaded) {
                return Err("the compute job was not loaded".to_owned());
            }
            let job = self.job.take().ok_or_else(|| "no compute job".to_owned())?;
            let began = Instant::now();
            let flight = match job.submit(&self.pipeline) {
                Ok(flight) => flight,
                Err((job, refusal)) => {
                    self.job = Some(job);
                    return Err(format!("submit: {refusal:?}"));
                }
            };
            let after_commit = Instant::now();
            committed();
            let done_job = flight.wait().map_err(|e| format!("compute job: {e:?}"))?;
            let done = Instant::now();
            let gpu = done_job
                .times()
                .map(|t| Duration::from_secs_f64((t.gpu_end - t.gpu_start).max(0.0)));
            self.job = Some(done_job.recycle());
            Ok(Timing {
                submit: after_commit.saturating_duration_since(began),
                complete: done.saturating_duration_since(after_commit),
                gpu,
                done,
            })
        }
    }
}

/// Sleep until `at`, at once where it has passed.
fn sleep_until(at: Instant) {
    if let Some(left) = at.checked_duration_since(Instant::now()) {
        std::thread::sleep(left);
    }
}

/// A whole-target clear pass that stores what it cleared.
fn pass(target: &SealedTexture) -> RenderPassDesc<'_> {
    RenderPassDesc {
        target,
        load: LoadAction::Clear(ClearColor {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        }),
        store: StoreAction::Store,
        viewport: None,
        scissor: None,
    }
}

/// One command buffer holding [`pass`] on `target`, committed.
fn clear(session: &EncodeSession, target: &SealedTexture) -> Result<Submitted, String> {
    let mut cb = session.begin()?;
    drop(cb.render_pass(&pass(target))?);
    Ok(cb.commit())
}

/// Wait for `sub` and require it completed.
fn completed(sub: &Submitted) -> Result<(), String> {
    match sub.wait_outcome() {
        CbOutcome::Completed => Ok(()),
        other => Err(format!(
            "a probe command buffer did not complete: {other:?}"
        )),
    }
}
