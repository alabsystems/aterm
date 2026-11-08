// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The frame clock driver (`examples/objc_frame_clock_drive.rs`, a row of `verify appkit`'s
//! objc drives): a real window's display link (`aterm-window`'s `frame_clock.rs`: `make_link`,
//! `set_running`, `tick_of` and `invalidate`, through the view's `atermFrameTick:` and
//! `end_frame_clock`) running the deck's clock ([`crate::stats_clock::DeckClock`], the clock
//! each window's deck holds, through the port the app asks it by), on a real AppKit
//! (`docs/DESIGN-stats-deck-gpu-motion-2026-10-26.md` §3.4, §5.7).
//!
//! Every tick is judged by the link's own timestamps, as `WindowEvent::FrameTick` carries them:
//! its `target` (`-targetTimestamp`, the vsync its frame reaches) and its `period`
//! (`-targetTimestamp` less `-timestamp`), so its `stamp`, `target - period`, is `-timestamp`,
//! the vsync it fired for. Those are the host clock's, read as `Instant`s, so they compare with
//! the moment the drive paused the link or closed the window, whenever the loop got round to
//! handing the tick over. When the loop handed a tick over (its arrival) is read in two of stage
//! 1's judgements, each tick's `-timestamp` at or before its arrival and the sign of the median
//! and the best tick's lead, and the clock is fed arrival times, as the app feeds it. Whether the
//! link is paused is read off the `CADisplayLink` itself (its `-isPaused`), through a weak
//! reference to the link the window's view holds (`aterm-window`'s debug seam
//! `frame_clock_link`), never off the ticks the view queues. Five stages, in order, each judged on
//! what the window, its link and the clock did:
//!
//! 1. **Real ticks.** The clock runs the window's link and takes `TICKS` of its ticks. Each names
//!    a period the window's screen can run at (0.9 of its shortest to 1.1 of 60 Hz's, the least
//!    and the greatest period both); each targets a later vsync than the last, at least
//!    half the screen's shortest period on; the link never goes [`LINK_SILENCE`] between two of
//!    them; each fired for a vsync that had passed when it arrived; the median tick, and the best,
//!    arrived before the vsync it targets (so most target the coming vsync, not the one just
//!    passed); the link says it runs (`-isPaused` NO); and the clock's hang detector never
//!    tripped.
//! 2. **A paused link sends nothing, and the detector trips on real time.** The clock pauses the
//!    link, and the link says so (`-isPaused` YES). Then the clock's port answers for the window's
//!    real link and keeps it paused, so the clock believes its link runs while the link sends
//!    nothing: the link still says it is paused after that run and when the detector trips, no
//!    tick fires for a vsync after the pause, and the clock's hang detector, read each turn on the
//!    host clock, trips at its deadline ([`LINK_SILENCE`] into the run). The tripped clock is timed
//!    as a window with no link ([`DeckClock::timing_link`], [`deck_clock_at_panel_rate`]), arms no
//!    further detector wake, and the metrics count the window. The app's own loop around the
//!    detector (the deadline `deck_clock_next` arms for it, `service_deck_clocks`,
//!    `sync_deck_clocks`' `watch_link`) is not driven here: the `App` is built only by tests and
//!    benches, so no appkit drive can make one, and the unit test
//!    `stats_clock::tests::a_link_that_never_ticks_falls_back_to_the_timer` runs that loop on the
//!    inert port.
//! 3. **A tick resumes it.** The real port again: the restarted link's first tick puts the clock
//!    back on it, and the link says it runs.
//! 4. **The link ticks through event tracking.** A block the drive puts on the main run loop runs
//!    it in `NSEventTrackingRunLoopMode`, the mode AppKit runs it in through a live resize and a
//!    menu's tracking, outside any handler, as AppKit's own tracking loops do; `TRACK_TICKS` ticks
//!    must reach the drive's handler while it does. A link added to the default mode alone fires
//!    none there. The link still says it runs.
//! 5. **Closing the window invalidates every link its view made.** With the link running (it
//!    says so) and ticking, the drive takes an Objective-C weak reference to each `CADisplayLink`
//!    the view made (`aterm-window`'s debug seam `frame_clock_links`, recorded where its
//!    `make_link` returned each link) and one to the link the view holds (`frame_clock_link`); as
//!    a control, the one it holds loads and is among those made. The window closes, an
//!    autorelease pool drains and the loop turns once; every reference must then load nil. The
//!    main run loop retains a link added to it until `-invalidate`, so a link nothing invalidated
//!    (the one held at the close, or one the view replaced and dropped before it) stays alive
//!    whoever else let it go. And no tick fires for a vsync after the close for `AFTER_CLOSE`.
//!
//! A wait for something that must happen is a one-minute hang detector (`HANG`), never a
//! latency budget, and nothing is retried. Exit 0 every stage held; 1 a finding; 2 the drive
//! could not run here (no event loop, no window, a Mac before 14, no screen showing the window);
//! 3 the watchdog fired, so the drive proved nothing. `frame_clock_link` and `frame_clock_links`
//! are debug seams, so the drive exists in builds with debug assertions (`verify appkit` builds it
//! `--profile test`).
//!
//! [`LINK_SILENCE`]: crate::stats_clock::LINK_SILENCE
//! [`DeckClock::timing_link`]: crate::stats_clock::DeckClock::timing_link
//! [`deck_clock_at_panel_rate`]: crate::stats_clock::deck_clock_at_panel_rate

