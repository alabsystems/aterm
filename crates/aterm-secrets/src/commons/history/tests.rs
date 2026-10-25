// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::commons::{
    membership,
    node::{Edit, Node},
    roster::{
        self, Grant, Role,
        tests::{Fixture, NETWORK, ROOT, key, proof},
    },
};

fn access(f: &Fixture, n: usize) -> Access {
    roster::current(&f.journal, &f.identities[n])
        .unwrap()
        .unwrap()
}
fn admit(f: &mut Fixture, count: usize) {
    let grants = (0..count)
        .map(|n| {
            let certificate = membership::approve(
                &ROOT,
                &f.journal,
                &f.eligibility,
                &format!("m{}", n + 1),
                proof(&f.identities[n], &f.requests[n], &[n as u8 + 31; 32]),
            )
            .unwrap();
            Grant::certified(
                format!("m{}", n + 1),
                certificate,
                if n == 0 { Role::Writer } else { Role::Reader },
            )
        })
        .collect();
    let node = roster::rotate(&ROOT, &f.eligibility, &f.journal, grants).unwrap();
    f.journal.ingest(node.bytes()).unwrap();
}
fn issue(f: &Fixture, issuer: usize, recipient: usize, inherited: Option<&Handoff>) -> Handoff {
    Handoff::issue(
        &f.journal,
        &access(f, issuer),
        &f.identities[issuer],
        &[issuer as u8 + 31; 32],
        f.genesis.id(),
        f.requests[recipient].binding(),
        inherited,
    )
    .unwrap()
}
fn open(f: &Fixture, h: &Handoff, n: usize) -> Result<Historical> {
    h.open(&f.journal, &access(f, n), &f.identities[n])
}
fn resign(wire: &mut [u8], seed: &[u8; 32]) {
    let split = wire.len() - 64;
    let pair = Ed25519KeyPair::from_seed_unchecked(seed).unwrap();
    let sig = pair.sign(&[DOMAIN, &wire[..split]].concat());
    wire[split..].copy_from_slice(sig.as_ref());
}
fn altered_material(f: &Fixture, h: &Handoff, change: impl FnOnce(&mut [u8])) -> Handoff {
    let plain = age::decrypt(&f.identities[1..2], &h.wire[HEADER + 4..h.wire.len() - 64]).unwrap();
    let mut plain = Zeroizing::new(plain.as_slice().to_vec());
    change(&mut plain);
    let sealed = age::encrypt(&[f.identities[1].recipient()], &plain).unwrap();
    let mut wire = h.wire[..HEADER].to_vec();
    wire.extend_from_slice(&(sealed.len() as u32).to_le_bytes());
    wire.extend_from_slice(&sealed);
    wire.resize(wire.len() + 64, 0);
    resign(&mut wire, &[31; 32]);
    Handoff::decode(&wire).unwrap()
}

#[test]
fn new_members_read_original_roles_and_pass_history_on_without_the_original_member() {
    let mut f = Fixture::enrolled(3, 1);
    let expected = key(&access(&f, 0), &f.journal);
    admit(&mut f, 2);
    let h = issue(&f, 0, 1, None);
    assert!(
        access(&f, 1)
            .historical(&f.journal, &f.identities[1], f.genesis.id())
            .is_err()
    );
    let past = open(&f, &h, 1).unwrap();
    assert_eq!(past.members().len(), 1);
    assert_eq!(past.members()[0].role(), Role::Writer);
    assert_eq!(past.with_key(&f.journal, |key| *key).unwrap(), expected);
    admit(&mut f, 3);
    assert!(past.with_key(&f.journal, |_| ()).is_err(), "stale handle");
    let onward = issue(&f, 1, 2, Some(&h));
    let retired = roster::rotate_retiring(
        &ROOT,
        &f.eligibility,
        &f.journal,
        f.grants(3).into_iter().skip(1).collect(),
        &[f.requests[0].binding().actor()],
    )
    .unwrap();
    f.journal.ingest(retired.bytes()).unwrap();
    assert_eq!(
        open(&f, &h, 1)
            .unwrap()
            .with_key(&f.journal, |key| *key)
            .unwrap(),
        expected
    );
    assert_eq!(
        open(&f, &onward, 2)
            .unwrap()
            .with_key(&f.journal, |key| *key)
            .unwrap(),
        expected
    );
    assert_eq!(*h.issuer(), *f.requests[0].binding());
    assert_eq!(*onward.recipient(), *f.requests[2].binding());
}

