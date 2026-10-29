// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::{
    client::{self, Paths},
    commons::{
        self,
        binding::Pending,
        bootstrap::{Descriptor, Device},
        roster::{Enrollment, Grant},
    },
    text::Audience,
};
use std::{path::PathBuf, time::Duration};

const NETWORK: [u8; 16] = [9; 16];

pub(crate) fn repair_commons(lab: &Lab, rounds: usize) {
    let members: Vec<usize> = (0..lab.paths.len()).collect();
    repair_commons_with(lab, rounds, &members, None);
}

/// [`repair_commons`] with each Mac's ring windows placed by its own clock, `now[n]` seconds.
pub(crate) fn repair_commons_at(lab: &Lab, rounds: usize, now: &[u64]) {
    let members: Vec<usize> = (0..lab.paths.len()).collect();
    repair_commons_among(lab, rounds, &members, now);
}

/// [`repair_commons_at`] among `members` alone (the first exchanges with each other), as one
/// side of a split.
pub(crate) fn repair_commons_among(lab: &Lab, rounds: usize, members: &[usize], now: &[u64]) {
    assert_eq!(now.len(), lab.paths.len(), "one clock per Mac");
    repair_commons_with(lab, rounds, members, Some(now));
}

fn repair_commons_with(lab: &Lab, rounds: usize, members: &[usize], now: Option<&[u64]>) {
    use crate::control::{Endpoint, Repair, Side};
    use aterm_net::lan::{Connection, Scope as LinkScope};
    let endpoints: Vec<_> = members
        .iter()
        .map(|&n| {
            let endpoint = Endpoint::from_bootstrap(&lab.bootstrap(n))
                .unwrap()
                .unwrap()
                .with_commons(lab.paths[n].clone(), lab.paths[n].home.join("config.toml"))
                .unwrap();
            match now {
                Some(now) => endpoint.with_ring_clock(now[n]),
                None => endpoint,
            }
        })
        .collect();
    let mut repairs: Vec<_> = (0..endpoints.len()).map(|_| Repair::default()).collect();
    for _ in 0..rounds {
        for peer in 1..endpoints.len() {
            let (head, tail) = repairs.split_at_mut(peer);
            let left = &mut head[0];
            let right = &mut tail[0];
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            std::thread::scope(|tasks| {
                let incoming = tasks.spawn(|| {
                    endpoints[peer].exchange(
                        Connection::admit(listener.accept().unwrap().0, LinkScope::Loopback)
                            .unwrap(),
                        Side::Responder,
                        right,
                    )
                });
                endpoints[0]
                    .exchange(
                        Connection::connect(address, LinkScope::Loopback, Duration::from_secs(60))
                            .unwrap(),
                        Side::Initiator,
                        left,
                    )
                    .unwrap();
                incoming.join().unwrap().unwrap();
            });
        }
    }
}

#[test]
fn a_public_foundation_never_requests_a_historical_epoch_key() {
    let lab = Lab::with_foundation(&[Role::Writer, Role::Writer]);
    for _ in 0..2 {
        for n in 0..2 {
            let bootstrap = lab.bootstrap(n);
            let session = Session::open(&bootstrap).unwrap();
            let (needs, next) = session.history_needs(None, 16).unwrap();
            assert!(needs.is_empty());
            assert!(next.is_some());
            assert_ne!(next, Some(bootstrap.trust().genesis));
            assert!(session.history_needs(next, 16).unwrap().0.is_empty());
        }
        lab.rotate(&[Role::Writer, Role::Writer]);
    }
}