/// Every stage held.
const PASS: i32 = 0;
/// At least one finding.
const FINDING: i32 = 1;
/// The drive could not run here.
const NOT_RUN: i32 = 2;
/// The watchdog fired: the main thread never came back from AppKit.
const HUNG: i32 = 3;

/// Drive a real window's display link through the deck's clock and return the exit code above.
#[must_use]
pub fn run_frame_clock_drive() -> i32 {
    macos::run(crate::platform::Windowed::launch())
}

mod macos {
    use std::cell::Cell;
    use std::ffi::c_void;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use aterm_objc::cf::{
        CFRunLoopGetMain, CFRunLoopRef, CFRunLoopRunInMode, CFRunLoopWakeUp, kCFRunLoopDefaultMode,
    };
    use aterm_objc::send::{
        send_bool, send_bool_sel, send_id, send_isize, send_usize, send_v, send_v_isize,
    };
    use aterm_objc::{Id, RcBlock, WeakObj, autoreleasepool, sel};
    use aterm_window::application::ApplicationHandler;
    use aterm_window::dpi::{LogicalPosition, LogicalSize};
    use aterm_window::event::WindowEvent;
    use aterm_window::event_loop::{ActiveEventLoop, EventLoop};
    use aterm_window::platform::macos::{frame_clock_link, frame_clock_links};
    use aterm_window::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
    use aterm_window::window::{Window, WindowId as WinitWindowId};

    use super::{FINDING, HUNG, NOT_RUN, PASS};
    use crate::Wake;
    use crate::stats_clock::{DeckClock, LINK_SILENCE, deck_clock_at_panel_rate, window_port};

    /// A wait for something that must happen: a hang detector, never a latency budget.
    const HANG: Duration = Duration::from_secs(60);

    /// The whole drive's watchdog, past the seven [`HANG`]s a run can sit out one after another
    /// (the window, its screen, and one in each stage) and [`AFTER_CLOSE`]: a main thread that
    /// never comes back from AppKit exits [`HUNG`].
    const WATCHDOG: Duration = Duration::from_secs(480);

    /// The ticks stage 1 takes: a second at 120 Hz, two at 60 Hz.
    const TICKS: usize = 120;

    /// The ticks stage 4 needs handed to the drive while the main run loop runs in
    /// `NSEventTrackingRunLoopMode`: a few vsyncs (50 ms at 120 Hz, 100 ms at 60 Hz).
    const TRACK_TICKS: usize = 6;

    /// The ticks stage 5 sees before the window closes: its control that the link runs.
    const TICKS_BEFORE_CLOSE: usize = 10;

    /// How long a closed window is watched for a tick: sixty vsyncs at 60 Hz, which a link that
    /// still ran would fill.
    const AFTER_CLOSE: Duration = Duration::from_secs(1);

    /// One turn of the loop's wait.
    const TURN: Duration = Duration::from_millis(4);

    /// `NSFloatingWindowLevel`: above every normal window, so no other app's window covers the
    /// drive's (a covered window's link need not tick).
    const NS_FLOATING_WINDOW_LEVEL: isize = 3;

    /// `NSWindowOcclusionStateVisible`.
    const NS_WINDOW_OCCLUSION_STATE_VISIBLE: usize = 1 << 1;

