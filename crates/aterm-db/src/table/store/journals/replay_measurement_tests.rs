// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Descriptor allocation and the ownership left by a production journal read.
//! The identified-record fixture supplies no envelope authentication or timing claim.

use super::*;
use crate::table::store::replay::Linked;
use astream_broker::BrokerRecord;
use astream_wire::Offset;
use aterm_heap_count::{allocations_in, bytes_freed_in};

const RECORDS: usize = 3000;

fn snapshot(spare: usize, tail_bytes: usize) -> Arc<Links> {
    sized_snapshot(RECORDS, spare, 64, tail_bytes)
}

fn sized_snapshot(count: usize, spare: usize, body_bytes: usize, tail_bytes: usize) -> Arc<Links> {
    let mut records = Vec::with_capacity(count + spare);
    records.extend((0..count).map(|at| {
        let mut body = (at as u64 + 1).to_le_bytes().to_vec();
        body.extend_from_slice(&(at as u64).to_le_bytes());
        body.resize(
            if at + 1 == count {
                tail_bytes
            } else {
                body_bytes
            } + 8,
            7,
        );
        BrokerRecord {
            seq: Offset(at as u64),
            producer_id: 1,
            producer_seq: at as u64 + 1,
            subject: SUBJECT.into(),
            body,
            commit: None,
        }
    }));
    let (links, refused) = Links::identify(None, records, None);
    assert!(refused.is_none());
    Arc::new(links)
}

fn consumption_bounded<const READERS: usize>(links: Arc<Links>, start: usize, end: usize) {
    let wire_bytes: Vec<_> = (start..end)
        .map(|at| links.get(at).unwrap().wire.allocation_bytes())
        .collect();
    let mut entries =
        std::array::from_fn::<_, READERS, _>(|_| observed(&links, start, end).entries.into_iter());
    drop(links);
    let mut freed = Vec::with_capacity(end - start);
    for _ in start..end {
        let (_, bytes) = bytes_freed_in(|| {
            for entries in &mut entries {
                drop(entries.next().unwrap());
            }
        });
        freed.push(bytes);
    }
    let (_, final_bytes) = bytes_freed_in(|| drop(entries));
    let mut retained = freed.iter().sum::<usize>() + final_bytes;
    let mut remaining_wires = wire_bytes.iter().sum::<usize>();
    let descriptors = READERS * (end - start) * size_of::<replay::Entry>();
    assert!(retained <= descriptors + remaining_wires);
    for (freed, wire) in freed.into_iter().zip(wire_bytes) {
        retained -= freed;
        remaining_wires -= wire;
        assert!(retained <= descriptors + remaining_wires);
    }
    assert_eq!(retained, final_bytes);
}

fn observed(links: &Arc<Links>, start: usize, end: usize) -> Replay {
    let through = |count: usize| {
        count
            .checked_sub(1)
            .map_or_else(Witness::default, |at| links.witness(at).unwrap())
    };
    let read = Read {
        links: Arc::clone(links),
        refused: None,
        seen: None,
        prefix: None,
    };
    if start == 0 {
        read.witnessed(through(end)).unwrap()
    } else {
        read.witnessed_after(through(end), through(start))
            .unwrap()
            .unwrap()
    }
}

