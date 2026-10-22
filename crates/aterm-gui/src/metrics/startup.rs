// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE STARTUP LEDGER: the boot's write-once stamps and the startup facts
//! derived from them, published on the `metrics` summary and kept by `metrics
//! reset`, since a process has one startup timeline.
//!
//! Every stamp is a first-write-wins `OnceLock` of a [`Startup`]. The boot's
//! call sites offer them through the free `mark_*`, `stamp_claim` and
//! `record_*` functions, which write the process's [`STARTUP`] (re-exported
//! from `metrics`, where those call sites name them). A fact derived from
//! stamps alone can never change once all of them exist, so
//! [`Startup::facts`] derives each one once its inputs are complete and keeps
//! it, and derives it live until then: the kernel's birth → Rust `main` (a
//! `proc_pidinfo` call), the backend worker's partition and the GPU build's
//! split. The present-anchored sample is published once, at the first present
//! ([`Startup::publish_first_present`]). What can still move is read on every
//! reply: the first reveal, the GUI-preparation split (checked against the
//! parent this reply read), the claim timeline (whose late stamps may never
//! come) and the on-demand effect-pipeline ledger (a running total).

use super::wire::{self, FieldSink};
use crate::label_enum::label_enum;
use aterm_uds::process::{BootClock, ProcessStart};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The process's startup ledger, written by the boot and read by every
/// `metrics` summary. Tests build a [`Startup::new`] of their own.
pub(crate) static STARTUP: Startup = Startup::new();

/// One startup ledger: the boot's stamps, the first present's sample, and the
/// facts kept once their inputs exist.
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
    /// subtract to the pre-main time whatever NTP did between them. `None`
    /// where this process cannot read that clock in the kernel's unit
    /// (Rosetta).
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
    /// This is the number the user's eye measures at launch: "when did a window
    /// exist AT ALL", as opposed to `first_present` ("when did real content
    /// land"). The warm-launch early reveal moves it BEFORE the backend join, so
    /// it must be its own stamp — it is not derivable from any phase of the
    /// exclusive present partition, and unlike that partition it can also
    /// legally land AFTER the first present (an overlap handoff reveals only
    /// once the carried pixels are on). First-write-wins: every window's reveal
    /// path offers the stamp; only the first records, so later Cmd-N windows can
    /// never replace it.
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
    /// the grant. Unset when the parent sent no park stamp.
    claim_parked_before: OnceLock<Duration>,
    /// The boot's own boundaries ([`stamp_claim`]).
    claim_stamps: [OnceLock<Instant>; ClaimStamp::COUNT],
    /// One coherent, immutable startup fact. `OnceLock` publishes both clock
    /// projections together, so a racing control read cannot pair a populated
    /// compatibility field with a missing router field. `rust_main_ns` is 0
    /// only for the thin dev GUI binary.
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

    /// The process clock's anchor: the GUI entry, or this read's instant when
    /// nothing has stamped or read it yet.
    pub(crate) fn clock_anchor(&self) -> Instant {
        *self.gui_entry.get_or_init(Instant::now)
    }

    /// The facts one summary publishes, read once per reply. Call it only
    /// after acquiring the frame count (`Counter::get_acquire`): once a caller
    /// observes frame 1, it must also observe the sample
    /// [`Self::publish_first_present`] published before that frame's Release
    /// increment.
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
            // A valid split is final: the probe is first-write-wins, and a
            // read that finds its last leg set holds every leg of that build
            // ([`read_gpu_probe`]). A windowed launch's worker fills the probe
            // before its `done`. A headless launch's worker builds the CPU
            // face (`defer_gpu_build`) and the probe fills on the main thread
            // at the first pixel demand (`App::ensure_pixel_backend`), after
            // the worker's partition is kept: the split is read live until a
            // read closes it valid.
            self.worker.get()?;
            Some(close_startup_gpu(read_probe(), worker.gpu_build_ns)).filter(|gpu| gpu.valid)
        })
        .unwrap_or_default();
        // Read STRAIGHT from the probe, outside the GPU split: the demand-build
        // ledger is a running total for the life of the process, not a
        // cold-build leg, so it must not be folded into (or invalidated by) the
        // startup partition's reconciliation against `gpu_build_ns`, nor kept.
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
    /// prompt waits here for the person's click), dyld and page-in. An image
    /// that re-exec'd itself (the boot apply's swap) counts the image before it
    /// too: `execve` keeps the birth. `None` off Apple (no mach timebase and
    /// no wall-clock birth), on the thin dev GUI binary (no Rust-main stamp;
    /// the kernel is not asked), or when the two readings do not order.
    /// Subtracted on the boot clock (mach ticks), so a wall-clock step between
    /// birth and `main` moves nothing; a translated process, whose ticks read
    /// in another unit, falls back to the wall clock. Kept once read, so the
    /// process pays one kernel read and the startup path only the clock reads
    /// in [`mark_rust_main_start`]. A `None` is read again next time: a
    /// transient failure must not stick.
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

    /// The summary's fields for everything before winit runs: the claim
    /// timeline ([`Self::emit_handoff_claim`]), then the birth and the
    /// GUI-preparation split ([`Self::emit_startup_prepare`]).
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

