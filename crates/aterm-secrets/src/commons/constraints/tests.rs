// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    age::Identity,
    commons::{
        journal::{Journal, State, Trust},
        node::{Edit, Node},
    },
};
use ring::signature::{Ed25519KeyPair, KeyPair};

const NETWORK: [u8; 16] = [7; 16];
const SEED: [u8; 32] = [41; 32];
const TABLE: Digest = [51; 32];
fn authority() -> [u8; 32] {
    Ed25519KeyPair::from_seed_unchecked(&SEED)
        .unwrap()
        .public_key()
        .as_ref()
        .try_into()
        .unwrap()
}
fn actor(n: u8) -> Actor {
    [n; 48]
}
fn point(n: u8) -> Digest {
    [n; 32]
}
fn epoch(generation: u64, key: u8) -> Fact {
    Fact::Epoch {
        generation,
        commitment: key_commitment(NETWORK, &[key; 32]),
    }
}
fn node(parent: Option<Digest>, identities: &[&Identity], payload: &[u8], facts: &[Fact]) -> Node {
    let hosts = ["m1".to_owned()];
    let edit = match parent {
        Some(parent) => Edit::Update {
            parent,
            hosts: &hosts,
            payload,
        },
        None => Edit::Create {
            hosts: &hosts,
            payload,
        },
    };
    Node::seal_restricted(
        &SEED,
        NETWORK,
        edit,
        &identities.iter().map(|i| i.recipient()).collect::<Vec<_>>(),
        0,
        facts,
    )
    .unwrap()
}
fn recover(heads: &[Digest], selected: Digest) -> Node {
    Node::seal(
        &SEED,
        NETWORK,
        Edit::Recover { heads, selected },
        &[],
        u64::MAX,
    )
    .unwrap()
}
fn open(dir: &std::path::Path, genesis: &Node) -> Journal {
    Journal::open(
        dir,
        Trust {
            network: NETWORK,
            authority: genesis.authority(),
            genesis: genesis.id(),
        },
    )
    .unwrap()
}
fn table_index() -> Index {
    let mut index = Index::new(20);
    for n in 1..=3 {
        let previous = (n > 1).then(|| point(n - 1));
        index
            .insert(Header {
                id: point(n),
                actor: actor(11),
                sequence: u64::from(n),
                previous,
                parents: previous.into_iter().collect(),
            })
            .unwrap();
    }
    index
}

#[test]
fn a_pending_frontier_does_not_withhold_its_independent_earlier_prefix() {
    let identity = Identity::from_secret_bytes([13; 32]);
    let genesis = node(
        None,
        &[&identity],
        b"prefix policy",
        &[
            Fact::Retire { actor: actor(11) },
            Fact::Cutoff {
                table: TABLE,
                actor: actor(11),
                through: Some(Frontier {
                    sequence: 2,
                    digest: point(2),
                }),
            },
        ],
    );
    let constraints =
        Constraints::collect(NETWORK, authority(), genesis.id(), [&genesis].into_iter()).unwrap();
    let mut index = Index::new(3);
    index
        .insert(Header {
            id: point(1),
            actor: actor(11),
            sequence: 1,
            previous: None,
            parents: vec![],
        })
        .unwrap();
    index
        .insert(Header {
            id: point(2),
            actor: actor(11),
            sequence: 2,
            previous: Some(point(1)),
            parents: vec![point(1), point(99)],
        })
        .unwrap();
    index
        .insert(Header {
            id: point(3),
            actor: actor(12),
            sequence: 1,
            previous: None,
            parents: vec![point(1)],
        })
        .unwrap();
    let catalog = constraints.catalog(TABLE, &index);
    assert!(catalog.permits(&point(1)));
    assert!(!catalog.permits(&point(2)));
    assert!(catalog.permits(&point(3)));
}

