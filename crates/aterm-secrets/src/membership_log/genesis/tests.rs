// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::age::Identity;
use crate::membership_log::{Inserted, Root};
use aterm_digest::Sha256;
use ring::signature::Ed25519KeyPair;

/// The synthetic stand-in for the ALab vault's seed: the key both of these tests' ALab
/// geneses are signed with.
const ALAB: [u8; 32] = [77; 32];
const OTHER: [u8; 32] = [78; 32];
const NETWORK: [u8; 16] = [2; 16];
const AUTHORITY: [u8; 32] = [3; 32];

/// One Mac, its keys all derived from its tag.
#[derive(Clone, Copy, Debug)]
struct Mac(u8);

const A: Mac = Mac(1);
const B: Mac = Mac(2);
const L: Mac = Mac(3);
const X: Mac = Mac(4);
const I: Mac = Mac(5);

impl Mac {
    fn identity(self) -> Identity {
        Identity::from_secret_bytes([self.0 ^ 0x55; 32])
    }
    fn seed(self) -> [u8; 32] {
        [self.0; 32]
    }
    fn incarnation(self) -> [u8; 16] {
        [self.0; 16]
    }
    fn request_under(self, network: [u8; 16], authority: [u8; 32]) -> Request {
        Request::create(
            network,
            authority,
            [self.0; 16],
            self.incarnation(),
            &self.identity(),
            &self.seed(),
        )
        .unwrap()
    }
    fn request(self) -> Request {
        self.request_under(NETWORK, AUTHORITY)
    }
    fn id(self) -> MemberId {
        self.request().binding().actor()
    }
    fn recipient(self) -> Recipient {
        self.identity().recipient()
    }
    /// The Mac as a genesis lists it: by recipient, with its binding when `bound`.
    fn founder(self, label: &str, bound: bool) -> Founder {
        Founder {
            recipient: self.recipient(),
            binding: bound.then(|| self.request().binding().clone()),
            node_key: None,
            name: format!("mac-{}", self.0),
            label: label.to_owned(),
        }
    }
}

fn root() -> Root {
    Root {
        network: NETWORK,
        authority: AUTHORITY,
    }
}

fn genesis(seed: &[u8; 32], founders: &[(Mac, &str, bool)], authored_ms: u64) -> Record {
    let founders = founders
        .iter()
        .map(|(mac, label, bound)| mac.founder(label, *bound))
        .collect();
    Record::genesis(seed, root(), founders, authored_ms).unwrap()
}

fn lineage() -> [u8; 32] {
    crate::envelope::signer_public(&ALAB).unwrap()
}

fn members(log: &Log) -> BTreeSet<MemberId> {
    log.membership().members().keys().copied().collect()
}

/// A device request for `recipient`, a key the forger holds no identity of, signed by `forger`'s
/// own writer key: what a Mac that copied a listed founder's public key can send.
fn forged_request(forger: Mac, recipient: Recipient) -> Request {
    let pair = Ed25519KeyPair::from_seed_unchecked(&forger.seed()).unwrap();
    let mut wire = b"ACB1".to_vec();
    wire.extend_from_slice(&NETWORK);
    wire.extend_from_slice(&AUTHORITY);
    wire.extend_from_slice(&[forger.0; 16]);
    wire.extend_from_slice(&recipient.0);
    wire.extend_from_slice(ring::signature::KeyPair::public_key(&pair).as_ref());
    wire.extend_from_slice(&forger.incarnation());
    let signed = [b"aterm/commons/device-request/1\0".as_slice(), &wire].concat();
    wire.extend_from_slice(pair.sign(&signed).as_ref());
    Request::decode(&wire).unwrap()
}

