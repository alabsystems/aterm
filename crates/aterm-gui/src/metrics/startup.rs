// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process startup ledger: write-once boot milestones and derived startup metrics.

use super::wire::{self, FieldSink};
use crate::label_enum::label_enum;
use aterm_uds::process::{BootClock, ProcessStart};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Process-wide startup ledger written during boot and read by `metrics` summaries.
pub(crate) static STARTUP: Startup = Startup::new();

/// Write-once boot timestamps, first-present sample, and derived startup facts.
pub(crate) struct Startup {
    /// GUI entry ([`mark_process_start`]), and the anchor of the process clock
    /// (`metrics::now_ns`): a clock read before the entry is stamped anchors it
    /// at that read instead ([`Self::clock_anchor`]).
    gui_entry: OnceLock<Instant>,
    /// Rust `main` ([`mark_rust_main_start`]). The thin dev GUI binary never
    /// sets it.
    rust_main: OnceLock<Instant>,
    /// Rust `main` on the kernel's boot clock (mach ticks on Apple), the clock
    /// `ri_proc_start_abstime` records a birth on: it never steps, so the two
    /// subtract to the pre-main time whatever NTP did between them.
    rust_main_boot: OnceLock<Option<BootClock>>,
    /// Rust `main` on the wall clock, the fallback where `rust_main_boot` is
    /// `None`: the kernel's other birth record (`pbi_start`) is on that clock.
    rust_main_wall: OnceLock<SystemTime>,
    /// Ready for winit ([`mark_gui_ready_for_winit`]).
    gui_ready: OnceLock<Instant>,
    /// Winit's first `resumed` ([`mark_first_winit_resumed`]).
    winit_resumed: OnceLock<Instant>,
    /// The first attached window surface ([`mark_initial_surface_ready`]).
    surface_ready: OnceLock<Instant>,
    /// That attach's internal boundaries ([`record_initial_attach_milestones`]).
    attach: OnceLock<StartupAttachMilestones>,
    /// L1 (early reveal): the instant the FIRST OS window actually became
    /// visible — a `set_visible(true)` on the first window's reveal path, or
    /// (for a window created visible) the moment right after its creation.
    first_visible: OnceLock<Instant>,
    /// The backend-build worker's spawn, finish and legs (the BACKEND-BUILD
    /// WORKER section below).
    worker_spawn: OnceLock<Instant>,
    worker_done: OnceLock<Instant>,
    worker_legs: OnceLock<StartupWorkerLegs>,
    /// When the claim was granted. `None` on every launch that did not claim.
    claim_at: OnceLock<Instant>,
    /// When this successor dialled the rendezvous (before the grant hold).
    claim_dialled_at: OnceLock<Instant>,
    /// How long this successor spent warming BEFORE its dial (the design's P2):
    /// recorded beside the claim; zero on a launch whose warm prologue ran after
    /// its intake.
    claim_warm_pre_dial: OnceLock<Duration>,
    /// Elapsed parent park→sample time, sampled before encoding and sending
    /// the grant.
    claim_parked_before: OnceLock<Duration>,
    /// The boot's own boundaries ([`stamp_claim`]).
    claim_stamps: [OnceLock<Instant>; ClaimStamp::COUNT],
    /// One coherent, immutable startup fact. `OnceLock` publishes both clock
    /// projections together, so a racing control read cannot pair a populated
    /// compatibility field with a missing router field.
    present: OnceLock<StartupPresentSample>,
    /// The facts kept once every input exists ([`Self::facts`]).
    birth_to_main: OnceLock<u64>,
    worker: OnceLock<StartupWorkerSample>,
    gpu: OnceLock<StartupGpuSample>,
}

impl Startup {
    pub(crate) const fn new() -> Self {
        Self {
            gui_entry: OnceLock::new(),
            rust_main: OnceLock::new(),
            rust_main_boot: OnceLock::new(),
            rust_main_wall: OnceLock::new(),
            gui_ready: OnceLock::new(),
            winit_resumed: OnceLock::new(),
            surface_ready: OnceLock::new(),
            attach: OnceLock::new(),
            first_visible: OnceLock::new(),
            worker_spawn: OnceLock::new(),
            worker_done: OnceLock::new(),
            worker_legs: OnceLock::new(),
            claim_at: OnceLock::new(),
            claim_dialled_at: OnceLock::new(),
            claim_warm_pre_dial: OnceLock::new(),
            claim_parked_before: OnceLock::new(),
            claim_stamps: [const { OnceLock::new() }; ClaimStamp::COUNT],
            present: OnceLock::new(),
            birth_to_main: OnceLock::new(),
            worker: OnceLock::new(),
            gpu: OnceLock::new(),
        }
    }

    /// Process clock anchor (`process_start`, or `Instant::now()` if unset).
    pub(crate) fn clock_anchor(&self) -> Instant {
        *self.gui_entry.get_or_init(Instant::now)
    }

    /// Snapshot the startup facts published by a `metrics` summary.
    pub(crate) fn facts(&self) -> StartupFacts {
        self.facts_from(read_own_start, read_gpu_probe)
    }

    /// [`Self::facts`], given the two readings from outside the ledger: the
    /// kernel's start record of this process and the GPU probe.
    fn facts_from(
        &self,
        read_birth: impl FnOnce() -> Option<ProcessStart>,
        read_probe: impl FnOnce() -> StartupGpuSample,
    ) -> StartupFacts {
        let worker = self.worker();
        let gpu = kept(&self.gpu, || {
            self.worker.get()?;
            Some(close_startup_gpu(read_probe(), worker.gpu_build_ns)).filter(|gpu| gpu.valid)
        })
        .unwrap_or_default();
        let (effect_builds, effect_build_ns, effect_built_mask) =
            aterm_gpu::startup_probe::effect_build_ledger();
        let gui_entry = self.gui_entry.get().copied();
        StartupFacts {
            present: self.present.get().copied().unwrap_or_default(),
            worker,
            gpu,
            first_visible_ns: self.first_visible_since(gui_entry),
            rust_main_to_first_visible_ns: self.first_visible_since(self.rust_main.get().copied()),
            gui_prepare_ns: gui_entry
                .zip(self.gui_ready.get().copied())
                .and_then(|(entry, ready)| duration_ns(entry, ready))
                .unwrap_or(0),
            birth_to_main_ns: self.birth_to_main(read_birth),
            effect_builds,
            effect_build_ns,
            effect_built_mask,
        }
    }

    /// The kernel's birth of this process → Rust `main`, in ns: everything
    /// before the first Rust instruction — the launcher's spawn (LaunchServices'
    /// `xpcproxy`, a shell's fork), a Gatekeeper hold on the exec (a quarantine
    /// prompt waits here for the person's click), dyld and page-in.
    fn birth_to_main(&self, read_birth: impl FnOnce() -> Option<ProcessStart>) -> Option<u64> {
        kept(&self.birth_to_main, || {
            let main_wall = *self.rust_main_wall.get()?;
            let main_boot = self.rust_main_boot.get().copied().flatten();
            birth_to_main_ns(read_birth()?, main_boot, main_wall, mach_timebase())
        })
    }

    /// The backend worker's partition, kept once its five stamps exist. Each
    /// is set long before the first present (the join precedes the first frame
    /// by construction); until then the partition is invalid.
    fn worker(&self) -> StartupWorkerSample {
        kept(&self.worker, || {
            let (join_entry, join_exit) = self.attach.get()?.backend_finalize_bounds();
            Some(derive_startup_worker(
                Some(*self.worker_spawn.get()?),
                Some(*self.worker_done.get()?),
                Some(*self.worker_legs.get()?),
                Some(join_entry),
                Some(join_exit),
            ))
        })
        .unwrap_or_default()
    }

    /// Emit pre-winit boot fields (`handoff_claim`, `startup_prepare`, `startup_worker`).
    pub(crate) fn emit_boot(&self, facts: &StartupFacts, s: &mut dyn FieldSink) {
        self.emit_handoff_claim(s);
        self.emit_startup_prepare(facts, s);
    }
}

/// `cell`'s fact, else `derive`'s, kept when it is `Some`. Every input of a
/// startup fact is write-once, so a fact derived once all of them exist can
/// never change: `derive` answers `None` until they do, and the next read
/// derives again.
fn kept<T: Copy>(cell: &OnceLock<T>, derive: impl FnOnce() -> Option<T>) -> Option<T> {
    if let Some(fact) = cell.get() {
        return Some(*fact);
    }
    let fact = derive()?;
    Some(*cell.get_or_init(|| fact))
}

/// The kernel's start record of this process.
fn read_own_start() -> Option<ProcessStart> {
    aterm_uds::process::read_process_start(i32::try_from(std::process::id()).ok()?)
}

/// Anchor the GUI process-lifetime clock at the top of [`crate::main_entry`].
pub(crate) fn mark_process_start() {
    let _ = STARTUP.gui_entry.set(Instant::now());
}

/// Anchor the shipped binary's Rust `main` entry before argv0 routing.
pub(crate) fn mark_rust_main_start() {
    let _ = STARTUP.rust_main.set(Instant::now());
    let _ = STARTUP
        .rust_main_boot
        .set(aterm_uds::process::boot_clock_now());
    let _ = STARTUP.rust_main_wall.set(SystemTime::now());
}

/// The birth → Rust `main` subtraction ([`Startup::facts`]), on given
/// readings: the boot clock's ticks through `timebase` (`numer`, `denom`:
/// ticks × numer / denom = ns) when both `main_boot` and the timebase exist,
/// else the wall clock.
fn birth_to_main_ns(
    start: ProcessStart,
    main_boot: Option<BootClock>,
    main_wall: SystemTime,
    timebase: Option<(u32, u32)>,
) -> Option<u64> {
    if let (Some(main), Some((numer, denom))) = (main_boot, timebase) {
        let ticks = main.0.checked_sub(start.boot.0)?;
        let ns = u128::from(ticks) * u128::from(numer) / u128::from(denom.max(1));
        return u64::try_from(ns).ok();
    }
    let birth = start.wall?;
    let born = UNIX_EPOCH
        .checked_add(Duration::from_secs(birth.seconds))?
        .checked_add(Duration::from_micros(birth.microseconds))?;
    u64::try_from(main_wall.duration_since(born).ok()?.as_nanos()).ok()
}

