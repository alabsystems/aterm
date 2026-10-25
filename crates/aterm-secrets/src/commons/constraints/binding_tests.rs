// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    age::Identity,
    commons::{
        binding::Request,
        journal::{Journal, State, Trust},
        node::{Edit, Node},
    },
};
use ring::signature::{Ed25519KeyPair, KeyPair};

const NETWORK: [u8; 16] = [7; 16];
const ROOT: [u8; 32] = [41; 32];
fn authority() -> [u8; 32] {
    Ed25519KeyPair::from_seed_unchecked(&ROOT)
        .unwrap()
        .public_key()
        .as_ref()
        .try_into()
        .unwrap()
}
fn binding(identity: &Identity, writer: u8, machine: u8, incarnation: u8) -> Binding {
    Request::create(
        NETWORK,
        authority(),
        [machine; 16],
        [incarnation; 16],
        identity,
        &[writer; 32],
    )
    .unwrap()
    .binding()
    .clone()
}
fn fact(binding: &Binding) -> Fact {
    Fact::Bind {
        binding: binding.clone(),
    }
}
fn node(parent: Option<Digest>, facts: &[Fact]) -> Node {
    let hosts = ["m1".into()];
    let edit = match parent {
        Some(parent) => Edit::Update {
            parent,
            hosts: &hosts,
            payload: b"control",
        },
        None => Edit::Create {
            hosts: &hosts,
            payload: b"control",
        },
    };
    Node::seal_restricted(
        &ROOT,
        NETWORK,
        edit,
        &[Identity::from_secret_bytes([13; 32]).recipient()],
        1,
        facts,
    )
    .unwrap()
}
fn constraints(nodes: &[&Node], head: Digest) -> Constraints {
    Constraints::collect(NETWORK, authority(), head, nodes.iter().copied()).unwrap()
}

#[test]
fn recipient_retirement_wire_rejects_future_bindings_without_inventing_an_actor() {
    let identity = Identity::from_secret_bytes([19; 32]);
    let pending = binding(&identity, 21, 1, 1);
    let genesis = node(None, &[]);
    let retired = node(
        Some(genesis.id()),
        &[Fact::RetireRecipient {
            recipient: identity.recipient(),
        }],
    );
    let decoded = Node::verify(retired.bytes(), NETWORK, authority()).unwrap();
    assert_eq!(decoded.facts(), retired.facts());
    for end in 0..retired.bytes().len() {
        assert!(Node::verify(&retired.bytes()[..end], NETWORK, authority()).is_err());
    }
    let before = constraints(&[&genesis, &retired], retired.id());
    assert!(before.recipient_retired(&identity.recipient()).unwrap());
    assert!(before.check_device_candidate(&pending).is_err());
    assert!(before.check_known_actor(&pending.actor()).is_err());
    let late = node(Some(retired.id()), &[fact(&pending)]);
    let after = constraints(&[&late, &retired, &genesis], late.id());
    assert!(after.check_known_actor(&pending.actor()).is_ok());
    assert!(after.check_actor(&pending.actor()).is_err());
    assert!(after.check_device_candidate(&pending).is_err());
    assert!(
        Node::seal_restricted(
            &ROOT,
            NETWORK,
            Edit::Create {
                hosts: &[],
                payload: b"invalid"
            },
            &[identity.recipient()],
            1,
            &[Fact::RetireRecipient {
                recipient: crate::age::Recipient([0; 32])
            }]
        )
        .is_err()
    );
    let mut alias = identity.recipient().0;
    alias[31] |= 0x80;
    assert!(
        Node::seal_restricted(
            &ROOT,
            NETWORK,
            Edit::Create {
                hosts: &[],
                payload: b"invalid alias"
            },
            &[identity.recipient()],
            1,
            &[Fact::RetireRecipient {
                recipient: crate::age::Recipient(alias)
            }]
        )
        .is_err()
    );
}