#[test]
fn a_genesis_whose_digest_differs_from_the_compiled_one_is_refused() {
    let founders = [(A, "m22", true), (L, "m40", false)];
    let alab = genesis(&ALAB, &founders, 1);
    let elsewhere = genesis(&OTHER, &founders, 1);
    let compiled = Some(alab.id());

    let log = Log::against(alab.clone(), compiled, &lineage()).unwrap();
    assert_eq!(log.network(), alab.id());
    let refused = Log::against(elsewhere.clone(), compiled, &lineage()).unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("digest is not the ALab genesis"),
        "{refused}"
    );

    // With no digest compiled the check is dormant: a genesis founds a log, as `Log::new` does.
    assert!(Log::against(elsewhere.clone(), None, &lineage()).is_ok());
    // This build's own check, whichever digest it compiles: never a test's genesis.
    assert_eq!(
        Log::alab(elsewhere).is_ok(),
        compiled_digest_is_none(),
        "Log::alab is dormant exactly while no digest is compiled"
    );
}

fn compiled_digest_is_none() -> bool {
    super::compiled().unwrap().is_none()
}

#[test]
fn a_second_genesis_signed_by_alab_signer_is_refused() {
    let first = genesis(&ALAB, &[(A, "m22", true), (L, "m40", false)], 1);
    // The ALab vault's key signs a second genesis: one more founder, and one with the same
    // founders made later. Each is another network.
    let more = genesis(
        &ALAB,
        &[(A, "m22", true), (L, "m40", false), (X, "m41", true)],
        1,
    );
    let later = genesis(&ALAB, &[(A, "m22", true), (L, "m40", false)], 2);
    for second in [more, later] {
        assert!(matches!(
            (first.body(), second.body()),
            (Body::Genesis { signer: a, .. }, Body::Genesis { signer: b, .. }) if a == b
        ));
        let refused = Log::against(second.clone(), Some(first.id()), &lineage()).unwrap_err();
        assert!(
            refused
                .to_string()
                .contains("a second genesis signed with the ALab vault's key"),
            "{refused}"
        );
        // Its records never enter ALab's log.
        let mut alab = Log::against(first.clone(), Some(first.id()), &lineage()).unwrap();
        let theirs = Log::new(second).unwrap();
        let name = theirs
            .draft(&A.id(), 3, Body::Name { name: "a".into() })
            .sign(&A.seed(), A.incarnation())
            .unwrap();
        assert!(alab.insert(name).is_err());
    }
}

#[test]
fn a_member_proves_its_device_key_to_any_member() {
    // A (m22, the admin) and B hold Commons bindings; L is in [machines] with none.
    let record = genesis(
        &ALAB,
        &[(A, "m22", true), (B, "m7", true), (L, "m40", false)],
        1,
    );
    let mut log = Log::against(record.clone(), Some(record.id()), &lineage()).unwrap();
    assert_eq!(members(&log), BTreeSet::from([A.id(), B.id()]));
    let waiting: Vec<Recipient> = log
        .membership()
        .unproved()
        .iter()
        .map(|f| f.recipient)
        .collect();
    assert_eq!(waiting, [L.recipient()]);

    // A Mac that copied L's public key into a request under its own writer key is challenged,
    // but cannot open the challenge, and no member lets it in on a guessed answer.
    let forged = forged_request(I, L.recipient());
    let (proof, challenge) = log.challenge(&forged, [5; 32], "impostor".into()).unwrap();
    assert!(
        I.request()
            .answer(&I.identity(), &I.seed(), &challenge)
            .is_err()
    );
    let pair = Ed25519KeyPair::from_seed_unchecked(&I.seed()).unwrap();
    let guess = [
        b"ACP1".as_slice(),
        &Sha256::digest(&challenge),
        pair.sign(b"a guess").as_ref(),
    ]
    .concat();
    assert!(proof.admit(&log, &guess, &B.id(), 4).is_err());
    assert_eq!(log.membership().unproved().len(), 1);

    // L proves its age identity and writer key to B, not to the admin: B lets it in under the
    // genesis's label, with no person step.
    let request = L.request();
    let (proof, challenge) = log
        .challenge(&request, [40; 32], "Andrews-MacBook-Air".into())
        .unwrap();
    let answer = request
        .answer(&L.identity(), &L.seed(), &challenge)
        .unwrap();
    let admit = proof
        .admit(&log, &answer, &B.id(), 5)
        .unwrap()
        .sign(&B.seed(), B.incarnation())
        .unwrap();
    assert!(matches!(
        admit.body(),
        Body::Admit {
            evidence: Evidence::Listed,
            ..
        }
    ));
    assert_eq!(log.insert(admit).unwrap(), Inserted::Held);
    let membership = log.membership();
    let member = membership.get(&L.id()).unwrap();
    assert_eq!(
        (member.label.as_str(), member.admitted_by, member.node_key),
        ("m40", Some(B.id()), Some([40; 32]))
    );
    assert!(membership.unproved().is_empty());
    // Once in, its key opens no second challenge, and a Mac the genesis does not list gets none.
    assert!(log.challenge(&request, [40; 32], "again".into()).is_err());
    assert!(log.challenge(&X.request(), [4; 32], "x".into()).is_err());

    // Only a member lets a listed founder in.
    let fresh = Log::new(record).unwrap();
    let (proof, challenge) = fresh.challenge(&request, [40; 32], "air".into()).unwrap();
    let answer = request
        .answer(&L.identity(), &L.seed(), &challenge)
        .unwrap();
    assert!(proof.admit(&fresh, &answer, &X.id(), 5).is_err());
}