#[test]
fn negative_only_ciphertext_never_promotes_malformed_payloads() {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let scope = session.scope().unwrap();
    let mut wire = session
        .seal(
            scope,
            Causal {
                sequence: 1,
                previous: None,
                parents: &[],
            },
            b"metadata",
        )
        .unwrap();
    assert!(session.negative_record(scope, &wire).unwrap().is_some());
    let last_payload = wire.len() - 65;
    wire[last_payload] ^= 1;
    resign(&bootstrap, &mut wire);
    assert!(session.authenticate(scope, &wire).is_err());
    assert!(session.negative_record(scope, &wire).unwrap().is_none());
    let foreign = Scope::new(scope.trust(), *scope.table(), 2).unwrap();
    assert!(session.negative_record(foreign, &wire).is_err());
}

pub(crate) fn seal(
    session: &Session<'_>,
    scope: Scope,
    causal: Causal<'_>,
    payload: &[u8],
) -> Result<Vec<u8>> {
    session.seal(scope, causal, payload)
}

pub(crate) struct Lab {
    pub(crate) _home: aterm_tempfile::TempDir,
    pub(crate) paths: Vec<Paths>,
    pub(crate) signer: String,
    source: PathBuf,
}
impl Lab {
    pub(crate) fn new(roles: &[Role]) -> Self {
        Self::with_enrolled(roles, roles.len())
    }
    pub(crate) fn with_enrolled(roles: &[Role], enrolled: usize) -> Self {
        Self::build(roles, enrolled, false)
    }
    pub(crate) fn with_foundation(roles: &[Role]) -> Self {
        Self::build(roles, roles.len(), true)
    }
    fn build(roles: &[Role], enrolled: usize, foundation: bool) -> Self {
        let home = aterm_tempfile::tempdir().unwrap();
        let source = home.path().join("test.vault");
        let paths: Vec<_> = (0..roles.len())
            .map(|n| Paths::for_home(&home.path().join(format!("m{}", n + 1))))
            .collect();
        let created =
            client::test_create_unbound(&paths[0], source.to_str().unwrap(), None, Some("m1"))
                .unwrap();
        let admin = client::admin_open(&paths[0]).unwrap();
        let mut document = admin.doc.clone();
        for (n, path) in paths.iter().enumerate().skip(1) {
            let identity = client::machine_identity(path).unwrap();
            document
                .add_machine(&format!("m{}", n + 1), &identity.recipient().to_bech32())
                .unwrap();
        }
        let hosts = || (1..=roles.len()).map(|n| format!("m{n}")).collect();
        let anchor = foundation
            .then(|| commons::node::Node::foundation(admin.commons_authority().unwrap()).unwrap());
        let network = anchor.as_ref().map_or(NETWORK, |node| node.network());
        let initial = anchor.map_or_else(
            || b"pending".to_vec(),
            |node| Descriptor::from_genesis(node).unwrap().bytes(),
        );
        document
            .set_entry(&Audience::Hosts(hosts()), commons::ENTRY, &initial)
            .unwrap();
        client::admin_publish(&paths[0], &admin, &document).unwrap();
        let admin = client::admin_open(&paths[0]).unwrap();
        let proofs: Vec<_> = paths
            .iter()
            .zip(roles)
            .enumerate()
            .take(enrolled)
            .map(|(n, (path, role))| {
                let device = Device::open(path).unwrap().unwrap();
                let request = device
                    .request(network, admin.commons_authority().unwrap())
                    .unwrap();
                let (mut pending, challenge) =
                    Pending::issue(&request, network, *request.binding().authority()).unwrap();
                let proof = pending
                    .verify(&device.answer(&request, &challenge).unwrap())
                    .unwrap();
                (format!("m{}", n + 1), proof, *role)
            })
            .collect();
        let descriptor = if foundation {
            let bootstrap = Bootstrap::cached(&paths[0]).unwrap().unwrap();
            let mut journal =
                Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust()).unwrap();
            let grants = proofs
                .into_iter()
                .map(|(host, proof, role)| {
                    let certificate = admin
                        .approve_commons_member(&journal, &host, proof)
                        .unwrap();
                    Grant::certified(host, certificate, role)
                })
                .collect();
            let first = admin.prepare_commons_rotation(&journal, grants).unwrap();
            journal.ingest(first.bytes()).unwrap();
            Descriptor::from_journal(&journal).unwrap()
        } else {
            let enrollments = proofs
                .into_iter()
                .map(|(host, proof, role)| Enrollment::new(host, proof, role))
                .collect();
            Descriptor::from_genesis(admin.prepare_commons_genesis(network, enrollments).unwrap())
                .unwrap()
        };
        document
            .set_entry(
                &Audience::Hosts(hosts()),
                commons::ENTRY,
                &descriptor.bytes(),
            )
            .unwrap();
        client::admin_publish(&paths[0], &admin, &document).unwrap();
        for (n, path) in paths.iter().enumerate().skip(1) {
            client::join(
                path,
                &client::JoinOptions {
                    source: format!(
                        "{}#{}",
                        source.display(),
                        aterm_secrets::b64::encode_padded(&admin.config.signer.unwrap())
                    ),
                    host: Some(format!("m{}", n + 1)),
                    interactive: false,
                    wait: Duration::ZERO,
                    confirm: &|_| false,
                    password: aterm_secrets::client::JoinPassword::Given(None),
                    fetched: None,
                    requested: &|| {},
                    say: &|_| {},
                },
            )
            .unwrap();
        }
        Self {
            _home: home,
            paths,
            signer: created.signer,
            source,
        }
    }
    pub(crate) fn bootstrap(&self, n: usize) -> Bootstrap {
        Bootstrap::cached(&self.paths[n]).unwrap().unwrap()
    }
    pub(crate) fn admit(&self, roles: &[Role]) {
        let admin = client::admin_open(&self.paths[0]).unwrap();
        let bootstraps: Vec<_> = (0..self.paths.len()).map(|n| self.bootstrap(n)).collect();
        let mut journal =
            Journal::open_existing(bootstraps[0].journal_dir(), bootstraps[0].trust()).unwrap();
        let grants = roles
            .iter()
            .enumerate()
            .map(|(n, role)| {
                let device = Device::open(&self.paths[n]).unwrap().unwrap();
                let request = bootstraps[n].request().unwrap();
                let (mut pending, challenge) = Pending::issue(
                    &request,
                    *request.binding().network(),
                    *request.binding().authority(),
                )
                .unwrap();
                let possession = pending
                    .verify(&device.answer(&request, &challenge).unwrap())
                    .unwrap();
                let certificate = admin
                    .approve_commons_member(&journal, &format!("m{}", n + 1), possession)
                    .unwrap();
                Grant::certified(format!("m{}", n + 1), certificate, *role)
            })
            .collect();
        let node = admin.prepare_commons_rotation(&journal, grants).unwrap();
        journal.ingest(node.bytes()).unwrap();
        drop(journal);
        for b in bootstraps.iter().skip(1) {
            Journal::open_existing(b.journal_dir(), b.trust())
                .unwrap()
                .ingest(node.bytes())
                .unwrap();
        }
    }
    pub(crate) fn rotate(&self, roles: &[Role]) {
        let admin = client::admin_open(&self.paths[0]).unwrap();
        let bootstraps: Vec<_> = (0..self.paths.len()).map(|n| self.bootstrap(n)).collect();
        let mut journal =
            Journal::open_existing(bootstraps[0].journal_dir(), bootstraps[0].trust()).unwrap();
        let grants = bootstraps
            .iter()
            .zip(roles)
            .enumerate()
            .map(|(n, (b, role))| {
                Grant::recorded(
                    format!("m{}", n + 1),
                    b.request().unwrap().binding().clone(),
                    *role,
                )
            })
            .collect();
        let node = admin.prepare_commons_rotation(&journal, grants).unwrap();
        journal.ingest(node.bytes()).unwrap();
        drop(journal);
        for b in bootstraps.iter().skip(1) {
            Journal::open_existing(b.journal_dir(), b.trust())
                .unwrap()
                .ingest(node.bytes())
                .unwrap();
        }
    }
}

