// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[allow(unsafe_code, reason = "the test build is its own trusted launcher")]
fn authority() -> aterm_cap::Authority {
    // SAFETY: a test process that handles no untrusted input is its own
    // trusted launcher, which is `root_authority`'s whole contract.
    unsafe { aterm_cap::Authority::root_authority() }
}

fn read_cap() -> ReadCap {
    authority().grant(aterm_cap::Tier::Trusted)
}

fn write_cap() -> WriteCap {
    authority().grant(aterm_cap::Tier::Trusted)
}

#[test]
fn apply_read_round_trips() {
    let mut s = Surface::new();
    s.apply(&write_cap(), Edit::AppendLine("hello".into()));
    s.apply(&write_cap(), Edit::AppendLine("world".into()));
    let got = s.read_text(
        &read_cap(),
        Range {
            start: LineId(0),
            end: LineId(2),
        },
    );
    assert_eq!(got.text, "hello\nworld\n");
    assert_eq!(got.origin, OriginTag::Source);
    assert_eq!(got.seq, Seq(2));
}

/// COW scope: a snapshot — and a writable copy taken from it — share the
/// line and log spines with the live surface until a line write, which
/// detaches exactly those two on the writer and nothing on the readers.
#[test]
fn snapshot_shares_the_line_and_log_spines_until_a_line_write() {
    let mut live = Surface::new();
    live.apply(&write_cap(), Edit::AppendLine("frozen".into()));
    let snap = live.snapshot(&read_cap());
    let fork = snap.surface.clone();
    for candidate in [&snap.surface, &fork] {
        assert!(Arc::ptr_eq(&live.lines, &candidate.lines));
        assert!(Arc::ptr_eq(&live.log, &candidate.log));
    }

    live.apply(&write_cap(), Edit::SetLine(LineId(0), "live".into()));
    assert!(!Arc::ptr_eq(&live.lines, &snap.surface.lines));
    assert!(!Arc::ptr_eq(&live.log, &snap.surface.log));
    assert!(Arc::ptr_eq(&snap.surface.lines, &fork.lines));
    assert!(Arc::ptr_eq(&snap.surface.log, &fork.log));
    assert_eq!(snap.surface.lines[0].1.as_ref(), "frozen");
    assert_eq!(live.lines[0].1.as_ref(), "live");
}

/// An edit naming an absent line still rides the one event spine, but must
/// not copy a shared line spine it cannot change (`apply` resolves the id
/// before `make_mut`).
#[test]
fn absent_line_edits_append_an_event_without_detaching_the_line_spine() {
    let mut source = Surface::new();
    source.apply(&write_cap(), Edit::AppendLine("line".into()));
    let snap = source.snapshot(&read_cap());

    for edit in [
        Edit::SetLine(LineId(99), "absent".into()),
        Edit::ClearLine(LineId(99)),
    ] {
        let mut fork = snap.surface.clone();
        fork.apply(&write_cap(), edit);
        assert!(
            Arc::ptr_eq(&fork.lines, &snap.surface.lines),
            "an absent line edit must not copy the shared line spine"
        );
        assert!(
            !Arc::ptr_eq(&fork.log, &snap.surface.log),
            "Set/Clear still append their one-spine event"
        );
        assert_eq!(fork.seq().0, snap.at.0 + 1);
    }
}

/// A write that changes nothing — the same text, or clearing an already
/// empty line — advances the log by one and leaves the line spine shared.
#[test]
fn equal_line_edits_advance_the_log_without_detaching_the_line_spine() {
    let mut source = Surface::new();
    source.apply(&write_cap(), Edit::AppendLine("same".into()));
    source.apply(&write_cap(), Edit::AppendLine(String::new()));
    let snap = source.snapshot(&read_cap());

    for edit in [
        Edit::SetLine(LineId(0), "same".into()),
        Edit::ClearLine(LineId(1)),
    ] {
        let mut fork = snap.surface.clone();
        fork.apply(&write_cap(), edit);
        assert!(Arc::ptr_eq(&fork.lines, &snap.surface.lines));
        assert!(!Arc::ptr_eq(&fork.log, &snap.surface.log));
        assert_eq!(fork.seq().0, snap.at.0 + 1);
    }
}

