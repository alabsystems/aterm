// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Main-thread stall watchdog.

use crate::label_enum::label_enum;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How often the sampler wakes: half the bar this build judges a stall against
fn sample_interval() -> Duration {
    threshold() / 2
}

/// How long the heartbeat may stay frozen at a work breadcrumb before it counts as a
const STALL_THRESHOLD: Duration = Duration::from_millis(500);

/// The same bar for a shipped release binary, where the reader is a user's `aterm.log`.
const RELEASE_STALL_THRESHOLD: Duration = Duration::from_secs(5);

/// How often a still-frozen main thread is re-reported after its first line.
const STALL_REPEAT_INTERVAL: Duration = Duration::from_secs(60);

/// THE MAIN THREAD'S WATCH: the cells [`beat`] stamps and the sampler, the control workers and a forking worker read.
pub(crate) struct MainWatch {
    /// Monotonic main-thread liveness counter. [`beat`] increments it on every
    heartbeat: AtomicU64,
    /// The last main-loop root [`beat`] was called from, as a [`Breadcrumb`]
    breadcrumb: AtomicU8,
    /// Whether the root named by `breadcrumb` has RETURNED — its handler is off
    returned: AtomicBool,
}

impl MainWatch {
    /// A watch no root has entered: at [`Breadcrumb::Startup`], no beat yet.
    pub(crate) const fn new() -> Self {
        Self {
            heartbeat: AtomicU64::new(0),
            breadcrumb: AtomicU8::new(Breadcrumb::Startup as u8),
            returned: AtomicBool::new(false),
        }
    }

    /// The breadcrumb the main thread last stamped.
    fn current(&self) -> Breadcrumb {
        Breadcrumb::from_u8(self.breadcrumb.load(Ordering::Relaxed))
    }

    /// Whether the main thread is at a park point now: in its OS event wait
    fn parked(&self) -> bool {
        Breadcrumb::from_u8(self.breadcrumb.load(Ordering::Acquire)).is_park_point()
    }

