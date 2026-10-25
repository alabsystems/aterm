// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
mod historical;
mod publication;
use crate::{
    commons::binding::{Pending, Request},
    text::{Audience, Document},
};

pub(crate) const ROOT: [u8; 32] = [11; 32];
pub(crate) const NETWORK: [u8; 16] = [7; 16];
pub(crate) struct Fixture {
    _directory: aterm_tempfile::TempDir,
    pub(crate) identities: Vec<Identity>,
    pub(crate) requests: Vec<Request>,
    pub(crate) eligibility: Eligibility,
    pub(crate) genesis: Node,
    pub(crate) journal: Journal,
}
pub(crate) fn proof(identity: &Identity, request: &Request, seed: &[u8; 32]) -> Possession {
    let (mut pending, challenge) =
        Pending::issue(request, NETWORK, authority(&ROOT).unwrap()).unwrap();
    pending
        .verify(&request.answer(identity, seed, &challenge).unwrap())
        .unwrap()
}
impl Fixture {
    fn new(count: usize) -> Self {
        Self::enrolled(count, count)
    }
    pub(crate) fn enrolled(count: usize, enrolled: usize) -> Self {
        let identities: Vec<_> = (0..count)
            .map(|n| Identity::from_secret_bytes([n as u8 + 19; 32]))
            .collect();
        let mut doc = Document::new_vault("m1", &identities[0].recipient().to_bech32());
        for (i, identity) in identities.iter().enumerate().skip(1) {
            doc.add_machine(&format!("m{}", i + 1), &identity.recipient().to_bech32())
                .unwrap();
        }
        doc.set_entry(
            &Audience::Hosts((1..=count).map(|n| format!("m{n}")).collect()),
            super::super::ENTRY,
            b"control",
        )
        .unwrap();
        let eligibility = Eligibility::from_document(&doc);
        let requests: Vec<_> = identities
            .iter()
            .enumerate()
            .map(|(n, identity)| {
                Request::create(
                    NETWORK,
                    authority(&ROOT).unwrap(),
                    [n as u8 + 41; 16],
                    [n as u8 + 47; 16],
                    identity,
                    &[n as u8 + 31; 32],
                )
                .unwrap()
            })
            .collect();
        let enrollments = identities
            .iter()
            .zip(&requests)
            .enumerate()
            .take(enrolled)
            .map(|(n, (identity, request))| {
                Enrollment::new(
                    format!("m{}", n + 1),
                    proof(identity, request, &[n as u8 + 31; 32]),
                    if n == 0 { Role::Writer } else { Role::Reader },
                )
            })
            .collect();
        let genesis = create(&ROOT, &eligibility, NETWORK, enrollments).unwrap();
        let directory = aterm_tempfile::tempdir().unwrap();
        let mut journal = Journal::open(
            directory.path(),
            Trust {
                network: NETWORK,
                authority: authority(&ROOT).unwrap(),
                genesis: genesis.id(),
            },
        )
        .unwrap();
        journal.ingest(genesis.bytes()).unwrap();
        Self {
            _directory: directory,
            identities,
            requests,
            eligibility,
            genesis,
            journal,
        }
    }
    pub(crate) fn grants(&self, count: usize) -> Vec<Grant> {
        self.requests
            .iter()
            .take(count)
            .enumerate()
            .map(|(n, request)| {
                Grant::recorded(
                    format!("m{}", n + 1),
                    request.binding().clone(),
                    if n == 0 { Role::Writer } else { Role::Reader },
                )
            })
            .collect()
    }
    fn raw(
        &self,
        parent: Option<[u8; 32]>,
        payload: &[u8],
        facts: &[Fact],
        recipients: &[age::Recipient],
    ) -> Node {
        let edit = match parent {
            Some(parent) => Edit::Update {
                parent,
                hosts: &[],
                payload,
            },
            None => Edit::Create {
                hosts: &[],
                payload,
            },
        };
        Node::seal_restricted(&ROOT, NETWORK, edit, recipients, 0, facts).unwrap()
    }
    fn payload(&self) -> SecretBytes {
        self.genesis.open(&self.identities[0]).unwrap()
    }
}
pub(crate) fn key(access: &Access, journal: &Journal) -> [u8; 32] {
    access.with_key(journal, |key| *key).unwrap()
}

