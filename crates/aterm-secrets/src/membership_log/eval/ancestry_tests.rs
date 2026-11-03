// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::age::Identity;
use crate::commons::binding::{Binding, Request};
use crate::membership_log::{Draft, EpochAudience, Evidence, Inserted, Log};
use crate::text::{Audience, Entry};

#[derive(Clone, Copy)]
struct Peer(u8);

impl Peer {
    fn seed(self) -> [u8; 32] {
        [self.0; 32]
    }

    fn incarnation(self) -> [u8; 16] {
        [self.0; 16]
    }

    fn identity(self) -> Identity {
        Identity::from_secret_bytes([self.0 + 64; 32])
    }

    fn request(self) -> Request {
        Request::create(
            [17; 16],
            [34; 32],
            [self.0 + 32; 16],
            self.incarnation(),
            &self.identity(),
            &self.seed(),
        )
        .unwrap()
    }

    fn actor(self) -> MemberId {
        self.request().binding().actor()
    }

    fn entrant(self) -> super::super::Entrant {
        super::super::Entrant {
            request: self.request(),
            node_key: [self.0 + 128; 32],
            name: format!("member-{}", self.0),
            label: format!("m{}", self.0),
        }
    }

    fn sign(self, draft: Draft) -> Record {
        draft.sign(&self.seed(), self.incarnation()).unwrap()
    }
}

fn founded(peers: &[Peer]) -> Log {
    Log::new(
        Record::found(
            &[99; 32],
            peers.iter().map(|peer| peer.entrant()).collect(),
            1,
        )
        .unwrap(),
    )
    .unwrap()
}

fn keep(log: &mut Log, record: Record) -> Record {
    assert_eq!(log.insert(record.clone()).unwrap(), Inserted::Held);
    record
}

fn name(log: &Log, peer: Peer, value: &str) -> Record {
    peer.sign(log.draft(
        &peer.actor(),
        7,
        Body::Name {
            name: value.to_owned(),
        },
    ))
}

macro_rules! graph {
    ($records:expr, $session:expr, $reference:expr) => {
        if $reference {
            Graph::with_oracle($records, $session, dense_past($records))
        } else {
            Graph::new($records, $session)
        }
    };
}

#[derive(Debug, PartialEq, Eq)]
struct Projection {
    membership: Membership,
    records: BTreeMap<RecordId, (bool, bool, bool, Vec<RecordId>)>,
    before: BTreeSet<(RecordId, RecordId)>,
    undo: BTreeMap<MemberId, Option<RecordId>>,
    historical_writers: BTreeMap<RecordId, Option<Vec<Binding>>>,
}

fn project(records: &[Record], peers: &[Peer], reference: bool) -> Projection {
    let ancestry = ancestry::Ancestry::new(records);
    let session = ancestry.session();
    let graph = graph!(records, &session, reference);
    let membership = graph.evaluate(&mut Memo::new());
    let mut scratch = AuthScratch::new(records.len());
    let plain = graph.authorized(&vec![false; records.len()], &mut Memo::new(), &mut scratch);
    let undo = peers
        .iter()
        .map(|peer| {
            let actor = peer.actor();
            let cutoff = graph
                .undo_cutoff(&actor, &plain, &mut scratch)
                .map(|at| *records[at].id());
            (actor, cutoff)
        })
        .collect();
    let effective = graph.effective();
    // Effective owns its shared index. Its point queries open a new session.
    drop(session);
    let mut before = BTreeSet::new();
    let flags = records
        .iter()
        .enumerate()
        .map(|(i, record)| {
            for (j, other) in records.iter().enumerate() {
                if effective.before(i, j) {
                    before.insert((*record.id(), *other.id()));
                }
            }
            let mut dependents: Vec<_> = effective
                .dependents(i)
                .iter()
                .map(|&at| *records[at].id())
                .collect();
            dependents.sort_unstable();
            (
                *record.id(),
                (
                    effective.counts(i),
                    effective.keyed(i),
                    effective.opens_wrap(i),
                    dependents,
                ),
            )
        })
        .collect();
    let historical_writers = records
        .iter()
        .enumerate()
        .filter(|(_, record)| {
            matches!(
                record.body(),
                Body::Epoch {
                    audience: EpochAudience::Commons,
                    ..
                }
            )
        })
        .map(|(i, record)| {
            let session = ancestry.session();
            let writers = graph!(records, &session, reference).historical_bindings_for_epoch(i);
            (*record.id(), writers)
        })
        .collect();
    Projection {
        membership,
        records: flags,
        before,
        undo,
        historical_writers,
    }
}

