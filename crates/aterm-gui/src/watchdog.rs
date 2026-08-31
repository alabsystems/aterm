// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dev/CI main-thread STALL watchdog (L0 hazard guard).
//!
//! Standing guard against the "main thread does unbounded work under a contended
//! lock → whole-Mac freeze" hazard class. The archetype was a width change that
//! rewrapped the ENTIRE scrollback synchronously on the UI thread under the
//! per-session `term` `Arc<Mutex<Terminal>>` — a 42-second freeze. That specific
//! site is fixed by `resize_offloading_scrollback`, but sixteen sibling triggers
//! all funnel into `Grid::resize`'s width-reflow sink, so the CLASS needs a
//! standing tripwire that pins any FUTURE regression to a named main-loop root
//! *without symbols* — exactly what the stripped-release spindump lacked.
//!
//! ## How it works
//!
//! Each winit `ApplicationHandler` root calls [`beat`] on entry with a
//! [`Breadcrumb`] naming where the main thread is. `beat` bumps a monotonic
//! [`HEARTBEAT`] counter and stamps the [`BREADCRUMB`]. A background sampler
//! thread (started by [`start`]) wakes every [`SAMPLE_INTERVAL`]; if the
//! heartbeat has not advanced for longer than [`STALL_THRESHOLD`] *and* the last
//! breadcrumb is a WORK root (not the idle park point), the main thread is wedged
//! inside bounded event handling — it logs (and optionally aborts) with the last
//! breadcrumb NAME.
//!
//! ## Why the park-point exemption matters
//!
//! Between events the winit loop parks in the OS event wait (after
//! `about_to_wait`), so the heartbeat legitimately freezes while idle. Firing on
//! that would be pure noise. The fix: [`Breadcrumb::AboutToWait`] (and the
//! pre-loop [`Breadcrumb::Startup`]) are marked [`Breadcrumb::is_park_point`], and
//! the sampler NEVER reports a stall while the last breadcrumb is a park point. A
//! real freeze happens *inside* `window_event` / `user_event` / the resize-settle
//! flush — a WORK breadcrumb that never advances to `AboutToWait` — so it trips.
//!
//! ## ON IN RELEASE, at a coarser threshold
//!
//! It used to be off in release: [`enabled`] was `cfg!(debug_assertions) ||
//! $ATERM_WATCHDOG`, so a shipped binary spawned no sampler and reported
//! nothing. On 2026-08-30 that cost five hours. A self-recursive `OnceLock`
//! (`app_update_screen::debug_seamless_reexec_armed`, shipped in v0.65.0 and
//! v0.66.0) parked the main thread inside `user_event` on the first automatic
//! update apply. The window stayed up, the process stayed alive, and
//! `aterm.log` recorded nothing at all from the main thread from that second
//! on — the only evidence was a macOS hang report, which had to be
//! hand-symbolicated against the stripped release binary to name the frame.
//! That is the exact scenario this module's own header says it exists for
//! ("*without symbols* — exactly what the stripped-release spindump lacked"),
//! and it was compiled out of the build that needed it.
//!
//! So the sampler now runs in EVERY build. What changes with the build is the
//! threshold, not the existence of the guard:
//!
//! * debug builds, or any build with `$ATERM_WATCHDOG` set —
//!   [`STALL_THRESHOLD`] (500 ms). Tight, for catching a regression while
//!   developing it.
//! * a shipped release binary — [`RELEASE_STALL_THRESHOLD`] (5 s). A main
//!   thread frozen at a WORK root for five seconds is never normal, so the
//!   coarser bar keeps a slow-but-progressing frame, a huge paste or a cold
//!   font scan from ever writing an alarming line, while still turning a
//!   PERMANENT wedge into a named log line within seconds instead of never.
//!
//! `ATERM_WATCHDOG=off` disables the sampler entirely; `ATERM_WATCHDOG=abort`
//! still `process::abort()`s on a detected stall (CI / repro). Everything else
//! logs at error level and keeps going. [`beat`] is two relaxed atomic writes
//! in every build either way (negligible on the hot event path), and the
//! sampler is one thread asleep 99.99% of the time.
//!
//! A stall that PERSISTS is re-reported every [`STALL_REPEAT_INTERVAL`] with
//! the accumulated frozen duration, so the log distinguishes "wedged for a
//! moment" from "wedged for an hour and never recovered" — the distinction the
//! 0.65.0 log could not make, because it said nothing at all.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How often the sampler thread wakes to inspect the heartbeat.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(250);

