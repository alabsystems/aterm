// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process startup ledger: write-once boot milestones and derived startup metrics.

use super::wire::{self, FieldSink};
use crate::label_enum::label_enum;
use aterm_uds::process::{BootClock, ProcessStart};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
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
    /// Admitted overlap intake enables diagnostic records on every transport lane.
    handoff_diagnostics: OnceLock<()>,
    /// Journal each existing or future milestone once, independently of the claim clock.
    claim_logged: [OnceLock<()>; ClaimStamp::COUNT],
    /// One coherent, immutable startup fact. `OnceLock` publishes both clock
    /// projections together, so a racing control read cannot pair a populated
    /// compatibility field with a missing router field.
    present: OnceLock<StartupPresentSample>,
    /// The facts kept once every input exists ([`Self::facts`]).
    birth_to_main: OnceLock<u64>,
    worker: OnceLock<StartupWorkerSample>,
    gpu: OnceLock<StartupGpuSample>,
    /// The keeper's boot HELLO answered, its offers in hand ([`mark_keeper_hello`]).
    keeper_hello: OnceLock<Instant>,
    /// Every offer of that HELLO admitted or refused ([`record_keeper_admitted`]).
    keeper_admitted: OnceLock<KeeperAdmitted>,
    /// Every session that admission recovered placed ([`mark_keeper_placed`]).
    keeper_placed: OnceLock<Instant>,
    /// Their history imports settled ([`mark_keeper_imported`]).
    keeper_imported: OnceLock<Instant>,
    /// The recovery-count fence's boot cohort: one read observer per old journal authority the
    /// admission retained ([`record_keeper_cohort`], `keeper_link::RecoveryCountAdmission`).
    #[cfg(unix)]
    keeper_cohort: OnceLock<Vec<std::sync::Arc<aterm_keeper::client::ReadObserver>>>,
    /// The first control request past that fence ([`mark_keeper_served`]).
    keeper_served: OnceLock<Instant>,
    /// What the restore lane's look-alike seam did so far (`record_keeper_seam`).
    keeper_seam: SeamCounters,
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
            handoff_diagnostics: OnceLock::new(),
            claim_logged: [const { OnceLock::new() }; ClaimStamp::COUNT],
            present: OnceLock::new(),
            birth_to_main: OnceLock::new(),
            worker: OnceLock::new(),
            gpu: OnceLock::new(),
            keeper_hello: OnceLock::new(),
            keeper_admitted: OnceLock::new(),
            keeper_placed: OnceLock::new(),
            keeper_imported: OnceLock::new(),
            #[cfg(unix)]
            keeper_cohort: OnceLock::new(),
            keeper_served: OnceLock::new(),
            keeper_seam: SeamCounters::new(),
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
        }
    }

    fn birth_to_main(&self, read_birth: impl FnOnce() -> Option<ProcessStart>) -> Option<u64> {
        kept(&self.birth_to_main, || {
            let main_wall = *self.rust_main_wall.get()?;
            let main_boot = self.rust_main_boot.get().copied().flatten();
            birth_to_main_ns(read_birth()?, main_boot, main_wall, mach_timebase())
        })
    }

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

    /// Emit pre-winit boot fields (`handoff_claim`, `startup_prepare`, `startup_worker`,
    /// `startup_keeper`).
    pub(crate) fn emit_boot(&self, facts: &StartupFacts, s: &mut dyn FieldSink) {
        self.emit_handoff_claim(s);
        self.emit_startup_prepare(facts, s);
        self.emit_keeper_recovery(s);
        #[cfg(all(unix, any(test, debug_assertions)))]
        super::restore_experiment::emit(s);
    }

    /// The keeper's boot recovery: `startup_keeper_offers=`, `startup_keeper_hello_ms=` (Rust
    /// `main` to the answered HELLO), `startup_keeper_admit_ms=` (that HELLO to every offer
    /// admitted or refused, one after another), `startup_keeper_admit_max_ms=` (the slowest one
    /// offer), `startup_keeper_placed_ms=` (that HELLO to every recovered session placed) and
    /// `startup_keeper_imported_ms=` (that HELLO to their history imports settled), then the
    /// recovery-count fence (`keeper_link::RecoveryCountAdmission`): `startup_keeper_fence_cohort=`
    /// (the old journal authorities it waits on), `startup_keeper_sync_sent_ms=` (that HELLO to
    /// the last moment the keeper was asked to confirm one, of the confirmations that lifted the
    /// fence: the SYNC written behind the frame that settled its journal, a new journal's
    /// COMPANION, a COMPANION_RELEASE or the master's RELEASE, or, for one no SYNC answered, the
    /// confirmation itself, `aterm_keeper::client::ConfirmedRead`),
    /// `startup_keeper_sync_answer_max_ms=` (the slowest one from asked to confirmed),
    /// `startup_keeper_fence_ms=` (that HELLO to the last confirmation, where the fence
    /// lifts) and `startup_keeper_served_ms=` (that HELLO to the first control request past the
    /// fence); every one `none` on a launch whose HELLO no keeper answered, and each until its
    /// moment (the SYNC readings and the fence's until every authority is confirmed, so never
    /// with a cohort of none). A build that compiles the crash-restore seams adds
    /// `startup_keeper_seam_copies=`, `startup_keeper_seam_plants=` and
    /// `startup_keeper_seam_ms=`: what the restore lane's look-alike seam did in that admission,
    /// whose time each reading above leaves out ([`keeper_recovery`]).
    fn emit_keeper_recovery(&self, s: &mut dyn FieldSink) {
        let sample = keeper_recovery(KeeperStamps {
            main: self.main_start(),
            hello: self.keeper_hello.get().copied(),
            admitted: self.keeper_admitted.get().copied(),
            placed: self.keeper_placed.get().copied(),
            imported: self.keeper_imported.get().copied(),
            fence: self.keeper_fence(),
            served: self.keeper_served.get().copied(),
        });
        s.opt_count("startup_keeper_offers", sample.map(|k| k.offers), "none");
        let read = |field: fn(&KeeperRecovery) -> Option<u64>| sample.as_ref().and_then(field);
        for (key, ns) in [
            ("startup_keeper_hello_ms", read(|k| k.hello_ns)),
            ("startup_keeper_admit_ms", read(|k| k.admit_ns)),
            ("startup_keeper_admit_max_ms", read(|k| k.slowest_ns)),
            ("startup_keeper_placed_ms", read(|k| k.placed_ns)),
            ("startup_keeper_imported_ms", read(|k| k.imported_ns)),
        ] {
            s.opt_decimal(key, ns.map(wire::ms), "none");
        }
        s.opt_count(
            "startup_keeper_fence_cohort",
            read(|k| k.fence_cohort),
            "none",
        );
        for (key, ns) in [
            ("startup_keeper_sync_sent_ms", read(|k| k.sync_sent_ns)),
            (
                "startup_keeper_sync_answer_max_ms",
                read(|k| k.sync_answer_max_ns),
            ),
            ("startup_keeper_fence_ms", read(|k| k.fence_ns)),
            ("startup_keeper_served_ms", read(|k| k.served_ns)),
        ] {
            s.opt_decimal(key, ns.map(wire::ms), "none");
        }
        #[cfg(any(test, debug_assertions))]
        {
            let seam = sample.and_then(|k| k.admit.map(|a| a.seam));
            s.opt_count("startup_keeper_seam_copies", seam.map(|w| w.copies), "none");
            s.opt_count("startup_keeper_seam_plants", seam.map(|w| w.plants), "none");
            s.opt_decimal(
                "startup_keeper_seam_ms",
                seam.map(|w| wire::ms(w.ns)),
                "none",
            );
        }
    }

    /// The fence's cohort as its observers read now (`None` before [`record_keeper_cohort`]).
    fn keeper_fence(&self) -> Option<KeeperFence> {
        #[cfg(unix)]
        {
            self.keeper_cohort.get().map(|cohort| {
                keeper_fence(
                    cohort
                        .iter()
                        .map(|o| o.confirmation().map(|r| (r.sync_sent, r.answer_read))),
                )
            })
        }
        #[cfg(not(unix))]
        {
            None
        }
    }
}