/// Anchor the GUI process-lifetime clock at the top of [`crate::main_entry`] so
/// `first_present_ns` retains its compatibility-stable GUI-entry → first
/// startup-metrics publication point inside the successful-present finalizer.
/// Publication runs after submit succeeds and includes initial reveal,
/// application acknowledgement, and any synchronous post-submit recovery
/// bookkeeping. Without this call the clock self-anchors at whichever metric
/// fires first and the startup figure is meaningless — call it before any
/// thread spawns. Platform compositor and scanout timing are outside this
/// boundary.
pub(crate) fn mark_process_start() {
    let _ = STARTUP.gui_entry.set(Instant::now());
}

/// Anchor the shipped one-binary Rust entry before argv0/router work. Kept
/// separate from [`mark_process_start`] so the long-standing
/// `first_present_ns` boundary stays comparable across versions and thin GUI
/// binaries. Dyld/process-loader work precedes this stamp and remains outside
/// both metrics; the kernel's birth → Rust `main` measures it instead
/// ([`Startup::facts`]), from the boot-clock and wall-clock twins taken here.
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
    // SAFETY: `base` is a live, writable `mach_timebase_info` record; the call
    // only fills it.
    let rc = unsafe { libc::mach_timebase_info(&raw mut base) };
    (rc == 0 && base.denom != 0).then_some((base.numer, base.denom))
}

/// See the Apple twin: no mach timebase here.
#[cfg(not(target_vendor = "apple"))]
fn mach_timebase() -> Option<(u32, u32)> {
    None
}

/// Mark the end of synchronous GUI preparation immediately before entering
/// winit. First-write wins because a process has one startup timeline.
pub(crate) fn mark_gui_ready_for_winit() {
    let _ = STARTUP.gui_ready.set(Instant::now());
}

/// Mark the first instruction of winit's first `resumed` callback.
pub(crate) fn mark_first_winit_resumed() {
    let _ = STARTUP.winit_resumed.set(Instant::now());
}

/// Mark the first successfully attached OS window surface. A failed attachment
/// never advances the startup timeline.
pub(crate) fn mark_initial_surface_ready() {
    let _ = STARTUP.surface_ready.set(Instant::now());
}

/// Offer the first-window-reveal stamp (L1, early reveal: [`Startup`]'s
/// `first_visible`).
pub(crate) fn mark_first_window_visible() {
    let _ = STARTUP.first_visible.set(Instant::now());
}

/// Wire schema for the exclusive Rust-main → first-present phase partition.
const STARTUP_PHASE_SCHEMA: u64 = 1;

/// Wire schema for the exclusive first-resume → initial-surface-ready attach
/// partition nested inside [`STARTUP_PHASE_SCHEMA`]'s surface-attach phase.
const STARTUP_ATTACH_SCHEMA: u64 = 1;

/// Ordered internal boundaries of the one successful initial window attach.
/// The caller records the complete array only after a present target has been
/// installed, so a failed OS-window or surface attempt cannot publish a partial
/// timeline. The outer `winit_resumed` and `surface_ready` stamps are supplied
/// by the existing startup ledger and make the eight subphases reconcile
/// exactly with `startup_initial_surface_attach_ns`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StartupAttachMilestones {
    points: [Instant; 7],
}

impl StartupAttachMilestones {
    pub(crate) const fn new(points: [Instant; 7]) -> Self {
        Self { points }
    }

