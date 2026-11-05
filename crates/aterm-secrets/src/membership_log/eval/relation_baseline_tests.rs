// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::age::Identity;
use crate::commons::binding::Request;
use crate::membership_log::{Draft, Entrant, Inserted, Log, MAX_WEIGHED_RECORDS};

/// What a weighing holds of its relations.
struct Sample {
    extracted: usize,
    apart: usize,
    undoes: usize,
    strips: usize,
    packed_row_bytes: usize,
    packed_rows: usize,
}

fn sample(log: &Log) -> Sample {
    let graph = Graph::new(log.history());
    let candidates = graph.candidates();
    let (_, apart, relations) =
        graph.weigh_all(&candidates, &mut Memo::new(), &mut graph.scratch());
    Sample {
        extracted: candidates.len(),
        apart: apart.weighed.len(),
        undoes: relations.undoes_pairs().count(),
        strips: relations.strips_pairs().count(),
        packed_row_bytes: relations.payload_bytes(),
        packed_rows: relations.allocated_rows(),
    }
}

fn founder(tag: u8) -> (MemberId, Entrant) {
    let identity = Identity::from_secret_bytes([tag + 64; 32]);
    let request = Request::create(
        [17; 16],
        [34; 32],
        [tag + 32; 16],
        [tag; 16],
        &identity,
        &[tag; 32],
    )
    .unwrap();
    (
        request.binding().actor(),
        Entrant {
            request,
            node_key: [tag + 128; 32],
            name: format!("candidate-{tag}"),
            label: format!("m{tag}"),
        },
    )
}

/// A log at the weighed record bound, each quorum candidate a signed group, weighs every one of
/// them. THE GAP: its pins on the packed relations read a shape that weighs one group apart and
/// has no relation, so they say only that nothing is allocated for none. How wide a row packs and
/// what the budget refuses is `relations::tests`' (the maximum layout's exact size, its rows
/// allocated where a bit landed): no log here counts its rows against the budget, and nor does
/// the §4.2 flood, whose 256 candidates have no relation either.
#[test]
fn signed_candidate_groups_reach_the_record_bound() {
    const K: usize = 2_047;
    assert_eq!(K, (MAX_WEIGHED_RECORDS - 1) / 2);
    let (a, entry_a) = founder(1);
    let (b, entry_b) = founder(2);
    let (c, entry_c) = founder(3);
    let actors = [a, b, c];
    let mut log =
        Log::new(Record::found(&[99; 32], vec![entry_a, entry_b, entry_c], 1).unwrap()).unwrap();
    let mut latest = [None; 3];
    let mut sequence = [0; 3];
    for round in 0..K {
        let target = round % actors.len();
        let cutoff = latest[target].unwrap_or(*log.network());
        for by in (0..actors.len()).filter(|by| *by != target) {
            let mut parents: Vec<_> = latest[by].into_iter().chain([cutoff]).collect();
            parents.sort_unstable();
            parents.dedup();
            sequence[by] += 1;
            let tag = by as u8 + 1;
            let record = Draft {
                network: *log.network(),
                sequence: sequence[by],
                previous: latest[by],
                parents,
                authored_ms: round as u64 + 2,
                body: Body::Remove {
                    member: actors[target],
                    cutoff,
                },
            }
            .sign(&[tag; 32], [tag; 16])
            .unwrap();
            latest[by] = Some(*record.id());
            assert_eq!(log.insert(record).unwrap(), Inserted::Held);
        }
    }
    assert_eq!(log.held_count(), 2 * K + 1);
    assert_eq!(log.waiting(), 0);
    let measured = sample(&log);
    assert_eq!(measured.extracted, K);
    assert!(measured.packed_row_bytes <= 1024 * 1024);
    assert!(measured.packed_rows <= 2 * measured.apart);
    if measured.undoes + measured.strips == 0 {
        assert_eq!(measured.packed_row_bytes, 0);
        assert_eq!(measured.packed_rows, 0);
    }
}
