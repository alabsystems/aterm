// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CoreFoundation run-loop wake timer ([`WAKE_TIMER_REPEAT_SECS`], [`WakePlan`]).

use std::cell::Cell;
use std::ffi::c_void;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::cf::{
    CFRelease, CFRunLoopGetMain, CFRunLoopRef, CFStringRef, kCFRunLoopCommonModes,
    kCFRunLoopDefaultMode,
};
use crate::declare::MainThread;

/// The repeat interval handed to CoreFoundation, in seconds: one Julian year.
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

/// CoreFoundation-independent decision state bound to the derived model.
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

    /// Whether the timer fired since it was last armed, i.e.
    #[must_use]
    pub const fn spent(&self) -> bool {
        self.spent
    }

    /// The timer's callout ran: whatever it was armed for is used up.
    pub const fn fired(&mut self) {
        if self.armed.is_some() {
            self.spent = true;
        }
    }

    /// No wake wanted (`ControlFlow::Wait`).
    pub const fn stop(&mut self) -> WakeArm {
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

type CFRunLoopTimerRef = *mut c_void;

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
}

/// The callout ([`Cell`], [`WakeTimer`]).
extern "C" fn wake_timer_fired(_timer: CFRunLoopTimerRef, info: *mut c_void) {
    if !info.is_null() {
        // SAFETY: `info` is `Rc::as_ptr` of the owning `WakeTimer`'s flag.
        unsafe { (*info.cast::<Cell<bool>>()).set(true) };
    }
}

/// A run-loop wake timer that cannot make CoreFoundation spin.
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
    /// The event loop's waker: the main run loop, in every common mode.
    #[must_use]
    pub fn main(_mtm: MainThread) -> Self {
        // SAFETY: Plain CoreFoundation getters.
        unsafe { Self::on(CFRunLoopGetMain(), kCFRunLoopCommonModes) }
    }

    /// A waker on the calling thread's run loop, default mode.
    #[must_use]
    pub fn current_thread() -> Self {
        // SAFETY: As in `main`.
        unsafe { Self::on(CFRunLoopGetCurrent(), kCFRunLoopDefaultMode) }
    }

    /// `run_loop` and `mode` are a live run loop and run-loop mode name.
    ///
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
        // SAFETY: CoreFoundation copies `context`.
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

    /// Return the plan after folding in any fire since the last call.
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
        // SAFETY: A plain getter.
        let current = unsafe { CFAbsoluteTimeGetCurrent() };
        let date = match arm {
            WakeArm::Keep => return,
            WakeArm::Now => current,
            WakeArm::At(at) => current + at.saturating_duration_since(now).as_secs_f64(),
            WakeArm::Disarm => f64::MAX,
        };
        self.armed_date = (arm != WakeArm::Disarm).then_some(date);
        // SAFETY: `self.timer` is live until `Drop`.
        unsafe { CFRunLoopTimerSetNextFireDate(self.timer, date) };
    }

    /// CoreFoundation's next fire date for this timer (absolute time, seconds since 2001).
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

    /// Whether the timer will fire within `horizon` — i.e.
    #[must_use]
    pub fn fires_within(&self, horizon: Duration) -> bool {
        // SAFETY: A plain getter.
        let now = unsafe { CFAbsoluteTimeGetCurrent() };
        self.next_fire_date() <= now + horizon.as_secs_f64()
    }
}

impl Drop for WakeTimer {
    fn drop(&mut self) {
        // SAFETY: The timer is live.
        unsafe {
            CFRunLoopTimerInvalidate(self.timer);
            CFRelease(self.timer.cast_const());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
