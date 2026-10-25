// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::table::store::Capacity;
use crate::{crypto, table::conflict::Conflict};
use astream_wire::sync::Actor;
use aterm_spec::interp::State;
use std::collections::BTreeSet;

fn packet(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut wire = b"ACTF1".to_vec();
    for source in [a, b] {
        wire.extend_from_slice(&(source.len() as u32).to_le_bytes());
        wire.extend_from_slice(source);
    }
    wire
}

#[test]
fn discarded_source_proof_restricts_without_importing_its_payload() {
    let f = Fixture::new(false, false, true);
    let a = f.lab.bootstrap(0);
    {
        let session = Session::open(&a).unwrap();
        assert!(Conflict::from_discarded(&session, f.scope, &f.wire).is_err());
    }
    f.recover();
    let b = f.lab.bootstrap(1);
    let wire = {
        let session = Session::open(&b).unwrap();
        let before = std::fs::read(f.dirs[1].join("table.astream")).unwrap();
        let store = Store::open_capacity(
            &session,
            &f.dirs[1],
            f.scope,
            false,
            Capacity {
                conflicts: 0,
                ..Capacity::default()
            },
        )
        .unwrap();
        assert!(
            store.conflicts.is_empty(),
            "opening quarantine must not append a proof"
        );
        let proof = store
            .conflict_wire(&session, &session.actor())
            .unwrap()
            .unwrap();
        assert!(proof.starts_with(b"ACTQ1"));
        assert_eq!(
            std::fs::read(f.dirs[1].join("table.astream")).unwrap(),
            before
        );
        proof
    };
    let session = Session::open(&a).unwrap();
    let proof = Conflict::authenticate(&session, f.scope, &wire).unwrap();
    assert_eq!(proof.headers().len(), 1);
    assert_eq!(proof.headers()[0].id, f.receipt.id);
    let actor = proof.actor();
    let mut store = Store::open(&session, &f.dirs[0], f.scope).unwrap();
    let receipt = store.ingest_conflict(&session, &wire).unwrap();
    assert_eq!(store.ingest_conflict(&session, &wire).unwrap(), receipt);
    assert_eq!(store.inventory(&session).unwrap().count(), 0);
    assert_eq!(
        store.conflict_inventory(&session).unwrap(),
        BTreeSet::from([actor])
    );
    let foreign = Scope::new(f.scope.trust(), *f.scope.table(), 2).unwrap();
    assert!(Conflict::authenticate(&session, foreign, &wire).is_err());
    for mutation in [0, 5, 37, wire.len() - 1] {
        let mut bad = wire.clone();
        bad[mutation] ^= 1;
        assert!(store.ingest_conflict(&session, &bad).is_err());
    }
    let mut trailing = wire.clone();
    trailing.push(0);
    assert!(store.ingest_conflict(&session, &trailing).is_err());
    assert!(
        store
            .ingest_conflict(&session, &wire[..wire.len() - 1])
            .is_err()
    );
    drop(store);
    let store = Store::open(&session, &f.dirs[0], f.scope).unwrap();
    assert_eq!(
        store.conflict_wire(&session, &actor).unwrap().unwrap(),
        wire
    );
    assert_eq!(store.inventory(&session).unwrap().count(), 0);
}

