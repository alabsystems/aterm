// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The label can be reached, cannot be confused with an absence of
//! measurement, and decides one thing in one direction.

use super::*;

/// A healthy video take: ~18.3 ms between captures, 5.4 s of a 60 s budget,
/// a driver that stayed on CPU.
fn healthy() -> SchedInputs {
    SchedInputs {
        ticks: 100,
        late_max_us: 3_000,
        sampling_hole_us: 18_400,
        nominal_dt_us: 18_300,
        wall_us: 5_400_000,
        budget_us: 60_000_000,
        late_threshold_us: DEFAULT_LATE_THRESHOLD_US,
        produced_take: true,
        qos: Some(Qos::UserInteractive),
    }
}

#[test]
fn a_take_that_got_its_time_disowns_only_driver_starvation() {
    let s = classify(&healthy());
    assert!(matches!(
        s,
        Starvation::KeptCadence {
            late_max_us: 3_000,
            ticks: 100,
            ..
        }
    ));
    assert_eq!((s.label(), s.evidence()), (Starved::No, Evidence::Sound));
}

#[test]
fn no_take_at_all_is_unknown_not_a_verdict_about_paint() {
    let s = classify(&SchedInputs {
        produced_take: false,
        ..healthy()
    });
    assert_eq!(s, Starvation::NoTake);
    assert_eq!(
        (s.label(), s.evidence()),
        (Starved::Unknown, Evidence::Unproved)
    );
}

#[test]
fn a_descheduled_driver_is_named_and_the_take_is_disowned() {
    let s = classify(&SchedInputs {
        late_max_us: 310_000,
        ..healthy()
    });
    assert!(matches!(
        s,
        Starvation::Descheduled {
            late_max_us: 310_000,
            ..
        }
    ));
    assert_eq!(
        (s.label(), s.evidence()),
        (Starved::Yes, Evidence::Unproved)
    );
}

#[test]
fn a_blown_capture_cadence_is_starvation_even_when_the_driver_ran() {
    let blown = classify(&SchedInputs {
        sampling_hole_us: 60_000,
        ..healthy()
    });
    assert_eq!(
        blown,
        Starvation::CadenceBlown {
            hole_us: 60_000,
            nominal_us: 18_300
        }
    );
    // A hole at the nominal interval is not: a red there is a rendered gap.
    assert_eq!(classify(&healthy()).label(), Starved::No);
}

#[test]
fn a_take_racing_its_budget_is_not_evidence() {
    let s = classify(&SchedInputs {
        wall_us: 58_000_000,
        ..healthy()
    });
    assert!(matches!(s, Starvation::BudgetPressure { .. }));
    assert_eq!(s.evidence(), Evidence::Unproved);
}

#[test]
fn no_samples_is_unknown_and_never_a_clean_bill() {
    let s = classify(&SchedInputs {
        ticks: 0,
        ..healthy()
    });
    assert_eq!(s, Starvation::NoSamples);
    assert_eq!(s.evidence(), Evidence::Unproved);
}

#[test]
fn a_starved_pass_is_unproved_and_a_sound_take_is_decided_by_its_pixels() {
    assert_eq!(
        paint_verdict(ShapeVerdict::Pass, Evidence::Unproved),
        Disposition::Unproved
    );
    assert_eq!(
        paint_verdict(ShapeVerdict::Pass, Evidence::Sound),
        Disposition::Pass
    );
    // Starvation may take a green away but never rescue a red.
    for standing in [Evidence::Sound, Evidence::Unproved] {
        assert_eq!(
            paint_verdict(ShapeVerdict::Fail, standing),
            Disposition::Fail
        );
    }
}

#[test]
fn a_clamped_tier_is_named_and_repeating_its_take_is_still_zero_evidence() {
    for _ in 0..8 {
        let s = classify(&SchedInputs {
            late_max_us: 88_046,
            qos: Some(Qos::Utility),
            ..healthy()
        });
        assert!(matches!(s, Starvation::Descheduled { qos: Some(q), .. } if q.clamped()));
        assert_eq!(
            paint_verdict(ShapeVerdict::Pass, s.evidence()),
            Disposition::Unproved
        );
    }
    assert!(!Qos::UserInteractive.clamped() && !Qos::Default.clamped());
    assert_eq!(Qos::from_raw(0x11), Some(Qos::Utility));
    assert_eq!(Qos::from_raw(0x42), None);
}

const MS: Duration = Duration::from_millis(1);

#[test]
fn lateness_is_measured_from_the_fixed_deadline() {
    let mut r = SchedReading::default();
    r.book(MS * 50, MS * 52, false);
    r.book(MS * 100, MS * 160, false);
    r.book(MS * 150, MS * 150, false);
    assert_eq!(r.ticks, 3);
    assert_eq!((r.late_max_us, r.late_max_at_us), (60_000, 100_000));
    assert_eq!(r.late_total_us, 62_000);
    // A tick that woke early books no negative lateness.
    r.book(MS * 200, MS * 190, false);
    assert_eq!(r.late_total_us, 62_000);
}

#[test]
fn the_capture_window_is_booked_apart_from_the_life() {
    let mut r = SchedReading::default();
    r.book(MS * 50, MS * 120, false); // a deschedule outside the window
    r.book(MS * 100, MS * 101, true);
    r.book(MS * 150, MS * 153, true);
    r.book(MS * 200, MS * 200, false);
    assert_eq!((r.win_ticks, r.ticks), (2, 4));
    assert_eq!(r.win_late_max_us, 3_000);
    assert_eq!(
        (r.win_first_at_us, r.win_last_at_us),
        (Some(100_000), Some(150_000))
    );
    // The window decides the standing when it was sampled; the life otherwise.
    assert_eq!(r.deciding(), (2, 3_000));
    let unwindowed = SchedReading { win_ticks: 0, ..r };
    assert_eq!(unwindowed.deciding(), (4, 70_000));
}
