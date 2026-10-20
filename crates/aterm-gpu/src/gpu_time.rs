// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GPU time per presented frame: how long the device spent executing one
//! present's work, which neither `present_latency` (a CPU span) nor the
//! present-to-glass ledger (`crate::present_glass`) can see.
//!
//! # What is measured
//!
//! A frame is opened when a present begins (`open_frame`) and sealed when
//! the present returns (`FrameToken::seal`). Every piece of GPU work the
//! present commits in between is a *part*: on Metal each command buffer the
//! encode session commits (Submit A, Submit B, inline staging copies) hangs an
//! `addCompletedHandler:` that reads `-[MTLCommandBuffer GPUStartTime]` and
//! `GPUEndTime`; wgpu timestamps each timed render pass. The frame's GPU
//! time is the SUM of its parts' spans: work on
//! one queue executes in commit order, so the sum is device busy time and
//! leaves out the idle gaps a first-start-to-last-end span would count. The
//! presentation-only command buffer (`presentDrawable:`) carries no draws and
//! is not a part. GL reports nothing: it has no completion clock that does not
//! stall the context.
//!
//! # Zero cost at rest, no wait on the present path
//!
//! `open_frame` returns `None` unless a sink is installed, so with no
//! listener no handler is registered and nothing is counted. A handler runs on
//! a Metal completion thread; the present never waits for it. The frame is
//! delivered by whichever side finishes last: the final completion handler, or
//! the seal when every part had already completed (the virtual present, which
//! waits for its own Submit B).
//!
//! # Lock-free accounting
//!
//! Frames live in a fixed ring indexed by serial, so the handlers neither lock
//! nor allocate. A slot is claimed only when its previous frame has been
//! delivered; a present that finds its slot still busy (the device is a whole
//! ring of frames behind) goes untimed and is counted ([`skipped`]). A frame
//! any part of which failed (a command-buffer error, a lost readback) is
//! dropped and counted ([`dropped`]), never reported as a short GPU time.
//!
//! The frontend installs one sink with [`install_sink`]; it runs on a
//! framework thread, so it must be lock-free and must not block or unwind.
//! [`settled`] answers the same result by serial for a reader that polls (the
//! effects perf lane).

#[cfg(any(target_os = "macos", wgpu_arm, test))]
use std::sync::OnceLock;
#[cfg(any(target_os = "macos", wgpu_arm, test))]
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
#[cfg(any(target_os = "macos", test))]
use std::time::Duration;

/// One delivered frame's GPU time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuFrameTime {
    /// The frame's process serial, monotonic from 1.
    pub serial: u64,
    /// Sum of the parts' GPU execution spans, nanoseconds.
    pub gpu_ns: u64,
    /// How many parts were summed: Metal command buffers or wgpu render passes.
    pub parts: u32,
}

/// What became of a frame, as [`settled`] reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settled {
    /// Every part completed and reported a span.
    Timed(GpuFrameTime),
    /// A part failed or reported no clock: the frame was dropped.
    Failed,
    /// The frame was sealed with no part (nothing reached the device).
    Untimed,
}

/// The frontend's receiver. Called on a framework thread; must not block.
pub type GpuTimeSink = fn(GpuFrameTime);

/// Whether this build has a device execution clock. A wgpu device must also
/// grant timestamp queries; GL has no clock here.
pub const SUPPORTED: bool = cfg!(any(target_os = "macos", wgpu_arm));

#[cfg(any(target_os = "macos", wgpu_arm, test))]
static FRAMES: FrameRing = FrameRing::new();

/// Frames in flight at once. A frame's slot is reused `RING` serials later,
/// long after its command buffers completed on any device that is presenting.
#[cfg(any(target_os = "macos", wgpu_arm, test))]
const RING: usize = 64;

/// `pending` while a slot is being delivered: no open can claim it and no
/// part can be added to it.
#[cfg(any(target_os = "macos", wgpu_arm, test))]
const DELIVERING: u32 = u32::MAX;

