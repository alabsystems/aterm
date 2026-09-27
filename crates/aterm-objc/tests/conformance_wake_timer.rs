// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TIER-1: the REAL `aterm_objc::WakeTimer`, on this test thread's own run
//! loop, against the REAL CoreFoundation, stepped beside
//! `aterm_spec::derive::run_loop_waker_model`.
//!
//! Every request goes through the shipping timer; every fire is CoreFoundation
//! running its own timer; every projected variable is read back from the plan
//! and from `CFRunLoopTimerGetNextFireDate` — never from the harness's own
//! bookkeeping. `steps` is CoreFoundation's catch-up walk measured the only way
//! it can be seen from outside: how many repeat intervals past the armed date
//! it moved the next fire date. Two negative controls replay the two defects the
//! model's `Buggy` family carries (the historical 0.1 µs interval, and a plan
//! deaf to the callout) on raw CoreFoundation timers, and show this projection
//! catches each — so a green run here is not the projection agreeing with
//! anything.
//!
//! Pure CoreFoundation on a non-main thread: no AppKit, no WindowServer.

#![cfg(target_os = "macos")]

use std::cell::Cell;
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::time::{Duration, Instant};

use aterm_objc::wake_timer::{WAKE_TIMER_REPEAT_SECS, WakePlan, WakeTarget};
use aterm_objc::{WakeArm, WakeTimer};
use aterm_spec::derive::{Model, run_loop_waker_model};

type State = BTreeMap<&'static str, i64>;

#[repr(C)]
struct TimerContext {
    version: isize,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopDefaultMode: *const c_void;
    fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source: u8) -> i32;
    fn CFRunLoopGetCurrent() -> *mut c_void;
    fn CFAbsoluteTimeGetCurrent() -> f64;
    fn CFRunLoopTimerCreate(
        allocator: *const c_void,
        fire_date: f64,
        interval: f64,
        flags: usize,
        order: isize,
        callout: extern "C" fn(*mut c_void, *mut c_void),
        context: *mut TimerContext,
    ) -> *mut c_void;
    fn CFRunLoopAddTimer(rl: *mut c_void, timer: *mut c_void, mode: *const c_void);
    fn CFRunLoopTimerSetNextFireDate(timer: *mut c_void, fire_date: f64);
    fn CFRunLoopTimerGetNextFireDate(timer: *mut c_void) -> f64;
    fn CFRunLoopTimerInvalidate(timer: *mut c_void);
    fn CFRelease(cf: *const c_void);
}

/// Run this thread's run loop long enough for a due timer to fire.
fn pump() {
    // SAFETY: this thread's own run loop, CoreFoundation's own mode constant.
    unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.02, 0) };
}

fn now_abs() -> f64 {
    // SAFETY: a plain getter.
    unsafe { CFAbsoluteTimeGetCurrent() }
}

/// A day: "will fire within the horizon" is CoreFoundation's armed; a spent or
/// disarmed timer is a year or more out.
const HORIZON: Duration = Duration::from_secs(86_400);

fn step(model: &Model, state: &mut State, action: &'static str) {
    let successors = model.successors(action, state);
    assert_eq!(successors.len(), 1, "{action} is not admitted at {state:?}");
    *state = successors[0].clone();
}

