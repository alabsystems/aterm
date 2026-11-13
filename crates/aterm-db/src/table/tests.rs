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
    Lab, NETWORK, repair_between, repair_commons, repair_commons_among_first, repair_commons_at,
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
                Connection::admit(
                    aterm_uds::accept(
                        &listener,
                        aterm_uds::AcceptWait::Forever,
                        aterm_uds::AcceptedMode::Blocking,
                    )
                    .unwrap()
                    .0,
                    LinkScope::Loopback,
                )
                .unwrap(),
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

/// A public foundation's members hold every epoch of its history, before and after a rotation,
/// and a history ask keeps none of their keys in its session: it asks whether each opens
/// (`Bootstrap::holds_histories`), not for its key.
/// NEGATIVE CONTROL: `Session::history_needs` keeping each `historicals` answer it was given in
/// `history` fails the second assertion.
#[test]
fn a_public_foundation_never_requests_a_historical_epoch_key() {
    let lab = Lab::with_foundation(&[Role::Writer, Role::Writer]);
    for _ in 0..2 {
        for n in 0..2 {
            let bootstrap = lab.bootstrap(n);
            let session = Session::open(&bootstrap).unwrap();
            let (needs, next) = session.history_needs(None, 16).unwrap();
            assert!(needs.is_empty());
            assert!(
                session.history.borrow().is_empty(),
                "a history ask keeps no key"
            );
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
    fn open_now(bootstrap: &Bootstrap) -> Result<Session<'_>> {
        // An expired journal wait still attempts the kernel lock once. Keep
        // subsequent session admission on its ordinary budget.
        let journal = Journal::open_existing_until(
            bootstrap.journal_dir(),
            bootstrap.trust(),
            std::time::Instant::now(),
        )
        .map_err(failure)?;
        Session::from_admitted_journal(bootstrap, journal)
    }

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
        let blocked = open_now(&a)
            .err()
            .expect("control observation is locked during the local batch");
        assert!(blocked.is_busy(), "unexpected lock refusal: {blocked}");
        drop(session);
        for n in 0..count {
            let bootstrap = lab.bootstrap(n);
            let session = open_now(&bootstrap).unwrap();
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
    let error =
        Journal::open_existing_until(bootstrap.journal_dir(), bootstrap.trust(), Instant::now())
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

/// A store read batch closes through a look at the vault of its own, made after its read
/// (`Session::close_read`), never through the look its session keeps, whose first look at each
/// file came before the read: in a log-backed session that keeps a look, with every vault file
/// judged settled (`client::test_as_if_later`, within which the digests kept are this thread's
/// own, so no other test's asks let one go) so that the kept look's digests stand, a batch whose
/// read rewrites the network pin or the device's age identity (its custody's pinned files) with
/// different bytes and a new inode, or writes the fold mark (whose arrival its currency judges:
/// `commons::fold::on` reads no byte of it), is refused before it returns, one batch a file.
/// CONTROLS: a batch that changes nothing returns; the read takes no key, so no key is checked
/// again (`KEYS_REVALIDATED`); custody, and currency, through the kept look after the change still
/// pass; and the fold mark's arrival leaves the access not current under a fresh look.
/// NEGATIVE CONTROL: `close_read` judging through the session's kept look returns each batch's
/// data (`each batch is refused before it returns: [("default.pin", None), ("device.age", None),
/// ("fold", None)]`).
#[test]
fn a_read_batch_closes_through_a_look_made_after_its_read() {
    const FILES: [&str; 3] = ["default.pin", "device.age", "fold"];
    let mut refusals = Vec::new();
    client::test_as_if_later(Duration::from_secs(3600), || {
        for file in FILES {
            let lab = Lab::with_foundation(&[Role::Writer]);
            let paths = &lab.paths[0];
            client::seed_vault_genesis(paths, &lab._home.path().join("genesis.v2")).unwrap();
            let commons = paths.dir.join("commons");
            let path = commons.join(file);
            assert_eq!(
                path.is_file(),
                file != "fold",
                "CONTROL: {file} is there, and the fold mark is not yet"
            );
            let bootstrap = lab.bootstrap(0);
            let mut session = Session::observe(&bootstrap).unwrap();
            assert!(session.is_log_backed(), "CONTROL: the seeded log backs it");
            session
                .keep_look(client::VaultLook::new(bootstrap.paths()))
                .unwrap();
            // As a table-repair admission does, the kept look judges the session's currency
            // before any batch runs.
            assert!(
                session.access.is_current_in(
                    &session.journal,
                    session.look().expect("the session keeps its look")
                ),
                "CONTROL: {file}: current through the kept look, as an admission judges it"
            );
            let scope = Scope::fresh(session.journal.trust()).unwrap();
            session
                .with_read_batch(scope, |_| Ok(()))
                .unwrap_or_else(|error| {
                    panic!("CONTROL: {file}: an unchanged batch returns: {error}")
                });
            let keys = KEYS_REVALIDATED.get();
            let refused = session.with_read_batch(scope, |_| {
                use std::os::unix::fs::MetadataExt as _;
                let inode = std::fs::symlink_metadata(&path).ok().map(|meta| meta.ino());
                if file == "fold" {
                    turn_fold_on(paths);
                } else {
                    let mut bytes = std::fs::read(&path).unwrap();
                    bytes.push(b'\n');
                    aterm_uds::private_file::write_private(&commons, file, &bytes).unwrap();
                }
                assert_ne!(
                    std::fs::symlink_metadata(&path).ok().map(|meta| meta.ino()),
                    inode,
                    "CONTROL: {file} has a new inode"
                );
                let kept = session.look().expect("the session keeps its look");
                assert!(
                    bootstrap.revalidate_custody_in(kept).is_ok(),
                    "CONTROL: {file}: custody through the kept look still passes"
                );
                assert!(
                    session.access.is_current_in(&session.journal, kept),
                    "CONTROL: {file}: currency through the kept look still passes"
                );
                if file == "fold" {
                    assert!(
                        !session
                            .access
                            .is_current_in(&session.journal, &client::VaultLook::new(paths)),
                        "CONTROL: the fold mark's arrival leaves the access not current under \
                         a fresh look"
                    );
                }
                Ok(())
            });
            assert_eq!(
                KEYS_REVALIDATED.get(),
                keys,
                "CONTROL: {file}: the read takes no key"
            );
            refusals.push((file, refused.err().map(|error| error.to_string())));
        }
    });
    assert_eq!(
        refusals
            .iter()
            .filter(|(_, refused)| refused.is_some())
            .map(|(file, _)| *file)
            .collect::<Vec<_>>(),
        FILES,
        "each batch is refused before it returns: {refusals:?}"
    );
}

/// A vault writer that holds the state lock while a session judges a table's context (each store
/// open asks it, outside a read batch's source reads) makes the context busy, which a read
/// observes again (`att::reopened`), not "stale membership", which nothing retries: the access is
/// log-backed, so its currency reads the vault.
/// NEGATIVE CONTROL: with `context` asking `Access::is_current` again, the held lock reads as
/// stale membership.
#[test]
fn a_vault_writer_at_a_store_open_is_busy_not_stale_membership() {
    let lab = Lab::with_foundation(&[Role::Writer]);
    client::seed_vault_genesis(&lab.paths[0], &lab._home.path().join("genesis.v2")).unwrap();
    let bootstrap = lab.bootstrap(0);
    let session = Session::observe(&bootstrap).unwrap();
    assert!(session.is_log_backed(), "CONTROL: the seeded log backs it");
    let scope = Scope::fresh(session.journal.trust()).unwrap();
    let writer = client::test_hold_state_lock(&lab.paths[0]).unwrap();
    let busy = session.context(scope).unwrap_err();
    drop(writer);
    assert!(busy.is_busy(), "{busy}");
    session.context(scope).unwrap();
}

/// A vault writer that takes the state lock between a session open's custody check and its read
/// of the home's access makes the open busy, which a reader observes again, not "Commons
/// membership is not ready for table access", a repair cause nothing retries. The lock is taken
/// after the custody check: held across the whole open, `revalidate_custody` is already busy, so
/// a test of that shape passes with or without the fix.
/// NEGATIVE CONTROL (2026-11-08): with `with_generation` reading `access_in`, the held lock reads
/// as not-ready membership.
#[test]
fn a_vault_writer_at_a_session_open_is_busy_not_unready_membership() {
    let lab = Lab::with_foundation(&[Role::Writer]);
    client::seed_vault_genesis(&lab.paths[0], &lab._home.path().join("genesis.v2")).unwrap();
    let bootstrap = lab.bootstrap(0);
    bootstrap
        .revalidate_custody()
        .expect("CONTROL: custody holds with no writer");
    let journal = Journal::read_existing(bootstrap.journal_dir(), bootstrap.trust()).unwrap();
    let writer = client::test_hold_state_lock(&lab.paths[0]).unwrap();
    let refused = Session::with_generation(
        &bootstrap,
        journal,
        None,
        Some(std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN),
        None,
    )
    .err()
    .expect("a held state lock opens no session");
    drop(writer);
    assert!(refused.is_busy(), "{refused}");
    assert!(
        !refused.to_string().contains("not ready for table access"),
        "{refused}"
    );
    Session::observe(&bootstrap).expect("the session opens once the writer is gone");
}

/// Under a log, with no generation, a read's currency is the log's answer, and a scope is held to
/// the journal's trust. A session with a generation asks that generation, whose check can wait.
///
/// NEGATIVE CONTROLS (2026-11-05): with the log arm's comparison of trust gone, the scope of another
/// trust is answered (`unwrap_err` on an `Ok` at the first refusal); with `self.generation.is_some()`
/// gone from the arm's condition, the session whose generation has run out of time is answered
/// (`unwrap_err` on an `Ok` at the second).
#[test]
fn a_log_backed_read_context_holds_a_scope_to_its_trust_and_asks_a_generation() {
    let lab = Lab::with_foundation(&[Role::Writer]);
    client::seed_vault_genesis(&lab.paths[0], &lab._home.path().join("genesis.v2")).unwrap();
    let bootstrap = lab.bootstrap(0);
    let mut session = Session::observe(&bootstrap).unwrap();
    assert!(session.is_log_backed(), "CONTROL: the seeded log backs it");
    assert!(session.generation.is_none(), "CONTROL: not a candidate");
    let scope = Scope::fresh(session.journal.trust()).unwrap();
    session.read_context(scope).unwrap();

    let mut trust = scope.trust();
    trust.genesis[0] ^= 1;
    let another = Scope::new(trust, *scope.table(), scope.generation()).unwrap();
    let refused = session.read_context(another).unwrap_err().to_string();
    assert!(refused.contains("another trust context"), "{refused}");

    session.generation = Some(
        crate::generation::Observation::observe(
            &lab.paths[0],
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap(),
    );
    session.read_context(scope).unwrap();
    session.generation_deadline = Some(std::time::Instant::now());
    let refused = session.read_context(scope).unwrap_err().to_string();
    assert!(refused.contains("writer generation"), "{refused}");
}

/// A journal-backed session's read context asks the handle it holds, which a fresh read of the
/// journal cannot answer for: the writer lease replaced under a held writer leaves the journal
/// reading as it did, and that handle uncertain.
///
/// NEGATIVE CONTROL (2026-11-05): with `!self.access.is_log_backed()` gone from the arm's condition,
/// the uncertain handle is not asked and the context is answered (`unwrap_err` on an `Ok`).
#[test]
fn a_journal_backed_read_context_asks_the_handle_it_holds() {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    assert!(!session.is_log_backed(), "CONTROL: its journal backs it");
    let scope = session.scope().unwrap();
    session.read_context(scope).unwrap();

    let lease = bootstrap.journal_dir().join("control.lock");
    let bytes = std::fs::read(&lease).unwrap();
    aterm_secrets::fsx::write_atomic(&lease, &bytes, 0o600).unwrap();
    let refused = session.read_context(scope).unwrap_err().to_string();
    assert!(refused.contains("stale membership"), "{refused}");
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

/// A lab of three log members in which m1 (`paths[1]`) holds a table written before m0 removes
/// m4, and m0's whole removal batch: its `Remove` first, then the rotation. The removed Mac's
/// recipient and the table's folder.
fn removal_lab(
    fold: bool,
) -> (
    Lab,
    Vec<aterm_secrets::membership_log::Record>,
    aterm_secrets::age::Recipient,
    std::path::PathBuf,
    Scope,
) {
    use aterm_secrets::{
        client::{Paths, admit_member_in_log, load_records, remove_member_in_log, save_records},
        commons::bootstrap::Device,
        membership_log::{Body, Entrant, Evidence},
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
    let dir = lab._home.path().join("table-m1");
    let scope = {
        let b1 = lab.bootstrap(1);
        let session = Session::open(&b1).unwrap();
        let mut store = store::Store::create(&session, &dir).unwrap();
        store
            .commit_catalog(&session, b"before")
            .expect("m1 writes before the removal");
        store.scope()
    };
    if fold {
        for paths in &lab.paths {
            turn_fold_on(paths);
        }
    }
    let removal = remove_member_in_log(&lab.paths[0], actor4).unwrap();
    assert!(
        matches!(removal[0].body(), Body::Remove { .. }),
        "CONTROL: the batch opens with the Remove"
    );
    (lab, removal, recipient4, dir, scope)
}

/// Turn the fold's adoption gate on for the vault at `paths` the way the node service does,
/// through `commons::fold::note`, here by the 60-day bound.
fn turn_fold_on(paths: &aterm_secrets::client::Paths) {
    use aterm_secrets::commons::fold;
    let gate = fold::Gate {
        reads: None,
        r2_shipped_ms: Some(0),
    };
    assert!(fold::note(paths, &gate, &[], fold::BOUND_MS).unwrap());
    assert!(fold::on(paths));
}

/// The audiences of the `Epoch` records in `batch`.
fn minted_audiences(
    batch: &[aterm_secrets::membership_log::Record],
) -> Vec<aterm_secrets::membership_log::EpochAudience> {
    batch
        .iter()
        .filter_map(|record| match record.body() {
            aterm_secrets::membership_log::Body::Epoch { audience, .. } => Some(audience.clone()),
            _ => None,
        })
        .collect()
}

/// One-mesh M6 (§11 B11): once the fold's adoption gate holds, a removal mints one audience,
/// `[everyone]`; before it, Commons keeps its own epochs (the control: the same lab with the gate
/// shut mints both).
#[test]
fn a_removal_mints_one_audience_once_the_gate_holds() {
    use aterm_secrets::membership_log::EpochAudience;
    let (_lab, folded, ..) = removal_lab(true);
    assert_eq!(minted_audiences(&folded), vec![EpochAudience::Everyone]);
    let (_lab, unfolded, ..) = removal_lab(false);
    let mut audiences = minted_audiences(&unfolded);
    audiences.sort_by_key(|audience| format!("{audience:?}"));
    assert_eq!(
        audiences,
        vec![EpochAudience::Commons, EpochAudience::Everyone],
        "CONTROL: with the gate shut the removal rotates Commons too"
    );
}

/// The fold end to end: a write m1 makes while it holds m0's `Remove` of m4 stays pending, named
/// for the `[everyone]` epoch that still wraps m4, and applies under the `[everyone]` epoch the
/// removal minted; m0, which sealed nothing under the fold yet, reads it, and the row written
/// before the fold, under Commons, stays readable beside it. A member admitted after a later
/// rotation reads the fold row through a history handoff of the derived key: a member admitted
/// after the fold reads rows written before it.
///
/// NEGATIVE CONTROL: before the handoff the late member cannot authenticate the row, and repair
/// names the fold epoch among the history it needs.
/// Opening a table seals its pending writes; a seal whose append is uncertain poisons the handle,
/// so the open fails with that append's own error rather than handing back a store every later
/// call refuses. CONTROL: while the new epoch has not arrived the seal is refused before it
/// appends, and the open stands with the write still pending.
#[test]
fn an_open_whose_seal_append_is_uncertain_fails_with_its_error() {
    use aterm_secrets::{client::save_records, membership_log::encode_records};
    let (lab, removal, _, dir, scope) = removal_lab(true);
    save_records(&lab.paths[1], &encode_records(&removal[..1])).unwrap();
    let b1 = lab.bootstrap(1);
    let session = Session::open(&b1).unwrap();
    let kept = store::Store::open(&session, &dir, scope)
        .unwrap()
        .commit_catalog(&session, b"during")
        .unwrap_err();
    assert!(kept.is_write_pending(), "{kept}");
    let held = store::Store::open(&session, &dir, scope).expect("CONTROL: the open stands");
    assert!(
        !held.pending_epochs(&session).unwrap().is_empty(),
        "CONTROL: still pending"
    );
    drop((held, session));

    save_records(&lab.paths[1], &encode_records(&removal)).unwrap();
    let session = Session::open(&b1).unwrap();
    store::APPEND_FAILS.with(|fails| fails.set(true));
    let opened = store::Store::open(&session, &dir, scope);
    store::APPEND_FAILS.with(|fails| fails.set(false));
    let Err(error) = opened else {
        panic!("the open fails with the append's error");
    };
    assert!(
        error.to_string().contains("injected append failure"),
        "{error}"
    );
    let store = store::Store::open(&session, &dir, scope).unwrap();
    assert!(
        store.pending_epochs(&session).unwrap().is_empty(),
        "the next open seals it"
    );
}

#[test]
fn under_the_fold_a_pending_write_applies_under_the_new_everyone_epoch_and_reads_everywhere() {
    use aterm_secrets::{
        client::{
            Paths, admit_member_in_log, load_records, remove_member_in_log, save_records,
            trusted_commons_log,
        },
        commons::bootstrap::{Bootstrap, Device},
        membership_log::{Entrant, Evidence, encode_records},
    };
    let (lab, removal, recipient4, dir, scope) = removal_lab(true);
    save_records(&lab.paths[1], &encode_records(&removal[..1])).unwrap();
    let b1 = lab.bootstrap(1);
    let fold_epoch = |paths: &Paths, id: &Digest| {
        trusted_commons_log(paths, lab.bootstrap(1).trust())
            .unwrap()
            .unwrap()
            .0
            .is_fold_epoch(id)
    };
    let session = Session::open(&b1).unwrap();
    let old = *session.access.basis();
    assert!(
        fold_epoch(&lab.paths[1], &old),
        "the fold seals under an [everyone] epoch"
    );
    assert!(basis_wraps(&lab, &old, recipient4));
    let kept = store::Store::open(&session, &dir, scope)
        .unwrap()
        .commit_catalog(&session, b"during")
        .unwrap_err();
    assert!(kept.is_write_pending(), "{kept}");
    drop(session);

    save_records(&lab.paths[1], &encode_records(&removal)).unwrap();
    let session = Session::open(&b1).unwrap();
    let new = *session.access.basis();
    assert_ne!(new, old);
    assert!(fold_epoch(&lab.paths[1], &new));
    // Opening the table for writing seals the write its new epoch was waiting on.
    let store = store::Store::open(&session, &dir, scope).unwrap();
    assert!(store.pending_epochs(&session).unwrap().is_empty());
    let rows = store.catalog(&session).unwrap();
    let during = rows.iter().find(|r| r.payload() == b"during").unwrap();
    assert_eq!(*during.roster(), new);
    let during_id = during.header().id;
    let before = rows.iter().find(|r| r.payload() == b"before").unwrap();
    assert!(
        !fold_epoch(&lab.paths[1], before.roster()),
        "the row written before the fold stays under its Commons epoch"
    );
    let wire = store.wire(&session, &during_id).unwrap().unwrap().to_vec();
    drop((store, session));

    // m0 reads the fold row (its own observation is the same `[everyone]` epoch).
    let b0 = lab.bootstrap(0);
    assert_eq!(
        Session::open(&b0)
            .unwrap()
            .authenticate(scope, &wire)
            .unwrap()
            .payload(),
        b"during"
    );

    // A later rotation, then a late member: it holds no `Wrap` of the row's epoch.
    let trust = b0.trust();
    let pinned = format!(
        "{}#{}",
        lab._home.path().join("test.vault").display(),
        lab.signer
    );
    let entrant = |name: &str, byte: u8| -> (Paths, Entrant) {
        let paths = Paths::for_home(&lab._home.path().join(name));
        client::configure_log_member(&paths, &pinned, name).unwrap();
        let _ = client::machine_identity(&paths).unwrap();
        let request = Device::open(&paths)
            .unwrap()
            .unwrap()
            .request(trust.network, trust.authority)
            .unwrap();
        (
            paths,
            Entrant {
                request,
                node_key: [byte; 32],
                name: name.to_owned(),
                label: name.to_owned(),
            },
        )
    };
    let (_m6, e6) = entrant("m6", 0x66);
    let actor6 = e6.request.binding().actor();
    admit_member_in_log(&lab.paths[0], e6, Evidence::Phrase).unwrap();
    let again = remove_member_in_log(&lab.paths[0], actor6).unwrap();
    assert_eq!(
        minted_audiences(&again),
        vec![aterm_secrets::membership_log::EpochAudience::Everyone]
    );
    let (m5, e5) = entrant("m5", 0x55);
    let actor5 = e5.request.binding().actor();
    admit_member_in_log(&lab.paths[0], e5, Evidence::Phrase).unwrap();
    turn_fold_on(&m5);
    let records = load_records(&lab.paths[0]).unwrap().encode();
    save_records(&lab.paths[1], &records).unwrap();
    save_records(&m5, &records).unwrap();
    let b5 = Bootstrap::required(&m5).unwrap();
    assert!(
        Session::open(&b5)
            .unwrap()
            .authenticate(scope, &wire)
            .is_err(),
        "CONTROL: m5 holds no key of the fold epoch the row names"
    );
    let (needs, _) = Session::open(&b5).unwrap().history_needs(None, 64).unwrap();
    assert!(needs.contains(&new), "repair asks for the fold epoch");
    let handoff = Session::open(&b1)
        .unwrap()
        .handoffs(actor5, &[new])
        .unwrap()
        .remove(0)
        .expect("m1 issues the handoff of a fold epoch's table key");
    Session::open(&b5)
        .unwrap()
        .import_handoff(new, &handoff)
        .unwrap();
    let (needs, _) = Session::open(&b5).unwrap().history_needs(None, 64).unwrap();
    assert!(!needs.contains(&new), "the handoff answers the need");
    assert_eq!(
        Session::open(&b5)
            .unwrap()
            .authenticate(scope, &wire)
            .unwrap()
            .payload(),
        b"during",
        "a member admitted after the fold reads the row written before it"
    );
}

/// Whether the log m1 holds wraps `basis` to `recipient`.
fn basis_wraps(lab: &Lab, basis: &Digest, recipient: aterm_secrets::age::Recipient) -> bool {
    aterm_secrets::client::load_records(&lab.paths[1])
        .unwrap()
        .log()
        .unwrap()
        .records()
        .any(|record| {
            matches!(record.body(), aterm_secrets::membership_log::Body::Wrap { epoch, recipient: to, .. }
                if epoch == basis && *to == recipient)
        })
}

/// The roster (the epoch) the catalog row `payload` of the table at `dir` was sealed under.
fn sealed_under(
    session: &Session<'_>,
    dir: &std::path::Path,
    scope: Scope,
    payload: &[u8],
) -> Option<Digest> {
    store::Store::open(session, dir, scope)
        .unwrap()
        .catalog(session)
        .unwrap()
        .into_iter()
        .find(|record| record.payload() == payload)
        .map(|record| *record.roster())
}

/// One-mesh M4 (§11 B11): a Mac that holds another Mac's `Remove` but not yet the removal's
/// rotation keeps its table write pending, named for the epoch it waited on, because that epoch
/// is still wrapped to the removed Mac, whose key would open the row; nothing is sealed or
/// published meanwhile. Once the rotation arrives the store seals it under the new epoch, never
/// under the old one.
///
/// NEGATIVE CONTROL: `a_table_write_lands_under_the_old_epoch_when_the_wait_is_off`, the same lab
/// with `TABLES_DONT_WAIT` on, seals the write at once under the epoch that wraps the removed Mac.
#[test]
fn a_table_write_on_a_mac_holding_a_remove_stays_pending_until_the_new_epoch_arrives() {
    use aterm_secrets::{client::save_records, membership_log::encode_records};
    let (lab, removal, recipient4, dir, scope) = removal_lab(false);
    save_records(&lab.paths[1], &encode_records(&removal[..1])).unwrap();
    let b1 = lab.bootstrap(1);
    let session = Session::open(&b1).unwrap();
    let old = *session.access.basis();
    assert!(
        basis_wraps(&lab, &old, recipient4),
        "CONTROL: the basis is still wrapped to the removed m4"
    );
    let kept = store::Store::open(&session, &dir, scope)
        .unwrap()
        .commit_catalog(&session, b"during")
        .unwrap_err();
    assert!(
        kept.is_write_pending(),
        "the write waits for the rotation: {kept}"
    );
    let held = store::Store::open(&session, &dir, scope).unwrap();
    assert_eq!(held.pending_epochs(&session).unwrap(), vec![old]);
    drop(held);
    assert_eq!(
        sealed_under(&session, &dir, scope, b"during"),
        None,
        "nothing is sealed yet"
    );
    // A second pass while the rotation is still due seals nothing and keeps one write.
    let mut held = store::Store::open(&session, &dir, scope).unwrap();
    assert_eq!(held.seal_pending(&session).unwrap(), 0);
    assert_eq!(held.pending_epochs(&session).unwrap().len(), 1);
    drop((held, session));

    save_records(&lab.paths[1], &encode_records(&removal)).unwrap();
    let session = Session::open(&b1).unwrap();
    let new = *session.access.basis();
    assert_ne!(new, old, "the rotation moved the epoch");
    assert!(
        !basis_wraps(&lab, &new, recipient4),
        "CONTROL: the new epoch reaches m4 no more"
    );
    // Opening the table for writing seals it, with no write of its own to carry it.
    let mut store = store::Store::open(&session, &dir, scope).unwrap();
    assert!(store.pending_epochs(&session).unwrap().is_empty());
    assert_eq!(store.seal_pending(&session).unwrap(), 0, "sealed once");
    drop(store);
    assert_eq!(
        sealed_under(&session, &dir, scope, b"during"),
        Some(new),
        "the pending write applies under the new epoch, never the old"
    );
}

/// The negative control of the writer's wait: with `TABLES_DONT_WAIT` on, the write a held
/// `Remove` would keep pending lands at once, under the epoch still wrapped to the removed Mac.
#[test]
fn a_table_write_lands_under_the_old_epoch_when_the_wait_is_off() {
    use aterm_secrets::{client::save_records, membership_log::encode_records};
    let (lab, removal, recipient4, dir, scope) = removal_lab(false);
    save_records(&lab.paths[1], &encode_records(&removal[..1])).unwrap();
    let b1 = lab.bootstrap(1);
    let session = Session::open(&b1).unwrap();
    let old = *session.access.basis();
    super::TABLES_DONT_WAIT.with(|off| off.set(true));
    let landed = store::Store::open(&session, &dir, scope)
        .unwrap()
        .commit_catalog(&session, b"during");
    super::TABLES_DONT_WAIT.with(|off| off.set(false));
    landed.expect("with the wait off the write is sealed at once");
    assert_eq!(sealed_under(&session, &dir, scope, b"during"), Some(old));
    assert!(
        basis_wraps(&lab, &old, recipient4),
        "the row is sealed under the epoch the removed Mac holds"
    );
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

/// A writer session keeps no look ([`Session::keep_look`]) and is lent none
/// ([`Session::lend_look`]): each check a write makes observes the vault on its own call, so a
/// write is judged by the vault as it stands, never by the moment its admission, or a round
/// group's earlier admission, looked. CONTROL: an observed session keeps the look it is given, and
/// judges through one it is lent.
/// NEGATIVE CONTROLS: (2026-11-10) a `keep_look` that keeps any session's look lets the writer
/// keep it (`a writer session keeps no look`); (2026-11-12) a `lend_look` that asks no
/// `Session::reader` lends the writer one (`a writer session is lent no look`).
#[test]
fn a_writer_session_keeps_no_look() {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let mut writer = Session::open(&bootstrap).unwrap();
    assert!(writer.journal.is_writer(), "the fixture's writer session");
    let lent = client::VaultLook::new(bootstrap.paths());
    let refused = writer.keep_look(client::VaultLook::new(bootstrap.paths()));
    assert!(
        refused.is_err() && writer.look().is_none(),
        "a writer session keeps no look: {refused:?}"
    );
    let refused = writer.lend_look(&lent);
    assert!(
        refused.is_err() && writer.look().is_none(),
        "a writer session is lent no look: {refused:?}"
    );
    drop(writer);
    let mut reader = Session::observe(&bootstrap).unwrap();
    reader
        .keep_look(client::VaultLook::new(bootstrap.paths()))
        .unwrap();
    assert!(
        reader.look().is_some(),
        "CONTROL: an observed session keeps its look"
    );
    drop(reader);
    let mut reader = Session::observe(&bootstrap).unwrap();
    reader.lend_look(&lent).unwrap();
    assert!(
        reader.look().is_some_and(|look| std::ptr::eq(look, &lent)),
        "CONTROL: an observed session judges through the look it is lent"
    );
}