fn reselection_trace(corrupt_payload: bool) -> Vec<State> {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let a = lab.bootstrap(0);
    let b = lab.bootstrap(1);
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let grants = || {
        vec![
            Grant::recorded("m1", a.request().unwrap().binding().clone(), Role::Writer),
            Grant::recorded("m2", b.request().unwrap().binding().clone(), Role::Writer),
        ]
    };
    let journal = Journal::open_existing(a.journal_dir(), a.trust()).unwrap();
    let left = admin.prepare_commons_rotation(&journal, grants()).unwrap();
    let right = admin.prepare_commons_rotation(&journal, grants()).unwrap();
    drop(journal);
    ingest(&lab, &left);
    let session = Session::open(&a).unwrap();
    let scope = session.scope().unwrap();
    let mut source = session
        .seal(
            scope,
            Causal {
                sequence: 1,
                previous: None,
                parents: &[],
            },
            b"selected again",
        )
        .unwrap();
    if corrupt_payload {
        let at = source.len() - 65;
        source[at] ^= 1;
        crate::table::tests::resign(&a, &mut source);
    }
    drop(session);
    let journal = Journal::open_existing(a.journal_dir(), a.trust()).unwrap();
    let left_child = admin.prepare_commons_rotation(&journal, grants()).unwrap();
    drop(journal);
    let bad = admin
        .seal_commons_restricted(
            a.trust().network,
            Edit::Recover {
                heads: &[left.id(), right.id()],
                selected: right.id(),
            },
            &[],
        )
        .unwrap();
    ingest(&lab, &right);
    ingest(&lab, &bad);
    lab.rotate(&[Role::Writer, Role::Writer]);
    let journal = Journal::open_existing(a.journal_dir(), a.trust()).unwrap();
    let bad_child = match journal.state() {
        aterm_secrets::commons::journal::State::Ready { head, .. } => head,
        _ => panic!("ready recovery"),
    };
    drop(journal);
    let session = Session::open(&a).unwrap();
    let proof = Conflict::from_discarded(&session, scope, &source)
        .unwrap()
        .wire()
        .to_vec();
    let dir = lab._home.path().join("selected-again");
    let mut store = Store::join(&session, &dir, scope).unwrap();
    store.ingest_conflict(&session, &proof).unwrap();
    assert!(store.liability_evidence(&session).is_err());
    let writable = store
        .commit_catalog(&session, b"cannot skip quarantined source")
        .is_ok();
    let mut trace = vec![reselection_state(0, &store, &session, writable)];
    drop((store, session));
    ingest(&lab, &left_child);
    let good = admin
        .seal_commons_restricted(
            a.trust().network,
            Edit::Recover {
                heads: &[bad_child, left_child.id()],
                selected: left_child.id(),
            },
            &[],
        )
        .unwrap();
    ingest(&lab, &good);
    lab.rotate(&[Role::Writer, Role::Writer]);
    let session = Session::open(&a).unwrap();
    assert!(!session.discarded_source(&session.envelope(scope, &source).unwrap()));
    assert!(Conflict::authenticate(&session, scope, &proof).is_err());
    assert!(!Conflict::restore(&session, scope, &proof).unwrap().active());
    let mut store = Store::open(&session, &dir, scope).unwrap();
    assert!(store.proof_inventory(&session).unwrap().is_empty());
    assert!(
        store.catalog(&session).unwrap().is_empty(),
        "a negative proof never admits payload"
    );
    let writable = store
        .commit_catalog(&session, b"cannot skip known source")
        .is_ok();
    trace.push(reselection_state(1, &store, &session, writable));
    if corrupt_payload {
        assert!(store.ingest(&session, &source).is_err());
        assert!(store.metadata_evidence(&session).unwrap().is_empty());
        assert!(store.liability_evidence(&session).unwrap().is_empty());
        return trace;
    }
    assert!(
        store.liability_evidence(&session).is_err(),
        "a valid outstanding source must not silently refund a liability"
    );
    store.ingest(&session, &source).unwrap();
    assert_eq!(store.catalog(&session).unwrap().len(), 1);
    assert_eq!(store.liability_evidence(&session).unwrap().len(), 1);
    let writable = store
        .commit_catalog(&session, b"can extend authenticated history")
        .is_ok();
    trace.push(reselection_state(2, &store, &session, writable));
    trace
}

fn reselection_state(phase: i64, store: &Store, session: &Session<'_>, writable: bool) -> State {
    State::from([
        ("phase", phase),
        ("stored", i64::from(!store.conflicts.is_empty())),
        (
            "active",
            i64::from(!store.quarantined(session).unwrap().is_empty()),
        ),
        (
            "positive",
            i64::from(store.inventory(session).unwrap().next().is_some()),
        ),
        ("writable", i64::from(writable)),
        (
            "liability",
            i64::from(store.liability_evidence(session).is_ok()),
        ),
    ])
}

pub(crate) fn reselection_transition(state: &State, action: &str) -> State {
    let trace = reselection_trace(false);
    let index = match action {
        "Reselect" => 0,
        "Admit" => 1,
        _ => panic!("unknown reselection action"),
    };
    assert_eq!(*state, trace[index]);
    trace[index + 1].clone()
}

#[test]
fn reselected_invalid_ciphertext_does_not_permanently_freeze_liabilities() {
    reselection_trace(true);
}

#[test]
fn an_overruled_recovery_cannot_restrict_a_selected_source() {
    let model = aterm_models_db::source_reselection_model();
    aterm_spec::interp::prove_and_catch(&model);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = reselection_transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&next));
                let mut wrong = next;
                wrong.insert("stored", 0);
                assert!(!model.check_invariant("RetainedEvidence", &wrong));
                assert!(!model.successors(action.name, &state).contains(&wrong));
            }
        }
    }
}

fn repair_fixture(f: &Fixture, repairs: &mut [crate::control::Repair; 2]) {
    use crate::control::{Endpoint, Side};
    use aterm_net::lan::{Connection, Scope as LinkScope};
    use std::{net::TcpListener, thread, time::Duration};
    let endpoints: Vec<_> = (0..2)
        .map(|n| {
            Endpoint::from_bootstrap(&f.lab.bootstrap(n))
                .unwrap()
                .unwrap()
                .with_tables(f.lab.paths[n].clone(), vec![(f.scope, f.dirs[n].clone())])
                .unwrap()
        })
        .collect();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let [ra, rb] = repairs;
    thread::scope(|threads| {
        let server = threads.spawn(|| {
            endpoints[1].exchange(
                Connection::admit(listener.accept().unwrap().0, LinkScope::Loopback).unwrap(),
                Side::Responder,
                rb,
            )
        });
        endpoints[0]
            .exchange(
                Connection::connect(address, LinkScope::Loopback, Duration::from_secs(60)).unwrap(),
                Side::Initiator,
                ra,
            )
            .unwrap();
        server.join().unwrap().unwrap();
    });
    assert!(
        ra.application().is_some() && rb.application().is_some(),
        "{ra:?}\n{rb:?}"
    );
}

