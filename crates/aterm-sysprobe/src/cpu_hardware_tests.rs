// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
fn states(values: &[(&str, i64)]) -> Vec<State> {
    values
        .iter()
        .map(|(name, ticks)| State {
            name: (*name).into(),
            ticks: *ticks,
        })
        .collect()
}
#[test]
fn hardware_residency_keeps_idle_separate_from_active_frequency() {
    let s = states(&[("DOWN", 100), ("IDLE", 400), ("P0", 100), ("P1", 400)]);
    let (active, frequency) = interpret(&s, Some(&[1000.0, 2000.0]));
    assert_eq!(active, Ok(0.5));
    assert_eq!(frequency, Ok(1800.0));
    let native = states(&[("IDLE", 500), ("V0P1", 100), ("V1P0", 400)]);
    assert_eq!(
        interpret(&native, Some(&[1000.0, 2000.0])),
        (Ok(0.5), Ok(1800.0))
    );
    let (active, frequency) = interpret(&states(&[("IDLE", 100), ("P0", 0)]), Some(&[1000.0]));
    assert_eq!(active, Ok(0.0));
    assert!(frequency.is_err());
}
#[test]
fn invalid_residency_and_incomplete_frequency_are_not_zero() {
    for s in [
        states(&[]),
        states(&[("IDLE", -1), ("P0", 2)]),
        states(&[("IDLE", 0), ("P0", 0)]),
        states(&[("P0", 5), ("IDLE", 3)]),
    ] {
        assert!(interpret(&s, Some(&[1000.0])).0.is_err(), "{s:?}");
    }
    let s = states(&[("IDLE", 2), ("P0", 3), ("P1", 5)]);
    assert!(interpret(&s, Some(&[1000.0])).1.is_err());
    assert!(interpret(&s, None).1.is_err());
    let out_of_order = states(&[("IDLE", 2), ("P1", 3), ("P0", 5)]);
    assert!(interpret(&out_of_order, Some(&[1000.0, 2000.0])).1.is_err());
}

/// A snapshot more than its demand's gap after the previous one reads `warming` and starts
/// the next interval from itself: it never pairs with the snapshot before the gap, whose
/// delta would be the gap's average. CONTROL: today's reader paired every snapshot with the
/// previous one whatever the gap (an unbounded gap here), so after the same 9 s gap it read
/// the 9 s average.
#[test]
fn a_cpu_stats_sample_after_a_gap_reads_warming_not_the_gaps_average() {
    let t0 = Instant::now();
    let at = |s: u64| Link {
        snapshot: s,
        after: t0 + Duration::from_secs(s),
        call_ns: s + 1,
    };
    let pair =
        |p: Option<Pair<'_, u64>>| p.map(|p| (p.previous, *p.current, p.window_ns, p.span_ns));
    let gap = Duration::from_millis(2_500);
    let mut chain = Chain::default();
    assert_eq!(pair(chain.push(at(0), gap)), None, "the first sample warms");
    assert_eq!(
        pair(chain.push(at(1), gap)),
        Some((0, 1, 1_000_000_000, 3)),
        "the window between returns, and both calls' span"
    );
    assert!(chain.warm(t0 + Duration::from_millis(3_500), gap));
    assert!(!chain.warm(t0 + Duration::from_millis(3_501), gap));
    assert_eq!(
        pair(chain.push(at(10), gap)),
        None,
        "after a 9 s gap: warming"
    );
    assert_eq!(
        pair(chain.push(at(11), gap)),
        Some((10, 11, 1_000_000_000, 23)),
        "the sample after the gap starts the next interval"
    );
    chain.clear();
    assert!(
        !chain.warm(t0 + Duration::from_secs(11), gap),
        "cleared: cold"
    );

    let mut today = Chain::default();
    let unbounded = Duration::MAX;
    let _ = today.push(at(0), unbounded);
    let _ = today.push(at(1), unbounded);
    assert_eq!(
        pair(today.push(at(10), unbounded)),
        Some((1, 10, 9_000_000_000, 13)),
        "control: with no gap rule the sample after the gap is its average"
    );
}