    /// Wait at most `bound` for the main thread to park ([`Self::parked`]):
    fn wait_for_park(&self, bound: Duration) -> bool {
        let until = std::time::Instant::now() + bound;
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

/// The process's main-thread watch ([`MainWatch`]).
static MAIN: MainWatch = MainWatch::new();

/// The work the main thread last announced inside the current root, as a [`Phase`] `u8`:
static PHASE: AtomicU8 = AtomicU8::new(Phase::None as u8);

label_enum! {
    /// The main-loop roots the watchdog can pin a stall to.
    #[repr(u8)]
    pub(crate) enum Breadcrumb {
        /// Before the event loop runs (synchronous launch). Park point: startup does
        Startup => "startup",
        /// The loop has finished `about_to_wait`'s work and is parked in the OS
        AboutToWait => "idle",
        /// `window_event`: OS-delivered input/resize/close for a window. WORK root.
        WindowEvent => "window_event",
        /// `user_event`: a proxy `Wake` (control socket, config reload, …). WORK root.
        UserEvent => "user_event",
        /// `new_events`: a `WaitUntil` deadline fired (blink / bell / resize settle).
        NewEvents => "new_events",
        /// The resize-settle flush — the coalesced final width lands here, the exact
        ResizeSettle => "resize_settle",
        /// The overlap-handoff park + readiness wait: the main thread deliberately
        UpdateHandoff => "update_handoff",
        /// A NESTED MODAL RUN LOOP spun from inside a work root — `-[NSAlert
        Modal => "modal",
        /// `about_to_wait`'s BODY: the message settle, the restore pass, the crash
        AboutToWaitWork => "about_to_wait",
        /// The event loop has ended and teardown is waiting for accepted work
        ShutdownWait => "shutdown_wait",
    }
}

impl Breadcrumb {
    /// The breadcrumb a `u8` cell holds as its [`index`](Self::index).
    pub(crate) fn from_u8(v: u8) -> Self {
        Self::from_index(usize::from(v)).unwrap_or(Self::Startup)
    }

    /// Whether a main thread on-CPU with a frozen heartbeat at this root is a stall: true
    fn spin_is_a_stall(self) -> bool {
        !matches!(
            self,
            Breadcrumb::Startup | Breadcrumb::UpdateHandoff | Breadcrumb::Modal
        )
    }

    /// A root where a frozen heartbeat is expected, so the sampler does not
    fn is_park_point(self) -> bool {
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

/// The pure stall decision: a frozen heartbeat is a stall iff the main thread sits at a
fn is_stall_at(bc: Breadcrumb, frozen: Duration, threshold: Duration) -> bool {
    !bc.is_park_point() && frozen >= threshold
}

/// The bar a main-thread control verb is refused at ([`main_stall`]): the shipped stall
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

/// Whether a verb that needs the main thread should be refused now rather than posted: the
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
    frozen: Duration,
    /// The main thread was ON-CPU across the samples leading here — a spin,
    busy: bool,
}

/// The share of a sample interval the main thread must spend on-CPU for the
const BUSY_PERCENT: u128 = 80;

/// The sampler's detection state machine, apart from the background thread so its
struct Sampler {
    /// The heartbeat value last observed to advance.
    last_beat: u64,
    /// When the heartbeat last advanced (start of the current frozen span).
    last_advance: Instant,
    /// Whether the current contiguous stall was already reported (fire once).
    reported: bool,
    /// When the current contiguous stall was last reported, for the repeat
    last_report: Option<Instant>,
    /// How many times this contiguous stall has been reported. Per-stall state, so it
    reports: u32,
    /// The bar this sampler judges against — [`STALL_THRESHOLD`] for the dev
    threshold: Duration,
    /// The previous sample's `(instant, main-thread CPU time)`.
    cpu_sample: Option<(Instant, Duration)>,
    /// Since when every sample found the main thread on-CPU for at least
    busy_since: Option<Instant>,
    /// The reported stall that has not ended yet, until the main thread moves (a heartbeat,
    open: Option<OpenStall>,
    /// A reported stall that has since ended, waiting for [`Sampler::take_ended`]. Its line
    ended: Option<Ended>,
}

/// A reported stall that has not ended yet ([`Sampler::open`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OpenStall {
    /// The root it was first reported at.
    root: Breadcrumb,
    /// When it began: the last heartbeat, or, for a spin at a park point, when
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
    fn with_threshold(now: Instant, beat: u64, threshold: Duration) -> Self {
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

    /// Start over at `now`: the process was not running for `gap` ([`process_gap`]), so
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

    /// The reported stall that ended since the last call, once.
    fn take_ended(&mut self) -> Option<Ended> {
        self.ended.take()
    }

    /// One wake of the sampler thread, as the thread takes it: it asked to
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

    /// Fold one sample. Returns a [`Hit`] when this sample should be reported: once when a
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

    /// A report is going out: the stall it reports is open until the main
    fn open_stall(&mut self, hit: Hit, since: Instant) -> Hit {
        self.open.get_or_insert(OpenStall {
            root: hit.root,
            since,
        });
        hit
    }

    /// The open stall, if any, ended at `now`.
    fn end_open(&mut self, now: Instant, by: EndedBy) {
        if let Some(stall) = self.open.take() {
            self.ended = Some(Ended {
                frozen: now.saturating_duration_since(stall.since),
                root: stall.root,
                by,
            });
        }
    }

    /// Track whether the main thread stayed on-CPU between samples. Returns
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

    /// Whether a stall that is past the threshold is due a line now, and
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

    fn clear_reports(&mut self) {
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
    /// The warn-level lines this wake logs, in order, ahead of any stall
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

    /// Whether the wall clock ran far ahead of the monotonic one across this
    fn wall_moved(&self) -> bool {
        matches!(
            self.gap,
            Some(Gap {
                wall: Wall::Slept(_),
                ..
            })
        )
    }

    /// The info-level line for a wake that found the Mac had slept and this
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

/// How late a sampler wake must be before it means the whole process was not running.
const PROCESS_GAP: Duration = Duration::from_secs(30);

/// The lateness of a sampler wake that asked to sleep `requested` and was gone
fn process_gap(requested: Duration, observed: Duration) -> Option<Duration> {
    let late = observed.saturating_sub(requested);
    (late >= PROCESS_GAP).then_some(late)
}

/// How long one sampler wake was gone, on both clocks, since the wake before
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Elapsed {
    /// On the monotonic clock ([`Instant`]), which stops while the Mac sleeps
    mono: Duration,
    /// On the wall clock ([`SystemTime`]), which keeps running while the Mac
    wall: Option<Duration>,
    /// Of `mono`, the report pass before the sleep: the stall lines, the
    pass: Duration,
}

/// Both clocks, read together at one sampler wake. The sampler keeps the previous wake's
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

    /// The span from `earlier` to this reading, for a sampler thread that
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
    unscheduled: Option<Duration>,
    /// What the wall clock says about the same span.
    wall: Wall,
    /// How much of the span was the report pass before the sleep
    pass: Duration,
}

/// Read a wake's `elapsed` span on both clocks: lateness on the monotonic
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

/// A report pass at least this long is named in the gap line: it normally
const PASS_WORTH_NAMING: Duration = Duration::from_secs(1);

/// The warn line for a gap of `late` awake time ([`Gap::unscheduled`]), with
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

/// The info line for a wake that found only the Mac asleep ([`Wall::Slept`]
fn slept_message(slept: Duration) -> String {
    format!(
        "the Mac slept for about {}s: the wall clock ran that much further than the \
         monotonic clock, which stops in sleep (or the clock was set forward). This process \
         lost no awake time, so the stall accounting is unchanged.",
        slept.as_secs()
    )
}

/// A main-loop turn at or over this counts as long: one 30 fps frame budget, the bar
const LONG_TURN_THRESHOLD_NS: u64 = crate::metrics::SLOW_FRAME_THRESHOLD_NS;

/// The main-loop turn census: how long the main thread spent in each work root.
struct TurnLedger {
    /// When the CURRENT root was entered (`crate::metrics::now_ns` clock), for
    open_ns: AtomicU64,
    /// When the main thread last entered a root, on the same clock: the heartbeat's
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
    busy_ns: AtomicU64,
    window_from_ns: AtomicU64,
    /// Each root's booked turns since reset, indexed by [`Breadcrumb::index`]:
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

    /// The span a turn that just ended is attributable for, or `None` when it is not this
    fn attributable_span(previous: Breadcrumb, open_ns: u64, now_ns: u64) -> Option<u64> {
        if open_ns == 0 || previous.is_park_point() {
            return None;
        }
        Some(now_ns.saturating_sub(open_ns))
    }

    /// Close the turn `previous` owned at `now_ns` and open one for the root
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

    /// `(root, n, p50_ms, p95_ms, p99_ms)` per root with at least one booked
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

    /// Clear the window, including the open stamp: the turn straddling a reset began
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

/// The process-global turn census. See [`TurnLedger`].
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
    /// `main_busy_ms_per_s` at `now_ns`: the booked turns' sum over the window's length, in
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
#[inline]
fn beat_at(bc: Breadcrumb, now_ns: u64) -> Option<u64> {
    beat_into(&MAIN, &TURNS, &PHASE, bc, now_ns)
}

/// The beat path with its ledger and phase cell passed in, so the stamp-close-clear-bump
#[inline]
fn beat_into(
    main: &MainWatch,
    turns: &TurnLedger,
    phase: &AtomicU8,
    bc: Breadcrumb,
    now_ns: u64,
) -> Option<u64> {
    turns.last_beat_ns.store(now_ns.max(1), Ordering::Relaxed);
    let previous = Breadcrumb::from_u8(main.breadcrumb.swap(bc as u8, Ordering::Release));
    let booked = turns.close(previous, now_ns);
    phase.store(Phase::None as u8, Ordering::Relaxed);
    main.returned.store(false, Ordering::Relaxed);
    main.heartbeat.fetch_add(1, Ordering::Relaxed);
    booked
}

/// Enter a winit root: [`beat`] now, and mark the root returned when the guard drops at the end of the handler, so a stall after it (in AppKit or CoreFoundation) is not reported as inside it.
#[must_use = "the root lasts only as long as the guard lives"]
pub(crate) fn enter(bc: Breadcrumb) -> RootGuard {
    enter_in(&MAIN, bc)
}

/// [`enter`] on `main`.
fn enter_in(main: &'static MainWatch, bc: Breadcrumb) -> RootGuard {
    beat_on(main, bc);
    RootGuard { main, park: None }
}

/// [`beat`] on `main`, with the process's ledger, phase cell and clock.
fn beat_on(main: &MainWatch, bc: Breadcrumb) {
    beat_into(main, &TURNS, &PHASE, bc, crate::metrics::now_ns());
}

/// Enter `about_to_wait`.
#[must_use = "the root lasts only as long as the guard lives"]
pub(crate) fn enter_about_to_wait() -> RootGuard {
    enter_about_to_wait_in(&MAIN)
}

/// [`enter_about_to_wait`] on `main`.
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
    park: Option<Breadcrumb>,
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

/// How often [`MainWatch::wait_for_park`] looks again.
const PARK_POLL: Duration = Duration::from_micros(50);

thread_local! {
    /// How long this thread's forks wait for the park ([`forks_wait_for_park`]);
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

/// [`park_before_fork`] on `main`.
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
    advance_modal_epoch();
    ModalPark {
        main,
        previous,
        _main_thread: std::marker::PhantomData,
    }
}

#[cfg(any(target_os = "macos", test))]
thread_local! {
    static MODAL_EPOCH: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn modal_epoch() -> u64 {
    MODAL_EPOCH.get()
}

#[cfg(any(target_os = "macos", test))]
fn advance_modal_epoch() {
    MODAL_EPOCH.set(MODAL_EPOCH.get().wrapping_add(1));
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
        advance_modal_epoch();
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
    /// Reconstruct a phase from its stored `u8` (unknown values fold to
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Phase::PixelBackendRedeem,
            2 => Phase::ImageCapture,
            _ => Phase::None,
        }
    }

    /// Stable, symbol-free wording for the log line.
    pub(crate) fn describe(self) -> &'static str {
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

    /// The clause the stall line carries after the root name: empty for
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

/// The announcement over an explicit cell: [`phase`] is this on [`PHASE`], and the phase
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

/// The phase a cell holds.
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
    /// Restore `previous` only over this guard's own announcement. Anything
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Locus {
    /// The root's handler is still on the stack.
    Inside,
    /// The root's handler returned; no aterm handler is running.
    Returned,
}

/// The stall line, from the sampler's facts alone so its wording is testable without a
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

/// The line when a reported stall ends: how long it lasted, the root it was reported at,
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

/// The prefix of every stack line. It contains `MAIN-THREAD STALL`, which the
const STACK_LINE: &str = "MAIN-THREAD STALL stack:";

/// Between two frames of a stack line.
const FRAME_SEPARATOR: &str = " | ";

/// The main thread's stack as log lines, as many as keep each within `cap`, the logger's
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

/// `at` as the log writes its own timestamps: Unix seconds, a dot, millis.
fn epoch_stamp(at: SystemTime) -> String {
    match at.duration_since(UNIX_EPOCH) {
        Ok(since) => format!("{}.{:03}", since.as_secs(), since.subsec_millis()),
        Err(_) => "an unknown time (the clock reads before 1970)".into(),
    }
}

/// The bytes `c` takes in a log line: the logger writes every control
fn logged_char_len(c: char) -> usize {
    if c.is_control() {
        char::REPLACEMENT_CHARACTER.len_utf8()
    } else {
        c.len_utf8()
    }
}

/// The bytes `s` takes in a log line ([`logged_char_len`]).
fn logged_len(s: &str) -> usize {
    s.chars().map(logged_char_len).sum()
}

/// `frame` as it fits in `budget` logged bytes: whole when it fits, else cut
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

/// The `$ATERM_WATCHDOG` development seam's value; `None` in every shipped binary.
fn seam() -> Option<String> {
    aterm_types::dev_seam!("ATERM_WATCHDOG").map(|v| v.to_string_lossy().trim().to_string())
}

/// Whether the sampler runs: in every build, unless a development build sets
fn enabled() -> bool {
    !seam().is_some_and(|v| {
        v.eq_ignore_ascii_case("off") || v.eq_ignore_ascii_case("0") || v.is_empty()
    })
}

/// The bar this build judges a stall against: tight when a developer is
fn threshold() -> Duration {
    if cfg!(debug_assertions) || seam().is_some() {
        STALL_THRESHOLD
    } else {
        RELEASE_STALL_THRESHOLD
    }
}

/// Whether a detected stall ends the process with a crash report (repro / CI) rather than
fn abort_on_stall() -> bool {
    seam().is_some_and(|v| v.eq_ignore_ascii_case("abort"))
}

/// Spawn the background stall sampler; call once, just before `run_app`.
pub(crate) fn start(on_wall_moved: impl Fn() + Send + 'static) {
    if !enabled() {
        return;
    }
    let abort = abort_on_stall();
    let threshold = threshold();
    crate::main_thread_probe::register();
    let builder = std::thread::Builder::new().name("aterm-watchdog".into());
    let _ = builder.spawn(move || {
        let sample = sample_interval();
        aterm_log::info!(
            "main-thread stall watchdog armed (sample {sample:?}, threshold \
             {threshold:?}, repeat {STALL_REPEAT_INTERVAL:?}, abort={abort})"
        );
        let mut last = Clocks::now();
        let mut sampler =
            Sampler::with_threshold(last.mono, MAIN.heartbeat.load(Ordering::Relaxed), threshold);
        loop {
            let asleep = Instant::now();
            std::thread::sleep(sample);
            let here = Clocks::now();
            let wake = sampler.wake(
                here.mono,
                sample,
                here.since(last, asleep),
                MAIN.heartbeat.load(Ordering::Relaxed),
                MAIN.current(),
                crate::main_thread_probe::cpu_time(),
            );
            last = here;
            if wake.wall_moved() {
                on_wall_moved();
            }
            if let Some(line) = wake.sleep_note() {
                aterm_log::info!("{line}");
            }
            for line in wake.notices() {
                aterm_log::warn!("{line}");
            }
            if let Some(hit) = wake.hit {
                let locus = if MAIN.returned.load(Ordering::Relaxed) {
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
                if let Some(frames) = crate::main_thread_probe::stack() {
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
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The process's ledger ([`TURNS`]) and phase cell ([`PHASE`]) are
    static BEAT_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn beat_serial() -> std::sync::MutexGuard<'static, ()> {
        BEAT_SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `park_modal` round-trips: the outer root's breadcrumb comes back when
    #[test]
    fn park_modal_restores_the_outer_root_with_a_fresh_beat() {
        static WATCH: MainWatch = MainWatch::new();
        let _serial = beat_serial();
        beat_on(&WATCH, Breadcrumb::WindowEvent);
        let before = WATCH.heartbeat.load(Ordering::Relaxed);
        let modal_before = modal_epoch();
        {
            let _park = park_modal_in(&WATCH);
            assert_eq!(WATCH.current(), Breadcrumb::Modal);
            assert_eq!(modal_epoch(), modal_before.wrapping_add(1));
        }
        assert_eq!(modal_epoch(), modal_before.wrapping_add(2));
        assert_eq!(WATCH.current(), Breadcrumb::WindowEvent);
        assert_eq!(WATCH.heartbeat.load(Ordering::Relaxed), before + 2);
    }

    /// A phase is scoped to its guard: nested correctly, restored when the guard drops, and
    #[test]
    fn a_phase_is_scoped_to_its_guard_and_cleared_by_the_next_root() {
        static PH: AtomicU8 = AtomicU8::new(Phase::None as u8);
        const T0: u64 = 11_000_000_000;
        const MS: u64 = 1_000_000;

        let _serial = beat_serial();
        let turns = TurnLedger::new();
        let watch = MainWatch::new();

        beat_into(&watch, &turns, &PH, Breadcrumb::UserEvent, T0);
        assert_eq!(
            watch.heartbeat.load(Ordering::Relaxed),
            1,
            "a root entry beats"
        );
        assert_eq!(phase_in(&PH), Phase::None, "a root entry announces nothing");
        {
            let _capture = announce(&PH, Phase::ImageCapture);
            assert_eq!(phase_in(&PH), Phase::ImageCapture);
            {
                let _redeem = announce(&PH, Phase::PixelBackendRedeem);
                assert_eq!(
                    phase_in(&PH),
                    Phase::PixelBackendRedeem,
                    "the inner phase reads while it lasts"
                );
            }
            assert_eq!(
                phase_in(&PH),
                Phase::ImageCapture,
                "the outer phase comes back when the inner one ends"
            );
        }
        assert_eq!(phase_in(&PH), Phase::None);
        let held = announce(&PH, Phase::PixelBackendRedeem);
        beat_into(&watch, &turns, &PH, Breadcrumb::AboutToWait, T0 + MS);
        assert_eq!(
            phase_in(&PH),
            Phase::None,
            "the next root's beat clears a phase whose guard is still alive"
        );
        drop(held);
        assert_eq!(phase_in(&PH), Phase::None);
        beat_into(&watch, &turns, &PH, Breadcrumb::UserEvent, T0 + 2 * MS);
        let outer = announce(&PH, Phase::ImageCapture);
        let inner = announce(&PH, Phase::PixelBackendRedeem);
        beat_into(&watch, &turns, &PH, Breadcrumb::AboutToWait, T0 + 3 * MS);
        assert_eq!(phase_in(&PH), Phase::None);
        drop(inner);
        assert_eq!(
            phase_in(&PH),
            Phase::None,
            "a stale inner guard must not re-announce the outer phase"
        );
        drop(outer);
        assert_eq!(phase_in(&PH), Phase::None);
        let outer = announce(&PH, Phase::ImageCapture);
        let inner = announce(&PH, Phase::PixelBackendRedeem);
        beat_into(&watch, &turns, &PH, Breadcrumb::UserEvent, T0 + 4 * MS);
        let fresh = announce(&PH, Phase::ImageCapture);
        assert_eq!(phase_in(&PH), Phase::ImageCapture);
        drop(fresh);
        assert_eq!(
            phase_in(&PH),
            Phase::None,
            "the fresh guard sampled the beat's None and puts it back"
        );
        drop(inner);
        drop(outer);
        assert_eq!(phase_in(&PH), Phase::None);
    }

    /// A turn that ended inside a work root is attributable; a park point's span (idle, a
    #[test]
    fn a_work_root_turn_is_attributable_and_a_designed_park_is_never_booked() {
        const OPEN: u64 = 1_000_000;
        const PARK_NS: u64 = 600 * 1_000_000_000;
        for work in [
            Breadcrumb::WindowEvent,
            Breadcrumb::UserEvent,
            Breadcrumb::NewEvents,
            Breadcrumb::ResizeSettle,
            Breadcrumb::AboutToWaitWork,
        ] {
            assert_eq!(
                TurnLedger::attributable_span(work, OPEN, OPEN + 300_000_000),
                Some(300_000_000),
                "a 300 ms park in `{}` is the exact reading this census exists to \
                 name: the release sampler ignores it and the redraw timer never \
                 sees it",
                work.as_str()
            );
        }
        for park in [
            Breadcrumb::Startup,
            Breadcrumb::AboutToWait,
            Breadcrumb::UpdateHandoff,
            Breadcrumb::Modal,
        ] {
            assert_eq!(
                TurnLedger::attributable_span(park, OPEN, OPEN + PARK_NS),
                None,
                "`{}` is a DESIGNED freeze — booking it would make every reading \
                 on this line meaningless",
                park.as_str()
            );
        }
        assert_eq!(
            TurnLedger::attributable_span(Breadcrumb::UserEvent, 0, OPEN + PARK_NS),
            None,
            "a turn that began before this window is not this window's to price"
        );
    }

    /// A 312 ms park in a work root, which the redraw timer and the release sampler both
    #[test]
    fn the_turn_census_names_the_worst_root_and_counts_the_long_turns() {
        const T0: u64 = 5_000_000_000;
        const LONG: u64 = 312_000_000;
        const SHORT: u64 = 2_000_000;
        let ledger = TurnLedger::new();

        assert_eq!(ledger.close(Breadcrumb::AboutToWait, T0), None);
        let c = ledger.snapshot();
        assert_eq!(c.turns, 0);
        assert_eq!(
            c.max_owner, None,
            "an empty census must name no owner at all: `startup` would be a \
             claim about a root that never ran"
        );

        assert_eq!(ledger.close(Breadcrumb::UserEvent, T0 + LONG), Some(LONG));
        let c = ledger.snapshot();
        assert_eq!(c.max_ns, LONG);
        assert_eq!(
            c.max_owner,
            Some(Breadcrumb::UserEvent),
            "a max that cannot name its root sends the next reader to the GPU"
        );
        assert_eq!(c.max_at_ns, T0 + LONG, "the worst turn says WHEN it ended");
        assert_eq!(c.last_ns, LONG);
        assert_eq!((c.turns, c.long_turns), (1, 1));

        assert_eq!(
            ledger.close(Breadcrumb::WindowEvent, T0 + LONG + SHORT),
            Some(SHORT)
        );
        let c = ledger.snapshot();
        assert_eq!((c.max_ns, c.last_ns), (LONG, SHORT));
        assert_eq!(
            c.max_owner,
            Some(Breadcrumb::UserEvent),
            "a shorter turn must never steal the max's owner label"
        );
        assert_eq!(
            (c.turns, c.long_turns),
            (2, 1),
            "the count is what separates one hitch from a main thread that is \
             late all the time"
        );

        let roots = |ledger: &TurnLedger| {
            ledger
                .root_rows()
                .into_iter()
                .map(|(root, n, ..)| (root, n))
                .collect::<Vec<_>>()
        };
        assert_eq!(roots(&ledger), [("window_event", 1), ("user_event", 1)]);

        let idle = T0 + LONG + SHORT;
        assert_eq!(
            ledger.close(Breadcrumb::AboutToWait, idle + 600_000_000_000),
            None
        );
        assert_eq!(ledger.snapshot().turns, 2);
        assert_eq!(roots(&ledger), [("window_event", 1), ("user_event", 1)]);

        ledger.reset();
        let c = ledger.snapshot();
        assert_eq!((c.max_ns, c.last_ns, c.turns, c.long_turns), (0, 0, 0, 0));
        assert_eq!(c.max_owner, None);
        assert_eq!(roots(&ledger), [], "the distributions are window stats");
        assert_eq!(
            ledger.close(Breadcrumb::UserEvent, T0 + 900_000_000_000),
            None,
            "the turn open across a `metrics reset` began outside the new window"
        );
    }

    /// THE CENSUS IS ON THE HOT PATH, not beside it. The rule above is only
    #[test]
    fn a_beat_books_the_turn_it_closes_to_the_root_that_owned_it() {
        let _serial = beat_serial();
        const T0: u64 = 7_000_000_000;
        const PARKED: u64 = 250_000_000;
        let turns = TurnLedger::new();
        let phase = AtomicU8::new(Phase::None as u8);
        let watch = MainWatch::new();

        assert_eq!(
            beat_into(&watch, &turns, &phase, Breadcrumb::UserEvent, T0),
            None
        );
        assert_eq!(watch.current(), Breadcrumb::UserEvent);

        assert_eq!(
            beat_into(&watch, &turns, &phase, Breadcrumb::AboutToWait, T0 + PARKED),
            Some(PARKED),
            "the beat that ends a 250 ms `user_event` turn must book it"
        );
        let c = turns.snapshot();
        assert_eq!(c.max_ns, PARKED);
        assert_eq!(
            c.max_owner,
            Some(Breadcrumb::UserEvent),
            "the turn belongs to the root that HELD the thread, not to the one \
             that ended it"
        );
        assert_eq!((c.turns, c.long_turns), (1, 1));

        assert_eq!(
            beat_into(
                &watch,
                &turns,
                &phase,
                Breadcrumb::NewEvents,
                T0 + PARKED + 600_000_000_000
            ),
            None
        );
        assert_eq!(turns.snapshot().turns, 1);
    }

    /// Each root's verdict at the stall bar, and its round trip through the
    #[test]
    fn each_root_is_a_stall_only_as_a_work_root_past_the_bar() {
        let ten_min = Duration::from_secs(600);
        let under = STALL_THRESHOLD - Duration::from_millis(1);
        #[rustfmt::skip]
        let table = [
            (Breadcrumb::ResizeSettle, STALL_THRESHOLD, STALL_THRESHOLD, true),
            (Breadcrumb::ResizeSettle, under, STALL_THRESHOLD, false),
            (Breadcrumb::WindowEvent, STALL_THRESHOLD, STALL_THRESHOLD, true),
            (Breadcrumb::WindowEvent, Duration::from_secs(1), RELEASE_STALL_THRESHOLD, false),
            (Breadcrumb::NewEvents, ten_min, STALL_THRESHOLD, true),
            (Breadcrumb::UserEvent, Duration::from_secs(3588), RELEASE_STALL_THRESHOLD, true),
            (Breadcrumb::AboutToWait, ten_min, STALL_THRESHOLD, false),
            (Breadcrumb::Startup, ten_min, STALL_THRESHOLD, false),
            (Breadcrumb::Modal, ten_min, STALL_THRESHOLD, false),
            (Breadcrumb::Modal, ten_min, RELEASE_STALL_THRESHOLD, false),
            (Breadcrumb::UpdateHandoff, ten_min, RELEASE_STALL_THRESHOLD, false),
            (Breadcrumb::AboutToWaitWork, STALL_THRESHOLD, STALL_THRESHOLD, true),
            (Breadcrumb::ShutdownWait, ten_min, STALL_THRESHOLD, false),
        ];
        for (bc, frozen, bar, stall) in table {
            assert_eq!(
                is_stall_at(bc, frozen, bar),
                stall,
                "{bc:?} frozen {frozen:?} at the {bar:?} bar"
            );
            assert_eq!(Breadcrumb::from_u8(bc as u8), bc);
        }
        assert_eq!(Breadcrumb::from_u8(200), Breadcrumb::Startup);
        assert!(Breadcrumb::from_u8(200).is_park_point());
    }

    /// `about_to_wait`'s body is booked to `about_to_wait` and its wait never is: a 2 ms
    #[test]
    fn the_about_to_wait_body_is_booked_and_the_wait_after_it_never_is() {
        const MS: u64 = 1_000_000;
        const T0: u64 = 7_000 * MS;
        let ledger = TurnLedger::new();
        assert_eq!(ledger.close(Breadcrumb::AboutToWait, T0), None, "arms only");
        assert_eq!(ledger.close(Breadcrumb::WindowEvent, T0 + MS), Some(MS));
        assert_eq!(
            ledger.close(Breadcrumb::AboutToWaitWork, T0 + 3 * MS),
            Some(2 * MS)
        );
        assert_eq!(
            ledger.close(Breadcrumb::AboutToWait, T0 + 3 * MS + 600_000 * MS),
            None
        );
        let c = ledger.snapshot();
        assert_eq!((c.turns, c.busy_ns), (2, 3 * MS));
        assert_eq!(c.max_owner.map(Breadcrumb::as_str), Some("about_to_wait"));
        assert!((c.busy_ms_per_s(3_000 * MS) - 1.0).abs() < 1e-9);
        assert_eq!(c.busy_ms_per_s(0), 0.0, "an empty window is idle");
        ledger.reset();
        assert_eq!(ledger.snapshot().busy_ns, 0, "a window stat clears");
    }

    /// A worker about to fork waits for the park, never past its bound: inside
    #[test]
    fn a_fork_waits_for_the_main_thread_to_park() {
        let watch = MainWatch::new();
        let _serial = beat_serial();
        for root in [
            Breadcrumb::UserEvent,
            Breadcrumb::WindowEvent,
            Breadcrumb::NewEvents,
            Breadcrumb::AboutToWaitWork,
        ] {
            beat_on(&watch, root);
            assert!(!watch.parked(), "{root:?} is a turn");
            assert!(!watch.wait_for_park(Duration::ZERO), "{root:?}");
        }
        for park in [
            Breadcrumb::AboutToWait,
            Breadcrumb::Startup,
            Breadcrumb::Modal,
            Breadcrumb::UpdateHandoff,
        ] {
            beat_on(&watch, park);
            assert!(watch.parked(), "{park:?} is a park point");
            assert!(watch.wait_for_park(Duration::from_secs(60)), "{park:?}");
        }
    }

    /// A fork waits for the park only inside `forks_wait_for_park`, for its
    #[test]
    fn a_fork_waits_for_the_park_only_inside_a_spawn_that_asks() {
        let watch = MainWatch::new();
        let _serial = beat_serial();
        let bound = Duration::from_millis(30);
        let ambient = || FORK_PARK_BOUND.get();
        beat_on(&watch, Breadcrumb::UserEvent);
        assert_eq!(ambient(), Duration::ZERO, "no spawn asked: no wait");
        assert!(!park_before_fork_on(&watch));
        let inside = forks_wait_for_park(bound, || {
            let t0 = std::time::Instant::now();
            (
                ambient(),
                park_before_fork_on(&watch),
                t0.elapsed() >= bound,
            )
        });
        assert_eq!(
            inside,
            (bound, false, true),
            "a turn holds the fork for its bound"
        );
        assert_eq!(ambient(), Duration::ZERO, "the bound went with the spawn");
        let unwound = std::panic::catch_unwind(|| {
            forks_wait_for_park(bound, || panic!("the spawn panicked"))
        });
        assert!(unwound.is_err());
        assert_eq!(
            ambient(),
            Duration::ZERO,
            "the bound went with the unwound spawn"
        );
        beat_on(&watch, Breadcrumb::AboutToWait);
        assert!(forks_wait_for_park(Duration::from_secs(60), || {
            park_before_fork_on(&watch)
        }));
    }

    /// The guard `about_to_wait` holds stamps its body as the work root, and the idle park
    #[test]
    fn about_to_wait_s_guard_parks_only_as_the_handler_returns() {
        static WATCH: MainWatch = MainWatch::new();
        let _serial = beat_serial();
        beat_on(&WATCH, Breadcrumb::UserEvent);
        let before = WATCH.heartbeat.load(Ordering::Relaxed);
        {
            let _root = enter_about_to_wait_in(&WATCH);
            assert_eq!(WATCH.current(), Breadcrumb::AboutToWaitWork);
            assert!(!WATCH.returned.load(Ordering::Relaxed));
        }
        assert_eq!(WATCH.current(), Breadcrumb::AboutToWait);
        assert!(WATCH.returned.load(Ordering::Relaxed));
        assert_eq!(WATCH.heartbeat.load(Ordering::Relaxed), before + 2);
    }

    /// A block inside `about_to_wait`'s body that takes no CPU (a lock, a wait on a thread)
    #[test]
    fn an_off_cpu_block_in_the_about_to_wait_body_is_a_named_stall() {
        let t0 = Instant::now();
        let step = Duration::from_millis(250);
        let quiet = |tick: u32| Some(Duration::from_millis(tick.into()));
        let mut body = Sampler::with_threshold(t0, 3, STALL_THRESHOLD);
        let mut idle = Sampler::with_threshold(t0, 3, STALL_THRESHOLD);
        let mut hit = None;
        for tick in 1..=8u32 {
            let now = t0 + step * tick;
            hit = hit.or(body.poll_with(now, 3, Breadcrumb::AboutToWaitWork, quiet(tick)));
            assert_eq!(
                idle.poll_with(now, 3, Breadcrumb::AboutToWait, quiet(tick)),
                None,
                "the OS wait is a designed park"
            );
        }
        let hit = hit.expect("the blocked body is reported");
        assert_eq!((hit.root, hit.busy), (Breadcrumb::AboutToWaitWork, false));
        let line = stall_message(1, hit.frozen, hit.root, Phase::None, Locus::Inside, false);
        assert!(line.contains("`AboutToWaitWork`"), "{line}");
        let beat_ns = REFUSAL_T0;
        assert_eq!(
            main_stall(
                beat_ns + 3_000_000,
                beat_ns,
                Breadcrumb::AboutToWaitWork,
                false,
                Some(beat_ns - 1_000_000_000),
            ),
            None
        );
    }

    /// A reported stall that ends says so, once, with how long it lasted and where:
    #[test]
    fn a_reported_stall_that_ends_says_so_once_and_an_unreported_one_never_does() {
        let t0 = Instant::now();
        let mut s = Sampler::with_threshold(t0, 7, STALL_THRESHOLD);
        assert_eq!(
            s.poll(t0 + Duration::from_millis(300), 7, Breadcrumb::UserEvent),
            None
        );
        assert_eq!(
            s.poll(t0 + Duration::from_millis(400), 8, Breadcrumb::UserEvent),
            None
        );
        assert_eq!(s.take_ended(), None);
        let from = t0 + Duration::from_millis(400);
        assert_eq!(
            s.poll(from + Duration::from_millis(600), 8, Breadcrumb::UserEvent),
            Some(Breadcrumb::UserEvent)
        );
        assert_eq!(s.take_ended(), None, "still frozen: nothing has ended");
        assert_eq!(
            s.poll(from + Duration::from_secs(9), 9, Breadcrumb::AboutToWait),
            None
        );
        let ended = Ended {
            frozen: Duration::from_secs(9),
            root: Breadcrumb::UserEvent,
            by: EndedBy::Beat,
        };
        assert_eq!(s.take_ended(), Some(ended));
        assert_eq!(s.take_ended(), None);
        #[cfg(unix)]
        for by in [EndedBy::Beat, EndedBy::Quiet] {
            assert!(
                stall_ended_message(Ended { by, ..ended })
                    .starts_with(crate::recovery_census::STALL_ENDED_LINE)
            );
        }
        for locus in [Locus::Inside, Locus::Returned] {
            for reports in [1, 2] {
                assert!(
                    !stall_message(
                        reports,
                        Duration::from_secs(5),
                        Breadcrumb::UserEvent,
                        Phase::None,
                        locus,
                        true
                    )
                    .contains("ENDED")
                );
            }
        }
    }

    /// A shipped binary arms the watchdog; only a development seam switches it off.
    #[test]
    fn the_watchdog_is_armed_unless_explicitly_switched_off() {
        // `enabled()` reads the environment, which is process-global and shared
        if seam().is_none() {
            assert!(
                enabled(),
                "a shipped build must arm the stall watchdog: silence is what \
                 cost five hours on 2026-08-30"
            );
        }
    }

    /// A stall that never ends is re-reported on a cadence, and progress re-arms it.
    #[test]
    fn a_persisting_stall_is_reported_again_on_the_repeat_cadence() {
        let t0 = Instant::now();
        let mut s = Sampler::with_threshold(t0, 1, STALL_THRESHOLD);
        assert_eq!(
            s.poll(t0 + Duration::from_secs(1), 1, Breadcrumb::UserEvent),
            Some(Breadcrumb::UserEvent),
            "the first crossing must report"
        );
        assert!(
            s.poll(t0 + Duration::from_secs(30), 1, Breadcrumb::UserEvent)
                .is_none(),
            "inside the repeat interval it stays quiet — no flood"
        );
        assert_eq!(
            s.poll(
                t0 + Duration::from_secs(1) + STALL_REPEAT_INTERVAL,
                1,
                Breadcrumb::UserEvent
            ),
            Some(Breadcrumb::UserEvent),
            "a stall still live one interval later must say so again"
        );
        assert!(
            s.poll(t0 + Duration::from_secs(200), 2, Breadcrumb::UserEvent)
                .is_none(),
            "a heartbeat means the main thread is back"
        );
    }

    /// The report count re-arms on recovery: a second wedge reports as a first, not as
    #[test]
    fn a_second_stall_after_recovery_reports_as_a_first_again() {
        let t0 = Instant::now();
        let mut s = Sampler::with_threshold(t0, 1, STALL_THRESHOLD);
        assert_eq!(
            s.poll(t0 + Duration::from_secs(1), 1, Breadcrumb::UserEvent),
            Some(Breadcrumb::UserEvent)
        );
        assert_eq!(s.reports, 1, "the first line of the first stall");
        assert!(
            s.poll(t0 + Duration::from_secs(2), 2, Breadcrumb::UserEvent)
                .is_none()
        );
        assert_eq!(s.reports, 0, "recovery re-arms the report count");
        assert_eq!(
            s.poll(t0 + Duration::from_secs(9000), 2, Breadcrumb::WindowEvent),
            Some(Breadcrumb::WindowEvent)
        );
        assert_eq!(
            s.reports, 1,
            "a separate wedge must announce itself in full, not as a continuation"
        );
    }

    /// A spin in CoreFoundation after `NewEvents` returned is named as outside aterm and
    #[test]
    fn the_2026_09_26_spin_is_named_as_outside_aterm_and_on_cpu() {
        static WATCH: MainWatch = MainWatch::new();
        let _serial = beat_serial();
        {
            let _root = enter_in(&WATCH, Breadcrumb::NewEvents);
            assert!(
                !WATCH.returned.load(Ordering::Relaxed),
                "inside the handler"
            );
        }
        assert!(
            WATCH.returned.load(Ordering::Relaxed),
            "the handler returned"
        );
        assert_eq!(WATCH.current(), Breadcrumb::NewEvents);

        let t0 = Instant::now();
        let mut s = Sampler::with_threshold(t0, 9, RELEASE_STALL_THRESHOLD);
        let mut hit = None;
        for tick in 1..=4u32 {
            let at = t0 + Duration::from_millis(2_500) * tick;
            let cpu = Duration::from_millis(2_500) * tick;
            hit = hit.or(s.poll_with(at, 9, Breadcrumb::NewEvents, Some(cpu)));
        }
        let hit = hit.expect("a frozen work root past the bar is a stall");
        assert!(hit.busy, "on-CPU across the samples: a spin, not a park");
        let line = stall_message(
            1,
            hit.frozen,
            hit.root,
            Phase::None,
            Locus::Returned,
            hit.busy,
        );
        assert!(line.contains("since `NewEvents` returned"), "{line}");
        assert!(line.contains("ON-CPU"), "{line}");
        assert!(!line.contains("while inside"), "{line}");
        {
            let _root = enter_in(&WATCH, Breadcrumb::UserEvent);
            drop(park_modal_in(&WATCH));
            assert!(
                !WATCH.returned.load(Ordering::Relaxed),
                "back inside the outer root"
            );
        }
    }

    /// A spin at the idle park point is a stall; a quiet park is not, however
    #[test]
    fn a_spin_at_the_idle_park_is_a_stall_and_a_quiet_park_is_not() {
        let t0 = Instant::now();
        let step = Duration::from_millis(250);
        let run = |bc: Breadcrumb, busy: bool| {
            let mut s = Sampler::with_threshold(t0, 3, STALL_THRESHOLD);
            let mut hits = Vec::new();
            for tick in 1..=12u32 {
                let cpu = if busy {
                    step * tick
                } else {
                    Duration::from_millis(1) * tick
                };
                if let Some(hit) = s.poll_with(t0 + step * tick, 3, bc, Some(cpu)) {
                    hits.push((tick, hit));
                }
            }
            hits
        };
        for park in [Breadcrumb::AboutToWait, Breadcrumb::ShutdownWait] {
            let spun = run(park, true);
            assert_eq!(spun.len(), 1, "one report per contiguous spin: {spun:?}");
            let (tick, hit) = spun[0];
            assert!(hit.busy && hit.root == park);
            assert!(hit.frozen >= STALL_THRESHOLD, "{hit:?}");
            assert_eq!(
                tick, 3,
                "busy from the first interval, the bar crossed at the third sample"
            );
            assert!(
                run(park, false).is_empty(),
                "a quiet {park:?} is not a stall"
            );
            let mut s = Sampler::with_threshold(t0, 3, STALL_THRESHOLD);
            for tick in 1..=12u32 {
                assert!(s.poll_with(t0 + step * tick, 3, park, None).is_none());
            }
        }
        for designed in [
            Breadcrumb::Startup,
            Breadcrumb::Modal,
            Breadcrumb::UpdateHandoff,
        ] {
            assert!(
                run(designed, true).is_empty(),
                "{designed:?} does real work by design"
            );
        }
    }

    /// A 26.6 h gap in the sampler's sleep reads as the process not running (scheduling
    #[test]
    fn a_process_gap_is_named_and_restarts_the_stall_clock() {
        let sample = RELEASE_STALL_THRESHOLD / 2;
        assert_eq!(
            process_gap(sample, sample + Duration::from_secs(95_674)),
            Some(Duration::from_secs(95_674))
        );
        assert_eq!(process_gap(sample, sample + Duration::from_secs(3)), None);
        assert_eq!(process_gap(sample, Duration::ZERO), None);

        let t0 = Instant::now();
        let mut s = Sampler::with_threshold(t0, 5, RELEASE_STALL_THRESHOLD);
        let back = t0 + Duration::from_secs(95_676);
        s.resync(back, 5, Duration::from_secs(95_674));
        assert!(
            s.poll_with(
                back + Duration::from_secs(1),
                5,
                Breadcrumb::UserEvent,
                None
            )
            .is_none(),
            "the gap is not a stall of the main thread"
        );
        assert!(
            s.poll_with(
                back + Duration::from_secs(6),
                5,
                Breadcrumb::UserEvent,
                None
            )
            .is_some(),
            "a real wedge after the gap still reports"
        );
    }

    /// One wake of `s`, driven as the sampler thread drives it (no CPU reading),
    fn wake_logged(
        s: &mut Sampler,
        lines: &mut Vec<String>,
        now: Instant,
        slept: Duration,
        beat: u64,
        bc: Breadcrumb,
    ) -> SamplerWake {
        let elapsed = Elapsed {
            mono: slept,
            wall: Some(slept),
            pass: Duration::ZERO,
        };
        wake_logged_on(s, lines, now, elapsed, beat, bc)
    }

    /// [`wake_logged`] with the two clocks given apart.
    fn wake_logged_on(
        s: &mut Sampler,
        lines: &mut Vec<String>,
        now: Instant,
        elapsed: Elapsed,
        beat: u64,
        bc: Breadcrumb,
    ) -> SamplerWake {
        let wake = s.wake(now, s.threshold / 2, elapsed, beat, bc, None);
        lines.extend(wake.sleep_note());
        lines.extend(wake.notices());
        if let Some(hit) = wake.hit {
            lines.push(stall_message(
                s.reports,
                hit.frozen,
                hit.root,
                Phase::None,
                Locus::Inside,
                hit.busy,
            ));
        }
        wake
    }

    /// A gap of `late` awake time with the Mac awake throughout, as
    fn awake_gap(late: Duration) -> Option<Gap> {
        Some(Gap {
            unscheduled: Some(late),
            wall: Wall::Awake,
            pass: Duration::ZERO,
        })
    }

    /// A gap inside the report pass after a stall line is seen because each wake is timed
    #[test]
    fn the_2026_09_28_gap_inside_a_report_pass_is_not_charged_to_the_main_thread() {
        let step = RELEASE_STALL_THRESHOLD / 2;
        let pass = Duration::from_secs(12_130);
        let t0 = Instant::now();
        let reported = |lines: &mut Vec<String>| {
            let mut s = Sampler::with_threshold(t0, 5, RELEASE_STALL_THRESHOLD);
            let w = wake_logged(&mut s, lines, t0 + step, step, 5, Breadcrumb::NewEvents);
            assert_eq!(w.hit, None, "under the bar");
            let w = wake_logged(&mut s, lines, t0 + step * 2, step, 5, Breadcrumb::NewEvents);
            assert_eq!(w.hit.map(|h| h.root), Some(Breadcrumb::NewEvents));
            s
        };
        let back = t0 + step * 3 + pass;

        let mut lines = Vec::new();
        let mut s = reported(&mut lines);
        let w = wake_logged_on(
            &mut s,
            &mut lines,
            back,
            Elapsed {
                mono: pass + step,
                wall: Some(pass + step),
                pass,
            },
            6,
            Breadcrumb::AboutToWait,
        );
        assert_eq!(
            w.gap,
            Some(Gap {
                unscheduled: Some(pass),
                wall: Wall::Awake,
                pass,
            })
        );
        assert_eq!(
            w.ended.map(|e| e.frozen),
            Some(step * 3),
            "the main thread's own 7.5 s, not the pass's 12130 s"
        );
        assert!(
            lines
                .last()
                .is_some_and(|l| l.starts_with("MAIN-THREAD STALL ENDED:")),
            "{lines:#?}"
        );
        #[cfg(unix)]
        assert_eq!(
            crate::recovery_census::stall_at_end(&lines),
            aterm_update::recovery_ledger::Tri::No,
            "{lines:#?}"
        );

        let mut lines = Vec::new();
        let mut s = reported(&mut lines);
        let w = wake_logged(&mut s, &mut lines, back, step, 6, Breadcrumb::AboutToWait);
        assert_eq!(w.gap, None, "a gap outside the timed sleep is invisible");
        assert_eq!(
            w.ended.map(|e| e.frozen),
            Some(step * 3 + pass),
            "and the main thread is charged with all of it"
        );

        let mut lines = Vec::new();
        let mut s = reported(&mut lines);
        let w = wake_logged_on(
            &mut s,
            &mut lines,
            t0 + step * 3,
            Elapsed {
                mono: step,
                wall: Some(step + pass),
                pass: Duration::ZERO,
            },
            6,
            Breadcrumb::AboutToWait,
        );
        assert_eq!(
            w.gap,
            Some(Gap {
                unscheduled: None,
                wall: Wall::Slept(pass),
                pass: Duration::ZERO,
            })
        );
        assert_eq!(w.ended.map(|e| e.frozen), Some(step * 3));
    }

    /// THE WALL MOVED (design §9.3.6, item 6): a wake whose wall clock ran a
    #[test]
    fn a_wake_that_finds_the_wall_ahead_says_the_wall_moved() {
        let step = RELEASE_STALL_THRESHOLD / 2;
        let hour = Duration::from_secs(3_600);
        let wake = |mono: Duration, wall: Option<Duration>| SamplerWake {
            gap: classify_gap(
                step,
                Elapsed {
                    mono,
                    wall,
                    pass: Duration::ZERO,
                },
            ),
            ended: None,
            hit: None,
        };
        assert!(wake(step, Some(step + hour)).wall_moved(), "a sleep");
        assert!(
            wake(step + hour, Some(step + hour * 3)).wall_moved(),
            "a sleep beside awake time lost"
        );
        assert!(!wake(step, Some(step)).wall_moved(), "on time");
        assert!(
            !wake(step + hour, Some(step + hour)).wall_moved(),
            "awake time lost alone: the wall moved with the monotonic clock"
        );
        assert!(!wake(step, None).wall_moved(), "the wall went backwards");
        assert!(
            !wake(step, Some(step + PROCESS_GAP - Duration::from_secs(1))).wall_moved(),
            "a lead short of the gap"
        );
    }

    /// Both clocks, read apart: lateness on the monotonic clock is awake time
    #[test]
    fn a_gap_is_read_on_both_clocks() {
        let step = RELEASE_STALL_THRESHOLD / 2;
        let hour = Duration::from_secs(3_600);
        let lost = Duration::from_secs(100);
        let on = |mono: Duration, wall: Option<Duration>| {
            classify_gap(
                step,
                Elapsed {
                    mono,
                    wall,
                    pass: Duration::ZERO,
                },
            )
        };
        let gap = |unscheduled: Option<Duration>, wall: Wall| {
            Some(Gap {
                unscheduled,
                wall,
                pass: Duration::ZERO,
            })
        };
        assert_eq!(
            on(step + hour, Some(step + hour)),
            gap(Some(hour), Wall::Awake)
        );
        assert_eq!(on(step, Some(step + hour)), gap(None, Wall::Slept(hour)));
        assert_eq!(
            on(step + lost, Some(step + lost + hour)),
            gap(Some(lost), Wall::Slept(hour))
        );
        let jitter = Duration::from_secs(3);
        assert_eq!(on(step + jitter, Some(step + jitter * 4)), None);
        assert_eq!(
            on(step, Some(step + PROCESS_GAP - Duration::from_millis(1))),
            None
        );
        assert_eq!(on(step, None), None);
        assert_eq!(on(step + hour, None), gap(Some(hour), Wall::Unknown));
        assert_eq!(on(step + hour, Some(step)), gap(Some(hour), Wall::Awake));

        let t0 = Instant::now();
        let w0 = UNIX_EPOCH + Duration::from_secs(1_790_000_000);
        let reported_for = Duration::from_millis(2);
        let earlier = Clocks { mono: t0, wall: w0 };
        let later = Clocks {
            mono: t0 + reported_for + step,
            wall: w0 + reported_for + step + hour,
        };
        assert_eq!(
            later.since(earlier, t0 + reported_for),
            Elapsed {
                mono: reported_for + step,
                wall: Some(reported_for + step + hour),
                pass: reported_for,
            }
        );
        let set_back = Clocks {
            mono: t0 + step,
            wall: w0 - hour,
        };
        assert_eq!(
            set_back.since(earlier, t0),
            Elapsed {
                mono: step,
                wall: None,
                pass: Duration::ZERO,
            }
        );

        let huge = Duration::from_secs(u64::MAX);
        for line in [
            gap_message(huge, Wall::Slept(huge), huge),
            gap_message(huge, Wall::Awake, huge),
            gap_message(huge, Wall::Unknown, huge),
        ] {
            assert!(
                line.len() <= aterm_log::record_cap(aterm_log::Level::Warn),
                "{} bytes: {line}",
                line.len()
            );
            assert!(!line.contains("MAIN-THREAD STALL"), "{line}");
        }
        let asleep = slept_message(huge);
        assert!(
            asleep.len() <= aterm_log::record_cap(aterm_log::Level::Info),
            "{} bytes: {asleep}",
            asleep.len()
        );
        assert!(!asleep.contains("MAIN-THREAD STALL"), "{asleep}");
    }

    /// A deep stack is logged whole, in order, in numbered and stamped parts, each under
    #[test]
    fn the_stack_is_logged_whole_in_numbered_parts_under_the_error_cap() {
        let cap = aterm_log::record_cap(aterm_log::Level::Error);
        let mut frames =
            vec!["load address 0x1004c8000, UUID 4C4C4453-5555-3144-A1B2-C3D4E5F60718".to_string()];
        frames.extend((0..64).map(|i| {
            format!(
                "#{i} AppKit + {} (-[NSSceneStatusItem _setupScene:] + {}) \
                 [a synthetic AppKit frame]",
                1_000_000 + 4_096 * i,
                60 + i
            )
        }));
        let captured = UNIX_EPOCH + Duration::from_millis(1_790_191_963_664);

        let old = format!("{STACK_LINE} {}", frames.join(FRAME_SEPARATOR));
        let logged = aterm_log::sanitize_record_for(aterm_log::Level::Error, &old);
        assert!(
            old.contains("| #11 ") && logged.contains("| #10 ") && !logged.contains("| #11 "),
            "{logged}"
        );

        let lines = stack_lines(&frames, captured, cap);
        let parts = lines.len();
        assert!(parts > 1, "{lines:#?}");
        let mut rejoined: Vec<String> = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let head = format!(
                "MAIN-THREAD STALL stack: (part {}/{parts}, captured at 1790191963.664) ",
                i + 1
            );
            let body = line
                .strip_prefix(head.as_str())
                .unwrap_or_else(|| panic!("numbered and stamped: {line}"));
            assert!(line.len() <= cap, "{} bytes: {line}", line.len());
            assert_eq!(
                aterm_log::sanitize_record_for(aterm_log::Level::Error, line),
                line.as_str(),
                "the logger passes every part untouched"
            );
            rejoined.extend(body.split(FRAME_SEPARATOR).map(str::to_string));
        }
        assert_eq!(rejoined, frames, "every frame, in order, none split");
    }

    /// A frame too long for a line of its own is clipped, on a character
    #[test]
    fn an_oversized_frame_is_clipped_into_a_part_of_its_own() {
        let cap = aterm_log::record_cap(aterm_log::Level::Error);
        let captured = UNIX_EPOCH + Duration::from_millis(1_790_191_963_664);
        let frames = [
            "#0 libsystem_kernel.dylib + 4660 (mach_msg2_trap + 8)".to_string(),
            format!("#1 {}", "é".repeat(1_100)),
            format!("#2 {}", "\u{1}".repeat(600)),
            "#3 dyld + 24680 (start + 2360)".to_string(),
        ];
        let lines = stack_lines(&frames, captured, cap);
        assert_eq!(lines.len(), 4, "{lines:#?}");
        for line in &lines {
            assert!(
                logged_len(line) <= cap,
                "{} bytes: {line}",
                logged_len(line)
            );
            assert_eq!(
                aterm_log::sanitize_record_for(aterm_log::Level::Error, line).len(),
                logged_len(line),
                "the logger replaces control characters and cuts nothing: {line}"
            );
        }
        assert!(lines[0].ends_with(") #0 libsystem_kernel.dylib + 4660 (mach_msg2_trap + 8)"));
        assert!(
            lines[1].contains("(part 2/4, ") && lines[1].ends_with("é…"),
            "{}",
            lines[1]
        );
        assert!(lines[2].ends_with("\u{1}…"), "{}", lines[2]);
        assert!(lines[3].ends_with(") #3 dyld + 24680 (start + 2360)"));
        assert!(stack_lines(&[], captured, cap).is_empty());

        assert_eq!(epoch_stamp(captured), "1790191963.664");
        assert_eq!(epoch_stamp(UNIX_EPOCH + Duration::from_millis(5)), "0.005");
        assert!(epoch_stamp(UNIX_EPOCH - Duration::from_secs(1)).starts_with("an unknown time"));
    }

    /// A stall reported before a process gap stays open across the resync, so its end is
    #[test]
    fn a_stall_reported_before_a_process_gap_is_still_ended_after_it() {
        let step = RELEASE_STALL_THRESHOLD / 2;
        let gap = Duration::from_secs(95_674);
        let t0 = Instant::now();
        let reported = |lines: &mut Vec<String>| {
            let mut s = Sampler::with_threshold(t0, 5, RELEASE_STALL_THRESHOLD);
            let w = wake_logged(&mut s, lines, t0 + step, step, 5, Breadcrumb::UserEvent);
            assert_eq!(w.hit, None, "under the bar");
            let w = wake_logged(&mut s, lines, t0 + step * 2, step, 5, Breadcrumb::UserEvent);
            assert_eq!(w.hit.map(|h| h.root), Some(Breadcrumb::UserEvent));
            s
        };
        let back = t0 + step * 3 + gap;

        let mut lines = Vec::new();
        let mut s = reported(&mut lines);
        let w = wake_logged(
            &mut s,
            &mut lines,
            back,
            step + gap,
            5,
            Breadcrumb::UserEvent,
        );
        assert_eq!((w.gap, w.ended, w.hit), (awake_gap(gap), None, None));
        let w = wake_logged(
            &mut s,
            &mut lines,
            back + step,
            step,
            5,
            Breadcrumb::UserEvent,
        );
        assert_eq!((w.ended, w.hit), (None, None), "under the bar again");
        let w = wake_logged(
            &mut s,
            &mut lines,
            back + step * 2,
            step,
            6,
            Breadcrumb::AboutToWait,
        );
        assert_eq!(
            w.ended,
            Some(Ended {
                frozen: step * 5,
                root: Breadcrumb::UserEvent,
                by: EndedBy::Beat,
            }),
            "the stall reported before the gap ends after it"
        );
        assert!(
            lines
                .last()
                .is_some_and(|l| l.starts_with("MAIN-THREAD STALL ENDED:")),
            "{lines:#?}"
        );
        assert_eq!(s.take_ended(), None, "said once");
        #[cfg(unix)]
        assert_eq!(
            crate::recovery_census::stall_at_end(&lines),
            aterm_update::recovery_ledger::Tri::No,
            "a stall the run survived is not its last word: {lines:#?}"
        );

        let mut lines = Vec::new();
        let mut s = reported(&mut lines);
        let w = wake_logged(
            &mut s,
            &mut lines,
            back,
            step + gap,
            6,
            Breadcrumb::AboutToWait,
        );
        assert_eq!(w.gap, awake_gap(gap));
        assert_eq!(
            w.ended,
            Some(Ended {
                frozen: step * 3,
                root: Breadcrumb::UserEvent,
                by: EndedBy::Beat,
            })
        );
        assert_eq!(w.notices().len(), 2, "the gap line, then the ENDED line");
        #[cfg(unix)]
        assert_eq!(
            crate::recovery_census::stall_at_end(&lines),
            aterm_update::recovery_ledger::Tri::No,
            "{lines:#?}"
        );
        assert_eq!(
            wake_logged(
                &mut s,
                &mut lines,
                back + step,
                step,
                7,
                Breadcrumb::UserEvent
            )
            .ended,
            None,
            "ended once, not again at the next beat"
        );

        let mut lines = Vec::new();
        let mut s = reported(&mut lines);
        wake_logged(
            &mut s,
            &mut lines,
            back,
            step + gap,
            5,
            Breadcrumb::UserEvent,
        );
        #[cfg(unix)]
        assert_eq!(
            crate::recovery_census::stall_at_end(&lines),
            aterm_update::recovery_ledger::Tri::Yes,
            "{lines:#?}"
        );
    }

    /// A process gap never invents a stall: a freeze under the bar before the
    #[test]
    fn a_process_gap_does_not_invent_a_stall() {
        let step = RELEASE_STALL_THRESHOLD / 2;
        let gap = Duration::from_secs(95_674);
        let t0 = Instant::now();
        let mut lines = Vec::new();
        let mut s = Sampler::with_threshold(t0, 5, RELEASE_STALL_THRESHOLD);
        let w = wake_logged(
            &mut s,
            &mut lines,
            t0 + step,
            step,
            5,
            Breadcrumb::UserEvent,
        );
        assert_eq!(w.hit, None);
        let back = t0 + step * 2 + gap;
        let w = wake_logged(
            &mut s,
            &mut lines,
            back,
            step + gap,
            5,
            Breadcrumb::UserEvent,
        );
        assert_eq!((w.gap, w.ended, w.hit), (awake_gap(gap), None, None));
        let w = wake_logged(
            &mut s,
            &mut lines,
            back + step,
            step,
            5,
            Breadcrumb::UserEvent,
        );
        assert_eq!(
            (w.ended, w.hit),
            (None, None),
            "the gap is not charged to the main thread"
        );
        let w = wake_logged(
            &mut s,
            &mut lines,
            back + step * 2,
            step,
            6,
            Breadcrumb::UserEvent,
        );
        assert_eq!((w.ended, w.hit), (None, None));
        assert_eq!(lines.len(), 1, "the gap line alone: {lines:#?}");
        #[cfg(unix)]
        assert_eq!(
            crate::recovery_census::stall_at_end(&lines),
            aterm_update::recovery_ledger::Tri::No,
            "{lines:#?}"
        );

        for beat_after in [8, 9] {
            let mut lines = Vec::new();
            let mut s = Sampler::with_threshold(t0, 7, RELEASE_STALL_THRESHOLD);
            wake_logged(
                &mut s,
                &mut lines,
                t0 + step * 2,
                step,
                7,
                Breadcrumb::UserEvent,
            );
            let w = wake_logged(
                &mut s,
                &mut lines,
                t0 + step * 3,
                step,
                8,
                Breadcrumb::UserEvent,
            );
            assert!(w.ended.is_some());
            let w = wake_logged(
                &mut s,
                &mut lines,
                t0 + step * 4 + gap,
                step + gap,
                beat_after,
                Breadcrumb::UserEvent,
            );
            assert_eq!((w.ended, w.hit), (None, None), "beat {beat_after}");
            #[cfg(unix)]
            assert_eq!(
                crate::recovery_census::stall_at_end(&lines),
                aterm_update::recovery_ledger::Tri::No,
                "{lines:#?}"
            );
        }
    }

    /// A spin reported at the idle park ends once a CPU reading finds the main thread
    #[test]
    fn a_spin_reported_at_the_idle_park_ends_when_a_reading_finds_it_quiet() {
        let t0 = Instant::now();
        let step = Duration::from_millis(250);
        let spin_to_report = |s: &mut Sampler| {
            let mut hit = None;
            for tick in 1..=3u32 {
                hit = hit.or(s.poll_with(
                    t0 + step * tick,
                    3,
                    Breadcrumb::AboutToWait,
                    Some(step * tick),
                ));
            }
            assert!(hit.is_some_and(|h| h.busy), "{hit:?}");
            assert_eq!(s.take_ended(), None);
        };
        let mut s = Sampler::with_threshold(t0, 3, STALL_THRESHOLD);
        spin_to_report(&mut s);
        let cpu = step * 3 + Duration::from_millis(1);
        assert_eq!(
            s.poll_with(t0 + step * 4, 3, Breadcrumb::AboutToWait, Some(cpu)),
            None
        );
        assert_eq!(
            s.take_ended(),
            Some(Ended {
                frozen: step * 3,
                root: Breadcrumb::AboutToWait,
                by: EndedBy::Quiet,
            })
        );
        assert!(
            s.poll_with(t0 + step * 5, 4, Breadcrumb::NewEvents, Some(cpu))
                .is_none()
                && s.take_ended().is_none(),
            "ended once: the next heartbeat ends nothing"
        );
        let mut s = Sampler::with_threshold(t0, 3, STALL_THRESHOLD);
        spin_to_report(&mut s);
        for tick in 4..=40u32 {
            assert_eq!(
                s.poll_with(t0 + step * tick, 3, Breadcrumb::AboutToWait, None),
                None
            );
            assert_eq!(s.take_ended(), None, "no reading is no evidence it ended");
        }
        s.poll_with(t0 + step * 41, 4, Breadcrumb::NewEvents, None);
        assert_eq!(
            s.take_ended().map(|e| (e.root, e.by)),
            Some((Breadcrumb::AboutToWait, EndedBy::Beat))
        );
    }

    /// The quiet rule ends only a spin reported at a park point. A work-root stall (parked
    #[test]
    fn a_work_root_stall_ends_by_its_beat_never_as_a_quiet_spin() {
        let t0 = Instant::now();
        let step = Duration::from_millis(250);
        let quiet = |tick: u32| Some(Duration::from_millis(tick.into()));
        let mut s = Sampler::with_threshold(t0, 3, STALL_THRESHOLD);
        let mut hit = None;
        for tick in 1..=3u32 {
            hit = hit.or(s.poll_with(t0 + step * tick, 3, Breadcrumb::UserEvent, quiet(tick)));
        }
        assert!(
            hit.is_some_and(|h| h.root == Breadcrumb::UserEvent && !h.busy),
            "a park on a lock inside `UserEvent`, reported: {hit:?}"
        );
        assert_eq!(
            s.poll_with(t0 + step * 4, 3, Breadcrumb::AboutToWait, quiet(4)),
            None
        );
        assert_eq!(s.take_ended(), None, "not a spin that went quiet");
        assert_eq!(
            s.poll_with(t0 + step * 5, 4, Breadcrumb::AboutToWait, quiet(5)),
            None
        );
        assert_eq!(
            s.take_ended(),
            Some(Ended {
                frozen: step * 5,
                root: Breadcrumb::UserEvent,
                by: EndedBy::Beat,
            })
        );
        assert_eq!(s.take_ended(), None);
    }

    /// A process-clock reading long after launch, where a stall happens.
    const REFUSAL_T0: u64 = 40_000_000_000;

    /// One tick of the derived `MainThreadStallRefusal` machine: half the
    const REFUSAL_TICK_NS: u64 = 2_500_000_000;

    fn secs(n: u64) -> u64 {
        n * 1_000_000_000
    }

    /// A work root with no beat for the bar is refused, whether its handler returned (the
    #[test]
    fn a_work_root_with_no_beat_for_the_bar_is_refused_and_says_where() {
        let beat = REFUSAL_T0;
        assert_eq!(
            main_stall(beat + secs(4), beat, Breadcrumb::NewEvents, true, None),
            None,
            "under the bar is a slow turn, not a stall"
        );
        let stall = main_stall(beat + secs(12_138), beat, Breadcrumb::NewEvents, true, None)
            .expect("a work root frozen past the bar is stalled");
        assert_eq!(stall.stalled, Duration::from_secs(12_138));
        let inside = main_stall(beat + secs(7), beat, Breadcrumb::UserEvent, false, None)
            .expect("a handler still on the stack is stalled too");
        assert_eq!(inside.stalled, Duration::from_secs(7));
    }

    /// The idle park is no stall by itself (an idle thread's heartbeat is old by design),
    #[test]
    fn the_idle_park_is_refused_only_once_a_hop_has_waited_the_bar() {
        let beat = REFUSAL_T0;
        let idle = beat + secs(600);
        let park = Breadcrumb::AboutToWait;
        assert_eq!(main_stall(idle, beat, park, true, None), None, "idle");
        assert_eq!(
            main_stall(idle, beat, park, true, Some(idle)),
            None,
            "a hop just posted"
        );
        assert_eq!(
            main_stall(idle + secs(4), beat, park, true, Some(idle)),
            None
        );
        let stall = main_stall(idle + secs(5), beat, park, true, Some(idle))
            .expect("a hop that waited the bar on an unmoved heartbeat");
        assert_eq!(
            stall.stalled,
            Duration::from_secs(5),
            "counted from the hop, not from the idle before it"
        );
        assert_eq!(
            main_stall(idle + secs(6), idle + secs(2), park, true, Some(idle)),
            None
        );
        assert!(main_stall(idle, beat, park, true, Some(1)).is_some());
    }

    /// The designed freezes never read as stalled, however long and whatever
    #[test]
    fn a_designed_freeze_or_a_thread_that_never_beat_is_never_refused() {
        let beat = REFUSAL_T0;
        let late = beat + secs(3_600);
        for root in [
            Breadcrumb::Modal,
            Breadcrumb::UpdateHandoff,
            Breadcrumb::Startup,
        ] {
            assert_eq!(
                main_stall(late, beat, root, false, Some(beat)),
                None,
                "{root:?}"
            );
        }
        assert_eq!(
            main_stall(late, 0, Breadcrumb::UserEvent, false, Some(beat)),
            None,
            "no beat yet is no evidence"
        );
    }

    /// The real writer, reset, hop count and reader, driven beside the derived
    struct RefusalLockstep<'a> {
        /// This lockstep's own main-thread watch, which the writer stamps.
        watch: &'a MainWatch,
        ledger: &'a TurnLedger,
        phase: &'a AtomicU8,
        /// `'static`, as the process's own is, so a real `Wake::Hop` can
        hops: &'static crate::control::control_media::Hops,
        /// The worker's side of the posted hop, alive while it waits.
        hop: Option<crate::control::control_media::HopGuard<'static>>,
        /// When `hop` was posted.
        posted: u64,
        now: u64,
        root: Breadcrumb,
        returned: bool,
        /// The census defect: the reader takes the turn census's open stamp,
        census: bool,
        /// The heartbeat-only defect: a waiting hop is judged as if it had
        beat_only: bool,
        /// The count-until-reply defect: a hop counts from its post for as
        until_reply: bool,
    }

    impl RefusalLockstep<'_> {
        fn enter_root(&mut self, root: Breadcrumb, returned: bool) {
            beat_into(self.watch, self.ledger, self.phase, root, self.now);
            self.root = root;
            self.returned = returned;
        }

        /// The main thread takes the posted hop: the real `take_hop` on the
        fn take(&self) {
            let hop = self.hop.as_ref().expect("a posted hop to take");
            let request = crate::control::control_media::take_hop(crate::Wake::Hop {
                mark: hop.mark(),
                wake: Box::new(crate::Wake::WarmupResult),
            });
            assert!(matches!(request, crate::Wake::WarmupResult), "{request:?}");
        }

        /// The stamp the reader under test takes for the heartbeat's time.
        fn stamp(&self) -> u64 {
            if self.census {
                self.ledger.open_ns.load(Ordering::Relaxed)
            } else {
                self.ledger.last_beat_ns.load(Ordering::Relaxed)
            }
        }

        /// The real reader's verdict.
        fn refuses(&self) -> bool {
            let since = if self.until_reply {
                self.hop.as_ref().map(|_| self.posted)
            } else {
                self.hops.since_ns()
            };
            let since = since.map(|since| if self.beat_only { 1 } else { since });
            main_stall(self.now, self.stamp(), self.root, self.returned, since).is_some()
        }

        /// Take the model's `action` on the real code; `Some` is the reader's
        fn step(&mut self, action: &str) -> Option<bool> {
            match action {
                "BeatIdle" => self.enter_root(Breadcrumb::AboutToWait, true),
                "BeatWork" => self.enter_root(Breadcrumb::NewEvents, true),
                "BeatPark" => self.enter_root(Breadcrumb::Modal, false),
                "Tick" => self.now += REFUSAL_TICK_NS,
                "Post" => {
                    let hops: &'static crate::control::control_media::Hops = self.hops;
                    self.hop = Some(hops.post(self.now));
                    self.posted = self.now;
                }
                "Answer" => {
                    self.enter_root(Breadcrumb::UserEvent, false);
                    self.take();
                    self.hop = None;
                }
                "Take" => {
                    self.enter_root(Breadcrumb::UserEvent, false);
                    self.take();
                }
                "Reply" => {
                    self.enter_root(Breadcrumb::UserEvent, false);
                    self.hop = None;
                }
                "GiveUp" => self.hop = None,
                "Reset" => self.ledger.reset(),
                "Probe" => return Some(self.refuses()),
                other => panic!("no real step for model action `{other}`"),
            }
            None
        }
    }

    /// One schedule taken on `model` and on the real code in lockstep, with `mutation` (a
    fn refusal_lockstep(
        model: &aterm_spec::derive::Model,
        mutation: Option<&'static str>,
        schedule: &[&'static str],
    ) -> Vec<(i64, i64, bool)> {
        let ledger = TurnLedger::new();
        let phase = AtomicU8::new(Phase::None as u8);
        let watch = MainWatch::new();
        let hops: &'static crate::control::control_media::Hops =
            Box::leak(Box::new(crate::control::control_media::Hops::new()));
        let mut real = RefusalLockstep {
            watch: &watch,
            ledger: &ledger,
            phase: &phase,
            hops,
            hop: None,
            posted: 0,
            now: REFUSAL_T0,
            root: Breadcrumb::Startup,
            returned: false,
            census: mutation == Some("MutateCensusStamp"),
            beat_only: mutation == Some("MutateHeartbeatOnly"),
            until_reply: mutation == Some("MutateCountsUntilReply"),
        };
        let mut state = model.init_state();
        if let Some(mutation) = mutation {
            assert!(model.fire(mutation, &mut state), "{mutation}");
        }
        let mut probes = Vec::new();
        for &action in schedule {
            assert!(
                model.fire(action, &mut state),
                "{action} is not admitted at {state:?} ({schedule:?})"
            );
            let verdict = real.step(action);
            assert_eq!(
                state["waiting"],
                i64::from(real.hop.is_some()),
                "{action} in {schedule:?}: the worker's live guard projects onto `waiting`"
            );
            assert_eq!(
                state["hop"],
                i64::from(hops.since_ns().is_some()),
                "{action} in {schedule:?}: the hop count projects onto `hop`"
            );
            assert_eq!(
                state["stamp"],
                i64::from(real.stamp() != 0),
                "{action} in {schedule:?}: the reader's stamp projects onto `stamp`"
            );
            if let Some(refuses) = verdict {
                assert_eq!(
                    state["refused"],
                    i64::from(refuses),
                    "{action} in {schedule:?}: the real reader and the model disagree"
                );
                probes.push((state["refused"], state["stalled"], refuses));
            }
        }
        probes
    }

    /// `main_stall` conforms to the derived `MainThreadStallRefusal` machine: each schedule
    #[test]
    fn main_stall_conforms_to_the_stall_refusal_model() {
        let model = aterm_models_gui::main_thread_stall_refusal_model();
        assert!(model.consts.contains(&("Bar", 2)), "two ticks are the bar");
        assert_eq!(Duration::from_nanos(2 * REFUSAL_TICK_NS), MAIN_STALL_BAR);

        let _serial = beat_serial();
        let schedules: &[&[&'static str]] = &[
            &[
                "BeatWork", "Probe", "Tick", "Probe", "Tick", "Probe", "Post", "Tick", "Probe",
            ],
            &[
                "BeatIdle", "Tick", "Tick", "Tick", "Post", "Probe", "Tick", "Probe", "Tick",
                "Probe",
            ],
            &[
                "BeatIdle", "Tick", "Tick", "Post", "Tick", "Answer", "Probe", "BeatIdle", "Tick",
                "Tick", "Probe",
            ],
            &[
                "BeatWork", "BeatPark", "Post", "Tick", "Tick", "Tick", "Probe",
            ],
            &[
                "BeatWork", "Tick", "Reset", "Tick", "Probe", "Reset", "Probe",
            ],
            &[
                "BeatIdle", "Post", "Tick", "Tick", "Probe", "GiveUp", "Probe",
            ],
            &[
                "BeatIdle", "Post", "Tick", "BeatIdle", "Tick", "Probe", "Tick", "Probe",
            ],
            &["Tick", "Tick", "Post", "Tick", "Tick", "Probe"],
            &[
                "BeatIdle", "Post", "Take", "BeatIdle", "Tick", "Tick", "Probe", "Tick", "Probe",
                "Reply", "Probe",
            ],
            &["BeatIdle", "Post", "Take", "Tick", "Tick", "Probe"],
        ];
        let mut refused = 0;
        for schedule in schedules {
            for (model_refused, stalled, _) in refusal_lockstep(&model, None, schedule) {
                assert_eq!(model_refused, stalled, "{schedule:?}");
                refused += model_refused;
            }
        }
        assert!(refused > 0, "the schedules reach a refusal");

        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let reset_under_stall: &[&'static str] = &["BeatWork", "Tick", "Reset", "Tick", "Probe"];
        assert_eq!(
            refusal_lockstep(&model, None, reset_under_stall),
            [(1, 1, true)]
        );
        assert_eq!(
            refusal_lockstep(&buggy, Some("MutateCensusStamp"), reset_under_stall),
            [(0, 1, false)],
            "read through the census stamp, a reset lets a stalled thread's verbs through"
        );
        let fresh_hop: &[&'static str] = &["BeatIdle", "Tick", "Tick", "Tick", "Post", "Probe"];
        assert_eq!(refusal_lockstep(&model, None, fresh_hop), [(0, 0, false)]);
        assert_eq!(
            refusal_lockstep(&buggy, Some("MutateHeartbeatOnly"), fresh_hop),
            [(1, 0, true)],
            "judged on the heartbeat alone, a fresh hop after idleness is refused"
        );
        let queued_reply: &[&'static str] = &[
            "BeatIdle", "Post", "Take", "BeatIdle", "Tick", "Tick", "Probe",
        ];
        assert_eq!(
            refusal_lockstep(&model, None, queued_reply),
            [(0, 0, false)]
        );
        assert_eq!(
            refusal_lockstep(&buggy, Some("MutateCountsUntilReply"), queued_reply),
            [(1, 0, true)],
            "counted until its reply, a queued settings write makes a healthy idle thread \
             read as stuck"
        );
    }
}