#[test]
fn one_and_two_members_open_the_same_key_without_raw_key_in_the_control_payload() {
    for count in [1, 2] {
        let fixture = Fixture::new(count);
        let first = current(&fixture.journal, &fixture.identities[0])
            .unwrap()
            .unwrap();
        assert_eq!(first.generation(), 1);
        assert_eq!(first.local().role(), Role::Writer);
        assert_eq!(first.members().len(), count);
        let expected = key(&first, &fixture.journal);
        assert!(
            !fixture
                .payload()
                .as_slice()
                .windows(32)
                .any(|bytes| bytes == expected)
        );
        assert!(format!("{first:?}").contains("redacted"));
        for identity in &fixture.identities {
            let member = current(&fixture.journal, identity).unwrap().unwrap();
            assert_eq!(key(&member, &fixture.journal), expected);
            assert_eq!(member.local().binding().recipient(), identity.recipient());
        }
        if count == 2 {
            assert_eq!(
                current(&fixture.journal, &fixture.identities[1])
                    .unwrap()
                    .unwrap()
                    .local()
                    .role(),
                Role::Reader
            );
        }
    }
}

#[test]
fn an_extra_outer_recipient_cannot_open_any_network_key_capsule() {
    let fixture = Fixture::new(2);
    let outsider = Identity::from_secret_bytes([83; 32]);
    let payload = fixture.payload();
    let copied = fixture.raw(
        None,
        payload.as_slice(),
        fixture.genesis.facts(),
        &[
            fixture.identities[0].recipient(),
            fixture.identities[1].recipient(),
            outsider.recipient(),
        ],
    );
    let directory = aterm_tempfile::tempdir().unwrap();
    let mut journal = Journal::open(
        directory.path(),
        Trust {
            network: NETWORK,
            authority: authority(&ROOT).unwrap(),
            genesis: copied.id(),
        },
    )
    .unwrap();
    journal.ingest(copied.bytes()).unwrap();
    assert!(
        copied.open(&outsider).is_ok(),
        "outer control metadata is deliberately readable in this attack"
    );
    assert!(current(&journal, &outsider).is_err());
    let decoded = decode(payload.as_slice()).unwrap();
    for capsule in decoded.capsules {
        assert!(age::decrypt(std::slice::from_ref(&outsider), &capsule.wire).is_err());
    }
    assert!(current(&journal, &fixture.identities[0]).unwrap().is_some());
}

#[test]
fn rotation_is_explicit_and_stale_handles_refuse_key_use_after_append_and_restart() {
    let mut fixture = Fixture::new(2);
    let old = current(&fixture.journal, &fixture.identities[0])
        .unwrap()
        .unwrap();
    let old_key = key(&old, &fixture.journal);
    let prepared = rotate(
        &ROOT,
        &fixture.eligibility,
        &fixture.journal,
        fixture.grants(2),
    )
    .unwrap();
    assert_eq!(
        key(&old, &fixture.journal),
        old_key,
        "preparation has no journal effect"
    );
    fixture.journal.ingest(prepared.bytes()).unwrap();
    let mut called = false;
    assert!(
        old.with_key(&fixture.journal, |_| {
            called = true;
        })
        .is_err()
    );
    assert!(!called);
    let fresh = current(&fixture.journal, &fixture.identities[0])
        .unwrap()
        .unwrap();
    assert_eq!(fresh.generation(), 2);
    let fresh_key = key(&fresh, &fixture.journal);
    assert_ne!(old_key, fresh_key);
    assert_eq!(
        key(
            &current(&fixture.journal, &fixture.identities[1])
                .unwrap()
                .unwrap(),
            &fixture.journal
        ),
        fresh_key
    );
    let trust = fixture.journal.trust();
    drop(fixture.journal);
    let reopened = Journal::open(fixture._directory.path(), trust).unwrap();
    assert_eq!(
        key(
            &current(&reopened, &fixture.identities[0]).unwrap().unwrap(),
            &reopened
        ),
        fresh_key
    );
    assert!(old.with_key(&reopened, |_| ()).is_err());
}