#[test]
fn recorded_binding_is_exact_and_repetition_is_inert() {
    let identity = Identity::from_secret_bytes([19; 32]);
    let original = binding(&identity, 21, 1, 1);
    let genesis = node(None, &[]);
    let empty = constraints(&[&genesis], genesis.id());
    assert!(empty.check_device_candidate(&original).is_ok());
    assert!(empty.check_recorded_device(&original).is_err());
    assert!(empty.check_known_actor(&original.actor()).is_err());
    let recorded = node(Some(genesis.id()), &[fact(&original)]);
    assert_eq!(recorded.facts(), &[fact(&original)]);
    assert!(recorded.bytes().starts_with(b"ACN3"));
    let repeated = node(Some(recorded.id()), &[fact(&original)]);
    let current = constraints(&[&repeated, &genesis, &recorded], repeated.id());
    assert!(current.check_recorded_device(&original).is_ok());
    assert!(current.check_known_actor(&original.actor()).is_ok());
    assert_eq!(
        current
            .facts()
            .filter(|f| matches!(f, Fact::Bind { .. }))
            .count(),
        1
    );
    let changed_machine = binding(&identity, 21, 2, 1);
    assert!(current.check_device_candidate(&changed_machine).is_err());
    let changed_recipient = binding(&Identity::from_secret_bytes([23; 32]), 21, 1, 1);
    assert!(current.check_device_candidate(&changed_recipient).is_err());
    let fresh_writer = binding(&identity, 25, 1, 2);
    assert!(current.check_device_candidate(&fresh_writer).is_ok());
    assert!(current.check_recorded_device(&fresh_writer).is_err());
    let same_writer = binding(&identity, 21, 1, 2);
    assert!(current.check_device_candidate(&same_writer).is_ok());
    for changed in [
        binding(&identity, 21, 2, 2),
        binding(&Identity::from_secret_bytes([23; 32]), 21, 1, 2),
    ] {
        assert!(current.check_device_candidate(&changed).is_err());
        let conflict = node(Some(repeated.id()), &[fact(&changed)]);
        let conflicted = constraints(&[&conflict, &repeated, &genesis, &recorded], conflict.id());
        assert!(conflicted.check_recorded_device(&original).is_err());
        assert!(conflicted.check_known_actor(&original.actor()).is_err());
        assert!(conflicted.check_device_candidate(&fresh_writer).is_err());
    }
}

#[test]
fn unreadable_unselected_retirement_prevents_return_under_new_keys_after_restart() {
    let identity = Identity::from_secret_bytes([19; 32]);
    let original = binding(&identity, 21, 1, 1);
    let genesis = node(None, &[fact(&original)]);
    let retired = node(
        Some(genesis.id()),
        &[Fact::Retire {
            actor: original.actor(),
        }],
    );
    let unchanged = node(Some(genesis.id()), &[fact(&original)]);
    let recovery = Node::seal(
        &ROOT,
        NETWORK,
        Edit::Recover {
            heads: &[retired.id(), unchanged.id()],
            selected: unchanged.id(),
        },
        &[],
        4,
    )
    .unwrap();
    let newcomer = binding(&identity, 25, 1, 2);
    let new_recipient_same_writer = binding(&Identity::from_secret_bytes([27; 32]), 21, 1, 2);
    let safe = binding(&Identity::from_secret_bytes([29; 32]), 31, 3, 1);
    for reverse in [false, true] {
        let directory = aterm_tempfile::tempdir().unwrap();
        let trust = Trust {
            network: NETWORK,
            authority: authority(),
            genesis: genesis.id(),
        };
        let mut journal = Journal::open(directory.path(), trust).unwrap();
        let mut nodes = [&genesis, &retired, &unchanged, &recovery];
        if reverse {
            nodes.reverse();
        }
        for node in nodes {
            journal.ingest(node.bytes()).unwrap();
        }
        assert!(matches!(journal.state(), State::Ready { basis, .. } if basis == unchanged.id()));
        assert!(
            journal.current(&identity).is_err(),
            "no historical decryption key is needed"
        );
        drop(journal);
        let reopened = Journal::open(directory.path(), trust).unwrap();
        let current = reopened.constraints().unwrap();
        assert!(current.check_device_candidate(&original).is_err());
        assert!(
            current.check_known_actor(&original.actor()).is_ok(),
            "retirement does not erase identity evidence"
        );
        assert!(current.check_device_candidate(&newcomer).is_err());
        assert!(
            current
                .check_device_candidate(&new_recipient_same_writer)
                .is_err()
        );
        assert!(current.check_device_candidate(&safe).is_ok());
    }
}