/// The recovery-count fence's cohort as the ledger reads it ([`keeper_fence`]).
#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KeeperFence {
    /// The old journal authorities the fence waits on.
    cohort: u64,
    /// `None` until the keeper confirmed, for every one of them, that it can no longer offer
    /// that journal.
    lifted: Option<FenceLift>,
}

/// When the fence lifted, and what decided it.
#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FenceLift {
    /// The last moment the keeper was asked to confirm one, of the confirmations that lifted
    /// it: the SYNC written behind the frame that settled the journal (a COMPANION, a
    /// COMPANION_RELEASE or a RELEASE), or the confirmation itself where no SYNC answered it
    /// (`aterm_keeper::client::ConfirmedRead::sync_sent`).
    last_sent: Instant,
    /// The last of those confirmations: the fence lifts there.
    last_read: Instant,
    /// The slowest one from asked to confirmed.
    slowest_answer_ns: u64,
}

/// The fence over `reads`, each authority's confirmation as (when it was asked, when it was
/// confirmed; `aterm_keeper::client::ConfirmedRead`), `None` before it: lifted once every one
/// is confirmed, never with an empty cohort (whose fence never closed).
#[cfg_attr(not(unix), allow(dead_code))]
fn keeper_fence(reads: impl IntoIterator<Item = Option<(Instant, Instant)>>) -> KeeperFence {
    let mut cohort = 0u64;
    let mut unconfirmed = false;
    let mut lift: Option<FenceLift> = None;
    for read in reads {
        cohort = cohort.saturating_add(1);
        let Some((sent, answered)) = read else {
            unconfirmed = true;
            continue;
        };
        let answer_ns = duration_ns(sent, answered).unwrap_or(0);
        lift = Some(match lift {
            None => FenceLift {
                last_sent: sent,
                last_read: answered,
                slowest_answer_ns: answer_ns,
            },
            Some(so_far) => FenceLift {
                last_sent: so_far.last_sent.max(sent),
                last_read: so_far.last_read.max(answered),
                slowest_answer_ns: so_far.slowest_answer_ns.max(answer_ns),
            },
        });
    }
    KeeperFence {
        cohort,
        lifted: lift.filter(|_| !unconfirmed),
    }
}

