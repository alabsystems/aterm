// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Main-thread stall watchdog.
//!
//! It judges only while there is something to judge (ATERM-KI-1105-20). While the main thread is
//! at a park point where nothing can be reported without a beat first ([`rests_at`]: the run
//! loop's own wait after `about_to_wait`, a designed freeze, or anywhere when no CPU reading
//! exists), the sampler rests in one `poll(2)` with no timeout on its doorbell and the wall
//! watch ([`aterm_uds::wall_watch`]); the main thread's next beat rings the doorbell
//! ([`MainWatch::resting`], set and read in sequentially consistent order against the
//! breadcrumb, so either the beat sees the claim or the claim sees the beat). On macOS the main
//! thread leaves that wait only through winit's `AfterWaiting` observer (the first to run,
//! `CFIndex::MIN`), which calls `new_events` and so beats `NewEvents`; a spin that follows (the
//! 2026-09-26 timer catch-up) is judged under that root, as before. Off a rest it samples every
//! [`sample_interval`] as it always did, and it rests only after [`QUIET_SAMPLES`] samples in a
//! row found no beat: a main thread that wakes more often than that is sampled on the interval
//! alone, with no ring, so resting never costs it more wakeups than the interval does (a ring
//! and the samples after it are `1 + QUIET_SAMPLES` wakes for a gap longer than
//! `QUIET_SAMPLES` intervals, at most a quarter more than the interval alone).
//!
//! An exit asked inside `about_to_wait` parks at `ShutdownWait`, never at its rest: winit beats
//! no `NewEvents` once it is exiting ([`RootGuard::loop_ending`]).
//!
//! A sleep and a wall-clock step are heard from the system's own announcements (the wall watch)
//! and drive the caller's `on_wall_moved`; only where the system announces neither does the
//! sampler keep reading its two clocks' drift for them, and then it never rests.
//!
//! On Windows the doorbell and the wall watch are events (`aterm_uds::wall_watch`), and the rest
//! is one `WaitForMultipleObjects`. Windows announces a suspend and its resume to the wall watch
//! but a set of the wall clock to windows only, so a forward step is still read off the clocks,
//! at the sampler's next wake (`stepped_forward`), and a rest there is bounded by [`STEP_POLL`]
//! (`wait_until`) so that wake comes while the main thread stays parked. std's `Instant` keeps
//! counting through a suspend there, so each wake reads how long the machine was suspended
//! (`suspended`) and the sampler's clock skips it (`Sampler::skip`): a sleep is neither a
//! stall nor a span this process "was not running".

use crate::label_enum::label_enum;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn sample_interval() -> Duration {
    threshold() / 2
}

const STALL_THRESHOLD: Duration = Duration::from_millis(500);

const RELEASE_STALL_THRESHOLD: Duration = Duration::from_secs(5);

const STALL_REPEAT_INTERVAL: Duration = Duration::from_secs(60);
/// How many samples in a row must find no beat before the sampler may rest: below it a main
/// thread that wakes between samples is sampled on the interval with no ring (a ring is a wake
/// of the sampler's and a `write(2)` of the main thread's).
const QUIET_SAMPLES: u32 = 4;

/// The sampler's doorbell: a pipe on Unix, an event on Windows.
#[cfg(unix)]
type Bell = aterm_uds::ready::Doorbell;
#[cfg(windows)]
type Bell = aterm_uds::wall_watch::Doorbell;

/// THE MAIN THREAD'S WATCH: the cells [`beat`] stamps and the sampler, the control workers and a forking worker read.
pub(crate) struct MainWatch {
    /// Monotonic main-thread liveness counter, incremented on every winit root entry.
    heartbeat: AtomicU64,
    /// The last main-loop root [`beat`] was called from, as a [`Breadcrumb`]
    /// `u8`.
    breadcrumb: AtomicU8,
    /// Whether the root named by `breadcrumb` has RETURNED — its handler is off
    /// the stack and the main thread is back in AppKit / CoreFoundation,
    /// between aterm's handlers.
    returned: AtomicBool,
    /// The sampler rests over the current breadcrumb (its wait has no deadline), so the next
    /// beat takes this claim and rings [`MainWatch::bell`]. Set and read against `breadcrumb` in
    /// sequentially consistent order ([`MainWatch::claim_rest`], [`beat_into`]).
    resting: AtomicBool,
    /// The sampler's doorbell, made as it starts; `None` where no pipe could be made (it then
    /// never rests).
    #[cfg(any(unix, windows))]
    bell: std::sync::OnceLock<Option<Bell>>,
}

impl MainWatch {
    /// A watch no root has entered: at [`Breadcrumb::Startup`], no beat yet.
    pub(crate) const fn new() -> Self {
        Self {
            heartbeat: AtomicU64::new(0),
            breadcrumb: AtomicU8::new(Breadcrumb::Startup as u8),
            returned: AtomicBool::new(false),
            resting: AtomicBool::new(false),
            #[cfg(any(unix, windows))]
            bell: std::sync::OnceLock::new(),
        }
    }

    /// The sampler's doorbell, made on the first call.
    #[cfg(any(unix, windows))]
    fn bell(&self) -> Option<&Bell> {
        self.bell
            .get_or_init(|| match Bell::new() {
                Ok(bell) => Some(bell),
                Err(error) => {
                    aterm_log::warn!(
                        "main-thread stall watchdog: no doorbell ({error}); it samples on its \
                         interval at rest too"
                    );
                    None
                }
            })
            .as_ref()
    }

    /// The sampler's claim to rest: true when the breadcrumb, read after the claim was made, is
    /// one it may rest at ([`rests_at`]), so the next beat rings; false (and the claim withdrawn)
    /// otherwise.
    fn claim_rest(&self, cpu_read: bool) -> bool {
        self.resting.store(true, Ordering::SeqCst);
        if rests_at(
            Breadcrumb::from_u8(self.breadcrumb.load(Ordering::SeqCst)),
            cpu_read,
        ) {
            return true;
        }
        self.resting.store(false, Ordering::SeqCst);
        false
    }

    /// A beat's half of the claim: ring the resting sampler once.
    fn ring_if_resting(&self) {
        if self.resting.load(Ordering::SeqCst) && self.resting.swap(false, Ordering::SeqCst) {
            #[cfg(any(unix, windows))]
            if let Some(bell) = self.bell.get().and_then(Option::as_ref) {
                bell.ring();
            }
        }
    }

    fn current(&self) -> Breadcrumb {
        Breadcrumb::from_u8(self.breadcrumb.load(Ordering::Relaxed))
    }

    fn parked(&self) -> bool {
        Breadcrumb::from_u8(self.breadcrumb.load(Ordering::Acquire)).is_park_point()
    }

    fn wait_for_park(&self, bound: Duration) -> bool {
        let now = std::time::Instant::now();
        let until = now
            .checked_add(bound)
            .unwrap_or_else(|| now + Duration::from_secs(365 * 24 * 3600));
        loop {
            if self.parked() {
                return true;
            }
            if std::time::Instant::now() >= until {
                return false;
            }
            std::thread::sleep(PARK_POLL);
        }
    }
}

static MAIN: MainWatch = MainWatch::new();

static PHASE: AtomicU8 = AtomicU8::new(Phase::None as u8);

label_enum! {
    /// The main-loop roots the watchdog can pin a stall to.
    #[repr(u8)]
    pub(crate) enum Breadcrumb {
        /// Before the event loop runs (synchronous launch).
        Startup => "startup",
        /// The loop has finished `about_to_wait`'s work and is parked in the OS
        /// event wait (stamped as that handler returns, [`enter_about_to_wait`]).
        AboutToWait => "idle",
        /// `window_event`: OS-delivered input/resize/close for a window.
        WindowEvent => "window_event",
        /// `user_event`: a proxy `Wake` (control socket, config reload, …).
        UserEvent => "user_event",
        /// `new_events`: a `WaitUntil` deadline fired (blink / bell / resize settle).
        NewEvents => "new_events",
        /// The resize-settle flush — the coalesced final width lands here, the exact
        /// path that funnels into `Grid::resize`'s width-reflow sink.
        ResizeSettle => "resize_settle",
        /// The overlap-handoff park + readiness wait: the main thread deliberately
        /// blocks (bounded) while the update child boots under the frozen frame,
        /// beating every poll tick.
        UpdateHandoff => "update_handoff",
        /// A NESTED MODAL RUN LOOP spun from inside a work root — `-[NSAlert
        /// runModal]` (`menu::confirm`) and `-[NSOpenPanel
        /// runModal]` (`menu::choose_local_file`).
        Modal => "modal",
        /// `about_to_wait`'s BODY: the message settle, the restore pass, the crash
        /// journal's capture, the update reconcile and every deadline fold, run on
        /// every loop iteration before the wait.
        AboutToWaitWork => "about_to_wait",
        /// The event loop is ending (an exit was asked, after which winit beats no
        /// `NewEvents`) or has ended, and teardown is waiting for accepted work to
        /// become durable.
        ShutdownWait => "shutdown_wait",
    }
}

