// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::{client, commons::roster::Grant, text::Audience};
use std::time::Duration;

#[test]
fn account_targets_wait_for_a_writer_instead_of_admitting_a_denied_peer() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let paths = &lab.paths[0];
    let mut roster = client::admin_seed_swarm_roster(paths, &[]).unwrap();
    roster.deny_labels.push("m2".to_owned());
    let admin = client::admin_open(paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Everyone,
        client::SWARM_ROSTER_ENTRY,
        roster.to_toml().as_bytes(),
    )
    .unwrap();
    client::admin_publish(paths, &admin, &doc).unwrap();
    client::sync(paths, None).unwrap();
    let bootstrap = lab.bootstrap(0);
    let peer = lab.bootstrap(1).request().unwrap().binding().actor();
    let session = Session::observe(&bootstrap).unwrap();
    let writer = std::rc::Rc::new(std::cell::RefCell::new(Some(
        client::test_hold_state_lock(paths).unwrap(),
    )));
    let old = client::test_without_roster_wait(|| session.observed_account_targets()).unwrap();
    assert!(
        old.iter().any(|binding| binding.actor() == peer),
        "negative control admits the denied peer"
    );
    let release = writer.clone();
    let targets = client::test_with_denied_nodes_busy(
        move || drop(release.borrow_mut().take()),
        || session.observed_account_targets(),
    )
    .unwrap();
    assert!(!targets.iter().any(|binding| binding.actor() == peer));
    assert!(
        writer.borrow().is_none(),
        "the check observed the writer before release"
    );
}

pub(crate) use super::fixture::{
    Lab, NETWORK, repair_commons, repair_commons_among_first, repair_commons_at,
    repair_commons_first, repair_commons_reports,
};

/// One Commons round between Macs `a` (initiator) and `b` (responder), each with its ring windows
/// and wall clock at `now[i]` seconds and its clock mark on where `marks[i]` (`false` stands for a
/// build from before F22): each side's control repair after it, which carries what it read of the
/// other's clock (`crate::control::Repair::clock`).
pub(crate) fn clock_round(
    lab: &Lab,
    (a, b): (usize, usize),
    now: [u64; 2],
    marks: [bool; 2],
) -> [crate::control::Repair; 2] {
    use crate::control::{Endpoint, Repair, Side};
    use aterm_net::lan::{Connection, Scope as LinkScope};
    let endpoint = |n: usize, i: usize| {
        Endpoint::from_bootstrap(&lab.bootstrap(n))
            .unwrap()
            .unwrap()
            .with_commons(lab.paths[n].clone(), lab.paths[n].home.join("config.toml"))
            .unwrap()
            .with_ring_clock(now[i])
            .with_marks(marks[i])
    };
    let (left, right) = (endpoint(a, 0), endpoint(b, 1));
    let (mut ra, mut rb) = (Repair::default(), Repair::default());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::scope(|tasks| {
        let incoming = tasks.spawn(|| {
            right.exchange(
                Connection::admit(listener.accept().unwrap().0, LinkScope::Loopback).unwrap(),
                Side::Responder,
                &mut rb,
            )
        });
        left.exchange(
            Connection::connect(address, LinkScope::Loopback, Duration::from_secs(60)).unwrap(),
            Side::Initiator,
            &mut ra,
        )
        .unwrap();
        incoming.join().unwrap().unwrap();
    });
    [ra, rb]
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
    let valid = wire.clone();
    let last_payload = wire.len() - 65;
    wire[last_payload] ^= 1;
    resign(&bootstrap, &mut wire);
    for _ in 0..2 {
        assert!(session.authenticate(scope, &wire).is_err());
        assert!(session.negative_record(scope, &wire).unwrap().is_none());
    }
    session
        .with_read_batch(scope, |batch| {
            assert!(batch.authenticate(&valid).is_ok());
            assert!(batch.authenticate(&wire).is_err());
            assert!(batch.authenticate(&valid).is_ok());
            Ok(())
        })
        .unwrap();
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
    for _ in 0..2 {
        assert!(
            sa.authenticate(scope, &forgery)
                .err()
                .unwrap()
                .0
                .contains("historical writer")
        );
    }
}