/// How long the heartbeat may stay frozen at a WORK breadcrumb before it counts
/// as a stall, in a DEBUG build or under an explicit `$ATERM_WATCHDOG`. Two
/// sample intervals, so a genuine wedge is caught within ~750 ms while a single
/// slow-but-progressing frame never trips.
const STALL_THRESHOLD: Duration = Duration::from_millis(500);

/// The same bar for a SHIPPED release binary, where the reader is a user's
/// `aterm.log` rather than a developer's terminal.
///
/// Ten sample intervals. The trade is deliberate and one-directional: 500 ms of
/// main-thread work is unusual but not impossible in the field (a cold font
/// catalog, a very large paste, a first-frame pipeline build), and an error line
/// for one of those is noise that teaches a reader to ignore the guard. Five
/// seconds is not survivable UI latency under any reading — nothing in this
/// program is allowed to hold the main thread that long — so a line at 5 s is
/// always a real finding. It still converts a PERMANENT park from silence into
/// a named log line within seconds, which is the whole point.
const RELEASE_STALL_THRESHOLD: Duration = Duration::from_secs(5);

/// How often a still-frozen main thread is re-reported after its first line.
/// One line proves a wedge happened; the repeats prove it never ended, and
/// carry the growing duration.
const STALL_REPEAT_INTERVAL: Duration = Duration::from_secs(60);

/// Monotonic main-thread liveness counter. [`beat`] increments it on every winit
/// root entry; the sampler watches it for a frozen span.
static HEARTBEAT: AtomicU64 = AtomicU64::new(0);

/// The last main-loop root [`beat`] was called from, as a [`Breadcrumb`] `u8`.
/// Initialised to [`Breadcrumb::Startup`] so the pre-event-loop launch window is
/// treated as a park point (no false stall during heavy synchronous startup).
static BREADCRUMB: AtomicU8 = AtomicU8::new(Breadcrumb::Startup as u8);

/// The main-loop roots the watchdog can pin a stall to. `#[repr(u8)]` so it round
/// trips through the [`BREADCRUMB`] atomic with no allocation and no symbols — the
/// NAME survives into a stripped-release log line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Breadcrumb {
    /// Before the event loop runs (synchronous launch). Park point: startup does
    /// heavy main-thread work by design and must not trip the guard.
    Startup = 0,
    /// `about_to_wait`: the loop finished this iteration and is about to park in
    /// the OS event wait. Park point — the heartbeat legitimately freezes here.
    AboutToWait = 1,
    /// `window_event`: OS-delivered input/resize/close for a window. WORK root.
    WindowEvent = 2,
    /// `user_event`: a proxy `Wake` (control socket, config reload, …). WORK root.
    UserEvent = 3,
    /// `new_events`: a `WaitUntil` deadline fired (blink / bell / resize settle).
    /// WORK root.
    NewEvents = 4,
    /// The resize-settle flush — the coalesced final width lands here, the exact
    /// path that funnels into `Grid::resize`'s width-reflow sink. WORK root.
    ResizeSettle = 5,
    /// The overlap-handoff park + readiness wait: the main thread deliberately
    /// blocks (bounded) while the update child boots under the frozen frame,
    /// beating every poll tick. Park point — a frozen heartbeat here is the
    /// designed wait, not a stall (the wait itself is deadline-bounded).
    UpdateHandoff = 6,
}

