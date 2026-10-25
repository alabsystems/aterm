// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    age::Identity,
    commons::{
        binding::{Pending, Request},
        bootstrap::Descriptor,
        constraints::{Epoch, Fact},
        journal::{Journal, State, Trust},
        membership::{self, Eligibility},
        node::Edit,
        roster::{self, Grant, Role},
    },
    text::{Audience, Document},
};

const ROOT: [u8; 32] = [11; 32];

struct Lab {
    directory: aterm_tempfile::TempDir,
    foundation: Node,
    identities: [Identity; 2],
    requests: [Request; 2],
    eligibility: Eligibility,
}

impl Lab {
    fn new() -> Self {
        let foundation = Node::foundation(crate::envelope::signer_public(&ROOT).unwrap()).unwrap();
        let identities = [
            Identity::from_secret_bytes([19; 32]),
            Identity::from_secret_bytes([20; 32]),
        ];
        let requests = std::array::from_fn(|n| {
            Request::create(
                foundation.network(),
                foundation.authority(),
                [n as u8 + 41; 16],
                [n as u8 + 47; 16],
                &identities[n],
                &[n as u8 + 31; 32],
            )
            .unwrap()
        });
        let mut document = Document::new_vault("m1", &identities[0].recipient().to_bech32());
        document
            .add_machine("m2", &identities[1].recipient().to_bech32())
            .unwrap();
        document
            .set_entry(
                &Audience::Hosts(vec!["m1".into(), "m2".into()]),
                crate::commons::ENTRY,
                &Descriptor::from_genesis(foundation.clone())
                    .unwrap()
                    .bytes(),
            )
            .unwrap();
        Self {
            directory: aterm_tempfile::tempdir().unwrap(),
            foundation,
            identities,
            requests,
            eligibility: Eligibility::from_document(&document),
        }
    }

    fn journal(&self, n: usize) -> Journal {
        let mut journal = Journal::open(
            &self.directory.path().join(n.to_string()),
            Trust {
                network: self.foundation.network(),
                authority: self.foundation.authority(),
                genesis: self.foundation.id(),
            },
        )
        .unwrap();
        journal.ingest(self.foundation.bytes()).unwrap();
        journal
    }

    fn first(&self, journal: &Journal, n: usize) -> Node {
        let request = &self.requests[n];
        let (mut pending, challenge) = Pending::issue(
            request,
            self.foundation.network(),
            self.foundation.authority(),
        )
        .unwrap();
        let proof = pending
            .verify(
                &request
                    .answer(&self.identities[n], &[n as u8 + 31; 32], &challenge)
                    .unwrap(),
            )
            .unwrap();
        let host = format!("m{}", n + 1);
        let certificate =
            membership::approve(&ROOT, journal, &self.eligibility, &host, proof).unwrap();
        assert_eq!(*certificate.issued_on(), self.foundation.id());
        let node = roster::rotate(
            &ROOT,
            &self.eligibility,
            journal,
            vec![Grant::certified(host, certificate, Role::Writer)],
        )
        .unwrap();
        assert_eq!(node.kind(), Kind::Update);
        assert_eq!(node.parents(), [self.foundation.id()]);
        node
    }

    fn recover(&self, a: &Node, b: &Node) -> Node {
        Node::seal(
            &ROOT,
            self.foundation.network(),
            Edit::Recover {
                heads: &[a.id(), b.id()],
                selected: a.id(),
            },
            &[],
            1,
        )
        .unwrap()
    }

    fn rotate(&self, journal: &Journal) -> Node {
        roster::rotate(
            &ROOT,
            &self.eligibility,
            journal,
            self.requests
                .iter()
                .enumerate()
                .map(|(n, r)| {
                    Grant::recorded(format!("m{}", n + 1), r.binding().clone(), Role::Writer)
                })
                .collect(),
        )
        .unwrap()
    }
}

fn available(journal: &Journal, identity: &Identity) -> bool {
    roster::current(journal, identity)
        .ok()
        .flatten()
        .is_some_and(|access| access.with_key(journal, |_| ()).is_ok())
}