#[test]
fn newly_admitted_member_enforces_history_it_cannot_decrypt() {
    let old = Identity::from_secret_bytes([13; 32]);
    let new = Identity::from_secret_bytes([17; 32]);
    let genesis = node(None, &[&old], b"old roster", &[epoch(1, 31)]);
    let retirement = node(
        Some(genesis.id()),
        &[&old],
        b"retired",
        &[
            Fact::Retire { actor: actor(11) },
            epoch(2, 37),
            Fact::Cutoff {
                table: TABLE,
                actor: actor(11),
                through: Some(Frontier {
                    sequence: 2,
                    digest: point(2),
                }),
            },
        ],
    );
    let admission = node(Some(retirement.id()), &[&old, &new], b"new member", &[]);
    assert!(genesis.open(&new).is_err());
    assert!(retirement.open(&new).is_err());
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut journal = open(dir.path(), &genesis);
    journal.ingest(admission.bytes()).unwrap();
    assert!(journal.constraints().is_err());
    journal.ingest(retirement.bytes()).unwrap();
    journal.ingest(genesis.bytes()).unwrap();
    let current = journal.current(&new).unwrap().unwrap();
    assert_eq!(current.payload.as_slice(), b"new member");
    let constraints = &current.constraints;
    assert!(!constraints.has_legacy());
    assert!(constraints.check_actor(&actor(11)).is_err());
    assert!(constraints.check_actor(&actor(12)).is_ok());
    assert!(constraints.check_key(1, &[31; 32]).is_err());
    assert!(constraints.check_key(2, &[37; 32]).is_ok());
    let index = table_index();
    let catalog = constraints.catalog(TABLE, &index);
    assert!(catalog.permits(&point(1)));
    assert!(catalog.permits(&point(2)));
    assert!(!catalog.permits(&point(3)));
    assert!(!constraints.catalog(point(52), &index).permits(&point(1)));
    drop(journal);
    let journal = open(dir.path(), &genesis);
    let restarted = journal.constraints().unwrap();
    assert_eq!(
        restarted.facts().collect::<Vec<_>>(),
        constraints.facts().collect::<Vec<_>>()
    );
}

#[test]
fn selected_old_payload_cannot_discard_a_nonselected_revocation_or_key_epoch() {
    let old = Identity::from_secret_bytes([13; 32]);
    let new = Identity::from_secret_bytes([17; 32]);
    let genesis = node(None, &[&old], b"key 31", &[epoch(1, 31)]);
    let left = node(
        Some(genesis.id()),
        &[&old],
        b"key 37",
        &[Fact::Retire { actor: actor(11) }, epoch(2, 37)],
    );
    let right = node(Some(genesis.id()), &[&old, &new], b"key 31", &[]);
    let recovery = recover(&[left.id(), right.id()], right.id());
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut journal = open(dir.path(), &genesis);
    for event in [&recovery, &right, &genesis] {
        journal.ingest(event.bytes()).unwrap();
    }
    assert_eq!(journal.state(), State::Pending);
    journal.ingest(left.bytes()).unwrap();
    let current = journal.current(&new).unwrap().unwrap();
    assert_eq!(current.payload.as_slice(), b"key 31");
    assert!(current.constraints.check_actor(&actor(11)).is_err());
    assert!(current.constraints.check_key(1, &[31; 32]).is_err());
    assert!(left.open(&new).is_err());
    let fresh = node(
        Some(recovery.id()),
        &[&old, &new],
        b"key 43",
        &[epoch(3, 43)],
    );
    journal.ingest(fresh.bytes()).unwrap();
    let constraints = journal.constraints().unwrap();
    assert!(constraints.check_key(3, &[43; 32]).is_ok());
    assert!(constraints.check_actor(&actor(11)).is_err());
}