#[test]
fn missing_and_conflicting_binding_evidence_withholds_all_device_authorization() {
    let identity = Identity::from_secret_bytes([19; 32]);
    let original = binding(&identity, 21, 1, 1);
    let candidate = binding(&Identity::from_secret_bytes([23; 32]), 25, 2, 1);
    let genesis = node(
        None,
        &[Fact::Retire {
            actor: original.actor(),
        }],
    );
    assert!(
        constraints(&[&genesis], genesis.id())
            .check_device_candidate(&candidate)
            .is_err()
    );
    let supplied = node(Some(genesis.id()), &[fact(&original)]);
    assert!(
        constraints(&[&supplied, &genesis], supplied.id())
            .check_device_candidate(&candidate)
            .is_ok()
    );
    let conflicting = binding(&Identity::from_secret_bytes([27; 32]), 21, 1, 1);
    let branch = node(Some(genesis.id()), &[fact(&conflicting)]);
    let recovered = Node::seal(
        &ROOT,
        NETWORK,
        Edit::Recover {
            heads: &[supplied.id(), branch.id()],
            selected: supplied.id(),
        },
        &[],
        3,
    )
    .unwrap();
    let current = constraints(&[&recovered, &supplied, &genesis, &branch], recovered.id());
    assert!(current.check_recorded_device(&original).is_err());
    assert!(current.check_device_candidate(&candidate).is_err());
}

#[test]
fn binding_computation_limit_preserves_the_authenticated_journal() {
    let original = binding(&Identity::from_secret_bytes([19; 32]), 21, 1, 1);
    let directory = aterm_tempfile::tempdir().unwrap();
    let genesis = node(None, &[]);
    let mut journal = Journal::open(
        directory.path(),
        Trust {
            network: NETWORK,
            authority: authority(),
            genesis: genesis.id(),
        },
    )
    .unwrap();
    journal.ingest(genesis.bytes()).unwrap();
    let mut parent = genesis.id();
    for start in (0..MAX_BINDINGS).step_by(MAX_FACTS) {
        let facts: Vec<_> = (start..start + MAX_FACTS)
            .map(|index| {
                let mut bytes = original.bytes();
                bytes[128..144].fill(0);
                bytes[128..136].copy_from_slice(&(index as u64 + 1).to_le_bytes());
                fact(&Binding::decode(&bytes).unwrap())
            })
            .collect();
        let next = node(Some(parent), &facts);
        journal.ingest(next.bytes()).unwrap();
        parent = next.id();
    }
    assert!(journal.constraints().is_ok());
    let mut bytes = original.bytes();
    bytes[128..144].fill(0);
    bytes[128..136].copy_from_slice(&(MAX_BINDINGS as u64 + 1).to_le_bytes());
    let overflow = node(Some(parent), &[fact(&Binding::decode(&bytes).unwrap())]);
    journal.ingest(overflow.bytes()).unwrap();
    assert!(matches!(journal.state(), State::Ready { .. }));
    assert!(journal.constraints().is_err());
    assert!(journal.node(&overflow.id()).is_some());
    assert_eq!(journal.inventory().count(), MAX_BINDINGS / MAX_FACTS + 2);
}

fn resign(mut unsigned: Vec<u8>, domain: &[u8]) -> Vec<u8> {
    let mut message = domain.to_vec();
    message.extend_from_slice(&unsigned);
    unsigned.extend_from_slice(
        Ed25519KeyPair::from_seed_unchecked(&ROOT)
            .unwrap()
            .sign(&message)
            .as_ref(),
    );
    unsigned
}