    /// The two stamps that bracket `backend_finalize_ns` — the join entry and
    /// the join exit. Named rather than indexed at the call site so the worker
    /// drill-down and `derive_startup_attach` can never disagree about which
    /// pair of points the phase is.
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

/// Publish the complete internal timeline for the first successfully installed
/// window surface. Every successful `attach_os_window` call may offer a complete
/// candidate; `OnceLock` admits exactly the first and later Cmd-N windows cannot
/// replace it. The winit application handler serializes production attaches on
/// the event-loop thread, while the atomic slot remains a backstop if that
/// lifecycle ever changes.
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
    /// Unlike the present-anchored startup sample this is read LIVE from the
    /// stamps per summary reply: the reveal (early-reveal path) legitimately
    /// exists long before any present publishes [`Self::publish_first_present`]'s
    /// sample, and time-to-visible must be reportable in exactly that window.
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

// BACKEND-BUILD WORKER — the inside of `backend_finalize`.
//
// `startup_attach_backend_finalize_ns` measures ONE `handle.join()`: the event
// loop blocking on the backend-build worker spawned in `crate::main_entry`.
// That single number was READ AS the largest phase in the whole ledger
// (300.83 ms median on macOS, 66.1% of rust_main → first_present, recorded
// 2026-07-30) and, until these stamps, the only phase with no drill-down — so
// every optimization proposed inside it had to be argued from a guess, and
// three in a row were refused for being unsizeable. These stamps settled it on
// 2026-08-23: `backend_finalize` is 0.01 ms median (0.02 ms max, 40 fresh
// processes) with `after_join_ns` exactly 0.00 in every sample — the SMALLEST
// phase in the ledger, not the largest
// (`docs/measured/arena/2026-08-23-start-backend-finalize-drilldown-dev-smoke.md`,
// M5 Max DEV-SMOKE / NON-PUBLISHABLE). The 300.83 ms figure is superseded and
// must not be used to size work.
//
// TWO SEPARATE QUESTIONS live in that number, and conflating them is what made
// the guesses wrong:
//
//   1. WHAT does the worker do? The exclusive legs below ([`StartupWorkerLegs`])
//      plus the renderer-side split in `aterm_gpu::startup_probe`, which times
//      the wgpu instance/adapter/device legs, the parallel font thread and its
//      join, and every render pipeline.
//   2. HOW MUCH of that was still OUTSTANDING when the join was reached? A
//      worker already 90% done at the join has nothing left to win from more
//      overlap; a worker 10% done has everything. `backend_finalize_ns` cannot
//      tell those apart — it reads the same either way. `overlap_ns` vs
//      `after_join_ns` IS that split, and it is what any "start it earlier /
//      overlap it harder" proposal has to be sized against.
//
// Both stamps ([`Startup`]'s `worker_spawn` and `worker_done`) ride the one
// process `Instant` clock: `spawn` is taken on the MAIN thread immediately
// before `thread::spawn` (thread-creation cost lands inside the worker's own
// timeline, where it belongs), `done` on the WORKER thread immediately before
// it publishes the finished backend. First-write-wins like every other startup
// stamp — a process has one cold build.

/// Wire schema for the backend-build worker partition — the exclusive
/// drill-down of `startup_attach_backend_finalize_ns`.
const STARTUP_WORKER_SCHEMA: u64 = 1;

/// Wire schema for the renderer-side sub-split of the worker's GPU build,
/// sourced from `aterm_gpu::startup_probe`.
const STARTUP_GPU_SCHEMA: u64 = 1;

/// The worker's own exclusive legs, timed on the worker thread and published in
/// ONE transaction so a snapshot can never pair a filled leg with a missing one.
/// The remaining slice (`epilogue_ns`) is DERIVED against the worker's total
/// rather than stamped, so an unmeasured line inside the worker surfaces as
/// epilogue instead of silently vanishing from the partition.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StartupWorkerLegs {
    /// Worker entry → the renderer constructor call.
    pub(crate) prelude_ns: u64,
    /// `GpuRenderer::new_with_family` (or the CPU `from_system_with_family`
    /// fallback) — the leg `aterm_gpu::startup_probe` splits further.
    pub(crate) gpu_build_ns: u64,
    /// The worker's wait for the main thread to hand over the resolved font
    /// generation (`startup_font_rx.recv()`). Non-zero means the worker
    /// out-ran launch config resolution, not that fonts are slow.
    pub(crate) font_admit_ns: u64,
    /// `apply_font_config_to_backend`.
    pub(crate) font_apply_ns: u64,
    /// `seal_admitted_font_sources` — the worker-only broad fallback/symbol/
    /// emoji admission. Effectively ZERO on a headless launch, which defers that
    /// admission to its first pixel demand (`defer_font_seal`) and so does not
    /// perform it on this worker at all; the leg names the work this worker did,
    /// never the work the generation will eventually cost.
    pub(crate) font_seal_ns: u64,
}