#[test]
fn retirement_rejects_the_entire_unsafe_roster_and_a_fresh_rotation_excludes_the_device() {
    let mut fixture = Fixture::new(2);
    let retired = fixture.raw(
        Some(fixture.genesis.id()),
        b"pending rotation",
        &[Fact::Retire {
            actor: fixture.requests[1].binding().actor(),
        }],
        &[fixture.identities[0].recipient()],
    );
    fixture.journal.ingest(retired.bytes()).unwrap();
    assert!(
        rotate(
            &ROOT,
            &fixture.eligibility,
            &fixture.journal,
            fixture.grants(2)
        )
        .is_err()
    );
    // Even an authority-signed, internally consistent roster is unusable if one
    // member is retired. The new epoch itself observes the retirement.
    let members = fixture.grants(2).into_iter().map(|g| g.member).collect();
    let (unsafe_node, _) = prepare(&ROOT, NETWORK, 2, members, Some(retired.id()), &[]).unwrap();
    fixture.journal.ingest(unsafe_node.bytes()).unwrap();
    assert!(current(&fixture.journal, &fixture.identities[0]).is_err());
    let safe = rotate(
        &ROOT,
        &fixture.eligibility,
        &fixture.journal,
        fixture.grants(1),
    )
    .unwrap();
    fixture.journal.ingest(safe.bytes()).unwrap();
    assert_eq!(
        current(&fixture.journal, &fixture.identities[0])
            .unwrap()
            .unwrap()
            .generation(),
        3
    );
    assert!(current(&fixture.journal, &fixture.identities[1]).is_err());
}

#[test]
fn a_fork_and_recovery_to_an_old_roster_do_not_restore_an_old_key() {
    let mut fixture = Fixture::new(2);
    let old = current(&fixture.journal, &fixture.identities[0])
        .unwrap()
        .unwrap();
    let left = rotate(
        &ROOT,
        &fixture.eligibility,
        &fixture.journal,
        fixture.grants(2),
    )
    .unwrap();
    let right = rotate(
        &ROOT,
        &fixture.eligibility,
        &fixture.journal,
        fixture.grants(1),
    )
    .unwrap();
    fixture.journal.ingest(left.bytes()).unwrap();
    fixture.journal.ingest(right.bytes()).unwrap();
    assert!(
        current(&fixture.journal, &fixture.identities[0])
            .unwrap()
            .is_none()
    );
    assert!(old.with_key(&fixture.journal, |_| ()).is_err());
    let recovery = Node::seal(
        &ROOT,
        NETWORK,
        Edit::Recover {
            heads: &[left.id(), right.id()],
            selected: left.id(),
        },
        &[],
        0,
    )
    .unwrap();
    fixture.journal.ingest(recovery.bytes()).unwrap();
    assert!(
        current(&fixture.journal, &fixture.identities[0]).is_err(),
        "equal-epoch different-key evidence survives selection"
    );
    let repaired = rotate(
        &ROOT,
        &fixture.eligibility,
        &fixture.journal,
        fixture.grants(2),
    )
    .unwrap();
    fixture.journal.ingest(repaired.bytes()).unwrap();
    assert_eq!(
        current(&fixture.journal, &fixture.identities[0])
            .unwrap()
            .unwrap()
            .generation(),
        3
    );
}

