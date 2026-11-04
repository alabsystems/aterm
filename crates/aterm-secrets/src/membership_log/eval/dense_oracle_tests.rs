// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The evaluator reading the log's reachability index gives what it gives reading
//! [`dense_past`], the all-pairs oracle, on signed forks, reorderings, quorums and keyed epochs.

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
    ($history:expr, $reference:expr) => {
        if $reference {
            Graph::with_oracle($history, dense_past($history.records))
        } else {
            Graph::new($history)
        }
    };
}

#[derive(Debug, PartialEq, Eq)]
struct Projection {
    weighing: Weighing,
    membership: Membership,
    records: BTreeMap<RecordId, (bool, bool, bool, Vec<RecordId>)>,
    before: BTreeSet<(RecordId, RecordId)>,
    undo: BTreeMap<MemberId, Option<RecordId>>,
    historical_writers: BTreeMap<RecordId, Option<Vec<Binding>>>,
}

fn project(records: &[Record], peers: &[Peer], reference: bool) -> Projection {
    let indexed = Indexed::of(records);
    let history = indexed.history(records);
    let graph = graph!(history, reference);
    let mut scratch = graph.scratch();
    let weighing = graph.weighing(&mut Memo::new(), &mut scratch);
    let membership = graph.evaluate(&weighing, &mut scratch);
    let plain = graph.authorized_records(&graph.no_void(), &weighing.apart.plain, &mut scratch);
    let undo = peers
        .iter()
        .map(|peer| {
            let actor = peer.actor();
            let cutoff = graph
                .undo_cutoff(&actor, &plain, &weighing, &mut scratch)
                .map(|at| *records[at].id());
            (actor, cutoff)
        })
        .collect();
    let effective = graph.effective(&weighing);
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
            let writers = graph!(history, reference).historical_bindings_for_epoch(&weighing, i);
            (*record.id(), writers)
        })
        .collect();
    Projection {
        weighing,
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
