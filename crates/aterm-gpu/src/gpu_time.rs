// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-frame GPU execution timing ([`FrameToken::complete_parts`](crate::gpu_time::FrameToken::complete_parts)).

#[cfg(any(target_os = "macos", test))]
use std::sync::OnceLock;
#[cfg(any(target_os = "macos", test))]
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
    /// How many parts were summed: Metal command buffers.
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

/// The frontend's receiver.
pub type GpuTimeSink = fn(GpuFrameTime);

/// Whether this build has a device execution clock.
pub const SUPPORTED: bool = cfg!(target_os = "macos");

#[cfg(any(target_os = "macos", test))]
static FRAMES: FrameRing = FrameRing::new();

/// Frames in flight at once.
#[cfg(any(target_os = "macos", test))]
const RING: usize = 64;

/// `pending` while a slot is being delivered: no open can claim it and no part can be added to it.
#[cfg(any(target_os = "macos", test))]
const DELIVERING: u32 = u32::MAX;

/// One frame's accumulator.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug)]
struct Slot {
    /// Outstanding holds: the seal's one plus one per unfinished part ([`DELIVERING`]).
    pending: AtomicU32,
    /// The serial of the open frame using this slot.
    serial: AtomicU64,
    /// Serial whose result `gpu_ns`/`parts`/`failed` hold once delivered (0 while open).
    done_serial: AtomicU64,
    gpu_ns: AtomicU64,
    parts: AtomicU32,
    failed: AtomicBool,
}

#[cfg(any(target_os = "macos", test))]
impl Slot {
    #[inline]
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

/// One accounting domain.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug)]
pub(crate) struct FrameRing {
    sink: OnceLock<GpuTimeSink>,
    /// [`set_paused`]: open no frame, as with no sink (a development seam).
    #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
    paused: AtomicBool,
    next_serial: AtomicU64,
    delivered: AtomicU64,
    dropped: AtomicU64,
    skipped: AtomicU64,
    slots: [Slot; RING],
}

#[cfg(any(target_os = "macos", test))]
impl FrameRing {
    const fn new() -> Self {
        Self {
            sink: OnceLock::new(),
            #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
            paused: AtomicBool::new(false),
            next_serial: AtomicU64::new(1),
            delivered: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            slots: [const { Slot::new() }; RING],
        }
    }

    #[inline]
    fn slot(&self, serial: u64) -> &Slot {
        &self.slots[usize::try_from(serial % RING as u64).unwrap_or(0)]
    }

    /// A test's own ring, listening (a no-op sink), so frames open.
    #[cfg(test)]
    pub(crate) fn listening() -> Self {
        fn sink(_: GpuFrameTime) {}
        let ring = Self::new();
        assert!(ring.sink.set(sink).is_ok());
        ring
    }
}

/// Install the process's one sink.
pub fn install_sink(sink: GpuTimeSink) -> bool {
    #[cfg(any(target_os = "macos", test))]
    {
        FRAMES.sink.set(sink).is_ok()
    }
    #[cfg(not(any(target_os = "macos", test)))]
    {
        let _ = sink;
        false
    }
}

/// Pause the clock (a development seam a shipped binary never has).
#[inline]
#[cfg(any(test, debug_assertions, feature = "dev-seams"))]
pub fn set_paused(paused: bool) {
    #[cfg(any(target_os = "macos", test))]
    FRAMES.paused.store(paused, Ordering::Relaxed);
    #[cfg(not(any(target_os = "macos", test)))]
    let _ = paused;
}

/// Whether a sink is installed — presents open frames only then.
#[must_use]
#[inline]
pub fn sink_installed() -> bool {
    #[cfg(any(target_os = "macos", test))]
    {
        FRAMES.sink.get().is_some()
    }
    #[cfg(not(any(target_os = "macos", test)))]
    {
        false
    }
}

/// Frames delivered to the sink since process start.
#[must_use]
#[inline]
pub fn delivered() -> u64 {
    #[cfg(any(target_os = "macos", test))]
    {
        FRAMES.delivered.load(Ordering::Relaxed)
    }
    #[cfg(not(any(target_os = "macos", test)))]
    {
        0
    }
}

/// Frames dropped because a part failed or reported no clock.
#[must_use]
#[inline]
pub fn dropped() -> u64 {
    #[cfg(any(target_os = "macos", test))]
    {
        FRAMES.dropped.load(Ordering::Relaxed)
    }
    #[cfg(not(any(target_os = "macos", test)))]
    {
        0
    }
}

/// Presents that went untimed because their ring slot was still in flight.
#[must_use]
#[inline]
pub fn skipped() -> u64 {
    #[cfg(any(target_os = "macos", test))]
    {
        FRAMES.skipped.load(Ordering::Relaxed)
    }
    #[cfg(not(any(target_os = "macos", test)))]
    {
        0
    }
}

#[cfg(any(target_os = "macos", test))]
/// One open frame: the handle every part of it reports through.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameToken<'a> {
    serial: u64,
    ring: &'a FrameRing,
}