fn agrees(log: &Log, peers: &[Peer]) -> Projection {
    let records: Vec<_> = log.records().cloned().collect();
    let actual = project(&records, peers, false);
    assert_eq!(actual, project(&records, peers, true));
    actual
}

#[test]
fn signed_forks_and_topological_reorderings_match_the_dense_oracle() {
    let peers = [Peer(1), Peer(2), Peer(3)];
    let base = founded(&peers);
    let left = name(&base, peers[0], "left");
    let right = name(&base, peers[0], "right");
    assert_eq!(left.sequence(), right.sequence());
    let mut left_side = base.clone();
    keep(&mut left_side, left.clone());
    let left_child = name(&left_side, peers[1], "left-child");
    let mut right_side = base.clone();
    keep(&mut right_side, right.clone());
    let right_child = name(&right_side, peers[2], "right-child");

    let mut one = base.clone();
    for record in [&left, &left_child, &right, &right_child] {
        keep(&mut one, record.clone());
    }
    let mut two = base;
    for record in [&right, &right_child, &left, &left_child] {
        keep(&mut two, record.clone());
    }
    let joined = name(&one, peers[0], "joined");
    assert!(
        joined.parents().len() >= 2,
        "the record joins both branches"
    );
    keep(&mut one, joined.clone());
    keep(&mut two, joined.clone());
    let result = agrees(&one, &peers);
    assert_eq!(result, agrees(&two, &peers));
    assert!(!result.before.contains(&(*left.id(), *right.id())));
    assert!(!result.before.contains(&(*right.id(), *left.id())));
    assert!(result.before.contains(&(*left.id(), *left_child.id())));
    assert!(!result.before.contains(&(*right.id(), *left_child.id())));
    assert!(result.before.contains(&(*left.id(), *joined.id())));
    assert!(result.before.contains(&(*right.id(), *joined.id())));
}

#[test]
fn delayed_undo_and_voided_admissions_match_the_dense_oracle() {
    let peers = [Peer(1), Peer(2), Peer(3), Peer(4), Peer(5), Peer(6)];
    let mut log = founded(&peers[..5]);
    let cutoff = *log.network();
    let admit = peers[0].sign(log.draft(
        &peers[0].actor(),
        7,
        Body::Admit {
            entrant: peers[5].entrant(),
            evidence: Evidence::Phrase,
        },
    ));
    keep(&mut log, admit.clone());
    for peer in &peers[1..5] {
        let removal = peers[0].sign(log.draft(
            &peers[0].actor(),
            8,
            Body::Remove {
                member: peer.actor(),
                cutoff,
            },
        ));
        keep(&mut log, removal);
    }
    assert_eq!(agrees(&log, &peers).membership.members().len(), 2);
    let shares: Vec<_> = peers[1..4]
        .iter()
        .map(|peer| {
            peer.sign(log.draft(
                &peer.actor(),
                9,
                Body::Remove {
                    member: peers[0].actor(),
                    cutoff,
                },
            ))
        })
        .collect();
    for share in &shares[..2] {
        keep(&mut log, share.clone());
        assert_eq!(agrees(&log, &peers).membership.members().len(), 2);
    }
    keep(&mut log, shares[2].clone());
    let result = agrees(&log, &peers);
    assert_eq!(result.membership.members().len(), 4);
    assert!(result.membership.voided().contains(admit.id()));
    assert!(!result.membership.contains(&peers[5].actor()));
}