/// What the restore lane's look-alike seam did (`crash_restore::LOOKALIKE_SEAM`, the
/// `verify perf restore-e2e` copy and lookalike arms): the copies of offered regions it made,
/// the plants that took in them, and the time both took. Work a really damaged region never
/// pays, so [`keeper_recovery`] leaves it out of every reading that spans it. Only a debug or
/// test build's seam adds to it (`record_keeper_seam`); everywhere else it stays zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SeamSpent {
    pub(crate) copies: u64,
    pub(crate) plants: u64,
    pub(crate) ns: u64,
}

impl SeamSpent {
    /// What was spent after `earlier`, a reading taken before this one.
    #[cfg_attr(not(unix), allow(dead_code))]
    #[must_use]
    pub(crate) const fn since(self, earlier: Self) -> Self {
        Self {
            copies: self.copies.saturating_sub(earlier.copies),
            plants: self.plants.saturating_sub(earlier.plants),
            ns: self.ns.saturating_sub(earlier.ns),
        }
    }

    /// The time it took.
    #[cfg_attr(not(unix), allow(dead_code))]
    #[must_use]
    pub(crate) const fn spent(self) -> Duration {
        Duration::from_nanos(self.ns)
    }
}

/// The process's running [`SeamSpent`] ([`keeper_seam`]).
struct SeamCounters {
    copies: AtomicU64,
    plants: AtomicU64,
    ns: AtomicU64,
}