#[test]
fn equal_epoch_conflicts_and_old_key_reuse_require_a_fresh_higher_epoch() {
    let identity = Identity::from_secret_bytes([13; 32]);
    let genesis = node(None, &[&identity], b"first", &[epoch(1, 31)]);
    let left = node(Some(genesis.id()), &[&identity], b"left", &[epoch(2, 37)]);
    let right = node(Some(genesis.id()), &[&identity], b"right", &[epoch(2, 43)]);
    let recovery = recover(&[left.id(), right.id()], left.id());
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut journal = open(dir.path(), &genesis);
    for event in [&genesis, &left, &right, &recovery] {
        journal.ingest(event.bytes()).unwrap();
    }
    assert_eq!(
        journal.constraints().unwrap().epoch(),
        Epoch::Conflict { generation: 2 }
    );
    assert!(
        journal
            .constraints()
            .unwrap()
            .check_key(2, &[37; 32])
            .is_err()
    );
    let reused = node(
        Some(recovery.id()),
        &[&identity],
        b"reused",
        &[epoch(3, 31)],
    );
    journal.ingest(reused.bytes()).unwrap();
    assert_eq!(
        journal.constraints().unwrap().epoch(),
        Epoch::Reused { generation: 3 }
    );
    let fresh = node(Some(reused.id()), &[&identity], b"fresh", &[epoch(4, 47)]);
    journal.ingest(fresh.bytes()).unwrap();
    assert!(
        journal
            .constraints()
            .unwrap()
            .check_key(4, &[47; 32])
            .is_ok()
    );
    assert!(
        journal
            .constraints()
            .unwrap()
            .check_key(5, &[47; 32])
            .is_err()
    );
    assert!(
        journal
            .constraints()
            .unwrap()
            .check_key(4, &[43; 32])
            .is_err()
    );
}

#[test]
fn cutoffs_intersect_and_do_not_follow_arbitrary_causal_edges() {
    let identity = Identity::from_secret_bytes([13; 32]);
    let genesis = node(
        None,
        &[&identity],
        b"start",
        &[
            Fact::Cutoff {
                table: TABLE,
                actor: actor(11),
                through: Some(Frontier {
                    sequence: 2,
                    digest: point(2),
                }),
            },
            Fact::Cutoff {
                table: TABLE,
                actor: actor(11),
                through: Some(Frontier {
                    sequence: 3,
                    digest: point(3),
                }),
            },
        ],
    );
    let constraints = Constraints::collect(
        NETWORK,
        authority(),
        genesis.id(),
        std::iter::once(&genesis),
    )
    .unwrap();
    let mut index = table_index();
    assert!(constraints.catalog(TABLE, &index).permits(&point(2)));
    assert!(!constraints.catalog(TABLE, &index).permits(&point(3)));
    let stop = node(
        Some(genesis.id()),
        &[&identity],
        b"stop",
        &[Fact::Cutoff {
            table: TABLE,
            actor: actor(11),
            through: None,
        }],
    );
    let stopped = Constraints::collect(
        NETWORK,
        authority(),
        stop.id(),
        [&genesis, &stop].into_iter(),
    )
    .unwrap();
    assert!(!stopped.catalog(TABLE, &index).permits(&point(1)));
    // Forge a causal side-edge to another same-actor branch. The generic DAG
    // observes it, but the writer's previous chain does not contain it.
    let fork = Header {
        id: point(12),
        actor: actor(11),
        sequence: 2,
        previous: Some(point(1)),
        parents: vec![point(1)],
    };
    index.insert(fork.clone()).unwrap();
    let tip = Header {
        id: point(13),
        actor: actor(11),
        sequence: 4,
        previous: Some(point(3)),
        parents: vec![point(3), point(12)],
    };
    index.insert(tip).unwrap();
    assert!(index.observes(&point(13), &point(12)));
    assert!(
        !allowed_prefix(
            &index,
            &index.inventory().copied().collect(),
            actor(11),
            &[Some(Frontier {
                sequence: 4,
                digest: point(13)
            })]
        )
        .unwrap()
        .contains(&fork.id)
    );
    index
        .insert(Header {
            id: point(21),
            actor: actor(22),
            sequence: 4,
            previous: Some(point(3)),
            parents: vec![point(3)],
        })
        .unwrap();
    index
        .insert(Header {
            id: point(22),
            actor: actor(11),
            sequence: 5,
            previous: Some(point(21)),
            parents: vec![point(21)],
        })
        .unwrap();
    assert!(
        allowed_prefix(
            &index,
            &index.inventory().copied().collect(),
            actor(11),
            &[Some(Frontier {
                sequence: 5,
                digest: point(22)
            })]
        )
        .is_none(),
        "an interior previous link changed actor"
    );
    index
        .insert(Header {
            id: point(23),
            actor: actor(11),
            sequence: 6,
            previous: Some(point(3)),
            parents: vec![point(3)],
        })
        .unwrap();
    assert!(
        allowed_prefix(
            &index,
            &index.inventory().copied().collect(),
            actor(11),
            &[Some(Frontier {
                sequence: 6,
                digest: point(23)
            })]
        )
        .is_none(),
        "an interior previous link skipped a sequence"
    );
    assert!(
        !constraints.catalog(TABLE, &index).permits(&point(1)),
        "quarantine also withholds the actor"
    );
}