/// One frame's accumulator.
#[cfg(any(target_os = "macos", wgpu_arm, test))]
#[derive(Debug)]
struct Slot {
    /// Outstanding holds: the seal's one plus one per unfinished part. 0 is a
    /// free slot; [`DELIVERING`] is a slot whose result is being published.
    pending: AtomicU32,
    /// The serial of the open frame using this slot.
    serial: AtomicU64,
    /// The serial whose result `gpu_ns`/`parts`/`failed` hold, once delivered
    /// (0 while a frame is open). The seqlock word [`settled`] reads twice.
    done_serial: AtomicU64,
    gpu_ns: AtomicU64,
    parts: AtomicU32,
    failed: AtomicBool,
}

#[cfg(any(target_os = "macos", wgpu_arm, test))]
impl Slot {
    const fn new() -> Self {
        Self {
            pending: AtomicU32::new(0),
            serial: AtomicU64::new(0),
            done_serial: AtomicU64::new(0),
            gpu_ns: AtomicU64::new(0),
            parts: AtomicU32::new(0),
            failed: AtomicBool::new(false),
        }
    }
}

/// One accounting domain. Production uses `FRAMES`; tests own independent
/// instances of the same ring so eviction never crosses fixture boundaries.
#[cfg(any(target_os = "macos", wgpu_arm, test))]
#[derive(Debug)]
struct FrameRing {
    sink: OnceLock<GpuTimeSink>,
    next_serial: AtomicU64,
    delivered: AtomicU64,
    dropped: AtomicU64,
    skipped: AtomicU64,
    slots: [Slot; RING],
}

#[cfg(any(target_os = "macos", wgpu_arm, test))]
impl FrameRing {
    const fn new() -> Self {
        Self {
            sink: OnceLock::new(),
            next_serial: AtomicU64::new(1),
            delivered: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            slots: [const { Slot::new() }; RING],
        }
    }

    fn slot(&self, serial: u64) -> &Slot {
        &self.slots[usize::try_from(serial % RING as u64).unwrap_or(0)]
    }
}

/// Install the process's one sink. First install wins; `false` when a sink was
/// already installed or this backend has no device clock. Installing turns timing on.
pub fn install_sink(sink: GpuTimeSink) -> bool {
    #[cfg(any(target_os = "macos", wgpu_arm, test))]
    {
        FRAMES.sink.set(sink).is_ok()
    }
    #[cfg(not(any(target_os = "macos", wgpu_arm, test)))]
    {
        let _ = sink;
        false
    }
}

/// Whether a sink is installed — presents open frames only then.
#[must_use]
pub fn sink_installed() -> bool {
    #[cfg(any(target_os = "macos", wgpu_arm, test))]
    {
        FRAMES.sink.get().is_some()
    }
    #[cfg(not(any(target_os = "macos", wgpu_arm, test)))]
    {
        false
    }
}

/// Frames delivered to the sink since process start.
#[must_use]
pub fn delivered() -> u64 {
    #[cfg(any(target_os = "macos", wgpu_arm, test))]
    {
        FRAMES.delivered.load(Ordering::Relaxed)
    }
    #[cfg(not(any(target_os = "macos", wgpu_arm, test)))]
    {
        0
    }
}

/// Frames dropped because a part failed or reported no clock.
#[must_use]
pub fn dropped() -> u64 {
    #[cfg(any(target_os = "macos", wgpu_arm, test))]
    {
        FRAMES.dropped.load(Ordering::Relaxed)
    }
    #[cfg(not(any(target_os = "macos", wgpu_arm, test)))]
    {
        0
    }
}

/// Presents that went untimed because their ring slot was still in flight.
#[must_use]
pub fn skipped() -> u64 {
    #[cfg(any(target_os = "macos", wgpu_arm, test))]
    {
        FRAMES.skipped.load(Ordering::Relaxed)
    }
    #[cfg(not(any(target_os = "macos", wgpu_arm, test)))]
    {
        0
    }
}