impl Breadcrumb {
    /// The breadcrumb a `u8` cell holds as its [`index`](Self::index).
    pub(crate) fn from_u8(v: u8) -> Self {
        Self::from_index(usize::from(v)).unwrap_or(Self::Startup)
    }

    const fn spin_is_a_stall(self) -> bool {
        !matches!(
            self,
            Breadcrumb::Startup | Breadcrumb::UpdateHandoff | Breadcrumb::Modal
        )
    }

    const fn is_park_point(self) -> bool {
        matches!(
            self,
            Breadcrumb::Startup
                | Breadcrumb::AboutToWait
                | Breadcrumb::UpdateHandoff
                | Breadcrumb::Modal
                | Breadcrumb::ShutdownWait
        )
    }
}

fn is_stall_at(bc: Breadcrumb, frozen: Duration, threshold: Duration) -> bool {
    !bc.is_park_point() && frozen >= threshold
}

/// Whether the sampler may rest while the main thread is at `bc` (`cpu_read`: the main thread's
/// CPU time can be read, which is what judges a spin at a park point): only where nothing could
/// be reported until the main thread beats again. That is a park point where a spin is no stall
/// (a designed freeze), any park point when there is no CPU reading (nothing at a park point is
/// judged without one), and `about_to_wait`'s park, where the main thread is in its run loop's
/// kernel wait: winit's `BeforeWaiting` observer is the last to run before that wait, and its
/// `AfterWaiting` observer the first after it, which beats `NewEvents`. `ShutdownWait` stays
/// judged: the loop is ending or has ended, and nothing beats while teardown waits (an exit
/// asked inside `about_to_wait` parks there, [`RootGuard::loop_ending`]).
const fn rests_at(bc: Breadcrumb, cpu_read: bool) -> bool {
    bc.is_park_point()
        && (!cpu_read || !bc.spin_is_a_stall() || matches!(bc, Breadcrumb::AboutToWait))
}

const MAIN_STALL_BAR: Duration = RELEASE_STALL_THRESHOLD;

/// A main thread that cannot take a verb now, as [`main_stall`] reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MainStall {
    /// How long the main thread has gone without a beat while it had work.
    pub(crate) stalled: Duration,
    /// The root the heartbeat last named.
    pub(crate) root: Breadcrumb,
    /// That root's handler had returned: the thread is in AppKit or CoreFoundation, outside every aterm handler.
    pub(crate) returned: bool,
}

impl std::fmt::Display for MainStall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "main thread stalled {}s; retry", self.stalled.as_secs())
    }
}

fn main_stall(
    now_ns: u64,
    last_beat_ns: u64,
    root: Breadcrumb,
    returned: bool,
    hop_since_ns: Option<u64>,
) -> Option<MainStall> {
    if last_beat_ns == 0 {
        return None;
    }
    let quiet = Duration::from_nanos(now_ns.saturating_sub(last_beat_ns));
    let stalled = if is_stall_at(root, quiet, MAIN_STALL_BAR) {
        quiet
    } else {
        let since = hop_since_ns.filter(|_| root.spin_is_a_stall())?;
        let waited = Duration::from_nanos(now_ns.saturating_sub(since.max(last_beat_ns)));
        if waited < MAIN_STALL_BAR {
            return None;
        }
        waited
    };
    Some(MainStall {
        stalled,
        root,
        returned,
    })
}

/// [`main_stall`] now, for a control worker about to post a main-thread hop, with `hop_since_ns` from the caller's own hop count (`control_media`).
pub(crate) fn main_stall_now(hop_since_ns: Option<u64>) -> Option<MainStall> {
    let root = Breadcrumb::from_u8(MAIN.breadcrumb.load(Ordering::Acquire));
    let last_beat_ns = TURNS.last_beat_ns.load(Ordering::Relaxed);
    let returned = MAIN.returned.load(Ordering::Relaxed);
    main_stall(
        crate::metrics::now_ns(),
        last_beat_ns,
        root,
        returned,
        hop_since_ns,
    )
}

/// A stall the sampler decided to report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Hit {
    /// The root the heartbeat last named.
    root: Breadcrumb,
    /// How long the stall has lasted: since the heartbeat last advanced, or,
    /// for a spin at a park point, since the main thread went on-CPU.
    frozen: Duration,
    /// The main thread was ON-CPU across the samples leading here — a spin,
    /// not a park on a lock.
    busy: bool,
}

const BUSY_PERCENT: u128 = 80;

struct Sampler {
    /// The heartbeat value last observed to advance.
    last_beat: u64,
    /// When the heartbeat last advanced (start of the current frozen span).
    last_advance: Instant,
    /// Whether the current contiguous stall was already reported (fire once).
    reported: bool,
    /// When the current contiguous stall was last reported, for the repeat
    /// cadence.
    last_report: Option<Instant>,
    /// How many times this contiguous stall has been reported.
    reports: u32,
    /// The bar this sampler judges against — [`STALL_THRESHOLD`] for the dev
    /// lane, [`RELEASE_STALL_THRESHOLD`] for a shipped binary.
    threshold: Duration,
    /// The previous sample's `(instant, main-thread CPU time)`.
    cpu_sample: Option<(Instant, Duration)>,
    /// Since when every sample found the main thread on-CPU for at least
    /// [`BUSY_PERCENT`] of the interval with no heartbeat in between.
    busy_since: Option<Instant>,
    /// The reported stall that has not ended yet, until the main thread moves (a heartbeat,
    /// or a CPU reading that finds a spin reported at a park point quiet).
    open: Option<OpenStall>,
    /// A reported stall that has since ended, waiting for [`Sampler::take_ended`].
    ended: Option<Ended>,
}

/// A reported stall that has not ended yet ([`Sampler::open`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OpenStall {
    /// The root it was first reported at.
    root: Breadcrumb,
    /// When it began: the last heartbeat, or, for a spin at a park point, when
    /// the main thread went on-CPU.
    since: Instant,
}

/// A reported stall that ended, as [`Sampler::take_ended`] hands it out once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ended {
    /// How long it lasted, a process gap not counted.
    frozen: Duration,
    /// The root it was first reported at.
    root: Breadcrumb,
    /// What showed it over.
    by: EndedBy,
}

/// The evidence that a reported stall is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EndedBy {
    /// The heartbeat moved: the main thread entered a root again.
    Beat,
    /// A spin reported at a park point went quiet.
    Quiet,
}

impl Sampler {
    const fn with_threshold(now: Instant, beat: u64, threshold: Duration) -> Self {
        Self {
            last_beat: beat,
            last_advance: now,
            reported: false,
            last_report: None,
            reports: 0,
            threshold,
            cpu_sample: None,
            busy_since: None,
            open: None,
            ended: None,
        }
    }

    fn resync(&mut self, now: Instant, beat: u64, gap: Duration) {
        let moved = beat != self.last_beat;
        let open = self.open.take().map(|stall| OpenStall {
            since: stall
                .since
                .checked_add(gap)
                .map_or(now, |since| since.min(now)),
            ..stall
        });
        let ended = self.ended.take();
        *self = Self::with_threshold(now, beat, self.threshold);
        self.open = open;
        self.ended = ended;
        if moved {
            self.end_open(now, EndedBy::Beat);
        }
    }