/// The real timer's state on the model's variables.
fn project(timer: &mut WakeTimer, want: i64, delivered: i64) -> [(&'static str, i64); 5] {
    let plan = timer.plan();
    let cache = match plan.armed() {
        None => 0,
        Some(WakeTarget::Now) => 1,
        Some(WakeTarget::At(_)) => 2,
    };
    [
        ("cache", cache),
        ("spent", i64::from(plan.spent())),
        ("armed", i64::from(timer.fires_within(HORIZON))),
        ("want", want),
        ("delivered", delivered),
    ]
}

fn agree(model: &Model, state: &State, observed: [(&'static str, i64); 5], after: &str) {
    for (var, real) in observed {
        assert_eq!(
            state[var], real,
            "after {after}: model `{var}` = {} but the real timer reads {real} (model state {state:?})",
            state[var]
        );
    }
    for inv in ["NoLostWake", "BoundedCatchUp"] {
        assert!(model.check_invariant(inv, state), "{inv} after {after}");
    }
}

/// How many repeat intervals CoreFoundation moved the next fire date past the
/// date the timer was armed for — its catch-up walk, seen from outside.
fn catch_up_steps(next_fire: f64, armed: f64, interval: f64) -> i64 {
    ((next_fire - armed) / interval).round() as i64
}

#[test]
fn the_real_wake_timer_refines_the_run_loop_waker_model() {
    let model = run_loop_waker_model();
    let mut state = model.init_state();
    let mut timer = WakeTimer::current_thread();
    let (mut want, mut delivered) = (0, 0);
    agree(&model, &state, project(&mut timer, want, delivered), "init");

    // Poll: arm now, CoreFoundation fires it, the timer is SPENT.
    timer.start();
    step(&model, &mut state, "RequestNow");
    (want, delivered) = (1, 0);
    agree(
        &model,
        &state,
        project(&mut timer, want, delivered),
        "RequestNow",
    );
    pump();
    step(&model, &mut state, "Fire");
    delivered = i64::from(timer.plan().spent());
    agree(&model, &state, project(&mut timer, want, delivered), "Fire");
    assert_eq!(delivered, 1, "CoreFoundation fired the armed timer");

    // The SAME request again: the spent timer must be re-armed, not trusted.
    timer.start();
    step(&model, &mut state, "RequestNow");
    (want, delivered) = (1, 0);
    agree(
        &model,
        &state,
        project(&mut timer, want, delivered),
        "RequestNow again",
    );
    pump();
    step(&model, &mut state, "Fire");
    delivered = 1;
    agree(
        &model,
        &state,
        project(&mut timer, want, delivered),
        "second Fire",
    );

    // A deadline: armed for it, kept when asked again, disarmed on Wait.
    let far = Instant::now() + Duration::from_secs(10);
    timer.start_at(Instant::now(), Some(far));
    step(&model, &mut state, "RequestAt");
    (want, delivered) = (2, 0);
    agree(
        &model,
        &state,
        project(&mut timer, want, delivered),
        "RequestAt",
    );
    timer.start_at(Instant::now(), Some(far));
    step(&model, &mut state, "RequestAt");
    agree(
        &model,
        &state,
        project(&mut timer, want, delivered),
        "RequestAt kept",
    );
    timer.start_at(Instant::now(), None);
    step(&model, &mut state, "RequestWait");
    (want, delivered) = (0, 0);
    agree(
        &model,
        &state,
        project(&mut timer, want, delivered),
        "RequestWait",
    );
    timer.stop();
    step(&model, &mut state, "RequestWait");
    agree(
        &model,
        &state,
        project(&mut timer, want, delivered),
        "RequestWait kept",
    );

    // A near deadline that CoreFoundation fires on its own.
    let near = Instant::now() + Duration::from_millis(30);
    timer.start_at(Instant::now(), Some(near));
    step(&model, &mut state, "RequestAt");
    (want, delivered) = (2, 0);
    agree(
        &model,
        &state,
        project(&mut timer, want, delivered),
        "near RequestAt",
    );
    std::thread::sleep(Duration::from_millis(40));
    pump();
    step(&model, &mut state, "Fire");
    delivered = 1;
    agree(
        &model,
        &state,
        project(&mut timer, want, delivered),
        "deadline Fire",
    );

    // THE INCIDENT, SCALED: armed, then this thread does not run its loop
    // (a stopped process), then CoreFoundation fires the late timer. One step.
    timer.start();
    step(&model, &mut state, "RequestNow");
    (want, delivered) = (1, 0);
    agree(
        &model,
        &state,
        project(&mut timer, want, delivered),
        "RequestNow before the stop",
    );
    let armed = timer.armed_date().expect("armed");
    std::thread::sleep(Duration::from_millis(60));
    step(&model, &mut state, "Suspend");
    step(&model, &mut state, "Suspend");
    let before = Instant::now();
    pump();
    let pass = before.elapsed();
    step(&model, &mut state, "Fire");
    delivered = 1;
    agree(
        &model,
        &state,
        project(&mut timer, want, delivered),
        "late Fire",
    );
    let steps = catch_up_steps(timer.next_fire_date(), armed, WAKE_TIMER_REPEAT_SECS);
    assert_eq!(
        steps, state["steps"],
        "CoreFoundation's catch-up walk took {steps} step(s) for a 60 ms-late fire \
         (the run-loop pass took {pass:?}); the model says {}",
        state["steps"]
    );
}

/// NEGATIVE CONTROL 1 — the historical 0.1 µs interval. The same projection,
/// on a raw timer armed exactly as winit's `EventLoopWaker` armed its own,
/// reads a catch-up walk of ~lateness/0.1 µs steps: the shipped model's `1`
/// rejects it, and the `tiny` mutant is what predicts it.
#[test]
fn the_historical_interval_is_caught_by_the_same_projection() {
    extern "C" fn fired(_timer: *mut c_void, info: *mut c_void) {
        // SAFETY: `info` is the test's live `Cell<u32>` counter.
        let count = unsafe { &*info.cast::<Cell<u32>>() };
        count.set(count.get() + 1);
    }
    let count = Cell::new(0u32);
    let mut context = TimerContext {
        version: 0,
        info: (&raw const count).cast_mut().cast(),
        retain: std::ptr::null(),
        release: std::ptr::null(),
        copy_description: std::ptr::null(),
    };
    let interval = 0.000_000_1;
    // SAFETY: a timer on this thread's run loop whose callout's `info` outlives
    // it (invalidated below, before `count` drops).
    let timer = unsafe {
        let t = CFRunLoopTimerCreate(
            std::ptr::null(),
            f64::MAX,
            interval,
            0,
            0,
            fired,
            &raw mut context,
        );
        CFRunLoopAddTimer(CFRunLoopGetCurrent(), t, kCFRunLoopDefaultMode);
        t
    };
    let armed = now_abs();
    // SAFETY: the live timer above.
    unsafe { CFRunLoopTimerSetNextFireDate(timer, armed) };
    std::thread::sleep(Duration::from_millis(60));
    let before = Instant::now();
    pump();
    let pass = before.elapsed();
    // SAFETY: the live timer above.
    let next = unsafe { CFRunLoopTimerGetNextFireDate(timer) };
    // SAFETY: the live timer above; invalidated before release, `count` after.
    unsafe {
        CFRunLoopTimerInvalidate(timer);
        CFRelease(timer.cast_const());
    }
    assert!(count.get() >= 1, "CoreFoundation fired the late timer");
    let steps = catch_up_steps(next, armed, interval);
    assert!(
        steps > 100_000,
        "a 60 ms-late 0.1 µs timer walks ~600,000 steps; measured {steps} (pass {pass:?})"
    );

    let model = run_loop_waker_model();
    let mut fixed = model.init_state();
    for action in ["RequestNow", "Suspend", "Suspend", "Fire"] {
        step(&model, &mut fixed, action);
    }
    assert_ne!(
        fixed["steps"], steps,
        "the shipped model must REJECT the historical interval's walk"
    );
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut tiny = buggy.init_state();
    for action in [
        "MutateTinyInterval",
        "RequestNow",
        "Suspend",
        "Suspend",
        "Fire",
    ] {
        step(&buggy, &mut tiny, action);
    }
    assert!(
        tiny["steps"] > 1 && !buggy.check_invariant("BoundedCatchUp", &tiny),
        "the tiny-interval mutant predicts the walk and violates BoundedCatchUp"
    );
}

/// NEGATIVE CONTROL 2 — a plan deaf to the callout. The shipping `WakePlan`,
/// driving a raw year-interval timer but never told it fired, keeps a spent
/// timer for an unchanged request: CoreFoundation will not fire again, the
/// projection reads `armed = 0` with a wake wanted and none delivered, and
/// `NoLostWake` rejects that state — the lost wake the spent flag exists for.
#[test]
fn a_plan_deaf_to_the_callout_is_caught_as_a_lost_wake() {
    extern "C" fn fired(_timer: *mut c_void, info: *mut c_void) {
        // SAFETY: `info` is the test's live `Cell<bool>`.
        unsafe { (*info.cast::<Cell<bool>>()).set(true) };
    }
    let flag = Cell::new(false);
    let mut context = TimerContext {
        version: 0,
        info: (&raw const flag).cast_mut().cast(),
        retain: std::ptr::null(),
        release: std::ptr::null(),
        copy_description: std::ptr::null(),
    };
    // SAFETY: as in the first control.
    let timer = unsafe {
        let t = CFRunLoopTimerCreate(
            std::ptr::null(),
            f64::MAX,
            WAKE_TIMER_REPEAT_SECS,
            0,
            0,
            fired,
            &raw mut context,
        );
        CFRunLoopAddTimer(CFRunLoopGetCurrent(), t, kCFRunLoopDefaultMode);
        t
    };
    let mut plan = WakePlan::new();
    let apply = |arm: WakeArm| {
        if arm == WakeArm::Now {
            // SAFETY: the live timer above.
            unsafe { CFRunLoopTimerSetNextFireDate(timer, now_abs()) };
        }
    };
    apply(plan.start());
    pump();
    assert!(flag.replace(false), "CoreFoundation fired the armed timer");
    // Deaf: `plan.fired()` is never called.
    let arm = plan.start();
    apply(arm);
    // SAFETY: the live timer above.
    let next = unsafe { CFRunLoopTimerGetNextFireDate(timer) };
    // SAFETY: the live timer above; `flag` drops after.
    unsafe {
        CFRunLoopTimerInvalidate(timer);
        CFRelease(timer.cast_const());
    }
    assert_eq!(arm, WakeArm::Keep, "the deaf plan trusts its cache");
    let real_armed = i64::from(next <= now_abs() + HORIZON.as_secs_f64());
    assert_eq!(real_armed, 0, "the spent timer is parked a year out");

    let model = run_loop_waker_model();
    let mut projected = model.init_state();
    for action in ["RequestNow", "Fire", "RequestNow"] {
        step(&model, &mut projected, action);
    }
    assert_eq!(projected["armed"], 1, "the shipped model re-arms");
    projected.insert("armed", real_armed);
    projected.insert("spent", 0);
    assert!(
        !model.check_invariant("NoLostWake", &projected),
        "NoLostWake must reject the deaf plan's real state {projected:?}"
    );
}