fn foundation_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let lab = Lab::new();
    let node = &lab.foundation;
    let (mut accepted, mut canonical, mut grants) = (false, true, false);
    let scenario = match action {
        "Derive" => {
            let other = Node::foundation(node.authority()).unwrap();
            accepted = true;
            canonical = node.bytes() == other.bytes()
                && node.id() == other.id()
                && node.network() == other.network();
            1
        }
        "RoundTrip" => {
            let decoded = Node::verify(node.bytes(), node.network(), node.authority()).unwrap();
            accepted = decoded.bytes() == node.bytes() && decoded.is_foundation();
            2
        }
        "Altered" => {
            let mut wire = node.bytes().to_vec();
            wire[20] ^= 1;
            accepted = Node::verify(&wire, node.network(), node.authority()).is_ok();
            3
        }
        "ForeignRoot" => {
            accepted = Node::verify(
                node.bytes(),
                node.network(),
                crate::envelope::signer_public(&[12; 32]).unwrap(),
            )
            .is_ok();
            4
        }
        "ForeignNetwork" => {
            let mut network = node.network();
            network[0] ^= 1;
            accepted = Node::verify(node.bytes(), network, node.authority()).is_ok();
            5
        }
        "NoAccess" => {
            let journal = lab.journal(0);
            grants = journal.current(&lab.identities[0]).unwrap().is_some()
                || available(&journal, &lab.identities[0])
                || node.open(&lab.identities[0]).is_ok();
            assert_eq!(journal.constraints().unwrap().epoch(), Epoch::Missing);
            6
        }
        "NoHistorical" => {
            let journal = lab.journal(0);
            grants = journal.historical(node.id(), &lab.identities[0]).is_ok()
                || journal.selected_ancestor(node.id(), node.id());
            7
        }
        "NonCanonicalDefault" => {
            let mut wire = node.bytes().to_vec();
            wire[4] ^= 1;
            let network = wire[4..20].try_into().unwrap();
            accepted = Node::verify(&wire, network, node.authority()).is_ok();
            8
        }
        _ => panic!("unknown foundation action"),
    };
    let mut next = state.clone();
    next.insert("scenario", scenario);
    next.insert("accepted", i64::from(accepted));
    next.insert("canonical", i64::from(canonical));
    next.insert("grants", i64::from(grants));
    next
}

#[test]
fn canonical_foundation_conforms_and_a_public_grant_mutant_is_detected() {
    let model = aterm_models_db::foundation_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = foundation_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {next:?}",
                    action.name
                );
            }
        }
    }
    let initial = model.init_state();
    let mut mutant = foundation_transition(&initial, "NoAccess");
    mutant.insert("grants", 1);
    assert!(!model.successors("NoAccess", &initial).contains(&mutant));
    assert!(!model.check_invariant("NoPublicGrant", &mutant));
}

fn repair_transition(state: &aterm_spec::interp::State, action: &str) -> aterm_spec::interp::State {
    let lab = Lab::new();
    let mut journals = [lab.journal(0), lab.journal(1)];
    let nodes = [lab.first(&journals[0], 0), lab.first(&journals[1], 1)];
    if state["a"] == 1 {
        journals[0].ingest(nodes[0].bytes()).unwrap();
    }
    if state["b"] == 1 {
        journals[1].ingest(nodes[1].bytes()).unwrap();
    }
    let exchange = |journals: &mut [Journal; 2]| {
        journals[0].ingest(nodes[1].bytes()).unwrap();
        journals[1].ingest(nodes[0].bytes()).unwrap();
    };
    let recovery = lab.recover(&nodes[0], &nodes[1]);
    if state["merged"] == 1 {
        exchange(&mut journals);
    }
    if state["recovered"] == 1 {
        for journal in &mut journals {
            journal.ingest(recovery.bytes()).unwrap();
        }
    }
    if state["fresh"] == 1 {
        let fresh = lab.rotate(&journals[0]);
        for journal in &mut journals {
            journal.ingest(fresh.bytes()).unwrap();
        }
    }
    let cached = std::array::from_fn::<_, 2, _>(|n| {
        roster::current(&journals[n], &lab.identities[n])
            .ok()
            .flatten()
    });
    match action {
        "FirstA" => {
            journals[0].ingest(nodes[0].bytes()).unwrap();
        }
        "FirstB" => {
            journals[1].ingest(nodes[1].bytes()).unwrap();
        }
        "Exchange" => {
            exchange(&mut journals);
            for n in 0..2 {
                assert!(matches!(journals[n].state(), State::Conflict { .. }));
                assert!(
                    cached[n]
                        .as_ref()
                        .unwrap()
                        .with_key(&journals[n], |_| ())
                        .is_err()
                );
            }
        }
        "Recover" => {
            for journal in &mut journals {
                journal.ingest(recovery.bytes()).unwrap();
                assert_eq!(
                    journal.constraints().unwrap().epoch(),
                    Epoch::Conflict { generation: 1 }
                );
            }
        }
        "Rotate" => {
            let fresh = lab.rotate(&journals[0]);
            for journal in &mut journals {
                journal.ingest(fresh.bytes()).unwrap();
            }
        }
        _ => panic!("unknown repair action"),
    }
    let mut next = state.clone();
    next.insert("a", i64::from(journals[0].node(&nodes[0].id()).is_some()));
    next.insert("b", i64::from(journals[1].node(&nodes[1].id()).is_some()));
    next.insert(
        "merged",
        i64::from(
            journals[0].node(&nodes[1].id()).is_some()
                && journals[1].node(&nodes[0].id()).is_some(),
        ),
    );
    next.insert(
        "recovered",
        i64::from(journals[0].node(&recovery.id()).is_some()),
    );
    next.insert(
        "fresh",
        i64::from(
            journals[0]
                .inventory()
                .filter_map(|id| journals[0].node(id))
                .any(|node| {
                    node.facts()
                        .iter()
                        .any(|fact| matches!(fact, Fact::Epoch { generation: 2, .. }))
                }),
        ),
    );
    next.insert(
        "available_a",
        i64::from(available(&journals[0], &lab.identities[0])),
    );
    next.insert(
        "available_b",
        i64::from(available(&journals[1], &lab.identities[1])),
    );
    let other = Node::foundation(lab.foundation.authority()).unwrap();
    next.insert(
        "root_same",
        i64::from(
            journals[0].trust() == journals[1].trust() && other.id() == journals[0].trust().genesis,
        ),
    );
    next
}