#[test]
fn later_epoch_evidence_cannot_supply_a_missing_fact_on_the_selected_basis() {
    let fixture = Fixture::new(1);
    let payload = fixture.payload();
    let decoded = decode(payload.as_slice()).unwrap();
    let binding_facts: Vec<_> = fixture
        .genesis
        .facts()
        .iter()
        .filter(|fact| matches!(fact, Fact::Bind { .. }))
        .cloned()
        .collect();
    let faulty = fixture.raw(
        None,
        payload.as_slice(),
        &binding_facts,
        &[fixture.identities[0].recipient()],
    );
    let left = fixture.raw(
        Some(faulty.id()),
        payload.as_slice(),
        &binding_facts,
        &[fixture.identities[0].recipient()],
    );
    let right = fixture.raw(
        Some(faulty.id()),
        b"epoch evidence",
        &[Fact::Epoch {
            generation: 1,
            commitment: decoded.commitment,
        }],
        &[fixture.identities[0].recipient()],
    );
    let recovery = Node::seal(
        &ROOT,
        NETWORK,
        Edit::Recover {
            heads: &[left.id(), right.id()],
            selected: left.id(),
        },
        &[],
        0,
    )
    .unwrap();
    let directory = aterm_tempfile::tempdir().unwrap();
    let mut journal = Journal::open(
        directory.path(),
        Trust {
            network: NETWORK,
            authority: authority(&ROOT).unwrap(),
            genesis: faulty.id(),
        },
    )
    .unwrap();
    for node in [&faulty, &left, &right, &recovery] {
        journal.ingest(node.bytes()).unwrap();
    }
    assert!(matches!(
        journal.constraints().unwrap().epoch(),
        super::super::constraints::Epoch::Unique { .. }
    ));
    assert!(current(&journal, &fixture.identities[0]).is_err());
}

#[test]
fn capsule_context_and_canonical_roster_checks_reject_authenticated_malformed_payloads() {
    let fixture = Fixture::new(2);
    let payload = fixture.payload();
    for mutation in 0..7 {
        let mut roster = decode(payload.as_slice()).unwrap();
        match mutation {
            0 => roster.capsules.swap(0, 1),
            1 => roster.capsules[0].member.role = Role::Reader,
            2 => roster.generation += 1,
            3 => roster.commitment[0] ^= 1,
            4 => roster.capsules[0].wire[100] ^= 1,
            5 => roster.capsules[1].member.binding = roster.capsules[0].member.binding.clone(),
            6 => {
                roster.capsules[0].member.binding = Request::create(
                    NETWORK,
                    authority(&ROOT).unwrap(),
                    [103; 16],
                    [47; 16],
                    &fixture.identities[0],
                    &[31; 32],
                )
                .unwrap()
                .binding()
                .clone()
            }
            _ => unreachable!(),
        }
        let modified = encode(&roster);
        let forged = fixture.raw(
            None,
            modified.as_slice(),
            &facts(&roster).into_iter().collect::<Vec<_>>(),
            &[
                fixture.identities[0].recipient(),
                fixture.identities[1].recipient(),
            ],
        );
        let directory = aterm_tempfile::tempdir().unwrap();
        let mut journal = Journal::open(
            directory.path(),
            Trust {
                network: NETWORK,
                authority: authority(&ROOT).unwrap(),
                genesis: forged.id(),
            },
        )
        .unwrap();
        journal.ingest(forged.bytes()).unwrap();
        assert!(
            current(&journal, &fixture.identities[0]).is_err(),
            "malformed roster {mutation}"
        );
    }
    for end in [0, 1, 4, HEADER - 1, HEADER, payload.len() - 1] {
        assert!(decode(&payload.as_slice()[..end]).is_err());
    }
    let mut bytes = payload.as_slice().to_vec();
    bytes.extend_from_slice(b"trailing");
    assert!(decode(&bytes).is_err());
    let mut bytes = payload.as_slice().to_vec();
    bytes[HEADER..HEADER + BINDING_LEN].fill(0);
    assert!(decode(&bytes).is_err());
    let mut bytes = payload.as_slice().to_vec();
    bytes[HEADER + BINDING_LEN] = 2;
    assert!(decode(&bytes).is_err());
    let mut bytes = payload.as_slice().to_vec();
    bytes[HEADER - 2..HEADER].copy_from_slice(&64u16.to_le_bytes());
    assert!(decode(&bytes).is_err());
}