fn causal() -> Causal<'static> {
    Causal {
        sequence: 1,
        previous: None,
        parents: &[],
    }
}

#[test]
fn one_or_two_existing_members_read_signed_ciphertext_without_plaintext_keys() {
    for count in [1, 2] {
        let lab = Lab::new(&vec![Role::Writer; count]);
        let a = lab.bootstrap(0);
        let session = Session::open(&a).unwrap();
        let scope = session.scope().unwrap();
        let wire = session
            .seal_at(scope, causal(), b"private-account-metadata", u64::MAX)
            .unwrap();
        assert!(
            !wire
                .windows(24)
                .any(|bytes| bytes == b"private-account-metadata")
        );
        let other_wire = session
            .seal_at(scope, causal(), b"private-account-metadata", u64::MAX)
            .unwrap();
        assert_ne!(
            wire, other_wire,
            "a restored sequence cannot reuse the record subkey"
        );
        assert!(
            Session::open(&a).is_err(),
            "control observation is locked during the local batch"
        );
        drop(session);
        for n in 0..count {
            let bootstrap = lab.bootstrap(n);
            let session = Session::open(&bootstrap).unwrap();
            let record = session.authenticate(scope, &wire).unwrap();
            assert_eq!(record.payload(), b"private-account-metadata");
            assert_eq!(record.authored_ms(), u64::MAX);
            assert_eq!(
                record.header().actor,
                a.request().unwrap().binding().actor()
            );
            assert_eq!(record.header().id, Sha256::digest(&wire));
            assert_eq!(record.epoch(), 1);
            assert_eq!(record.scope(), scope);
            assert!(!lab.paths[n].dir.join("database.json").exists());
        }
        assert!(lab.source.exists());
    }
}