/// [`Session::writers`] names exactly the members [`Session::peer_writer`] accepts one at a
/// time: a Reader and a Writer the swarm roster denies are left out.
#[test]
fn writers_are_the_members_each_peer_writer_check_accepts() {
    let lab = Lab::new(&[Role::Writer, Role::Reader, Role::Writer]);
    let denied = lab.bootstrap(2).request().unwrap().binding().clone();
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    let deny = format!(
        "deny_nodes = [\"{}\"]\n",
        aterm_codec::hex::encode_lower(denied.machine())
    );
    doc.set_entry(
        &Audience::Everyone,
        aterm_secrets::swarm_roster::ROSTER_ENTRY,
        deny.as_bytes(),
    )
    .unwrap();
    client::admin_publish(&lab.paths[0], &admin, &doc).unwrap();
    client::sync(&lab.paths[0], None).unwrap();
    let bootstrap = lab.bootstrap(0);
    let session = Session::observe(&bootstrap).unwrap();
    let accepted = session
        .members()
        .unwrap()
        .into_iter()
        .filter(|binding| session.peer_writer(binding.actor()).is_ok())
        .collect::<Vec<_>>();
    assert_eq!(session.writers().unwrap(), accepted);
    assert_eq!(
        accepted,
        vec![bootstrap.request().unwrap().binding().clone()]
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
fn observed_session_keeps_its_read_view_and_refuses_other_custody_or_a_writer() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    drop(lab.bootstrap(0));
    let deadline = crate::admission::deadline();
    let (bootstrap, journal) =
        Bootstrap::established_with_journal_until(&lab.paths[0], deadline).unwrap();
    assert!(!journal.is_writer());
    let expected = journal.inventory().copied().collect::<Vec<_>>();
    let held = Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust()).unwrap();
    let session = Session::from_observed_journal(&bootstrap, journal, Some(deadline)).unwrap();
    assert_eq!(
        session.journal.inventory().copied().collect::<Vec<_>>(),
        expected
    );
    assert_eq!(
        session.actor(),
        bootstrap.request().unwrap().binding().actor()
    );
    assert!(
        session.writer().is_err(),
        "an observed writer role grants no writer lock"
    );
    drop(session);
    let error = Session::from_observed_journal(&bootstrap, held, Some(deadline))
        .err()
        .expect("an exclusively locked journal cannot become a read-only session");
    assert!(error.to_string().contains("read-only journal"));

    let other = lab.bootstrap(1);
    let foreign = Journal::read_existing(other.journal_dir(), other.trust()).unwrap();
    assert_eq!(other.trust(), bootstrap.trust());
    let error = Session::from_observed_journal(&bootstrap, foreign, Some(deadline))
        .err()
        .expect("equal network trust does not identify the observed local custody");
    assert!(error.to_string().contains("another custody directory"));
}