#[test]
fn an_entrant_bound_under_another_commons_root_is_refused() {
    let record = genesis(&ALAB, &[(A, "m22", true), (L, "m40", false)], 1);
    let mut log = Log::new(record).unwrap();
    let admit = |entrant: Entrant, evidence: Evidence| {
        log.draft(&A.id(), 2, Body::Admit { entrant, evidence })
            .sign(&A.seed(), A.incarnation())
            .unwrap()
    };
    let entrant = |mac: Mac, request: Request, label: &str| Entrant {
        request,
        node_key: [mac.0; 32],
        name: format!("mac-{}", mac.0),
        label: label.to_owned(),
    };
    // A binding made for another lab's Commons, or under another authority, never enters.
    for (network, authority) in [([9; 16], AUTHORITY), (NETWORK, [9; 32])] {
        let elsewhere = admit(
            entrant(X, X.request_under(network, authority), "m41"),
            Evidence::Phrase,
        );
        assert!(log.clone().insert(elsewhere).is_err());
    }
    // A listed admission names a founder the genesis lists with no binding, under its label.
    let unlisted = admit(entrant(X, X.request(), "m41"), Evidence::Listed);
    assert!(log.clone().insert(unlisted).is_err());
    let relabelled = admit(entrant(L, L.request(), "m41"), Evidence::Listed);
    assert!(log.clone().insert(relabelled).is_err());
    let listed = admit(entrant(L, L.request(), "m40"), Evidence::Listed);
    assert_eq!(log.insert(listed).unwrap(), Inserted::Held);
    // A genesis binds its founders under its own root too.
    let mut stray = X.founder("m41", true);
    stray.binding = Some(X.request_under([9; 16], AUTHORITY).binding().clone());
    assert!(Record::genesis(&ALAB, root(), vec![A.founder("m22", true), stray], 1).is_err());
    // And at least one founder is bound, so a member can sign.
    assert!(Record::genesis(&ALAB, root(), vec![L.founder("m40", false)], 1).is_err());
}