#[test]
fn an_existing_quarantine_proof_does_not_hide_a_later_fork() {
    let f = Fixture::new(true, false, false);
    f.recover();
    let mut repairs = [
        crate::control::Repair::default(),
        crate::control::Repair::default(),
    ];
    repair_fixture(&f, &mut repairs);
    let b = f.lab.bootstrap(1);
    let sb = Session::open(&b).unwrap();
    let target = Store::open(&sb, &f.dirs[1], f.scope).unwrap();
    let source = &target.metadata_evidence(&sb).unwrap()[0];
    assert!(!source.catalog_eligible && source.record.is_none());
    assert_eq!(source.header.id, f.receipt.id);
    assert_ne!(
        source.receipt.id, source.header.id,
        "receipt identifies the negative certificate"
    );
    assert!(
        target
            .proof_inventory(&sb)
            .unwrap()
            .iter()
            .all(|key| key[48] == 0)
    );
    drop((target, sb));
    let a = f.lab.bootstrap(0);
    let sa = Session::open(&a).unwrap();
    let mut source = Store::open(&sa, &f.dirs[0], f.scope).unwrap();
    let fork = sa
        .seal(
            f.scope,
            Causal {
                sequence: 1,
                previous: None,
                parents: &[],
            },
            b"new fork",
        )
        .unwrap();
    source.ingest(&sa, &fork).unwrap();
    assert!(
        source
            .proof_inventory(&sa)
            .unwrap()
            .iter()
            .all(|key| key[48] == 1)
    );
    drop((source, sa));
    repair_fixture(&f, &mut repairs);
    assert_eq!(repairs[1].application().unwrap().conflicts_received, 1);
    let sb = Session::open(&b).unwrap();
    let mut target = Store::open(&sb, &f.dirs[1], f.scope).unwrap();
    assert!(
        target
            .proof_inventory(&sb)
            .unwrap()
            .iter()
            .all(|key| key[48] == 1)
    );
    assert!(
        target.inventory(&sb).unwrap().next().is_none(),
        "proofs do not admit payload"
    );
    target
        .commit_catalog(&sb, b"independent writer remains available")
        .unwrap();
}

#[test]
fn discarded_sources_cannot_consume_reserved_fork_capacity() {
    let f = Fixture::new(false, false, true);
    f.recover();
    let a = f.lab.bootstrap(0);
    let b = f.lab.bootstrap(1);
    let sa = Session::open(&a).unwrap();
    let sb = Session::open(&b).unwrap();
    let q = Conflict::from_discarded(&sa, f.scope, &f.wire).unwrap();
    let forked = sb
        .seal(
            f.scope,
            Causal {
                sequence: 1,
                previous: None,
                parents: &[],
            },
            b"fork",
        )
        .unwrap();
    let fork = Conflict::from_pair(&sa, f.scope, &f.wire, &forked).unwrap();
    let capacity = Capacity {
        operations: 1,
        bytes: f.wire.len(),
        conflicts: 1,
    };
    let mut store = Store::open_capacity(&sa, &f.dirs[0], f.scope, false, capacity).unwrap();
    store.ingest_conflict(&sa, q.wire()).unwrap();
    store.ingest_conflict(&sa, fork.wire()).unwrap();
    assert_eq!(store.conflicts.len(), 2);
    assert_eq!(
        store.proof_inventory(&sa).unwrap(),
        BTreeSet::from([fork.key()])
    );
    assert_eq!(store.inventory(&sa).unwrap().count(), 0);
    drop(store);
    let store = Store::open_capacity(&sa, &f.dirs[0], f.scope, false, capacity).unwrap();
    assert_eq!(
        store.proof_inventory(&sa).unwrap(),
        BTreeSet::from([fork.key()])
    );
}