impl SeamCounters {
    const fn new() -> Self {
        Self {
            copies: AtomicU64::new(0),
            plants: AtomicU64::new(0),
            ns: AtomicU64::new(0),
        }
    }

    fn read(&self) -> SeamSpent {
        SeamSpent {
            copies: self.copies.load(Ordering::Relaxed),
            plants: self.plants.load(Ordering::Relaxed),
            ns: self.ns.load(Ordering::Relaxed),
        }
    }
}

/// The end of the boot recovery's admission ([`record_keeper_admitted`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KeeperAdmitted {
    at: Instant,
    offers: u64,
    /// The slowest one offer, less the seam's work inside it.
    slowest_ns: u64,
    /// What the seam did between the answered HELLO and `at`.
    seam: SeamSpent,
}

/// The boot recovery's stamps, as [`keeper_recovery`] reads them.
#[derive(Clone, Copy, Debug, Default)]
struct KeeperStamps {
    /// Rust `main` (or the GUI entry).
    main: Option<Instant>,
    hello: Option<Instant>,
    admitted: Option<KeeperAdmitted>,
    placed: Option<Instant>,
    imported: Option<Instant>,
    fence: Option<KeeperFence>,
    served: Option<Instant>,
}

/// The keeper's boot recovery as `metrics` reports it ([`keeper_recovery`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KeeperRecovery {
    /// The offers the HELLO answered with (0 before the admission's end is recorded).
    offers: u64,
    /// Rust `main` (or the GUI entry) to the answered HELLO; `None` with no such stamp.
    hello_ns: Option<u64>,
    /// The answered HELLO to the admission's end, less the seam; `None` before that end.
    admit_ns: Option<u64>,
    /// The slowest one offer, less the seam; `None` before the admission's end.
    slowest_ns: Option<u64>,
    /// The answered HELLO to every recovered session placed, less the seam; `None` before then.
    placed_ns: Option<u64>,
    /// The answered HELLO to their history imports settled, less the seam; `None` before then.
    imported_ns: Option<u64>,
    /// The old journal authorities the recovery-count fence waits on; `None` before the
    /// admission retained them.
    fence_cohort: Option<u64>,
    /// The answered HELLO to the last confirming SYNC written, less the seam; `None` until the
    /// fence lifts.
    sync_sent_ns: Option<u64>,
    /// The slowest one confirming SYNC from written to its answer read; `None` until the fence
    /// lifts.
    sync_answer_max_ns: Option<u64>,
    /// The answered HELLO to the fence lifting (the last answer read), less the seam.
    fence_ns: Option<u64>,
    /// The answered HELLO to the first control request past the fence, less the seam.
    served_ns: Option<u64>,
    admit: Option<KeeperAdmitted>,
}

/// The keeper's boot recovery from its stamps: `None` until a keeper answered the HELLO. The
/// seam's work all falls inside the admission, so every reading from the HELLO to a moment at or
/// after the admission's end leaves it out, and placement, the imports and the fence's moments
/// read only once the admission's end is stamped.
fn keeper_recovery(stamps: KeeperStamps) -> Option<KeeperRecovery> {
    let hello = stamps.hello?;
    let admitted = stamps.admitted;
    let less_seam = |at: Instant| {
        let a = admitted?;
        (at >= a.at)
            .then(|| duration_ns(hello, at))
            .flatten()
            .map(|ns| ns.saturating_sub(a.seam.ns))
    };
    let lift = stamps.fence.and_then(|f| f.lifted);
    Some(KeeperRecovery {
        offers: admitted.map_or(0, |a| a.offers),
        hello_ns: stamps.main.and_then(|main| duration_ns(main, hello)),
        admit_ns: admitted.and_then(|a| less_seam(a.at)),
        slowest_ns: admitted.map(|a| a.slowest_ns),
        placed_ns: stamps.placed.and_then(less_seam),
        imported_ns: stamps.imported.and_then(less_seam),
        fence_cohort: stamps.fence.map(|f| f.cohort),
        sync_sent_ns: lift.and_then(|l| less_seam(l.last_sent)),
        sync_answer_max_ns: lift.map(|l| l.slowest_answer_ns),
        fence_ns: lift.and_then(|l| less_seam(l.last_read)),
        served_ns: stamps.served.and_then(less_seam),
        admit: admitted,
    })
}

