// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    age::Identity,
    commons::{
        binding::{Pending, Possession, Request},
        journal::{Journal, Trust},
        node::{Edit, Node},
    },
    text::Document,
};
use ring::signature::{Ed25519KeyPair, KeyPair};

const NETWORK: [u8; 16] = [7; 16];
const ROOT_SEED: [u8; 32] = [11; 32];
const WRITER_SEED: [u8; 32] = [13; 32];

fn approve(
    seed: &[u8; 32],
    journal: &Journal,
    document: &Document,
    host: &str,
    possession: Possession,
) -> Result<Certificate> {
    super::approve(
        seed,
        journal,
        &Eligibility::from_document(document),
        host,
        possession,
    )
}

fn root() -> [u8; 32] {
    Ed25519KeyPair::from_seed_unchecked(&ROOT_SEED)
        .unwrap()
        .public_key()
        .as_ref()
        .try_into()
        .unwrap()
}
fn document(identity: &Identity) -> Document {
    let mut document = Document::new_vault("m1", &identity.recipient().to_bech32());
    document
        .set_entry(
            &crate::text::Audience::Hosts(vec!["m1".into()]),
            crate::commons::ENTRY,
            b"control descriptor",
        )
        .unwrap();
    document
}
fn excluded_document(identity: &Identity) -> Document {
    let mut document = Document::new_vault("m1", &identity.recipient().to_bech32());
    let other = Identity::from_secret_bytes([29; 32]);
    document
        .add_machine("m2", &other.recipient().to_bech32())
        .unwrap();
    document
        .set_entry(
            &crate::text::Audience::Hosts(vec!["m2".into()]),
            crate::commons::ENTRY,
            b"control descriptor",
        )
        .unwrap();
    document
}
fn request(
    identity: &Identity,
    network: [u8; 16],
    authority: [u8; 32],
    incarnation: u8,
) -> Request {
    Request::create(
        network,
        authority,
        [17; 16],
        [incarnation; 16],
        identity,
        &WRITER_SEED,
    )
    .unwrap()
}
fn proof(identity: &Identity, request: &Request) -> Possession {
    let (mut pending, challenge) = Pending::issue(
        request,
        *request.binding().network(),
        *request.binding().authority(),
    )
    .unwrap();
    let answer = request.answer(identity, &WRITER_SEED, &challenge).unwrap();
    pending.verify(&answer).unwrap()
}
fn genesis(identity: &Identity) -> Node {
    Node::seal(
        &ROOT_SEED,
        NETWORK,
        Edit::Create {
            hosts: &[],
            payload: b"initial control",
        },
        &[identity.recipient()],
        0,
    )
    .unwrap()
}
fn journal(dir: &std::path::Path, genesis: &Node) -> Journal {
    Journal::open(
        dir,
        Trust {
            network: NETWORK,
            authority: root(),
            genesis: genesis.id(),
        },
    )
    .unwrap()
}

#[test]
fn explicit_approval_binds_keys_and_exact_current_head_without_granting_roster_membership() {
    let identity = Identity::from_secret_bytes([19; 32]);
    let request = request(&identity, NETWORK, root(), 23);
    let doc = document(&identity);
    let genesis = genesis(&identity);
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut journal = journal(dir.path(), &genesis);
    journal.ingest(genesis.bytes()).unwrap();
    let certificate =
        approve(&ROOT_SEED, &journal, &doc, "m1", proof(&identity, &request)).unwrap();
    assert_eq!(certificate.binding(), request.binding());
    assert_eq!(certificate.issued_on(), &genesis.id());
    assert_eq!(certificate.bytes().len(), CERTIFICATE_LEN);
    assert_eq!(
        certificate.id(),
        aterm_digest::Sha256::digest(certificate.bytes())
    );
    let decoded = Certificate::verify(certificate.bytes(), NETWORK, root()).unwrap();
    assert_eq!(decoded.binding(), request.binding());
    assert_eq!(
        journal.inventory().count(),
        1,
        "approval is not a roster publication"
    );
    for at in [0, 4, 20, 52, 68, 100, 132, 148, CERTIFICATE_LEN - 1] {
        let mut wire = certificate.bytes().to_vec();
        wire[at] ^= 1;
        assert!(Certificate::verify(&wire, NETWORK, root()).is_err());
    }
    for n in 0..CERTIFICATE_LEN {
        assert!(Certificate::verify(&certificate.bytes()[..n], NETWORK, root()).is_err());
    }
    let mut extended = certificate.bytes().to_vec();
    extended.push(0);
    assert!(Certificate::verify(&extended, NETWORK, root()).is_err());
    assert!(Certificate::verify(certificate.bytes(), [8; 16], root()).is_err());
    assert!(Certificate::verify(certificate.bytes(), NETWORK, [8; 32]).is_err());
    let next = Node::seal(
        &ROOT_SEED,
        NETWORK,
        Edit::Update {
            parent: genesis.id(),
            hosts: &[],
            payload: b"new control",
        },
        &[identity.recipient()],
        0,
    )
    .unwrap();
    journal.ingest(next.bytes()).unwrap();
    let newer = approve(&ROOT_SEED, &journal, &doc, "m1", proof(&identity, &request)).unwrap();
    assert_eq!(newer.issued_on(), &next.id());
    assert_ne!(newer.id(), certificate.id());
    assert!(
        Certificate::verify(certificate.bytes(), NETWORK, root()).is_ok(),
        "older certificates remain historical evidence, not a current grant"
    );
}