#[test]
fn observed_session_applies_revocation_in_the_supplied_history() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let retained = lab.bootstrap(1);
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let deadline = crate::admission::deadline();
    let (bootstrap, mut journal) = Bootstrap::cached_with_journal_until(&lab.paths[0], deadline)
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
    drop(journal);
    let observed = Journal::read_existing(bootstrap.journal_dir(), bootstrap.trust()).unwrap();
    let error = Session::from_observed_journal(&bootstrap, observed, Some(deadline))
        .err()
        .expect("observing an existing prefix cannot restore a revoked grant");
    assert!(
        error.to_string().contains("could not be decrypted"),
        "{error}"
    );
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
    let error = aterm_uds::file_lock::with_lock_wait_observer(
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
        let error = aterm_uds::file_lock::with_lock_wait_observer(
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

#[test]
fn a_bounded_flat_session_keeps_its_deadline_without_expiring_an_admitted_session() {
    use crate::{catalog::Descriptor, table::store::Store};
    use std::time::Instant;

    let lab = Lab::new(&[Role::Writer; 3]);
    let bootstrap = lab.bootstrap(0);
    let directory = crate::catalog::Directory::new(&lab.paths[0], bootstrap.trust());
    let scope = {
        let session = Session::open(&bootstrap).unwrap();
        let descriptor =
            Descriptor::application(bootstrap.trust(), "bounded-flat", "records", 1).unwrap();
        let scope = directory.publish(&session, &descriptor).unwrap();
        Store::open(&session, &directory.path(scope).unwrap(), scope)
            .unwrap()
            .commit_catalog(&session, b"retained before bounded access")
            .unwrap();
        scope
    };
    let path = directory.path(scope).unwrap();
    let retained = std::fs::read(path.join("table.astream")).unwrap();
    {
        let admitted =
            Session::open_until(&bootstrap, Instant::now() + aterm_spec::wait::MUST_HAPPEN)
                .unwrap();
        assert!(admitted.generation_deadline.is_none());
        admitted.writer().unwrap();
        admitted.mutation_guard().unwrap();
        assert_eq!(
            Store::open(&admitted, &path, scope)
                .unwrap()
                .inventory(&admitted)
                .unwrap()
                .count(),
            1
        );
    }
    let journal = Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust()).unwrap();
    let mut bounded = Session::from_admitted_journal_until(
        &bootstrap,
        journal,
        Some(Instant::now() + aterm_spec::wait::MUST_HAPPEN),
    )
    .unwrap();
    assert!(bounded.generation.is_none());
    bounded.generation_deadline = Some(Instant::now() - Duration::from_secs(1));
    assert!(
        bounded
            .require_current_application_generation(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
            .is_err(),
        "a fresh caller deadline cannot extend the retained session bound"
    );
    let refused = (
        bounded.writer().is_err(),
        bounded.mutation_guard().is_err(),
        Store::open(&bounded, &path, scope).is_err(),
    );
    assert_eq!(std::fs::read(path.join("table.astream")).unwrap(), retained);
    assert_eq!(
        refused,
        (true, true, true),
        "the retained bound covers every mutation entry"
    );
}

#[test]
fn an_admitted_successor_does_not_inherit_the_lock_wait_deadline() {
    use crate::{catalog::Descriptor, table::store::Store};
    use std::time::Instant;

    let lab = Lab::new(&[Role::Writer; 3]);
    let original = lab.bootstrap(0);
    let scope = {
        let session = Session::open(&original).unwrap();
        let directory = crate::catalog::Directory::new(&lab.paths[0], original.trust());
        let descriptor =
            Descriptor::application(original.trust(), "admission-test", "records", 1).unwrap();
        let scope = directory.publish(&session, &descriptor).unwrap();
        Store::open(&session, &directory.path(scope).unwrap(), scope)
            .unwrap()
            .commit_catalog(&session, b"retained before succession")
            .unwrap();
        scope
    };
    let wire = {
        let peer = lab.bootstrap(2);
        Session::open(&peer)
            .unwrap()
            .seal(scope, causal(), b"received after writer admission")
            .unwrap()
    };
    let config = lab.paths[0].home.join("config.toml");
    let next = crate::generation::test_complete_successor(
        lab._home.path(),
        &lab.paths[0],
        &lab.paths[1],
        &config,
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    let bootstrap = Bootstrap::required(&next).unwrap();
    let directory = crate::catalog::Directory::new(&next, bootstrap.trust());
    let path = directory.path(scope).unwrap();
    let mut session =
        Session::open_until(&bootstrap, Instant::now() + aterm_spec::wait::MUST_HAPPEN).unwrap();
    // Age any retained admission deadline without sleeping or depending on
    // replay speed. Once admitted, ordinary storage must use fresh checks.
    session.generation_deadline = session
        .generation_deadline
        .map(|_| Instant::now() - Duration::from_secs(1));
    let mut store = Store::open(&session, &path, scope)
        .expect("an admitted successor can replay after the lock-wait deadline");
    let receipt = store.ingest(&session, &wire).unwrap();
    assert_eq!(receipt.id, aterm_digest::Sha256::digest(&wire));
    assert_eq!(store.inventory(&session).unwrap().count(), 2);
    let retained = std::fs::read(path.join("table.astream")).unwrap();

    // A fresh check still withdraws this admitted successor when its original
    // sharing policy changes; an elapsed admission budget grants no authority.
    aterm_secrets::fsx::write_atomic(&config, b"[commons]\nenabled = false\n", 0o600).unwrap();
    assert!(store.ingest(&session, &wire).is_err());
    assert_eq!(std::fs::read(path.join("table.astream")).unwrap(), retained);
    drop(store);
    drop(session);
    aterm_secrets::fsx::write_atomic(&config, b"", 0o600).unwrap();

    // Callers explicitly bounding a whole session retain that bound. The
    // correction must not disable deadline checks in the generation guard.
    let journal = Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust()).unwrap();
    let mut bounded = Session::from_admitted_journal_until(
        &bootstrap,
        journal,
        Some(Instant::now() + aterm_spec::wait::MUST_HAPPEN),
    )
    .unwrap();
    bounded.generation_deadline = Some(Instant::now() - Duration::from_secs(1));
    assert!(Store::open(&bounded, &path, scope).is_err());
    assert_eq!(std::fs::read(path.join("table.astream")).unwrap(), retained);
    drop(bounded);
    assert!(Session::open_until(&bootstrap, Instant::now() - Duration::from_secs(1)).is_err());
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
        let accepted = session.authenticate(scope, &wire).is_ok();
        let batch_accepted = session
            .with_read_batch(scope, |batch| batch.authenticate(&wire))
            .is_ok();
        assert_eq!(batch_accepted, accepted, "{action}");
        accepted
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

#[test]
fn read_batches_refuse_membership_or_custody_changes_before_returning() {
    for changed in ["control", "source.toml", "commons/device.age"] {
        let lab = Lab::new(&[Role::Writer]);
        let bootstrap = lab.bootstrap(0);
        let writer = Session::open(&bootstrap).unwrap();
        let scope = writer.scope().unwrap();
        let wire = writer.seal(scope, causal(), b"retained metadata").unwrap();
        drop(writer);
        let reader = Session::observe(&bootstrap).unwrap();
        let admin = client::admin_open(&lab.paths[0]).unwrap();
        let grant = Grant::recorded(
            "m1",
            bootstrap.request().unwrap().binding().clone(),
            Role::Reader,
        );
        let demotion = admin
            .prepare_commons_rotation(&reader.journal, vec![grant])
            .unwrap();
        let result = reader.with_read_batch(scope, |batch| {
            let record = batch.authenticate(&wire)?;
            batch.authenticate(&wire)?;
            if changed == "control" {
                Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust())
                    .unwrap()
                    .ingest(demotion.bytes())
                    .unwrap();
            } else {
                let path = lab.paths[0].dir.join(changed);
                std::fs::rename(&path, path.with_extension("moved")).unwrap();
            }
            // The key is already warm. Its final currency check and the
            // enclosing source check must still withhold this result.
            let _ = batch.authenticate(&wire);
            Ok(record)
        });
        assert!(
            result.is_err(),
            "batch returned data after {changed} changed"
        );
    }
}

pub(crate) mod frontier;
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

/// A Mac the membership log lets in after a Commons rotation holds no `Wrap` of the log-era
/// epoch an older row is sealed under (an admission wraps the admission epochs alone): table
/// repair names that epoch among the history it needs, a member issues the history handoff
/// (ACHG1) of it, and the Mac then reads the row.
///
/// NEGATIVE CONTROL: before the handoff the Mac cannot authenticate the row, and repair asks
/// for the epoch.
#[test]
fn a_log_admitted_mac_reads_an_older_log_epochs_row_through_a_history_handoff() {
    use aterm_secrets::{
        client::{Paths, admit_member_in_log, load_records, remove_member_in_log, save_records},
        commons::bootstrap::{Bootstrap, Device},
        membership_log::{Entrant, Evidence},
    };
    let lab = Lab::with_foundation(&[Role::Writer, Role::Writer]);
    client::seed_vault_genesis(&lab.paths[0], &lab._home.path().join("genesis.v2")).unwrap();
    save_records(
        &lab.paths[1],
        &load_records(&lab.paths[0]).unwrap().encode(),
    )
    .unwrap();
    let b1 = lab.bootstrap(0);
    let trust = b1.trust();
    let pinned = format!(
        "{}#{}",
        lab._home.path().join("test.vault").display(),
        lab.signer
    );
    let session = Session::open(&b1).unwrap();
    assert!(
        session.is_log_backed(),
        "CONTROL: the seeded log backs Commons"
    );
    let dir = lab._home.path().join("table-log");
    let mut original = store::Store::create(&session, &dir).unwrap();
    let scope = original.scope();
    let receipt = original.commit_catalog(&session, b"log-era-row").unwrap();
    let wire = original
        .wire(&session, &receipt.id)
        .unwrap()
        .unwrap()
        .to_vec();
    let source = *session.authenticate(scope, &wire).unwrap().roster();
    drop((original, session));

    let entrant = |name: &str, byte: u8| -> (Paths, Entrant) {
        let paths = Paths::for_home(&lab._home.path().join(name));
        client::configure_log_member(&paths, &pinned, name).unwrap();
        let _ = client::machine_identity(&paths).unwrap();
        let request = Device::open(&paths)
            .unwrap()
            .unwrap()
            .request(trust.network, trust.authority)
            .unwrap();
        let entrant = Entrant {
            request,
            node_key: [byte; 32],
            name: name.to_owned(),
            label: name.to_owned(),
        };
        (paths, entrant)
    };
    // A rotation: m4 is let in and removed again.
    let (_m4, e4) = entrant("m4", 0x44);
    let actor4 = e4.request.binding().actor();
    admit_member_in_log(&lab.paths[0], e4, Evidence::Phrase).unwrap();
    remove_member_in_log(&lab.paths[0], actor4).unwrap();
    let (m3, e3) = entrant("m3", 0x33);
    let actor3 = e3.request.binding().actor();
    admit_member_in_log(&lab.paths[0], e3, Evidence::Phrase).unwrap();
    let records = load_records(&lab.paths[0]).unwrap().encode();
    save_records(&lab.paths[1], &records).unwrap();
    save_records(&m3, &records).unwrap();
    let b3 = Bootstrap::required(&m3).unwrap();
    assert!(
        Session::open(&b3)
            .unwrap()
            .authenticate(scope, &wire)
            .is_err(),
        "CONTROL: m3 holds no key of the older epoch"
    );
    let (needs, _) = Session::open(&b3).unwrap().history_needs(None, 64).unwrap();
    assert!(needs.contains(&source), "repair asks for the log-era epoch");

    let handoff = Session::open(&b1)
        .unwrap()
        .handoffs(actor3, &[source])
        .unwrap()
        .remove(0)
        .expect("m1 issues the handoff of a log-era epoch");
    Session::open(&b3)
        .unwrap()
        .import_handoff(source, &handoff)
        .unwrap();
    let (needs, _) = Session::open(&b3).unwrap().history_needs(None, 64).unwrap();
    assert!(!needs.contains(&source), "the handoff answers the need");
    assert_eq!(
        Session::open(&b3)
            .unwrap()
            .authenticate(scope, &wire)
            .unwrap()
            .payload(),
        b"log-era-row"
    );
}

/// A Mac that holds another Mac's `Remove` but not yet the removal's rotation seals no table row:
/// the Commons epoch it would seal under is still wrapped to the removed Mac, whose key would open
/// the row. Once the rotation lands, its session is a writer's again, under the rotation's epoch.
///
/// NEGATIVE CONTROL: the oracle reads the log itself: before the rotation the session's basis
/// carries a `Wrap` to the removed Mac, and after it none.
#[test]
fn a_mac_holding_a_removal_without_its_rotation_seals_no_row() {
    use aterm_secrets::{
        client::{Paths, admit_member_in_log, load_records, remove_member_in_log, save_records},
        commons::bootstrap::Device,
        membership_log::{Body, Entrant, Evidence, encode_records},
    };
    let lab = Lab::with_foundation(&[Role::Writer, Role::Writer]);
    client::seed_vault_genesis(&lab.paths[0], &lab._home.path().join("genesis.v2")).unwrap();
    let trust = lab.bootstrap(0).trust();
    let pinned = format!(
        "{}#{}",
        lab._home.path().join("test.vault").display(),
        lab.signer
    );
    let m4 = Paths::for_home(&lab._home.path().join("m4"));
    client::configure_log_member(&m4, &pinned, "m4").unwrap();
    let _ = client::machine_identity(&m4).unwrap();
    let request = Device::open(&m4)
        .unwrap()
        .unwrap()
        .request(trust.network, trust.authority)
        .unwrap();
    let (actor4, recipient4) = (request.binding().actor(), request.binding().recipient());
    let entrant = Entrant {
        request,
        node_key: [0x44; 32],
        name: "m4".to_owned(),
        label: "m4".to_owned(),
    };
    admit_member_in_log(&lab.paths[0], entrant, Evidence::Phrase).unwrap();
    save_records(
        &lab.paths[1],
        &load_records(&lab.paths[0]).unwrap().encode(),
    )
    .unwrap();
    let b1 = lab.bootstrap(1);
    let dir = lab._home.path().join("table-m1");
    // Each session reopens the replica: a session's custody is its own.
    let write = |session: &Session<'_>, payload: &[u8]| {
        store::Store::create(session, &dir)?.commit_catalog(session, payload)
    };
    let session = Session::open(&b1).unwrap();
    write(&session, b"before").expect("m1 writes before the removal");
    drop(session);

    let removal = remove_member_in_log(&lab.paths[0], actor4).unwrap();
    assert!(
        matches!(removal[0].body(), Body::Remove { .. }),
        "CONTROL: the batch opens with the Remove"
    );
    save_records(&lab.paths[1], &encode_records(&removal[..1])).unwrap();
    let wraps_removed = |session: &Session<'_>| {
        load_records(&lab.paths[1])
            .unwrap()
            .log()
            .unwrap()
            .records()
            .any(|record| {
                matches!(record.body(), Body::Wrap { epoch, recipient, .. }
                    if epoch == session.access.basis() && *recipient == recipient4)
            })
    };
    let session = Session::open(&b1).unwrap();
    assert!(
        wraps_removed(&session),
        "CONTROL: the basis is still wrapped to the removed m4"
    );
    let refused = write(&session, b"during").unwrap_err();
    assert!(
        refused.to_string().contains("removed Mac"),
        "the write waits for the rotation: {refused}"
    );
    drop(session);

    save_records(&lab.paths[1], &encode_records(&removal)).unwrap();
    let session = Session::open(&b1).unwrap();
    assert!(
        !wraps_removed(&session),
        "CONTROL: the rotation's epoch reaches m4 no more"
    );
    session
        .writer()
        .expect("a writer's session opens once the rotation lands");
}