/// The mach timebase (`numer`, `denom`), which turns the boot clock's ticks
/// into nanoseconds; `None` where there is none (off Apple) or the call fails.
#[cfg(target_vendor = "apple")]
fn mach_timebase() -> Option<(u32, u32)> {
    let mut base = libc::mach_timebase_info { numer: 0, denom: 0 };
    // SAFETY: `base` is a live, writable `mach_timebase_info` record; the call only fills it.
    let rc = unsafe { libc::mach_timebase_info(&raw mut base) };
    (rc == 0 && base.denom != 0).then_some((base.numer, base.denom))
}

/// See the Apple twin: no mach timebase here.
#[cfg(not(target_vendor = "apple"))]
fn mach_timebase() -> Option<(u32, u32)> {
    None
}

/// Mark the end of synchronous GUI preparation immediately before entering winit.
pub(crate) fn mark_gui_ready_for_winit() {
    let _ = STARTUP.gui_ready.set(Instant::now());
}

/// Mark the start of winit's first `resumed` callback.
pub(crate) fn mark_first_winit_resumed() {
    let _ = STARTUP.winit_resumed.set(Instant::now());
}

/// Mark the first successfully attached OS window surface.
pub(crate) fn mark_initial_surface_ready() {
    let _ = STARTUP.surface_ready.set(Instant::now());
}

/// Record the first window-visibility milestone.
pub(crate) fn mark_first_window_visible() {
    let _ = STARTUP.first_visible.set(Instant::now());
}

/// Wire schema for the exclusive Rust-main → first-present phase partition.
const STARTUP_PHASE_SCHEMA: u64 = 1;

/// Wire schema for the exclusive first-resume → initial-surface-ready attach
/// partition nested inside [`STARTUP_PHASE_SCHEMA`]'s surface-attach phase.
const STARTUP_ATTACH_SCHEMA: u64 = 1;

/// Ordered internal timestamps for the initial window surface attach.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StartupAttachMilestones {
    points: [Instant; 7],
}

impl StartupAttachMilestones {
    pub(crate) const fn new(points: [Instant; 7]) -> Self {
        Self { points }
    }

    /// The two stamps that bracket `backend_finalize_ns` — the join entry and
    /// the join exit.
    fn backend_finalize_bounds(self) -> (Instant, Instant) {
        let [
            _,
            _,
            _,
            before_backend_finalize,
            after_backend_finalize,
            _,
            _,
        ] = self.points;
        (before_backend_finalize, after_backend_finalize)
    }
}

