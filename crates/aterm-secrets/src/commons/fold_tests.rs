// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: u64 = 1_800_000_000_000;

fn seat(ago_ms: u64, build: Option<&str>) -> Heard<'_> {
    Heard {
        last_heard_ms: Some(NOW - ago_ms),
        build,
    }
}

const GATE: Gate = Gate {
    reads: Some((0, 158)),
    r2_shipped_ms: None,
};

#[test]
fn every_seat_heard_within_a_week_on_a_reading_build_holds_it() {
    let seats = [
        seat(1_000, Some("0.158.0")),
        seat(DAY_MS, Some("0.159.0+g1234abcd")),
        // Heard eight days ago on an old build: it no longer holds the gate shut.
        seat(8 * DAY_MS, Some("0.154.0")),
        Heard {
            last_heard_ms: None,
            build: None,
        },
    ];
    assert!(GATE.holds(&seats, NOW));
    assert!(GATE.behind(&seats, NOW).is_empty());
}

#[test]
fn one_seat_heard_on_an_older_build_or_none_keeps_it_shut() {
    for old in [
        Some("0.157.0+gdeadbeef"),
        None,
        Some("0.158"),
        Some("0.158.0+"),
        Some("00.158.0"),
    ] {
        let seats = [seat(1_000, Some("0.158.0")), seat(6 * DAY_MS, old)];
        assert!(!GATE.holds(&seats, NOW), "{old:?} reads no fold keys");
        assert_eq!(GATE.behind(&seats, NOW), vec![1]);
    }
}

#[test]
fn the_bound_holds_it_sixty_days_after_r2_whatever_the_seats_run() {
    let seats = [seat(1_000, Some("0.154.0"))];
    let bounded = Gate {
        r2_shipped_ms: Some(NOW - BOUND_MS),
        ..GATE
    };
    assert!(bounded.holds(&seats, NOW));
    let early = Gate {
        r2_shipped_ms: Some(NOW - BOUND_MS + 1),
        ..GATE
    };
    assert!(!early.holds(&seats, NOW));
}

#[test]
fn the_compiled_facts_hold_nothing_before_r2_and_no_seat_holds_nothing() {
    // Before R2 is cut no build counts and no bound runs.
    let compiled = Gate::compiled();
    if compiled.reads.is_none() && compiled.r2_shipped_ms.is_none() {
        assert!(!compiled.holds(&[seat(0, Some("9.9.9"))], NOW));
    }
    // A lab with no seat heard holds nothing open: silence is no adoption.
    assert!(!GATE.holds(&[], NOW));
    assert!(!GATE.holds(&[seat(30 * DAY_MS, Some("0.158.0"))], NOW));
}
