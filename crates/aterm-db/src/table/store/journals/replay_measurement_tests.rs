// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Descriptor allocation and the ownership left by a production journal read.
//! The identified-record fixture supplies no envelope authentication or timing claim.

use super::*;
use astream_broker::BrokerRecord;
use astream_wire::Offset;
use aterm_heap_count::{allocations_in, bytes_freed_in};

const RECORDS: usize = 3000;

fn snapshot(spare: usize, tail_bytes: usize) -> Arc<Vec<Linked>> {
    let records = (0..RECORDS)
        .map(|at| {
            let mut body = (at as u64 + 1).to_le_bytes().to_vec();
            body.extend_from_slice(&(at as u64).to_le_bytes());
            body.resize(if at + 1 == RECORDS { tail_bytes } else { 64 } + 8, 7);
            BrokerRecord {
                seq: Offset(at as u64),
                producer_id: 1,
                producer_seq: at as u64 + 1,
                subject: SUBJECT.into(),
                body,
                commit: None,
            }
        })
        .collect();
    let mut links = Vec::with_capacity(RECORDS + spare);
    replay::identify_onto(&mut links, records).unwrap();
    Arc::new(links)
}

fn observed(links: &Arc<Vec<Linked>>, start: usize, end: usize) -> Replay {
    let through = |count: usize| {
        count
            .checked_sub(1)
            .map_or_else(Witness::default, |at| links[at].through)
    };
    let read = Read {
        links: Arc::clone(links),
        refused: None,
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
#[ignore = "manual production-read allocation and snapshot-lifetime measurement; no timing claim"]
fn replay_descriptor_allocations_and_retained_snapshots() {
    for (case, start, end, spare, tail_bytes) in [
        ("whole", 0, RECORDS, 0, 64),
        ("unacknowledged_tail", 0, RECORDS - 1, 0, 4096),
        ("kept_one_record_tail", RECORDS - 1, RECORDS, 0, 64),
        ("empty_kept_tail", RECORDS, RECORDS, 0, 64),
        ("spare_capacity", 0, RECORDS, RECORDS, 64),
    ] {
        let links = snapshot(spare, tail_bytes);
        let (replay, allocations) = allocations_in(|| observed(&links, start, end));
        assert_eq!(replay.entries.len(), end - start);
        for (at, entry) in replay.entries.iter().enumerate() {
            assert_eq!(entry.offset, (start + at) as u64);
            assert_eq!(entry.receipt().id, links[start + at].wire.id());
        }
        let (_, descriptors_while_snapshot_held) = bytes_freed_in(|| drop(replay));

        // Losing the cache's last snapshot owner must not conceal excluded wires
        // or excess descriptor capacity behind a short replay range.
        let replay = observed(&links, start, end);
        drop(links);
        let (_, retained_after_eviction) = bytes_freed_in(|| drop(replay));

        // The production extension already clones the link vector. Its new
        // snapshot shares the existing wires; the old replay's final drop then
        // exposes exactly the old descriptor ownership left by replacement.
        let links = snapshot(spare, tail_bytes);
        let replay = observed(&links, start, end);
        let mut extended = links.as_ref().clone();
        let mut body = (RECORDS as u64 + 1).to_le_bytes().to_vec();
        body.extend_from_slice(b"extension");
        replay::identify_onto(
            &mut extended,
            vec![BrokerRecord {
                seq: Offset(RECORDS as u64),
                producer_id: 1,
                producer_seq: RECORDS as u64 + 1,
                subject: SUBJECT.into(),
                body,
                commit: None,
            }],
        )
        .unwrap();
        drop(links);
        let (_, retained_after_replacement) = bytes_freed_in(|| drop(replay));
        std::hint::black_box(&extended);
        drop(extended);

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
                "descriptors_freed_with_snapshot_held_bytes": descriptors_while_snapshot_held,
                "retained_after_snapshot_eviction_bytes": retained_after_eviction,
                "old_ownership_after_snapshot_replacement_bytes": retained_after_replacement,
                "scope": "requested heap bytes freed by final Replay drop; excludes allocator rounding, RSS and fixture setup; cache snapshots are represented by their actual Arc owners",
            })
        );
    }
}