#[test]
fn readers_can_authenticate_but_cannot_write_even_with_a_copied_header() {
    let lab = Lab::new(&[Role::Writer, Role::Reader]);
    let a = lab.bootstrap(0);
    let b = lab.bootstrap(1);
    let sa = Session::open(&a).unwrap();
    let sb = Session::open(&b).unwrap();
    let scope = sa.scope().unwrap();
    let wire = sa.seal(scope, causal(), b"account").unwrap();
    assert!(sb.authenticate(scope, &wire).is_ok());
    assert!(sb.seal(scope, causal(), b"forged").is_err());
    assert!(sb.scope().is_err());
    let mut forgery = wire;
    let at = 5 + SCOPE_LEN + 32 + 8;
    forgery[at..at + 48].copy_from_slice(&sb.actor());
    resign(&b, &mut forgery);
    assert!(
        sa.authenticate(scope, &forgery)
            .err()
            .unwrap()
            .0
            .contains("historical writer")
    );
}

pub(crate) fn resign(bootstrap: &Bootstrap, wire: &mut [u8]) {
    let end = wire.len() - 64;
    let signature = bootstrap
        .device()
        .with_seed(|seed| crypto::signed(seed, DOMAIN, &wire[..end]))
        .unwrap();
    wire[end..].copy_from_slice(&signature);
}