struct Evidence {
    lab: Lab,
    scope: Scope,
    dir: PathBuf,
    wires: [Vec<u8>; 2],
    wire: Vec<u8>,
    actor: Actor,
}
impl Evidence {
    fn new() -> Self {
        let lab = Lab::new(&[Role::Writer, Role::Writer]);
        let bootstrap = lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        let scope = session.scope().unwrap();
        let causal = || Causal {
            sequence: 1,
            previous: None,
            parents: &[],
        };
        let wires = [
            session.seal(scope, causal(), b"branch one").unwrap(),
            session.seal(scope, causal(), b"branch two").unwrap(),
        ];
        let wire = Conflict::from_pair(&session, scope, &wires[0], &wires[1])
            .unwrap()
            .wire()
            .to_vec();
        let actor = session.actor();
        let dir = lab._home.path().join("conflict");
        Store::join(&session, &dir, scope).unwrap();
        drop(session);
        Self {
            lab,
            scope,
            dir,
            wires,
            wire,
            actor,
        }
    }
}

#[test]
fn certificates_require_exact_scope_signatures_canonical_sources_and_registered_identity() {
    let f = Evidence::new();
    let bootstrap = f.lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let evidence = Conflict::authenticate(&session, f.scope, &f.wire).unwrap();
    assert_eq!(evidence.actor(), f.actor);
    assert!(evidence.headers()[0].id < evidence.headers()[1].id);
    let foreign = Scope::new(f.scope.trust(), *f.scope.table(), 2).unwrap();
    assert!(Conflict::authenticate(&session, foreign, &f.wire).is_err());
    assert!(Conflict::from_pair(&session, f.scope, &f.wires[0], &f.wires[0]).is_err());
    let mut sources = f.wires.clone();
    sources.sort_by_key(|wire| aterm_digest::Sha256::digest(wire));
    assert!(Conflict::authenticate(&session, f.scope, &packet(&sources[1], &sources[0])).is_err());
    for mutation in [0, 5, f.wire.len() - 1] {
        let mut wire = f.wire.clone();
        wire[mutation] ^= 1;
        assert!(Conflict::authenticate(&session, f.scope, &wire).is_err());
    }
    let mut trailing = f.wire.clone();
    trailing.push(0);
    assert!(Conflict::authenticate(&session, f.scope, &trailing).is_err());
    assert!(Conflict::authenticate(&session, f.scope, &f.wire[..f.wire.len() - 1]).is_err());
    let unknown = [0x73; 32];
    let at = 5 + crate::table::SCOPE_LEN + 32 + 8;
    for wire in &mut sources {
        wire[at..at + 32].copy_from_slice(&crypto::public(&unknown).unwrap());
        let end = wire.len() - 64;
        let signature = crypto::signed(&unknown, crate::table::DOMAIN, &wire[..end]).unwrap();
        wire[end..].copy_from_slice(&signature);
    }
    let error = Conflict::from_pair(&session, f.scope, &sources[0], &sources[1])
        .err()
        .unwrap();
    assert!(error.0.contains("enrollment"));
}