#[test]
fn approval_rechecks_recipient_context_readiness_and_retired_writer_key() {
    let identity = Identity::from_secret_bytes([19; 32]);
    let request = request(&identity, NETWORK, root(), 23);
    let doc = document(&identity);
    let genesis = genesis(&identity);
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut journal = journal(dir.path(), &genesis);
    assert!(approve(&ROOT_SEED, &journal, &doc, "m1", proof(&identity, &request)).is_err());
    journal.ingest(genesis.bytes()).unwrap();
    assert!(
        approve(
            &ROOT_SEED,
            &journal,
            &excluded_document(&identity),
            "m1",
            proof(&identity, &request)
        )
        .is_err()
    );
    let mut without_eligibility = Document::new_vault("m1", &identity.recipient().to_bech32());
    for open in [true, false] {
        without_eligibility.set_open(open).unwrap();
        assert!(
            approve(
                &ROOT_SEED,
                &journal,
                &without_eligibility,
                "m1",
                proof(&identity, &request)
            )
            .is_err(),
            "open and formerly open vault entries do not imply Commons eligibility"
        );
    }
    let proved_before_removal = proof(&identity, &request);
    let mut removed = document(&identity);
    removed
        .unset_entry(
            &crate::text::Audience::Hosts(vec!["m1".into()]),
            crate::commons::ENTRY,
        )
        .unwrap();
    assert!(approve(&ROOT_SEED, &journal, &removed, "m1", proved_before_removal).is_err());
    assert!(approve(&ROOT_SEED, &journal, &doc, "m2", proof(&identity, &request)).is_err());
    assert!(approve(&[31; 32], &journal, &doc, "m1", proof(&identity, &request)).is_err());
    let other = Identity::from_secret_bytes([29; 32]);
    let other_doc = document(&other);
    assert!(
        approve(
            &ROOT_SEED,
            &journal,
            &other_doc,
            "m1",
            proof(&identity, &request)
        )
        .is_err()
    );
    let fork_dir = aterm_tempfile::tempdir().unwrap();
    let mut forked = self::journal(fork_dir.path(), &genesis);
    forked.ingest(genesis.bytes()).unwrap();
    for payload in [b"left".as_slice(), b"right".as_slice()] {
        let branch = Node::seal(
            &ROOT_SEED,
            NETWORK,
            Edit::Update {
                parent: genesis.id(),
                hosts: &[],
                payload,
            },
            &[identity.recipient()],
            0,
        )
        .unwrap();
        forked.ingest(branch.bytes()).unwrap();
    }
    assert!(
        approve(&ROOT_SEED, &forked, &doc, "m1", proof(&identity, &request)).is_err(),
        "a complete but conflicting journal cannot approve"
    );
    for (network, authority) in [([8; 16], root()), (NETWORK, [9; 32])] {
        let foreign = self::request(&identity, network, authority, 23);
        assert!(approve(&ROOT_SEED, &journal, &doc, "m1", proof(&identity, &foreign)).is_err());
    }
    let proved_before_revocation = proof(&identity, &request);
    let retirement = Node::seal_restricted(
        &ROOT_SEED,
        NETWORK,
        Edit::Update {
            parent: genesis.id(),
            hosts: &[],
            payload: b"retired",
        },
        &[identity.recipient()],
        0,
        &[crate::commons::constraints::Fact::Retire {
            actor: request.binding().actor(),
        }],
    )
    .unwrap();
    journal.ingest(retirement.bytes()).unwrap();
    assert!(approve(&ROOT_SEED, &journal, &doc, "m1", proved_before_revocation).is_err());
    assert!(approve(&ROOT_SEED, &journal, &doc, "m1", proof(&identity, &request)).is_err());
    let reincarnated = self::request(&identity, NETWORK, root(), 24);
    assert!(
        approve(
            &ROOT_SEED,
            &journal,
            &doc,
            "m1",
            proof(&identity, &reincarnated)
        )
        .is_err()
    );
    let evidence = Node::seal_restricted(
        &ROOT_SEED,
        NETWORK,
        Edit::Update {
            parent: retirement.id(),
            hosts: &[],
            payload: b"binding evidence",
        },
        &[identity.recipient()],
        0,
        &[crate::commons::constraints::Fact::Bind {
            binding: request.binding().clone(),
        }],
    )
    .unwrap();
    journal.ingest(evidence.bytes()).unwrap();
    for (returning, expected) in [(&identity, false), (&other, true)] {
        let seed = [61; 32];
        let fresh = Request::create(NETWORK, root(), [17; 16], [31; 16], returning, &seed).unwrap();
        let (mut pending, challenge) = Pending::issue(&fresh, NETWORK, root()).unwrap();
        let possession = pending
            .verify(&fresh.answer(returning, &seed, &challenge).unwrap())
            .unwrap();
        assert_eq!(
            approve(&ROOT_SEED, &journal, &document(returning), "m1", possession).is_ok(),
            expected,
            "a fresh writer cannot evade recipient retirement; unrelated eligible devices still work",
        );
    }
}