/// Mark the instant the backend-build worker is spawned (main thread, strictly
/// before `thread::spawn`).
pub(crate) fn mark_backend_worker_spawn() {
    let _ = STARTUP.worker_spawn.set(Instant::now());
}

/// Nanoseconds elapsed since `started`, saturating.
///
/// The backend-build worker runs on a background thread with no ledger state of
/// its own: it times its legs against plain `Instant`s and hands the finished
/// durations over in ONE transaction ([`record_backend_worker_legs`]), so this
/// is the only ns conversion it needs.
pub(crate) fn leg_elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Publish the worker's complete leg timing (worker thread, before `done`).
pub(crate) fn record_backend_worker_legs(legs: StartupWorkerLegs) {
    let _ = STARTUP.worker_legs.set(legs);
}

/// Mark the instant the worker finished building the backend (worker thread,
/// immediately before it publishes).
pub(crate) fn mark_backend_worker_done() {
    let _ = STARTUP.worker_done.set(Instant::now());
}

// THE SUCCESSOR'S CLAIM TIMELINE (warm successor P0,
// docs/DESIGN-warm-successor-2026-09-29.md "Where the milliseconds go").
//
// Under the late park the outgoing terminal is frozen from just before this
// successor's rendezvous claim until Commit, and `claim->proof` is the part of
// that freeze this process pays. That one number used to be all the log said;
// every segment inside it was inferred from unrelated lines. These stamps make
// each boundary a measurement, relative to the claim instant.
//
// OBSERVATION ONLY. Every stamp is a first-write-wins `OnceLock` set that
// nothing reads back to decide anything. Every launch records them (a cold one
// too: its GUI preparation is split at the same boundaries, see
// [`Startup::emit_startup_prepare`]), but this timeline is published only on a
// launch that recorded a claim — `none` on an ordinary launch and off macOS.
// The boundaries the startup ledger already stamps (`run_app`, the first
// `resumed`, the seven attach points, the backend worker's spawn and done, the
// first reveal) are READ from that ledger, never stamped twice.

label_enum! {
    /// The boundaries the boot stamps for itself: the ones the startup ledger above
    /// has no stamp for. The label is the boundary's name in the claim timeline.
    #[cfg_attr(not(unix), allow(dead_code))] // `Proof` and `Commit` are stamped on the unix lane only
    pub(crate) enum ClaimStamp {
        /// `seamless::take_incoming` returned: the intake's authentication is done.
        Intake => "intake",
        /// Session 0 (the first adopted shell) was bound by `spawn_session`.
        Adopt0 => "adopt0",
        /// The winit `EventLoop` was built.
        EventLoopBuilt => "event_loop",
        /// The first successful present of any window (`first_present_done`).
        FirstPresent => "first_present",
        /// The deferred restore (windows 2..N, the other adopted shells) began.
        RestoreStart => "restore",
        /// The deferred restore returned: every carried shell is placed.
        RestoreDrained => "drained",
        /// The adoption proof is about to be written to the parent.
        Proof => "proof",
        /// The winit `EventLoop`'s build began: the warm prologue has returned (or
        /// was joined), and the launch font and grid are resolved.
        LoopBuild => "loop_build",
        /// The background services are started: the config watcher and the update
        /// checker have returned. The package lane is not among them: it starts at
        /// config load, before [`ClaimStamp::LoopBuild`] (`spawn_pkg_update_check`).
        Services => "services",
        /// The restore take and the crash-journal claim returned, and the journal
        /// writer exists.
        Journal => "journal",
        /// The control socket is planned and `control::spawn` returned.
        Control => "control",
        /// The Commit arrived (the successor's commit waiter): with [`Self::Proof`]
        /// the freeze's last segment, proof→commit, on this process's clock — the
        /// one that outlives the outgoing process's `_exit`.
        Commit => "commit",
        /// The config-runtime worker was joined and its path feeds installed.
        ConfigJoined => "config_joined",
        /// The `App` was built.
        AppBuilt => "app_built",
        /// The warm prologue has returned (or was joined), and the launch font and
        /// grid are resolved: session 0's preparation, and where it opens a display no
        /// server has answered yet the event loop's build (`main_entry`'s
        /// `loop_before_session0`), begins.
        Warmed => "warmed",
    }
}