fn conflict_transition(state: &State, action: &str) -> State {
    let names = [
        "Receive",
        "Restart",
        "Tail",
        "Rollback",
        "OwnReset",
        "WrongScope",
        "Forged",
        "Retired",
        "KnownOperation",
    ];
    if action == "Replicate" {
        return tls_transition(state);
    }
    if action == "Capacity" {
        return capacity_transition(state);
    }
    let scenario = if action == "KnownOperation" {
        10
    } else {
        names.iter().position(|name| *name == action).unwrap() as i64 + 1
    };
    let f = Evidence::new();
    let bootstrap = f.lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let mut store = Store::open(&session, &f.dir, f.scope).unwrap();
    let mut next = state.clone();
    next.insert("scenario", scenario);
    if matches!(action, "WrongScope" | "Forged") {
        let mut wire = f.wire.clone();
        if action == "Forged" {
            *wire.last_mut().unwrap() ^= 1;
        } else {
            let scope = Scope::new(f.scope.trust(), *f.scope.table(), 2).unwrap();
            wire = Conflict::from_pair(
                &session,
                scope,
                &session
                    .seal(
                        scope,
                        Causal {
                            sequence: 1,
                            previous: None,
                            parents: &[],
                        },
                        b"x",
                    )
                    .unwrap(),
                &session
                    .seal(
                        scope,
                        Causal {
                            sequence: 1,
                            previous: None,
                            parents: &[],
                        },
                        b"y",
                    )
                    .unwrap(),
            )
            .unwrap()
            .wire()
            .to_vec();
        }
        next.insert(
            "acknowledged",
            i64::from(store.ingest_conflict(&session, &wire).is_ok()),
        );
        next.insert("usable", i64::from(store.inventory(&session).is_ok()));
        return next;
    }
    if action == "KnownOperation" {
        store.ingest(&session, &f.wires[0]).unwrap();
    }
    let before_log = std::fs::read(f.dir.join("table.astream")).unwrap();
    let before_checkpoint = std::fs::read(f.dir.join("table.checkpoint")).unwrap();
    if action == "Tail" {
        let mut body = 7u64.to_le_bytes().to_vec();
        body.extend_from_slice(&f.wire);
        store
            .log
            .append(super::super::super::CONFLICT_SUBJECT, body)
            .unwrap();
    } else {
        let receipt = store.ingest_conflict(&session, &f.wire).unwrap();
        assert_eq!(store.ingest_conflict(&session, &f.wire).unwrap(), receipt);
        assert_eq!(
            store.witness.count,
            if action == "KnownOperation" { 2 } else { 1 }
        );
        next.insert("acknowledged", 1);
    }
    drop(store);
    if action == "Rollback" {
        std::fs::write(f.dir.join("table.astream"), before_log).unwrap();
        std::fs::write(f.dir.join("table.checkpoint"), before_checkpoint).unwrap();
    }
    drop(session);
    if action == "Retired" {
        f.lab.rotate(&[Role::Reader, Role::Writer]);
        let a = f.lab.bootstrap(0);
        let b = f.lab.bootstrap(1);
        let admin = client::admin_open(&f.lab.paths[0]).unwrap();
        let mut journal = Journal::open_existing(b.journal_dir(), b.trust()).unwrap();
        let retirement = admin
            .prepare_commons_retirement(
                &journal,
                vec![Grant::recorded(
                    "m2",
                    b.request().unwrap().binding().clone(),
                    Role::Writer,
                )],
                &[a.request().unwrap().binding().actor()],
            )
            .unwrap();
        journal.ingest(retirement.bytes()).unwrap();
        drop(journal);
        let session = Session::open(&b).unwrap();
        assert!(Conflict::authenticate(&session, f.scope, &f.wire).is_ok());
        let dir = f.lab._home.path().join("retired");
        let mut store = Store::join(&session, &dir, f.scope).unwrap();
        store.ingest_conflict(&session, &f.wire).unwrap();
        return measure(next, &store, &session, false);
    }
    let session = Session::open(&bootstrap).unwrap();
    let Ok(mut store) = Store::open(&session, &f.dir, f.scope) else {
        return next;
    };
    let writable = if action == "OwnReset" {
        store.commit_catalog(&session, b"reset").is_ok()
    } else {
        false
    };
    let retained = store.inventory(&session).unwrap().count();
    assert_eq!(
        retained,
        usize::from(action == "KnownOperation"),
        "certificate must not create operations"
    );
    let mut next = measure(next, &store, &session, writable);
    next.insert("imported", 0);
    next
}

fn measure(mut next: State, store: &Store, session: &Session<'_>, writable: bool) -> State {
    next.insert("usable", 1);
    next.insert("imported", store.inventory(session).unwrap().count() as i64);
    next.insert(
        "restricted",
        store.conflict_inventory(session).unwrap().len() as i64,
    );
    next.insert("visible", store.catalog(session).unwrap().len() as i64);
    next.insert(
        "liability",
        i64::from(store.liability_evidence(session).is_ok()),
    );
    next.insert("writable", i64::from(writable));
    next
}

#[test]
fn conflict_custody_conforms_with_unsafe_negative_controls() {
    let model = aterm_models_db::table_conflict_model();
    aterm_spec::interp::prove_and_catch(&model);
    let state = model.init_state();
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    for action in &model.actions {
        let actual = conflict_transition(&state, action.name);
        assert!(
            model.successors(action.name, &state).contains(&actual),
            "{}: {actual:?}",
            action.name
        );
        let mut bad = actual.clone();
        let law = match action.name {
            "WrongScope" | "Forged" => {
                bad.insert("acknowledged", 1);
                "AuthenticateBeforeAck"
            }
            "Rollback" => {
                bad.insert("usable", 1);
                "LostWitnessRefuses"
            }
            "OwnReset" => {
                bad.insert("writable", 1);
                "NoWriterReset"
            }
            _ => {
                bad.insert("restricted", 0);
                "RetainedRestriction"
            }
        };
        assert!(!model.check_invariant(law, &bad));
        if action.name == "Receive" {
            let mut denied = actual.clone();
            denied.insert("liability", 0);
            assert!(!model.check_invariant("UnrelatedLiabilityAvailable", &denied));
        }
        if action.name == "Replicate" {
            let mut visible = actual.clone();
            visible.insert("visible", 1);
            assert!(!model.check_invariant("NoPayloadAdmission", &visible));
        }
        if matches!(action.name, "KnownOperation" | "Replicate") {
            let mut refunded = actual.clone();
            refunded.insert("liability", 1);
            assert!(!model.check_invariant("NoFalseLiability", &refunded));
        }
    }
}

