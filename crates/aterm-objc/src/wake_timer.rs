// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE RUN-LOOP WAKE TIMER — the one `CFRunLoopTimer` aterm arms, and why its
//! repeat interval is a year.
//!
//! # The 2026-09-26 freeze
//!
//! aterm 0.93.0 (pid 3497) froze for minutes with the main thread ON-CPU in
//! `__CFRunLoopDoTimer + 1220` in all eleven samples of the hang report, and no
//! aterm frame beneath `-[NSApplication run]` (symbolicated against the
//! release's own dSYM: `aterm_gui::main_entry` → winit's
//! `EventLoop::run_on_demand` → `-[NSApplication run]` → … →
//! `__CFRunLoopDoTimers` → `__CFRunLoopDoTimer`). Disassembled on the same
//! macOS build (26.6.2, 25G83), that offset is the `add` of CoreFoundation's
//! catch-up walk, run after a repeating timer's callout returns:
//!
//! ```text
//! nextFireTSR = oldFireTSR;
//! while (nextFireTSR <= mach_absolute_time_at_entry) nextFireTSR += intervalTSR;
//! ```
//!
//! — one iteration per interval of LATENESS, with the run-loop lock held, so
//! every thread that touches the main run loop (`CFRunLoopWakeUp` from the PTY
//! reader and the control workers, `CFRunLoopPerformBlock` from AppKit's own
//! event thread) parks behind it. The timer was winit's `EventLoopWaker`,
//! created with a 0.1 µs interval "to mimic polling": 2 ticks of the 24 MHz
//! timebase. The process had not been scheduled for 95,674 s (every thread's
//! "last ran" in the report is either ~43 s or ≥ 95,881 s old; the system
//! itself was awake — `kern.waketime` was eight days earlier), so the armed
//! timer came due 26.6 hours late and CoreFoundation set out to walk
//! 1.1 × 10¹² steps. Measured on the same Mac with CoreFoundation itself: an
//! overdue timer at that interval costs 59 ms of spin per 20 s of lateness —
//! ~4.7 minutes for the incident's gap. The same timer at the interval below
//! takes ONE step and no measurable time.
//!
//! # The rule, and what enforces it
//!
//! A repeating `CFRunLoopTimer`'s interval bounds how much work CoreFoundation
//! does for lateness it did not cause. So the waker's interval is not "fast
//! enough to poll" — polling is done by RE-ARMING, which is what a waker is for
//! — it is [`WAKE_TIMER_REPEAT_SECS`], a year: any lateness a process can
//! survive costs at most one step. After it fires, the timer is SPENT (its next
//! date a year out) until the event loop arms it again, and the callout records
//! that, so [`WakePlan`] never trusts a cached "already armed for now" that
//! CoreFoundation has used up — the lost wake that would otherwise replace the
//! spin (model: `aterm_spec::derive::run_loop_waker_model`).
//!
//! This module is the ONE place in the tree that creates a run-loop timer
//! (`tools/grep_guard.sh` B15 fences `CFRunLoopTimerCreate` and the `NSTimer`
//! constructors to it), so a new timer is argued for here, beside the arithmetic
//! that prices it, instead of being copied in with a microsecond interval.

use std::cell::Cell;
use std::ffi::c_void;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::declare::MainThread;

/// The repeat interval handed to CoreFoundation, in seconds: one Julian year.
///
/// Not a cadence — the waker never waits for a repeat. It is the bound on
/// CoreFoundation's catch-up walk ([`cf_catch_up_steps`]): lateness shorter than
/// this costs one step, however long the process was stopped, suspended under a
/// debugger or starved. CoreFoundation clamps intervals above ~16 years itself,
/// so this stays inside the range it honours as written.
pub const WAKE_TIMER_REPEAT_SECS: f64 = 31_557_600.0;

/// How the waker was last armed, as the event loop asked for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeTarget {
    /// Wake at once (`ControlFlow::Poll`, or a deadline already past).
    Now,
    /// Wake at this instant.
    At(Instant),
}

/// What the timer must be told after a [`WakePlan`] decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeArm {
    /// Nothing: the timer is already armed exactly as asked and has not fired.
    Keep,
    /// Fire at once.
    Now,
    /// Fire at this instant.
    At(Instant),
    /// Park the timer beyond any horizon.
    Disarm,
}

/// The waker's decision state, with no CoreFoundation in it — the part the
/// derived model binds to, and the part a regression would break.
///
/// `armed` is what the event loop last asked for; `spent` is whether the timer
/// has fired since. A spent timer is parked a year out, so an unchanged request
/// must still re-arm it: `armed == Some(Now)` with `spent` set is NOT "already
/// armed", and treating it as one loses every wake after the first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WakePlan {
    armed: Option<WakeTarget>,
    spent: bool,
}

