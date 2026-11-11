// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! LOCAL PATCH (aterm): a window's frame clock on macOS 14 and later
//! (`Window::set_frame_clock`, `WindowEvent::FrameTick`;
//! `docs/DESIGN-stats-deck-gpu-motion-2026-10-26.md` §3.4): the `CADisplayLink` its view makes
//! with `-[NSView displayLinkWithTarget:selector:]`, which follows the view's screen. It runs in
//! the main run loop's common modes, so it ticks through live resize and menu tracking, at up to
//! the screen's rate, and is paused while nothing wants it. A Mac before 14 has no such link; the
//! caller keeps its own clock there. The headless loop has no windows, so no link either.

#[cfg(any(test, debug_assertions))]
use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use aterm_objc::send::{
    send_bool_sel, send_f64, send_id, send_isize, send_v, send_v_bool, send_v_id_id,
};
#[cfg(any(test, debug_assertions))]
use aterm_objc::WeakObj;
use aterm_objc::{checked_msg, class, sel, Encode, Id, Obj, Sel};

/// `CAFrameRateRange`: the rates a display link may run at, frames per second.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct FrameRateRange {
    minimum: f32,
    maximum: f32,
    preferred: f32,
}

// SAFETY: `#[repr(C)]` over three `f32`s is QuartzCore's `{CAFrameRateRange=fff}`.
unsafe impl Encode for FrameRateRange {
    const ENCODING: &'static str = "{CAFrameRateRange=fff}";
}

/// The rate a link runs at when its screen does not say: a ProMotion panel's.
const RATE_UNKNOWN: isize = 120;

/// The lowest rate a link is held to (a 60 Hz panel's), frames per second.
const RATE_MIN: f32 = 60.0;

#[repr(C)]
struct MachTimebase {
    numer: u32,
    denom: u32,
}

extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebase) -> i32;
}

/// The host clock now, seconds: the timebase of a display link's timestamps
/// (`CACurrentMediaTime`).
fn host_seconds() -> f64 {
    static RATIO: OnceLock<f64> = OnceLock::new();
    let ratio = *RATIO.get_or_init(|| {
        let mut tb = MachTimebase { numer: 0, denom: 0 };
        // SAFETY: `mach_timebase_info` writes the two fields of the struct it is handed.
        let ok = unsafe { mach_timebase_info(&mut tb) } == 0 && tb.denom != 0;
        if ok {
            f64::from(tb.numer) / f64::from(tb.denom)
        } else {
            1.0
        }
    });
    // SAFETY: `mach_absolute_time` reads the host clock; it has no preconditions.
    let ticks = unsafe { mach_absolute_time() };
    ticks as f64 * ratio / 1e9
}

/// The reads [`instant_of`]'s calibration takes, back to back, to keep the tightest.
const PAIR_READS: usize = 8;

/// Of `reads`, each an `Instant::now()` with a host clock read either side of it (seconds), the
/// pair with the least gap between its two host reads: the `Instant` with the midpoint of that
/// gap, so a read the thread was preempted in is passed over for one that ran straight through.
/// `None` with no reads.
fn tightest_pair(reads: impl IntoIterator<Item = (f64, Instant, f64)>) -> Option<(f64, Instant)> {
    reads
        .into_iter()
        .min_by(|a, b| (a.2 - a.0).total_cmp(&(b.2 - b.0)))
        .map(|(before, at, after)| (before + (after - before) / 2.0, at))
}

/// `secs` on the host clock as an `Instant`, through the one (host seconds, `Instant`) pair taken
/// at the first conversion, the tightest of [`PAIR_READS`] back-to-back reads
/// ([`tightest_pair`]); the pair's `Instant` where `secs` is not finite or lands outside what an
/// `Instant` holds (this runs in the display link's callback, where a panic would cross the
/// Objective-C boundary).
pub(super) fn instant_of(secs: f64) -> Instant {
    static PAIR: OnceLock<(f64, Instant)> = OnceLock::new();
    let &(at_s, at) = PAIR.get_or_init(|| {
        let reads = (0..PAIR_READS).map(|_| {
            let before = host_seconds();
            let at = Instant::now();
            (before, at, host_seconds())
        });
        tightest_pair(reads).unwrap_or_else(|| (host_seconds(), Instant::now()))
    });
    let d = secs - at_s;
    let off = |d: f64| Duration::try_from_secs_f64(d).ok();
    if d >= 0.0 {
        off(d).and_then(|d| at.checked_add(d))
    } else {
        off(-d).and_then(|d| at.checked_sub(d))
    }
    .unwrap_or(at)
}

/// A tick of `link`: when the frame drawn now reaches the display, and the refresh period it
/// runs at (`-targetTimestamp` less `-timestamp`).
///
/// # Safety
/// `link` must be a live `CADisplayLink`.
pub(super) unsafe fn tick_of(link: Id) -> (Instant, Duration) {
    // SAFETY: the caller pins `link` as a live `CADisplayLink`; `-targetTimestamp` and
    // `-timestamp` are both `d16@0:8` (`CFTimeInterval`).
    let (target_s, at_s) =
        unsafe { (send_f64(link, sel!(targetTimestamp)), send_f64(link, sel!(timestamp))) };
    let period = Duration::try_from_secs_f64(target_s - at_s).unwrap_or(Duration::ZERO);
    (instant_of(target_s), period)
}

/// Whether `view` can make a display link (macOS 14 and later).
///
/// # Safety
/// `view` must be a live `NSView`.
pub(super) unsafe fn can_link(view: Id) -> bool {
    // SAFETY: `-respondsToSelector:` is `B24@0:8:16` on every `NSObject`.
    unsafe { send_bool_sel(view, sel!(respondsToSelector:), sel!(displayLinkWithTarget:selector:)) }
}