/// Line lookup is an ordered lower bound, not a prefix scan: reading or
/// rewriting the tail of a long surface costs at most `ceil(log2(lines + 1))`
/// id comparisons per lookup.
#[test]
fn narrow_tail_line_operations_do_logarithmic_work() {
    for lines in [1usize, 2, 3, 7, 8, 15, 16, 1023, 1024, 4096] {
        let max_per_lookup = (lines + 1).next_power_of_two().ilog2() as usize;
        let mut s = Surface::new();
        for i in 0..lines {
            let text = if i + 1 == lines {
                "tail needle"
            } else {
                "prefix"
            };
            s.apply(&write_cap(), Edit::AppendLine(text.into()));
        }
        let tail = Range {
            start: LineId((lines - 1) as u64),
            end: LineId(lines as u64),
        };

        let _ = take_line_id_comparisons();
        assert_eq!(s.read_text(&read_cap(), tail).text, "tail needle\n");
        let read_comparisons = take_line_id_comparisons();
        assert!(read_comparisons > 0, "read must reach the lower bound");
        assert!(
            read_comparisons <= 2 * max_per_lookup,
            "tail read made {read_comparisons} id comparisons for {lines} lines (bound {})",
            2 * max_per_lookup
        );

        s.apply(
            &write_cap(),
            Edit::SetLine(LineId((lines - 1) as u64), "updated".into()),
        );
        let lookup_comparisons = take_line_id_comparisons();
        assert!(lookup_comparisons > 0, "SetLine must reach ordered lookup");
        assert!(
            lookup_comparisons <= max_per_lookup,
            "tail SetLine made {lookup_comparisons} id comparisons for {lines} lines (bound {max_per_lookup})"
        );

        assert!(
            s.read_text(
                &read_cap(),
                Range {
                    start: LineId(12),
                    end: LineId(3),
                },
            )
            .text
            .is_empty(),
            "an inverted range stays empty"
        );
    }
}

#[test]
fn subscribe_pulls_new_events_then_drains() {
    let mut s = Surface::new();
    let cur = s.subscribe(&read_cap());
    s.apply(&write_cap(), Edit::AppendLine("a".into()));
    s.apply(&write_cap(), Edit::AppendLine("b".into()));
    let (upd, cur) = s.poll(cur);
    match upd {
        SubUpdate::Events(ev) => {
            assert_eq!(ev.len(), 2);
            assert_eq!(ev[0].seq, Seq(1));
            assert_eq!(ev[1].seq, Seq(2));
        }
        other => panic!("expected events, got {other:?}"),
    }
    let (upd, _cur) = s.poll(cur);
    assert_eq!(upd, SubUpdate::Events(vec![]));
}

#[test]
fn event_log_ring_does_not_double_past_max() {
    let mut log = EventLog::default();
    let mut max_cap_seen = 0usize;
    for _ in 0..(MAX_LOG_EVENTS + 4096) {
        let (_seq, _evicted) = log.append_at(Op::Resize { rows: 0, cols: 0 }, Ticks(0));
        max_cap_seen = max_cap_seen.max(log.ring.capacity());
    }
    assert_eq!(log.ring.len(), MAX_LOG_EVENTS, "ring stays bounded");
    assert!(
        log.ring.capacity() <= MAX_LOG_EVENTS,
        "no doubling past MAX_LOG_EVENTS: settled cap {} must be <= {}",
        log.ring.capacity(),
        MAX_LOG_EVENTS
    );
    assert!(
        max_cap_seen <= MAX_LOG_EVENTS,
        "never doubled at any point: peak cap {} must be <= {}",
        max_cap_seen,
        MAX_LOG_EVENTS
    );
}