#[test]
fn replay_descriptor_allocations_and_retained_snapshots() {
    for (case, start, end, spare, tail_bytes) in [
        ("whole", 0, RECORDS, 0, 64),
        ("unacknowledged_tail", 0, RECORDS - 1, 0, 4096),
        ("kept_one_record_tail", RECORDS - 1, RECORDS, 0, 64),
        ("empty_kept_tail", RECORDS, RECORDS, 0, 64),
        ("spare_capacity", 0, RECORDS, RECORDS, 64),
    ] {
        // Construct outside the counter: only retained snapshot ownership is freed here.
        let snapshot_to_measure = snapshot(spare, tail_bytes);
        let (_, cached_snapshot_retained_bytes) = bytes_freed_in(|| drop(snapshot_to_measure));
        let links = snapshot(spare, tail_bytes);
        let (replay, allocations) = allocations_in(|| observed(&links, start, end));
        assert_eq!(replay.entries.len(), end - start);
        for (at, entry) in replay.entries.iter().enumerate() {
            assert_eq!(entry.offset, (start + at) as u64);
            assert_eq!(entry.receipt().id, links.get(start + at).unwrap().wire.id());
        }
        let selected_wires: usize = (start..end)
            .map(|at| links.get(at).unwrap().wire.allocation_bytes())
            .sum();
        let baseline_descriptors = (end - start) * size_of::<replay::Entry>();
        let (_, descriptors_while_snapshot_held) = bytes_freed_in(|| drop(replay));
        assert!(
            allocations <= 1,
            "no additional allocations over eager replay"
        );
        assert!(descriptors_while_snapshot_held <= baseline_descriptors);

        // Losing the cache's last snapshot owner must not conceal excluded wires
        // or excess descriptor capacity behind a short replay range.
        let replay = observed(&links, start, end);
        drop(links);
        let (_, retained_after_eviction) = bytes_freed_in(|| drop(replay));
        // NEGATIVE CONTROL: holding the complete old journal snapshot for a
        // one-record tail retains the excluded prefix and violates this bound.
        assert!(retained_after_eviction <= baseline_descriptors + selected_wires);

        // At every consumption boundary, an evicted replay owns no more than
        // the eager vector's fixed allocation plus the unconsumed wire bytes.
        consumption_bounded::<1>(snapshot(spare, tail_bytes), start, end);
        consumption_bounded::<2>(snapshot(spare, tail_bytes), start, end);

        // A production extension shares the existing wires in its new
        // snapshot; the old replay's final drop then
        // exposes exactly the old descriptor ownership left by replacement.
        let links = snapshot(spare, tail_bytes);
        let replay = observed(&links, start, end);
        let mut body = (RECORDS as u64 + 1).to_le_bytes().to_vec();
        body.extend_from_slice(b"extension");
        let (extended, refused) = Links::identify(
            Some(&links),
            vec![BrokerRecord {
                seq: Offset(RECORDS as u64),
                producer_id: 1,
                producer_seq: RECORDS as u64 + 1,
                subject: SUBJECT.into(),
                body,
                commit: None,
            }],
            None,
        );
        assert!(refused.is_none());
        drop(links);
        let (_, retained_after_replacement) = bytes_freed_in(|| drop(replay));
        assert!(retained_after_replacement <= baseline_descriptors);
        std::hint::black_box(&extended);
        drop(extended);

        // The eager vector retains its descriptor allocation until its iterator
        // drops, but releases each consumed wire. A shared cursor must stay
        // within that bound even after losing the cache halfway through replay.
        let links = snapshot(spare, tail_bytes);
        let remaining_wire_bytes = if end > start {
            links.get(end - 1).unwrap().wire.allocation_bytes()
        } else {
            0
        };
        let replay = observed(&links, start, end);
        let mut entries = replay.entries.into_iter();
        for _ in 0..(end - start) / 2 {
            drop(entries.next());
        }
        drop(links);
        while entries.len() > 1 {
            drop(entries.next());
        }
        let (_, consumed_replay_retained_bytes) = bytes_freed_in(|| drop(entries));
        // NEGATIVE CONTROL: a flat shared slice retains all consumed wires
        // after eviction and exceeds this bound for a nonempty large replay.
        assert!(consumed_replay_retained_bytes <= baseline_descriptors + remaining_wire_bytes);

        println!(
            "{}",
            aterm_json::json!({
                "measurement": "journal_replay_descriptors",
                "case": case,
                "fixture_records": RECORDS,
                "replayed_records": end - start,
                "linked_size": size_of::<Linked>(),
                "entry_size": size_of::<replay::Entry>(),
                "allocations_per_read": allocations,
                "cached_snapshot_retained_bytes": cached_snapshot_retained_bytes,
                "descriptors_freed_with_snapshot_held_bytes": descriptors_while_snapshot_held,
                "retained_after_snapshot_eviction_bytes": retained_after_eviction,
                "old_ownership_after_snapshot_replacement_bytes": retained_after_replacement,
                "consumed_replay_retained_bytes": consumed_replay_retained_bytes,
                "scope": "requested heap bytes freed by final replay or snapshot drop; excludes allocator rounding, RSS and fixture setup; cache snapshots are represented by their actual Arc owners",
            })
        );
    }
    // Small histories and large records exercise the owning representation:
    // sharing is allowed only when its retained memory fits the same bound.
    for (count, body_bytes) in [(0, 64), (1, 64), (2, 64), (12, 64), (100, 64), (100, 8192)] {
        consumption_bounded::<1>(sized_snapshot(count, 0, body_bytes, body_bytes), 0, count);
        consumption_bounded::<2>(sized_snapshot(count, 0, body_bytes, body_bytes), 0, count);
    }
}