#[test]
fn even_a_member_signed_handoff_cannot_substitute_keys_or_role_plaintext() {
    let mut f = Fixture::enrolled(2, 1);
    admit(&mut f, 2);
    let h = issue(&f, 0, 1, None);
    for index in [4, 36, 52] {
        let changed = altered_material(&f, &h, |plain| plain[index] ^= 1);
        assert!(open(&f, &changed, 1).is_err());
    }
    for index in [5, 85, 117, 149 + BINDING_LEN, HEADER - 1, h.wire.len() - 1] {
        let mut wire = h.wire.clone();
        wire[index] ^= 1;
        assert!(Handoff::decode(&wire).is_err());
    }
    for length in 0..h.wire.len() {
        assert!(Handoff::decode(&h.wire[..length]).is_err());
    }
    let mut huge = h.wire.clone();
    huge.resize(MAX_HANDOFF + 1, 0);
    assert!(Handoff::decode(&huge).is_err());
}

fn handoff_scenario(action: &str) -> bool {
    let mut f = Fixture::enrolled(3, 1);
    admit(&mut f, 2);
    let h = issue(&f, 0, 1, None);
    let observer = access(&f, 1);
    match action {
        "NewMember" => open(&f, &h, 1).is_ok(),
        "Rotated" => {
            admit(&mut f, 3);
            open(&f, &h, 1).is_ok()
        }
        "Onward" => {
            admit(&mut f, 3);
            open(&f, &issue(&f, 1, 2, Some(&h)), 2).is_ok()
        }
        "WrongRecipient" => {
            admit(&mut f, 3);
            open(&f, &h, 2).is_ok()
        }
        "WrongEpochKey" => open(&f, &altered_material(&f, &h, |p| p[52] ^= 1), 1).is_ok(),
        "WrongFileKey" => open(&f, &altered_material(&f, &h, |p| p[36] ^= 1), 1).is_ok(),
        "Stale" => {
            let past = open(&f, &h, 1).unwrap();
            admit(&mut f, 3);
            past.with_key(&f.journal, |_| ()).is_ok()
        }
        "RetiredRecipient" => {
            let a = access(&f, 1);
            let node = roster::rotate_retiring(
                &ROOT,
                &f.eligibility,
                &f.journal,
                f.grants(1),
                &[f.requests[1].binding().actor()],
            )
            .unwrap();
            f.journal.ingest(node.bytes()).unwrap();
            h.open(&f.journal, &a, &f.identities[1]).is_ok()
        }
        "Forked" | "DiscardedIssuance" => {
            // Fork at the genesis so the handoff's issuance node can be discarded.
            let dir = aterm_tempfile::tempdir().unwrap();
            let mut alternate = Journal::open(dir.path(), f.journal.trust()).unwrap();
            alternate.ingest(f.genesis.bytes()).unwrap();
            let grants = (0..2)
                .map(|n| {
                    let cert = membership::approve(
                        &ROOT,
                        &alternate,
                        &f.eligibility,
                        &format!("m{}", n + 1),
                        proof(&f.identities[n], &f.requests[n], &[n as u8 + 31; 32]),
                    )
                    .unwrap();
                    Grant::certified(format!("m{}", n + 1), cert, Role::Reader)
                })
                .collect();
            let right = roster::rotate(&ROOT, &f.eligibility, &alternate, grants).unwrap();
            f.journal.ingest(right.bytes()).unwrap();
            if action == "Forked" {
                return h.open(&f.journal, &observer, &f.identities[1]).is_ok();
            }
            let recovery = Node::seal(
                &ROOT,
                NETWORK,
                Edit::Recover {
                    heads: &[h.issued_under, right.id()],
                    selected: right.id(),
                },
                &[],
                0,
            )
            .unwrap();
            f.journal.ingest(recovery.bytes()).unwrap();
            admit(&mut f, 2);
            open(&f, &h, 1).is_ok()
        }
        "FutureSource" => future_source(),
        _ => panic!("unknown handoff scenario"),
    }
}

pub(crate) fn handoff_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let scenario = [
        "NewMember",
        "Rotated",
        "Onward",
        "WrongRecipient",
        "WrongEpochKey",
        "WrongFileKey",
        "Stale",
        "RetiredRecipient",
        "Forked",
        "DiscardedIssuance",
        "FutureSource",
    ]
    .iter()
    .position(|name| *name == action)
    .unwrap()
        + 1;
    let mut next = state.clone();
    next.insert("scenario", scenario as i64);
    next.insert("exposed", i64::from(handoff_scenario(action)));
    next
}

#[test]
fn handoffs_conform_and_wrong_epoch_key_negative_control_is_detected() {
    let model = aterm_models_db::history_handoff_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    let initial = model.init_state();
    for action in &model.actions {
        let actual = handoff_transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&actual),
            "{}: {actual:?}",
            action.name
        );
    }
    let mut bad = handoff_transition(&initial, "WrongEpochKey");
    bad.insert("exposed", 1);
    assert!(!model.successors("WrongEpochKey", &initial).contains(&bad));
    assert!(!model.check_invariant("OnlyCurrentRecipientReads", &bad));
}