/// Mark the keeper's boot HELLO answered, its offers in hand (`keeper_link::boot`).
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn mark_keeper_hello() {
    let _ = STARTUP.keeper_hello.set(Instant::now());
}

/// Record the end of the boot recovery's admission: `offers` admitted or refused one after
/// another, the slowest one taking `slowest` (less the seam's work inside it), and what the
/// restore lane's seam did since the HELLO (`seam`, [`keeper_seam`]'s reading since then).
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn record_keeper_admitted(offers: usize, slowest: Duration, seam: SeamSpent) {
    let _ = STARTUP.keeper_admitted.set(KeeperAdmitted {
        at: Instant::now(),
        offers: u64::try_from(offers).unwrap_or(u64::MAX),
        slowest_ns: u64::try_from(slowest.as_nanos()).unwrap_or(u64::MAX),
        seam,
    });
}

/// Mark every session the keeper's boot recovery brought back placed: the restore pass that
/// places them after the first has drained, which a window runs once its first frame is on glass.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn mark_keeper_placed() {
    let _ = STARTUP.keeper_placed.set(Instant::now());
}

/// Mark the history imports settled: the first settle after the keeper's boot recovery placed
/// its sessions (a launch with no HELLO answered reports none, whatever it imported).
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn mark_keeper_imported() {
    if STARTUP.keeper_placed.get().is_some() {
        let _ = STARTUP.keeper_imported.set(Instant::now());
    }
}

/// Record the recovery-count fence's boot cohort: the read observer of every old journal
/// authority the admission retained (`keeper_link::RecoveryCountAdmission`), which the ledger
/// reads for when the keeper confirmed it can no longer offer each one.
#[cfg(unix)]
pub(crate) fn record_keeper_cohort(
    cohort: Vec<std::sync::Arc<aterm_keeper::client::ReadObserver>>,
) {
    let _ = STARTUP.keeper_cohort.set(cohort);
}

/// Mark a control request past the recovery-count fence: the first one stays.
pub(crate) fn mark_keeper_served() {
    if STARTUP.keeper_served.get().is_none() {
        let _ = STARTUP.keeper_served.set(Instant::now());
    }
}

/// What the restore lane's look-alike seam did so far in this process ([`SeamSpent`]).
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn keeper_seam() -> SeamSpent {
    STARTUP.keeper_seam.read()
}

/// Count one copy the restore lane's look-alike seam made of an offered region, `planted` when
/// its plant took, and the `spent` time both took ([`SeamSpent`]).
#[cfg(all(unix, any(test, debug_assertions)))]
pub(crate) fn record_keeper_seam(spent: Duration, planted: bool) {
    let seam = &STARTUP.keeper_seam;
    seam.copies.fetch_add(1, Ordering::Relaxed);
    if planted {
        seam.plants.fetch_add(1, Ordering::Relaxed);
    }
    seam.ns.fetch_add(
        u64::try_from(spent.as_nanos()).unwrap_or(u64::MAX),
        Ordering::Relaxed,
    );
}

fn kept<T: Copy>(cell: &OnceLock<T>, derive: impl FnOnce() -> Option<T>) -> Option<T> {
    if let Some(fact) = cell.get() {
        return Some(*fact);
    }
    let fact = derive()?;
    Some(*cell.get_or_init(|| fact))
}

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