fn signed(mut wire: Vec<u8>, legacy: bool) -> Vec<u8> {
    let mut message = if legacy {
        b"aterm/commons/control-node/1\0".to_vec()
    } else if wire.starts_with(b"ACN2") {
        b"aterm/commons/control-node/2\0".to_vec()
    } else {
        b"aterm/commons/control-node/3\0".to_vec()
    };
    message.extend_from_slice(&wire);
    wire.extend_from_slice(
        Ed25519KeyPair::from_seed_unchecked(&SEED)
            .unwrap()
            .sign(&message)
            .as_ref(),
    );
    wire
}

#[test]
fn legacy_history_stays_readable_but_cannot_supply_complete_policy_evidence() {
    let identity = Identity::from_secret_bytes([13; 32]);
    let current = node(None, &[&identity], b"legacy private", &[]);
    let mut wire = current.bytes()[..current.bytes().len() - 64].to_vec();
    wire[..4].copy_from_slice(b"ACN1");
    let count = 4 + 16 + 32 + 2 + 32 + 8;
    wire.drain(count..count + 2);
    let legacy = Node::verify(&signed(wire, true), NETWORK, current.authority()).unwrap();
    assert!(legacy.legacy());
    assert_eq!(
        legacy.open(&identity).unwrap().as_slice(),
        b"legacy private"
    );
    let fresh = node(Some(legacy.id()), &[&identity], b"fresh", &[epoch(1, 31)]);
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut journal = open(dir.path(), &legacy);
    journal.ingest(legacy.bytes()).unwrap();
    journal.ingest(fresh.bytes()).unwrap();
    let current = journal.current(&identity).unwrap().unwrap();
    assert!(current.constraints.has_legacy());
    assert_eq!(current.constraints.epoch(), Epoch::Legacy);
    assert!(current.constraints.check_actor(&actor(11)).is_err());
    assert!(current.constraints.check_key(1, &[31; 32]).is_err());
    assert!(
        !current
            .constraints
            .catalog(TABLE, &table_index())
            .permits(&point(1))
    );
}

#[test]
fn numeric_epoch_cannot_hide_an_unobserved_retirement_or_republish_an_old_key() {
    let identity = Identity::from_secret_bytes([13; 32]);
    let genesis = node(None, &[&identity], b"start", &[epoch(1, 31)]);
    let retired = node(
        Some(genesis.id()),
        &[&identity],
        b"retire",
        &[Fact::Retire { actor: actor(11) }],
    );
    let concurrent = node(
        Some(genesis.id()),
        &[&identity],
        b"higher number",
        &[epoch(100, 37)],
    );
    let recovery = recover(&[retired.id(), concurrent.id()], concurrent.id());
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut journal = open(dir.path(), &genesis);
    for event in [&genesis, &retired, &concurrent, &recovery] {
        journal.ingest(event.bytes()).unwrap();
    }
    assert_eq!(
        journal.constraints().unwrap().epoch(),
        Epoch::MissingRetirements { generation: 100 }
    );
    let republished = node(
        Some(recovery.id()),
        &[&identity],
        b"same key",
        &[epoch(100, 37)],
    );
    journal.ingest(republished.bytes()).unwrap();
    assert_eq!(
        journal.constraints().unwrap().epoch(),
        Epoch::MissingRetirements { generation: 100 }
    );
    let rotated = node(
        Some(republished.id()),
        &[&identity],
        b"new key",
        &[epoch(101, 43)],
    );
    journal.ingest(rotated.bytes()).unwrap();
    assert!(
        journal
            .constraints()
            .unwrap()
            .check_key(101, &[43; 32])
            .is_ok()
    );
    let repeated = node(
        Some(rotated.id()),
        &[&identity],
        b"same retirement",
        &[Fact::Retire { actor: actor(11) }],
    );
    journal.ingest(repeated.bytes()).unwrap();
    assert!(
        journal
            .constraints()
            .unwrap()
            .check_key(101, &[43; 32])
            .is_ok(),
        "repeated facts are idempotent"
    );
}