#[cfg(target_os = "macos")]
/// Open a frame for the present about to run, or `None` when no sink is installed.
#[inline]
pub(crate) fn open_frame() -> Option<FrameToken<'static>> {
    FRAMES.open_frame()
}

#[cfg(any(target_os = "macos", test))]
impl FrameRing {
    pub(crate) fn open_frame(&self) -> Option<FrameToken<'_>> {
        self.sink.get()?;
        #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
        if self.paused.load(Ordering::Relaxed) {
            return None;
        }
        let serial = self.next_serial.fetch_add(1, Ordering::Relaxed);
        let s = self.slot(serial);
        if s.pending
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            self.skipped.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        s.done_serial.store(0, Ordering::SeqCst);
        s.gpu_ns.store(0, Ordering::SeqCst);
        s.parts.store(0, Ordering::SeqCst);
        s.failed.store(false, Ordering::SeqCst);
        s.serial.store(serial, Ordering::SeqCst);
        Some(FrameToken { serial, ring: self })
    }
}

#[cfg(any(target_os = "macos", test))]
impl FrameToken<'_> {
    /// The frame's serial.
    #[inline]
    pub(crate) const fn serial(self) -> u64 {
        self.serial
    }

    /// One more part is about to be submitted.
    #[inline]
    pub(crate) fn hold(self) {
        self.ring
            .slot(self.serial)
            .pending
            .fetch_add(1, Ordering::AcqRel);
    }

    /// A held part finished.
    #[inline]
    pub(crate) fn complete(self, span_ns: Option<u64>) {
        self.complete_parts(span_ns, 1);
    }

    /// Complete a held multi-part span counting `parts` parts.
    pub(crate) fn complete_parts(self, span_ns: Option<u64>, parts: u32) {
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
                s.parts.fetch_add(parts, Ordering::SeqCst);
            }
            None => s.failed.store(true, Ordering::SeqCst),
        }
        release(self);
    }

    /// The present returned: release the frame's own hold.
    #[inline]
    pub(crate) fn seal(self) {
        release(self);
    }
}

#[cfg(any(target_os = "macos", test))]
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

#[cfg(any(target_os = "macos", test))]
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

/// The result of frame `serial`, once delivered and until its ring slot is reused.
#[must_use]
#[inline]
pub fn settled(serial: u64) -> Option<Settled> {
    #[cfg(any(target_os = "macos", test))]
    {
        FRAMES.settled(serial)
    }
    #[cfg(not(any(target_os = "macos", test)))]
    {
        let _ = serial;
        None
    }
}

#[cfg(any(target_os = "macos", test))]
impl FrameRing {
    pub(crate) fn settled(&self, serial: u64) -> Option<Settled> {
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
/// Compute a GPU span in nanoseconds from `(start_s, end_s)`, or `None` if invalid.
#[must_use]
#[inline]
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
        FrameRing::listening()
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

    /// A paused ring opens nothing and counts nothing; resumed, it times again.
    #[test]
    fn a_paused_clock_opens_no_frame() {
        let ring = listening();
        ring.paused.store(true, super::Ordering::Relaxed);
        let skipped = ring.skipped.load(super::Ordering::Relaxed);
        assert!(ring.open_frame().is_none(), "a paused clock opens no frame");
        assert_eq!(ring.skipped.load(super::Ordering::Relaxed), skipped);
        ring.paused.store(false, super::Ordering::Relaxed);
        let f = ring.open_frame().expect("resumed, it times again");
        f.seal();
        assert_eq!(ring.settled(f.serial()), Some(Settled::Untimed));
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
