// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for [`EventLog::append_at`] and the `NoSilentLoss` invariant.

use aterm_buffer::{BlobId, Event, EventLog, KeyframeId, MAX_LOG_EVENTS, Op, Ticks};

#[test]
fn append_at_records_seq_tick_and_op() {
    let mut log = EventLog::default();
    let (s1, ev1) = log.append_at(Op::RawIn(BlobId(7)), Ticks(100));
    let (s2, ev2) = log.append_at(Op::Reply(BlobId(8)), Ticks(150));
    let (s3, ev3) = log.append_at(Op::Resize { rows: 24, cols: 80 }, Ticks(150));
    let (s4, _ev4) = log.append_at(Op::Keyframe(KeyframeId(1)), Ticks(200));

    assert_eq!(
        (s1.0, s2.0, s3.0, s4.0),
        (1, 2, 3, 4),
        "monotone gap-free seq"
    );
    assert!(
        ev1.is_none() && ev2.is_none() && ev3.is_none(),
        "no eviction under capacity"
    );

    let live: Vec<&Event> = log.live().collect();
    assert_eq!(live.len(), 4);
    assert_eq!(live[0].ts, Ticks(100), "tick recorded");
    assert_eq!(live[0].op, Op::RawIn(BlobId(7)), "op handle recorded");
    assert_eq!(live[2].op, Op::Resize { rows: 24, cols: 80 });
    assert_eq!(log.total(), 4);
}

#[test]
fn append_at_spills_every_evicted_event_no_silent_loss() {
    let mut log = EventLog::default();
    let overflow = 5_000usize;
    let total = MAX_LOG_EVENTS + overflow;

    let mut tiered: Vec<Event> = Vec::new();
    for i in 0..total {
        let (_seq, evicted) = log.append_at(Op::RawIn(BlobId(i as u64)), Ticks(i as u64));
        if let Some(ev) = evicted {
            tiered.push(ev);
        }
    }

    let live: Vec<Event> = log.live().cloned().collect();
    assert_eq!(
        live.len(),
        MAX_LOG_EVENTS,
        "live ring saturates at MAX_LOG_EVENTS"
    );
    assert_eq!(
        tiered.len(),
        overflow,
        "every over-cap event was spilled (not dropped)"
    );

    let mut all_seqs: Vec<u64> = tiered.iter().chain(live.iter()).map(|e| e.seq.0).collect();
    let reconstructed = all_seqs.clone();
    all_seqs.sort_unstable();
    all_seqs.dedup();
    assert_eq!(all_seqs.len(), total, "no seq lost and none duplicated");
    assert_eq!(*all_seqs.first().unwrap(), 1);
    assert_eq!(*all_seqs.last().unwrap(), total as u64);
    assert!(
        reconstructed.windows(2).all(|w| w[0] + 1 == w[1]),
        "tiered++live is the contiguous spine in order"
    );
    assert_eq!(
        tiered[0].ts,
        Ticks(0),
        "oldest spilled event keeps its tick"
    );
}

#[test]
fn dropping_the_evicted_event_loses_seqs_control() {
    let mut log = EventLog::default();
    let overflow = 1_000usize;
    let total = MAX_LOG_EVENTS + overflow;
    for i in 0..total {
        let _ = log.append_at(Op::RawIn(BlobId(i as u64)), Ticks(i as u64));
    }
    let live: Vec<Event> = log.live().cloned().collect();
    assert_eq!(live.len(), MAX_LOG_EVENTS);
    assert_eq!(
        live.first().unwrap().seq.0,
        overflow as u64 + 1,
        "oldest live seq advanced past the dropped prefix"
    );
    assert!(
        !live.iter().any(|e| e.seq.0 <= overflow as u64),
        "the dropped prefix is unrecoverable without the spill seam (control)"
    );
}