impl WakePlan {
    /// A plan for a timer created disarmed.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            armed: None,
            spent: false,
        }
    }

    /// What the event loop last asked for (`None`: nothing, or disarmed).
    #[must_use]
    pub const fn armed(&self) -> Option<WakeTarget> {
        self.armed
    }

    /// Whether the timer fired since it was last armed, i.e. is parked a year
    /// out until the next arm.
    #[must_use]
    pub const fn spent(&self) -> bool {
        self.spent
    }

    /// The timer's callout ran: whatever it was armed for is used up.
    pub fn fired(&mut self) {
        if self.armed.is_some() {
            self.spent = true;
        }
    }

    /// No wake wanted (`ControlFlow::Wait`).
    pub fn stop(&mut self) -> WakeArm {
        if self.armed.is_none() {
            return WakeArm::Keep;
        }
        self.armed = None;
        self.spent = false;
        WakeArm::Disarm
    }

    /// Wake at once.
    pub fn start(&mut self) -> WakeArm {
        self.arm(WakeTarget::Now)
    }

    /// Wake at `at` (at once if it is not after `now`), or never for `None`.
    pub fn start_at(&mut self, now: Instant, at: Option<Instant>) -> WakeArm {
        match at {
            None => self.stop(),
            Some(at) if now >= at => self.start(),
            Some(at) => self.arm(WakeTarget::At(at)),
        }
    }

    fn arm(&mut self, target: WakeTarget) -> WakeArm {
        if !self.spent && self.armed == Some(target) {
            return WakeArm::Keep;
        }
        self.armed = Some(target);
        self.spent = false;
        match target {
            WakeTarget::Now => WakeArm::Now,
            WakeTarget::At(at) => WakeArm::At(at),
        }
    }
}

/// How many times CoreFoundation's catch-up walk runs for a repeating timer that
/// fires `lag_ticks` after its due date, at `interval_ticks` per repeat — the
/// loop quoted in the module header (`while next <= now { next += interval }`,
/// starting from the missed date). `None` for a zero interval, which
/// CoreFoundation refuses by halting the process ("A CFRunLoopTimer with an
/// interval of 0 is set to repeat").
#[must_use]
pub const fn cf_catch_up_steps(lag_ticks: u64, interval_ticks: u64) -> Option<u64> {
    if interval_ticks == 0 {
        return None;
    }
    Some(lag_ticks / interval_ticks + 1)
}

/// `secs` in ticks of a clock running at `ticks_per_sec`, truncated as
/// CoreFoundation's `__CFTimeIntervalToTSR` truncates (0.1 µs is 2 ticks, not
/// 2.4, at Apple silicon's 24 MHz).
#[must_use]
pub fn interval_ticks(secs: f64, ticks_per_sec: f64) -> u64 {
    let ticks = secs * ticks_per_sec;
    if ticks.is_nan() || ticks <= 0.0 {
        0
    } else if ticks >= u64::MAX as f64 {
        u64::MAX
    } else {
        ticks as u64
    }
}

/// `mach_absolute_time`'s rate on this machine, in ticks per second.
#[must_use]
pub fn mach_ticks_per_sec() -> f64 {
    #[repr(C)]
    struct TimebaseInfo {
        numer: u32,
        denom: u32,
    }
    unsafe extern "C" {
        fn mach_timebase_info(info: *mut TimebaseInfo) -> i32;
    }
    let mut info = TimebaseInfo { numer: 0, denom: 0 };
    // SAFETY: `info` is a live, correctly laid out out-parameter.
    let ok = unsafe { mach_timebase_info(&raw mut info) };
    if ok != 0 || info.numer == 0 || info.denom == 0 {
        // Nanosecond ticks: the rate every Intel Mac reports.
        return 1e9;
    }
    1e9 * f64::from(info.denom) / f64::from(info.numer)
}

type CFRunLoopRef = *mut c_void;
type CFRunLoopTimerRef = *mut c_void;
type CFStringRef = *const c_void;

#[repr(C)]
struct CFRunLoopTimerContext {
    version: isize,
    info: *mut c_void,
    retain: Option<extern "C" fn(*const c_void) -> *const c_void>,
    release: Option<extern "C" fn(*const c_void)>,
    copy_description: Option<extern "C" fn(*const c_void) -> CFStringRef>,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopCommonModes: CFStringRef;
    static kCFRunLoopDefaultMode: CFStringRef;
    fn CFRunLoopGetMain() -> CFRunLoopRef;
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFAbsoluteTimeGetCurrent() -> f64;
    fn CFRunLoopTimerCreate(
        allocator: *const c_void,
        fire_date: f64,
        interval: f64,
        flags: usize,
        order: isize,
        callout: extern "C" fn(CFRunLoopTimerRef, *mut c_void),
        context: *mut CFRunLoopTimerContext,
    ) -> CFRunLoopTimerRef;
    fn CFRunLoopAddTimer(rl: CFRunLoopRef, timer: CFRunLoopTimerRef, mode: CFStringRef);
    fn CFRunLoopTimerSetNextFireDate(timer: CFRunLoopTimerRef, fire_date: f64);
    fn CFRunLoopTimerGetNextFireDate(timer: CFRunLoopTimerRef) -> f64;
    fn CFRunLoopTimerInvalidate(timer: CFRunLoopTimerRef);
    fn CFRelease(cf: *const c_void);
}