    /// The machine was suspended for `slept` of the span since the last wake, and the clock
    /// this sampler judges on (std's `Instant`) counted it (Windows): every instant it holds
    /// moves that much later, so no span it measures holds the suspend.
    #[cfg(any(windows, test))]
    fn skip(&mut self, slept: Duration) {
        let later = |at: Instant| at.checked_add(slept).unwrap_or(at);
        self.last_advance = later(self.last_advance);
        self.last_report = self.last_report.map(later);
        self.cpu_sample = self.cpu_sample.map(|(at, cpu)| (later(at), cpu));
        self.busy_since = self.busy_since.map(later);
        self.open = self.open.map(|stall| OpenStall {
            since: later(stall.since),
            ..stall
        });
    }

    const fn take_ended(&mut self) -> Option<Ended> {
        self.ended.take()
    }

    /// Nothing of this sampler's own is pending: no reported stall waits for its end, and no spin
    /// is being timed.
    const fn settled(&self) -> bool {
        self.open.is_none() && self.ended.is_none() && self.busy_since.is_none()
    }

    fn wake(
        &mut self,
        now: Instant,
        requested: Duration,
        elapsed: Elapsed,
        beat: u64,
        bc: Breadcrumb,
        cpu: Option<Duration>,
    ) -> SamplerWake {
        let gap = classify_gap(requested, elapsed);
        if let Some(late) = gap.and_then(|gap| gap.unscheduled) {
            self.resync(now, beat, late);
            return SamplerWake {
                gap,
                ended: self.take_ended(),
                hit: None,
            };
        }
        let hit = self.poll_with(now, beat, bc, cpu);
        SamplerWake {
            gap,
            ended: self.take_ended(),
            hit,
        }
    }

    /// The heartbeat rule alone, with no CPU reading (as on a platform without the probe).
    #[cfg(test)]
    fn poll(&mut self, now: Instant, cur_beat: u64, bc: Breadcrumb) -> Option<Breadcrumb> {
        self.poll_with(now, cur_beat, bc, None).map(|hit| hit.root)
    }

    fn poll_with(
        &mut self,
        now: Instant,
        cur_beat: u64,
        bc: Breadcrumb,
        cpu: Option<Duration>,
    ) -> Option<Hit> {
        let judged = self.fold_cpu(now, cpu);
        if cur_beat != self.last_beat {
            self.end_open(now, EndedBy::Beat);
            self.last_beat = cur_beat;
            self.last_advance = now;
            self.clear_reports();
            self.busy_since = None;
            return None;
        }
        if bc.is_park_point() {
            match self.busy_since.filter(|_| bc.spin_is_a_stall()) {
                Some(since) => {
                    let spun = now.saturating_duration_since(since);
                    if spun < self.threshold || !self.report_due(now) {
                        return None;
                    }
                    return Some(self.open_stall(
                        Hit {
                            root: bc,
                            frozen: spun,
                            busy: true,
                        },
                        since,
                    ));
                }
                None => {
                    // Idle / startup park: a frozen heartbeat is expected, so keep the
                    // clock reset. A spin reported here ends once a reading finds the thread
                    // quiet (with no reading, at the next beat). A stall reported at a work
                    // root is not ended here: a park point under its unmoved heartbeat is the
                    // next root's breadcrumb read before its beat, and that beat ends it.
                    if judged == Some(false)
                        && self.open.is_some_and(|stall| stall.root.is_park_point())
                    {
                        self.end_open(now, EndedBy::Quiet);
                    }
                    self.last_advance = now;
                    self.clear_reports();
                    return None;
                }
            }
        }
        let frozen = now.saturating_duration_since(self.last_advance);
        if !is_stall_at(bc, frozen, self.threshold) || !self.report_due(now) {
            return None;
        }
        let since = self.last_advance;
        Some(self.open_stall(
            Hit {
                root: bc,
                frozen,
                busy: self.busy_since.is_some(),
            },
            since,
        ))
    }

    fn open_stall(&mut self, hit: Hit, since: Instant) -> Hit {
        self.open.get_or_insert(OpenStall {
            root: hit.root,
            since,
        });
        hit
    }

    fn end_open(&mut self, now: Instant, by: EndedBy) {
        if let Some(stall) = self.open.take() {
            self.ended = Some(Ended {
                frozen: now.saturating_duration_since(stall.since),
                root: stall.root,
                by,
            });
        }
    }

    fn fold_cpu(&mut self, now: Instant, cpu: Option<Duration>) -> Option<bool> {
        let Some(cpu) = cpu else {
            self.cpu_sample = None;
            self.busy_since = None;
            return None;
        };
        let mut judged = None;
        if let Some((then, was)) = self.cpu_sample {
            let wall = now.saturating_duration_since(then).as_nanos();
            let used = cpu.saturating_sub(was).as_nanos();
            let busy = wall > 0 && used * 100 >= wall * BUSY_PERCENT;
            if busy {
                self.busy_since.get_or_insert(then);
            } else {
                self.busy_since = None;
            }
            judged = Some(busy);
        }
        self.cpu_sample = Some((now, cpu));
        judged
    }

    fn report_due(&mut self, now: Instant) -> bool {
        let due = match self.last_report {
            None => !self.reported,
            Some(at) => now.saturating_duration_since(at) >= STALL_REPEAT_INTERVAL,
        };
        if due {
            self.reported = true;
            self.last_report = Some(now);
            self.reports = self.reports.saturating_add(1);
        }
        due
    }

    const fn clear_reports(&mut self) {
        self.reported = false;
        self.last_report = None;
        self.reports = 0;
    }
}

/// What one [`Sampler::wake`] decided, before anything is logged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SamplerWake {
    /// The wake was late on either clock ([`classify_gap`]).
    gap: Option<Gap>,
    /// A reported stall that ended by this wake.
    ended: Option<Ended>,
    /// A stall to report now.
    hit: Option<Hit>,
}

impl SamplerWake {
    fn notices(&self) -> Vec<String> {
        self.gap
            .and_then(|gap| {
                gap.unscheduled
                    .map(|late| gap_message(late, gap.wall, gap.pass))
            })
            .into_iter()
            .chain(self.ended.map(stall_ended_message))
            .collect()
    }

    const fn wall_moved(&self) -> bool {
        matches!(
            self.gap,
            Some(Gap {
                wall: Wall::Slept(_),
                ..
            })
        )
    }

    fn sleep_note(&self) -> Option<String> {
        match self.gap {
            Some(Gap {
                unscheduled: None,
                wall: Wall::Slept(slept),
                ..
            }) => Some(slept_message(slept)),
            _ => None,
        }
    }
}

const PROCESS_GAP: Duration = Duration::from_secs(30);

fn process_gap(requested: Duration, observed: Duration) -> Option<Duration> {
    let late = observed.saturating_sub(requested);
    (late >= PROCESS_GAP).then_some(late)
}

/// How long one sampler wake was gone, on both clocks, since the wake before
/// it — so the report pass between two sleeps is inside the span, the stack
/// capture included ([`Clocks`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Elapsed {
    /// On the monotonic clock ([`Instant`]), which stops while the Mac sleeps
    /// (on Apple platforms std reads `CLOCK_UPTIME_RAW`).
    mono: Duration,
    /// On the wall clock ([`SystemTime`]), which keeps running while the Mac
    /// sleeps; `None` when it went backwards (the clock was set back).
    wall: Option<Duration>,
    /// Of `mono`, the report pass before the sleep: the stall lines, the
    /// stack capture and the stack lines when the wake before this one
    /// reported, next to nothing otherwise.
    pass: Duration,
}

/// Both clocks, read together at one sampler wake.
#[derive(Clone, Copy)]
struct Clocks {
    mono: Instant,
    wall: SystemTime,
}

impl Clocks {
    fn now() -> Self {
        Self {
            mono: Instant::now(),
            wall: SystemTime::now(),
        }
    }

    fn since(self, earlier: Self, asleep: Instant) -> Elapsed {
        Elapsed {
            mono: self.mono.saturating_duration_since(earlier.mono),
            wall: self.wall.duration_since(earlier.wall).ok(),
            pass: asleep.saturating_duration_since(earlier.mono),
        }
    }
}

/// What the wall clock says about a span the monotonic clock measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wall {
    /// It ran at least [`PROCESS_GAP`] further than the monotonic clock, which stops while the Mac sleeps.
    Slept(Duration),
    /// It ran no more than that further: the Mac was awake throughout.
    Awake,
    /// It went backwards (the clock was set back), so it says nothing.
    Unknown,
}