#[test]
fn rotation_covers_retirements_across_bitmap_words() {
    let identity = Identity::from_secret_bytes([13; 32]);
    let mut facts: Vec<_> = (1..=63).map(|n| Fact::Retire { actor: actor(n) }).collect();
    facts.push(epoch(1, 31));
    let genesis = node(None, &[&identity], b"63 retirements", &facts);
    let extra = node(
        Some(genesis.id()),
        &[&identity],
        b"two more",
        &[
            Fact::Retire { actor: actor(64) },
            Fact::Retire { actor: actor(65) },
        ],
    );
    let rotated = node(Some(extra.id()), &[&identity], b"rotated", &[epoch(2, 37)]);
    let before = Constraints::collect(
        NETWORK,
        authority(),
        extra.id(),
        [&genesis, &extra].into_iter(),
    )
    .unwrap();
    assert_eq!(before.epoch(), Epoch::MissingRetirements { generation: 1 });
    let after = Constraints::collect(
        NETWORK,
        authority(),
        rotated.id(),
        [&genesis, &extra, &rotated].into_iter(),
    )
    .unwrap();
    assert!(after.check_key(2, &[37; 32]).is_ok());
}

#[test]
fn computation_limit_preserves_history_and_withholds_current_policy() {
    let identity = Identity::from_secret_bytes([13; 32]);
    let genesis = node(None, &[&identity], b"start", &[]);
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut journal = open(dir.path(), &genesis);
    journal.ingest(genesis.bytes()).unwrap();
    let mut parent = genesis.id();
    for batch in 0..64 {
        let facts: Vec<_> = (1..=64)
            .map(|within| {
                let mut actor = [1; 48];
                actor[..4].copy_from_slice(&(batch * 64u32 + within).to_be_bytes());
                Fact::Retire { actor }
            })
            .collect();
        let event = node(Some(parent), &[&identity], b"history", &facts);
        parent = event.id();
        journal.ingest(event.bytes()).unwrap();
    }
    assert!(journal.constraints().is_ok());
    let mut actor = [1; 48];
    actor[..4].copy_from_slice(&4097u32.to_be_bytes());
    let last = node(
        Some(parent),
        &[&identity],
        b"preserved",
        &[Fact::Retire { actor }],
    );
    journal.ingest(last.bytes()).unwrap();
    assert!(matches!(journal.state(), State::Ready { .. }));
    assert!(journal.constraints().is_err());
    assert!(journal.current(&identity).is_err());
    assert_eq!(journal.inventory().count(), 66);
    drop(journal);
    let journal = open(dir.path(), &genesis);
    assert_eq!(journal.inventory().count(), 66);
    assert!(journal.constraints().is_err());
}

#[test]
fn invalid_duplicate_over_limit_and_recovery_restrictions_are_refused() {
    let identity = Identity::from_secret_bytes([13; 32]);
    let hosts = ["m1".to_owned()];
    let edit = || Edit::Create {
        hosts: &hosts,
        payload: b"test",
    };
    for facts in [
        vec![Fact::Retire { actor: [0; 48] }],
        vec![epoch(0, 31)],
        vec![epoch(1, 31); 2],
        vec![epoch(1, 31); MAX_FACTS + 1],
        vec![Fact::Cutoff {
            table: TABLE,
            actor: actor(11),
            through: Some(Frontier {
                sequence: 0,
                digest: point(1),
            }),
        }],
    ] {
        assert!(
            Node::seal_restricted(&SEED, NETWORK, edit(), &[identity.recipient()], 0, &facts)
                .is_err()
        );
    }
    assert!(
        Node::seal_restricted(
            &SEED,
            NETWORK,
            Edit::Recover {
                heads: &[point(1), point(2)],
                selected: point(1)
            },
            &[],
            0,
            &[epoch(1, 31)]
        )
        .is_err()
    );
    let valid = node(
        None,
        &[&identity],
        b"test",
        &[Fact::Retire { actor: actor(11) }],
    );
    let mut wire = valid.bytes()[..valid.bytes().len() - 64].to_vec();
    let count = 4 + 16 + 32 + 2 + 32 + 8;
    wire[count..count + 2].copy_from_slice(&((MAX_FACTS + 1) as u16).to_le_bytes());
    assert!(Node::verify(&signed(wire, false), NETWORK, valid.authority()).is_err());
    let mut wire = valid.bytes().to_vec();
    wire[count + 3] ^= 1;
    assert!(
        Node::verify(&wire, NETWORK, valid.authority()).is_err(),
        "facts are signed"
    );
}