/// A wgpu present found every readback slot occupied.
#[cfg(wgpu_arm)]
pub(crate) fn note_skipped() {
    FRAMES.skipped.fetch_add(1, Ordering::Relaxed);
}

#[cfg(any(target_os = "macos", wgpu_arm, test))]
/// One open frame: the handle every part of it reports through. `Copy` so a
/// completion handler can capture it without allocating.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameToken<'a> {
    serial: u64,
    ring: &'a FrameRing,
}

#[cfg(any(target_os = "macos", wgpu_arm))]
/// Open a frame for the present about to run, or `None` when nothing listens
/// (no sink: zero cost at rest) or its ring slot is still in flight.
pub(crate) fn open_frame() -> Option<FrameToken<'static>> {
    FRAMES.open_frame()
}

#[cfg(any(target_os = "macos", wgpu_arm, test))]
impl FrameRing {
    fn open_frame(&self) -> Option<FrameToken<'_>> {
        self.sink.get()?;
        let serial = self.next_serial.fetch_add(1, Ordering::Relaxed);
        let s = self.slot(serial);
        if s.pending
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            self.skipped.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        // Claimed: the previous frame in this slot was delivered and nothing else
        // writes it until this frame's last release. `done_serial` is cleared
        // first so a concurrent `settled` reader of the old result sees it change.
        s.done_serial.store(0, Ordering::SeqCst);
        s.gpu_ns.store(0, Ordering::SeqCst);
        s.parts.store(0, Ordering::SeqCst);
        s.failed.store(false, Ordering::SeqCst);
        s.serial.store(serial, Ordering::SeqCst);
        Some(FrameToken { serial, ring: self })
    }
}

#[cfg(any(target_os = "macos", wgpu_arm, test))]
impl FrameToken<'_> {
    /// The frame's serial.
    pub(crate) const fn serial(self) -> u64 {
        self.serial
    }

    /// One more part is about to be submitted. Call before the work can
    /// complete (before `commit`), so the frame cannot be delivered without it.
    pub(crate) fn hold(self) {
        self.ring
            .slot(self.serial)
            .pending
            .fetch_add(1, Ordering::AcqRel);
    }

    /// A held part finished: `Some(ns)` is its GPU span, `None` a failure or
    /// a missing clock (the frame is then dropped), as is an overflowing sum.
    pub(crate) fn complete(self, span_ns: Option<u64>) {
        let s = self.ring.slot(self.serial);
        match span_ns {
            Some(ns) => {
                if s.gpu_ns
                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |total| {
                        total.checked_add(ns)
                    })
                    .is_err()
                {
                    s.failed.store(true, Ordering::SeqCst);
                }
                s.parts.fetch_add(1, Ordering::SeqCst);
            }
            None => s.failed.store(true, Ordering::SeqCst),
        }
        release(self);
    }

    /// The present returned: release the frame's own hold. The frame is
    /// delivered here when every part already completed.
    pub(crate) fn seal(self) {
        release(self);
    }
}

#[cfg(any(target_os = "macos", wgpu_arm, test))]
/// Drop one hold; the last one publishes the frame.
fn release(token: FrameToken<'_>) {
    let s = token.ring.slot(token.serial);
    let mut cur = s.pending.load(Ordering::Acquire);
    loop {
        debug_assert!(
            cur != 0 && cur != DELIVERING,
            "gpu_time: a release on a frame with no hold"
        );
        if cur == 1 {
            match s
                .pending
                .compare_exchange(1, DELIVERING, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    deliver(token, s);
                    s.pending.store(0, Ordering::Release);
                    return;
                }
                Err(now) => cur = now,
            }
        } else {
            match s
                .pending
                .compare_exchange(cur, cur - 1, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return,
                Err(now) => cur = now,
            }
        }
    }
}