fn approval_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let identity = Identity::from_secret_bytes([19; 32]);
    let request = request(&identity, NETWORK, root(), 23);
    let mut doc = document(&identity);
    let genesis = genesis(&identity);
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut journal = journal(dir.path(), &genesis);
    if action != "IncompleteControl" {
        journal.ingest(genesis.bytes()).unwrap();
    }
    if action == "RetiredWriter" {
        let retired = Node::seal_restricted(
            &ROOT_SEED,
            NETWORK,
            Edit::Update {
                parent: genesis.id(),
                hosts: &[],
                payload: b"retired",
            },
            &[identity.recipient()],
            0,
            &[crate::commons::constraints::Fact::Retire {
                actor: request.binding().actor(),
            }],
        )
        .unwrap();
        journal.ingest(retired.bytes()).unwrap();
    }
    if action == "ForeignRecipient" {
        doc = document(&Identity::from_secret_bytes([29; 32]));
    }
    if action == "IneligibleRecipient" {
        doc = excluded_document(&identity);
    }
    let (scenario, seed) = match action {
        "Valid" => (1, ROOT_SEED),
        "ForeignAuthority" => (2, [31; 32]),
        "ForeignRecipient" => (3, ROOT_SEED),
        "RetiredWriter" => (4, ROOT_SEED),
        "IncompleteControl" => (5, ROOT_SEED),
        "IneligibleRecipient" => (6, ROOT_SEED),
        _ => panic!("unknown certification action"),
    };
    let granted = approve(&seed, &journal, &doc, "m1", proof(&identity, &request)).is_ok();
    let mut next = state.clone();
    next.insert("scenario", scenario);
    next.insert("approved", i64::from(granted));
    next
}

#[test]
fn a_valid_authority_signature_cannot_override_invalid_certificate_fields() {
    let identity = Identity::from_secret_bytes([19; 32]);
    let request = request(&identity, NETWORK, root(), 23);
    let doc = document(&identity);
    let genesis = genesis(&identity);
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut journal = journal(dir.path(), &genesis);
    journal.ingest(genesis.bytes()).unwrap();
    let certificate =
        approve(&ROOT_SEED, &journal, &doc, "m1", proof(&identity, &request)).unwrap();
    let signer = Ed25519KeyPair::from_seed_unchecked(&ROOT_SEED).unwrap();
    let mutations: [(usize, Vec<u8>); 7] = [
        (20, vec![9; 32]),
        (148, vec![0; 32]),
        (52, vec![0; 16]),
        (132, vec![0; 16]),
        (100, root().to_vec()),
        (68, vec![0; 32]),
        (99, vec![certificate.bytes()[99] | 0x80]),
    ];
    for (at, replacement) in mutations {
        let mut wire = certificate.bytes()[..CERTIFICATE_LEN - 64].to_vec();
        wire[at..at + replacement.len()].copy_from_slice(&replacement);
        let mut signed = DOMAIN.to_vec();
        signed.extend_from_slice(&wire);
        let signature = signer.sign(&signed);
        assert!(
            UnparsedPublicKey::new(&ED25519, root())
                .verify(&signed, signature.as_ref())
                .is_ok()
        );
        wire.extend_from_slice(signature.as_ref());
        assert!(
            Certificate::verify(&wire, NETWORK, root()).is_err(),
            "field at {at}"
        );
    }
}

#[test]
fn certified_admission_conforms_and_catches_bypassed_authority() {
    let model = aterm_models_db::member_certificate_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = approval_transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&next));
            }
        }
    }
    let state = [("scenario", 0), ("approved", 0)].into_iter().collect();
    let mut bypassed = approval_transition(&state, "ForeignAuthority");
    bypassed.insert("approved", 1);
    assert!(
        !model
            .successors("ForeignAuthority", &state)
            .contains(&bypassed)
    );
}