/// A sampler wake that was late on either clock ([`classify_gap`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Gap {
    /// Time the system was awake and this process did not run, when it is a
    /// [`PROCESS_GAP`]: the sampler resyncs over it.
    unscheduled: Option<Duration>,
    /// What the wall clock says about the same span.
    wall: Wall,
    /// How much of the span was the report pass before the sleep
    /// ([`Elapsed::pass`]).
    pass: Duration,
}

fn classify_gap(requested: Duration, elapsed: Elapsed) -> Option<Gap> {
    let unscheduled = process_gap(requested, elapsed.mono);
    let wall = match elapsed.wall {
        None => Wall::Unknown,
        Some(wall) => match wall.checked_sub(elapsed.mono) {
            Some(ahead) if ahead >= PROCESS_GAP => Wall::Slept(ahead),
            _ => Wall::Awake,
        },
    };
    (unscheduled.is_some() || matches!(wall, Wall::Slept(_))).then_some(Gap {
        unscheduled,
        wall,
        pass: elapsed.pass,
    })
}

const PASS_WORTH_NAMING: Duration = Duration::from_secs(1);

fn gap_message(late: Duration, wall: Wall, pass: Duration) -> String {
    let secs = late.as_secs();
    let pass = if pass >= PASS_WORTH_NAMING {
        format!(
            " {}s of it fell in the watchdog's own report pass (the stall lines and the stack \
             capture), where a log write stuck on the disk would read the same.",
            pass.as_secs()
        )
    } else {
        String::new()
    };
    if cfg!(windows) {
        // Windows: the span is awake time (`suspended`), and none of the Mac's causes apply.
        let wall = match wall {
            Wall::Slept(ahead) => format!(
                "The wall clock ran another {}s past that, so this PC also slept (or its \
                 clock was set forward).",
                ahead.as_secs()
            ),
            Wall::Awake => {
                "The wall clock ran no further, so this PC did not sleep through it.".to_owned()
            }
            Wall::Unknown => "The wall clock was set back meanwhile, so it cannot say whether \
                              this PC also slept."
                .to_owned(),
        };
        return format!(
            "this process was not running for {secs}s while the system was awake: it was not \
             scheduled (CPU starvation, or memory so short that it was paged out), or it was \
             suspended or paused under a debugger.{pass} {wall} The stall accounting starts \
             over here, so none of this is charged to the main thread. Every timer the \
             process had armed came due at once."
        );
    }
    let wall: String = match wall {
        Wall::Slept(slept) => format!(
            "The wall clock ran another {}s past that, so the Mac also slept (or its clock \
             was set forward).",
            slept.as_secs()
        ),
        Wall::Awake => "The wall clock ran no further, so the Mac did not sleep through it.".into(),
        Wall::Unknown => {
            "The wall clock was set back meanwhile, so it cannot say whether the Mac also \
             slept."
                .into()
        }
    };
    format!(
        "this process was not running for {secs}s while the system was awake: it was not \
         scheduled (App Nap or CPU starvation; on 2026-09-28 a window lost 3 h 22 m this way \
         on a Mac at load 45-151 with its swap full), or it was stopped (SIGSTOP), paused \
         under a debugger, or suspended.{pass} {wall} The stall accounting starts over here, so \
         none of this is charged to the main thread. Every timer the process had armed came \
         due at once; the 2026-09-26 freeze was CoreFoundation walking one such timer \
         forward after 26.6 hours of this."
    )
}

/// How much of a span std's `Instant` counted the machine was suspended for, where it counts a
/// suspend at all (Windows): `counted` less the time the machine was `awake` in it (the unbiased
/// interrupt time's run), or nothing when that was not read or is under [`SUSPEND_FLOOR`]. PURE.
#[cfg(any(windows, test))]
fn suspended(counted: Duration, awake: Option<Duration>) -> Duration {
    awake
        .map(|awake| counted.saturating_sub(awake))
        .filter(|slept| *slept >= SUSPEND_FLOOR)
        .unwrap_or_default()
}

/// The least suspend [`suspended`] names: under it the two clocks' own grain (the interrupt
/// time ticks at up to 15.6 ms) would read as one.
#[cfg(any(windows, test))]
const SUSPEND_FLOOR: Duration = Duration::from_secs(1);

/// How far the wall clock ran past a clock that counts a suspend as it does (`counted`: std's
/// `Instant` on Windows), when that is a [`PROCESS_GAP`] or more: the wall clock was set forward,
/// which no sleep explains there. `None` for a wall that ran no further, went backwards (`wall`
/// is `None`), or moved by less. PURE.
#[cfg(any(windows, test))]
fn stepped_forward(wall: Option<Duration>, counted: Duration) -> Option<Duration> {
    wall?
        .checked_sub(counted)
        .filter(|ahead| *ahead >= PROCESS_GAP)
}

#[cfg(any(windows, test))]
fn stepped_message(ahead: Duration) -> String {
    format!(
        "the wall clock was set forward by about {}s (read off this process's clocks: Windows \
         announces a clock set to windows only): wall-clock deadlines are derived again",
        ahead.as_secs()
    )
}

fn slept_message(slept: Duration) -> String {
    if cfg!(windows) {
        return format!(
            "this PC slept for about {}s: the wall clock ran that much further than the time \
             the system was awake. This process lost no awake time, so the stall accounting is \
             unchanged.",
            slept.as_secs()
        );
    }
    format!(
        "the Mac slept for about {}s: the wall clock ran that much further than the \
         monotonic clock, which stops in sleep (or the clock was set forward). This process \
         lost no awake time, so the stall accounting is unchanged.",
        slept.as_secs()
    )
}

const LONG_TURN_THRESHOLD_NS: u64 = crate::metrics::SLOW_FRAME_THRESHOLD_NS;

struct TurnLedger {
    /// When the CURRENT root was entered (`crate::metrics::now_ns` clock), for
    /// the window stats.
    open_ns: AtomicU64,
    /// When the main thread last entered a root, on the same clock: the heartbeat's
    /// timestamp [`main_stall`] reads.
    last_beat_ns: AtomicU64,
    /// The most recently booked turn.
    last_ns: AtomicU64,
    /// The worst booked turn since reset, the root that owned it, and when it ended.
    max_ns: AtomicU64,
    max_owner: AtomicU8,
    max_at_ns: AtomicU64,
    /// How many turns were booked, and how many at or over [`LONG_TURN_THRESHOLD_NS`].
    turns: AtomicU64,
    long_turns: AtomicU64,
    /// The booked turns' sum since reset, and when that window began (0: process start), on
    /// the same clock: `main_busy_ms_per_s`, the share of the window the main thread spent in
    /// aterm's work.
    busy_ns: AtomicU64,
    window_from_ns: AtomicU64,
    /// Each root's booked turns since reset, indexed by [`Breadcrumb::index`].
    by_root: [crate::metrics::Histogram; Breadcrumb::COUNT],
}

impl TurnLedger {
    const fn new() -> Self {
        Self {
            open_ns: AtomicU64::new(0),
            last_beat_ns: AtomicU64::new(0),
            last_ns: AtomicU64::new(0),
            max_ns: AtomicU64::new(0),
            max_owner: AtomicU8::new(Breadcrumb::Startup as u8),
            max_at_ns: AtomicU64::new(0),
            turns: AtomicU64::new(0),
            long_turns: AtomicU64::new(0),
            busy_ns: AtomicU64::new(0),
            window_from_ns: AtomicU64::new(0),
            by_root: [const { crate::metrics::Histogram::new() }; Breadcrumb::COUNT],
        }
    }

    fn attributable_span(previous: Breadcrumb, open_ns: u64, now_ns: u64) -> Option<u64> {
        if open_ns == 0 || previous.is_park_point() {
            return None;
        }
        Some(now_ns.saturating_sub(open_ns))
    }

    fn close(&self, previous: Breadcrumb, now_ns: u64) -> Option<u64> {
        let open = self.open_ns.swap(now_ns, Ordering::Relaxed);
        let span = Self::attributable_span(previous, open, now_ns)?;
        self.by_root[previous.index()].record(span);
        self.last_ns.store(span, Ordering::Relaxed);
        self.turns.fetch_add(1, Ordering::Relaxed);
        self.busy_ns.fetch_add(span, Ordering::Relaxed);
        if span >= LONG_TURN_THRESHOLD_NS {
            self.long_turns.fetch_add(1, Ordering::Relaxed);
        }
        if self.max_ns.fetch_max(span, Ordering::Relaxed) < span {
            self.max_owner.store(previous as u8, Ordering::Relaxed);
            self.max_at_ns.store(now_ns, Ordering::Relaxed);
        }
        Some(span)
    }