/// Record the internal timeline for the first attached window surface.
pub(crate) fn record_initial_attach_milestones(milestones: StartupAttachMilestones) {
    let _ = STARTUP.attach.set(milestones);
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct StartupPresentTiming {
    frame_started: Instant,
    pre_present: Instant,
    surface_return: Instant,
}

impl StartupPresentTiming {
    pub(crate) const fn new(
        frame_started: Instant,
        pre_present: Instant,
        surface_return: Instant,
    ) -> Self {
        Self {
            frame_started,
            pre_present,
            surface_return,
        }
    }

    pub(crate) const fn collapsed(at: Instant) -> Self {
        Self::new(at, at, at)
    }

    pub(crate) const fn frame_started(self) -> Instant {
        self.frame_started
    }

    pub(crate) fn finish(frame_started: Instant, pre_present: Option<Instant>) -> Self {
        pre_present.map_or_else(
            || Self::collapsed(frame_started),
            |pre_present| Self::new(frame_started, pre_present, Instant::now()),
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StartupPhaseSample {
    valid: bool,
    router_ns: u64,
    gui_prepare_ns: u64,
    winit_dispatch_ns: u64,
    initial_surface_attach_ns: u64,
    surface_to_successful_redraw_ns: u64,
    successful_compose_ns: u64,
    successful_surface_transaction_ns: u64,
    successful_finalize_ns: u64,
}

impl StartupPhaseSample {
    fn total_ns(self) -> Option<u64> {
        [
            self.router_ns,
            self.gui_prepare_ns,
            self.winit_dispatch_ns,
            self.initial_surface_attach_ns,
            self.surface_to_successful_redraw_ns,
            self.successful_compose_ns,
            self.successful_surface_transaction_ns,
            self.successful_finalize_ns,
        ]
        .into_iter()
        .try_fold(0u64, |total, phase| total.checked_add(phase))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StartupAttachSample {
    valid: bool,
    dispatch_ns: u64,
    prepare_ns: u64,
    window_create_ns: u64,
    window_setup_ns: u64,
    backend_finalize_ns: u64,
    chrome_geometry_ns: u64,
    surface_create_ns: u64,
    finish_ns: u64,
}

impl StartupAttachSample {
    fn total_ns(self) -> Option<u64> {
        [
            self.dispatch_ns,
            self.prepare_ns,
            self.window_create_ns,
            self.window_setup_ns,
            self.backend_finalize_ns,
            self.chrome_geometry_ns,
            self.surface_create_ns,
            self.finish_ns,
        ]
        .into_iter()
        .try_fold(0u64, |total, phase| total.checked_add(phase))
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct StartupMilestones {
    rust_main: Option<Instant>,
    gui_entry: Option<Instant>,
    gui_ready: Option<Instant>,
    winit_resumed: Option<Instant>,
    surface_ready: Option<Instant>,
}

fn duration_ns(start: Instant, end: Instant) -> Option<u64> {
    u64::try_from(end.checked_duration_since(start)?.as_nanos()).ok()
}

impl Startup {
    /// `anchor` → the first-window reveal, or 0 while either stamp is missing.
    fn first_visible_since(&self, anchor: Option<Instant>) -> u64 {
        anchor
            .zip(self.first_visible.get().copied())
            .and_then(|(start, end)| duration_ns(start, end))
            .unwrap_or(0)
    }
}

fn derive_startup_phases(
    milestones: StartupMilestones,
    timing: StartupPresentTiming,
    published_at: Instant,
) -> StartupPhaseSample {
    let Some(rust_main) = milestones.rust_main else {
        return StartupPhaseSample::default();
    };
    let Some(gui_entry) = milestones.gui_entry else {
        return StartupPhaseSample::default();
    };
    let Some(gui_ready) = milestones.gui_ready else {
        return StartupPhaseSample::default();
    };
    let Some(winit_resumed) = milestones.winit_resumed else {
        return StartupPhaseSample::default();
    };
    let Some(surface_ready) = milestones.surface_ready else {
        return StartupPhaseSample::default();
    };
    let Some(router_ns) = duration_ns(rust_main, gui_entry) else {
        return StartupPhaseSample::default();
    };
    let Some(gui_prepare_ns) = duration_ns(gui_entry, gui_ready) else {
        return StartupPhaseSample::default();
    };
    let Some(winit_dispatch_ns) = duration_ns(gui_ready, winit_resumed) else {
        return StartupPhaseSample::default();
    };
    let Some(initial_surface_attach_ns) = duration_ns(winit_resumed, surface_ready) else {
        return StartupPhaseSample::default();
    };
    let Some(surface_to_successful_redraw_ns) = duration_ns(surface_ready, timing.frame_started)
    else {
        return StartupPhaseSample::default();
    };
    let Some(successful_compose_ns) = duration_ns(timing.frame_started, timing.pre_present) else {
        return StartupPhaseSample::default();
    };
    let Some(successful_surface_transaction_ns) =
        duration_ns(timing.pre_present, timing.surface_return)
    else {
        return StartupPhaseSample::default();
    };
    let Some(successful_finalize_ns) = duration_ns(timing.surface_return, published_at) else {
        return StartupPhaseSample::default();
    };
    let sample = StartupPhaseSample {
        valid: true,
        router_ns,
        gui_prepare_ns,
        winit_dispatch_ns,
        initial_surface_attach_ns,
        surface_to_successful_redraw_ns,
        successful_compose_ns,
        successful_surface_transaction_ns,
        successful_finalize_ns,
    };
    let rust_main_total = duration_ns(rust_main, published_at);
    let gui_total = duration_ns(gui_entry, published_at);
    let partition_total = sample.total_ns();
    if partition_total != rust_main_total
        || partition_total.and_then(|total| total.checked_sub(router_ns)) != gui_total
    {
        return StartupPhaseSample::default();
    }
    sample
}

fn derive_startup_attach(
    winit_resumed: Option<Instant>,
    milestones: Option<StartupAttachMilestones>,
    surface_ready: Option<Instant>,
) -> StartupAttachSample {
    let Some(winit_resumed) = winit_resumed else {
        return StartupAttachSample::default();
    };
    let Some(milestones) = milestones else {
        return StartupAttachSample::default();
    };
    let Some(surface_ready) = surface_ready else {
        return StartupAttachSample::default();
    };
    let [
        attach_entry,
        before_window_create,
        after_window_create,
        before_backend_finalize,
        after_backend_finalize,
        before_surface_create,
        after_surface_create,
    ] = milestones.points;
    let Some(dispatch_ns) = duration_ns(winit_resumed, attach_entry) else {
        return StartupAttachSample::default();
    };
    let Some(prepare_ns) = duration_ns(attach_entry, before_window_create) else {
        return StartupAttachSample::default();
    };
    let Some(window_create_ns) = duration_ns(before_window_create, after_window_create) else {
        return StartupAttachSample::default();
    };
    let Some(window_setup_ns) = duration_ns(after_window_create, before_backend_finalize) else {
        return StartupAttachSample::default();
    };
    let Some(backend_finalize_ns) = duration_ns(before_backend_finalize, after_backend_finalize)
    else {
        return StartupAttachSample::default();
    };
    let Some(chrome_geometry_ns) = duration_ns(after_backend_finalize, before_surface_create)
    else {
        return StartupAttachSample::default();
    };
    let Some(surface_create_ns) = duration_ns(before_surface_create, after_surface_create) else {
        return StartupAttachSample::default();
    };
    let Some(finish_ns) = duration_ns(after_surface_create, surface_ready) else {
        return StartupAttachSample::default();
    };
    let sample = StartupAttachSample {
        valid: true,
        dispatch_ns,
        prepare_ns,
        window_create_ns,
        window_setup_ns,
        backend_finalize_ns,
        chrome_geometry_ns,
        surface_create_ns,
        finish_ns,
    };
    if sample.total_ns() != duration_ns(winit_resumed, surface_ready) {
        return StartupAttachSample::default();
    }
    sample
}

/// Wire schema for the backend-build worker partition — the exclusive
/// drill-down of `startup_attach_backend_finalize_ns`.
const STARTUP_WORKER_SCHEMA: u64 = 1;

/// Wire schema for the renderer-side sub-split of the worker's GPU build,
/// sourced from `aterm_gpu::startup_probe`.
const STARTUP_GPU_SCHEMA: u64 = 1;

/// Background backend-build worker leg durations in nanoseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StartupWorkerLegs {
    /// Worker entry to renderer constructor call.
    pub(crate) prelude_ns: u64,
    /// Renderer construction duration (`GpuRenderer` or CPU fallback).
    pub(crate) gpu_build_ns: u64,
    /// Worker wait for the resolved font configuration.
    pub(crate) font_admit_ns: u64,
    /// Duration of `apply_font_config_to_backend`.
    pub(crate) font_apply_ns: u64,
    /// Duration of `seal_admitted_font_sources`.
    pub(crate) font_seal_ns: u64,
}

/// Mark the instant the backend-build worker thread is spawned.
pub(crate) fn mark_backend_worker_spawn() {
    let _ = STARTUP.worker_spawn.set(Instant::now());
}

/// Nanoseconds elapsed since `started`, saturating.
pub(crate) fn leg_elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Record the background worker's leg timings.
pub(crate) fn record_backend_worker_legs(legs: StartupWorkerLegs) {
    let _ = STARTUP.worker_legs.set(legs);
}

/// Mark the instant the background worker finished building the backend.
pub(crate) fn mark_backend_worker_done() {
    let _ = STARTUP.worker_done.set(Instant::now());
}

label_enum! {
    /// Boot milestones stamped into the handoff claim timeline.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) enum ClaimStamp {
        /// Handoff intake authentication completed.
        Intake => "intake",
        /// Session 0 bound by `spawn_session`.
        Adopt0 => "adopt0",
        /// Winit `EventLoop` constructed.
        EventLoopBuilt => "event_loop",
        /// First window present completed.
        FirstPresent => "first_present",
        /// Deferred session restore started.
        RestoreStart => "restore",
        /// Deferred session restore completed.
        RestoreDrained => "drained",
        /// Adoption proof ready to write to the parent.
        Proof => "proof",
        /// Winit `EventLoop` construction started.
        LoopBuild => "loop_build",
        /// Background config watcher and update checker started.
        Services => "services",
        /// Restore intake and crash-journal writer initialized.
        Journal => "journal",
        /// Control socket planned and spawned.
        Control => "control",
        /// Handoff commit received from parent.
        Commit => "commit",
        /// Config-runtime worker joined.
        ConfigJoined => "config_joined",
        /// `App` state constructed.
        AppBuilt => "app_built",
        /// Warm prologue completed and launch font/grid resolved.
        Warmed => "warmed",
    }
}

impl Startup {
    /// The earliest in-process instant this process stamped: the shipped
    /// binary's Rust `main` ([`mark_rust_main_start`]), else the GUI entry (the
    /// thin dev binary).
    fn main_start(&self) -> Option<Instant> {
        self.rust_main
            .get()
            .or_else(|| self.gui_entry.get())
            .copied()
    }
}

/// Earliest in-process timestamp (`rust_main` or `process_start`).
#[cfg(target_os = "macos")]
#[must_use]
pub(crate) fn process_main_start() -> Option<Instant> {
    STARTUP.main_start()
}

/// Record the handoff rendezvous claim timestamp and pre-claim leg durations.
#[cfg(target_os = "macos")]
pub(crate) fn mark_handoff_claimed(
    dialled_at: Instant,
    claimed_at: Instant,
    warm_pre_dial: Duration,
    parked_before_grant: Option<Duration>,
) {
    let _ = STARTUP.claim_dialled_at.set(dialled_at);
    let _ = STARTUP.claim_warm_pre_dial.set(warm_pre_dial);
    if let Some(parked) = parked_before_grant {
        let _ = STARTUP.claim_parked_before.set(parked);
    }
    let _ = STARTUP.claim_at.set(claimed_at);
}

/// Record a [`ClaimStamp`] boot milestone.
pub(crate) fn stamp_claim(stamp: ClaimStamp) {
    // `get_or_init`: a repeat offer reads no clock.
    let _ = STARTUP.claim_stamps[stamp.index()].get_or_init(Instant::now);
}

/// Everything the claim timeline is derived from, gathered so the derivation is
/// a pure function a test can drive with invented instants.
#[derive(Clone, Copy, Debug)]
struct ClaimTimelineInputs {
    claim: Instant,
    /// [`Startup::main_start`].
    exec: Option<Instant>,
    /// Where the warm prologue started, on a launch that warmed before its
    /// dial (P2): the dial minus its warm stage.
    warm: Option<Instant>,
    dial: Option<Instant>,
    /// A projected park point: this claim minus the parent's sampled park
    /// duration.
    park: Option<Instant>,
    own: [Option<Instant>; ClaimStamp::COUNT],
    worker_spawn: Option<Instant>,
    worker_done: Option<Instant>,
    run_app: Option<Instant>,
    resumed: Option<Instant>,
    attach: Option<StartupAttachMilestones>,
    surface_ready: Option<Instant>,
    visible: Option<Instant>,
}

/// One labelled boundary, in signed microseconds after the claim (the Rust
/// main and the dial precede it, so they read negative).
type ClaimPoint = (&'static str, i64);

fn signed_us(zero: Instant, at: Instant) -> i64 {
    match at.checked_duration_since(zero) {
        Some(after) => i64::try_from(after.as_micros()).unwrap_or(i64::MAX),
        None => {
            i64::try_from(zero.duration_since(at).as_micros()).map_or(i64::MIN, std::ops::Neg::neg)
        }
    }
}

/// The claim timeline in its NOMINAL boot order (the order the code runs them;
/// the values say what really happened).
fn claim_timeline_points(inputs: &ClaimTimelineInputs) -> Vec<ClaimPoint> {
    let own = |stamp: ClaimStamp| (stamp.as_str(), inputs.own[stamp.index()]);
    let attach = inputs.attach.map(|milestones| milestones.points);
    let attach_point = |index: usize| attach.map(|points| points[index]);
    let ordered: [(&'static str, Option<Instant>); 31] = [
        ("main", inputs.exec),
        ("warm", inputs.warm),
        ("dial", inputs.dial),
        ("park", inputs.park),
        own(ClaimStamp::Intake),
        ("worker_spawn", inputs.worker_spawn),
        own(ClaimStamp::Journal),
        own(ClaimStamp::Adopt0),
        own(ClaimStamp::LoopBuild),
        own(ClaimStamp::EventLoopBuilt),
        own(ClaimStamp::Services),
        own(ClaimStamp::Control),
        own(ClaimStamp::ConfigJoined),
        own(ClaimStamp::AppBuilt),
        ("run_app", inputs.run_app),
        ("resumed", inputs.resumed),
        ("attach", attach_point(0)),
        ("window_create", attach_point(1)),
        ("window_created", attach_point(2)),
        ("join", attach_point(3)),
        ("worker_done", inputs.worker_done),
        ("joined", attach_point(4)),
        ("surface_create", attach_point(5)),
        ("surface_created", attach_point(6)),
        ("surface_ready", inputs.surface_ready),
        own(ClaimStamp::FirstPresent),
        ("visible", inputs.visible),
        own(ClaimStamp::RestoreStart),
        own(ClaimStamp::RestoreDrained),
        own(ClaimStamp::Proof),
        own(ClaimStamp::Commit),
    ];
    ordered
        .into_iter()
        .filter_map(|(label, at)| Some((label, signed_us(inputs.claim, at?))))
        .collect()
}

/// The backend worker against the join that waits for it: how long the join
/// blocked, and how far the worker's `done` sat before (positive) or after
/// (negative) the join's entry.
#[cfg_attr(not(unix), allow(dead_code))]
fn claim_worker_vs_join(inputs: &ClaimTimelineInputs) -> Option<(i64, i64)> {
    let (join_entry, join_exit) = inputs.attach?.backend_finalize_bounds();
    let done = inputs.worker_done?;
    Some((
        signed_us(join_entry, join_exit),
        signed_us(done, join_entry),
    ))
}

impl Startup {
    /// The claim timeline's inputs, or `None` on a launch that did not claim.
    fn claim_timeline_inputs(&self) -> Option<ClaimTimelineInputs> {
        let claim = self.claim_at.get().copied()?;
        let mut own = [None; ClaimStamp::COUNT];
        for (slot, stamp) in own.iter_mut().zip(self.claim_stamps.iter()) {
            *slot = stamp.get().copied();
        }
        let dial = self.claim_dialled_at.get().copied();
        let warm = dial
            .zip(self.claim_warm_pre_dial.get().copied())
            .and_then(|(dial, warm)| (!warm.is_zero()).then(|| dial.checked_sub(warm)).flatten());
        Some(ClaimTimelineInputs {
            claim,
            exec: self.main_start(),
            warm,
            dial,
            park: self
                .claim_parked_before
                .get()
                .and_then(|parked| claim.checked_sub(*parked)),
            own,
            worker_spawn: self.worker_spawn.get().copied(),
            worker_done: self.worker_done.get().copied(),
            run_app: self.gui_ready.get().copied(),
            resumed: self.winit_resumed.get().copied(),
            attach: self.attach.get().copied(),
            surface_ready: self.surface_ready.get().copied(),
            visible: self.first_visible.get().copied(),
        })
    }

    /// The warm stage's cost before the dial, as `metrics` says it: `None`
    /// on a launch that did not claim.
    fn claim_warm_pre_dial(&self) -> Option<Duration> {
        self.claim_at.get()?;
        self.claim_warm_pre_dial.get().copied()
    }

    /// The `metrics` summary's claim fields: `handoff_claim_stamps`, the
    /// timeline's `label:ms` pairs (an object in JSON; `none`/`null` on a
    /// launch that claimed no handoff), and `handoff_warm_pre_dial_ms`
    /// (`none`/`null` without one).
    fn emit_handoff_claim(&self, s: &mut dyn FieldSink) {
        match self.claim_timeline_inputs() {
            Some(inputs) => {
                let points: Vec<(&str, wire::Cell<'_>)> = claim_timeline_points(&inputs)
                    .into_iter()
                    .map(|(label, us)| (label, wire::Cell::Decimal(wire::signed_us_ms(us))))
                    .collect();
                s.pairs("handoff_claim_stamps", &points, "");
            }
            None => s.absent("handoff_claim_stamps", "none"),
        }
        s.opt_decimal(
            "handoff_warm_pre_dial_ms",
            self.claim_warm_pre_dial()
                .map(|warm| wire::ms_f64(warm.as_secs_f64() * 1000.0)),
            "none",
        );
    }
}

/// Format signed microseconds as milliseconds with `decimals` fractional digits.
pub(super) fn signed_ms(us: i64, decimals: usize) -> String {
    let sign = if us < 0 { "-" } else { "" };
    let ms = us.unsigned_abs() as f64 / 1000.0;
    format!("{sign}{ms:.decimals$}")
}

/// Format the handoff claim timeline for logging, if a claim was recorded.
#[cfg_attr(not(unix), allow(dead_code))]
#[must_use]
pub(crate) fn claim_timeline_log_text() -> Option<String> {
    let inputs = STARTUP.claim_timeline_inputs()?;
    Some(render_claim_timeline_log(&inputs))
}

#[cfg_attr(not(unix), allow(dead_code))]
fn render_claim_timeline_log(inputs: &ClaimTimelineInputs) -> String {
    let points = claim_timeline_points(inputs)
        .into_iter()
        .map(|(label, us)| {
            let sign = if us < 0 { "" } else { "+" };
            format!("{label} {sign}{}", signed_ms(us, 1))
        })
        .collect::<Vec<_>>()
        .join(", ");
    match claim_worker_vs_join(inputs) {
        Some((waited, slack)) => format!(
            "{points}; the backend join waited {} ms, the worker done {} ms {} it",
            signed_ms(waited, 1),
            signed_ms(slack.saturating_abs(), 1),
            if slack < 0 { "after" } else { "before" },
        ),
        None => points,
    }
}

/// Wire schema for the exclusive split of GUI entry → ready for winit.
const STARTUP_PREPARE_SCHEMA: u64 = 1;

/// The phases in boot order, as their `startup_prepare_<name>_ms` fields:
const STARTUP_PREPARE_PHASES: [&str; PREPARE_PHASES] = [
    "boot",
    "warm",
    "journal",
    "session0",
    "event_loop",
    "services",
    "control",
    "config_join",
    "app",
    "arm",
];

const PREPARE_PHASES: usize = 10;

/// The cuts between GUI entry and ready for winit, in boot order.
const STARTUP_PREPARE_CUTS: [ClaimStamp; PREPARE_PHASES - 1] = [
    ClaimStamp::Intake,
    ClaimStamp::Warmed,
    ClaimStamp::Journal,
    ClaimStamp::Adopt0,
    ClaimStamp::EventLoopBuilt,
    ClaimStamp::Services,
    ClaimStamp::Control,
    ClaimStamp::ConfigJoined,
    ClaimStamp::AppBuilt,
];

/// The cuts of a launch that builds its event loop before session 0 (a
/// free-unix window, whose build opens a display no server has answered yet, or any
/// launch under the `ATERM_DEBUG_LOOP_FIRST` seam: `main_entry`'s
/// `loop_before_session0`).
const LOOP_FIRST_CUTS: [ClaimStamp; PREPARE_PHASES - 1] = [
    ClaimStamp::Intake,
    ClaimStamp::Warmed,
    ClaimStamp::EventLoopBuilt,
    ClaimStamp::Journal,
    ClaimStamp::Adopt0,
    ClaimStamp::Services,
    ClaimStamp::Control,
    ClaimStamp::ConfigJoined,
    ClaimStamp::AppBuilt,
];

/// The split of whichever boot order `stamp` was taken in
/// ([`STARTUP_PREPARE_CUTS`], else [`LOOP_FIRST_CUTS`]), its phases in
/// [`STARTUP_PREPARE_PHASES`]' order.
fn derive_startup_prepare_of(
    entry: Option<Instant>,
    stamp: impl Fn(ClaimStamp) -> Option<Instant>,
    ready: Option<Instant>,
) -> Option<[u64; PREPARE_PHASES]> {
    derive_startup_prepare(entry, STARTUP_PREPARE_CUTS.map(&stamp), ready).or_else(|| {
        derive_startup_prepare(entry, LOOP_FIRST_CUTS.map(&stamp), ready)
            .map(|p| [p[0], p[1], p[3], p[4], p[2], p[5], p[6], p[7], p[8], p[9]])
    })
}

/// The phases between `entry`, the `cuts` and `ready`, in ns, or `None`
/// unless every boundary exists in that order.
fn derive_startup_prepare(
    entry: Option<Instant>,
    cuts: [Option<Instant>; PREPARE_PHASES - 1],
    ready: Option<Instant>,
) -> Option<[u64; PREPARE_PHASES]> {
    let mut bounds = [entry?; PREPARE_PHASES + 1];
    for (bound, cut) in bounds[1..PREPARE_PHASES].iter_mut().zip(cuts) {
        *bound = cut?;
    }
    bounds[PREPARE_PHASES] = ready?;
    let mut phases = [0u64; PREPARE_PHASES];
    for (phase, pair) in phases.iter_mut().zip(bounds.windows(2)) {
        *phase = duration_ns(pair[0], pair[1])?;
    }
    let sum = phases
        .iter()
        .try_fold(0u64, |total, phase| total.checked_add(*phase));
    (sum == duration_ns(bounds[0], bounds[PREPARE_PHASES])).then_some(phases)
}

impl Startup {
    /// The GUI-preparation split, when it belongs to `gui_prepare_ns`.
    fn prepare_sample(&self, gui_prepare_ns: u64) -> Option<[u64; PREPARE_PHASES]> {
        derive_startup_prepare_of(
            self.gui_entry.get().copied(),
            |cut| self.claim_stamps[cut.index()].get().copied(),
            self.gui_ready.get().copied(),
        )
        .filter(|phases| {
            phases.iter().try_fold(0u64, |sum, ns| sum.checked_add(*ns)) == Some(gui_prepare_ns)
        })
    }

    /// The `metrics` summary's fields for everything before winit runs, after
    /// the claim timeline: `process_birth_to_rust_main_ms` (`none`/`null` where
    /// the platform cannot read the birth), `startup_prepare_schema`,
    /// `startup_prepare_valid` and one `startup_prepare_<phase>_ms` per phase,
    /// every phase 0 while the split is invalid (as the other startup
    /// partitions publish).
    fn emit_startup_prepare(&self, facts: &StartupFacts, s: &mut dyn FieldSink) {
        s.opt_decimal(
            "process_birth_to_rust_main_ms",
            facts.birth_to_main_ns.map(wire::ms),
            "none",
        );
        let sample = self.prepare_sample(facts.gui_prepare_ns);
        s.count("startup_prepare_schema", STARTUP_PREPARE_SCHEMA);
        s.flag("startup_prepare_valid", sample.is_some());
        for (name, ns) in STARTUP_PREPARE_PHASES
            .iter()
            .zip(sample.unwrap_or_default())
        {
            s.decimal(&format!("startup_prepare_{name}_ms"), wire::ms(ns));
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StartupWorkerSample {
    valid: bool,
    total_ns: u64,
    overlap_ns: u64,
    after_join_ns: u64,
    post_join_ns: u64,
    prelude_ns: u64,
    gpu_build_ns: u64,
    font_admit_ns: u64,
    font_apply_ns: u64,
    font_seal_ns: u64,
    epilogue_ns: u64,
}

impl StartupWorkerSample {
    /// The measured legs, in worker order. `epilogue_ns` closes them against
    /// `total_ns`, so this sum plus the epilogue IS the worker's wall time.
    fn measured_leg_total_ns(self) -> Option<u64> {
        [
            self.prelude_ns,
            self.gpu_build_ns,
            self.font_admit_ns,
            self.font_apply_ns,
            self.font_seal_ns,
        ]
        .into_iter()
        .try_fold(0u64, |total, leg| total.checked_add(leg))
    }
}

/// Derive the worker partition from the four `Instant`s that bound it plus the
/// worker's own legs.
fn derive_startup_worker(
    spawn: Option<Instant>,
    done: Option<Instant>,
    legs: Option<StartupWorkerLegs>,
    join_entry: Option<Instant>,
    join_exit: Option<Instant>,
) -> StartupWorkerSample {
    let Some(spawn) = spawn else {
        return StartupWorkerSample::default();
    };
    let Some(done) = done else {
        return StartupWorkerSample::default();
    };
    let Some(legs) = legs else {
        return StartupWorkerSample::default();
    };
    let Some(join_entry) = join_entry else {
        return StartupWorkerSample::default();
    };
    let Some(join_exit) = join_exit else {
        return StartupWorkerSample::default();
    };
    let Some(total_ns) = duration_ns(spawn, done) else {
        return StartupWorkerSample::default();
    };
    let split = done.min(join_entry);
    let resume = done.max(join_entry);
    let Some(overlap_ns) = duration_ns(spawn, split) else {
        return StartupWorkerSample::default();
    };
    let after_join_ns = duration_ns(join_entry, done).unwrap_or(0);
    let Some(post_join_ns) = duration_ns(resume, join_exit) else {
        return StartupWorkerSample::default();
    };
    let sample = StartupWorkerSample {
        valid: true,
        total_ns,
        overlap_ns,
        after_join_ns,
        post_join_ns,
        prelude_ns: legs.prelude_ns,
        gpu_build_ns: legs.gpu_build_ns,
        font_admit_ns: legs.font_admit_ns,
        font_apply_ns: legs.font_apply_ns,
        font_seal_ns: legs.font_seal_ns,
        epilogue_ns: 0,
    };
    let Some(measured_ns) = sample.measured_leg_total_ns() else {
        return StartupWorkerSample::default();
    };
    let Some(epilogue_ns) = total_ns.checked_sub(measured_ns) else {
        return StartupWorkerSample::default();
    };
    if overlap_ns.checked_add(after_join_ns) != Some(total_ns)
        || after_join_ns.checked_add(post_join_ns) != duration_ns(join_entry, join_exit)
    {
        return StartupWorkerSample::default();
    }
    StartupWorkerSample {
        epilogue_ns,
        ..sample
    }
}

/// The renderer-side split of the worker's GPU build, read back from
/// `aterm_gpu::startup_probe` — the crate where the work happens and which
/// cannot see this ledger.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StartupGpuSample {
    valid: bool,
    instance_ns: u64,
    adapter_ns: u64,
    device_ns: u64,
    context_tail_ns: u64,
    font_thread_ns: u64,
    font_join_ns: u64,
    pipelines_ns: u64,
    pipe_shader_ns: u64,
    pipe_uniform_atlas_ns: u64,
    pipe_cell_ns: u64,
    pipe_blit_ns: u64,
    pipe_tray_ns: u64,
    pipe_bloom_ns: u64,
    pipe_vbuf_ns: u64,
    pipe_tail_ns: u64,
    tail_ns: u64,
    cell_pipeline_ns: [u64; aterm_gpu::startup_probe::CELL_PIPELINE_COUNT],
}

/// Read the process-global probe slots into an unclosed sample (`tail_ns` still
/// 0, `valid` still provisional).
fn read_gpu_probe() -> StartupGpuSample {
    use aterm_gpu::startup_probe::{Leg, cell_pipeline_ns, leg_ns};
    let pipelines_ns = leg_ns(Leg::PipeTotal);
    StartupGpuSample {
        valid: true,
        instance_ns: leg_ns(Leg::GpuInstance),
        adapter_ns: leg_ns(Leg::GpuAdapter),
        device_ns: leg_ns(Leg::GpuDevice),
        context_tail_ns: leg_ns(Leg::GpuContextTail),
        font_thread_ns: leg_ns(Leg::FontThread),
        font_join_ns: leg_ns(Leg::FontJoin),
        pipelines_ns,
        pipe_shader_ns: leg_ns(Leg::PipeShader),
        pipe_uniform_atlas_ns: leg_ns(Leg::PipeUniformAtlas),
        pipe_cell_ns: leg_ns(Leg::PipeCell),
        pipe_blit_ns: leg_ns(Leg::PipeBlit),
        pipe_tray_ns: leg_ns(Leg::PipeTray),
        pipe_bloom_ns: leg_ns(Leg::PipeBloom),
        pipe_vbuf_ns: leg_ns(Leg::PipeVertexBuffers),
        pipe_tail_ns: leg_ns(Leg::PipeTail),
        tail_ns: 0,
        cell_pipeline_ns: cell_pipeline_ns(),
    }
}

/// Fold a probe read into a sample, closed against the worker's own
/// measurement of the same call (`gpu_build_ns`).
fn close_startup_gpu(sample: StartupGpuSample, gpu_build_ns: u64) -> StartupGpuSample {
    // A zero slot is the probe's UNSET sentinel (a recorded leg floors at 1 ns),
    if [
        sample.instance_ns,
        sample.adapter_ns,
        sample.device_ns,
        sample.context_tail_ns,
        sample.font_thread_ns,
        sample.font_join_ns,
        sample.pipelines_ns,
    ]
    .into_iter()
    .any(|leg| leg == 0)
    {
        return StartupGpuSample::default();
    }
    let exclusive_ns = [
        sample.instance_ns,
        sample.adapter_ns,
        sample.device_ns,
        sample.context_tail_ns,
        sample.font_join_ns,
        sample.pipelines_ns,
    ]
    .into_iter()
    .try_fold(0u64, |total, leg| total.checked_add(leg));
    let Some(tail_ns) = exclusive_ns.and_then(|measured| gpu_build_ns.checked_sub(measured)) else {
        return StartupGpuSample::default();
    };
    StartupGpuSample { tail_ns, ..sample }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StartupPresentSample {
    gui_entry_ns: u64,
    rust_main_ns: u64,
    phases: StartupPhaseSample,
    attach: StartupAttachSample,
}

impl Startup {
    /// Publish the first-present timing sample.
    pub(crate) fn publish_first_present(&self, timing: StartupPresentTiming) {
        let _ = self.present.get_or_init(|| {
            let published_at = Instant::now();
            let elapsed_ns = |start: Instant| {
                u64::try_from(published_at.saturating_duration_since(start).as_nanos())
                    .unwrap_or(u64::MAX)
                    .max(1)
            };
            StartupPresentSample {
                gui_entry_ns: elapsed_ns(*self.gui_entry.get_or_init(|| published_at)),
                rust_main_ns: self.rust_main.get().copied().map_or(0, elapsed_ns),
                phases: derive_startup_phases(
                    StartupMilestones {
                        rust_main: self.rust_main.get().copied(),
                        gui_entry: self.gui_entry.get().copied(),
                        gui_ready: self.gui_ready.get().copied(),
                        winit_resumed: self.winit_resumed.get().copied(),
                        surface_ready: self.surface_ready.get().copied(),
                    },
                    timing,
                    published_at,
                ),
                attach: derive_startup_attach(
                    self.winit_resumed.get().copied(),
                    self.attach.get().copied(),
                    self.surface_ready.get().copied(),
                ),
            }
        });
    }
}

/// Snapshot of startup metrics emitted in a summary reply ([`Startup::facts`]).
#[derive(Clone, Copy, Default)]
pub(crate) struct StartupFacts {
    /// The present-anchored sample [`Startup::publish_first_present`]
    /// published at the first present (`Default` until then; `rust_main_ns`
    /// stays 0 for the thin dev GUI binary).
    present: StartupPresentSample,
    /// The backend worker's partition, kept rather than baked into the
    /// present sample: every stamp it needs is set long before the first
    /// present (the join precedes the first frame by construction).
    worker: StartupWorkerSample,
    gpu: StartupGpuSample,
    /// GUI entry → the first window's reveal ([`Startup::first_visible_since`]).
    first_visible_ns: u64,
    /// Rust `main` → the same reveal.
    rust_main_to_first_visible_ns: u64,
    /// GUI entry → ready for winit, read live from its immutable stamps even
    /// before a first present.
    gui_prepare_ns: u64,
    /// The kernel's birth → Rust `main` ([`Startup::birth_to_main`]).
    birth_to_main_ns: Option<u64>,
    /// The effect pipelines built on demand: count, wall time and the built
    /// slots' mask (`aterm_gpu::startup_probe::effect_build_ledger`).
    effect_builds: u64,
    effect_build_ns: u64,
    effect_built_mask: u64,
}

impl StartupFacts {
    /// Emit the startup span fields into `s`.
    pub(crate) fn emit_span(&self, s: &mut dyn FieldSink) {
        use wire::ms;
        s.decimal(
            "rust_main_to_first_present_ms",
            ms(self.present.rust_main_ns),
        );
        s.decimal(
            "rust_main_to_first_visible_ms",
            ms(self.rust_main_to_first_visible_ns),
        );
        let phases = self.present.phases;
        s.count("startup_phase_schema", STARTUP_PHASE_SCHEMA);
        s.flag("startup_phase_valid", phases.valid);
        s.decimal("startup_router_ms", ms(phases.router_ns));
        s.decimal("startup_gui_prepare_ms", ms(self.gui_prepare_ns));
        s.decimal("startup_winit_dispatch_ms", ms(phases.winit_dispatch_ns));
        s.decimal(
            "startup_initial_surface_attach_ms",
            ms(phases.initial_surface_attach_ns),
        );
        s.decimal(
            "startup_surface_to_successful_redraw_ms",
            ms(phases.surface_to_successful_redraw_ns),
        );
        s.decimal(
            "startup_successful_compose_ms",
            ms(phases.successful_compose_ns),
        );
        s.decimal(
            "startup_successful_surface_transaction_ms",
            ms(phases.successful_surface_transaction_ns),
        );
        s.decimal(
            "startup_successful_finalize_ms",
            ms(phases.successful_finalize_ns),
        );
        let attach = self.present.attach;
        s.count("startup_attach_schema", STARTUP_ATTACH_SCHEMA);
        s.flag("startup_attach_valid", attach.valid);
        s.decimal("startup_attach_dispatch_ms", ms(attach.dispatch_ns));
        s.decimal("startup_attach_prepare_ms", ms(attach.prepare_ns));
        s.decimal(
            "startup_attach_window_create_ms",
            ms(attach.window_create_ns),
        );
        s.decimal("startup_attach_window_setup_ms", ms(attach.window_setup_ns));
        s.decimal(
            "startup_attach_backend_finalize_ms",
            ms(attach.backend_finalize_ns),
        );
        s.decimal(
            "startup_attach_chrome_geometry_ms",
            ms(attach.chrome_geometry_ns),
        );
        s.decimal(
            "startup_attach_surface_create_ms",
            ms(attach.surface_create_ns),
        );
        s.decimal("startup_attach_finish_ms", ms(attach.finish_ns));
        let worker = self.worker;
        s.count("startup_worker_schema", STARTUP_WORKER_SCHEMA);
        s.flag("startup_worker_valid", worker.valid);
        s.decimal("startup_worker_total_ms", ms(worker.total_ns));
        s.decimal("startup_worker_overlap_ms", ms(worker.overlap_ns));
        s.decimal("startup_worker_after_join_ms", ms(worker.after_join_ns));
        s.decimal("startup_worker_post_join_ms", ms(worker.post_join_ns));
        s.decimal("startup_worker_prelude_ms", ms(worker.prelude_ns));
        s.decimal("startup_worker_gpu_build_ms", ms(worker.gpu_build_ns));
        s.decimal("startup_worker_font_admit_ms", ms(worker.font_admit_ns));
        s.decimal("startup_worker_font_apply_ms", ms(worker.font_apply_ns));
        s.decimal("startup_worker_font_seal_ms", ms(worker.font_seal_ns));
        s.decimal("startup_worker_epilogue_ms", ms(worker.epilogue_ns));
        let gpu = self.gpu;
        s.count("startup_gpu_schema", STARTUP_GPU_SCHEMA);
        s.flag("startup_gpu_valid", gpu.valid);
        s.decimal("startup_gpu_instance_ms", ms(gpu.instance_ns));
        s.decimal("startup_gpu_adapter_ms", ms(gpu.adapter_ns));
        s.decimal("startup_gpu_device_ms", ms(gpu.device_ns));
        s.decimal("startup_gpu_context_tail_ms", ms(gpu.context_tail_ns));
        s.decimal("startup_gpu_font_thread_ms", ms(gpu.font_thread_ns));
        s.decimal("startup_gpu_font_join_ms", ms(gpu.font_join_ns));
        s.decimal("startup_gpu_pipelines_ms", ms(gpu.pipelines_ns));
        s.decimal("startup_gpu_pipe_shader_ms", ms(gpu.pipe_shader_ns));
        s.decimal(
            "startup_gpu_pipe_uniform_atlas_ms",
            ms(gpu.pipe_uniform_atlas_ns),
        );
        s.decimal("startup_gpu_pipe_cell_ms", ms(gpu.pipe_cell_ns));
        s.decimal("startup_gpu_pipe_blit_ms", ms(gpu.pipe_blit_ns));
        s.decimal("startup_gpu_pipe_tray_ms", ms(gpu.pipe_tray_ns));
        s.decimal("startup_gpu_pipe_bloom_ms", ms(gpu.pipe_bloom_ns));
        s.decimal("startup_gpu_pipe_vbuf_ms", ms(gpu.pipe_vbuf_ns));
        s.decimal("startup_gpu_pipe_tail_ms", ms(gpu.pipe_tail_ns));
        s.decimal("startup_gpu_tail_ms", ms(gpu.tail_ns));
        let cell_pipelines: Vec<(&str, wire::Cell<'_>)> =
            aterm_gpu::startup_probe::CELL_PIPELINE_NAMES
                .iter()
                .zip(gpu.cell_pipeline_ns)
                .map(|(name, ns)| (*name, wire::Cell::Decimal(ms(ns))))
                .collect();
        s.pairs("startup_gpu_cell_pipeline_ms", &cell_pipelines, "none");
        s.count("effect_pipeline_builds", self.effect_builds);
        s.decimal("effect_pipeline_build_ms", ms(self.effect_build_ns));
        s.label(
            "effect_pipelines_built",
            &effect_pipeline_names(self.effect_built_mask),
        );
        s.decimal("first_present_ms", ms(self.present.gui_entry_ns));
        s.decimal("first_visible_ms", ms(self.first_visible_ns));
    }
}

/// Name the EFFECT pipelines an `effect_pipeline_built_mask` says were
/// compiled, comma-joined, or `none` — a self-labelling field, so a reader
/// never has to know the slot order and a new `EffectPipeline` cannot silently
/// shift someone else's bit.
fn effect_pipeline_names(mask: u64) -> String {
    let built = aterm_gpu::EFFECT_PIPELINE_NAMES
        .iter()
        .enumerate()
        .filter(|(slot, _)| mask & (1u64 << slot) != 0)
        .map(|(_, name)| *name)
        .collect::<Vec<_>>();
    if built.is_empty() {
        "none".to_string()
    } else {
        built.join(",")
    }
}

#[cfg(test)]
mod claim_timeline_tests {
    use super::{
        ClaimStamp, ClaimTimelineInputs, StartupAttachMilestones, claim_timeline_points,
        claim_worker_vs_join,
    };
    use std::time::{Duration, Instant};

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// The shape of a real handoff boot (the 24802 run's segments), with the
    /// worker finishing before the join.
    fn a_boot() -> ClaimTimelineInputs {
        let exec = Instant::now();
        let dial = exec + ms(31);
        let claim = dial + ms(5);
        let mut own = [None; ClaimStamp::COUNT];
        own[ClaimStamp::Intake.index()] = Some(claim + ms(12));
        own[ClaimStamp::Journal.index()] = Some(claim + ms(48));
        own[ClaimStamp::Adopt0.index()] = Some(claim + ms(60));
        own[ClaimStamp::LoopBuild.index()] = Some(claim + ms(66));
        own[ClaimStamp::EventLoopBuilt.index()] = Some(claim + ms(80));
        own[ClaimStamp::Services.index()] = Some(claim + ms(99));
        own[ClaimStamp::Control.index()] = Some(claim + ms(103));
        own[ClaimStamp::ConfigJoined.index()] = Some(claim + ms(104));
        own[ClaimStamp::AppBuilt.index()] = Some(claim + ms(105));
        own[ClaimStamp::FirstPresent.index()] = Some(claim + ms(203));
        own[ClaimStamp::RestoreStart.index()] = Some(claim + ms(204));
        own[ClaimStamp::RestoreDrained.index()] = Some(claim + ms(219));
        own[ClaimStamp::Proof.index()] = Some(claim + ms(223));
        own[ClaimStamp::Commit.index()] = Some(claim + ms(256));
        let attach = [110, 115, 135, 140, 141, 160, 170].map(|n| claim + ms(n));
        ClaimTimelineInputs {
            claim,
            exec: Some(exec),
            warm: None,
            dial: Some(dial),
            park: Some(claim - ms(2)),
            own,
            worker_spawn: Some(claim + ms(32)),
            worker_done: Some(claim + ms(90)),
            run_app: Some(claim + ms(106)),
            resumed: Some(claim + ms(108)),
            attach: Some(StartupAttachMilestones::new(attach)),
            surface_ready: Some(claim + ms(172)),
            visible: Some(claim + ms(205)),
        }
    }

    #[test]
    fn every_boundary_is_signed_ms_after_the_claim_in_boot_order() {
        let points = claim_timeline_points(&a_boot());
        let labels: Vec<_> = points.iter().map(|(label, _)| *label).collect();
        assert_eq!(
            labels,
            [
                "main",
                "dial",
                "park",
                "intake",
                "worker_spawn",
                "journal",
                "adopt0",
                "loop_build",
                "event_loop",
                "services",
                "control",
                "config_joined",
                "app_built",
                "run_app",
                "resumed",
                "attach",
                "window_create",
                "window_created",
                "join",
                "worker_done",
                "joined",
                "surface_create",
                "surface_created",
                "surface_ready",
                "first_present",
                "visible",
                "restore",
                "drained",
                "proof",
                "commit",
            ]
        );
        let at = |label: &str| points.iter().find(|(l, _)| *l == label).unwrap().1;
        assert_eq!(at("main"), -36_000);
        assert_eq!(at("dial"), -5_000);
        assert_eq!(at("intake"), 12_000);
        assert_eq!(at("surface_create"), 160_000);
        assert_eq!(at("surface_created"), 170_000);
        assert_eq!(at("proof"), 223_000);
        assert_eq!(at("commit") - at("proof"), 33_000);
        assert_eq!(at("park"), -2_000);
        assert_eq!(at("proof") - at("park"), 225_000, "park→proof");
    }

    #[test]
    fn a_boundary_never_reached_is_left_out_not_printed_as_zero() {
        let mut boot = a_boot();
        boot.own[ClaimStamp::RestoreStart.index()] = None;
        boot.own[ClaimStamp::RestoreDrained.index()] = None;
        boot.attach = None;
        boot.park = None;
        let points = claim_timeline_points(&boot);
        for gone in [
            "park",
            "restore",
            "drained",
            "attach",
            "join",
            "joined",
            "surface_created",
        ] {
            assert!(points.iter().all(|(l, _)| *l != gone), "{gone}");
        }
        assert!(claim_worker_vs_join(&boot).is_none());
    }

    #[test]
    fn the_worker_is_judged_against_the_join_on_both_sides() {
        let boot = a_boot();
        assert_eq!(claim_worker_vs_join(&boot), Some((1_000, 50_000)));
        let mut late = boot;
        late.worker_done = Some(boot.claim + ms(141));
        assert_eq!(claim_worker_vs_join(&late), Some((1_000, -1_000)));
    }
}

#[cfg(test)]
mod startup_prepare_tests {
    use super::{
        ClaimStamp, LOOP_FIRST_CUTS, STARTUP_PREPARE_CUTS, STARTUP_PREPARE_PHASES,
        birth_to_main_ns, derive_startup_prepare, derive_startup_prepare_of,
    };
    use std::time::{Duration, Instant, UNIX_EPOCH};

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// The nine cuts of a cold launch, in boot order, `entry`-relative.
    fn cuts(entry: Instant) -> [Option<Instant>; 9] {
        [4, 14, 34, 36, 41, 52, 60, 75, 90].map(|n| Some(entry + ms(n)))
    }

    #[test]
    fn the_preparation_is_exclusive_phases_that_sum_to_it() {
        let entry = Instant::now();
        let ready = entry + ms(110);
        let phases =
            derive_startup_prepare(Some(entry), cuts(entry), Some(ready)).expect("every cut");
        assert_eq!(STARTUP_PREPARE_PHASES.len(), phases.len());
        assert_eq!(
            phases,
            [4, 10, 20, 2, 5, 11, 8, 15, 15, 20].map(|n| n * 1_000_000)
        );
        assert_eq!(
            phases.iter().sum::<u64>(),
            110_000_000,
            "the parent, exactly"
        );
    }

    #[test]
    fn a_missing_or_out_of_order_boundary_leaves_the_split_invalid() {
        let entry = Instant::now();
        let ready = Some(entry + ms(110));
        for missing in 0..9 {
            let mut gone = cuts(entry);
            gone[missing] = None;
            assert_eq!(derive_startup_prepare(Some(entry), gone, ready), None);
        }
        assert_eq!(derive_startup_prepare(None, cuts(entry), ready), None);
        assert_eq!(derive_startup_prepare(Some(entry), cuts(entry), None), None);
        let mut swapped = cuts(entry);
        swapped.swap(5, 6);
        assert_eq!(derive_startup_prepare(Some(entry), swapped, ready), None);
        assert_eq!(
            derive_startup_prepare(Some(entry), cuts(entry), Some(entry + ms(80))),
            None
        );
    }

    /// The stamps of a boot that offered `order`'s cuts at [`cuts`]' instants, in
    /// that order, and no other stamp.
    fn stamped(entry: Instant, order: [ClaimStamp; 9]) -> impl Fn(ClaimStamp) -> Option<Instant> {
        let at: Vec<(ClaimStamp, Option<Instant>)> = order.into_iter().zip(cuts(entry)).collect();
        move |stamp| {
            at.iter()
                .find(|(cut, _)| *cut == stamp)
                .and_then(|(_, t)| *t)
        }
    }

    /// LAG-11: session 0 is spawned before the event loop is built, except where the
    /// build opens a display no server has answered yet and goes first.
    #[test]
    fn either_boot_order_is_split_under_the_same_names() {
        let entry = Instant::now();
        let ready = Some(entry + ms(110));
        let of = |order| derive_startup_prepare_of(Some(entry), stamped(entry, order), ready);
        assert_eq!(
            of(STARTUP_PREPARE_CUTS),
            Some([4, 10, 20, 2, 5, 11, 8, 15, 15, 20].map(|n| n * 1_000_000)),
            "session 0 first: journal 20, session0 2, event_loop 5"
        );
        assert_eq!(
            of(LOOP_FIRST_CUTS),
            Some([4, 10, 2, 5, 20, 11, 8, 15, 15, 20].map(|n| n * 1_000_000)),
            "the loop first: its 20 ms build is still event_loop's"
        );
        let mut neither = STARTUP_PREPARE_CUTS;
        neither.swap(3, 4);
        assert_eq!(of(neither), None);
    }

    /// A birth at boot-clock tick `boot`, with its wall-clock record.
    fn start(boot: u64) -> aterm_uds::process::ProcessStart {
        aterm_uds::process::ProcessStart {
            boot: aterm_uds::process::BootClock(boot),
            wall: Some(aterm_uds::process::ProcessBirth {
                seconds: 1_792_118_934,
                microseconds: 927_250,
            }),
        }
    }

    /// Apple silicon's timebase: 24 MHz ticks, 125/3 ns each.
    const APPLE_SILICON: Option<(u32, u32)> = Some((125, 3));

    /// THE BOOT CLOCK, NOT THE WALL (sweep item 36): the subtraction is on
    /// the kernel's mach ticks through the timebase, so a wall clock NTP
    /// stepped an hour ahead between birth and `main` reads the same 5 ms.
    #[test]
    fn birth_to_main_is_read_on_the_boot_clock_through_a_wall_step() {
        let born = start(1 << 30);
        let main_boot = Some(aterm_uds::process::BootClock((1 << 30) + 120_000));
        let stepped = UNIX_EPOCH
            + Duration::from_secs(1_792_118_934 + 3_600)
            + Duration::from_micros(927_250 + 5_000);
        assert_eq!(
            birth_to_main_ns(born, main_boot, stepped, APPLE_SILICON),
            Some(5_000_000)
        );
        assert_eq!(
            birth_to_main_ns(born, None, stepped, APPLE_SILICON),
            Some(3_600_005_000_000),
            "the wall clock alone reads the step"
        );
        let early = Some(aterm_uds::process::BootClock((1 << 30) - 1));
        assert_eq!(birth_to_main_ns(born, early, stepped, APPLE_SILICON), None);
    }

    /// The wall-clock fallback (a translated process; no timebase) is the
    /// kernel birth to the microsecond.
    #[test]
    fn birth_to_main_falls_back_to_the_kernel_birth_to_the_microsecond() {
        let main = UNIX_EPOCH + Duration::from_secs(1_792_118_936) + Duration::from_micros(857_000);
        assert_eq!(
            birth_to_main_ns(start(0), None, main, APPLE_SILICON),
            Some(1_929_750_000)
        );
        let boot = Some(aterm_uds::process::BootClock(5));
        assert_eq!(
            birth_to_main_ns(start(0), boot, main, None),
            Some(1_929_750_000),
            "no timebase: the wall clock"
        );
        let before = UNIX_EPOCH + Duration::from_secs(1_792_118_934);
        assert_eq!(
            birth_to_main_ns(start(0), None, before, APPLE_SILICON),
            None
        );
    }

    /// On this machine: the kernel's start record of this very process orders
    /// before a boot-clock reading taken now, and the timebase turns ticks
    /// into the nanoseconds `Instant` counts.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn the_boot_clock_reads_in_nanoseconds_through_this_machines_timebase() {
        let pid = i32::try_from(std::process::id()).expect("a pid");
        let born = aterm_uds::process::read_process_start(pid).expect("our own start record");
        let now = aterm_uds::process::boot_clock_now();
        assert!(
            birth_to_main_ns(
                born,
                now,
                std::time::SystemTime::now(),
                super::mach_timebase()
            )
            .is_some(),
            "born before now on the boot clock"
        );
        if aterm_uds::process::translation() == aterm_uds::process::Translation::Translated {
            assert_eq!(now, None, "a translated process reads no boot clock");
            println!(
                "NOT RUN under Rosetta: metrics::startup::startup_prepare_tests::\
                 the_boot_clock_reads_in_nanoseconds_through_this_machines_timebase: a translated \
                 process reads no boot clock"
            );
            return;
        }
        let boot = || aterm_uds::process::boot_clock_now().expect("a native boot clock");
        let (outer0, first, inner0) = (Instant::now(), boot(), Instant::now());
        std::thread::sleep(ms(20));
        let (inner1, second, outer1) = (Instant::now(), boot(), Instant::now());
        let at_first = aterm_uds::process::ProcessStart {
            boot: first,
            wall: None,
        };
        let read = birth_to_main_ns(
            at_first,
            Some(second),
            std::time::SystemTime::now(),
            super::mach_timebase(),
        )
        .expect("a later reading orders");
        let inner = u64::try_from((inner1 - inner0).as_nanos()).expect("fits");
        let outer = u64::try_from((outer1 - outer0).as_nanos()).expect("fits");
        assert!(
            (inner.saturating_sub(1_000)..=outer + 1_000).contains(&read),
            "{read} ns outside [{inner}, {outer}]"
        );
    }
}

#[cfg(test)]
mod startup_worker_tests {
    use super::{StartupGpuSample, StartupWorkerLegs, close_startup_gpu, derive_startup_worker};
    use std::time::{Duration, Instant};

    /// Synthetic 30 ms worker leg sample for tests.
    pub(super) fn legs() -> StartupWorkerLegs {
        StartupWorkerLegs {
            prelude_ns: 1_000_000,
            gpu_build_ns: 12_000_000,
            font_admit_ns: 2_000_000,
            font_apply_ns: 1_000_000,
            font_seal_ns: 14_000_000,
        }
    }

    #[test]
    fn a_worker_still_running_at_the_join_splits_both_intervals_exactly() {
        let spawn = Instant::now();
        let join_entry = spawn + Duration::from_millis(20);
        let done = join_entry + Duration::from_millis(10);
        let join_exit = done + Duration::from_millis(1);
        let sample = derive_startup_worker(
            Some(spawn),
            Some(done),
            Some(legs()),
            Some(join_entry),
            Some(join_exit),
        );
        assert!(sample.valid);
        assert_eq!(sample.total_ns, 30_000_000);
        assert_eq!(sample.overlap_ns, 20_000_000);
        assert_eq!(sample.after_join_ns, 10_000_000);
        assert_eq!(sample.post_join_ns, 1_000_000);
        assert_eq!(sample.overlap_ns + sample.after_join_ns, sample.total_ns);
        assert_eq!(sample.after_join_ns + sample.post_join_ns, 11_000_000);
        assert_eq!(sample.epilogue_ns, 0);
    }

    #[test]
    fn a_worker_finished_before_the_join_costs_the_join_nothing() {
        let spawn = Instant::now();
        let done = spawn + Duration::from_millis(30);
        let join_entry = done + Duration::from_millis(5);
        let join_exit = join_entry + Duration::from_micros(10);
        let sample = derive_startup_worker(
            Some(spawn),
            Some(done),
            Some(legs()),
            Some(join_entry),
            Some(join_exit),
        );
        assert!(sample.valid);
        assert_eq!(sample.after_join_ns, 0);
        assert_eq!(sample.overlap_ns, sample.total_ns);
        assert_eq!(sample.post_join_ns, 10_000);
    }

    #[test]
    fn a_missing_stamp_or_leg_transaction_publishes_nothing() {
        let spawn = Instant::now();
        let join_entry = spawn + Duration::from_millis(20);
        let done = join_entry + Duration::from_millis(10);
        let join_exit = done + Duration::from_millis(1);
        for (spawn, done, legs, entry, exit) in [
            (
                None,
                Some(done),
                Some(legs()),
                Some(join_entry),
                Some(join_exit),
            ),
            (
                Some(spawn),
                None,
                Some(legs()),
                Some(join_entry),
                Some(join_exit),
            ),
            (
                Some(spawn),
                Some(done),
                None,
                Some(join_entry),
                Some(join_exit),
            ),
            (Some(spawn), Some(done), Some(legs()), None, Some(join_exit)),
            (
                Some(spawn),
                Some(done),
                Some(legs()),
                Some(join_entry),
                None,
            ),
        ] {
            let sample = derive_startup_worker(spawn, done, legs, entry, exit);
            assert!(!sample.valid);
            assert_eq!(sample.total_ns, 0);
        }
    }

    #[test]
    fn legs_that_overrun_the_worker_refuse_to_publish_a_partition() {
        let spawn = Instant::now();
        let join_entry = spawn + Duration::from_millis(1);
        let done = join_entry + Duration::from_millis(1);
        let join_exit = done + Duration::from_millis(1);
        let sample = derive_startup_worker(
            Some(spawn),
            Some(done),
            Some(legs()),
            Some(join_entry),
            Some(join_exit),
        );
        assert!(!sample.valid);
    }

    /// Synthetic 12 ms GPU probe sample for tests.
    pub(super) fn probe() -> StartupGpuSample {
        StartupGpuSample {
            valid: true,
            instance_ns: 10_000,
            adapter_ns: 7_000_000,
            device_ns: 2_400_000,
            context_tail_ns: 5_000,
            font_thread_ns: 5_400_000,
            font_join_ns: 10_000,
            pipelines_ns: 2_500_000,
            pipe_shader_ns: 850_000,
            pipe_uniform_atlas_ns: 50_000,
            pipe_cell_ns: 1_400_000,
            pipe_blit_ns: 100_000,
            pipe_tray_ns: 40_000,
            pipe_bloom_ns: 30_000,
            pipe_vbuf_ns: 20_000,
            pipe_tail_ns: 1_000,
            tail_ns: 0,
            cell_pipeline_ns: [100_000; aterm_gpu::startup_probe::CELL_PIPELINE_COUNT],
        }
    }

    #[test]
    fn the_gpu_tail_absorbs_whatever_the_probe_did_not_measure() {
        let exclusive_ns = 10_000 + 7_000_000 + 2_400_000 + 5_000 + 10_000 + 2_500_000;
        let sample = close_startup_gpu(probe(), exclusive_ns + 80_000);
        assert!(sample.valid);
        assert_eq!(sample.tail_ns, 80_000);
        assert_eq!(sample.font_thread_ns, 5_400_000);
    }

    #[test]
    fn an_unset_probe_is_not_a_zero_length_gpu_build() {
        assert!(!close_startup_gpu(StartupGpuSample::default(), 12_000_000).valid);
        let mut partial = probe();
        partial.device_ns = 0;
        assert!(!close_startup_gpu(partial, 12_000_000).valid);
    }

    #[test]
    fn legs_that_do_not_fit_their_parent_refuse_to_publish() {
        assert!(!close_startup_gpu(probe(), 1_000_000).valid);
    }
}

#[cfg(test)]
mod facts_tests {
    use super::startup_worker_tests::{legs, probe};
    use super::{
        Startup, StartupAttachMilestones, StartupGpuSample, birth_to_main_ns, close_startup_gpu,
        derive_startup_worker,
    };
    use aterm_uds::process::{BootClock, ProcessBirth, ProcessStart};
    use std::time::{Duration, Instant, UNIX_EPOCH};

    /// Each kept fact of a local ledger is read live while one of its inputs
    /// is missing, and kept once every input exists, equal to a fresh
    /// derivation.
    #[test]
    fn facts_publish_once_every_input_exists() {
        let ledger = Startup::new();
        let unset_probe = StartupGpuSample::default;

        let birth = ProcessStart {
            boot: BootClock(0),
            wall: Some(ProcessBirth {
                seconds: 1_792_118_934,
                microseconds: 927_250,
            }),
        };
        let main = UNIX_EPOCH + Duration::from_secs(1_792_118_936) + Duration::from_micros(857_000);
        let facts = ledger.facts_from(
            || unreachable!("asked the kernel with no Rust-main stamp"),
            unset_probe,
        );
        assert_eq!(facts.birth_to_main_ns, None);
        let _ = ledger.rust_main_wall.set(main);
        assert_eq!(
            ledger.facts_from(|| None, unset_probe).birth_to_main_ns,
            None
        );
        assert_eq!(
            ledger.birth_to_main.get(),
            None,
            "an unreadable record is read again"
        );
        let fresh = birth_to_main_ns(birth, None, main, None);
        assert!(fresh.is_some());
        assert_eq!(
            ledger
                .facts_from(|| Some(birth), unset_probe)
                .birth_to_main_ns,
            fresh
        );
        assert_eq!(ledger.birth_to_main.get().copied(), fresh);
        assert_eq!(
            ledger
                .facts_from(|| unreachable!("asked the kernel again"), unset_probe)
                .birth_to_main_ns,
            fresh
        );

        let spawn = Instant::now();
        let join_entry = spawn + Duration::from_millis(20);
        let done = join_entry + Duration::from_millis(10);
        let join_exit = done + Duration::from_millis(1);
        let _ = ledger.worker_spawn.set(spawn);
        let _ = ledger.worker_done.set(done);
        let _ = ledger.worker_legs.set(legs());
        let facts = ledger.facts_from(|| None, probe);
        assert!(!facts.worker.valid);
        assert_eq!(ledger.worker.get(), None);
        assert!(!facts.gpu.valid);
        assert_eq!(ledger.gpu.get(), None);
        let _ = ledger.attach.set(StartupAttachMilestones::new([
            spawn, spawn, spawn, join_entry, join_exit, join_exit, join_exit,
        ]));
        let worker = derive_startup_worker(
            Some(spawn),
            Some(done),
            Some(legs()),
            Some(join_entry),
            Some(join_exit),
        );
        assert!(worker.valid);
        let facts = ledger.facts_from(|| None, unset_probe);
        assert_eq!(facts.worker, worker);
        assert_eq!(ledger.worker.get(), Some(&worker));
        assert!(!facts.gpu.valid);
        assert_eq!(ledger.gpu.get(), None);
        let in_flight = || StartupGpuSample {
            pipelines_ns: 0,
            ..probe()
        };
        assert!(!ledger.facts_from(|| None, in_flight).gpu.valid);
        assert_eq!(ledger.gpu.get(), None);

        let gpu = close_startup_gpu(probe(), worker.gpu_build_ns);
        assert!(gpu.valid);
        assert_eq!(ledger.facts_from(|| None, probe).gpu, gpu);
        assert_eq!(ledger.gpu.get(), Some(&gpu));
        assert_eq!(
            ledger
                .facts_from(|| None, || unreachable!("read the probe again"))
                .gpu,
            gpu
        );
    }
}

#[cfg(test)]
mod startup_phase_tests {
    use super::*;

    fn timeline() -> (StartupMilestones, StartupPresentTiming, Instant) {
        let rust_main = Instant::now();
        let gui_entry = rust_main + std::time::Duration::from_millis(1);
        let gui_ready = gui_entry + std::time::Duration::from_millis(2);
        let winit_resumed = gui_ready + std::time::Duration::from_millis(3);
        let surface_ready = winit_resumed + std::time::Duration::from_millis(4);
        let frame_started = surface_ready + std::time::Duration::from_millis(5);
        let pre_present = frame_started + std::time::Duration::from_millis(6);
        let surface_return = pre_present + std::time::Duration::from_millis(7);
        let published_at = surface_return + std::time::Duration::from_millis(8);
        (
            StartupMilestones {
                rust_main: Some(rust_main),
                gui_entry: Some(gui_entry),
                gui_ready: Some(gui_ready),
                winit_resumed: Some(winit_resumed),
                surface_ready: Some(surface_ready),
            },
            StartupPresentTiming::new(frame_started, pre_present, surface_return),
            published_at,
        )
    }

    fn attach_timeline() -> (Instant, StartupAttachMilestones, Instant) {
        let winit_resumed = Instant::now();
        let points = std::array::from_fn(|index| {
            winit_resumed + std::time::Duration::from_millis((index + 1) as u64)
        });
        let surface_ready = winit_resumed + std::time::Duration::from_millis(8);
        (
            winit_resumed,
            StartupAttachMilestones::new(points),
            surface_ready,
        )
    }

    #[test]
    fn startup_phases_are_an_exact_exclusive_partition() {
        let (milestones, timing, published_at) = timeline();
        let phases = derive_startup_phases(milestones, timing, published_at);
        assert!(phases.valid);
        assert_eq!(
            [
                phases.router_ns,
                phases.gui_prepare_ns,
                phases.winit_dispatch_ns,
                phases.initial_surface_attach_ns,
                phases.surface_to_successful_redraw_ns,
                phases.successful_compose_ns,
                phases.successful_surface_transaction_ns,
                phases.successful_finalize_ns,
            ],
            [
                1_000_000, 2_000_000, 3_000_000, 4_000_000, 5_000_000, 6_000_000, 7_000_000,
                8_000_000,
            ]
        );
        assert_eq!(phases.total_ns(), Some(36_000_000));
        assert_eq!(
            phases
                .total_ns()
                .and_then(|total| total.checked_sub(phases.router_ns)),
            Some(35_000_000)
        );
    }

    #[test]
    fn startup_attach_is_an_exact_exclusive_parent_partition() {
        let (winit_resumed, milestones, surface_ready) = attach_timeline();
        let attach =
            derive_startup_attach(Some(winit_resumed), Some(milestones), Some(surface_ready));
        assert!(attach.valid);
        assert_eq!(
            [
                attach.dispatch_ns,
                attach.prepare_ns,
                attach.window_create_ns,
                attach.window_setup_ns,
                attach.backend_finalize_ns,
                attach.chrome_geometry_ns,
                attach.surface_create_ns,
                attach.finish_ns,
            ],
            [1_000_000; 8]
        );
        assert_eq!(attach.total_ns(), Some(8_000_000));
        assert_eq!(
            attach.total_ns(),
            duration_ns(winit_resumed, surface_ready),
            "the drill-down must equal its initial-surface-attach parent"
        );
    }

    #[test]
    fn startup_attach_fails_closed_on_missing_reordered_or_overflowing_data() {
        let (winit_resumed, milestones, surface_ready) = attach_timeline();
        assert_eq!(
            derive_startup_attach(Some(winit_resumed), None, Some(surface_ready)),
            StartupAttachSample::default()
        );
        let mut reversed = milestones;
        reversed.points.swap(3, 4);
        assert_eq!(
            derive_startup_attach(Some(winit_resumed), Some(reversed), Some(surface_ready)),
            StartupAttachSample::default()
        );
        let overflow = StartupAttachSample {
            valid: true,
            dispatch_ns: u64::MAX,
            finish_ns: 1,
            ..StartupAttachSample::default()
        };
        assert_eq!(overflow.total_ns(), None);
    }

    #[test]
    fn phase_derivation_conforms_to_the_derived_publication_gate() {
        let model = aterm_models_core::startup_phase_publication_model();
        let (milestones, timing, published_at) = timeline();
        let mut incomplete_milestones = milestones;
        incomplete_milestones.surface_ready = None;

        let mut state = model.init_state();
        for _ in 0..7 {
            assert!(model.fire("Step", &mut state));
        }
        let host_valid = derive_startup_phases(incomplete_milestones, timing, published_at).valid;
        assert_eq!(
            !model.successors("Publish", &state).is_empty(),
            host_valid,
            "Tier-1: the real missing-milestone decision matches phase 7"
        );

        assert!(model.fire("Step", &mut state));
        let host_valid = derive_startup_phases(milestones, timing, published_at).valid;
        assert_eq!(
            !model.successors("Publish", &state).is_empty(),
            host_valid,
            "Tier-1: the complete real partition matches phase 8"
        );
        assert!(model.fire("Publish", &mut state));

        let reversed = StartupPresentTiming::new(
            timing.pre_present,
            timing.frame_started,
            timing.surface_return,
        );
        assert!(
            !derive_startup_phases(milestones, reversed, published_at).valid,
            "negative control: real out-of-order stamps must fail closed"
        );

        let attach_model = aterm_models_core::startup_phase_publication_model();
        let (winit_resumed, attach_milestones, surface_ready) = attach_timeline();
        let mut attach_state = attach_model.init_state();
        for _ in 0..7 {
            assert!(attach_model.fire("Step", &mut attach_state));
        }
        let incomplete_attach =
            derive_startup_attach(Some(winit_resumed), Some(attach_milestones), None).valid;
        assert_eq!(
            !attach_model.successors("Publish", &attach_state).is_empty(),
            incomplete_attach,
            "Tier-1: the missing surface-ready boundary matches attach phase 7"
        );
        assert!(attach_model.fire("Step", &mut attach_state));
        let complete_attach = derive_startup_attach(
            Some(winit_resumed),
            Some(attach_milestones),
            Some(surface_ready),
        )
        .valid;
        assert_eq!(
            !attach_model.successors("Publish", &attach_state).is_empty(),
            complete_attach,
            "Tier-1: the complete attach partition matches phase 8"
        );
        let mut reversed_attach = attach_milestones;
        reversed_attach.points.swap(1, 2);
        assert!(
            !derive_startup_attach(
                Some(winit_resumed),
                Some(reversed_attach),
                Some(surface_ready),
            )
            .valid,
            "negative control: an out-of-order attach stamp must fail closed"
        );
    }
}