#[test]
fn creation_and_rotation_require_eligible_proofs_or_recorded_bindings() {
    let fixture = Fixture::new(1);
    let outsider = Identity::from_secret_bytes([83; 32]);
    let request = Request::create(
        NETWORK,
        authority(&ROOT).unwrap(),
        [89; 16],
        [97; 16],
        &outsider,
        &[101; 32],
    )
    .unwrap();
    let possession = proof(&outsider, &request, &[101; 32]);
    assert!(
        create(
            &ROOT,
            &fixture.eligibility,
            NETWORK,
            vec![Enrollment::new("m1", possession, Role::Writer)]
        )
        .is_err()
    );
    assert!(create(&ROOT, &fixture.eligibility, NETWORK, vec![]).is_err());
    assert!(
        rotate(
            &[17; 32],
            &fixture.eligibility,
            &fixture.journal,
            fixture.grants(1)
        )
        .is_err()
    );
    assert!(
        rotate(
            &ROOT,
            &fixture.eligibility,
            &fixture.journal,
            vec![Grant::recorded(
                "m1",
                request.binding().clone(),
                Role::Writer
            )]
        )
        .is_err()
    );
    let mut duplicated = fixture.grants(1);
    duplicated.extend(fixture.grants(1));
    assert!(rotate(&ROOT, &fixture.eligibility, &fixture.journal, duplicated).is_err());
    assert!(rotate(&ROOT, &fixture.eligibility, &fixture.journal, vec![]).is_err());
}

#[test]
fn retirement_and_rotation_are_atomic_and_cannot_include_a_retired_member() {
    let mut fixture = Fixture::new(2);
    let retired = fixture.requests[1].binding().actor();
    let before = fixture.journal.state();
    assert!(
        rotate_retiring(
            &ROOT,
            &fixture.eligibility,
            &fixture.journal,
            fixture.grants(2),
            &[retired]
        )
        .is_err()
    );
    assert_eq!(fixture.journal.state(), before);
    let node = rotate_retiring(
        &ROOT,
        &fixture.eligibility,
        &fixture.journal,
        fixture.grants(1),
        &[retired],
    )
    .unwrap();
    assert!(node.facts().contains(&Fact::Retire { actor: retired }));
    assert!(
        current(&fixture.journal, &fixture.identities[1])
            .unwrap()
            .is_some()
    );
    fixture.journal.ingest(node.bytes()).unwrap();
    assert_eq!(
        current(&fixture.journal, &fixture.identities[0])
            .unwrap()
            .unwrap()
            .generation(),
        2
    );
    assert!(current(&fixture.journal, &fixture.identities[1]).is_err());
}

#[test]
fn cascaded_recovery_cannot_restore_a_causally_superseded_payload() {
    let mut fixture = Fixture::new(2);
    let n1 = rotate(
        &ROOT,
        &fixture.eligibility,
        &fixture.journal,
        fixture.grants(2),
    )
    .unwrap();
    let n2 = fixture.raw(
        Some(fixture.genesis.id()),
        b"other policy",
        &[],
        &[fixture.identities[0].recipient()],
    );
    let r1 = Node::seal(
        &ROOT,
        NETWORK,
        Edit::Recover {
            heads: &[n1.id(), n2.id()],
            selected: n1.id(),
        },
        &[],
        0,
    )
    .unwrap();
    for node in [&n1, &n2, &r1] {
        fixture.journal.ingest(node.bytes()).unwrap();
    }
    assert!(
        current(&fixture.journal, &fixture.identities[0])
            .unwrap()
            .is_some()
    );
    let n3 = fixture.raw(
        Some(n1.id()),
        b"later policy",
        &[],
        &[fixture.identities[0].recipient()],
    );
    let r2 = Node::seal(
        &ROOT,
        NETWORK,
        Edit::Recover {
            heads: &[r1.id(), n3.id()],
            selected: r1.id(),
        },
        &[],
        0,
    )
    .unwrap();
    fixture.journal.ingest(n3.bytes()).unwrap();
    fixture.journal.ingest(r2.bytes()).unwrap();
    assert!(matches!(fixture.journal.state(), State::Ready { basis, .. } if basis == n1.id()));
    assert!(current(&fixture.journal, &fixture.identities[0]).is_err());
    let repaired = rotate(
        &ROOT,
        &fixture.eligibility,
        &fixture.journal,
        fixture.grants(2),
    )
    .unwrap();
    fixture.journal.ingest(repaired.bytes()).unwrap();
    assert_eq!(
        current(&fixture.journal, &fixture.identities[0])
            .unwrap()
            .unwrap()
            .generation(),
        3
    );
}