    fn snapshot(&self) -> TurnCensus {
        let turns = self.turns.load(Ordering::Relaxed);
        TurnCensus {
            busy_ns: self.busy_ns.load(Ordering::Relaxed),
            window_from_ns: self.window_from_ns.load(Ordering::Relaxed),
            last_ns: self.last_ns.load(Ordering::Relaxed),
            max_ns: self.max_ns.load(Ordering::Relaxed),
            max_owner: (turns != 0)
                .then(|| Breadcrumb::from_u8(self.max_owner.load(Ordering::Relaxed))),
            max_at_ns: self.max_at_ns.load(Ordering::Relaxed),
            turns,
            long_turns: self.long_turns.load(Ordering::Relaxed),
        }
    }

    fn root_rows(&self) -> Vec<(&'static str, u64, f64, f64, f64)> {
        let ms = |ns: u64| ns as f64 / 1e6;
        Breadcrumb::ALL
            .into_iter()
            .filter_map(|root| {
                let h = self.by_root[root.index()].copy_out();
                let n = h.n();
                (n != 0).then(|| {
                    let p = |q: f64| ms(h.percentile(q).unwrap_or(0));
                    (root.as_str(), n, p(0.50), p(0.95), p(0.99))
                })
            })
            .collect()
    }

    fn reset(&self) {
        self.window_from_ns
            .store(crate::metrics::now_ns(), Ordering::Relaxed);
        self.busy_ns.store(0, Ordering::Relaxed);
        self.open_ns.store(0, Ordering::Relaxed);
        self.last_ns.store(0, Ordering::Relaxed);
        self.max_ns.store(0, Ordering::Relaxed);
        self.max_owner
            .store(Breadcrumb::Startup as u8, Ordering::Relaxed);
        self.max_at_ns.store(0, Ordering::Relaxed);
        self.turns.store(0, Ordering::Relaxed);
        self.long_turns.store(0, Ordering::Relaxed);
        for h in &self.by_root {
            h.reset();
        }
    }
}

static TURNS: TurnLedger = TurnLedger::new();

/// A read of [`TURNS`] for the `metrics` verb.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TurnCensus {
    /// The most recently booked turn.
    pub last_ns: u64,
    /// The worst booked turn since the last reset.
    pub max_ns: u64,
    /// The root that owned the worst turn, or `None` when none was booked.
    pub max_owner: Option<Breadcrumb>,
    /// When the worst turn ended, on the `crate::metrics::now_ns` process clock.
    pub max_at_ns: u64,
    /// Turns booked since the last reset, and the subset at or over [`LONG_TURN_THRESHOLD_NS`].
    pub turns: u64,
    pub long_turns: u64,
    /// The booked turns' sum since the last reset, and when that window began (0: process start), on the `crate::metrics::now_ns` clock.
    pub busy_ns: u64,
    pub window_from_ns: u64,
}

impl TurnCensus {
    fn busy_ms_per_s(&self, now_ns: u64) -> f64 {
        let window_ns = now_ns.saturating_sub(self.window_from_ns);
        if window_ns == 0 {
            return 0.0;
        }
        self.busy_ns as f64 * 1e3 / window_ns as f64
    }
}

/// `main_busy_ms_per_s` at `now_ns`, for the `metrics percentiles` line (the summary line carries it in [`emit_turn_census`]).
#[must_use]
pub(crate) fn main_busy_ms_per_s(now_ns: u64) -> f64 {
    turn_census().busy_ms_per_s(now_ns)
}

/// Record that the main thread just entered `bc`, and price the turn that ended.
#[inline]
pub(crate) fn beat(bc: Breadcrumb) {
    beat_at(bc, crate::metrics::now_ns());
}

/// [`beat`] with the clock passed in, returning the span it booked (a real clock would make
/// any assertion about a span a race).
#[inline]
fn beat_at(bc: Breadcrumb, now_ns: u64) -> Option<u64> {
    beat_into(&MAIN, &TURNS, &PHASE, bc, now_ns)
}

/// The beat path with its ledger and phase cell passed in, so the stamp-close-clear-bump
/// sequence is testable on cells no other test writes and no `metrics reset` clears
/// mid-assertion.
#[inline]
fn beat_into(
    main: &MainWatch,
    turns: &TurnLedger,
    phase: &AtomicU8,
    bc: Breadcrumb,
    now_ns: u64,
) -> Option<u64> {
    turns.last_beat_ns.store(now_ns.max(1), Ordering::Relaxed);
    let previous = Breadcrumb::from_u8(main.breadcrumb.swap(bc as u8, Ordering::SeqCst));
    let booked = turns.close(previous, now_ns);
    phase.store(Phase::None as u8, Ordering::Relaxed);
    main.returned.store(false, Ordering::Relaxed);
    main.heartbeat.fetch_add(1, Ordering::Relaxed);
    main.ring_if_resting();
    booked
}

/// Enter a winit root: [`beat`] now, and mark the root returned when the guard drops at the end of the handler, so a stall after it (in AppKit or CoreFoundation) is not reported as inside it.
#[must_use = "the root lasts only as long as the guard lives"]
pub(crate) fn enter(bc: Breadcrumb) -> RootGuard {
    enter_in(&MAIN, bc)
}

fn enter_in(main: &'static MainWatch, bc: Breadcrumb) -> RootGuard {
    beat_on(main, bc);
    RootGuard { main, park: None }
}

fn beat_on(main: &MainWatch, bc: Breadcrumb) {
    beat_into(main, &TURNS, &PHASE, bc, crate::metrics::now_ns());
}

/// Enter `about_to_wait`.
#[must_use = "the root lasts only as long as the guard lives"]
pub(crate) fn enter_about_to_wait() -> RootGuard {
    enter_about_to_wait_in(&MAIN)
}

fn enter_about_to_wait_in(main: &'static MainWatch) -> RootGuard {
    beat_on(main, Breadcrumb::AboutToWaitWork);
    RootGuard {
        main,
        park: Some(Breadcrumb::AboutToWait),
    }
}

/// The RAII half of [`enter`] and [`enter_about_to_wait`].
pub(crate) struct RootGuard {
    /// The watch the root was entered on.
    main: &'static MainWatch,
    /// The park point beaten on drop, as the handler returns (closing the handler's turn);
    /// `None`: the root's turn runs on to the next root's beat.
    park: Option<Breadcrumb>,
}

impl RootGuard {
    /// The event loop is exiting: park at `ShutdownWait` (judged) as the handler returns, not at
    /// the root's own park. winit's `AfterWaiting` returns before `NewEvents` once it is exiting,
    /// so no beat would end a rest at `about_to_wait`'s park.
    pub(crate) const fn loop_ending(&mut self) {
        if self.park.is_some() {
            self.park = Some(Breadcrumb::ShutdownWait);
        }
    }
}

impl Drop for RootGuard {
    fn drop(&mut self) {
        if let Some(park) = self.park {
            beat_on(self.main, park);
        }
        self.main.returned.store(true, Ordering::Relaxed);
    }
}

/// The turn census, for the `metrics` verb.
#[must_use]
pub(crate) fn turn_census() -> TurnCensus {
    TURNS.snapshot()
}

/// Clear the turn census, its per-root distributions included.
pub(crate) fn reset_turn_census() {
    TURNS.reset();
}

/// The turn-census fields of the `metrics` summary.
pub(crate) fn emit_turn_census(
    ctx: &crate::metrics::wire::ReadCtx,
    s: &mut dyn crate::metrics::wire::FieldSink,
) {
    use crate::metrics::wire::{ms, ms_f64, ms1};
    let c = turn_census();
    s.decimal("max_turn_ms", ms(c.max_ns));
    s.label(
        "max_turn_owner",
        c.max_owner.map_or("none", Breadcrumb::as_str),
    );
    s.decimal("max_turn_at_ms", ms(c.max_at_ns));
    s.decimal("last_turn_ms", ms(c.last_ns));
    s.count("turns", c.turns);
    s.count("long_turns", c.long_turns);
    s.decimal("long_turn_threshold_ms", ms1(LONG_TURN_THRESHOLD_NS));
    s.decimal("main_busy_ms_per_s", ms_f64(c.busy_ms_per_s(ctx.now_ns)));
}