impl Breadcrumb {
    /// Reconstruct a breadcrumb from its stored `u8` (unknown values fold to
    /// [`Breadcrumb::Startup`], the benign park point).
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Breadcrumb::AboutToWait,
            2 => Breadcrumb::WindowEvent,
            3 => Breadcrumb::UserEvent,
            4 => Breadcrumb::NewEvents,
            5 => Breadcrumb::ResizeSettle,
            6 => Breadcrumb::UpdateHandoff,
            _ => Breadcrumb::Startup,
        }
    }

    /// Stable, symbol-free name for the log line.
    pub fn name(self) -> &'static str {
        match self {
            Breadcrumb::Startup => "Startup",
            Breadcrumb::AboutToWait => "AboutToWait",
            Breadcrumb::WindowEvent => "WindowEvent",
            Breadcrumb::UserEvent => "UserEvent",
            Breadcrumb::NewEvents => "NewEvents",
            Breadcrumb::ResizeSettle => "ResizeSettle",
            Breadcrumb::UpdateHandoff => "UpdateHandoff",
        }
    }

    /// A root where a frozen heartbeat is EXPECTED (idle park / pre-loop startup),
    /// so the sampler must not report a stall while parked here.
    fn is_park_point(self) -> bool {
        matches!(
            self,
            Breadcrumb::Startup | Breadcrumb::AboutToWait | Breadcrumb::UpdateHandoff
        )
    }
}

/// The pure stall decision, factored out of the sampler loop so it is
/// deterministically testable: a frozen heartbeat is a STALL iff the main thread
/// is sitting in a WORK root (not a park point) and has been frozen at least
/// `threshold` — [`STALL_THRESHOLD`] for the dev lane,
/// [`RELEASE_STALL_THRESHOLD`] for a shipped binary. This is the exact
/// predicate that FLAGS the L0 hazard — a wedge in the `ResizeSettle` reflow
/// that never reaches `AboutToWait`, or a main thread parked forever inside
/// `user_event` on a lazy-init cycle.
fn is_stall_at(bc: Breadcrumb, frozen: Duration, threshold: Duration) -> bool {
    !bc.is_park_point() && frozen >= threshold
}

/// The sampler's detection state machine, split out of the background thread so
/// its behaviour (fire once per contiguous stall, re-arm on progress, never fire
/// at a park point) is testable against a SYNTHETIC clock — no real sleeps, no
/// global logger. The live thread just feeds it real samples.
struct Sampler {
    /// The heartbeat value last observed to advance.
    last_beat: u64,
    /// When the heartbeat last advanced (start of the current frozen span).
    last_advance: Instant,
    /// Whether the current contiguous stall was already reported (fire once).
    reported: bool,
    /// When the current contiguous stall was last reported, for the repeat
    /// cadence. `None` until the first report.
    last_report: Option<Instant>,
    /// How many times THIS contiguous stall has been reported. Lives here, with
    /// the rest of the per-stall state, so it re-arms on recovery: kept in the
    /// sampler loop instead, a second stall hours after the first was announced
    /// as "STALL CONTINUES" and two distinct incidents read as one wedge.
    reports: u32,
    /// The bar this sampler judges against — [`STALL_THRESHOLD`] for the dev
    /// lane, [`RELEASE_STALL_THRESHOLD`] for a shipped binary.
    threshold: Duration,
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
        }
    }

    /// Fold one sample. Returns `Some(bc)` when this sample should be REPORTED:
    /// once when a contiguous stall crosses the threshold, and then once per
    /// [`STALL_REPEAT_INTERVAL`] for as long as it lasts. A main thread that
    /// never comes back is the case this guard exists for, and one line an hour
    /// ago is not the same evidence as a line saying it is still frozen now.
    fn poll(&mut self, now: Instant, cur_beat: u64, bc: Breadcrumb) -> Option<Breadcrumb> {
        if cur_beat != self.last_beat {
            // Progress: the main thread is alive. Reset the stall clock + re-arm.
            self.last_beat = cur_beat;
            self.last_advance = now;
            self.reported = false;
            self.last_report = None;
            self.reports = 0;
            return None;
        }
        if bc.is_park_point() {
            // Idle / startup park: a frozen heartbeat is expected here. Keep the
            // clock reset so leaving idle starts a fresh span.
            self.last_advance = now;
            self.reported = false;
            self.last_report = None;
            self.reports = 0;
            return None;
        }
        let frozen = now.saturating_duration_since(self.last_advance);
        if !is_stall_at(bc, frozen, self.threshold) {
            return None;
        }
        let due = match self.last_report {
            None => !self.reported,
            Some(at) => now.saturating_duration_since(at) >= STALL_REPEAT_INTERVAL,
        };
        if !due {
            return None;
        }
        self.reported = true;
        self.last_report = Some(now);
        self.reports = self.reports.saturating_add(1);
        Some(bc)
    }
}