/// The callout. `info` is the timer's [`Cell`] flag, kept alive by the
/// [`WakeTimer`] that owns the timer; the timer is invalidated before the flag
/// is dropped. `extern "C"` and panic-free (one `Cell::set`).
extern "C" fn wake_timer_fired(_timer: CFRunLoopTimerRef, info: *mut c_void) {
    if !info.is_null() {
        // SAFETY: `info` is `Rc::as_ptr` of the owning `WakeTimer`'s flag,
        // which outlives the timer (see `Drop`); a `Cell` is written through a
        // shared reference on the one thread the timer's run loop runs on.
        unsafe { (*info.cast::<Cell<bool>>()).set(true) };
    }
}

/// A run-loop wake timer that cannot make CoreFoundation spin: repeat interval
/// [`WAKE_TIMER_REPEAT_SECS`], re-armed by [`WakePlan`] on every request the
/// timer has not already got pending.
///
/// `!Send`: the callout writes the fired flag on the run loop's own thread, so
/// the timer and its owner stay there.
pub struct WakeTimer {
    timer: CFRunLoopTimerRef,
    fired: Rc<Cell<bool>>,
    plan: WakePlan,
    armed_date: Option<f64>,
}

impl std::fmt::Debug for WakeTimer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WakeTimer")
            .field("plan", &self.plan)
            .field("fired", &self.fired.get())
            .field("armed_date", &self.armed_date)
            .finish_non_exhaustive()
    }
}

impl WakeTimer {
    /// The event loop's waker: the MAIN run loop, in every common mode.
    #[must_use]
    pub fn main(_mtm: MainThread) -> Self {
        // SAFETY: plain CoreFoundation getters; the common-modes constant is
        // a static CFString CoreFoundation owns.
        unsafe { Self::on(CFRunLoopGetMain(), kCFRunLoopCommonModes) }
    }

    /// A waker on the CALLING thread's run loop, default mode — the same timer,
    /// for a thread that runs its own loop (and for driving the real timer
    /// from a test thread without touching the main run loop).
    #[must_use]
    pub fn current_thread() -> Self {
        // SAFETY: as in `main`.
        unsafe { Self::on(CFRunLoopGetCurrent(), kCFRunLoopDefaultMode) }
    }

    /// # Safety
    /// `run_loop` and `mode` are a live run loop and run-loop mode name.
    unsafe fn on(run_loop: CFRunLoopRef, mode: CFStringRef) -> Self {
        let fired = Rc::new(Cell::new(false));
        let mut context = CFRunLoopTimerContext {
            version: 0,
            info: Rc::as_ptr(&fired).cast::<c_void>().cast_mut(),
            retain: None,
            release: None,
            copy_description: None,
        };
        // SAFETY: CoreFoundation copies `context`; the flag it points at lives
        // as long as `Self` (the timer is invalidated in `Drop` first). The
        // first fire date is past CoreFoundation's own limit, so the timer
        // starts disarmed, matching `WakePlan::new`.
        let timer = unsafe {
            CFRunLoopTimerCreate(
                std::ptr::null(),
                f64::MAX,
                WAKE_TIMER_REPEAT_SECS,
                0,
                0,
                wake_timer_fired,
                &raw mut context,
            )
        };
        assert!(!timer.is_null(), "CFRunLoopTimerCreate returned NULL");
        // SAFETY: `timer` is the live timer just created; the run loop retains it.
        unsafe { CFRunLoopAddTimer(run_loop, timer, mode) };
        Self {
            timer,
            fired,
            plan: WakePlan::new(),
            armed_date: None,
        }
    }

    /// Fold the callout's flag into the plan.
    fn sync(&mut self) {
        if self.fired.replace(false) {
            self.plan.fired();
        }
    }

    /// The plan, with any fire since the last call folded in.
    pub fn plan(&mut self) -> WakePlan {
        self.sync();
        self.plan
    }

    /// No wake wanted.
    pub fn stop(&mut self) {
        self.sync();
        let arm = self.plan.stop();
        self.apply(Instant::now(), arm);
    }

