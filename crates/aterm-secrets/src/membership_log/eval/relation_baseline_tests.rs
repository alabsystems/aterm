// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::age::Identity;
use crate::commons::binding::Request;
use crate::membership_log::{Draft, Entrant, Inserted, Log, MAX_WEIGHED_RECORDS};

struct Sample {
    records: usize,
    extracted: usize,
    apart: usize,
    accepted: usize,
    undoes: usize,
    strips: usize,
    pair_slots: usize,
    relation_key_bytes: usize,
    grounded_edge_slots: usize,
    grounded_row_header_bytes: usize,
    packed_row_bytes: usize,
    packed_metadata_bytes: usize,
    packed_rows: usize,
}

impl std::fmt::Debug for Sample {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Sample")
            .field("records", &self.records)
            .field("extracted", &self.extracted)
            .field("apart", &self.apart)
            .field("accepted", &self.accepted)
            .field("undoes", &self.undoes)
            .field("strips", &self.strips)
            .field("pair_slots", &self.pair_slots)
            .field("relation_key_bytes", &self.relation_key_bytes)
            .field("grounded_edge_slots", &self.grounded_edge_slots)
            .field("grounded_row_header_bytes", &self.grounded_row_header_bytes)
            .field("packed_row_bytes", &self.packed_row_bytes)
            .field("packed_metadata_bytes", &self.packed_metadata_bytes)
            .field("packed_rows", &self.packed_rows)
            .finish()
    }
}

fn sample(log: &Log) -> Sample {
    let records = &log.order;
    let graph = Graph::new(log.history());
    let candidates = graph.candidates();
    let (accepted, apart, relations) = graph.weigh_all(
        &candidates,
        &mut Memo::new(),
        &mut AuthScratch::new(records.len()),
    );
    let undoes: BTreeSet<_> = relations.undoes_pairs().collect();
    let strips: BTreeSet<_> = relations.strips_pairs().collect();
    let defeats = undoes
        .iter()
        .filter(|&&(one, other)| {
            !undoes.contains(&(other, one))
                || strips.contains(&(one, other)) && !strips.contains(&(other, one))
        })
        .count();
    Sample {
        records: records.len(),
        extracted: candidates.len(),
        apart: apart.weighed.len(),
        accepted: accepted.len(),
        undoes: undoes.len(),
        strips: strips.len(),
        pair_slots: apart.weighed.len() * apart.weighed.len().saturating_sub(1),
        // Logical old BTreeSet key payload for comparison; node/allocator overhead is additional.
        relation_key_bytes: (undoes.len() + strips.len()) * std::mem::size_of::<(usize, usize)>(),
        // Logical old-grounder payload for comparison, not an allocation in this implementation.
        grounded_edge_slots: defeats,
        grounded_row_header_bytes: apart.weighed.len() * std::mem::size_of::<Vec<usize>>(),
        packed_row_bytes: relations.payload_bytes(),
        packed_metadata_bytes: relations.metadata_bytes(),
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
    eprintln!("RELATION_BASELINE signed_bound {measured:?}");
}

#[test]
fn real_weigh_corpus_reports_extraction_and_relation_density() {
    for (name, log) in crate::membership_log::tests::weigh::relation_baseline_corpus() {
        let measured = sample(&log);
        assert!(measured.accepted <= measured.apart);
        assert!(measured.apart <= measured.extracted);
        assert!(measured.undoes <= measured.pair_slots);
        assert!(measured.strips <= measured.pair_slots);
        assert_eq!(measured.records, log.held_count());
        assert!(measured.grounded_edge_slots <= measured.undoes);
        assert!(measured.packed_row_bytes <= 1024 * 1024);
        assert!(measured.packed_rows <= 2 * measured.apart);
        if measured.undoes + measured.strips == 0 {
            assert_eq!(measured.packed_row_bytes, 0);
            assert_eq!(measured.packed_rows, 0);
        }
        assert!(measured.packed_metadata_bytes >= measured.apart * std::mem::size_of::<usize>());
        eprintln!("RELATION_BASELINE {name} {measured:?}");
    }
}