#[cfg(any(target_os = "macos", wgpu_arm, test))]
fn deliver(token: FrameToken<'_>, s: &Slot) {
    let gpu_ns = s.gpu_ns.load(Ordering::SeqCst);
    let parts = s.parts.load(Ordering::SeqCst);
    let failed = s.failed.load(Ordering::SeqCst);
    s.done_serial.store(token.serial, Ordering::SeqCst);
    if failed {
        token.ring.dropped.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if parts == 0 {
        return;
    }
    token.ring.delivered.fetch_add(1, Ordering::Relaxed);
    if let Some(sink) = token.ring.sink.get() {
        sink(GpuFrameTime {
            serial: token.serial,
            gpu_ns,
            parts,
        });
    }
}

/// The result of frame `serial`, once delivered and until its ring slot is
/// reused; `None` while any part is outstanding, and for a serial never
/// opened or already evicted.
#[must_use]
pub fn settled(serial: u64) -> Option<Settled> {
    #[cfg(any(target_os = "macos", wgpu_arm, test))]
    {
        FRAMES.settled(serial)
    }
    #[cfg(not(any(target_os = "macos", wgpu_arm, test)))]
    {
        let _ = serial;
        None
    }
}

#[cfg(any(target_os = "macos", wgpu_arm, test))]
impl FrameRing {
    fn settled(&self, serial: u64) -> Option<Settled> {
        if serial == 0 {
            return None;
        }
        let s = self.slot(serial);
        if s.done_serial.load(Ordering::SeqCst) != serial {
            return None;
        }
        let gpu_ns = s.gpu_ns.load(Ordering::SeqCst);
        let parts = s.parts.load(Ordering::SeqCst);
        let failed = s.failed.load(Ordering::SeqCst);
        // A reopen clears `done_serial` before touching the fields, so an
        // unchanged word means the three reads belong to `serial`.
        if s.done_serial.load(Ordering::SeqCst) != serial {
            return None;
        }
        Some(if failed {
            Settled::Failed
        } else if parts == 0 {
            Settled::Untimed
        } else {
            Settled::Timed(GpuFrameTime {
                serial,
                gpu_ns,
                parts,
            })
        })
    }
}

#[cfg(any(target_os = "macos", test))]
/// One part's GPU span from a start and end on the same seconds clock
/// (`GPUStartTime`/`GPUEndTime`), or `None` when the device reported no
/// clock: a zero or non-finite reading, an end before the start, or a span
/// outside the unsigned 64-bit nanosecond domain.
#[must_use]
pub(crate) fn span_ns(start_s: f64, end_s: f64) -> Option<u64> {
    if !(start_s.is_finite() && end_s.is_finite()) || start_s <= 0.0 || end_s < start_s {
        return None;
    }
    let ns = Duration::try_from_secs_f64(end_s - start_s)
        .ok()?
        .as_nanos();
    u64::try_from(ns).ok()
}

#[cfg(test)]
mod tests {
    use super::{FrameRing, Settled, span_ns};

    fn listening() -> FrameRing {
        fn sink(_: super::GpuFrameTime) {}
        let ring = FrameRing::new();
        assert!(ring.sink.set(sink).is_ok());
        ring
    }

    /// A zero, non-finite or reversed reading is no clock; a real one is its
    /// span in nanoseconds.
    #[test]
    fn a_span_needs_a_real_clock_reading() {
        assert_eq!(span_ns(0.0, 1.0), None, "GPUStartTime 0: never ran");
        assert_eq!(span_ns(f64::NAN, 1.0), None);
        assert_eq!(span_ns(1.0, f64::INFINITY), None);
        assert_eq!(span_ns(2.0, 1.0), None, "end before start");
        assert_eq!(
            span_ns(1.0, f64::MAX),
            None,
            "finite readings can exceed the duration domain"
        );
        assert_eq!(
            span_ns(1.0, 1.0e18),
            None,
            "nanoseconds must fit the metric"
        );
        assert_eq!(span_ns(1.0, 20_000_000_001.0), None);
        assert_eq!(
            span_ns(1.0, 1_000_000_001.0),
            Some(1_000_000_000_000_000_000)
        );
        let ns = span_ns(10.0, 10.000_25).expect("a real span");
        assert!((249_000..=251_000).contains(&ns), "250 us read as {ns} ns");
        assert_eq!(span_ns(5.0, 5.0), Some(0));
    }

    /// The frame is the sum of its parts and is delivered exactly once, by
    /// whichever release is last: here a part completing after the seal.
    #[test]
    fn a_frame_sums_its_parts_and_settles_after_the_last_one() {
        let ring = listening();
        let f = ring.open_frame().expect("a sink is installed");
        f.hold();
        f.hold();
        f.complete(Some(300));
        f.seal();
        assert_eq!(
            ring.settled(f.serial()),
            None,
            "one part is still on the device"
        );
        f.complete(Some(200));
        assert_eq!(
            ring.settled(f.serial()),
            Some(Settled::Timed(super::GpuFrameTime {
                serial: f.serial(),
                gpu_ns: 500,
                parts: 2,
            }))
        );
    }

    /// A failed part drops the whole frame instead of reporting a short time.
    #[test]
    fn a_failed_part_drops_the_frame() {
        let ring = listening();
        let f = ring.open_frame().expect("a sink is installed");
        let before = ring.dropped.load(super::Ordering::Relaxed);
        f.hold();
        f.hold();
        f.complete(Some(1_000));
        f.complete(None);
        f.seal();
        assert_eq!(ring.settled(f.serial()), Some(Settled::Failed));
        assert!(ring.dropped.load(super::Ordering::Relaxed) > before);

        // Each part is representable in the metric, but their sum is not.
        // It must follow the same failed-frame path, never wrap to zero.
        let overflow = ring.open_frame().expect("a sink is installed");
        overflow.hold();
        overflow.hold();
        overflow.complete(Some(u64::MAX));
        overflow.complete(Some(1));
        overflow.seal();
        assert_eq!(ring.settled(overflow.serial()), Some(Settled::Failed));
    }

    /// A present that committed nothing is untimed, not a zero GPU time.
    #[test]
    fn a_frame_with_no_part_is_untimed() {
        let ring = listening();
        let f = ring.open_frame().expect("a sink is installed");
        f.seal();
        assert_eq!(ring.settled(f.serial()), Some(Settled::Untimed));
    }

    /// A slot still in flight a whole ring later is not reclaimed: that
    /// present goes untimed and is counted, and the old frame still settles.
    #[test]
    fn a_busy_ring_slot_skips_the_present_instead_of_corrupting_it() {
        let ring = listening();
        let independent = listening();
        let settled_elsewhere = independent.open_frame().expect("independent ring");
        settled_elsewhere.hold();
        settled_elsewhere.complete(Some(11));
        settled_elsewhere.seal();
        let held = ring.open_frame().expect("a sink is installed");
        held.hold();
        held.seal();
        // Walk this fixture's serial counter round to the held frame's slot.
        let before = ring.skipped.load(super::Ordering::Relaxed);
        for _ in 1..super::RING {
            ring.open_frame().expect("the other slots are free").seal();
        }
        assert!(
            ring.open_frame().is_none(),
            "the held slot must refuse a reopen"
        );
        assert_eq!(ring.skipped.load(super::Ordering::Relaxed), before + 1);
        assert!(
            matches!(
                independent.settled(settled_elsewhere.serial()),
                Some(Settled::Timed(t)) if t.gpu_ns == 11
            ),
            "eviction in one ring must not erase another fixture's result"
        );
        held.complete(Some(7));
        assert!(matches!(
            ring.settled(held.serial()),
            Some(Settled::Timed(t)) if t.gpu_ns == 7
        ));
    }

    /// Serial 0 and serials never opened read as nothing.
    #[test]
    fn unknown_serials_are_not_settled() {
        let ring = FrameRing::new();
        assert!(ring.open_frame().is_none(), "no sink means no frame");
        assert_eq!(ring.settled(0), None);
        assert_eq!(ring.settled(u64::MAX), None);
    }
}