    /// Wake at once.
    pub fn start(&mut self) {
        self.sync();
        let arm = self.plan.start();
        self.apply(Instant::now(), arm);
    }

    /// Wake at `at` (at once if not after `now`), or never for `None`.
    pub fn start_at(&mut self, now: Instant, at: Option<Instant>) {
        self.sync();
        let arm = self.plan.start_at(now, at);
        self.apply(now, arm);
    }

    fn apply(&mut self, now: Instant, arm: WakeArm) {
        // SAFETY: a plain getter.
        let current = unsafe { CFAbsoluteTimeGetCurrent() };
        let date = match arm {
            WakeArm::Keep => return,
            // CoreFoundation turns any past date into "now", so the current
            // time IS "now" — and it is the date the catch-up bound is
            // measured from ([`WakeTimer::armed_date`]).
            WakeArm::Now => current,
            WakeArm::At(at) => current + at.saturating_duration_since(now).as_secs_f64(),
            WakeArm::Disarm => f64::MAX,
        };
        self.armed_date = (arm != WakeArm::Disarm).then_some(date);
        // SAFETY: `self.timer` is live until `Drop`; a finite or MAX date is
        // accepted (CoreFoundation clamps the latter to its own limit).
        unsafe { CFRunLoopTimerSetNextFireDate(self.timer, date) };
    }

    /// CoreFoundation's next fire date for this timer (absolute time, seconds
    /// since 2001) — what the timer will actually do, as opposed to what the
    /// plan asked for.
    #[must_use]
    pub fn next_fire_date(&self) -> f64 {
        // SAFETY: `self.timer` is live until `Drop`.
        unsafe { CFRunLoopTimerGetNextFireDate(self.timer) }
    }

    /// The absolute time the timer was last armed for (`None` when disarmed).
    #[must_use]
    pub const fn armed_date(&self) -> Option<f64> {
        self.armed_date
    }

    /// Whether the timer will fire within `horizon` — i.e. is armed and not
    /// spent, as CoreFoundation itself reports it.
    #[must_use]
    pub fn fires_within(&self, horizon: Duration) -> bool {
        // SAFETY: a plain getter.
        let now = unsafe { CFAbsoluteTimeGetCurrent() };
        self.next_fire_date() <= now + horizon.as_secs_f64()
    }
}

impl Drop for WakeTimer {
    fn drop(&mut self) {
        // SAFETY: the timer is live; invalidating removes it from every run
        // loop, so the callout can no longer reach `self.fired`, which drops
        // after this body.
        unsafe {
            CFRunLoopTimerInvalidate(self.timer);
            CFRelease(self.timer.cast_const());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE ROOT CAUSE, AS ARITHMETIC. The interval bounds CoreFoundation's
    /// catch-up walk; winit's 0.1 µs made a 26.6 h stop cost ~10¹² steps.
    #[test]
    fn a_week_of_lateness_costs_the_waker_one_catch_up_step() {
        let rate = mach_ticks_per_sec();
        let week = interval_ticks(7.0 * 86_400.0, rate);
        let ours = interval_ticks(WAKE_TIMER_REPEAT_SECS, rate);
        assert!(ours > 0, "a zero-tick interval makes CoreFoundation halt");
        assert_eq!(cf_catch_up_steps(week, ours), Some(1));
        // The historical interval, priced the same way: the incident.
        let historical = interval_ticks(0.000_000_1, rate);
        let incident = interval_ticks(95_674.0, rate);
        assert!(
            cf_catch_up_steps(incident, historical).unwrap() > 1_000_000_000,
            "the 0.1 µs waker walked ~10¹² steps for the 2026-09-26 gap"
        );
    }

    #[test]
    fn a_spent_timer_is_re_armed_for_an_unchanged_request() {
        let t0 = Instant::now();
        let mut plan = WakePlan::new();
        assert_eq!(plan.start(), WakeArm::Now);
        assert_eq!(
            plan.start(),
            WakeArm::Keep,
            "armed and pending: nothing to do"
        );
        plan.fired();
        assert_eq!(
            plan.start(),
            WakeArm::Now,
            "spent: the same request re-arms"
        );
        let later = t0 + Duration::from_secs(5);
        assert_eq!(plan.start_at(t0, Some(later)), WakeArm::At(later));
        assert_eq!(plan.start_at(t0, Some(later)), WakeArm::Keep);
        plan.fired();
        assert_eq!(plan.start_at(t0, Some(later)), WakeArm::At(later));
        assert_eq!(
            plan.start_at(later, Some(later)),
            WakeArm::Now,
            "due is now"
        );
        assert_eq!(plan.start_at(later, None), WakeArm::Disarm);
        assert_eq!(plan.stop(), WakeArm::Keep);
        plan.fired();
        assert!(!plan.spent(), "a disarmed timer has nothing to spend");
    }
}