impl Startup {
    /// The earliest in-process instant this process stamped: the shipped
    /// binary's Rust `main` ([`mark_rust_main_start`]), else the GUI entry (the
    /// thin dev binary). The loader's work before it is outside every stamp
    /// here.
    fn main_start(&self) -> Option<Instant> {
        self.rust_main
            .get()
            .or_else(|| self.gui_entry.get())
            .copied()
    }
}

/// [`Startup::main_start`] of the process: the warm stage's dial is placed
/// against it.
#[cfg(target_os = "macos")]
#[must_use]
pub(crate) fn process_main_start() -> Option<Instant> {
    STARTUP.main_start()
}

/// Record the rendezvous claim: the zero point of every [`ClaimStamp`]. First
/// write wins, as for every startup stamp. `parked_before_grant` projects a
/// `park` point relative to this claim; parent sample→successor claim is not
/// measured.
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

/// Offer one boot boundary: claim-relative in the claim timeline, and the cuts
/// of the GUI-preparation split on every launch. A no-op after the first write.
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
    /// dial (P2): the dial minus its warm stage. `None` otherwise.
    warm: Option<Instant>,
    dial: Option<Instant>,
    /// A projected park point: this claim minus the parent's sampled park
    /// duration. It excludes parent sample→this claim; `None` when the parent
    /// sent no stamp.
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
/// the values say what really happened). A boundary not reached is left out, so
/// every point printed is a measurement.
fn claim_timeline_points(inputs: &ClaimTimelineInputs) -> Vec<ClaimPoint> {
    let own = |stamp: ClaimStamp| (stamp.as_str(), inputs.own[stamp.index()]);
    let attach = inputs.attach.map(|milestones| milestones.points);
    let attach_point = |index: usize| attach.map(|points| points[index]);
    let ordered: [(&'static str, Option<Instant>); 31] = [
        ("main", inputs.exec),
        // Before the dial on the launched lane (P2): the worker's spawn below
        // then reads negative too, which is the point.
        ("warm", inputs.warm),
        ("dial", inputs.dial),
        // `proof - park` sums parent park→sample and successor claim→proof.
        // Parent sample→claim is excluded; this is a projected point.
        ("park", inputs.park),
        own(ClaimStamp::Intake),
        ("worker_spawn", inputs.worker_spawn),
        // Session 0 before the event loop's build: the boot order wherever a
        // claim is published (macOS, whose build opens no display before it).
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
        // The seventh attach point: without it the surface's own creation and
        // the finish after it read as one slice.
        ("surface_created", attach_point(6)),
        ("surface_ready", inputs.surface_ready),
        own(ClaimStamp::FirstPresent),
        ("visible", inputs.visible),
        own(ClaimStamp::RestoreStart),
        own(ClaimStamp::RestoreDrained),
        own(ClaimStamp::Proof),
        // On the commit waiter's thread, after the proof's write: published
        // from the successor, the process that is still running afterwards.
        own(ClaimStamp::Commit),
    ];
    ordered
        .into_iter()
        .filter_map(|(label, at)| Some((label, signed_us(inputs.claim, at?))))
        .collect()
}

/// The backend worker against the join that waits for it: how long the join
/// blocked, and how far the worker's `done` sat before (positive) or after
/// (negative) the join's entry. `None` until both halves exist.
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
    /// (`none`/`null` without one). Self-labelling pairs, like
    /// `startup_gpu_cell_pipeline_ms`, so a new boundary never shifts a column.
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

/// Signed microseconds as milliseconds with `decimals` places (`-3.1`, `12.40`).
pub(super) fn signed_ms(us: i64, decimals: usize) -> String {
    let sign = if us < 0 { "-" } else { "" };
    let ms = us.unsigned_abs() as f64 / 1000.0;
    format!("{sign}{ms:.decimals$}")
}

/// The log's rendering of the whole timeline, or `None` on a launch that did not
/// claim: `intake +12.3, adopt0 +80.1, … ; the backend join waited 0.4 ms, the
/// worker done 3.2 ms before it`.
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

// GUI PREPARATION, SPLIT (CS-2 of the 2026-10-15 launch audit).
//
// `startup_gui_prepare_ns` (GUI entry → ready for winit) was one number: on the
// owner's first launch 127.69 ms, about 57 ms of it between the event loop's
// build and the control socket with no stamp inside. The boot's own
// [`ClaimStamp`]s cut it into ten exclusive phases on every launch — headless
// too, whose preparation is the same code up to the event loop's run — read
// live like the first reveal, so a launch that never presents still answers.
// Valid only when every cut exists in one of the boot's two orders (session 0
// before the event loop's build, or after it where the build opens a display
// no server has answered yet); the ten then sum to the parent exactly, which
// `verify perf start` reconciles.

/// Wire schema for the exclusive split of GUI entry → ready for winit.
const STARTUP_PREPARE_SCHEMA: u64 = 1;

/// The phases in boot order, as their `startup_prepare_<name>_ms` fields:
///
/// * `boot`: GUI entry → the intake. The CLI, the logger, the boot apply,
///   containment; on a handoff, the claim's hold and the intake's checks.
/// * `warm`: → the warm's end. The warm prologue or its join, the launch font
///   and grid.
/// * `journal`: → the restore take and the crash-journal claim. The session
///   factory and the PTY keeper's boot hello before them.
/// * `session0`: → session 0 spawned or adopted.
/// * `event_loop`: → built. On a macOS window, `NSApplication` and its
///   WindowServer connection; headless, the display-free loop.
/// * `services`: → the config watcher, update checker and package update check
///   started.
/// * `control`: → the control socket planned and spawned.
/// * `config_join`: → the config-runtime worker joined. The supervisor host,
///   the first window's state, and the join's wait.
/// * `app`: → the `App` built. The config notices, the native lanes.
/// * `arm`: → ready for winit. The `App`'s last wiring and the watchdog.
///
/// A launch whose event loop opens a display no server has answered yet builds it
/// first instead, between `warm` and `journal` ([`LOOP_FIRST_CUTS`]); its phases are
/// published under the same names.
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
/// `loop_before_session0`). Its phases come as `boot`, `warm`, `event_loop`,
/// `journal`, `session0`, then as [`STARTUP_PREPARE_CUTS`]'s.
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
        // The control thread can read the parent just before readiness is
        // published and this split just after. Only publish a valid split when
        // it belongs to the same parent captured for this metrics reply.
        .filter(|phases| {
            phases.iter().try_fold(0u64, |sum, ns| sum.checked_add(*ns)) == Some(gui_prepare_ns)
        })
    }

    /// The `metrics` summary's fields for everything before winit runs, after
    /// the claim timeline: `process_birth_to_rust_main_ms` (`none`/`null` where
    /// the platform cannot read the birth), `startup_prepare_schema`,
    /// `startup_prepare_valid` and one `startup_prepare_<phase>_ms` per phase,
    /// every phase 0 while the split is invalid (as the other startup
    /// partitions publish). The split is read live against the reply's
    /// `startup_gui_prepare_ms`.
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
///
/// The worker can legally finish BEFORE the join is reached (a warm launch, a
/// CPU backend, a machine where window creation outlasts the build) or AFTER it
/// (every measured macOS cold launch so far). Both orders produce a well-formed
/// partition — which one happened is the finding, not an error — so the split
/// point is `min(done, join_entry)` and the resume point `max(done, join_entry)`,
/// giving two exact identities:
///
/// * `overlap_ns + after_join_ns == total_ns` — the worker's own wall time,
///   split at the join.
/// * `after_join_ns + post_join_ns == backend_finalize_ns` — the join's wall
///   time, split at the worker's finish.
///
/// `post_join_ns` is therefore the part of `backend_finalize` that is NOT the
/// worker at all: the thread handoff plus the post-join setup `finalize_backend`
/// runs (pad/head seed, GPU effect pins, font-feature warnings).
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
///
/// `font_thread_ns` is the ONE parallel leg (it overlaps the GPU legs by
/// design), so it is reported ALONGSIDE the partition, never inside it;
/// `font_join_ns` is its exclusive residue — the wait GPU init actually paid
/// for it, and the honest ceiling on what removing the font work could win.
/// `tail_ns` is derived, closing the measured legs against `gpu_build_ns`.
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
/// 0, `valid` still provisional). Kept separate from [`close_startup_gpu`] so
/// the reconciliation rules have a local, GPU-free test, and so a local
/// [`Startup`] can be read with a probe of its own.
///
/// `PipeTotal` is read first: it is the last leg a build records, published
/// with Release and read with Acquire, so once it reads set every slot read
/// after it holds that build's leg. A read racing a build in flight reads it
/// unset, which closes invalid, never a split with a sub-leg missing.
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
///
/// `valid` is false — and every field stays zero — whenever the process took the
/// CPU backend (no GPU leg ever ran), the probe is incomplete, or the legs do
/// not fit inside `gpu_build_ns`. An honest "no data" beats a partition that
/// does not reconcile.
fn close_startup_gpu(sample: StartupGpuSample, gpu_build_ns: u64) -> StartupGpuSample {
    // A zero slot is the probe's UNSET sentinel (a recorded leg floors at 1 ns),
    // so any zero here means this process never took the GPU path.
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
    /// Publish the present-anchored sample: the first startup-metrics
    /// publication point inside the successful-present finalizer (see
    /// [`mark_process_start`]). One end `Instant` derives both clock
    /// projections, published together through the `OnceLock`; the caller
    /// publishes before its frame count becomes observable. A no-op after the
    /// first present.
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

/// The startup facts one summary publishes ([`Startup::facts`]). All of them
/// survive `metrics reset`, and every one is 0 (or absent) until it is known.
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
    /// before a first present. This one completed phase does not validate the
    /// full present-anchored startup partition.
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
    /// The summary's startup span, after `max_frame_gap_ms`.
    ///
    /// `first_present_ms` keeps the compatibility-stable GUI `main_entry` →
    /// startup-metrics publication point inside the first successful-present
    /// finalizer. That point follows a successful submit and includes initial
    /// reveal, acknowledgement, and synchronous post-submit recovery
    /// bookkeeping. `rust_main_to_first_present_ms` adds the shipped
    /// one-binary Rust `main` boundary before router work (0.00 in the thin GUI
    /// binary). Schema 1's eight `startup_*_ms` fields exclusively partition
    /// that broader interval: router, synchronous GUI preparation, winit
    /// dispatch, initial surface attachment, wait/retries to the eventually
    /// successful redraw, that redraw's compose, its surface transaction, and
    /// successful-present finalization. `startup_phase_valid` is true only when
    /// every timestamp is present, ordered, and sums exactly to both enclosing
    /// startup metrics. All startup facts remain 0.00 until first present,
    /// survive `metrics reset`, and exclude dyld/process-loader time. None
    /// observes compositor selection, display timing, scanout, or photons.
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
        // The per-cell-pipeline split, `name:ms`: ONE self-labelling field rather
        // than twelve positional ones, so a reader never has to know
        // `build_cell_pipelines`' order and adding or removing a pipeline cannot
        // silently shift someone else's column.
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
/// shift someone else's bit. `none` is the healthy reading for a launch with
/// the shipped defaults.
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
        // The Rust main and the dial PRECEDE the claim, so they read negative.
        assert_eq!(at("main"), -36_000);
        assert_eq!(at("dial"), -5_000);
        assert_eq!(at("intake"), 12_000);
        // Every one of the seven attach points is printed, the last included.
        assert_eq!(at("surface_create"), 160_000);
        assert_eq!(at("surface_created"), 170_000);
        assert_eq!(at("proof"), 223_000);
        // The successor's own reading of the freeze's last segment.
        assert_eq!(at("commit") - at("proof"), 33_000);
        // The projected sum: parent park→sample was 2 ms, plus claim→proof.
        assert_eq!(at("park"), -2_000);
        assert_eq!(at("proof") - at("park"), 225_000, "park→proof");
    }

    #[test]
    fn a_boundary_never_reached_is_left_out_not_printed_as_zero() {
        let mut boot = a_boot();
        boot.own[ClaimStamp::RestoreStart.index()] = None;
        boot.own[ClaimStamp::RestoreDrained.index()] = None;
        boot.attach = None;
        // A parent from before the park stamp.
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
        // Done at +90, join entered at +140 and left at +141.
        assert_eq!(claim_worker_vs_join(&boot), Some((1_000, 50_000)));
        // A worker still running at the join is AFTER it (the negative control).
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
        // The control socket stamped before session 0: not this boot's order.
        let mut swapped = cuts(entry);
        swapped.swap(5, 6);
        assert_eq!(derive_startup_prepare(Some(entry), swapped, ready), None);
        // Ready for winit before the last cut.
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
    /// build opens a display no server has answered yet and goes first. Either order
    /// is a split, published under the same names; a boot in neither order is none.
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
        // The event loop built between the journal claim and session 0.
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
    /// NEGATIVE CONTROL: the wall clock alone (no boot-clock stamp, the
    /// translated-process fallback) reads the step.
    #[test]
    fn birth_to_main_is_read_on_the_boot_clock_through_a_wall_step() {
        let born = start(1 << 30);
        // 120,000 ticks of 125/3 ns: exactly 5 ms.
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
        // A `main` before the birth on the boot clock is no reading at all.
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
        // A wall clock stepped back between the two is no reading at all.
        let before = UNIX_EPOCH + Duration::from_secs(1_792_118_934);
        assert_eq!(
            birth_to_main_ns(start(0), None, before, APPLE_SILICON),
            None
        );
    }

    /// On this machine: the kernel's start record of this very process orders
    /// before a boot-clock reading taken now, and the timebase turns ticks
    /// into the nanoseconds `Instant` counts. Bracketed: the boot-clock
    /// interval lies between the `Instant` interval inside its reads and the
    /// one around them, however the reads were preempted; the 20 ms sleep
    /// puts a missing or inverted conversion (125/3 on Apple silicon) far
    /// outside it.
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
            // Rosetta: the translated `mach_absolute_time` counts nanoseconds while start
            // records count the kernel's ticks, so this process reads no boot clock
            // (`aterm_uds::process::apple_boot_clock`) and its start orders on the wall clock,
            // which the assertion above held. The bracket has no boot clock to read.
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

    /// Legs summing to 30 ms, the shape a real cold launch produces (the seal
    /// dominates, the GPU build is next, everything else is noise).
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
        // The two identities the whole drill-down rests on.
        assert_eq!(sample.overlap_ns + sample.after_join_ns, sample.total_ns);
        assert_eq!(sample.after_join_ns + sample.post_join_ns, 11_000_000);
        // Legs plus the derived epilogue close against the worker's wall time.
        assert_eq!(sample.epilogue_ns, 0);
    }

    #[test]
    fn a_worker_finished_before_the_join_costs_the_join_nothing() {
        // The measured macOS shape today: the whole build is hidden, so the
        // join waits on NOTHING and `after_join` must read exactly zero.
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
        // Each row drops exactly one input; every one must publish nothing.
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
        // A leg longer than the whole worker means the stamps disagree; an
        // honest "no data" beats an epilogue that would have to be negative.
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

    /// A probe read whose legs sum to 12 ms — the parent `gpu_build_ns` a
    /// reconciling sample must be closed against.
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
        // The parallel font leg is reported but never spent from the parent.
        assert_eq!(sample.font_thread_ns, 5_400_000);
    }

    #[test]
    fn an_unset_probe_is_not_a_zero_length_gpu_build() {
        // The CPU-backend launch: no GPU leg ever ran, so every slot is the
        // UNSET sentinel and the sample must refuse rather than report zeros.
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
    /// derivation. The control: every read before the last input arrived
    /// answered nothing, so a fact kept that early would have kept that
    /// nothing; each later read answers the derivation instead.
    #[test]
    fn facts_publish_once_every_input_exists() {
        let ledger = Startup::new();
        let unset_probe = StartupGpuSample::default;

        // The birth → Rust `main`: the stamp, then the kernel's record.
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
        // No boot-clock stamp: the wall clock.
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

        // The worker's partition: four of its five stamps, then the join's.
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
        // The GPU split waits for the worker's: a complete probe is not kept
        // before the partition it closes against.
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
        // A CPU launch's probe, and a headless launch's before its deferred
        // build: the GPU split stays invalid and is read again.
        let facts = ledger.facts_from(|| None, unset_probe);
        assert_eq!(facts.worker, worker);
        assert_eq!(ledger.worker.get(), Some(&worker));
        assert!(!facts.gpu.valid);
        assert_eq!(ledger.gpu.get(), None);
        // The deferred build in flight: every leg but its last, `PipeTotal`.
        let in_flight = || StartupGpuSample {
            pipelines_ns: 0,
            ..probe()
        };
        assert!(!ledger.facts_from(|| None, in_flight).gpu.valid);
        assert_eq!(ledger.gpu.get(), None);

        // The GPU split: kept once a complete build closes against the kept
        // worker, and the probe is not read again.
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
        // The derived model intentionally covers only interval completeness.
        // Ordering and exact arithmetic remain real host obligations exercised
        // by the reordered controls below and the exact-partition tests above.
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