#[test]
fn wire_versions_and_independent_trust_context_cannot_reinterpret_a_binding() {
    let identity = Identity::from_secret_bytes([19; 32]);
    let original = binding(&identity, 21, 1, 1);
    let genesis = node(None, &[fact(&original)]);
    let mut old_version = genesis.bytes()[..genesis.bytes().len() - 64].to_vec();
    old_version[..4].copy_from_slice(b"ACN2");
    assert!(
        Node::verify(
            &resign(old_version, b"aterm/commons/control-node/2\0"),
            NETWORK,
            authority()
        )
        .is_err()
    );
    let empty = node(None, &[]);
    let mut old_version = empty.bytes()[..empty.bytes().len() - 64].to_vec();
    old_version[..4].copy_from_slice(b"ACN2");
    let old = Node::verify(
        &resign(old_version, b"aterm/commons/control-node/2\0"),
        NETWORK,
        authority(),
    )
    .unwrap();
    assert!(
        constraints(&[&old], old.id())
            .check_device_candidate(&original)
            .is_err()
    );
    assert!(old.open(&Identity::from_secret_bytes([13; 32])).is_ok());
    let upgraded = node(Some(old.id()), &[fact(&original)]);
    assert!(
        constraints(&[&old, &upgraded], upgraded.id())
            .check_recorded_device(&original)
            .is_err()
    );
    assert!(
        constraints(&[&old], old.id())
            .check_recorded_device(&original)
            .is_err()
    );
    assert!(
        Constraints::collect([8; 16], authority(), genesis.id(), [&genesis].into_iter()).is_err()
    );
    assert!(Constraints::collect(NETWORK, [9; 32], genesis.id(), [&genesis].into_iter()).is_err());
    let offset = 4 + 16 + 32 + 2 + 32 + 8 + 2 + 1;
    for inner_offset in [0, 16, 48, 64, 128] {
        let mut unsigned = genesis.bytes()[..genesis.bytes().len() - 64].to_vec();
        unsigned[offset + inner_offset] = 0;
        if inner_offset < 48 {
            unsigned[offset + inner_offset] ^= 0x80;
        } else if inner_offset == 64 {
            unsigned[offset + inner_offset..offset + inner_offset + 32].fill(0);
        } else {
            unsigned[offset + inner_offset..offset + inner_offset + 16].fill(0);
        }
        assert!(
            Node::verify(
                &resign(unsigned, b"aterm/commons/control-node/3\0"),
                NETWORK,
                authority()
            )
            .is_err()
        );
    }
}

fn binding_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "ObserveBinding" => next.insert("bound", 1),
        "ObserveRetirement" => next.insert("retired", 1),
        "ObserveConflict" => next.insert("conflict", 1),
        "ObserveWriterConflict" => next.insert("conflict", 2),
        "Recover" => next.insert("recovered", 1),
        _ => panic!("unknown binding transition"),
    };
    let identity = Identity::from_secret_bytes([19; 32]);
    let original = binding(&identity, 21, 1, 1);
    let returning = binding(&identity, 23, 1, 2);
    let genesis = node(None, &[]);
    let mut nodes = vec![genesis];
    if next["bound"] == 1 {
        nodes.push(node(Some(nodes.last().unwrap().id()), &[fact(&original)]));
    }
    if next["retired"] == 1 {
        nodes.push(node(
            Some(nodes.last().unwrap().id()),
            &[Fact::Retire {
                actor: original.actor(),
            }],
        ));
    }
    if next["conflict"] == 1 {
        let changed = binding(&Identity::from_secret_bytes([25; 32]), 21, 1, 1);
        nodes.push(node(Some(nodes.last().unwrap().id()), &[fact(&changed)]));
    }
    if next["conflict"] == 2 {
        let changed = binding(&Identity::from_secret_bytes([25; 32]), 21, 1, 2);
        nodes.push(node(Some(nodes.last().unwrap().id()), &[fact(&changed)]));
    }
    if next["recovered"] == 1 {
        // Select a concurrent payload without binding evidence. The full DAG
        // must still retain every binding and retirement from the other branch.
        if nodes.len() == 1 {
            nodes.push(node(Some(nodes[0].id()), &[]));
        }
        let previous = nodes.last().unwrap().id();
        nodes.push(node(Some(nodes[0].id()), &[]));
        let selected = nodes.last().unwrap().id();
        nodes.push(
            Node::seal(
                &ROOT,
                NETWORK,
                Edit::Recover {
                    heads: &[previous, selected],
                    selected,
                },
                &[],
                9,
            )
            .unwrap(),
        );
    }
    let current = constraints(
        &nodes.iter().collect::<Vec<_>>(),
        nodes.last().unwrap().id(),
    );
    next.insert(
        "allowed",
        i64::from(current.check_device_candidate(&returning).is_ok()),
    );
    next.insert(
        "recorded",
        i64::from(current.check_recorded_device(&original).is_ok()),
    );
    next
}

#[test]
fn real_binding_transitions_conform_and_forgetting_a_retirement_is_detected() {
    let model = aterm_models_db::binding_ledger_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = binding_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let initial = model.init_state();
    let retired = binding_transition(&initial, "ObserveRetirement");
    let mut forgotten = binding_transition(&retired, "Recover");
    forgotten.insert("allowed", 1);
    assert!(!model.successors("Recover", &retired).contains(&forgotten));
    assert!(!model.check_invariant("RetiredRecipientCannotReturn", &forgotten));
}