/// `turns_by_root` for `metrics percentiles`.
pub(crate) fn emit_turns_by_root(s: &mut dyn crate::metrics::wire::FieldSink) {
    use crate::metrics::wire::{Cell, ms_f64};
    let rows = TURNS.root_rows();
    let records: Vec<[(&str, Cell<'_>); 4]> = rows
        .iter()
        .map(|&(_, n, p50, p95, p99)| {
            [
                ("n", Cell::Count(n)),
                ("p50_ms", Cell::Decimal(ms_f64(p50))),
                ("p95_ms", Cell::Decimal(ms_f64(p95))),
                ("p99_ms", Cell::Decimal(ms_f64(p99))),
            ]
        })
        .collect();
    let pairs: Vec<(&str, Cell<'_>)> = rows
        .iter()
        .zip(&records)
        .map(|(row, record)| (row.0, Cell::Rec(record)))
        .collect();
    s.pairs("turns_by_root", &pairs, "none");
}

const PARK_POLL: Duration = Duration::from_micros(50);

thread_local! {
    static FORK_PARK_BOUND: std::cell::Cell<Duration> = const { std::cell::Cell::new(Duration::ZERO) };
}

/// Run `spawn` with each fork this thread makes inside it waiting at most `bound` for the main thread to park, at the fork itself ([`park_before_fork`], just before the PTY spawn).
pub(crate) fn forks_wait_for_park<R>(bound: Duration, spawn: impl FnOnce() -> R) -> R {
    struct Restore(Duration);
    impl Drop for Restore {
        fn drop(&mut self) {
            FORK_PARK_BOUND.set(self.0);
        }
    }
    let _restore = Restore(FORK_PARK_BOUND.replace(bound));
    spawn()
}

/// At a fork: wait for the main thread to park within this thread's [`forks_wait_for_park`] bound.
pub(crate) fn park_before_fork() -> bool {
    park_before_fork_on(&MAIN)
}

fn park_before_fork_on(main: &MainWatch) -> bool {
    let bound = FORK_PARK_BOUND.get();
    !bound.is_zero() && main.wait_for_park(bound)
}

/// The breadcrumb the main thread last stamped (macOS's dialog refusal; the tests read their own watch).
#[cfg(target_os = "macos")]
pub(crate) fn current() -> Breadcrumb {
    MAIN.current()
}

/// Park the watchdog for a nested modal run loop.
#[must_use = "the park lasts only as long as the guard lives"]
#[cfg(target_os = "macos")]
pub(crate) fn park_modal() -> ModalPark {
    park_modal_in(&MAIN)
}

/// [`park_modal`] on `main`.
#[cfg(any(target_os = "macos", test))]
fn park_modal_in(main: &'static MainWatch) -> ModalPark {
    let previous = main.current();
    beat_on(main, Breadcrumb::Modal);
    ModalPark {
        main,
        previous,
        _main_thread: std::marker::PhantomData,
    }
}

/// The RAII half of [`park_modal`].
#[cfg(any(target_os = "macos", test))]
pub(crate) struct ModalPark {
    main: &'static MainWatch,
    previous: Breadcrumb,
    _main_thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg(any(target_os = "macos", test))]
impl Drop for ModalPark {
    fn drop(&mut self) {
        beat_on(self.main, self.previous);
    }
}

/// The work a stall line can name inside a root: which of the many things `user_event` does it was doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Phase {
    /// Nothing announced: the root is all the line says.
    None = 0,
    /// `App::ensure_pixel_backend`.
    PixelBackendRedeem = 1,
    /// `App::render_image`: the `image` verb's capture on the main thread.
    ImageCapture = 2,
}

impl Phase {
    const fn from_u8(v: u8) -> Self {
        match v {
            1 => Phase::PixelBackendRedeem,
            2 => Phase::ImageCapture,
            _ => Phase::None,
        }
    }

    /// Stable, symbol-free wording for the log line.
    pub(crate) const fn describe(self) -> &'static str {
        match self {
            Phase::None => "",
            Phase::PixelBackendRedeem => {
                "the headless pixel-backend redemption (the deferred font seal and its chrome \
                 re-sync, the face fork, and the GPU context with its font thread)"
            }
            Phase::ImageCapture => {
                "an `image` capture (headless with a GPU intent, the capture that is the run's \
                 first pixel demand also mints the GPU device and compiles its shaders)"
            }
        }
    }

    fn clause(self) -> String {
        match self {
            Phase::None => String::new(),
            named => format!(", in {}", named.describe()),
        }
    }
}

/// Announce the work the main thread is about to do inside the current root, for the guard's lifetime.
#[must_use = "the announcement lasts only as long as the guard lives"]
pub(crate) fn phase(p: Phase) -> PhaseGuard {
    announce(&PHASE, p)
}

fn announce(cell: &'static AtomicU8, p: Phase) -> PhaseGuard {
    let previous = phase_in(cell);
    cell.store(p as u8, Ordering::Relaxed);
    PhaseGuard {
        cell,
        written: p,
        previous,
    }
}

/// The phase the main thread last announced (none between roots).
pub(crate) fn current_phase() -> Phase {
    phase_in(&PHASE)
}

fn phase_in(cell: &AtomicU8) -> Phase {
    Phase::from_u8(cell.load(Ordering::Relaxed))
}

/// The RAII half of [`phase`]: the cell it wrote, what it wrote there, and what stood before.
pub(crate) struct PhaseGuard {
    cell: &'static AtomicU8,
    written: Phase,
    previous: Phase,
}

impl Drop for PhaseGuard {
    fn drop(&mut self) {
        let _ = self.cell.compare_exchange(
            self.written as u8,
            self.previous as u8,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }
}

/// Where the main thread was when a stall was reported, as far as the
/// heartbeat can tell: inside the root it last entered, or back in AppKit /
/// CoreFoundation after that root RETURNED ([`MainWatch`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Locus {
    /// The root's handler is still on the stack.
    Inside,
    /// The root's handler returned; no aterm handler is running.
    Returned,
}

fn stall_message(
    reports: u32,
    frozen: Duration,
    bc: Breadcrumb,
    phase: Phase,
    locus: Locus,
    busy: bool,
) -> String {
    let cpu = if busy {
        ", ON-CPU the whole time (a spin, not a park)"
    } else {
        ""
    };
    match (locus, reports) {
        (Locus::Inside, 1) => format!(
            "MAIN-THREAD STALL: no heartbeat for {frozen:?} while inside `{}`{}{cpu} — \
             the UI is not responding. Either unbounded work under a contended lock \
             (the L0 freeze hazard) or a park that will never end (a lock or lazy-init \
             cycle). This line names the main-loop root without symbols; a hang report \
             is not required to find it.",
            bc.ident(),
            phase.clause()
        ),
        (Locus::Inside, _) => format!(
            "MAIN-THREAD STALL CONTINUES: still no heartbeat after {frozen:?} inside \
             `{}`{}{cpu} — this is a wedge, not a slow frame.",
            bc.ident(),
            phase.clause()
        ),
        (Locus::Returned, 1) => format!(
            "MAIN-THREAD STALL: no heartbeat for {frozen:?} since `{}` returned{cpu} — \
             the main thread is outside every aterm handler, in AppKit or CoreFoundation's \
             own run-loop work (a timer, source or observer; the 2026-09-26 freeze was \
             CoreFoundation's catch-up for a late timer), and the UI is not responding. \
             The stack lines that follow name the frame.",
            bc.ident()
        ),
        (Locus::Returned, _) => format!(
            "MAIN-THREAD STALL CONTINUES: still no heartbeat after {frozen:?} since `{}` \
             returned{cpu} — the main thread is still outside aterm's handlers.",
            bc.ident()
        ),
    }
}

fn stall_ended_message(ended: Ended) -> String {
    let Ended { frozen, root, by } = ended;
    match by {
        EndedBy::Beat => format!(
            "MAIN-THREAD STALL ENDED: the main thread beat again after about {frozen:?} \
             stalled at `{}`.",
            root.ident()
        ),
        EndedBy::Quiet => format!(
            "MAIN-THREAD STALL ENDED: the main thread stopped spinning at `{}` after about \
             {frozen:?} on-CPU and is parked in the event wait again.",
            root.ident()
        ),
    }
}

const STACK_LINE: &str = "MAIN-THREAD STALL stack:";

const FRAME_SEPARATOR: &str = " | ";

fn stack_lines(frames: &[String], captured: SystemTime, cap: usize) -> Vec<String> {
    let captured = epoch_stamp(captured);
    let header = |part: usize, parts: usize| {
        format!("{STACK_LINE} (part {part}/{parts}, captured at {captured}) ")
    };
    let widest = frames.len().max(1);
    let budget = cap.saturating_sub(logged_len(&header(widest, widest)));
    let mut bodies = Vec::new();
    let mut body = String::new();
    let mut used = 0usize;
    let mut in_body = 0usize;
    for frame in frames {
        let frame = clip(frame, budget);
        let len = logged_len(&frame);
        if in_body > 0 && used + FRAME_SEPARATOR.len() + len > budget {
            bodies.push(std::mem::take(&mut body));
            used = 0;
            in_body = 0;
        }
        if in_body > 0 {
            body.push_str(FRAME_SEPARATOR);
            used += FRAME_SEPARATOR.len();
        }
        body.push_str(&frame);
        used += len;
        in_body += 1;
    }
    if in_body > 0 {
        bodies.push(body);
    }
    let parts = bodies.len();
    bodies
        .iter()
        .enumerate()
        .map(|(i, body)| format!("{}{body}", header(i + 1, parts)))
        .collect()
}

fn epoch_stamp(at: SystemTime) -> String {
    match at.duration_since(UNIX_EPOCH) {
        Ok(since) => format!("{}.{:03}", since.as_secs(), since.subsec_millis()),
        Err(_) => "an unknown time (the clock reads before 1970)".into(),
    }
}

fn logged_char_len(c: char) -> usize {
    if c.is_control() {
        char::REPLACEMENT_CHARACTER.len_utf8()
    } else {
        c.len_utf8()
    }
}

fn logged_len(s: &str) -> usize {
    s.chars().map(logged_char_len).sum()
}

fn clip(frame: &str, budget: usize) -> String {
    if logged_len(frame) <= budget {
        return frame.to_string();
    }
    let room = budget.saturating_sub('…'.len_utf8());
    let mut out = String::new();
    let mut used = 0usize;
    for c in frame.chars() {
        let len = logged_char_len(c);
        if used + len > room {
            break;
        }
        out.push(c);
        used += len;
    }
    out.push('…');
    out
}

fn seam() -> Option<String> {
    aterm_types::dev_seam!("ATERM_WATCHDOG").map(|v| v.to_string_lossy().trim().to_string())
}

fn enabled_for(seam: Option<&str>) -> bool {
    !seam.is_some_and(|v| {
        let v = v.trim();
        v.eq_ignore_ascii_case("off") || v.eq_ignore_ascii_case("0") || v.is_empty()
    })
}

fn enabled() -> bool {
    enabled_for(seam().as_deref())
}

const fn threshold_for(debug_assertions: bool, seam_present: bool) -> Duration {
    if debug_assertions || seam_present {
        STALL_THRESHOLD
    } else {
        RELEASE_STALL_THRESHOLD
    }
}

fn threshold() -> Duration {
    threshold_for(cfg!(debug_assertions), seam().is_some())
}

fn abort_on_stall_for(seam: Option<&str>) -> bool {
    seam.is_some_and(|v| v.trim().eq_ignore_ascii_case("abort"))
}

fn abort_on_stall() -> bool {
    abort_on_stall_for(seam().as_deref())
}

/// How the sampler judges: its bar, its pace, whether a stall aborts, and the main thread's
/// probes (a test's are inert).
#[derive(Clone, Copy)]
struct Judge {
    threshold: Duration,
    sample: Duration,
    abort: bool,
    cpu: fn() -> Option<Duration>,
    stack: fn() -> Option<Vec<String>>,
}

/// What the loop did, for a test's eyes ([`run`]); production passes a closure that ignores it
/// and always answers `true` (go on). A test's answers `false` to end the loop at its next wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seen {
    /// It is about to wait: with no deadline (`rest`), or until its next sample.
    Waits { rest: bool },
    /// It sampled the main thread once.
    Sampled,
    /// It reported a stall at this root.
    Hit(Breadcrumb),
    /// The wall moved, heard from the system or (with no wall watch) the clocks' drift.
    WallMoved,
}