fn tls_transition(state: &State) -> State {
    use crate::control::{Endpoint, Repair, Side};
    use aterm_net::lan::{Connection, Scope as LinkScope};
    use std::{net::TcpListener, thread, time::Duration};

    let f = Fixture::new(true, false, false);
    f.recover();
    let a = f.lab.bootstrap(0);
    let b = f.lab.bootstrap(1);
    let sa = Session::open(&a).unwrap();
    let sb = Session::open(&b).unwrap();
    let mut source = Store::open(&sa, &f.dirs[0], f.scope).unwrap();
    let mut target = Store::open(&sb, &f.dirs[1], f.scope).unwrap();
    let free = target
        .commit_catalog(&sb, b"previously shared branch")
        .unwrap();
    source
        .ingest(&sa, target.wire(&sb, &free.id).unwrap().unwrap())
        .unwrap();
    assert_eq!(source.catalog(&sa).unwrap().len(), 1);
    assert_eq!(target.catalog(&sb).unwrap().len(), 1);
    let dependent = sb
        .seal(
            f.scope,
            Causal {
                sequence: 1,
                previous: None,
                parents: &[f.receipt.id],
            },
            b"late branch depends on discarded operation",
        )
        .unwrap();
    source.ingest(&sa, &dependent).unwrap();
    assert!(source.catalog(&sa).unwrap().is_empty());
    assert!(source.replication_inventory(&sa).unwrap().next().is_none());
    assert_eq!(
        target.catalog(&sb).unwrap().len(),
        1,
        "remote peer has not learned the fork"
    );
    drop((source, target, sa, sb));
    let endpoint = |n| {
        Endpoint::from_bootstrap(&f.lab.bootstrap(n))
            .unwrap()
            .unwrap()
            .with_tables(f.lab.paths[n].clone(), vec![(f.scope, f.dirs[n].clone())])
            .unwrap()
    };
    let ea = endpoint(0);
    let eb = endpoint(1);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let mut ra = Repair::default();
    let mut rb = Repair::default();
    thread::scope(|threads| {
        let server = threads.spawn(|| {
            eb.exchange(
                Connection::admit(listener.accept().unwrap().0, LinkScope::Loopback).unwrap(),
                Side::Responder,
                &mut rb,
            )
        });
        ea.exchange(
            Connection::connect(address, LinkScope::Loopback, Duration::from_secs(2)).unwrap(),
            Side::Initiator,
            &mut ra,
        )
        .unwrap();
        server.join().unwrap().unwrap();
    });
    // The discarded source's proof and the dependent writer's fork are both
    // permanent restrictions, transferred before any positive operation.
    assert_eq!(ra.application().unwrap().conflicts_sent, 2);
    assert_eq!(rb.application().unwrap().conflicts_received, 2);
    assert_eq!(ra.application().unwrap().sent, 0);
    let sb = Session::open(&b).unwrap();
    let mut target = Store::open(&sb, &f.dirs[1], f.scope).unwrap();
    assert_eq!(
        target.inventory(&sb).unwrap().count(),
        1,
        "no embedded operation was admitted"
    );
    assert!(target.catalog(&sb).unwrap().is_empty());
    assert!(target.liability_evidence(&sb).is_err());
    let writable = target
        .commit_catalog(&sb, b"restart the conflicted identity")
        .is_ok();
    assert!(!writable);
    assert_eq!(target.conflict_inventory(&sb).unwrap().len(), 2);
    let mut next = measure(state.clone(), &target, &sb, writable);
    next.insert("scenario", 9);
    next.insert(
        "imported",
        target.inventory(&sb).unwrap().count() as i64 - 1,
    );
    next.insert(
        "acknowledged",
        rb.application().unwrap().conflicts_received as i64,
    );
    next
}

#[test]
fn a_certificate_does_not_poison_an_unrelated_writer_or_grant_embedded_payloads() {
    let f = Evidence::new();
    let a = f.lab.bootstrap(0);
    let b = f.lab.bootstrap(1);
    let sa = Session::open(&a).unwrap();
    let sb = Session::open(&b).unwrap();
    let dir = f.lab._home.path().join("unrelated");
    let mut store = Store::join(&sb, &dir, f.scope).unwrap();
    store.ingest_conflict(&sb, &f.wire).unwrap();
    let receipt = store
        .commit_catalog(&sb, b"unrelated account metadata")
        .unwrap();
    assert_eq!(store.catalog(&sb).unwrap().len(), 1);
    assert_eq!(store.catalog(&sb).unwrap()[0].header().id, receipt.id);
    assert!(store.ingest(&sb, &f.wires[0]).is_err());
    assert!(
        store.liability_evidence(&sb).is_ok(),
        "an unrelated certificate cannot freeze this writer"
    );
    let mut malformed_payload = f.wires[0].clone();
    let end = malformed_payload.len() - 64;
    malformed_payload[end - 1] ^= 1;
    crate::table::tests::resign(&a, &mut malformed_payload);
    assert!(sa.authenticate(f.scope, &malformed_payload).is_err());
    let negative = Conflict::from_pair(&sa, f.scope, &malformed_payload, &f.wires[1]).unwrap();
    assert_eq!(
        negative.actor(),
        sa.actor(),
        "a valid signature is restrictive even without valid AEAD"
    );
}