/// Record that the main thread just entered `bc`. Two relaxed atomic writes — cheap
/// enough to sit on the hot event path in every build. The breadcrumb is stamped
/// BEFORE the heartbeat bumps so the sampler never reads a fresh count against a
/// stale location.
#[inline]
pub fn beat(bc: Breadcrumb) {
    BREADCRUMB.store(bc as u8, Ordering::Relaxed);
    HEARTBEAT.fetch_add(1, Ordering::Relaxed);
}

/// Whether the watchdog sampler should run. EVERY build, unless explicitly
/// switched off with `ATERM_WATCHDOG=off` — see the module header for why a
/// release binary is the build that needs this most.
fn enabled() -> bool {
    !std::env::var("ATERM_WATCHDOG").is_ok_and(|v| {
        let v = v.trim();
        v.eq_ignore_ascii_case("off") || v.eq_ignore_ascii_case("0") || v.is_empty()
    })
}

/// The bar this build judges a stall against: tight when a developer is
/// watching (debug, or an explicit `$ATERM_WATCHDOG`), coarse in a shipped
/// binary where the reader is a user's `aterm.log`.
fn threshold() -> Duration {
    if cfg!(debug_assertions) || std::env::var_os("ATERM_WATCHDOG").is_some() {
        STALL_THRESHOLD
    } else {
        RELEASE_STALL_THRESHOLD
    }
}

/// Whether a detected stall should `process::abort()` (repro / CI) rather than
/// just log. Opt-in via `ATERM_WATCHDOG=abort`.
fn abort_on_stall() -> bool {
    std::env::var("ATERM_WATCHDOG").is_ok_and(|v| v.eq_ignore_ascii_case("abort"))
}