#[test]
fn authenticated_noncanonical_fact_wires_are_refused() {
    let identity = Identity::from_secret_bytes([13; 32]);
    let empty = node(None, &[&identity], b"payload", &[]);
    let count_at = 4 + 16 + 32 + 2 + 32 + 8;
    // Keep the signed envelope and real age payload intact, but construct facts
    // independently of seal_restricted's sorting and validation.
    let wire_with = |facts: &[Vec<u8>]| {
        let mut wire = empty.bytes()[..count_at].to_vec();
        wire.extend_from_slice(&(facts.len() as u16).to_le_bytes());
        for fact in facts {
            wire.extend_from_slice(fact);
        }
        wire.extend_from_slice(&empty.bytes()[count_at + 2..empty.bytes().len() - 64]);
        signed(wire, false)
    };
    let mut first = vec![1];
    first.extend_from_slice(&actor(11));
    let mut second = vec![1];
    second.extend_from_slice(&actor(12));
    let accepted = wire_with(&[first.clone(), second.clone()]);
    assert!(Node::verify(&accepted, NETWORK, empty.authority()).is_ok());
    for facts in [
        vec![first.clone(), first.clone()],
        vec![second, first],
        vec![vec![255]],
        vec![vec![1]],
    ] {
        assert!(Node::verify(&wire_with(&facts), NETWORK, empty.authority()).is_err());
    }
    for (sequence, digest) in [(0u64, point(1)), (1, [0; 32])] {
        let mut cutoff = vec![3];
        cutoff.extend_from_slice(&TABLE);
        cutoff.extend_from_slice(&actor(11));
        cutoff.extend_from_slice(&sequence.to_le_bytes());
        cutoff.extend_from_slice(&digest);
        assert!(Node::verify(&wire_with(&[cutoff]), NETWORK, empty.authority()).is_err());
    }
}

// A union-model state is projected through a complete, explicitly recovered
// signed DAG. Incomplete/conflicting histories never bypass collect's readiness.
fn joined_constraints<'a>(nodes: impl Iterator<Item = &'a Node>) -> Constraints {
    let mut nodes: Vec<Node> = nodes.cloned().collect();
    let mut index = Index::new(nodes.len() + 1);
    for node in &nodes {
        index.insert(node.header()).unwrap();
    }
    let projection = index.project();
    assert!(projection.pending.is_empty());
    let heads: Vec<_> = projection.heads.into_iter().collect();
    let head = if heads.len() > 1 {
        let recovery = recover(&heads, heads[0]);
        let head = recovery.id();
        nodes.push(recovery);
        head
    } else {
        heads[0]
    };
    Constraints::collect(NETWORK, authority(), head, nodes.iter()).unwrap()
}

#[test]
fn incomplete_conflicting_or_mislabeled_control_heads_cannot_construct_constraints() {
    let identity = Identity::from_secret_bytes([13; 32]);
    let genesis = node(None, &[&identity], b"genesis", &[]);
    let left = node(Some(genesis.id()), &[&identity], b"left", &[epoch(1, 31)]);
    let right = node(Some(genesis.id()), &[&identity], b"right", &[epoch(1, 31)]);
    assert!(Constraints::collect(NETWORK, authority(), left.id(), [&left].into_iter()).is_err());
    assert!(
        Constraints::collect(
            NETWORK,
            authority(),
            left.id(),
            [&genesis, &left, &right].into_iter()
        )
        .is_err()
    );
    assert!(
        Constraints::collect(
            NETWORK,
            authority(),
            genesis.id(),
            [&genesis, &left].into_iter()
        )
        .is_err()
    );
    assert!(
        Constraints::collect(
            NETWORK,
            authority(),
            left.id(),
            [&genesis, &left].into_iter()
        )
        .is_ok()
    );
}