#[test]
fn a_bound_reader_can_only_restrict_its_own_identity() {
    let lab = Lab::new(&[Role::Writer, Role::Reader]);
    let a = lab.bootstrap(0);
    let b = lab.bootstrap(1);
    let sa = Session::open(&a).unwrap();
    let sb = Session::open(&b).unwrap();
    let scope = sa.scope().unwrap();
    let mut sources = [
        sa.seal(
            scope,
            Causal {
                sequence: 1,
                previous: None,
                parents: &[],
            },
            b"first",
        )
        .unwrap(),
        sa.seal(
            scope,
            Causal {
                sequence: 1,
                previous: None,
                parents: &[],
            },
            b"second",
        )
        .unwrap(),
    ];
    for wire in &mut sources {
        let at = 5 + crate::table::SCOPE_LEN + 32 + 8;
        wire[at..at + 48].copy_from_slice(&sb.actor());
        crate::table::tests::resign(&b, wire);
        assert!(sa.authenticate(scope, wire).is_err());
    }
    let certificate = Conflict::from_pair(&sa, scope, &sources[0], &sources[1]).unwrap();
    let mut store = Store::join(&sa, &lab._home.path().join("reader-proof"), scope).unwrap();
    store.ingest_conflict(&sa, certificate.wire()).unwrap();
    assert_eq!(
        store.conflict_inventory(&sa).unwrap(),
        [sb.actor()].into_iter().collect()
    );
    assert!(store.commit_catalog(&sa, b"writer still works").is_ok());
    assert_eq!(store.catalog(&sa).unwrap().len(), 1);
    assert!(
        store.liability_evidence(&sa).is_ok(),
        "a reader cannot freeze unrelated liabilities"
    );
}

#[test]
fn invalid_crash_tail_never_advances_the_independent_witness() {
    let f = Evidence::new();
    let bootstrap = f.lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let mut store = Store::open(&session, &f.dir, f.scope).unwrap();
    let claim = store.claim.clone();
    let claim_path = store.claim_dir.join(&store.claim_name);
    let mut malformed = f.wire;
    *malformed.last_mut().unwrap() ^= 1;
    let mut body = 7u64.to_le_bytes().to_vec();
    body.extend_from_slice(&malformed);
    store
        .log
        .append(super::super::super::CONFLICT_SUBJECT, body)
        .unwrap();
    drop(store);
    assert!(Store::open(&session, &f.dir, f.scope).is_err());
    assert_eq!(std::fs::read(claim_path).unwrap(), claim);
}

fn capacity_transition(state: &State) -> State {
    use super::super::super::{Capacity, MAX_RECORDS};
    let f = Evidence::new();
    let bootstrap = f.lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let capacity = Capacity {
        operations: 1,
        bytes: f.wires[0].len(),
        conflicts: 1,
    };
    let mut store = Store::open_capacity(&session, &f.dir, f.scope, false, capacity).unwrap();
    store.ingest(&session, &f.wires[0]).unwrap();
    assert!(
        store
            .ingest(&session, &f.wires[1])
            .err()
            .unwrap()
            .0
            .contains("capacity")
    );
    let payload = vec![0x63; crypto::MAX_BODY];
    let causal = || Causal {
        sequence: 1,
        previous: None,
        parents: &[],
    };
    let a = session.seal(f.scope, causal(), &payload).unwrap();
    let b = session.seal(f.scope, causal(), &payload).unwrap();
    let certificate = Conflict::from_pair(&session, f.scope, &a, &b).unwrap();
    let receipt = store.ingest_conflict(&session, certificate.wire()).unwrap();
    drop(store);
    let mut store = Store::open_capacity(&session, &f.dir, f.scope, false, capacity).unwrap();
    assert_eq!(
        store.ingest_conflict(&session, certificate.wire()).unwrap(),
        receipt
    );
    assert_eq!(store.inventory(&session).unwrap().count(), 1);
    // The global custody counter also reserves slots beyond a full positive log.
    let mut bytes = [1; 40];
    bytes[..8].copy_from_slice(&(MAX_RECORDS as u64).to_le_bytes());
    let advanced = Witness::decode(&bytes)
        .unwrap()
        .advance(MAX_RECORDS as u64, receipt.id, 7)
        .unwrap();
    assert!(Witness::decode(&advanced.bytes()).is_ok());
    let mut next = measure(state.clone(), &store, &session, false);
    next.insert("scenario", 11);
    next.insert("acknowledged", 1);
    next.insert("imported", 0);
    next
}