#[test]
fn keyed_epoch_and_historical_writer_results_match_the_dense_oracle() {
    let peers = [Peer(1), Peer(2), Peer(3)];
    let mut base = founded(&peers);
    for audience in [EpochAudience::Everyone, EpochAudience::Commons] {
        let (draft, key) = base.draft_epoch(&peers[0].actor(), 3, audience, 1).unwrap();
        let epoch = keep(&mut base, peers[0].sign(draft));
        for peer in peers {
            let wrap = base
                .draft_wrap(
                    &peers[0].actor(),
                    4,
                    *epoch.id(),
                    &key,
                    peer.identity().recipient(),
                )
                .unwrap();
            keep(&mut base, peers[0].sign(wrap));
        }
    }
    let removal = peers[0].sign(base.draft(
        &peers[0].actor(),
        5,
        Body::Remove {
            member: peers[1].actor(),
            cutoff: *base.network(),
        },
    ));
    let (draft, key) = base
        .draft_epoch(&peers[1].actor(), 6, EpochAudience::Everyone, 2)
        .unwrap();
    let epoch = keep(&mut base, peers[1].sign(draft));
    for peer in peers {
        let wrap = base
            .draft_wrap(
                &peers[1].actor(),
                7,
                *epoch.id(),
                &key,
                peer.identity().recipient(),
            )
            .unwrap();
        keep(&mut base, peers[1].sign(wrap));
    }
    let entry = Entry {
        name: "honest-dependent".to_owned(),
        value: crate::SecretBytes::from(b"kept".as_slice()),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    };
    let write = base
        .draft_put(&peers[2].actor(), 8, *epoch.id(), &key, &entry)
        .unwrap();
    let write = keep(&mut base, peers[2].sign(write));
    keep(&mut base, removal);
    let result = agrees(&base, &peers);
    let flags = &result.records[epoch.id()];
    assert!(
        !flags.0 && flags.1,
        "the cut epoch remains a keyed dependency"
    );
    assert_eq!(flags.3, vec![*write.id()]);
    assert!(result.records[write.id()].0);
    assert!(!result.historical_writers.is_empty());
}

#[test]
fn warm_effective_queries_do_not_wait_for_an_unrelated_builder() {
    use std::sync::mpsc;

    let peers = [Peer(1), Peer(2), Peer(3)];
    let mut log = founded(&peers);
    let left = name(&log, peers[0], "left");
    let right = name(&log, peers[1], "right");
    keep(&mut log, left.clone());
    let child = name(&log, peers[2], "left-child");
    keep(&mut log, right.clone());
    keep(&mut log, child.clone());
    let records: Vec<_> = log.records().cloned().collect();
    let index = |id: &RecordId| records.iter().position(|record| record.id() == id).unwrap();
    let a = index(left.id());
    let absent = index(right.id());
    let b = index(child.id());
    let ancestry = ancestry::Ancestry::new(&records);
    let session = ancestry.session();
    let effective = Graph::new(&records, &session).effective();
    drop(session);
    assert!(effective.before(a, b));
    assert!(!effective.before(absent, b));

    let (locked_tx, locked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let holder = std::thread::spawn(move || {
        ancestry.with_builder_held_for_test(|| {
            locked_tx.send(()).unwrap();
            // The owner below releases after its bounded observation, including
            // the failure path; dropping it on unwind also wakes this receiver.
            let _ = release_rx.recv();
        });
    });
    locked_rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .unwrap();
    let (answer_tx, answer_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        answer_tx
            .send((effective.before(a, b), effective.before(absent, b)))
            .unwrap();
    });
    let observed = answer_rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    reader.join().unwrap();
    assert_eq!(
        observed.ok(),
        Some((true, false)),
        "a warm positive and negative answer need no cold-builder lock"
    );
}

