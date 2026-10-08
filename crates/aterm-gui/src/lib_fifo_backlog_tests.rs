// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The FIFO backlog model ([`super::FifoBacklog`]) and the two pacing decisions that
//! read it, driven against a discrete FIFO (one queued frame shown per vblank, as on a
//! Mailbox-less X11 Vulkan surface), so assertions are about when frames reach glass.
use super::{
    CONTENT_FIFO_BACKLOG_MAX, EFFECT_FIFO_BACKLOG_MAX, FifoBacklog, PresentQueue,
    content_floor_after_fifo_drain, effect_slot_after_fifo_drain, present_queue_admits_panel_rate,
};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const P: Duration = Duration::from_nanos(16_666_667);

#[test]
fn the_model_adds_one_per_present_and_drains_one_per_period() {
    let t0 = Instant::now();
    let mut b = FifoBacklog::default();
    assert_eq!(b.level_at(t0), 0.0);
    assert_eq!(b.drained_to(0.0), None, "an empty queue owes no wait");
    b.note_present(t0, P);
    assert!((b.level_at(t0) - 1.0).abs() < 1e-9);
    assert!((b.level_at(t0 + P / 2) - 0.5).abs() < 1e-6);
    assert_eq!(b.level_at(t0 + P * 3), 0.0, "never negative");
    // A second present half a period later: 1.5 frames pending.
    b.note_present(t0 + P / 2, P);
    assert!((b.level_at(t0 + P / 2) - 1.5).abs() < 1e-6);
    let drained = b.drained_to(CONTENT_FIFO_BACKLOG_MAX).expect("above one");
    let want = t0 + P;
    let err = drained.max(want) - drained.min(want);
    assert!(err < Duration::from_micros(1), "drains to 1 at t0 + P");
    assert_eq!(b.drained_to(2.0), None);
}

/// The effect slot waits for the drain, but never past the halved lane's
/// slot; the content floor stretches only when the drain is later.
#[test]
fn the_two_gates_are_bounded() {
    let t0 = Instant::now();
    let fresh = t0 + P;
    let cap = t0 + P * 2;
    assert_eq!(effect_slot_after_fifo_drain(fresh, None, cap), fresh);
    assert_eq!(
        effect_slot_after_fifo_drain(fresh, Some(t0 + P / 2), cap),
        fresh,
        "an earlier drain never pulls a slot forward"
    );
    assert_eq!(
        effect_slot_after_fifo_drain(fresh, Some(t0 + P * 3 / 2), cap),
        t0 + P * 3 / 2
    );
    assert_eq!(
        effect_slot_after_fifo_drain(fresh, Some(t0 + P * 5), cap),
        cap,
        "an over-estimate costs at most the halving"
    );
    let half = P / 2;
    assert_eq!(content_floor_after_fifo_drain(half, None, Some(t0)), half);
    assert_eq!(
        content_floor_after_fifo_drain(half, Some(t0 + P), Some(t0)),
        P
    );
    assert_eq!(
        content_floor_after_fifo_drain(half, Some(t0 + P / 4), Some(t0)),
        half
    );
    assert_eq!(
        content_floor_after_fifo_drain(half, Some(t0 + P), None),
        half
    );
}

/// Findings 4 and 5 of the review: the panel-rate effect lane only where
/// the presenter can absorb it.
#[test]
fn panel_rate_needs_a_presenter_that_absorbs_it() {
    assert!(present_queue_admits_panel_rate(PresentQueue::NonBlocking));
    assert!(present_queue_admits_panel_rate(PresentQueue::Fifo(2)));
    assert!(present_queue_admits_panel_rate(PresentQueue::Fifo(3)));
    assert!(
        !present_queue_admits_panel_rate(PresentQueue::Fifo(1)),
        "a two-image FIFO is the pool the halving exists for"
    );
    assert!(
        !present_queue_admits_panel_rate(PresentQueue::Cpu),
        "the CPU presenter has no acquire for the park latch to hear"
    );
    assert!(present_queue_admits_panel_rate(PresentQueue::Unknown));
}

/// A discrete FIFO: frames shown one per vblank, vblanks at
/// `phase + k * P`. Returns each frame's (present, glass) pair.
fn glass_times(presents: &[Instant], first_vblank: Instant, end: Instant) -> Vec<Duration> {
    let mut queue: VecDeque<Instant> = VecDeque::new();
    let mut out = Vec::new();
    let mut next = 0usize;
    let mut vblank = first_vblank;
    while vblank <= end {
        while next < presents.len() && presents[next] <= vblank {
            queue.push_back(presents[next]);
            next += 1;
        }
        if let Some(p) = queue.pop_front() {
            out.push(vblank - p);
        }
        vblank += P;
    }
    out
}

/// Drive a panel-rate effect train with one echo landing 3 ms after an effect frame,
/// through the lane's real rules (phase-locked slot, the post-present rebase
/// `rebase_pending_trail_tick_after_present`, and the drain gate when `yield_to_fifo`).
/// Returns every frame's glass latency, in present order.
fn run(yield_to_fifo: bool) -> (Vec<Instant>, Vec<Duration>) {
    let t0 = Instant::now();
    let echo_at = t0 + P * 10 + Duration::from_millis(3);
    let end = t0 + P * 40;
    let mut backlog = FifoBacklog::default();
    let mut presents = Vec::new();
    let mut echoed = false;
    let mut slot = t0;
    loop {
        let (at, content) = if !echoed && echo_at < slot {
            echoed = true;
            (echo_at, true)
        } else {
            (slot, false)
        };
        if at > end {
            break;
        }
        if content {
            // The echo's own floor (counted from the last CONTENT present,
            // long ago here) must not hold it: only one frame is pending.
            let last_content = t0;
            let floor = content_floor_after_fifo_drain(
                P / 2,
                backlog.drained_to(CONTENT_FIFO_BACKLOG_MAX),
                Some(last_content),
            );
            assert!(
                last_content + floor <= at,
                "the echo is presented the moment it arrives"
            );
        }
        presents.push(at);
        backlog.note_present(at, P);
        let fresh = at + P;
        slot = if yield_to_fifo {
            effect_slot_after_fifo_drain(
                fresh,
                backlog.drained_to(EFFECT_FIFO_BACKLOG_MAX),
                at + P * 2,
            )
        } else {
            fresh
        };
    }
    let glass = glass_times(&presents, t0 + Duration::from_millis(5), end + P * 4);
    (presents, glass)
}

/// Without the drain gate the echo's extra frame rides the queue for the rest of the
/// episode (every later frame over a refresh late); with it the lane skips part of one
/// period and frames reach glass within one refresh.
#[test]
fn an_echo_mid_train_drains_instead_of_riding_the_queue() {
    let (_, stuck) = run(false);
    let tail = &stuck[stuck.len() - 10..];
    assert!(
        tail.iter().all(|g| *g > P),
        "negative control: without the gate the queue never drains: {tail:?}"
    );

    let (presents, drained) = run(true);
    let tail = &drained[drained.len() - 10..];
    assert!(
        tail.iter().all(|g| *g <= P),
        "with the gate every late frame reaches glass within a refresh: {tail:?}"
    );
    // The price: one effect slot moved, and never past the halved lane.
    let gaps: Vec<Duration> = presents.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(gaps.iter().all(|g| *g <= P * 2), "{gaps:?}");
    assert!(
        gaps.iter()
            .filter(|g| **g > P + Duration::from_millis(1))
            .count()
            <= 1
    );
}
