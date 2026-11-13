// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{DrainedEdge, INPUT_SLICE_CAP_NS, Metrics, PendingInputStamp};

#[test]
fn a_burst_before_one_present_books_every_edge() {
    let mut pending = PendingInputStamp::default();
    for arrival in [10, 20, 30, 40] {
        pending.arm(arrival, arrival);
    }
    assert_eq!(pending.drain_slices(110), vec![100, 90, 80, 70]);
    assert!(!pending.is_pending());
}

#[test]
fn a_full_ring_keeps_the_oldest_edges_and_refuses_the_rest() {
    let mut pending = PendingInputStamp::default();
    let held: Vec<bool> = (1..=(super::INPUT_EDGE_RING as u64 + 3))
        .map(|arrival| pending.arm(arrival, arrival))
        .collect();
    assert!(held[..super::INPUT_EDGE_RING].iter().all(|&h| h));
    assert_eq!(&held[super::INPUT_EDGE_RING..], &[false, false, false]);
    let slices = pending.drain_slices(1_000);
    assert_eq!(slices.len(), super::INPUT_EDGE_RING);
    assert_eq!(slices.first(), Some(&999), "the oldest edge is kept");
}

#[test]
fn each_key_a_full_ring_refuses_is_counted_and_no_other() {
    let m = Metrics::new();
    let dropped = || m.window.input.edges_dropped.get();
    let mut pending = PendingInputStamp::default();
    for _ in 0..super::INPUT_EDGE_RING + 3 {
        m.note_window_input(&mut pending, 1_000);
    }
    assert_eq!(dropped(), 3, "one count per refused key");

    let fresh = 1_000 + INPUT_SLICE_CAP_NS + 1;
    m.note_window_input(&mut pending, fresh);
    assert_eq!(dropped(), 3, "an accepted key was counted");
    assert_eq!(
        pending.drain_slices(fresh + 7),
        vec![7],
        "the fresh key was refused"
    );
}

#[test]
fn a_full_ring_of_aged_out_edges_takes_a_fresh_key() {
    let mut pending = PendingInputStamp::default();
    for arrival in 1..=super::INPUT_EDGE_RING as u64 {
        assert!(pending.arm(arrival, arrival));
    }
    let fresh = super::INPUT_EDGE_RING as u64 + INPUT_SLICE_CAP_NS + 1;
    assert!(
        pending.arm(fresh, fresh),
        "a full ring of never-echoed keys refused the fresh one"
    );
    assert_eq!(
        pending.drain_slices(fresh + 7),
        vec![7],
        "only the fresh key books a slice"
    );
    for arrival in 1..=super::INPUT_EDGE_RING as u64 {
        assert!(pending.arm(arrival, arrival));
    }
    assert!(!pending.arm(1_000, 1_000), "live edges are never evicted");
}

#[test]
fn a_stamp_armed_before_reset_is_classed_as_the_previous_windows() {
    let mut pending = PendingInputStamp::default();
    pending.arm(50, 60);
    assert_eq!(
        pending.drained(10_000, 61),
        vec![DrainedEdge {
            slice_ns: 9_950,
            before_epoch: true,
        }],
        "drained, and classed as the previous window's"
    );
    assert!(!pending.is_pending(), "discarded, not left armed");

    pending.arm(50, 61);
    assert_eq!(
        pending.drained(10_050, 61),
        vec![DrainedEdge {
            slice_ns: 10_000,
            before_epoch: false,
        }]
    );
}

#[test]
fn a_keystroke_that_never_echoed_ages_out() {
    let mut pending = PendingInputStamp::default();
    pending.arm(1, 1);
    assert!(pending.drain_slices(1 + INPUT_SLICE_CAP_NS + 1).is_empty());
    pending.arm(1, 1);
    assert_eq!(
        pending.drain_slices(1 + INPUT_SLICE_CAP_NS),
        vec![INPUT_SLICE_CAP_NS]
    );
}