#[test]
fn exact_context_epoch_and_causal_shape_are_authenticated() {
    let lab = Lab::new(&[Role::Writer]);
    let a = lab.bootstrap(0);
    let session = Session::open(&a).unwrap();
    let scope = session.scope().unwrap();
    let wire = session.seal(scope, causal(), b"account").unwrap();
    for alternative in [
        Scope::fresh(a.trust()).unwrap(),
        Scope::new(a.trust(), *scope.table(), 2).unwrap(),
    ] {
        assert!(session.authenticate(alternative, &wire).is_err());
    }
    let mut trust = a.trust();
    trust.genesis[0] ^= 1;
    assert!(
        session
            .authenticate(Scope::new(trust, *scope.table(), 1).unwrap(), &wire)
            .is_err()
    );
    for at in [
        0,
        5,
        5 + SCOPE_LEN,
        5 + SCOPE_LEN + 32,
        wire.len() - 65,
        wire.len() - 1,
    ] {
        let mut tampered = wire.clone();
        tampered[at] ^= 1;
        assert!(session.authenticate(scope, &tampered).is_err());
    }
    for end in [0, 4, 5, 100, wire.len() - 1] {
        assert!(session.authenticate(scope, &wire[..end]).is_err());
    }
    let mut extended = wire.clone();
    extended.push(0);
    assert!(session.authenticate(scope, &extended).is_err());
    let mut epoch = wire.clone();
    epoch[5 + SCOPE_LEN + 32] = 2;
    resign(&a, &mut epoch);
    assert!(session.authenticate(scope, &epoch).is_err());
    let mut bad_chain = wire.clone();
    let sequence = 5 + SCOPE_LEN + 32 + 8 + 48;
    bad_chain[sequence] = 2;
    resign(&a, &mut bad_chain);
    assert!(session.authenticate(scope, &bad_chain).is_err());
    assert!(
        session
            .seal(
                scope,
                Causal {
                    sequence: 0,
                    ..causal()
                },
                b"bad"
            )
            .is_err()
    );
    assert!(
        session
            .seal(
                scope,
                Causal {
                    sequence: 2,
                    previous: Some([0; 32]),
                    parents: &[[0; 32]]
                },
                b"bad"
            )
            .is_err()
    );
    assert!(
        session
            .seal(scope, causal(), &vec![0; MAX_BODY + 1])
            .is_err()
    );
    let maximum = session.seal(scope, causal(), &vec![7; MAX_BODY]).unwrap();
    assert_eq!(
        session
            .authenticate(scope, &maximum)
            .unwrap()
            .payload()
            .len(),
        MAX_BODY
    );
}

#[test]
fn rotation_preserves_old_read_evidence_and_demotion_does_not_grant_writes() {
    let lab = Lab::new(&[Role::Writer, Role::Reader]);
    let a = lab.bootstrap(0);
    let session = Session::open(&a).unwrap();
    let scope = session.scope().unwrap();
    let old = session.seal(scope, causal(), b"old-epoch").unwrap();
    drop(session);
    lab.rotate(&[Role::Writer, Role::Reader]);
    let session = Session::open(&a).unwrap();
    assert_eq!(session.epoch(), 2);
    assert_eq!(
        session.authenticate(scope, &old).unwrap().payload(),
        b"old-epoch"
    );
    let new = session.seal(scope, causal(), b"new-epoch").unwrap();
    assert_eq!(session.authenticate(scope, &new).unwrap().epoch(), 2);
    drop(session);
    lab.rotate(&[Role::Reader, Role::Reader]);
    let session = Session::open(&a).unwrap();
    assert!(
        session.authenticate(scope, &old).is_ok(),
        "past authorship is evidence, not a current write grant"
    );
    assert!(session.seal(scope, causal(), b"denied").is_err());
}

#[test]
fn admitted_session_reuses_the_fresh_control_lock_and_refuses_foreign_custody() {
    use std::time::Instant;

    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let (bootstrap, journal) = Bootstrap::cached_with_journal_until(
        &lab.paths[0],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
    .unwrap();
    let expected = bootstrap.request().unwrap().binding().actor();
    let error = Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust())
        .err()
        .expect("the admitted journal retains the actual exclusive writer lock");
    assert_eq!(error.kind(), aterm_secrets::ErrorKind::JournalBusy);
    let session = Session::from_admitted_journal(&bootstrap, journal)
        .expect("authorizing the held journal must not reopen its lock");
    assert_eq!(session.actor(), expected);
    assert_eq!(session.epoch(), 1);
    drop(session);
    let journal = Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust()).unwrap();
    drop(journal);

    let read_only = Journal::read_existing(bootstrap.journal_dir(), bootstrap.trust()).unwrap();
    assert!(
        Session::from_admitted_journal(&bootstrap, read_only).is_err(),
        "a cached read view cannot impersonate the held writer admission"
    );

    let other = lab.bootstrap(1);
    assert_eq!(other.trust(), bootstrap.trust());
    let foreign = Journal::open_existing(other.journal_dir(), other.trust()).unwrap();
    let error = Session::from_admitted_journal(&bootstrap, foreign)
        .err()
        .expect("equal network trust does not identify local custody");
    assert!(
        error.to_string().contains("another custody directory"),
        "{error}"
    );
}