#[test]
fn one_key_epoch_cannot_be_republished_under_another_node() {
    let mut fixture = Fixture::new(1);
    fixture.journal.ingest(fixture.genesis.bytes()).unwrap();
    assert!(
        current(&fixture.journal, &fixture.identities[0])
            .unwrap()
            .is_some()
    );
    let payload = fixture.payload();
    let repeated = fixture.raw(
        Some(fixture.genesis.id()),
        payload.as_slice(),
        fixture.genesis.facts(),
        &[fixture.identities[0].recipient()],
    );
    fixture.journal.ingest(repeated.bytes()).unwrap();
    assert!(current(&fixture.journal, &fixture.identities[0]).is_err());
    let rotated = rotate(
        &ROOT,
        &fixture.eligibility,
        &fixture.journal,
        fixture.grants(1),
    )
    .unwrap();
    fixture.journal.ingest(rotated.bytes()).unwrap();
    assert_eq!(
        current(&fixture.journal, &fixture.identities[0])
            .unwrap()
            .unwrap()
            .generation(),
        2
    );
}

#[test]
fn a_new_writer_requires_approval_from_this_history_and_known_members_can_stay_offline() {
    let mut fixture = Fixture::new(2);
    let seed = [101; 32];
    let request = Request::create(
        NETWORK,
        authority(&ROOT).unwrap(),
        [107; 16],
        [109; 16],
        &fixture.identities[1],
        &seed,
    )
    .unwrap();
    let unapproved = vec![
        fixture.grants(1).remove(0),
        Grant::recorded("m2", request.binding().clone(), Role::Reader),
    ];
    assert!(rotate(&ROOT, &fixture.eligibility, &fixture.journal, unapproved).is_err());
    let unrelated = Fixture::new(2);
    let foreign = super::super::membership::approve(
        &ROOT,
        &unrelated.journal,
        &fixture.eligibility,
        "m2",
        proof(&fixture.identities[1], &request, &seed),
    )
    .unwrap();
    let grants = vec![
        fixture.grants(1).remove(0),
        Grant::certified("m2", foreign, Role::Reader),
    ];
    assert!(rotate(&ROOT, &fixture.eligibility, &fixture.journal, grants).is_err());
    let approved = super::super::membership::approve(
        &ROOT,
        &fixture.journal,
        &fixture.eligibility,
        "m2",
        proof(&fixture.identities[1], &request, &seed),
    )
    .unwrap();
    let grants = vec![
        fixture.grants(1).remove(0),
        Grant::certified("m2", approved, Role::Reader),
    ];
    let next = rotate(&ROOT, &fixture.eligibility, &fixture.journal, grants).unwrap();
    fixture.journal.ingest(next.bytes()).unwrap();
    let access = current(&fixture.journal, &fixture.identities[1])
        .unwrap()
        .unwrap();
    assert_eq!(access.local().binding(), request.binding());
    assert_eq!(access.generation(), 2);
}

#[test]
fn member_and_epoch_bounds_refuse_growth_without_changing_the_current_journal() {
    let mut fixture = Fixture::new(MAX_MEMBERS);
    let access = current(&fixture.journal, &fixture.identities[0])
        .unwrap()
        .unwrap();
    assert_eq!(access.members().len(), MAX_MEMBERS);
    let too_many = (0..=MAX_MEMBERS)
        .map(|_| {
            Enrollment::new(
                "m1",
                proof(&fixture.identities[0], &fixture.requests[0], &[31; 32]),
                Role::Writer,
            )
        })
        .collect();
    assert!(create(&ROOT, &fixture.eligibility, NETWORK, too_many).is_err());
    let retired = fixture.requests.last().unwrap().binding().actor();
    let atomic = rotate_retiring(
        &ROOT,
        &fixture.eligibility,
        &fixture.journal,
        fixture.grants(MAX_MEMBERS - 1),
        &[retired],
    )
    .unwrap();
    assert_eq!(atomic.facts().len(), super::super::constraints::MAX_FACTS);
    fixture.journal.ingest(atomic.bytes()).unwrap();
    assert_eq!(
        current(&fixture.journal, &fixture.identities[0])
            .unwrap()
            .unwrap()
            .members()
            .len(),
        MAX_MEMBERS - 1
    );
    let mut fixture = Fixture::new(1);
    let exhausted = fixture.raw(
        Some(fixture.genesis.id()),
        b"exhausted",
        &[Fact::Epoch {
            generation: u64::MAX,
            commitment: [83; 32],
        }],
        &[fixture.identities[0].recipient()],
    );
    fixture.journal.ingest(exhausted.bytes()).unwrap();
    let before = fixture.journal.state();
    assert!(
        rotate(
            &ROOT,
            &fixture.eligibility,
            &fixture.journal,
            fixture.grants(1)
        )
        .is_err()
    );
    assert_eq!(fixture.journal.state(), before);
}