/// Table repair refuses a peer while this device has never read its roster (it could name that
/// peer), admits it once a read succeeds, and keeps that answer through a read that fails after
/// it. The read is made to fail by leaving the scratch vault's `source.toml` readable by other
/// users, which every vault read refuses.
///
/// NEGATIVE CONTROL (2026-11-01): with `Session::peer` reading the roster as
/// `swarm_roster(..).ok().flatten()` again (a failed read checks no roster), this fails (`a peer
/// is refused while no roster was ever read`).
#[test]
fn table_repair_refuses_a_peer_until_its_roster_is_read_then_keeps_the_last() {
    use std::os::unix::fs::PermissionsExt as _;
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let peer = lab.bootstrap(1).request().unwrap().binding().actor();
    let session = Session::observe(&bootstrap).unwrap();
    let source = lab.paths[0].dir.join("source.toml");
    let set = |mode| std::fs::set_permissions(&source, std::fs::Permissions::from_mode(mode));
    set(0o644).unwrap();
    let refused = session.peer(peer);
    set(0o600).unwrap();
    let Err(why) = refused else {
        panic!("a peer is refused while no roster was ever read");
    };
    assert!(
        why.to_string().contains("has not read its roster yet"),
        "{why}"
    );
    assert!(session.peer(peer).is_ok(), "admitted once a read succeeds");
    set(0o644).unwrap();
    let kept = session.peer(peer);
    set(0o600).unwrap();
    assert!(
        kept.is_ok(),
        "a failed read keeps the last answer: {kept:?}"
    );
}