#[test]
fn certificate_sources_withhold_descendants_even_without_positive_parent_records() {
    let f = Evidence::new();
    let bootstrap = f.lab.bootstrap(1);
    let session = Session::open(&bootstrap).unwrap();
    let parent = aterm_digest::Sha256::digest(&f.wires[0]);
    let child = session
        .seal(
            f.scope,
            Causal {
                sequence: 1,
                previous: None,
                parents: &[parent],
            },
            b"dependent",
        )
        .unwrap();
    let mut store = Store::join(
        &session,
        &f.lab._home.path().join("missing-parent"),
        f.scope,
    )
    .unwrap();
    store.ingest(&session, &child).unwrap();
    assert_eq!(store.replication_inventory(&session).unwrap().count(), 1);
    store.ingest_conflict(&session, &f.wire).unwrap();
    assert_eq!(store.inventory(&session).unwrap().count(), 1);
    assert_eq!(store.replication_inventory(&session).unwrap().count(), 0);
}

#[test]
fn a_signed_predecessor_claim_cannot_poison_another_actors_actual_record() {
    let f = Evidence::new();
    let a = f.lab.bootstrap(0);
    let b = f.lab.bootstrap(1);
    let sa = Session::open(&a).unwrap();
    let sb = Session::open(&b).unwrap();
    let mut store = Store::join(&sb, &f.lab._home.path().join("honest"), f.scope).unwrap();
    let honest = store.commit_catalog(&sb, b"independent").unwrap();
    let false_claim = sa
        .seal(
            f.scope,
            Causal {
                sequence: 2,
                previous: Some(honest.id),
                parents: &[honest.id],
            },
            b"false ownership claim",
        )
        .unwrap();
    let proof = Conflict::from_pair(&sa, f.scope, &f.wires[0], &false_claim).unwrap();
    store.ingest_conflict(&sb, proof.wire()).unwrap();
    assert_eq!(store.catalog(&sb).unwrap().len(), 1);
    assert_eq!(store.replication_inventory(&sb).unwrap().count(), 1);
    assert!(store.liability_evidence(&sb).is_ok());
}

#[test]
fn retained_but_withheld_operations_do_not_bounce_between_replicas() {
    use crate::{
        control::Side,
        replica::{Repair, Replica},
    };
    use std::{os::unix::net::UnixStream, thread, time::Duration};
    let f = Evidence::new();
    let a = f.lab.bootstrap(0);
    let b = f.lab.bootstrap(1);
    let sa = Session::open(&a).unwrap();
    let sb = Session::open(&b).unwrap();
    let parent = aterm_digest::Sha256::digest(&f.wires[0]);
    let causal = || Causal {
        sequence: 2,
        previous: Some(parent),
        parents: std::slice::from_ref(&parent),
    };
    let branch = sa.seal(f.scope, causal(), b"second").unwrap();
    let other = sa.seal(f.scope, causal(), b"other second").unwrap();
    let proof = Conflict::from_pair(&sa, f.scope, &branch, &other).unwrap();
    let child = sb
        .seal(
            f.scope,
            Causal {
                sequence: 1,
                previous: None,
                parents: &[parent],
            },
            b"dependent",
        )
        .unwrap();
    let dirs = [f.dir.clone(), f.lab._home.path().join("other-replica")];
    let mut left = Store::open(&sa, &dirs[0], f.scope).unwrap();
    let mut right = Store::join(&sb, &dirs[1], f.scope).unwrap();
    left.ingest(&sa, &f.wires[0]).unwrap();
    left.ingest(&sa, &child).unwrap();
    right.ingest(&sb, &child).unwrap();
    left.ingest_conflict(&sa, proof.wire()).unwrap();
    right.ingest_conflict(&sb, proof.wire()).unwrap();
    assert_eq!(left.replication_inventory(&sa).unwrap().count(), 0);
    assert_eq!(
        right.replication_inventory(&sb).unwrap().count(),
        1,
        "its predecessor is unknown locally"
    );
    let actors = [sa.actor(), sb.actor()];
    drop((left, right, sa, sb));
    let replicas: Vec<_> = (0..2)
        .map(|n| Replica::new(f.lab.paths[n].clone(), vec![(f.scope, dirs[n].clone())]).unwrap())
        .collect();
    let mut left_repair = Repair::default();
    let mut right_repair = Repair::default();
    for _ in 0..2 {
        let (mut left, mut right) = UnixStream::pair().unwrap();
        for stream in [&left, &right] {
            stream
                .set_read_timeout(Some(Duration::from_secs(60)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(60)))
                .unwrap();
        }
        let (sent, received) = thread::scope(|threads| {
            let peer = threads.spawn(|| {
                replicas[1]
                    .exchange(&mut right, actors[0], Side::Responder, &mut right_repair)
                    .unwrap()
            });
            let local = replicas[0]
                .exchange(&mut left, actors[1], Side::Initiator, &mut left_repair)
                .unwrap();
            (local, peer.join().unwrap())
        });
        assert_eq!(
            (sent.sent, sent.received, received.sent, received.received),
            (0, 0, 0, 0)
        );
        assert_eq!(sent.deferred + received.deferred, 0);
    }
}