/// How long ago this process began: its kernel start record against the boot clock now, through
/// the mach timebase ([`birth_to_main_ns`]), so a wall-clock step does not move it; against the
/// wall clock only where no boot clock reads (a translated process). `None` where the record is
/// unreadable or the reading comes before it.
#[cfg(target_os = "macos")]
pub(crate) fn process_age() -> Option<Duration> {
    process_age_at(SystemTime::now())
}

/// [`process_age`] with the wall clock's reading passed in, which counts only where no boot clock
/// reads.
#[cfg(target_os = "macos")]
fn process_age_at(wall_now: SystemTime) -> Option<Duration> {
    birth_to_main_ns(
        read_own_start()?,
        aterm_uds::process::boot_clock_now(),
        wall_now,
        mach_timebase(),
    )
    .map(Duration::from_nanos)
}

/// The time from the kernel's start record `start` to a reading taken later (`main_boot` on the
/// boot clock, `main_wall` on the wall clock), in nanoseconds: on the boot clock through
/// `timebase` where both exist, else on the wall clock. `None` where the reading comes before
/// the start, or neither clock can say.
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

/// Mark the start of winit's first `resumed` callback: the native launch finished, which a
/// handoff successor reports to its parent.
pub(crate) fn mark_first_winit_resumed() {
    if STARTUP.winit_resumed.set(Instant::now()).is_ok() {
        #[cfg(unix)]
        crate::handoff_progress::report("resumed");
    }
}

/// Whether winit's first `resumed` has come: this process's native launch finished.
#[cfg(unix)]
#[must_use]
pub(crate) fn launch_finished() -> bool {
    STARTUP.winit_resumed.get().is_some()
}

/// Whether this process presented its first window ([`ClaimStamp::FirstPresent`]): its native
/// launch, its first window and surface and a first frame all ran here.
#[cfg(unix)]
#[must_use]
pub(crate) fn first_present_done() -> bool {
    STARTUP.claim_stamps[ClaimStamp::FirstPresent.index()]
        .get()
        .is_some()
}

/// Mark the first successfully attached OS window surface.
pub(crate) fn mark_initial_surface_ready() {
    let _ = STARTUP.surface_ready.set(Instant::now());
}

/// Record the first window-visibility milestone.
pub(crate) fn mark_first_window_visible() {
    let _ = STARTUP.first_visible.set(Instant::now());
}

const STARTUP_PHASE_SCHEMA: u64 = 1;

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

    const fn backend_finalize_bounds(self) -> (Instant, Instant) {
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

const STARTUP_WORKER_SCHEMA: u64 = 1;

/// 2: the legs every arm runs (context, font thread and join, assembly); 1 was
/// wgpu's instance, adapter, device and pipeline legs.
const STARTUP_GPU_SCHEMA: u64 = 2;

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
    if STARTUP.claim_at.set(claimed_at).is_ok()
        && let Some(stamps) = claim_timeline_log_text()
    {
        aterm_log::info!(
            "overlap handoff: claim baseline pid={} build={} {stamps}",
            std::process::id(),
            crate::build_info::build_number()
        );
    }
}

/// Record a [`ClaimStamp`] boot milestone.
pub(crate) fn stamp_claim(stamp: ClaimStamp) {
    STARTUP.stamp_claim_with(stamp, &mut crate::handoff_progress::milestone);
}

/// Retain startup evidence after overlap intake admits both channels, including
/// inherited-descriptor successors that never dial a rendezvous or set its clock.
pub(crate) fn begin_handoff_diagnostics() {
    STARTUP.begin_handoff_diagnostics_with(&mut crate::handoff_progress::milestone);
}

impl Startup {
    fn begin_handoff_diagnostics_with(&self, emit: &mut impl FnMut(ClaimStamp)) {
        if self.handoff_diagnostics.set(()).is_ok() {
            for stamp in ClaimStamp::ALL {
                self.emit_claim_stamp_with(stamp, emit);
            }
        }
    }