#[test]
fn admitted_session_uses_revocation_in_the_held_history() {
    use std::time::Instant;

    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let retained = lab.bootstrap(1);
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let (bootstrap, mut journal) = Bootstrap::cached_with_journal_until(
        &lab.paths[0],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
    .unwrap();
    let revocation = admin
        .prepare_commons_rotation(
            &journal,
            vec![Grant::recorded(
                "m2",
                retained.request().unwrap().binding().clone(),
                Role::Writer,
            )],
        )
        .unwrap();
    journal.ingest(revocation.bytes()).unwrap();
    let error = Session::from_admitted_journal(&bootstrap, journal)
        .err()
        .expect("bootstrap custody cannot preserve a revoked application grant");
    assert!(
        error.to_string().contains("could not be decrypted"),
        "{error}"
    );
}

#[test]
fn admitted_session_and_open_revalidate_source_identity_and_device_custody() {
    use std::time::Instant;

    for admitted in [false, true] {
        for changed in ["source.toml", "identity", "commons/device.age"] {
            let lab = Lab::new(&[Role::Writer]);
            let (bootstrap, journal) = Bootstrap::cached_with_journal_until(
                &lab.paths[0],
                Instant::now() + Duration::from_secs(60),
            )
            .unwrap()
            .unwrap();
            let journal = if admitted {
                Some(journal)
            } else {
                drop(journal);
                None
            };
            std::fs::remove_file(lab.paths[0].dir.join(changed)).unwrap();
            let result = match journal {
                Some(journal) => Session::from_admitted_journal(&bootstrap, journal),
                None => Session::open(&bootstrap),
            };
            let error = result
                .err()
                .expect("no admission path can authorize changed source or custody");
            assert!(error.to_string().contains("custody"), "{error}");
        }
    }
}

#[test]
fn waiting_session_replays_revocation_before_authorizing_the_batch() {
    use std::time::Instant;
    use std::{cell::Cell, rc::Rc};

    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let revoked = lab.bootstrap(0);
    let retained = lab.bootstrap(1);
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let owner = Journal::open_existing(revoked.journal_dir(), revoked.trust()).unwrap();
    let revocation = admin
        .prepare_commons_rotation(
            &owner,
            vec![Grant::recorded(
                "m2",
                retained.request().unwrap().binding().clone(),
                Role::Writer,
            )],
        )
        .unwrap();
    let waited = Rc::new(Cell::new(0));
    let observed = waited.clone();
    let mut owner = Some(owner);
    let error = aterm_uds::private_file::with_lock_wait_observer(
        move |_| {
            observed.set(observed.get() + 1);
            owner.as_mut().unwrap().ingest(revocation.bytes()).unwrap();
            drop(owner.take());
        },
        || {
            Session::open_until(&revoked, Instant::now() + Duration::from_secs(60))
                .err()
                .expect("the newly revoked device has no current table access")
        },
    );
    assert_eq!(
        waited.get(),
        1,
        "revocation was appended during actual contention"
    );
    assert!(
        error.to_string().contains("could not be decrypted"),
        "{error}"
    );
}

#[test]
fn waiting_session_revalidates_source_identity_and_device_custody() {
    use std::{cell::Cell, rc::Rc, time::Instant};

    for changed in ["source.toml", "identity", "commons/device.age"] {
        let lab = Lab::new(&[Role::Writer]);
        let bootstrap = lab.bootstrap(0);
        let mut owner =
            Some(Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust()).unwrap());
        let changed = lab.paths[0].dir.join(changed);
        let waited = Rc::new(Cell::new(0));
        let observed = waited.clone();
        let error = aterm_uds::private_file::with_lock_wait_observer(
            move |_| {
                observed.set(observed.get() + 1);
                std::fs::remove_file(&changed).unwrap();
                drop(owner.take());
            },
            || {
                Session::open_until(&bootstrap, Instant::now() + Duration::from_secs(60))
                    .err()
                    .expect("changed custody cannot authorize a waited batch")
            },
        );
        assert_eq!(waited.get(), 1);
        assert!(error.to_string().contains("custody"), "{error}");
    }
}