#[test]
fn competing_first_rosters_conform_and_recovery_alone_cannot_restore_keys() {
    let model = aterm_models_db::foundation_repair_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = repair_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {next:?}",
                    action.name
                );
                if action.name == "Recover" {
                    let mut mutant = next;
                    mutant.insert("available_a", 1);
                    assert!(!model.check_invariant("ConflictWithholds", &mutant));
                    assert!(!model.successors(action.name, &state).contains(&mutant));
                }
            }
        }
    }
}

#[test]
fn every_foundation_byte_and_length_is_canonical_and_old_wires_stay_distinct() {
    let lab = Lab::new();
    let root = &lab.foundation;
    assert!(!root.has_payload());
    for index in 0..root.bytes().len() {
        let mut bytes = root.bytes().to_vec();
        bytes[index] ^= 1;
        assert!(
            Node::verify(&bytes, root.network(), root.authority()).is_err(),
            "byte {index}"
        );
    }
    for length in 0..root.bytes().len() {
        assert!(Node::verify(&root.bytes()[..length], root.network(), root.authority()).is_err());
    }
    let mut extended = root.bytes().to_vec();
    extended.push(0);
    assert!(Node::verify(&extended, root.network(), root.authority()).is_err());
    assert!(Node::foundation([0; 32]).is_err());
    let legacy = Node::test_seal(
        &ROOT,
        root.network(),
        Edit::Create {
            hosts: &[],
            payload: b"legacy",
        },
        &[lab.identities[0].recipient()],
        0,
        &[],
    )
    .unwrap();
    assert!(!legacy.is_foundation());
    assert!(legacy.has_payload());
    assert_eq!(
        legacy.open(&lab.identities[0]).unwrap().as_slice(),
        b"legacy"
    );
    let mut journal = lab.journal(0);
    assert!(journal.ingest(legacy.bytes()).is_err());
    assert_eq!(
        Descriptor::from_journal(&journal).unwrap().bytes(),
        Descriptor::from_genesis(root.clone()).unwrap().bytes()
    );
    assert!(
        roster::rotate(
            &ROOT,
            &lab.eligibility,
            &journal,
            vec![Grant::recorded(
                "m1",
                lab.requests[0].binding().clone(),
                Role::Writer
            )],
        )
        .is_err(),
        "a public foundation cannot certify an unapproved device"
    );
    let first = lab.first(&journal, 0);
    journal.ingest(first.bytes()).unwrap();
    let selecting_public_root = Node::seal(
        &ROOT,
        root.network(),
        Edit::Recover {
            heads: &[root.id(), first.id()],
            selected: root.id(),
        },
        &[],
        1,
    )
    .unwrap();
    journal.ingest(selecting_public_root.bytes()).unwrap();
    assert!(matches!(journal.state(), State::Invalid { .. }));
    assert!(journal.current(&lab.identities[0]).unwrap().is_none());
}