/// `live_after`'s seek matches `live().filter(|e| e.seq > after)` at every
/// watermark around an evicted ring, and `oldest_live`/`newest_live` are the
/// ring's ends (`newest_live().ts` is the max over a monotone recording).
#[test]
fn live_after_seek_matches_the_filter_and_the_ends_are_the_extremes() {
    let mut log = EventLog::default();
    for i in 0..(MAX_LOG_EVENTS + 32) {
        let (_seq, _evicted) =
            log.append_at(Op::Resize { rows: 0, cols: 0 }, Ticks((i as u64) / 3));
    }
    let low = log.oldest_live().expect("non-empty").seq;
    let high = log.newest_live().expect("non-empty").seq;
    assert!(
        low.0 > 1,
        "the fixture must have evicted, or the below-low arm is vacuous"
    );
    assert_eq!(high, log.head(), "newest live seq is the spine head");
    assert_eq!(
        log.newest_live().map(|e| e.ts),
        log.live().map(|e| e.ts).max(),
        "back() is the max tick when ts is monotone nondecreasing"
    );

    let reference = |after: Seq| -> Vec<u64> {
        log.live()
            .filter(|e| e.seq > after)
            .map(|e| e.seq.0)
            .collect()
    };
    let observed = |after: Seq| -> Vec<u64> { log.live_after(after).map(|e| e.seq.0).collect() };
    for after in [
        Seq(0),
        Seq(1),
        Seq(low.0 - 1),
        low,
        Seq(low.0 + 1),
        Seq(low.0 + 977),
        Seq(high.0 - 1),
        high,
        Seq(high.0 + 1),
        Seq(u64::MAX),
    ] {
        assert_eq!(
            observed(after),
            reference(after),
            "live_after({after:?}) diverged"
        );
    }
    assert_eq!(
        observed(Seq(low.0 - 1)).len(),
        MAX_LOG_EVENTS,
        "below low-water = all live"
    );
    assert!(observed(high).is_empty(), "at the head = nothing after");
}

#[test]
fn a_forgotten_log_keeps_no_event_and_counts_on_from_its_head() {
    let mut log = EventLog::default();
    for i in 0..5 {
        let _ = log.append_at(Op::Resize { rows: 1, cols: 1 }, Ticks(i));
    }
    log.forget();
    assert_eq!(log.live().count(), 0);
    assert!(log.oldest_live().is_none() && log.newest_live().is_none());
    assert_eq!(log.total(), 5, "the spine's length is kept");
    let (seq, evicted) = log.append_at(Op::Resize { rows: 2, cols: 2 }, Ticks(9));
    assert_eq!(seq, Seq(6), "seq counts on from the head");
    assert!(evicted.is_none());
    assert_eq!(log.live_after(Seq(0)).count(), 1);
}

#[test]
fn transact_is_atomic_and_cc_guarded() {
    let mut s = Surface::new();
    s.apply(&write_cap(), Edit::AppendLine("base".into()));
    let snap = s.snapshot(&read_cap());

    let out = s.transact(
        &write_cap(),
        snap.at,
        vec![Edit::AppendLine("a".into()), Edit::AppendLine("b".into())],
    );
    assert_eq!(out, TxnOutcome::Committed(Seq(3)));
    assert_eq!(
        s.read_text(
            &read_cap(),
            Range {
                start: LineId(0),
                end: LineId(9)
            }
        )
        .text,
        "base\na\nb\n"
    );

    let out2 = s.transact(&write_cap(), snap.at, vec![Edit::AppendLine("z".into())]);
    assert_eq!(out2, TxnOutcome::Conflict);
    assert_eq!(s.seq(), Seq(3), "conflict left the surface untouched");
}

#[test]
fn op_and_event_keep_their_slot_sizes() {
    assert_eq!(std::mem::size_of::<Op>(), 16);
    assert_eq!(std::mem::size_of::<Event>(), 32);
}
