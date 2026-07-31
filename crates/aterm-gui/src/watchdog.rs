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
//! ## Off in release
//!
//! [`start`] spawns the sampler ONLY when [`enabled`] — `#[cfg(debug_assertions)]`
//! builds, or any build with `$ATERM_WATCHDOG` set. A release binary with the env
//! unset spawns no thread and reports nothing; [`beat`] stays two relaxed atomic
//! writes either way (negligible on the hot event path). Set
//! `ATERM_WATCHDOG=abort` to `process::abort()` on a detected stall (CI / repro),
//! otherwise it logs at error level and keeps going.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How often the sampler thread wakes to inspect the heartbeat.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(250);

/// How long the heartbeat may stay frozen at a WORK breadcrumb before it counts
/// as a stall. Two sample intervals, so a genuine wedge is caught within ~750 ms
/// while a single slow-but-progressing frame never trips.
const STALL_THRESHOLD: Duration = Duration::from_millis(500);

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
/// [`STALL_THRESHOLD`]. This is the exact predicate that FLAGS the L0 hazard —
/// e.g. a wedge in the `ResizeSettle` reflow that never reaches `AboutToWait`.
fn is_stall(bc: Breadcrumb, frozen: Duration) -> bool {
    !bc.is_park_point() && frozen >= STALL_THRESHOLD
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
}

impl Sampler {
    fn new(now: Instant, beat: u64) -> Self {
        Self {
            last_beat: beat,
            last_advance: now,
            reported: false,
        }
    }

    /// Fold one sample. Returns `Some(bc)` exactly ONCE per contiguous stall (the
    /// breadcrumb to name in the log), else `None`.
    fn poll(&mut self, now: Instant, cur_beat: u64, bc: Breadcrumb) -> Option<Breadcrumb> {
        if cur_beat != self.last_beat {
            // Progress: the main thread is alive. Reset the stall clock + re-arm.
            self.last_beat = cur_beat;
            self.last_advance = now;
            self.reported = false;
            return None;
        }
        if bc.is_park_point() {
            // Idle / startup park: a frozen heartbeat is expected here. Keep the
            // clock reset so leaving idle starts a fresh span.
            self.last_advance = now;
            self.reported = false;
            return None;
        }
        let frozen = now.saturating_duration_since(self.last_advance);
        if is_stall(bc, frozen) && !self.reported {
            self.reported = true;
            return Some(bc);
        }
        None
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

/// Whether the watchdog sampler should run: debug builds, or any build with
/// `$ATERM_WATCHDOG` set. Release binaries with the env unset stay silent.
fn enabled() -> bool {
    cfg!(debug_assertions) || std::env::var_os("ATERM_WATCHDOG").is_some()
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
    let builder = std::thread::Builder::new().name("aterm-watchdog".into());
    // A spawn failure is non-fatal: the app runs fine without the dev tripwire.
    let _ = builder.spawn(move || {
        aterm_log::info!(
            "main-thread stall watchdog armed (sample {SAMPLE_INTERVAL:?}, threshold \
             {STALL_THRESHOLD:?}, abort={abort})"
        );
        let mut sampler = Sampler::new(Instant::now(), HEARTBEAT.load(Ordering::Relaxed));
        loop {
            std::thread::sleep(SAMPLE_INTERVAL);
            let now = Instant::now();
            let cur = HEARTBEAT.load(Ordering::Relaxed);
            let bc = Breadcrumb::from_u8(BREADCRUMB.load(Ordering::Relaxed));
            if let Some(hit) = sampler.poll(now, cur, bc) {
                let frozen = now.saturating_duration_since(sampler.last_advance);
                aterm_log::error!(
                    "MAIN-THREAD STALL: no heartbeat for {frozen:?} while inside \
                     `{}` — likely unbounded work under a contended lock (L0 \
                     freeze hazard).",
                    hit.name()
                );
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
        assert!(is_stall(Breadcrumb::ResizeSettle, STALL_THRESHOLD));
        assert!(is_stall(
            Breadcrumb::ResizeSettle,
            STALL_THRESHOLD + Duration::from_secs(42)
        ));
        assert!(is_stall(Breadcrumb::WindowEvent, STALL_THRESHOLD));
        // A sub-threshold freeze (one slow frame) is NOT a stall.
        assert!(!is_stall(
            Breadcrumb::ResizeSettle,
            STALL_THRESHOLD - Duration::from_millis(1)
        ));
        // Idle at a park point, even for a long time, is NEVER a stall.
        assert!(!is_stall(
            Breadcrumb::AboutToWait,
            STALL_THRESHOLD + Duration::from_secs(600)
        ));
        assert!(!is_stall(
            Breadcrumb::Startup,
            STALL_THRESHOLD + Duration::from_secs(600)
        ));
    }

    #[test]
    fn sampler_fires_once_on_a_wedged_resize_and_rearms_after_recovery() {
        // Synthetic clock: the exact archetype — the main thread beats into the
        // ResizeSettle reflow arm, then the heartbeat FREEZES (a 42 s wedge under
        // the term lock). The breadcrumb never advances to AboutToWait.
        let t0 = Instant::now();
        let beat_val = 100; // whatever `beat` left in HEARTBEAT before the wedge
        let mut s = Sampler::new(t0, beat_val);

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
        let mut s = Sampler::new(t0, 7);
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
}