    fn stamp_claim_with(&self, stamp: ClaimStamp, emit: &mut impl FnMut(ClaimStamp)) {
        let _ = self.claim_stamps[stamp.index()].get_or_init(Instant::now);
        self.emit_claim_stamp_with(stamp, emit);
    }

    fn emit_claim_stamp_with(&self, stamp: ClaimStamp, emit: &mut impl FnMut(ClaimStamp)) {
        if self.handoff_diagnostics.get().is_some()
            && self.claim_stamps[stamp.index()].get().is_some()
            && self.claim_logged[stamp.index()].set(()).is_ok()
        {
            emit(stamp);
        }
    }
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

type ClaimPoint = (&'static str, i64);

fn signed_us(zero: Instant, at: Instant) -> i64 {
    match at.checked_duration_since(zero) {
        Some(after) => i64::try_from(after.as_micros()).unwrap_or(i64::MAX),
        None => {
            i64::try_from(zero.duration_since(at).as_micros()).map_or(i64::MIN, std::ops::Neg::neg)
        }
    }
}

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

    fn claim_warm_pre_dial(&self) -> Option<Duration> {
        self.claim_at.get()?;
        self.claim_warm_pre_dial.get().copied()
    }

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

const STARTUP_PREPARE_SCHEMA: u64 = 1;

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
    context_ns: u64,
    font_thread_ns: u64,
    font_join_ns: u64,
    assemble_ns: u64,
    tail_ns: u64,
}

fn read_gpu_probe() -> StartupGpuSample {
    use aterm_gpu::startup_probe::{Leg, leg_ns};
    let assemble_ns = leg_ns(Leg::Assemble);
    StartupGpuSample {
        valid: true,
        context_ns: leg_ns(Leg::Context),
        font_thread_ns: leg_ns(Leg::FontThread),
        font_join_ns: leg_ns(Leg::FontJoin),
        assemble_ns,
        tail_ns: 0,
    }
}

fn close_startup_gpu(sample: StartupGpuSample, gpu_build_ns: u64) -> StartupGpuSample {
    // A zero slot is the probe's UNSET sentinel (a recorded leg floors at 1 ns),
    // so any zero here means a leg never ran in this process.
    if [
        sample.context_ns,
        sample.font_thread_ns,
        sample.font_join_ns,
        sample.assemble_ns,
    ]
    .into_iter()
    .any(|leg| leg == 0)
    {
        return StartupGpuSample::default();
    }
    let exclusive_ns = [sample.context_ns, sample.font_join_ns, sample.assemble_ns]
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
        s.decimal("startup_gpu_context_ms", ms(gpu.context_ns));
        s.decimal("startup_gpu_font_thread_ms", ms(gpu.font_thread_ns));
        s.decimal("startup_gpu_font_join_ms", ms(gpu.font_join_ns));
        s.decimal("startup_gpu_assemble_ms", ms(gpu.assemble_ns));
        s.decimal("startup_gpu_tail_ms", ms(gpu.tail_ns));
        s.decimal("first_present_ms", ms(self.present.gui_entry_ns));
        s.decimal("first_visible_ms", ms(self.first_visible_ns));
    }
}

#[cfg(test)]
#[path = "startup_claim_timeline_tests.rs"]
mod claim_timeline_tests;

#[cfg(test)]
#[path = "startup_startup_prepare_tests.rs"]
mod startup_prepare_tests;

#[cfg(test)]
#[path = "startup_startup_worker_tests.rs"]
mod startup_worker_tests;

#[cfg(test)]
#[path = "startup_facts_tests.rs"]
mod facts_tests;

#[cfg(test)]
#[path = "startup_startup_phase_tests.rs"]
mod startup_phase_tests;

#[cfg(test)]
#[path = "startup_keeper_recovery_tests.rs"]
mod keeper_recovery_tests;