/// Spawn the background stall sampler; call once, just before `run_app`. `on_wall_moved` runs
/// on its thread whenever the system announces a sleep's wake or a wall-clock step
/// ([`aterm_uds::wall_watch`]), also with the stall judging switched off (`ATERM_WATCHDOG=off`);
/// where the system announces neither, the sampler hears both by its clocks' drift as before.
pub(crate) fn start(on_wall_moved: impl Fn() + Send + 'static) {
    let judge = enabled().then(|| {
        crate::main_thread_probe::register();
        let threshold = threshold();
        Judge {
            threshold,
            sample: sample_interval(),
            abort: abort_on_stall(),
            cpu: crate::main_thread_probe::cpu_time,
            stack: crate::main_thread_probe::stack,
        }
    });
    #[cfg(any(unix, windows))]
    let wall = aterm_uds::wall_watch::WallWatch::start();
    #[cfg(not(any(unix, windows)))]
    let wall: Option<NoWallWatch> = None;
    if judge.is_none() && wall.is_none() {
        return;
    }
    let builder = std::thread::Builder::new().name("aterm-watchdog".into());
    let _ = builder.spawn(move || run(&MAIN, judge, wall.as_ref(), &on_wall_moved, &|_| true));
}

/// Where no wall watch exists (off Unix and Windows): never made.
#[cfg(not(any(unix, windows)))]
enum NoWallWatch {}

#[cfg(any(unix, windows))]
type WallSource = aterm_uds::wall_watch::WallWatch;
#[cfg(not(any(unix, windows)))]
type WallSource = NoWallWatch;

/// Whether the wall watch announces a step of the wall clock here
/// ([`aterm_uds::wall_watch::STEPS_ANNOUNCED`]); with no wall watch the sampler never rests.
#[cfg(any(unix, windows))]
const STEPS_ANNOUNCED: bool = aterm_uds::wall_watch::STEPS_ANNOUNCED;
#[cfg(not(any(unix, windows)))]
const STEPS_ANNOUNCED: bool = false;

/// The longest a rest waits where the wall watch announces no step of the wall clock (Windows,
/// [`STEPS_ANNOUNCED`]): each wake reads the two clocks for one (`stepped_forward`), so a step
/// set while the main thread is parked reaches `on_wall_moved` at most this late, for one wake
/// of the sampler's per this span while it rests.
const STEP_POLL: Duration = Duration::from_secs(10);

/// The deadline of a sampler wait that starts at `asleep`: the next sample (`sample` on) off a
/// rest; on a rest, none where `steps_announced` (the wall watch rings for a step) and
/// [`STEP_POLL`] on where it does not, so the clocks are read for a step that nothing would
/// ring for. PURE.
fn wait_until(
    rest: bool,
    asleep: Instant,
    sample: Duration,
    steps_announced: bool,
) -> Option<Instant> {
    if !rest {
        Some(asleep + sample)
    } else if steps_announced {
        None
    } else {
        asleep.checked_add(STEP_POLL)
    }
}

/// Wait until `until` (`None`: no deadline) or until the doorbell or the wall watch polls ready;
/// a failing poll, or nothing to poll, sleeps until `until` (or a `fallback` with no deadline),
/// never a spin. `true` when the wall watch may have news (it polled ready, or nothing was
/// polled), so a sample with nothing from it reads nothing of it.
fn wait(
    main: &MainWatch,
    wall: Option<&WallSource>,
    until: Option<Instant>,
    fallback: Duration,
) -> bool {
    #[cfg(unix)]
    {
        let mut fds = Vec::with_capacity(3);
        let bell = main.bell().map(aterm_uds::ready::Doorbell::fd);
        let first_wall = usize::from(bell.is_some());
        fds.extend(bell);
        fds.extend(wall.into_iter().flat_map(WallSource::fds));
        if !fds.is_empty()
            && let Ok(ready) = aterm_uds::ready::any(&fds, until)
        {
            return ready.iter().any(|&index| index >= first_wall);
        }
    }
    #[cfg(windows)]
    {
        let mut bells = Vec::with_capacity(2);
        let bell = main.bell();
        let first_wall = usize::from(bell.is_some());
        bells.extend(bell);
        bells.extend(wall.map(WallSource::bell));
        if !bells.is_empty()
            && let Ok(rung) = aterm_uds::wall_watch::any(&bells, until)
        {
            return rung.iter().any(|&index| index >= first_wall);
        }
    }
    #[cfg(not(any(unix, windows)))]
    let _ = (main, wall);
    let left = until.map_or(fallback, |until| {
        until.saturating_duration_since(Instant::now())
    });
    std::thread::sleep(left);
    true
}