/// A new display link of `view` that sends `action` (`v24@0:8@16`) to `view` each vsync, in the
/// main run loop's common modes, paused; `None` where the view makes none. In a build with debug
/// assertions it records a weak reference to the link it returns ([`links_made_for`]).
///
/// # Safety
/// `view` must be a live `NSView` on the main thread that implements `action`; the link retains
/// it until [`invalidate`].
pub(super) unsafe fn make_link(view: Id, action: Sel) -> Option<Obj> {
    // SAFETY: the caller pins `view`; `can_link` asked `-respondsToSelector:` first.
    if !unsafe { can_link(view) } {
        return None;
    }
    // SAFETY: `-displayLinkWithTarget:selector:` is `@32@0:8@16:24` on `NSView` (macOS 14),
    // returning a +0 autoreleased `CADisplayLink` that `Obj::retain` takes to +1;
    // `+mainRunLoop` is `@16@0:8`; `-addToRunLoop:forMode:` is `v32@0:8@16@24`, and
    // `kCFRunLoopCommonModes` is toll-free bridged to `NSRunLoopCommonModes`; `-setPaused:` is
    // `v20@0:8B16`.
    let link = unsafe {
        let make: unsafe extern "C-unwind" fn(Id, Sel, Id, Sel) -> Id = checked_msg();
        let link = Obj::retain(make(view, sel!(displayLinkWithTarget:selector:), view, action))?;
        send_v_bool(link.id(), sel!(setPaused:), true);
        let run_loop = send_id(class(c"NSRunLoop").as_id(), sel!(mainRunLoop));
        let common = Id::from_ptr(aterm_objc::cf::kCFRunLoopCommonModes.cast_mut().cast());
        send_v_id_id(link.id(), sel!(addToRunLoop:forMode:), run_loop, common);
        link
    };
    #[cfg(any(test, debug_assertions))]
    // SAFETY: the caller pins `view` as a live `NSView` on the main thread.
    unsafe {
        record_made(view, &link);
    }
    Some(link)
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// A debug seam's record (`platform::macos::frame_clock_links`): a weak reference to every
    /// display link [`make_link`] made, beside one to the view it made the link for. Main thread
    /// only, as `make_link` is; an entry goes once its view has gone.
    static MADE: RefCell<Vec<(WeakObj, WeakObj)>> = const { RefCell::new(Vec::new()) };
}

/// Records `link`, the display link [`make_link`] just made for `view`, in [`MADE`].
///
/// # Safety
/// `view` must be a live `NSView`, on the main thread.
#[cfg(any(test, debug_assertions))]
unsafe fn record_made(view: Id, link: &Obj) {
    // SAFETY: the caller pins `view` as live, so it is not deallocating.
    let view = unsafe { WeakObj::new(view) };
    MADE.with_borrow_mut(|made| {
        made.retain(|(v, _)| v.is_live());
        made.push((view, WeakObj::from_obj(link)));
    });
}

/// Weak references to every display link [`make_link`] made for `view` (a debug seam), in the
/// order made: each loads its link while something holds it (the main run loop, until
/// [`invalidate`]) and nil once it is gone. Empty for a view that made none, or off the main
/// thread, where `make_link` never runs.
#[cfg(any(test, debug_assertions))]
pub(super) fn links_made_for(view: Id) -> Vec<WeakObj> {
    MADE.with_borrow(|made| {
        made.iter()
            .filter(|(v, _)| v.load().is_some_and(|v| v.id() == view))
            .map(|(_, link)| link.clone_weak())
            .collect()
    })
}

/// Run `link` at up to the rate of the screen `view` is on, from 60 frames a second, and
/// unpause it (`want`) or pause it.
///
/// # Safety
/// `link` must be a live `CADisplayLink` and `view` a live `NSView`, on the main thread.
pub(super) unsafe fn set_running(link: Id, view: Id, want: bool) {
    if want {
        // SAFETY: `-window` and `-screen` are `@16@0:8` (+0, nil when off screen);
        // `-maximumFramesPerSecond` is `q16@0:8` on `NSScreen` (macOS 12); the screen answers
        // `-respondsToSelector:` first.
        let rate = unsafe {
            let window = send_id(view, sel!(window));
            let screen = if window.is_null() { Id::NIL } else { send_id(window, sel!(screen)) };
            if !screen.is_null()
                && send_bool_sel(screen, sel!(respondsToSelector:), sel!(maximumFramesPerSecond))
            {
                send_isize(screen, sel!(maximumFramesPerSecond))
            } else {
                RATE_UNKNOWN
            }
        };
        let max = (rate.max(1) as f32).max(RATE_MIN);
        let range = FrameRateRange { minimum: RATE_MIN.min(max), maximum: max, preferred: max };
        // SAFETY: `-setPreferredFrameRateRange:` is `v28@0:8{CAFrameRateRange=fff}16` on
        // `CADisplayLink`.
        unsafe {
            let set: unsafe extern "C-unwind" fn(Id, Sel, FrameRateRange) = checked_msg();
            set(link, sel!(setPreferredFrameRateRange:), range);
        }
    }
    // SAFETY: `-setPaused:` is `v20@0:8B16` on `CADisplayLink`.
    unsafe { send_v_bool(link, sel!(setPaused:), !want) };
}

/// Stop `link` for good: it leaves its run loop, which retained it, and releases its target.
///
/// # Safety
/// `link` must be a live `CADisplayLink`, on the main thread.
pub(super) unsafe fn invalidate(link: Id) {
    // SAFETY: `-invalidate` is `v16@0:8` on `CADisplayLink`.
    unsafe { send_v(link, sel!(invalidate)) };
}

#[cfg(test)]
#[path = "frame_clock_tests.rs"]
mod tests;