#[test]
fn a_self_signed_outsider_and_a_discarded_source_cannot_supply_history() {
    let mut f = Fixture::enrolled(3, 1);
    admit(&mut f, 2);
    let h = issue(&f, 0, 1, None);
    let mut header = h.wire[..HEADER].to_vec();
    header[149..149 + BINDING_LEN].copy_from_slice(&f.requests[2].binding().bytes());
    let p = age::decrypt(&f.identities[1..2], &h.wire[HEADER + 4..h.wire.len() - 64]).unwrap();
    let mut plain = Zeroizing::new(p.as_slice().to_vec());
    plain[4..36].copy_from_slice(&context(&header));
    let outsider = forge_handoff(&f, header, &plain, &[33; 32]);
    assert!(open(&f, &outsider, 1).is_err());

    // Keep a valid typed source and its exact keys, then discard its branch
    // using an opaque sibling (no competing key commitment for this epoch).
    let source = *access(&f, 0).basis();
    let source_key = key(&access(&f, 0), &f.journal);
    let file_key = f
        .journal
        .node(&source)
        .unwrap()
        .history_file_key(&f.identities[0])
        .unwrap();
    let right = Node::seal(
        &ROOT,
        NETWORK,
        Edit::Update {
            parent: f.genesis.id(),
            hosts: &[],
            payload: b"opaque unselected payload",
        },
        &[f.identities[0].recipient()],
        0,
    )
    .unwrap();
    f.journal.ingest(right.bytes()).unwrap();
    let recovery = Node::seal(
        &ROOT,
        NETWORK,
        Edit::Recover {
            heads: &[source, right.id()],
            selected: right.id(),
        },
        &[],
        0,
    )
    .unwrap();
    f.journal.ingest(recovery.bytes()).unwrap();
    admit(&mut f, 2);
    let valid = issue(&f, 0, 1, None);
    let mut header = valid.wire[..HEADER].to_vec();
    header[85..117].copy_from_slice(&source);
    let mut plain = Zeroizing::new(Vec::with_capacity(PLAIN));
    plain.extend_from_slice(b"AHK1");
    plain.extend_from_slice(&context(&header));
    plain.extend_from_slice(file_key.as_ref());
    plain.extend_from_slice(&source_key);
    let discarded = forge_handoff(&f, header, &plain, &[31; 32]);
    assert!(open(&f, &discarded, 1).is_err());
}

fn forge_handoff(f: &Fixture, mut header: Vec<u8>, plain: &[u8], seed: &[u8; 32]) -> Handoff {
    let sealed = age::encrypt(&[f.identities[1].recipient()], plain).unwrap();
    header.extend_from_slice(&(sealed.len() as u32).to_le_bytes());
    header.extend_from_slice(&sealed);
    header.resize(header.len() + 64, 0);
    resign(&mut header, seed);
    Handoff::decode(&header).unwrap()
}

fn future_source() -> bool {
    let mut f = Fixture::enrolled(2, 2);
    admit(&mut f, 2);
    let source = *access(&f, 0).basis();
    let current = Handoff::issue(
        &f.journal,
        &access(&f, 0),
        &f.identities[0],
        &[31; 32],
        source,
        f.requests[1].binding(),
        None,
    )
    .unwrap();
    assert!(
        open(&f, &current, 1).is_ok(),
        "same-roster handoff is harmless"
    );
    let mut header = current.wire[..HEADER].to_vec();
    header[117..149].copy_from_slice(&f.genesis.id());
    let original = age::decrypt(
        &f.identities[1..2],
        &current.wire[HEADER + 4..current.wire.len() - 64],
    )
    .unwrap();
    let mut plain = Zeroizing::new(original.as_slice().to_vec());
    plain[4..36].copy_from_slice(&context(&header));
    let forged = forge_handoff(&f, header, &plain, &[31; 32]);
    open(&f, &forged, 1).is_ok()
}

#[test]
fn later_key_misuse_cannot_erase_authentic_older_evidence() {
    let mut f = Fixture::enrolled(2, 1);
    let original = key(&access(&f, 0), &f.journal);
    let reused = Node::seal_restricted(
        &ROOT,
        NETWORK,
        Edit::Update {
            parent: f.genesis.id(),
            hosts: &[],
            payload: b"invalid key reuse",
        },
        &[f.identities[0].recipient()],
        0,
        &[crate::commons::constraints::Fact::Epoch {
            generation: 2,
            commitment: crate::commons::constraints::key_commitment(NETWORK, &original),
        }],
    )
    .unwrap();
    f.journal.ingest(reused.bytes()).unwrap();
    assert!(roster::current(&f.journal, &f.identities[0]).is_err());
    admit(&mut f, 2);
    assert_ne!(key(&access(&f, 0), &f.journal), original);
    let handoff = issue(&f, 0, 1, None);
    assert_eq!(
        open(&f, &handoff, 1)
            .unwrap()
            .with_key(&f.journal, |key| *key)
            .unwrap(),
        original
    );
}