#[test]
fn alab_founders_list_every_machine_with_its_binding_and_a_free_label_for_the_unlabelled() {
    let machines: Vec<Machine> = [(A, "m22"), (B, "studio"), (L, "air"), (X, "mini")]
        .iter()
        .map(|(mac, name)| Machine {
            name: (*name).to_owned(),
            recipient: mac.recipient(),
        })
        .collect();
    let bindings = [A.request().binding().clone(), B.request().binding().clone()];
    let labels = BTreeMap::from([
        (A.recipient(), "m22".to_owned()),
        (L.recipient(), "m40".to_owned()),
        (I.recipient(), "m41".to_owned()),
    ]);
    let founders = alab_founders(&machines, &bindings, &labels, 40).unwrap();
    let listed: Vec<(&str, bool, &str)> = founders
        .iter()
        .map(|f| (f.name.as_str(), f.binding.is_some(), f.label.as_str()))
        .collect();
    assert_eq!(
        listed,
        [
            ("m22", true, "m22"),
            ("studio", true, "m42"),
            ("air", false, "m40"),
            ("mini", false, "m43"),
        ]
    );
    let twice = [A.request().binding().clone(), A.request().binding().clone()];
    assert!(alab_founders(&machines, &twice, &labels, 40).is_err());
}

#[test]
fn a_removed_listed_founder_stays_out_whatever_its_own_admits_name() {
    // A and B hold Commons bindings; L is listed with none. Each case lets L in by the
    // possession challenge, takes it out, and then has L sign an `Admit` of its own actor naming
    // a key Q it made: before its removal (backdated), after it, and after a quorum removal.
    for case in ["backdated", "after the removal", "after a quorum removal"] {
        let record = genesis(
            &ALAB,
            &[(A, "m22", true), (B, "m7", true), (L, "m40", false)],
            1,
        );
        let mut store = crate::membership_log::Store::with_compiled(None);
        assert!(store.insert(record.clone()).unwrap());
        let log = |store: &crate::membership_log::Store| store.log().unwrap().clone();

        let request = L.request();
        let (proof, challenge) = log(&store)
            .challenge(&request, [40; 32], "air".into())
            .unwrap();
        let answer = request
            .answer(&L.identity(), &L.seed(), &challenge)
            .unwrap();
        let let_in = proof
            .admit(&log(&store), &answer, &B.id(), 2)
            .unwrap()
            .sign(&B.seed(), B.incarnation())
            .unwrap();
        assert!(store.insert(let_in.clone()).unwrap());
        assert!(log(&store).membership().contains(&L.id()));

        let removers: &[Mac] = if case == "after a quorum removal" {
            &[A, B]
        } else {
            &[A]
        };
        for (at, by) in (3..).zip(removers) {
            let body = Body::Remove {
                member: L.id(),
                cutoff: *let_in.id(),
            };
            let remove = log(&store)
                .draft(&by.id(), at, body)
                .sign(&by.seed(), by.incarnation())
                .unwrap();
            assert!(store.insert(remove).unwrap());
        }
        let membership = log(&store).membership();
        assert!(!membership.contains(&L.id()), "{case}");
        assert!(membership.unproved().is_empty(), "{case}");

        // L alone signs an `Admit` of itself naming Q: a request needs its writer key only.
        let q = Identity::from_secret_bytes([0x99; 32]);
        let other = Request::create(
            NETWORK,
            AUTHORITY,
            [L.0; 16],
            L.incarnation(),
            &q,
            &L.seed(),
        )
        .unwrap();
        assert_eq!(other.binding().actor(), L.id());
        let body = Body::Admit {
            entrant: Entrant {
                request: other,
                node_key: [40; 32],
                name: "air".into(),
                label: "m40".into(),
            },
            evidence: Evidence::Phrase,
        };
        let draft = if case == "backdated" {
            Draft {
                network: *record.id(),
                sequence: 1,
                previous: None,
                parents: vec![*let_in.id()],
                authored_ms: 5,
                body,
            }
        } else {
            log(&store).draft(&L.id(), 5, body)
        };
        let own = draft.sign(&L.seed(), L.incarnation()).unwrap();
        assert!(
            store.insert(own).unwrap(),
            "{case}: L has a seat, so it is held"
        );

        // L still holds the key it was let in with: no member challenges it again, under its
        // own incarnation or a new one, so no member lets it back in without a person.
        let membership = log(&store).membership();
        assert!(!membership.contains(&L.id()), "{case}");
        assert!(membership.unproved().is_empty(), "{case}");
        let reborn = Request::create(
            NETWORK,
            AUTHORITY,
            [L.0; 16],
            [0xee; 16],
            &L.identity(),
            &L.seed(),
        )
        .unwrap();
        for again in [L.request(), reborn] {
            assert!(
                log(&store)
                    .challenge(&again, [40; 32], "air".into())
                    .is_err(),
                "{case}: a member let L in or took it out"
            );
        }
    }
}

