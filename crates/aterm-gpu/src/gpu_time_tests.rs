// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