fn envelope_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let lab = Lab::new(&[Role::Writer, Role::Reader]);
    let a = lab.bootstrap(0);
    let b = lab.bootstrap(1);
    let mut session = Session::open(&a).unwrap();
    let mut scope = session.scope().unwrap();
    let mut wire = session.seal(scope, causal(), b"account").unwrap();
    let scenario = match action {
        "Valid" => 1,
        "UnknownTable" => {
            scope = session.scope().unwrap();
            2
        }
        "ForgedReader" => {
            let at = 5 + SCOPE_LEN + 32 + 8;
            wire[at..at + 48].copy_from_slice(&b.request().unwrap().binding().actor());
            resign(&b, &mut wire);
            3
        }
        "WrongEpoch" => {
            wire[5 + SCOPE_LEN + 32] = 2;
            resign(&a, &mut wire);
            4
        }
        "Tampered" => {
            let end = wire.len() - 65;
            wire[end] ^= 1;
            5
        }
        "Malformed" => {
            wire[5 + SCOPE_LEN + 32 + 8 + 48] = 2;
            resign(&a, &mut wire);
            6
        }
        "ReaderWrites" => 7,
        "OldEpoch" => {
            drop(session);
            lab.rotate(&[Role::Writer, Role::Reader]);
            session = Session::open(&a).unwrap();
            8
        }
        _ => panic!("unknown envelope action"),
    };
    let accepted = if action == "ReaderWrites" {
        Session::open(&b)
            .unwrap()
            .seal(scope, causal(), b"reader write")
            .is_ok()
    } else {
        session.authenticate(scope, &wire).is_ok()
    };
    let mut next = state.clone();
    next.insert("scenario", scenario);
    next.insert("accepted", i64::from(accepted));
    next
}

#[test]
fn signed_envelope_conforms_and_reader_forgery_mutant_is_detected() {
    let model = aterm_models_db::table_envelope_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    let initial = model.init_state();
    for action in &model.actions {
        let actual = envelope_transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&actual),
            "{}: {actual:?}",
            action.name
        );
    }
    let mut forged = initial.clone();
    forged.insert("scenario", 3);
    forged.insert("accepted", 1);
    assert!(!model.successors("ForgedReader", &initial).contains(&forged));
    assert!(!model.check_invariant("ExactWriterEvidenceOnly", &forged));
}

mod storage;