#[test]
fn a_removed_mac_naming_another_listed_founders_recipient_does_not_block_its_challenge() {
    // A and B hold Commons bindings; L and X are listed founders with no binding yet.
    // After B lets L in and L is taken out, L tries to squat X's recipient — via its own actor
    // (backdated, after plain removal, after quorum removal, and even before its plain removal's
    // cutoff) and via a backdated Sybil actor L admits and removes itself. In every case L stays
    // out while X remains unproved and joins cleanly through `Log::challenge`.
    for case in [
        "before plain removal cutoff",
        "sybil admit-and-remove before plain removal cutoff",
        "backdated self-admit",
        "after plain removal",
        "after quorum removal",
        "backdated sybil admit-and-remove",
    ] {
        let record = genesis(
            &ALAB,
            &[
                (A, "m22", true),
                (B, "m7", true),
                (L, "m40", false),
                (X, "m41", false),
            ],
            1,
        );
        let mut store = crate::membership_log::Store::with_compiled(None);
        assert!(store.insert(record.clone()).unwrap());
        let log = |store: &crate::membership_log::Store| store.log().unwrap().clone();

        let l_req = L.request();
        let (proof, challenge) = log(&store)
            .challenge(&l_req, [40; 32], "air".into())
            .unwrap();
        let answer = l_req.answer(&L.identity(), &L.seed(), &challenge).unwrap();
        let let_in = proof
            .admit(&log(&store), &answer, &B.id(), 2)
            .unwrap()
            .sign(&B.seed(), B.incarnation())
            .unwrap();
        assert!(store.insert(let_in.clone()).unwrap());

        let mut removal_cutoff = *let_in.id();
        if case == "before plain removal cutoff" {
            let squat = forged_request(L, X.recipient());
            let own = log(&store)
                .draft(
                    &L.id(),
                    3,
                    Body::Admit {
                        entrant: Entrant {
                            request: squat,
                            node_key: [40; 32],
                            name: "air".into(),
                            label: "m40".into(),
                        },
                        evidence: Evidence::Phrase,
                    },
                )
                .sign(&L.seed(), L.incarnation())
                .unwrap();
            removal_cutoff = *own.id();
            assert!(store.insert(own).unwrap());
        } else if case == "sybil admit-and-remove before plain removal cutoff" {
            let sybil = forged_request(I, X.recipient());
            let sybil_id = sybil.binding().actor();
            let admit_sybil = log(&store)
                .draft(
                    &L.id(),
                    3,
                    Body::Admit {
                        entrant: Entrant {
                            request: sybil,
                            node_key: [99; 32],
                            name: "sybil".into(),
                            label: "m99".into(),
                        },
                        evidence: Evidence::Phrase,
                    },
                )
                .sign(&L.seed(), L.incarnation())
                .unwrap();
            let admit_sybil_id = *admit_sybil.id();
            assert!(store.insert(admit_sybil).unwrap());
            let rem_sybil = log(&store)
                .draft(
                    &L.id(),
                    3,
                    Body::Remove {
                        member: sybil_id,
                        cutoff: admit_sybil_id,
                    },
                )
                .sign(&L.seed(), L.incarnation())
                .unwrap();
            removal_cutoff = *rem_sybil.id();
            assert!(store.insert(rem_sybil).unwrap());
        }

        let removers: &[Mac] = if case == "after quorum removal" {
            &[A, B]
        } else {
            &[A]
        };
        for (at, by) in (4..).zip(removers) {
            let remove = log(&store)
                .draft(
                    &by.id(),
                    at,
                    Body::Remove {
                        member: L.id(),
                        cutoff: removal_cutoff,
                    },
                )
                .sign(&by.seed(), by.incarnation())
                .unwrap();
            assert!(store.insert(remove).unwrap());
        }

        match case {
            "before plain removal cutoff"
            | "sybil admit-and-remove before plain removal cutoff" => {}
            "backdated sybil admit-and-remove" => {
                let sybil = forged_request(I, X.recipient());
                let sybil_id = sybil.binding().actor();
                let admit_sybil = Draft {
                    network: *record.id(),
                    sequence: 1,
                    previous: None,
                    parents: vec![*let_in.id()],
                    authored_ms: 6,
                    body: Body::Admit {
                        entrant: Entrant {
                            request: sybil,
                            node_key: [99; 32],
                            name: "sybil".into(),
                            label: "m99".into(),
                        },
                        evidence: Evidence::Phrase,
                    },
                }
                .sign(&L.seed(), L.incarnation())
                .unwrap();
                let admit_sybil_id = *admit_sybil.id();
                assert!(store.insert(admit_sybil).unwrap());
                let rem_sybil = Draft {
                    network: *record.id(),
                    sequence: 2,
                    previous: Some(admit_sybil_id),
                    parents: vec![admit_sybil_id],
                    authored_ms: 7,
                    body: Body::Remove {
                        member: sybil_id,
                        cutoff: admit_sybil_id,
                    },
                }
                .sign(&L.seed(), L.incarnation())
                .unwrap();
                assert!(store.insert(rem_sybil).unwrap());
            }
            _ => {
                let squat = forged_request(L, X.recipient());
                let body = Body::Admit {
                    entrant: Entrant {
                        request: squat,
                        node_key: [40; 32],
                        name: "air".into(),
                        label: "m40".into(),
                    },
                    evidence: Evidence::Phrase,
                };
                let draft = if case == "backdated self-admit" {
                    Draft {
                        network: *record.id(),
                        sequence: 1,
                        previous: None,
                        parents: vec![*let_in.id()],
                        authored_ms: 6,
                        body,
                    }
                } else {
                    log(&store).draft(&L.id(), 6, body)
                };
                let own = draft.sign(&L.seed(), L.incarnation()).unwrap();
                assert!(store.insert(own).unwrap());
            }
        }

        let cur = log(&store);
        let membership = cur.membership();
        assert!(!membership.contains(&L.id()), "{case}");
        assert!(
            cur.recipient_is_removed(&L.recipient()),
            "{case}: L's own recipient must stay removed"
        );
        assert!(
            !cur.recipient_is_removed(&X.recipient()),
            "{case}: X's recipient must not be marked removed"
        );
        assert_eq!(
            membership.removed_labels(),
            &BTreeSet::from(["m40".to_owned()]),
            "{case}: only L's bound label is removed"
        );
        let unproved: Vec<Recipient> = membership.unproved().iter().map(|f| f.recipient).collect();
        assert_eq!(unproved, [X.recipient()], "{case}: X must stay unproved");

        // X proves its possession challenge with no person Allow step, while L remains barred.
        let x_req = X.request();
        let (x_proof, x_challenge) = log(&store)
            .challenge(&x_req, [41; 32], "mini".into())
            .expect(case);
        let x_answer = x_req
            .answer(&X.identity(), &X.seed(), &x_challenge)
            .unwrap();
        let x_admit = x_proof
            .admit(&log(&store), &x_answer, &B.id(), 8)
            .unwrap()
            .sign(&B.seed(), B.incarnation())
            .unwrap();
        assert!(store.insert(x_admit).unwrap());
        let after = log(&store).membership();
        assert!(after.contains(&X.id()), "{case}");
        assert!(!after.contains(&L.id()), "{case}");
        assert!(after.unproved().is_empty(), "{case}");
    }
}