#[test]
fn log_clones_share_only_the_unchanged_complete_ancestry() {
    let peers = [Peer(1), Peer(2)];
    let mut log = founded(&peers);
    let initial = name(&log, peers[0], "initial");
    keep(&mut log, initial);
    let old = log.clone();
    let ancestry = std::sync::Arc::clone(log.ancestry());
    assert!(std::sync::Arc::ptr_eq(&ancestry, old.ancestry()));
    assert!(ancestry.session().before(0, 1));

    let parent = name(&log, peers[0], "parent");
    let mut future = log.clone();
    keep(&mut future, parent.clone());
    let child = name(&future, peers[0], "child");
    assert_eq!(log.insert(child).unwrap(), Inserted::Waiting);
    assert!(std::sync::Arc::ptr_eq(&ancestry, log.ancestry()));
    keep(&mut log, parent);
    assert_eq!(log.waiting(), 0);
    assert!(!std::sync::Arc::ptr_eq(&ancestry, log.ancestry()));
    assert!(std::sync::Arc::ptr_eq(&ancestry, old.ancestry()));
    assert!(ancestry.session().before(0, 1));
    let old_records: Vec<_> = old.records().cloned().collect();
    let current_records: Vec<_> = log.records().cloned().collect();
    assert_eq!(current_records.len(), old_records.len() + 2);
    assert!(ancestry.matches(&old_records));
    assert!(!ancestry.matches(&current_records));
    assert!(log.ancestry().matches(&current_records));
}

fn dense_rows(parents: &[Vec<usize>]) -> Vec<Vec<u64>> {
    let mut rows: Vec<Vec<u64>> = Vec::with_capacity(parents.len());
    for incoming in parents {
        let mut row = vec![0; parents.len().div_ceil(64)];
        for &parent in incoming {
            for (word, inherited) in row.iter_mut().zip(&rows[parent]) {
                *word |= inherited;
            }
            row[parent / 64] |= 1 << (parent % 64);
        }
        rows.push(row);
    }
    rows
}

#[test]
fn rotating_large_history_really_evicts_and_keeps_exact_rows() {
    const N: usize = 8_192;
    const BUDGET: usize = 2 * 1024 * 1024;
    let parents: Vec<Vec<usize>> = (0..N)
        .map(|i| {
            if i == 0 {
                Vec::new()
            } else if i < 3 {
                vec![i - 1]
            } else if i % 257 == 0 {
                vec![i - 2, i - 3]
            } else {
                vec![i - 2]
            }
        })
        .collect();
    let edges: usize = parents.iter().map(Vec::len).sum();
    let expected = dense_rows(&parents);
    let ancestry = ancestry::Ancestry::from_parents_for_tests(parents, BUDGET).unwrap();
    let session = ancestry.session();
    for reverse in [false, true, false] {
        for step in 0..3_072 {
            let b = if reverse {
                N - 1 - step
            } else {
                N - 3_072 + step
            };
            for a in [0, b / 2, b - 1, b, N - 1] {
                assert_eq!(
                    session.before(a, b),
                    expected[b][a / 64] & (1 << (a % 64)) != 0
                );
                assert_eq!(session.upto(a, b), a == b || session.before(a, b));
            }
            let mut copied = Bits::new(N);
            session.copy_past_into(b, &mut copied);
            assert_eq!(copied.0, expected[b]);
            let mut union = Bits::new(N);
            union.set(b);
            session.union_past_into(b, &mut union);
            let mut want = expected[b].clone();
            want[b / 64] |= 1 << (b % 64);
            assert_eq!(union.0, want);
            let mut probe = Bits::new(N);
            probe.set(b);
            assert!(!session.past_intersects(b, &probe));
            probe.set(b - 2);
            assert!(session.past_intersects(b, &probe));
        }
    }
    let cost = session.stats();
    assert!(
        cost.evictions > 0,
        "the workload must exceed the real cache"
    );
    assert!(cost.builds >= 3_072);
    assert!(cost.peak_row_bytes <= BUDGET);
    assert!(cost.peak_stack <= N);
    assert!(cost.edges <= cost.builds * edges);
    assert!(cost.before_queries >= 3 * 3_072);
    assert_eq!(cost.requests, cost.hits + cost.builds);
    assert!(cost.hits > 0, "repeated row use must hit the cache");
    assert!(cost.scratch_words <= (cost.builds + 2) * N.div_ceil(64));
    assert_eq!(
        cost.scan_words, 0,
        "evicting fills do not scan all row masks"
    );
}