/// Spawn the background stall sampler. Call once from `main` just before
/// `run_app`. A no-op (spawns nothing) when [`enabled`] is false, so release
/// binaries pay nothing. No self-terminate handshake is needed — the process is
/// exiting when this thread would otherwise notice, and it is a daemon by nature.
pub fn start() {
    if !enabled() {
        return;
    }
    let abort = abort_on_stall();
    let threshold = threshold();
    let builder = std::thread::Builder::new().name("aterm-watchdog".into());
    // A spawn failure is non-fatal: the app runs fine without the tripwire.
    let _ = builder.spawn(move || {
        aterm_log::info!(
            "main-thread stall watchdog armed (sample {SAMPLE_INTERVAL:?}, threshold \
             {threshold:?}, repeat {STALL_REPEAT_INTERVAL:?}, abort={abort})"
        );
        let mut sampler =
            Sampler::with_threshold(Instant::now(), HEARTBEAT.load(Ordering::Relaxed), threshold);
        loop {
            std::thread::sleep(SAMPLE_INTERVAL);
            let now = Instant::now();
            let cur = HEARTBEAT.load(Ordering::Relaxed);
            let bc = Breadcrumb::from_u8(BREADCRUMB.load(Ordering::Relaxed));
            if let Some(hit) = sampler.poll(now, cur, bc) {
                let frozen = now.saturating_duration_since(sampler.last_advance);
                if sampler.reports == 1 {
                    aterm_log::error!(
                        "MAIN-THREAD STALL: no heartbeat for {frozen:?} while inside \
                         `{}` — the UI is not responding. Either unbounded work under a \
                         contended lock (the L0 freeze hazard) or a park that will never \
                         end (a lock or lazy-init cycle). This line names the main-loop \
                         root without symbols; a hang report is not required to find it.",
                        hit.name()
                    );
                } else {
                    aterm_log::error!(
                        "MAIN-THREAD STALL CONTINUES: still no heartbeat after {frozen:?} \
                         inside `{}` — this is a wedge, not a slow frame.",
                        hit.name()
                    );
                }
                if abort {
                    std::process::abort();
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// LIVE end-to-end proof that the REAL background sampler thread wakes, sees a
    /// frozen heartbeat sitting on the `ResizeSettle` breadcrumb, and emits the
    /// named stall log line — the exact behaviour a stripped-release freeze needs.
    ///
    /// `#[ignore]` because it (a) sleeps ~1 s and (b) installs the process-global
    /// logger (a `OnceLock`), which would collide with the rest of the suite. Run
    /// it in isolation:
    ///
    /// ```text
    /// ATERM_WATCHDOG=1 cargo test -p aterm-gui -- --ignored --exact \
    ///     watchdog::tests::live_sampler_thread_logs_a_named_resize_stall
    /// ```
    #[test]
    #[ignore = "live: sleeps ~1s and installs the global logger; run with --ignored"]
    fn live_sampler_thread_logs_a_named_resize_stall() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;

        struct Capture {
            fired: Arc<AtomicBool>,
            saw_name: Arc<std::sync::Mutex<String>>,
        }
        impl aterm_log::Log for Capture {
            fn enabled(&self, _m: &aterm_log::Metadata<'_>) -> bool {
                true
            }
            fn log(&self, record: &aterm_log::Record<'_>) {
                let line = format!("{}", record.args());
                if line.contains("MAIN-THREAD STALL") {
                    self.fired.store(true, Ordering::SeqCst);
                    *self.saw_name.lock().unwrap() = line;
                }
            }
            fn flush(&self) {}
        }

        let fired = Arc::new(AtomicBool::new(false));
        let saw = Arc::new(std::sync::Mutex::new(String::new()));
        // Leak the logger to obtain the `&'static` `set_logger` requires.
        let cap: &'static Capture = Box::leak(Box::new(Capture {
            fired: fired.clone(),
            saw_name: saw.clone(),
        }));
        let _ = aterm_log::set_logger(cap);
        aterm_log::set_max_level(aterm_log::LevelFilter::Trace);

        // Ensure the sampler is enabled regardless of debug/release. Routed
        // through the workspace's one lock-scoped env helper.
        aterm_log::env::set("ATERM_WATCHDOG", "1");

        // Enter the resize-settle arm, then STOP beating (simulate the wedge).
        beat(Breadcrumb::ResizeSettle);
        start();

        // Give the sampler (250 ms tick, 500 ms threshold) time to trip.
        std::thread::sleep(Duration::from_millis(1100));

        assert!(
            fired.load(Ordering::SeqCst),
            "watchdog should have logged a stall for the frozen ResizeSettle beat"
        );
        assert!(
            saw.lock().unwrap().contains("ResizeSettle"),
            "the stall line must NAME the breadcrumb; got: {}",
            saw.lock().unwrap()
        );
    }

    #[test]
    fn breadcrumb_round_trips_through_u8() {
        for bc in [
            Breadcrumb::Startup,
            Breadcrumb::AboutToWait,
            Breadcrumb::WindowEvent,
            Breadcrumb::UserEvent,
            Breadcrumb::NewEvents,
            Breadcrumb::ResizeSettle,
        ] {
            assert_eq!(Breadcrumb::from_u8(bc as u8), bc);
            assert!(!bc.name().is_empty());
        }
    }

    #[test]
    fn unknown_u8_folds_to_the_benign_park_point() {
        assert_eq!(Breadcrumb::from_u8(200), Breadcrumb::Startup);
        assert!(Breadcrumb::from_u8(200).is_park_point());
    }

    #[test]
    fn only_idle_and_startup_are_park_points() {
        assert!(Breadcrumb::Startup.is_park_point());
        assert!(Breadcrumb::AboutToWait.is_park_point());
        assert!(!Breadcrumb::WindowEvent.is_park_point());
        assert!(!Breadcrumb::UserEvent.is_park_point());
        assert!(!Breadcrumb::NewEvents.is_park_point());
        assert!(!Breadcrumb::ResizeSettle.is_park_point());
    }

    #[test]
    fn flags_a_wedged_resize_settle_but_not_a_slow_progressing_frame() {
        // The hazard: stuck in the ResizeSettle reflow past the threshold → FLAG.
        assert!(is_stall_at(
            Breadcrumb::ResizeSettle,
            STALL_THRESHOLD,
            STALL_THRESHOLD
        ));
        assert!(is_stall_at(
            Breadcrumb::ResizeSettle,
            STALL_THRESHOLD + Duration::from_secs(42),
            STALL_THRESHOLD
        ));
        assert!(is_stall_at(
            Breadcrumb::WindowEvent,
            STALL_THRESHOLD,
            STALL_THRESHOLD
        ));
        // A sub-threshold freeze (one slow frame) is NOT a stall.
        assert!(!is_stall_at(
            Breadcrumb::ResizeSettle,
            STALL_THRESHOLD - Duration::from_millis(1),
            STALL_THRESHOLD
        ));
        // Idle at a park point, even for a long time, is NEVER a stall.
        assert!(!is_stall_at(
            Breadcrumb::AboutToWait,
            STALL_THRESHOLD + Duration::from_secs(600),
            STALL_THRESHOLD
        ));
        assert!(!is_stall_at(
            Breadcrumb::Startup,
            STALL_THRESHOLD + Duration::from_secs(600),
            STALL_THRESHOLD
        ));
    }

    #[test]
    fn sampler_fires_once_on_a_wedged_resize_and_rearms_after_recovery() {
        // Synthetic clock: the exact archetype — the main thread beats into the
        // ResizeSettle reflow arm, then the heartbeat FREEZES (a 42 s wedge under
        // the term lock). The breadcrumb never advances to AboutToWait.
        let t0 = Instant::now();
        let beat_val = 100; // whatever `beat` left in HEARTBEAT before the wedge
        let mut s = Sampler::with_threshold(t0, beat_val, STALL_THRESHOLD);

        // 250 ms in, still frozen but under threshold → no fire yet.
        assert_eq!(
            s.poll(
                t0 + Duration::from_millis(250),
                beat_val,
                Breadcrumb::ResizeSettle
            ),
            None
        );
        // 600 ms in, past the 500 ms threshold → FLAG, naming ResizeSettle.
        assert_eq!(
            s.poll(
                t0 + Duration::from_millis(600),
                beat_val,
                Breadcrumb::ResizeSettle
            ),
            Some(Breadcrumb::ResizeSettle)
        );
        // Still wedged at 42 s → does NOT spam; one report per contiguous stall.
        assert_eq!(
            s.poll(
                t0 + Duration::from_secs(42),
                beat_val,
                Breadcrumb::ResizeSettle
            ),
            None
        );
        // Main thread recovers (heartbeat advances) then wedges AGAIN → re-arms
        // and fires a second time. Proves the guard keeps standing.
        assert_eq!(
            s.poll(
                t0 + Duration::from_secs(43),
                beat_val + 1,
                Breadcrumb::AboutToWait
            ),
            None
        );
        assert_eq!(
            s.poll(
                t0 + Duration::from_secs(44),
                beat_val + 1,
                Breadcrumb::ResizeSettle
            ),
            Some(Breadcrumb::ResizeSettle)
        );
    }

    #[test]
    fn sampler_never_fires_while_idle_at_a_park_point() {
        // The heartbeat is frozen for ten minutes because the app is IDLE (parked
        // in the OS event wait after about_to_wait). This must never be a stall.
        let t0 = Instant::now();
        let mut s = Sampler::with_threshold(t0, 7, STALL_THRESHOLD);
        for secs in [1u64, 5, 60, 600] {
            assert_eq!(
                s.poll(t0 + Duration::from_secs(secs), 7, Breadcrumb::AboutToWait),
                None,
                "idle park at {secs}s must not fire"
            );
        }
    }

    #[test]
    fn beat_advances_the_heartbeat_and_stamps_the_breadcrumb() {
        let before = HEARTBEAT.load(Ordering::Relaxed);
        beat(Breadcrumb::ResizeSettle);
        assert!(HEARTBEAT.load(Ordering::Relaxed) > before);
        assert_eq!(
            Breadcrumb::from_u8(BREADCRUMB.load(Ordering::Relaxed)),
            Breadcrumb::ResizeSettle
        );
    }

    /// REGRESSION (2026-08-30, v0.65.0): the shipped binary must ARM this. The
    /// watchdog was `cfg!(debug_assertions) || $ATERM_WATCHDOG`, so the release
    /// that parked its main thread inside `user_event` for five hours spawned no
    /// sampler and logged nothing — and the only evidence left was a macOS hang
    /// report that had to be hand-symbolicated against a stripped binary.
    #[test]
    fn the_watchdog_is_armed_unless_explicitly_switched_off() {
        // `enabled()` reads the environment, which is process-global and shared
        // with every other test in this binary — so assert the DECISION, not by
        // mutating the env. With nothing set, it must be on.
        if std::env::var_os("ATERM_WATCHDOG").is_none() {
            assert!(
                enabled(),
                "a shipped build must arm the stall watchdog: silence is what \
                 cost five hours on 2026-08-30"
            );
        }
        // And the shipped bar is coarse enough that a slow frame is never a line,
        // while a permanent park still is.
        assert!(
            RELEASE_STALL_THRESHOLD > STALL_THRESHOLD,
            "the release bar must be the coarser of the two"
        );
        assert!(
            RELEASE_STALL_THRESHOLD < Duration::from_secs(30),
            "a bar this coarse stops being a freeze guard"
        );
    }

    /// The shipped lane judges against [`RELEASE_STALL_THRESHOLD`]: a 1 s frozen
    /// frame is NOT a line (that is a slow frame), a 6 s one is (that is a wedge).
    #[test]
    fn the_release_lane_ignores_a_slow_frame_and_reports_a_wedge() {
        let t0 = Instant::now();
        let mut s = Sampler::with_threshold(t0, 1, RELEASE_STALL_THRESHOLD);
        assert!(
            s.poll(t0 + Duration::from_secs(1), 1, Breadcrumb::WindowEvent)
                .is_none(),
            "one second of main-thread work must not write an error line"
        );
        assert_eq!(
            s.poll(t0 + Duration::from_secs(6), 1, Breadcrumb::WindowEvent),
            Some(Breadcrumb::WindowEvent),
            "six seconds frozen at a WORK root is a wedge and must be named"
        );
    }

    /// A stall that never ends is re-reported on a cadence. One line at the
    /// start proves a wedge happened; the repeats prove it never ended — the
    /// distinction the 0.65.0 log could not make, because it said nothing.
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
        // …and progress re-arms it completely.
        assert!(
            s.poll(t0 + Duration::from_secs(200), 2, Breadcrumb::UserEvent)
                .is_none(),
            "a heartbeat means the main thread is back"
        );
    }

    /// The 0.65.0 breadcrumb, exactly: the automatic apply arrives as
    /// `Wake::ApplyStagedUpdate` inside `user_event`, which is a WORK root, and
    /// the park happens before `UpdateHandoff` is ever stamped. So the guard
    /// covers the state the process was actually in — this is the assertion that
    /// makes "it would have fired" a fact rather than a claim.
    #[test]
    fn the_v065_park_state_is_one_this_watchdog_reports() {
        assert!(
            !Breadcrumb::UserEvent.is_park_point(),
            "user_event is WORK: a frozen heartbeat there is a stall"
        );
        assert!(
            is_stall_at(
                Breadcrumb::UserEvent,
                Duration::from_secs(3588),
                RELEASE_STALL_THRESHOLD
            ),
            "the field hang (3588 s unresponsive, parked in user_event) must be \
             a reported stall in a SHIPPED build"
        );
        // The handoff's own park point stays exempt — a quiesced reader wait is
        // not a wedge, and reporting it would be the noise that gets a guard
        // ignored.
        assert!(Breadcrumb::UpdateHandoff.is_park_point());
    }

    /// TWO SEPARATE WEDGES READ AS TWO. The report counter used to live in the
    /// sampler LOOP rather than in the sampler, so it never re-armed: a stall
    /// hours after the first announced itself as "STALL CONTINUES", and two
    /// distinct incidents read as one. Found by `codex review`.
    #[test]
    fn a_second_stall_after_recovery_reports_as_a_first_again() {
        let t0 = Instant::now();
        let mut s = Sampler::with_threshold(t0, 1, STALL_THRESHOLD);
        assert_eq!(
            s.poll(t0 + Duration::from_secs(1), 1, Breadcrumb::UserEvent),
            Some(Breadcrumb::UserEvent)
        );
        assert_eq!(s.reports, 1, "the first line of the first stall");
        // The main thread comes back…
        assert!(
            s.poll(t0 + Duration::from_secs(2), 2, Breadcrumb::UserEvent)
                .is_none()
        );
        assert_eq!(s.reports, 0, "recovery re-arms the report count");
        // …and wedges again, hours later. That is a NEW incident.
        assert_eq!(
            s.poll(t0 + Duration::from_secs(9000), 2, Breadcrumb::WindowEvent),
            Some(Breadcrumb::WindowEvent)
        );
        assert_eq!(
            s.reports, 1,
            "a separate wedge must announce itself in full, not as a continuation"
        );
    }
}