/// The wall watch's news since the last look, logged; `true` when anything moved.
fn heard_wall(wall: Option<&WallSource>) -> bool {
    #[cfg(any(unix, windows))]
    {
        let Some(moved) = wall
            .map(WallSource::take)
            .filter(aterm_uds::wall_watch::Moved::any)
        else {
            return false;
        };
        match moved.slept {
            Some(slept) => aterm_log::info!("{}", slept_message(slept)),
            None if moved.woke => {
                aterm_log::info!(
                    "the system woke (or an idle sleep it pondered did not come): wall-clock \
                     deadlines are derived again"
                );
            }
            None => aterm_log::info!(
                "the wall clock was set (the system announced it): wall-clock deadlines are \
                 derived again"
            ),
        }
        true
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = wall;
        false
    }
}

/// The sampler's thread: with `judge`, the main thread judged off its rests ([`rests_at`]) every
/// [`Judge::sample`]; the wall watch heard throughout. Without `judge` it only hears the wall.
fn run(
    main: &'static MainWatch,
    judge: Option<Judge>,
    wall: Option<&WallSource>,
    on_wall_moved: &dyn Fn(),
    seen: &dyn Fn(Seen) -> bool,
) {
    let Some(judge) = judge else {
        if wall.is_none() {
            return;
        }
        // Windows: nothing announces a step, so each wake reads the clocks for one, as the
        // judging loop below does.
        #[cfg(windows)]
        let mut last = Clocks::now();
        while seen(Seen::Waits { rest: true }) {
            let asleep = Instant::now();
            let until = wait_until(true, asleep, Duration::ZERO, STEPS_ANNOUNCED);
            let heard = wait(main, wall, until, Duration::from_secs(60)) && heard_wall(wall);
            #[cfg(windows)]
            let stepped = {
                let here = Clocks::now();
                let elapsed = here.since(last, asleep);
                last = here;
                stepped_forward(elapsed.wall, elapsed.mono).filter(|_| !heard)
            };
            #[cfg(not(windows))]
            let stepped: Option<Duration> = None;
            #[cfg(windows)]
            if let Some(ahead) = stepped {
                aterm_log::info!("{}", stepped_message(ahead));
            }
            if heard || stepped.is_some() {
                let _ = seen(Seen::WallMoved);
                on_wall_moved();
            }
        }
        return;
    };
    let Judge {
        threshold,
        sample,
        abort,
        cpu,
        stack,
    } = judge;
    #[cfg(any(unix, windows))]
    let can_rest = wall.is_some() && main.bell().is_some();
    #[cfg(not(any(unix, windows)))]
    let can_rest = false;
    aterm_log::info!(
        "main-thread stall watchdog armed (sample {sample:?}, threshold {threshold:?}, repeat \
         {STALL_REPEAT_INTERVAL:?}, abort={abort}, rests={can_rest})"
    );
    let mut last = Clocks::now();
    #[cfg(windows)]
    let mut awake = aterm_sysprobe::power::windows::unbiased_time();
    let mut sampler =
        Sampler::with_threshold(last.mono, main.heartbeat.load(Ordering::Relaxed), threshold);
    let mut cpu_read = cpu().is_some();
    // Samples in a row that found no beat since the one before ([`QUIET_SAMPLES`]).
    let mut quiet = 0_u32;
    let mut last_beat = main.heartbeat.load(Ordering::Relaxed);
    loop {
        let asleep = Instant::now();
        let rest =
            can_rest && quiet >= QUIET_SAMPLES && sampler.settled() && main.claim_rest(cpu_read);
        if !seen(Seen::Waits { rest }) {
            main.resting.store(false, Ordering::SeqCst);
            return;
        }
        let until = wait_until(rest, asleep, sample, STEPS_ANNOUNCED);
        let wall_ready = wait(main, wall, until, sample);
        if rest {
            main.resting.store(false, Ordering::SeqCst);
        }
        #[cfg(any(unix, windows))]
        if let Some(bell) = main.bell() {
            bell.clear();
        }
        let heard = wall_ready && heard_wall(wall);
        let here = Clocks::now();
        let mut elapsed = here.since(last, asleep);
        // Windows: `Instant` counted a suspend and nothing announces a clock set, so both are
        // read here, before a rest's span is set aside.
        #[cfg(windows)]
        let stepped = {
            let awake_here = aterm_sysprobe::power::windows::unbiased_time();
            let awake_span = awake
                .zip(awake_here)
                .map(|(before, here)| here.saturating_sub(before));
            awake = awake_here;
            let stepped = stepped_forward(elapsed.wall, elapsed.mono);
            let slept = suspended(elapsed.mono, awake_span);
            if !slept.is_zero() {
                elapsed.mono = elapsed.mono.saturating_sub(slept);
                sampler.skip(slept);
            }
            stepped
        };
        #[cfg(not(windows))]
        let stepped: Option<Duration> = None;
        let requested = if rest {
            // A rest asked for no deadline, so its length is neither late nor a sleep: the wall
            // watch hears those.
            elapsed.wall = Some(elapsed.mono);
            elapsed.mono
        } else {
            sample
        };
        let reading = cpu();
        cpu_read = reading.is_some();
        let beat = main.heartbeat.load(Ordering::Relaxed);
        // A rest ended by the wall alone keeps its count; a timed sample with no beat adds one.
        quiet = match (beat == last_beat, rest) {
            (false, _) => 0,
            (true, true) => quiet,
            (true, false) => quiet.saturating_add(1),
        };
        last_beat = beat;
        let wake = sampler.wake(here.mono, requested, elapsed, beat, main.current(), reading);
        let _ = seen(Seen::Sampled);
        last = here;
        // With a wall watch, only a step it cannot announce (Windows) is still read off the
        // clocks; a sleep is the watch's to tell, so a wake heard in this sample is not also a
        // step (on a machine whose performance counter stops in a sleep, the two clocks part
        // there too).
        let stepped = stepped.filter(|_| wall.is_some() && !heard);
        #[cfg(windows)]
        if let Some(ahead) = stepped {
            aterm_log::info!("{}", stepped_message(ahead));
        }
        let drifted = (wall.is_none() && wake.wall_moved()) || stepped.is_some();
        if heard || drifted {
            let _ = seen(Seen::WallMoved);
            on_wall_moved();
        }
        if wall.is_none()
            && let Some(line) = wake.sleep_note()
        {
            aterm_log::info!("{line}");
        }
        for line in wake.notices() {
            aterm_log::warn!("{line}");
        }
        if let Some(hit) = wake.hit {
            let _ = seen(Seen::Hit(hit.root));
            let locus = if main.returned.load(Ordering::Relaxed) {
                Locus::Returned
            } else {
                Locus::Inside
            };
            aterm_log::error!(
                "{}",
                stall_message(
                    sampler.reports,
                    hit.frozen,
                    hit.root,
                    current_phase(),
                    locus,
                    hit.busy
                )
            );
            let captured = SystemTime::now();
            if let Some(frames) = stack() {
                let cap = aterm_log::record_cap(aterm_log::Level::Error);
                for line in stack_lines(&frames, captured, cap) {
                    aterm_log::error!("{line}");
                }
            }
            if abort {
                let _ = crate::logging::flush_within(crate::logging::LOG_FLUSH_EXIT_WAIT);
                #[expect(
                    clippy::disallowed_methods,
                    reason = "the hang watchdog's configured abort of a wedged window: a real crash report is the point"
                )]
                std::process::abort();
            }
        }
    }
}

#[cfg(test)]
#[path = "watchdog_tests.rs"]
mod tests;
