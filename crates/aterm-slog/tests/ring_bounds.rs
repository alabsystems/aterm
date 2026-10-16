// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ring's bounds and cursor contract (§5.1): both bounds hold over far
//! more appends than the ring keeps, every eviction is counted exactly, and
//! a reader is told exactly what it lost.

use aterm_slog::{
    Conf, Cursor, Fields, GapWhy, Key, Kind, Limits, Log, MAX_BYTES, MAX_RECORDS, Mark, Src,
};
use aterm_spec::rng::XorShift;

use crate::support::at;

/// 10^5+ appends of varying sizes: after every one the ring is within 2048
/// records AND 256 KiB, `lost` is exactly the records no longer held, and a
/// reader from the start is told that number.
#[test]
fn many_appends_stay_within_both_bounds_and_count_every_eviction() {
    let mut log = Log::new(0);
    let mut rng = XorShift::new(7);
    let mut bytes_bound_hit = false;
    let mut count_bound_hit = false;
    for i in 0..150_000u64 {
        // Mostly small marks, with runs of large ones so the byte bound,
        // not the count, is what evicts for a while.
        let big = (i / 5_000) % 2 == 1;
        let n = if big {
            150 + rng.below(100)
        } else {
            rng.below(8)
        };
        let f = Fields::new()
            .text(Key::Text, &"n".repeat(n as usize))
            .unwrap_or_default();
        log.mark(
            Mark::new(Kind::Note, Src::Mark, Conf::Claim).fields(f),
            at(i),
        )
        .expect("a note fits");
        assert!(log.len() <= MAX_RECORDS);
        assert!(log.bytes() <= MAX_BYTES);
        assert_eq!(log.lost(), log.last_seq() - log.len() as u64, "at {i}");
        assert_eq!(log.oldest_seq(), log.lost() + 1);
        bytes_bound_hit |= log.len() < MAX_RECORDS && log.lost() > 0;
        count_bound_hit |= log.len() == MAX_RECORDS;
    }
    assert!(bytes_bound_hit && count_bound_hit, "both bounds evicted");
    let read = log.read(Cursor::new(0, 0), 10);
    let gap = read.gap.expect("a reader from the start is far behind");
    assert_eq!(gap.why, GapWhy::Evicted);
    assert_eq!(gap.lost, log.lost());
    assert_eq!(read.recs.first().map(|r| r.seq), Some(log.oldest_seq()));
}

/// `since=` is exclusive, seqs are dense and increasing, and pulling in any
/// page size yields every live record once.
#[test]
fn cursors_are_exclusive_monotone_and_resumable() {
    let mut log = Log::with_limits(3, Limits::new(64, 1 << 20).expect("limits"));
    for i in 0..40 {
        log.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at(i))
            .expect("fits");
    }
    for page in [1usize, 3, 7, 64] {
        let mut cur = Cursor::new(3, 0);
        let mut seen = Vec::new();
        loop {
            let read = log.read(cur, page);
            assert!(read.gap.is_none());
            if read.recs.is_empty() {
                assert_eq!(read.next, cur, "an empty read holds its place");
                break;
            }
            for r in &read.recs {
                assert!(r.seq > cur.seq, "since= is exclusive");
            }
            seen.extend(read.recs.iter().map(|r| r.seq));
            cur = read.next;
        }
        assert_eq!(seen, (1..=40).collect::<Vec<u64>>(), "page {page}");
    }
    // Repeating a read is safe: the same cursor, the same records.
    let a = log.read(Cursor::new(3, 10), 5);
    let b = log.read(Cursor::new(3, 10), 5);
    assert_eq!(a, b);
}

/// A seamless update in slice 1: the successor's log starts epoch `ep+1`
/// with `gap why=handoff`, seqs restart, and an old cursor is reset rather
/// than read as current.
#[test]
fn a_handoff_resets_old_cursors() {
    let mut old = Log::new(4);
    for i in 0..10 {
        old.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at(i))
            .expect("fits");
    }
    let cursor = old.read(Cursor::new(4, 0), 100).next;
    assert_eq!(cursor, Cursor::new(4, 10));

    let mut new = Log::after_handoff(5, Limits::SESSION, old.id_counters(), at(20));
    for i in 0..12 {
        new.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at(21 + i))
            .expect("fits");
    }
    // seq 10 exists in the new log, but the cursor is of epoch 4.
    let read = new.read(cursor, 100);
    assert_eq!(read.gap.map(|g| g.why), Some(GapWhy::Reset));
    let first = read.recs.first().expect("the new epoch from its start");
    assert_eq!((first.seq, first.k), (1, Kind::Gap));
    assert_eq!(first.f.get_word(Key::Why), Some("handoff"));
    assert_eq!(first.f.get_u(Key::Epoch), Some(5));
    assert_eq!(read.next, Cursor::new(5, 13));
    assert_eq!(new.state().ep, 5);
    // A cursor ahead of the head is foreign too.
    assert_eq!(
        new.read(Cursor::new(5, 99), 1).gap.map(|g| g.why),
        Some(GapWhy::Reset)
    );
}