#[test]
fn newly_admitted_machines_replay_durable_tables_and_relay_historical_access_offline() {
    let lab = Lab::with_enrolled(&[Role::Writer, Role::Reader, Role::Reader], 1);
    let a = lab.bootstrap(0);
    let session = Session::open(&a).unwrap();
    let dir = lab._home.path().join("table-a");
    let mut original = store::Store::create(&session, &dir).unwrap();
    let scope = original.scope();
    let receipt = original
        .commit_catalog(&session, b"old-account-metadata")
        .unwrap();
    let wire = original
        .wire(&session, &receipt.id)
        .unwrap()
        .unwrap()
        .to_vec();
    let source = *session.authenticate(scope, &wire).unwrap().roster();
    drop(original);
    drop(session);
    lab.admit(&[Role::Writer, Role::Reader]);
    let b = lab.bootstrap(1);
    assert!(
        Session::open(&b)
            .unwrap()
            .authenticate(scope, &wire)
            .is_err()
    );
    let journal = Journal::open_existing(a.journal_dir(), a.trust()).unwrap();
    let access = a.access_in(&journal).unwrap().unwrap();
    let handoff = a
        .history_handoff(&journal, &access, source, b.request().unwrap().binding())
        .unwrap();
    drop(journal);
    let journal = Journal::open_existing(b.journal_dir(), b.trust()).unwrap();
    let access = b.access_in(&journal).unwrap().unwrap();
    b.import_history(&journal, &access, handoff.bytes())
        .unwrap();
    let key = b
        .historical(&journal, &access, source)
        .unwrap()
        .with_key(&journal, |key| *key)
        .unwrap();
    b.import_history(&journal, &access, handoff.bytes())
        .unwrap();
    drop(journal);
    let history_dir = b.journal_dir().parent().unwrap().join("history");
    let entries: Vec<_> = std::fs::read_dir(&history_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(entries.len(), 1);
    let cached = std::fs::read(&entries[0]).unwrap();
    assert_eq!(cached, handoff.bytes());
    assert!(!cached.windows(32).any(|w| w == key));
    let restarted = lab.bootstrap(1);
    let session = Session::open(&restarted).unwrap();
    let replica_dir = lab._home.path().join("table-b");
    let mut replica = store::Store::join(&session, &replica_dir, scope).unwrap();
    replica.ingest(&session, &wire).unwrap();
    assert_eq!(
        replica.catalog(&session).unwrap()[0].payload(),
        b"old-account-metadata"
    );
    drop(replica);
    drop(session);
    lab.admit(&[Role::Writer, Role::Reader, Role::Reader]);
    let c = lab.bootstrap(2);
    let journal = Journal::open_existing(a.journal_dir(), a.trust()).unwrap();
    let access = a.access_in(&journal).unwrap().unwrap();
    let newer = a
        .history_handoff(&journal, &access, source, b.request().unwrap().binding())
        .unwrap();
    let to_self = a
        .history_handoff(&journal, &access, source, a.request().unwrap().binding())
        .unwrap();
    a.import_history(&journal, &access, to_self.bytes())
        .unwrap();
    assert!(!a.journal_dir().parent().unwrap().join("history").exists());
    drop(journal);
    let journal = Journal::open_existing(b.journal_dir(), b.trust()).unwrap();
    let access = b.access_in(&journal).unwrap().unwrap();
    let retained = b.import_history(&journal, &access, newer.bytes()).unwrap();
    assert_eq!(
        retained.id(),
        handoff.id(),
        "retain earlier usable issuance"
    );
    let onward = b
        .history_handoff(&journal, &access, source, c.request().unwrap().binding())
        .unwrap();
    drop(journal);
    let journal = Journal::open_existing(c.journal_dir(), c.trust()).unwrap();
    let access = c.access_in(&journal).unwrap().unwrap();
    c.import_history(&journal, &access, onward.bytes()).unwrap();
    drop(journal);
    assert_eq!(
        Session::open(&c)
            .unwrap()
            .authenticate(scope, &wire)
            .unwrap()
            .payload(),
        b"old-account-metadata"
    );
    let session = Session::open(&b).unwrap();
    assert_eq!(
        store::Store::open(&session, &replica_dir, scope)
            .unwrap()
            .catalog(&session)
            .unwrap()
            .len(),
        1
    );
    drop(session);
    // Cache loss/corruption cannot silently infer old roles or obtain an old key.
    let mut corrupt = cached.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    std::fs::write(&entries[0], &corrupt).unwrap();
    assert!(
        Session::open(&b)
            .unwrap()
            .authenticate(scope, &wire)
            .is_err()
    );
    std::fs::remove_file(&entries[0]).unwrap();
    assert!(
        Session::open(&b)
            .unwrap()
            .authenticate(scope, &wire)
            .is_err()
    );
}