fn retirement_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let identity = Identity::from_secret_bytes([13; 32]);
    let genesis = node(None, &[&identity], b"genesis", &[]);
    let retired = node(
        Some(genesis.id()),
        &[&identity],
        b"retired",
        &[Fact::Retire { actor: actor(11) }],
    );
    let other = node(Some(genesis.id()), &[&identity], b"other", &[]);
    let alternative = node(
        Some(genesis.id()),
        &[&identity],
        b"recovery alternative",
        &[],
    );
    let mut next = state.clone();
    next.insert(
        match action {
            "ObserveRetirement" => "retired",
            "ObserveOther" => "other",
            "Recover" => "recovered",
            _ => panic!("unknown action"),
        },
        1,
    );
    let mut nodes = vec![&genesis];
    if next["retired"] == 1 {
        nodes.push(&retired);
    }
    if next["other"] == 1 {
        nodes.push(&other);
    }
    if next["recovered"] == 1 {
        if next["other"] == 0 {
            nodes.push(&other);
        }
        nodes.push(&alternative);
    }
    let constraints = joined_constraints(nodes.into_iter());
    next.insert(
        "allowed",
        i64::from(constraints.check_actor(&actor(11)).is_ok()),
    );
    next
}

fn epoch_transition(state: &aterm_spec::interp::State, action: &str) -> aterm_spec::interp::State {
    let identity = Identity::from_secret_bytes([13; 32]);
    let genesis = node(None, &[&identity], b"genesis", &[]);
    let nodes = [
        epoch(1, 31),
        epoch(2, 37),
        epoch(2, 43),
        epoch(3, 47),
        epoch(4, 31),
    ]
    .map(|fact| node(Some(genesis.id()), &[&identity], b"state", &[fact]));
    let mut next = state.clone();
    next.insert(
        match action {
            "ObserveOld" => "old",
            "ObserveNew" => "newer",
            "ObserveConflict" => "conflict",
            "ObserveFresh" => "fresh",
            "ObserveReuse" => "reused",
            _ => panic!("unknown action"),
        },
        1,
    );
    let keys = ["old", "newer", "conflict", "fresh", "reused"];
    let known = std::iter::once(&genesis).chain(
        nodes
            .iter()
            .enumerate()
            .filter_map(|(i, node)| (next[keys[i]] == 1).then_some(node)),
    );
    let constraints = joined_constraints(known);
    let usable = match constraints.epoch() {
        Epoch::Unique { generation, .. } => generation as i64,
        _ => 0,
    };
    next.insert("usable", usable);
    next
}

fn rotation_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let identity = Identity::from_secret_bytes([13; 32]);
    let genesis = node(None, &[&identity], b"genesis", &[epoch(1, 31)]);
    let retired = node(
        Some(genesis.id()),
        &[&identity],
        b"retired",
        &[Fact::Retire { actor: actor(11) }],
    );
    let concurrent = node(
        Some(genesis.id()),
        &[&identity],
        b"concurrent",
        &[epoch(100, 37)],
    );
    let rotated = node(
        Some(retired.id()),
        &[&identity],
        b"rotated",
        &[epoch(101, 43)],
    );
    let repeated = node(
        Some(retired.id()),
        &[&identity],
        b"repeat",
        &[Fact::Retire { actor: actor(11) }],
    );
    let mut next = state.clone();
    next.insert(
        match action {
            "Retire" => "retired",
            "ConcurrentEpoch" => "concurrent",
            "Rotate" => "rotated",
            "RepeatRetirement" => "repeated",
            _ => panic!("unknown action"),
        },
        1,
    );
    let mut nodes = vec![&genesis];
    for (key, node) in [
        ("retired", &retired),
        ("concurrent", &concurrent),
        ("rotated", &rotated),
        ("repeated", &repeated),
    ] {
        if next[key] == 1 {
            nodes.push(node);
        }
    }
    let constraints = joined_constraints(nodes.into_iter());
    next.insert(
        "allowed",
        i64::from(matches!(constraints.epoch(), Epoch::Unique { .. })),
    );
    next
}