    /// The lowest rate `frame_clock.rs` holds a link to, frames per second (its `RATE_MIN`).
    const RATE_MIN: f64 = 60.0;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        /// `void CFRunLoopPerformBlock(CFRunLoopRef, CFTypeRef mode, void (^)(void))`: runs the
        /// block once, on the loop's next turn in `mode`; it copies the block.
        fn CFRunLoopPerformBlock(rl: CFRunLoopRef, mode: *const c_void, block: *mut c_void);
    }

    /// One tick: the link's own timestamps, and when the loop handed it to the driver.
    #[derive(Clone, Copy, Debug)]
    struct Tick {
        /// When the driver's handler ran: the loop's delivery. Two judgements read it, each
        /// tick's `-timestamp` at or before it and the sign of [`target_ahead`]'s median and best
        /// tick's lead, and the clock is fed it ([`feed`], stage 3), as the app feeds its clock.
        arrived: Instant,
        /// `-targetTimestamp`: the vsync this tick's frame reaches.
        target: Instant,
        /// `-targetTimestamp` less `-timestamp`.
        period: Duration,
    }

    impl Tick {
        /// `-timestamp`: the vsync the link fired this tick for.
        fn stamp(&self) -> Instant {
            self.target.checked_sub(self.period).unwrap_or(self.target)
        }

        /// How far its target lies past its arrival, seconds (negative where it arrived after it).
        fn lead(&self) -> f64 {
            signed(self.target, self.arrived)
        }
    }

    /// `a - b`, seconds.
    fn signed(a: Instant, b: Instant) -> f64 {
        if a >= b {
            a.duration_since(b).as_secs_f64()
        } else {
            -b.duration_since(a).as_secs_f64()
        }
    }

    struct Driver {
        window: Option<Window>,
        id: Option<WinitWindowId>,
        ticks: Vec<Tick>,
        blocked: Option<String>,
        tracking: Rc<Tracking>,
    }

    /// Stage 4's state, shared by the block that runs the main run loop in
    /// `NSEventTrackingRunLoopMode` ([`track`]) and the driver's handler.
    #[derive(Default)]
    struct Tracking {
        /// The block's loop runs the main run loop in the tracking mode now.
        now: Cell<bool>,
        /// The ticks handed to the driver while it did.
        ticks: Cell<usize>,
        /// How long the block ran the tracking mode, once it returned.
        ran: Cell<Option<Duration>>,
    }

    impl ApplicationHandler<Wake> for Driver {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            if self.window.is_some() || self.blocked.is_some() {
                return;
            }
            let attrs = Window::default_attributes()
                .with_title("objc-frame-clock-drive")
                .with_inner_size(LogicalSize::new(480.0, 320.0))
                .with_position(LogicalPosition::new(80.0, 80.0))
                .with_visible(true);
            match el.create_window(attrs) {
                Ok(w) => {
                    self.id = Some(w.id());
                    self.window = Some(w);
                }
                Err(e) => self.blocked = Some(format!("no window: {e}")),
            }
        }

        fn window_event(&mut self, _: &ActiveEventLoop, id: WinitWindowId, event: WindowEvent) {
            if let WindowEvent::FrameTick { target, period } = event
                && self.id == Some(id)
            {
                self.ticks.push(Tick {
                    arrived: Instant::now(),
                    target,
                    period,
                });
                if self.tracking.now.get() {
                    self.tracking.ticks.set(self.tracking.ticks.get() + 1);
                }
            }
        }

        fn user_event(&mut self, _: &ActiveEventLoop, _: Wake) {}
    }

    /// Turns the loop until `done` holds or `limit` passes; whether `done` held.
    fn pump_until(
        el: &mut EventLoop<Wake>,
        d: &mut Driver,
        limit: Duration,
        mut done: impl FnMut(&mut Driver) -> bool,
    ) -> bool {
        let start = Instant::now();
        loop {
            if done(d) {
                return true;
            }
            if start.elapsed() >= limit {
                return false;
            }
            if let PumpStatus::Exit(_) = el.pump_app_events(Some(TURN), d) {
                return done(d);
            }
        }
    }

    /// Turns the loop for `span`, whatever arrives.
    fn pump_for(el: &mut EventLoop<Wake>, d: &mut Driver, span: Duration) {
        let _ = pump_until(el, d, span, |_| false);
    }

    /// The window's screen's top rate, frames per second (`-[NSScreen maximumFramesPerSecond]`,
    /// macOS 12), and whether a screen shows the window now (`-occlusionState`); `None` with no
    /// screen.
    fn screen_of(win: &Window) -> Option<(isize, bool)> {
        let ns_window = crate::platform::ns_window_of(win);
        if ns_window.is_null() {
            return None;
        }
        // SAFETY: `ns_window` is the window's live `NSWindow`, on the main thread; `-screen` is
        // `@16@0:8` (+0, nil off every screen), `-occlusionState` `Q16@0:8`, and the screen
        // answers `-respondsToSelector:` (`B24@0:8:16`) before `-maximumFramesPerSecond`
        // (`q16@0:8`).
        unsafe {
            let screen = send_id(ns_window, sel!(screen));
            if screen.is_null() {
                return None;
            }
            let rate = if send_bool_sel(
                screen,
                sel!(respondsToSelector:),
                sel!(maximumFramesPerSecond),
            ) {
                send_isize(screen, sel!(maximumFramesPerSecond))
            } else {
                60
            };
            let shown = send_usize(ns_window, sel!(occlusionState))
                & NS_WINDOW_OCCLUSION_STATE_VISIBLE
                != 0;
            Some((rate, shown))
        }
    }

    /// Floats the window above every normal one and orders it front, active app or not.
    fn float(win: &Window) {
        let ns_window: Id = crate::platform::ns_window_of(win);
        if ns_window.is_null() {
            return;
        }
        // SAFETY: `ns_window` is the window's live `NSWindow`, on the main thread; `-setLevel:` is
        // `v24@0:8q16` and `-orderFrontRegardless` `v16@0:8`.
        unsafe {
            send_v_isize(ns_window, sel!(setLevel:), NS_FLOATING_WINDOW_LEVEL);
            send_v(ns_window, sel!(orderFrontRegardless));
        }
    }

    /// Whether the `CADisplayLink` the window's view holds says it is paused (its own
    /// `-isPaused`), loaded through the seam's weak reference; `None` with no link.
    fn link_paused(win: &Window) -> Option<bool> {
        let link = frame_clock_link(win)?.load()?;
        // SAFETY: `link` is a live `CADisplayLink` at +1, on the main thread; `-isPaused` is
        // `B16@0:8` on it.
        Some(unsafe { send_bool(link.id(), sel!(isPaused)) })
    }

    /// `link_paused` as words.
    fn paused_words(paused: Option<bool>) -> &'static str {
        match paused {
            Some(true) => "YES",
            Some(false) => "NO",
            None => "no link",
        }
    }

    /// Puts a block on the main run loop's default mode, which the loop's next turn runs outside
    /// any handler, as AppKit runs its own tracking loops inside `-sendEvent:`. The block runs the
    /// main run loop in `NSEventTrackingRunLoopMode`, a turn at a time, until [`TRACK_TICKS`] ticks
    /// reached the driver there or [`HANG`] passed, and records how long it ran; whether the block
    /// was put there.
    fn track(tracking: &Rc<Tracking>) -> bool {
        let t = Rc::clone(tracking);
        // SAFETY: `CFRunLoopPerformBlock` invokes a `void (^)(void)`, which is what `new0` over a
        // closure of no arguments returning `()` builds. Inside it, `NSEventTrackingRunLoopMode`
        // is AppKit's immortal `NSString *` constant, toll-free bridged to the `CFStringRef` mode
        // `CFRunLoopRunInMode` takes; the block runs on the main thread, the loop it runs.
        let block = unsafe {
            RcBlock::new0(move || {
                let start = Instant::now();
                let mode = aterm_macos_ui::appkit::event_tracking_run_loop_mode()
                    .as_ptr()
                    .cast_const()
                    .cast();
                t.now.set(true);
                while t.ticks.get() < TRACK_TICKS && start.elapsed() < HANG {
                    let _ = CFRunLoopRunInMode(mode, TURN.as_secs_f64(), 0);
                }
                t.now.set(false);
                t.ran.set(Some(start.elapsed()));
            })
        };
        let Some(block) = block else {
            return false;
        };
        // SAFETY: the main run loop is live for the process; the mode is CoreFoundation's
        // immortal `kCFRunLoopDefaultMode`; `CFRunLoopPerformBlock` copies the block, so this
        // holder's reference may go on return.
        unsafe {
            let main = CFRunLoopGetMain();
            CFRunLoopPerformBlock(main, kCFRunLoopDefaultMode.cast(), block.as_ptr());
            CFRunLoopWakeUp(main);
        }
        true
    }

    /// A port to a link that never ticks: it answers for the window's real link (whether the
    /// window has one) and keeps it paused whatever the clock asks, so a run the clock starts
    /// sees no tick, as a link the OS never fires.
    fn silent_link(window: Option<&Window>, _want: bool) -> bool {
        window.is_some_and(|w| w.set_frame_clock(false))
    }

    fn median(mut xs: Vec<f64>) -> Option<f64> {
        xs.sort_by(f64::total_cmp);
        xs.get(xs.len() / 2).copied()
    }

    fn ms(d: Duration) -> f64 {
        d.as_secs_f64() * 1e3
    }

    /// `at - from` in milliseconds, or `none`.
    fn ms_since(at: Option<Instant>, from: Instant) -> String {
        at.map_or_else(
            || "none".to_owned(),
            |at| format!("{:.3} ms", ms(at.saturating_duration_since(from))),
        )
    }

    /// The ticks the link fired for a vsync after `at`: by their own `-timestamp`, so a tick
    /// fired before `at` and handed over after it is not one.
    fn fired_after(ticks: &[Tick], at: Instant) -> usize {
        ticks.iter().filter(|t| t.stamp() > at).count()
    }

    /// Of `ticks`, how many arrived before the vsync they target, and the best and median lead,
    /// seconds: the loop hands a tick over after its `-timestamp`, so a tick that names that
    /// vsync (`-timestamp`, the one just passed) or an earlier one as its target is not ahead of
    /// it, and the median is ahead only where most ticks target a vsync still to come.
    fn target_ahead(ticks: &[Tick]) -> (usize, f64, f64) {
        let leads: Vec<f64> = ticks.iter().map(Tick::lead).collect();
        let ahead = leads.iter().filter(|&&l| l > 0.0).count();
        let best = leads.iter().copied().fold(f64::MIN, f64::max);
        (ahead, best, median(leads).unwrap_or(f64::MIN))
    }

    /// What the drive found.
    #[derive(Default)]
    struct Verdict {
        findings: usize,
    }

    impl Verdict {
        fn check(&mut self, ok: bool, what: &str) {
            if ok {
                println!("  ok   {what}");
            } else {
                println!("  FAIL {what}");
                self.findings += 1;
            }
        }
    }

    /// Feeds the clock each tick it has not seen that the link fired for a vsync after `since`, as
    /// the app's `on_deck_frame_tick` does, and runs its hang detector, as the app's
    /// `sync_deck_clocks` does each turn; how many times it fell back and resumed.
    fn feed(clock: &mut DeckClock, d: &Driver, seen: &mut usize, since: Instant) -> (u32, u32) {
        let (mut fell, mut resumed) = (0, 0);
        for t in d.ticks[*seen..].iter().filter(|t| t.target > since) {
            resumed += u32::from(clock.link_ticked(t.arrived));
        }
        *seen = d.ticks.len();
        fell += u32::from(clock.watch_link(Instant::now()));
        (fell, resumed)
    }

    /// Stage 1's judgement of the ticks the running link sent (`top` and `floor`, the shortest
    /// and the longest period the screen's link runs at, seconds).
    fn judge_ticks(v: &mut Verdict, ticks: &[Tick], top: f64, floor: f64) {
        let periods: Vec<f64> = ticks.iter().map(|t| t.period.as_secs_f64()).collect();
        let (lo, hi) = periods
            .iter()
            .fold((f64::MAX, 0.0_f64), |(lo, hi), &p| (lo.min(p), hi.max(p)));
        let period = median(periods).unwrap_or(0.0);
        v.check(
            lo >= top * 0.9 && hi <= floor * 1.1,
            &format!(
                "each tick names a period the screen runs: min {:.3} ms and max {:.3} (median \
                 {:.3}) within {:.2}..{:.2} ms",
                lo * 1e3,
                hi * 1e3,
                period * 1e3,
                top * 0.9e3,
                floor * 1.1e3
            ),
        );
        let gaps: Vec<f64> = ticks
            .windows(2)
            .map(|w| signed(w[1].target, w[0].target))
            .collect();
        let least = gaps.iter().copied().fold(f64::MAX, f64::min);
        let most = gaps.iter().copied().fold(f64::MIN, f64::max);
        v.check(
            least >= top * 0.5,
            &format!(
                "each tick targets a later vsync than the last: the targets' least gap {:.3} ms \
                 (median {:.3}), at least {:.2} ms, half the screen's shortest period",
                least * 1e3,
                median(gaps).unwrap_or(0.0) * 1e3,
                top * 0.5e3
            ),
        );
        v.check(
            most < LINK_SILENCE.as_secs_f64(),
            &format!(
                "the link never stalled: the targets' largest gap {:.3} ms, under the clock's \
                 hang detector's {LINK_SILENCE:?}",
                most * 1e3
            ),
        );
        let passed = ticks.iter().filter(|t| t.stamp() <= t.arrived).count();
        let worst = ticks
            .iter()
            .map(|t| signed(t.arrived, t.stamp()))
            .fold(f64::MAX, f64::min);
        v.check(
            passed == ticks.len(),
            &format!(
                "each tick fired for a vsync that had passed when it arrived (its -timestamp at or \
                 before its arrival): {passed} of {}, the closest {:.3} ms before",
                ticks.len(),
                worst * 1e3
            ),
        );
        let (ahead, best, mid) = target_ahead(ticks);
        v.check(
            mid > 0.0 && best > 0.0,
            &format!(
                "the ticks target the coming vsync, not the one just passed: the median tick \
                 arrived {:.3} ms before its target and the best {:.3} ms ({ahead} of {} arrived \
                 before their target)",
                mid * 1e3,
                best * 1e3,
                ticks.len()
            ),
        );
    }

    /// The judgement that the window's link runs, by its own `-isPaused`, in `stage`.
    fn check_runs(v: &mut Verdict, win: Option<&Window>, stage: &str) {
        let paused = win.and_then(link_paused);
        v.check(
            paused == Some(false),
            &format!(
                "{stage}, the link says it runs: -isPaused {}",
                paused_words(paused)
            ),
        );
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one linear drive of one window: each stage starts from the link and the clock \
                  the stage before it left"
    )]
    pub(super) fn run(_windowed: crate::platform::Windowed) -> i32 {
        if let Some(why) = crate::driver_display_unavailable() {
            crate::logging::stderr_line!("objc-frame-clock-drive: NOT RUN — {why}");
            return NOT_RUN;
        }
        std::thread::spawn(|| {
            std::thread::sleep(WATCHDOG);
            crate::logging::stderr_line!(
                "WATCHDOG: objc-frame-clock-drive hung (the main thread never came back from AppKit)"
            );
            std::process::exit(HUNG);
        });
        let mut el = match crate::quiet_driver_event_loop_builder::<Wake>().build() {
            Ok(el) => el,
            Err(e) => {
                crate::logging::stderr_line!(
                    "objc-frame-clock-drive: NOT RUN — no event loop: {e}"
                );
                return NOT_RUN;
            }
        };
        let mut d = Driver {
            window: None,
            id: None,
            ticks: Vec::new(),
            blocked: None,
            tracking: Rc::default(),
        };
        let made = pump_until(&mut el, &mut d, HANG, |d| {
            d.window.is_some() || d.blocked.is_some()
        });
        let Some(win) = d.window.as_ref() else {
            crate::logging::stderr_line!(
                "objc-frame-clock-drive: NOT RUN — {}",
                d.blocked
                    .take()
                    .unwrap_or_else(|| format!("no window within {HANG:?} (made: {made})"))
            );
            return NOT_RUN;
        };
        float(win);
        let mut clock = DeckClock::default();
        clock.ask_link(Some(win));
        if clock.has_link() != Some(true) {
            crate::logging::stderr_line!(
                "objc-frame-clock-drive: NOT RUN — the window's view makes no display link \
                 (-[NSView displayLinkWithTarget:selector:] is macOS 14 and later)"
            );
            return NOT_RUN;
        }
        let shown = pump_until(&mut el, &mut d, HANG, |d| {
            d.window
                .as_ref()
                .and_then(screen_of)
                .is_some_and(|(_, shown)| shown)
        });
        let screen = d.window.as_ref().and_then(screen_of);
        let Some((rate, true)) = screen.filter(|_| shown) else {
            crate::logging::stderr_line!(
                "objc-frame-clock-drive: NOT RUN — no screen shows the window within {HANG:?} \
                 (a display asleep, a locked session, or no display): {screen:?}"
            );
            return NOT_RUN;
        };
        let top = 1.0 / rate.max(1) as f64;
        let floor = 1.0 / (rate.max(1) as f64).min(RATE_MIN);
        println!(
            "objc-frame-clock-drive: the window's screen runs at up to {rate} Hz; its link at \
             {:.2}..{:.2} ms a period",
            top * 1e3,
            floor * 1e3
        );
        let mut v = Verdict::default();

        // 1. Real ticks.
        let win = d.window.as_ref().expect("the window, made above");
        let ran = Instant::now();
        clock.steer(Some(win), true, ran);
        let mut seen = 0;
        let (mut fell, mut resumed) = (0, 0);
        let took = pump_until(&mut el, &mut d, HANG, |d| {
            let (f, r) = feed(&mut clock, d, &mut seen, ran);
            fell += f;
            resumed += r;
            d.ticks.len() >= TICKS
        });
        let ticks = d.ticks.clone();
        v.check(
            took,
            &format!(
                "the running link ticked: {} ticks (of {TICKS}) within {HANG:?}, the first {} \
                 after the run began",
                ticks.len(),
                ms_since(ticks.first().map(|t| t.arrived), ran)
            ),
        );
        if ticks.len() >= 2 {
            judge_ticks(&mut v, &ticks, top, floor);
        }
        check_runs(&mut v, d.window.as_ref(), "While the clock runs it");
        v.check(
            fell == 0 && resumed == 0 && clock.on_link() && !clock.silent(),
            &format!(
                "the clock kept the link's time throughout: its hang detector tripped {fell} \
                 time(s), and a tick resumed it {resumed} time(s)"
            ),
        );

        // 2. A paused link sends nothing, and the detector trips on real time.
        let win = d.window.as_ref().expect("the window");
        clock.steer(Some(win), false, Instant::now());
        let paused = Instant::now();
        let on_pause = link_paused(win);
        clock.port = silent_link;
        let started = Instant::now();
        let win = d.window.as_ref().expect("the window");
        clock.steer(Some(win), true, started);
        let on_silent_run = link_paused(win);
        let armed = clock.silence_deadline();
        let mut fell_at = None;
        let fell = pump_until(&mut el, &mut d, HANG, |_| {
            let now = Instant::now();
            if clock.watch_link(now) {
                fell_at = Some(now);
            }
            fell_at.is_some()
        });
        let on_trip = d.window.as_ref().and_then(link_paused);
        seen = d.ticks.len();
        v.check(
            [on_pause, on_silent_run, on_trip]
                .iter()
                .all(|&p| p == Some(true)),
            &format!(
                "the clock's pause paused the link itself, and the silent port's run kept it so: \
                 -isPaused {} after the pause, {} after the silent port's run, {} when the \
                 detector tripped",
                paused_words(on_pause),
                paused_words(on_silent_run),
                paused_words(on_trip)
            ),
        );
        let late = fired_after(&d.ticks, paused);
        v.check(
            late == 0,
            &format!(
                "the link kept paused fired no tick: {late} for a vsync after the pause, by their \
                 own timestamps, over the {:.1} ms that followed it",
                ms(paused.elapsed())
            ),
        );
        v.check(
            fell && armed.is_some_and(|at| at > started && at <= started + LINK_SILENCE),
            &format!(
                "the clock's hang detector, read each turn on the host clock, tripped at its \
                 deadline within {HANG:?}: armed {} into the run (LINK_SILENCE {LINK_SILENCE:?}, \
                 less the silence since the link's last tick), tripped {} in",
                ms_since(armed, started),
                ms_since(fell_at, started)
            ),
        );
        v.check(
            clock.silent()
                && clock.timing_link() == Some(false)
                && !deck_clock_at_panel_rate(true, clock.timing_link(), false),
            "the tripped clock times the window as one with no link: its deck frames at the \
             effect lane's interval",
        );
        v.check(
            clock.silence_deadline().is_none() && !clock.watch_link(Instant::now()),
            "the tripped clock arms no further detector wake and trips once",
        );
        v.check(
            crate::metrics::deck_links_silent() == 1,
            &format!(
                "the metrics count the window ({} silent link(s))",
                crate::metrics::deck_links_silent()
            ),
        );

        // 3. A tick resumes it.
        clock.port = window_port;
        let win = d.window.as_ref().expect("the window");
        clock.steer(Some(win), false, Instant::now());
        let restarted = Instant::now();
        clock.steer(Some(win), true, restarted);
        let mut back = false;
        let resumed = pump_until(&mut el, &mut d, HANG, |d| {
            for t in d.ticks[seen..].iter().filter(|t| t.target > restarted) {
                back |= clock.link_ticked(t.arrived);
            }
            seen = d.ticks.len();
            back
        });
        v.check(
            resumed && clock.on_link() && clock.timing_link() == Some(true),
            &format!("the restarted link's first tick put the clock back on it within {HANG:?}"),
        );
        v.check(
            crate::metrics::deck_links_silent() == 0,
            "the metrics let the window go",
        );
        check_runs(&mut v, d.window.as_ref(), "Restarted");

        // 4. The link ticks through event tracking.
        let scheduled = track(&d.tracking);
        let tracked =
            scheduled && pump_until(&mut el, &mut d, HANG, |d| d.tracking.ran.get().is_some());
        let in_tracking = d.tracking.ticks.get();
        v.check(
            tracked && in_tracking >= TRACK_TICKS,
            &format!(
                "the link ticks through event tracking: {in_tracking} tick(s) (of {TRACK_TICKS}) \
                 reached the drive while the main run loop ran in NSEventTrackingRunLoopMode, \
                 the mode of a live resize and a menu's tracking, for {} (a {HANG:?} hang \
                 detector; the block {})",
                d.tracking
                    .ran
                    .get()
                    .map_or_else(|| "no time".to_owned(), |r| format!("{:.1} ms", ms(r))),
                if !scheduled {
                    "was never made"
                } else if tracked {
                    "ran"
                } else {
                    "never ran"
                }
            ),
        );
        check_runs(&mut v, d.window.as_ref(), "Through event tracking");

        // 5. Closing the window invalidates every link its view made.
        let running = d.ticks.len();
        let ticking = pump_until(&mut el, &mut d, HANG, |d| {
            d.ticks.len() >= running + TICKS_BEFORE_CLOSE
        });
        check_runs(&mut v, d.window.as_ref(), "Before the close");
        let link = d.window.as_ref().and_then(frame_clock_link);
        let made = d.window.as_ref().map(frame_clock_links).unwrap_or_default();
        // The held link at +1 only inside this block: a strong reference across the close would
        // keep it alive.
        let (held, recorded) = {
            let held = link.as_ref().and_then(WeakObj::load);
            let recorded = held.as_ref().is_some_and(|h| {
                made.iter()
                    .any(|m| m.load().is_some_and(|m| m.id() == h.id()))
            });
            (held.is_some(), recorded)
        };
        v.check(
            ticking && held && recorded,
            &format!(
                "CONTROL: the link ticks before the close ({} ticks within {HANG:?}); a weak \
                 reference to the CADisplayLink the view holds loads it ({}), and it is among \
                 the {} link(s) the view made ({})",
                d.ticks.len() - running,
                if held { "live" } else { "none" },
                made.len(),
                if recorded { "recorded" } else { "missing" }
            ),
        );
        autoreleasepool(|_| drop(d.window.take()));
        let closed_at = Instant::now();
        let _ = autoreleasepool(|_| el.pump_app_events(Some(TURN), &mut d));
        let left = link.as_ref().map(WeakObj::is_live);
        let alive = made.iter().filter(|m| m.is_live()).count();
        v.check(
            left == Some(false) && alive == 0 && !made.is_empty(),
            &format!(
                "closing the window invalidated every link its view made: after an \
                 autorelease-pool drain and a turn of the loop {alive} of the {} weak \
                 reference(s) to them load a link, and the one to the link it held at the close \
                 loads {} (the run loop retains a link until -invalidate)",
                made.len(),
                match left {
                    Some(true) => "the link, still alive",
                    Some(false) => "nil",
                    None => "nothing: there was no link to watch",
                }
            ),
        );
        drop(clock);
        pump_for(&mut el, &mut d, AFTER_CLOSE);
        let after = fired_after(&d.ticks, closed_at);
        v.check(
            after == 0,
            &format!(
                "no tick fired for a vsync after the close, by their own timestamps: {after} in \
                 the {AFTER_CLOSE:?} after it"
            ),
        );

        if v.findings == 0 {
            println!(
                "objc-frame-clock-drive: PASS — the real link ran the deck's clock at the \
                 display's period, targeting the coming vsync and never stalling; paused, it said \
                 so and fired nothing while the clock's detector tripped on real time, and its \
                 next tick resumed the clock; it ticked through NSEventTrackingRunLoopMode; and \
                 the close invalidated every link the view made"
            );
            PASS
        } else {
            println!(
                "objc-frame-clock-drive: FAIL — {} finding(s) above",
                v.findings
            );
            FINDING
        }
    }
}
