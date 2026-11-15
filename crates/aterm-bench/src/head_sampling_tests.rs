// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn a_record_lands_at_the_first_head_seen_past_it() {
    use super::*;

    let mut t = Timeline::default();
    assert!(t.is_empty());
    for (start, end, head) in [(0, 0, 10), (150, 150, 12), (400, 400, 13)] {
        t.observe(start, end, head);
    }
    assert!(!t.is_empty());
    assert_eq!(t.landed(10), Some(150), "offset 10 is the 11th record");
    assert_eq!(t.landed(11), Some(150));
    assert_eq!(t.landed(12), Some(400));
    assert_eq!(t.landed(13), None, "not yet seen");
    assert_eq!(t.landed(9), None, "landed before the sampler started");
    assert_eq!(Timeline::default().landed(0), None);
    let mut still = Timeline::default();
    still.observe(0, 0, 10);
    assert_eq!(still.landed(10), None);
}

#[test]
fn the_sampler_keeps_changes_only_and_stops_on_its_flag() {
    use super::*;

    use std::sync::atomic::AtomicU64;
    let acked = AtomicU64::new(5);
    let durable = AtomicU64::new(3);
    let stop = AtomicBool::new(false);
    let clock = Clock::start();
    let heads = std::thread::scope(|s| {
        let sampler = s.spawn(|| {
            sample_heads(
                clock,
                || {
                    (
                        acked.load(Ordering::Relaxed),
                        durable.load(Ordering::Relaxed),
                    )
                },
                &stop,
            )
        });
        let _stop = StopOnDrop(&stop);
        acked.store(6, Ordering::Relaxed);
        acked.store(8, Ordering::Relaxed);
        durable.store(8, Ordering::Relaxed);
        drop(_stop);
        sampler.join().expect("the sampler returns")
    });
    for (timeline, held) in [
        (&heads.acked, &[5, 6, 8][..]),
        (&heads.durable, &[3, 8][..]),
    ] {
        let entries = &timeline.changes;
        assert!(!entries.is_empty());
        assert!(held.contains(&entries[0].head));
        assert!(
            entries
                .windows(2)
                .all(|w| w[0].observed <= w[1].observed && w[0].head < w[1].head)
        );
        assert!(timeline.len() <= held.len());
    }
}

#[test]
fn an_observation_window_includes_a_transition_before_the_previous_read_finished() {
    use super::*;

    // The first head load happens before time30, but the pair's second head
    // load and timestamp finish at80. The old head may change during that read.
    let mut timeline = Timeline::default();
    timeline.observe(0, 80, 10);
    timeline.observe(100, 110, 11);
    assert_eq!(timeline.landed(10), Some(110));
    let (lower, upper) = timeline.latency_bounds_ms(20, 10).unwrap();
    let actual = ms(20, 30);
    assert!(
        lower <= actual && actual <= upper,
        "a transition consistent with both reads must remain possible: {lower} <= {actual} <= {upper}"
    );
}

#[test]
fn unchanged_polls_tighten_windows_without_adding_head_changes() {
    use super::*;

    let mut timeline = Timeline::default();
    for (start, end, head) in [(0, 1, 10), (20, 21, 10), (1000, 1001, 12)] {
        timeline.observe(start, end, head);
    }
    assert_eq!(timeline.len(), 2);
    for seq in [10, 11] {
        let (lower, upper) = timeline.latency_bounds_ms(0, seq).unwrap();
        assert_eq!((lower, upper), (0.019, 1.002));
    }
    assert!(timeline.latency_bounds_ms(0, 9).is_none());
    assert!(timeline.latency_bounds_ms(0, 12).is_none());
}

#[test]
fn a_head_rewind_or_backwards_clock_makes_observation_times_unavailable() {
    use super::*;

    for samples in [
        &[(0, 1, 10), (20, 21, 12), (30, 31, 11)][..],
        &[(0, 10, 10), (9, 12, 12)][..],
    ] {
        let mut timeline = Timeline::default();
        for &(start, end, head) in samples {
            timeline.observe(start, end, head);
        }
        assert!(!timeline.is_valid());
        assert!(timeline.landed(10).is_none());
        assert!(timeline.latency_bounds_ms(0, 10).is_none());
    }
}