fn catalog_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    next.insert(
        match action {
            "ReceiveParent" => "parent",
            "ReceiveChild" => "child",
            "ReceiveCutoff" => "cutoff",
            _ => panic!("unknown action"),
        },
        1,
    );
    let identity = Identity::from_secret_bytes([13; 32]);
    let facts = if next["cutoff"] == 1 {
        vec![Fact::Cutoff {
            table: TABLE,
            actor: actor(11),
            through: None,
        }]
    } else {
        vec![]
    };
    let genesis = node(None, &[&identity], b"catalog policy", &facts);
    let constraints =
        Constraints::collect(NETWORK, authority(), genesis.id(), [&genesis].into_iter()).unwrap();
    let mut index = Index::new(2);
    if next["parent"] == 1 {
        index
            .insert(Header {
                id: point(1),
                actor: actor(11),
                sequence: 1,
                previous: None,
                parents: vec![],
            })
            .unwrap();
    }
    if next["child"] == 1 {
        index
            .insert(Header {
                id: point(2),
                actor: actor(12),
                sequence: 1,
                previous: None,
                parents: vec![point(1)],
            })
            .unwrap();
    }
    let catalog = constraints.catalog(TABLE, &index);
    next.insert("visible_parent", i64::from(catalog.permits(&point(1))));
    next.insert("visible_child", i64::from(catalog.permits(&point(2))));
    next
}

#[test]
fn real_fact_unions_conform_at_every_reachable_state_and_reject_unsafe_projection() {
    for (model, transition) in [
        (
            aterm_models_db::retirement_union_model(),
            retirement_transition
                as fn(&aterm_spec::interp::State, &str) -> aterm_spec::interp::State,
        ),
        (aterm_models_db::key_epoch_union_model(), epoch_transition),
        (
            aterm_models_db::retirement_epoch_model(),
            rotation_transition,
        ),
        (aterm_models_db::catalog_cutoff_model(), catalog_transition),
    ] {
        assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
        assert_eq!(
            aterm_spec::interp::fired_actions(&model).len(),
            model.actions.len()
        );
        for state in aterm_spec::interp::reachable_states(&model) {
            for action in &model.actions {
                if model.action_enabled(action.name, &state) {
                    let next = transition(&state, action.name);
                    assert!(
                        model.successors(action.name, &state).contains(&next),
                        "{} {}: {state:?} -> {next:?}",
                        model.name,
                        action.name
                    );
                }
            }
        }
    }
    let model = aterm_models_db::retirement_union_model();
    let state = [
        ("retired", 1),
        ("other", 1),
        ("recovered", 0),
        ("allowed", 0),
    ]
    .into_iter()
    .collect();
    let mut unsafe_choice = retirement_transition(&state, "Recover");
    unsafe_choice.insert("allowed", 1);
    assert!(!model.successors("Recover", &state).contains(&unsafe_choice));
    let model = aterm_models_db::key_epoch_union_model();
    let state = [
        ("old", 0),
        ("newer", 1),
        ("conflict", 0),
        ("fresh", 0),
        ("reused", 0),
        ("usable", 2),
    ]
    .into_iter()
    .collect();
    let mut last_arrival = epoch_transition(&state, "ObserveOld");
    last_arrival.insert("usable", 1);
    assert!(
        !model
            .successors("ObserveOld", &state)
            .contains(&last_arrival)
    );
    let model = aterm_models_db::catalog_cutoff_model();
    let state = [
        ("parent", 1),
        ("child", 1),
        ("cutoff", 0),
        ("visible_parent", 1),
        ("visible_child", 1),
    ]
    .into_iter()
    .collect();
    let mut laundered = catalog_transition(&state, "ReceiveCutoff");
    laundered.insert("visible_child", 1);
    assert!(
        !model
            .successors("ReceiveCutoff", &state)
            .contains(&laundered)
    );
    let model = aterm_models_db::retirement_epoch_model();
    let state = [
        ("retired", 1),
        ("concurrent", 0),
        ("rotated", 0),
        ("repeated", 0),
        ("allowed", 0),
    ]
    .into_iter()
    .collect();
    let mut unsafe_epoch = rotation_transition(&state, "ConcurrentEpoch");
    unsafe_epoch.insert("allowed", 1);
    assert!(
        !model
            .successors("ConcurrentEpoch", &state)
            .contains(&unsafe_epoch)
    );
}