fn exposed(fixture: &Fixture, identity: &Identity) -> bool {
    current(&fixture.journal, identity)
        .ok()
        .flatten()
        .is_some_and(|access| access.with_key(&fixture.journal, |_| ()).is_ok())
}
fn access_transition(state: &aterm_spec::interp::State, action: &str) -> aterm_spec::interp::State {
    let mut fixture = Fixture::new(2);
    let (scenario, allowed) = match action {
        "Valid" => (1, exposed(&fixture, &fixture.identities[0])),
        "StaleHandle" => {
            let old = current(&fixture.journal, &fixture.identities[0])
                .unwrap()
                .unwrap();
            let next = rotate(
                &ROOT,
                &fixture.eligibility,
                &fixture.journal,
                fixture.grants(2),
            )
            .unwrap();
            fixture.journal.ingest(next.bytes()).unwrap();
            (3, old.with_key(&fixture.journal, |_| ()).is_ok())
        }
        "RetiredMember" => {
            let retired = fixture.raw(
                Some(fixture.genesis.id()),
                b"retired",
                &[Fact::Retire {
                    actor: fixture.requests[1].binding().actor(),
                }],
                &[fixture.identities[0].recipient()],
            );
            fixture.journal.ingest(retired.bytes()).unwrap();
            let members = fixture.grants(2).into_iter().map(|g| g.member).collect();
            let (unsafe_node, _) =
                prepare(&ROOT, NETWORK, 2, members, Some(retired.id()), &[]).unwrap();
            fixture.journal.ingest(unsafe_node.bytes()).unwrap();
            (2, exposed(&fixture, &fixture.identities[0]))
        }
        "MissingBasisFact" => {
            let facts: Vec<_> = fixture
                .genesis
                .facts()
                .iter()
                .filter(|f| matches!(f, Fact::Bind { .. }))
                .cloned()
                .collect();
            let payload = fixture.payload();
            let missing = fixture.raw(
                Some(fixture.genesis.id()),
                payload.as_slice(),
                &facts,
                &[fixture.identities[0].recipient()],
            );
            fixture.journal.ingest(missing.bytes()).unwrap();
            (4, exposed(&fixture, &fixture.identities[0]))
        }
        "WrongCapsule" | "DuplicateMember" => {
            let mut roster = decode(fixture.payload().as_slice()).unwrap();
            if action == "WrongCapsule" {
                roster.capsules[0].wire[100] ^= 1;
            } else {
                roster.capsules[1].member.binding = roster.capsules[0].member.binding.clone();
            }
            let payload = encode(&roster);
            let node = fixture.raw(
                Some(fixture.genesis.id()),
                payload.as_slice(),
                &facts(&roster).into_iter().collect::<Vec<_>>(),
                &[fixture.identities[0].recipient()],
            );
            fixture.journal.ingest(node.bytes()).unwrap();
            (
                if action == "WrongCapsule" { 5 } else { 9 },
                exposed(&fixture, &fixture.identities[0]),
            )
        }
        "ForeignRecipient" => {
            let outsider = Identity::from_secret_bytes([83; 32]);
            let payload = fixture.payload();
            let node = fixture.raw(
                Some(fixture.genesis.id()),
                payload.as_slice(),
                fixture.genesis.facts(),
                &[fixture.identities[0].recipient(), outsider.recipient()],
            );
            fixture.journal.ingest(node.bytes()).unwrap();
            assert!(node.open(&outsider).is_ok());
            (6, exposed(&fixture, &outsider))
        }
        "ForkedHistory" => {
            let left = rotate(
                &ROOT,
                &fixture.eligibility,
                &fixture.journal,
                fixture.grants(2),
            )
            .unwrap();
            let right = rotate(
                &ROOT,
                &fixture.eligibility,
                &fixture.journal,
                fixture.grants(2),
            )
            .unwrap();
            fixture.journal.ingest(left.bytes()).unwrap();
            fixture.journal.ingest(right.bytes()).unwrap();
            (7, exposed(&fixture, &fixture.identities[0]))
        }
        "OldEpoch" => {
            let payload = fixture.payload();
            let left = fixture.raw(
                Some(fixture.genesis.id()),
                payload.as_slice(),
                fixture.genesis.facts(),
                &[fixture.identities[0].recipient()],
            );
            let right = fixture.raw(
                Some(fixture.genesis.id()),
                b"new epoch",
                &[Fact::Epoch {
                    generation: 2,
                    commitment: [89; 32],
                }],
                &[fixture.identities[0].recipient()],
            );
            let recovery = Node::seal(
                &ROOT,
                NETWORK,
                Edit::Recover {
                    heads: &[left.id(), right.id()],
                    selected: left.id(),
                },
                &[],
                0,
            )
            .unwrap();
            for node in [&left, &right, &recovery] {
                fixture.journal.ingest(node.bytes()).unwrap();
            }
            (8, exposed(&fixture, &fixture.identities[0]))
        }
        "RepeatedEpoch" => {
            let payload = fixture.payload();
            let repeated = fixture.raw(
                Some(fixture.genesis.id()),
                payload.as_slice(),
                fixture.genesis.facts(),
                &[fixture.identities[0].recipient()],
            );
            fixture.journal.ingest(repeated.bytes()).unwrap();
            (11, exposed(&fixture, &fixture.identities[0]))
        }
        "CascadedRecovery" => {
            let n1 = rotate(
                &ROOT,
                &fixture.eligibility,
                &fixture.journal,
                fixture.grants(2),
            )
            .unwrap();
            let n2 = fixture.raw(
                Some(fixture.genesis.id()),
                b"other policy",
                &[],
                &[fixture.identities[0].recipient()],
            );
            let n3 = fixture.raw(
                Some(n1.id()),
                b"later policy",
                &[],
                &[fixture.identities[0].recipient()],
            );
            let r1 = Node::seal(
                &ROOT,
                NETWORK,
                Edit::Recover {
                    heads: &[n1.id(), n2.id()],
                    selected: n1.id(),
                },
                &[],
                0,
            )
            .unwrap();
            let r2 = Node::seal(
                &ROOT,
                NETWORK,
                Edit::Recover {
                    heads: &[r1.id(), n3.id()],
                    selected: r1.id(),
                },
                &[],
                0,
            )
            .unwrap();
            for node in [&n1, &n2, &n3, &r1, &r2] {
                fixture.journal.ingest(node.bytes()).unwrap();
            }
            assert!(
                matches!(fixture.journal.state(), State::Ready { basis, .. } if basis == n1.id())
            );
            (10, exposed(&fixture, &fixture.identities[0]))
        }
        _ => panic!("unknown roster transition"),
    };
    let mut next = state.clone();
    next.insert("scenario", scenario);
    next.insert("exposed", i64::from(allowed));
    next
}

#[test]
fn real_roster_key_release_conforms_and_bypassing_retirement_is_detected() {
    let model = aterm_models_db::roster_access_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = access_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let initial = model.init_state();
    let mut unsafe_release = access_transition(&initial, "RetiredMember");
    unsafe_release.insert("exposed", 1);
    assert!(
        !model
            .successors("RetiredMember", &initial)
            .contains(&unsafe_release)
    );
    assert!(!model.check_invariant("OnlyValidatedRosterReleases", &unsafe_release));
}
