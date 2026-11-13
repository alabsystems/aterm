// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    crypto::Causal,
    table::{
        Scope, Session,
        store::{Store, WITNESS_AT},
        tests::Lab,
    },
};
use astream_wire::sync::Digest;
use aterm_secrets::{
    client,
    commons::{
        journal::Journal,
        node::{Edit, Node},
        roster::{Grant, Role},
    },
};
use std::path::{Path, PathBuf};

mod closure;
mod conflict;
mod identified;
mod lazy_index;
mod membership;
mod policy;

#[test]
fn transient_control_uncertainty_invalidates_a_discarded_metadata_batch() {
    let fixture = Fixture::new(false, false, false);
    fixture.recover();
    let bootstrap = fixture.lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let envelope = Envelope::parse(fixture.scope, &fixture.wire).unwrap();
    let anchor = bootstrap.journal_dir().join("control.anchor");
    let moved = anchor.with_extension("moved");
    std::fs::rename(&anchor, &moved).unwrap();
    assert!(session.discarded_source(&envelope).is_err());
    std::fs::rename(&moved, &anchor).unwrap();
    let result = session.with_read_batch(fixture.scope, |batch| {
        std::fs::rename(&anchor, &moved).unwrap();
        let classification = batch.discarded_source(&envelope);
        std::fs::rename(&moved, &anchor).unwrap();
        // Even a caller that handles the classification failure cannot forget
        // that this batch observed an unavailable control journal.
        Ok(classification)
    });
    assert!(result.is_err(), "batch forgot its unavailable control view");
    session
        .with_read_batch(fixture.scope, |batch| {
            assert!(batch.discarded_source(&envelope)?);
            Ok(())
        })
        .unwrap();
}

#[test]
fn discarded_metadata_batches_withhold_changed_control_or_custody() {
    for changed in [
        "control",
        "control.anchor",
        "source.toml",
        "commons/device.age",
    ] {
        let fixture = Fixture::new(false, false, false);
        fixture.recover();
        let bootstrap = fixture.lab.bootstrap(0);
        let session = Session::observe(&bootstrap).unwrap();
        let envelope = Envelope::parse(fixture.scope, &fixture.wire).unwrap();
        let admin = client::admin_open(&fixture.lab.paths[0]).unwrap();
        let grants = (0..2)
            .map(|n| {
                Grant::recorded(
                    format!("m{}", n + 1),
                    fixture
                        .lab
                        .bootstrap(n)
                        .request()
                        .unwrap()
                        .binding()
                        .clone(),
                    Role::Writer,
                )
            })
            .collect();
        let update = admin
            .prepare_commons_rotation(&session.journal, grants)
            .unwrap();
        session
            .with_read_batch(fixture.scope, |batch| {
                assert!(batch.discarded_source(&envelope)?);
                assert!(batch.discarded_source(&envelope)?);
                Ok(())
            })
            .unwrap();
        let result = session.with_read_batch(fixture.scope, |batch| {
            assert!(batch.discarded_source(&envelope)?);
            if changed == "control" {
                Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust())
                    .unwrap()
                    .ingest(update.bytes())
                    .unwrap();
            } else {
                let path = if changed == "control.anchor" {
                    bootstrap.journal_dir().join(changed)
                } else {
                    fixture.lab.paths[0].dir.join(changed)
                };
                std::fs::rename(&path, path.with_extension("moved")).unwrap();
            }
            // No key is opened by this batch. Its final context/custody fence
            // must reject even this previously valid cached classification.
            batch.discarded_source(&envelope)
        });
        assert!(
            result.is_err(),
            "discarded metadata escaped after {changed}"
        );
    }
}

#[test]
fn a_changed_observation_refuses_reads_until_a_new_witness_is_verified() {
    let lab = Lab::new(&[Role::Writer]);
    let root = aterm_tempfile::tempdir().unwrap();
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let path = root.path().join("table");
    let mut writer = Store::create(&session, &path).unwrap();
    let scope = writer.scope();
    writer.commit_catalog(&session, b"one").unwrap();
    let observer = Store::observe(&session, &path, scope).unwrap();
    let claim_path = writer.claim_dir.join(&writer.claim_name);
    let before = std::fs::read(&claim_path).unwrap();
    writer.commit_catalog(&session, b"two").unwrap();
    let current = std::fs::read(&claim_path).unwrap();
    assert!(observer.inventory(&session).err().unwrap().is_busy());
    let fresh = Store::observe(&session, &path, scope).unwrap();
    assert_eq!(fresh.inventory(&session).unwrap().count(), 2);

    std::fs::write(&claim_path, &before).unwrap();
    assert!(!fresh.inventory(&session).err().unwrap().is_busy());
    assert!(!writer.inventory(&session).err().unwrap().is_busy());
    let mut damaged = current.clone();
    damaged[WITNESS_AT + 8] ^= 1;
    std::fs::write(&claim_path, &damaged).unwrap();
    assert!(!fresh.inventory(&session).err().unwrap().is_busy());
    damaged = current.clone();
    damaged[WITNESS_AT] ^= 1;
    std::fs::write(&claim_path, &damaged).unwrap();
    // A changed count invalidates the old handle; reopening must verify it,
    // and this invented extension is a hard failure, never accepted data.
    assert!(fresh.inventory(&session).err().unwrap().is_busy());
    assert!(
        !Store::observe(&session, &path, scope)
            .err()
            .unwrap()
            .is_busy()
    );
    assert!(!writer.inventory(&session).err().unwrap().is_busy());
    damaged = current.clone();
    damaged[5] ^= 1;
    std::fs::write(&claim_path, &damaged).unwrap();
    assert!(!observer.inventory(&session).err().unwrap().is_busy());
    std::fs::write(&claim_path, &current).unwrap();
    assert_eq!(fresh.inventory(&session).unwrap().count(), 2);
}

#[test]
fn an_observer_cannot_append_or_recover_an_unwitnessed_tail() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let root = aterm_tempfile::tempdir().unwrap();
    let a = lab.bootstrap(0);
    let b = lab.bootstrap(1);
    let session = Session::open(&a).unwrap();
    let peer_session = Session::open(&b).unwrap();
    let path = root.path().join("table");
    let mut held = Store::create(&session, &path).unwrap();
    let scope = held.scope();
    held.commit_catalog(&session, b"witnessed").unwrap();
    let mut peer = Store::join(&peer_session, &root.path().join("peer"), scope).unwrap();
    let receipt = peer.commit_catalog(&peer_session, b"unwitnessed").unwrap();
    let wire = peer
        .wire(&peer_session, &receipt.id)
        .unwrap()
        .unwrap()
        .to_vec();

    let mut observer = Store::observe(&session, &path, scope).unwrap();
    assert!(observer.ingest(&session, &wire).is_err());
    assert!(observer.commit_catalog(&session, b"forbidden").is_err());
    assert!(observer.commit_liability(&session, b"forbidden").is_err());
    assert_eq!(observer.inventory(&session).unwrap().count(), 1);
    let claim_path = held.claim_dir.join(&held.claim_name);
    let before_claim = std::fs::read(&claim_path).unwrap();
    let mut body = receipt.received_ms.to_le_bytes().to_vec();
    body.extend_from_slice(&wire);
    held.log.as_mut().unwrap().append(SUBJECT, body).unwrap();
    let before_log = std::fs::read(path.join("table.astream")).unwrap();
    let before_checkpoint = std::fs::read(path.join("table.checkpoint")).unwrap();

    let observer = Store::observe(&session, &path, scope).unwrap();
    assert_eq!(observer.inventory(&session).unwrap().count(), 1);
    assert_eq!(observer.witness.count, 1);
    assert_eq!(std::fs::read(&claim_path).unwrap(), before_claim);
    assert_eq!(
        std::fs::read(path.join("table.astream")).unwrap(),
        before_log
    );
    assert_eq!(
        std::fs::read(path.join("table.checkpoint")).unwrap(),
        before_checkpoint
    );
    drop((observer, held));
    let repaired = Store::open(&session, &path, scope).unwrap();
    assert_eq!(repaired.inventory(&session).unwrap().count(), 2);
    assert_eq!(repaired.witness.count, 2);
    drop(repaired);
    let acknowledged = std::fs::read(path.join("table.astream")).unwrap();
    let mut torn = acknowledged.clone();
    torn.extend_from_slice(b"unacknowledged physical tail");
    std::fs::write(path.join("table.astream"), &torn).unwrap();
    let observer = Store::observe(&session, &path, scope).unwrap();
    assert_eq!(observer.inventory(&session).unwrap().count(), 2);
    assert_eq!(std::fs::read(path.join("table.astream")).unwrap(), torn);
    drop(observer);
    torn[acknowledged.len() - 1] ^= 1;
    std::fs::write(path.join("table.astream"), &torn).unwrap();
    assert!(Store::observe(&session, &path, scope).is_err());
}

/// What an observer open judged a table to hold under its session: what it quarantines, retains,
/// offers and shows, whether its liabilities are complete, and the witness it read; or its
/// refusal's text.
type Judged = std::result::Result<
    (
        Vec<super::super::Quarantined>,
        Vec<Digest>,
        Vec<Digest>,
        usize,
        bool,
        u64,
    ),
    String,
>;

fn judged(store: Result<Store>, session: &Session<'_>) -> Judged {
    let text = |error: Error| error.to_string();
    let store = store.map_err(text)?;
    Ok((
        store.quarantined(session).map_err(text)?,
        store.inventory(session).map_err(text)?.copied().collect(),
        store
            .replication_inventory(session)
            .map_err(text)?
            .copied()
            .collect(),
        store.catalog(session).map_err(text)?.len(),
        store.liability_evidence(session).is_ok(),
        store.witness.count,
    ))
}

/// Each observer open of a kept table judges what it keeps under its own session, as an open of
/// the table read afresh (nothing kept) does: answered from what is kept (no read), an open after
/// a control recovery discarded its record's source quarantines the record; an open under a
/// writing session, and one whose session's control journal moved during its replay, answer as
/// a fresh open does; and of a table with an unwitnessed append, an open under the claim before
/// it exposes the witnessed records, and one after the writer adopted it exposes it too, read and
/// then answered from what is kept.
/// NEGATIVE CONTROL: the restore's classification of a record kept by its identifier and reused
/// when the journal is answered from what is kept takes the discarded record as live after the
/// recovery, and opens it under a roster the recovery left out, which refuses the open a fresh
/// one quarantines.
#[test]
fn each_open_of_a_kept_table_judges_it_under_its_own_session() {
    use super::super::journals::{Journals, as_if_later, with_own};
    use std::sync::Arc;
    // An hour on: an observed journal stood settled at each ask.
    let later = std::time::Duration::from_secs(3600);
    let kept = Arc::new(Journals::new(32, 1 << 30, 1 << 30));
    let cold = Arc::new(Journals::new(32, 0, 0));
    let observe = |journals: &Arc<Journals>, session: &Session<'_>, dir: &Path, scope| {
        let store = as_if_later(later, || {
            with_own(Arc::clone(journals), || Store::observe(session, dir, scope))
        });
        judged(store, session)
    };
    // Answered from what is kept where `read` says, and as a fresh open answers.
    let alike = |what: &str, session: &Session<'_>, dir: &Path, scope, read: Option<usize>| {
        let start = kept.test_reads();
        let answer = observe(&kept, session, dir, scope);
        if let Some(read) = read {
            assert_eq!(kept.test_reads() - start, read, "{what}: reads");
        }
        assert_eq!(answer, observe(&cold, session, dir, scope), "{what}");
        answer
    };

    let f = Fixture::new(false, false, false);
    let a = f.lab.bootstrap(0);
    let session = Session::observe(&a).unwrap();
    let live = alike("warmed", &session, &f.dirs[0], f.scope, Some(1)).unwrap();
    assert!(live.0.is_empty(), "nothing quarantined before the recovery");
    alike("kept", &session, &f.dirs[0], f.scope, Some(0)).unwrap();
    drop(session);
    f.recover();
    let session = Session::observe(&a).unwrap();
    let recovered = alike("recovered", &session, &f.dirs[0], f.scope, Some(0)).unwrap();
    assert_eq!(
        recovered.0.iter().map(|q| q.receipt.id).collect::<Vec<_>>(),
        [f.receipt.id],
        "the discarded source's record is quarantined"
    );
    drop(session);
    let writing = Session::open(&a).unwrap();
    alike("a writing session", &writing, &f.dirs[0], f.scope, Some(0)).unwrap();
    let anchor = a.journal_dir().join("control.anchor");
    let moved = anchor.with_extension("moved");
    // The control journal's anchor moved away once the table's records are replayed, and back.
    let during = |journals: &Arc<Journals>| {
        let (from, to) = (anchor.clone(), moved.clone());
        let answer = super::super::with_replay_observer(
            move || std::fs::rename(&from, &to).unwrap(),
            || observe(journals, &writing, &f.dirs[0], f.scope),
        );
        std::fs::rename(&moved, &anchor).unwrap();
        answer
    };
    let start = kept.test_reads();
    let answer = during(&kept);
    assert_eq!(kept.test_reads(), start, "answered from what is kept");
    assert!(answer.is_err(), "a moved control journal refuses the open");
    assert_eq!(
        answer,
        during(&cold),
        "a control journal moved during the replay"
    );
    drop(writing);

    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let root = aterm_tempfile::tempdir().unwrap();
    let (a, b) = (lab.bootstrap(0), lab.bootstrap(1));
    let session = Session::open(&a).unwrap();
    let peer_session = Session::open(&b).unwrap();
    let path = root.path().join("table");
    let mut held = Store::create(&session, &path).unwrap();
    let scope = held.scope();
    held.commit_catalog(&session, b"witnessed").unwrap();
    let mut peer = Store::join(&peer_session, &root.path().join("peer"), scope).unwrap();
    let receipt = peer.commit_catalog(&peer_session, b"unwitnessed").unwrap();
    let wire = peer
        .wire(&peer_session, &receipt.id)
        .unwrap()
        .unwrap()
        .to_vec();
    alike("before the append", &session, &path, scope, Some(1)).unwrap();
    let mut body = receipt.received_ms.to_le_bytes().to_vec();
    body.extend_from_slice(&wire);
    held.log.as_mut().unwrap().append(SUBJECT, body).unwrap();
    let witnessed = alike("an unwitnessed append", &session, &path, scope, Some(1)).unwrap();
    assert_eq!(
        (witnessed.1.len(), witnessed.5),
        (1, 1),
        "the claim's record alone"
    );
    alike(
        "an unwitnessed append, kept",
        &session,
        &path,
        scope,
        Some(0),
    )
    .unwrap();
    drop(held);
    // The writer's open moves what the witness shows: the next ask reads, with the kept prefix.
    drop(Store::open(&session, &path, scope).unwrap());
    let adopted = alike("the append adopted", &session, &path, scope, None).unwrap();
    assert_eq!(
        (adopted.1.len(), adopted.5),
        (2, 2),
        "the adopted record too"
    );
    alike("the append adopted, kept", &session, &path, scope, Some(0)).unwrap();
}

struct Fixture {
    lab: Lab,
    scope: Scope,
    dirs: [PathBuf; 2],
    wire: Vec<u8>,
    receipt: Receipt,
    authored_ms: u64,
    right: Node,
}
impl Fixture {
    fn new(own: bool, tail: bool, empty: bool) -> Self {
        let lab = Lab::new(&[Role::Writer, Role::Writer]);
        let a = lab.bootstrap(0);
        let b = lab.bootstrap(1);
        let dirs = [
            lab._home.path().join("table-a"),
            lab._home.path().join("table-b"),
        ];
        let session = Session::open(&a).unwrap();
        let store = Store::create(&session, &dirs[0]).unwrap();
        let scope = store.scope();
        drop((store, session));
        Store::join(&Session::open(&b).unwrap(), &dirs[1], scope).unwrap();
        let admin = client::admin_open(&lab.paths[0]).unwrap();
        let journal = Journal::open_existing(a.journal_dir(), a.trust()).unwrap();
        let grants = || {
            vec![
                Grant::recorded("m1", a.request().unwrap().binding().clone(), Role::Writer),
                Grant::recorded("m2", b.request().unwrap().binding().clone(), Role::Writer),
            ]
        };
        let left = admin.prepare_commons_rotation(&journal, grants()).unwrap();
        let right = admin.prepare_commons_rotation(&journal, grants()).unwrap();
        drop(journal);
        ingest(&lab, &left);
        let writer = Session::open(if own { &a } else { &b }).unwrap();
        let mut store = Store::open(&writer, &dirs[usize::from(!own)], scope).unwrap();
        let receipt = store
            .commit_catalog(&writer, b"reserved liability")
            .unwrap();
        let wire = store.wire(&writer, &receipt.id).unwrap().unwrap().to_vec();
        let authored_ms = writer.authenticate(scope, &wire).unwrap().authored_ms();
        drop((store, writer));
        if !own && !empty {
            let session = Session::open(&a).unwrap();
            let mut store = Store::open(&session, &dirs[0], scope).unwrap();
            if tail {
                let mut body = receipt.received_ms.to_le_bytes().to_vec();
                body.extend_from_slice(&wire);
                store.log.as_mut().unwrap().append(SUBJECT, body).unwrap();
            } else {
                store.ingest(&session, &wire).unwrap();
            }
        }
        Self {
            lab,
            scope,
            dirs,
            wire,
            receipt,
            authored_ms,
            right,
        }
    }
    fn recover(&self) {
        let a = self.lab.bootstrap(0);
        let admin = client::admin_open(&self.lab.paths[0]).unwrap();
        let journal = Journal::open_existing(a.journal_dir(), a.trust()).unwrap();
        let left = match journal.state() {
            aterm_secrets::commons::journal::State::Ready { head, .. } => head,
            _ => panic!("ready"),
        };
        let recovery = admin
            .seal_commons_restricted(
                a.trust().network,
                Edit::Recover {
                    heads: &[left, self.right.id()],
                    selected: self.right.id(),
                },
                &[],
            )
            .unwrap();
        drop(journal);
        ingest(&self.lab, &self.right);
        ingest(&self.lab, &recovery);
        self.lab.rotate(&[Role::Writer, Role::Writer]);
    }
}
fn ingest(lab: &Lab, node: &Node) {
    for n in 0..2 {
        let b = lab.bootstrap(n);
        Journal::open_existing(b.journal_dir(), b.trust())
            .unwrap()
            .ingest(node.bytes())
            .unwrap();
    }
}

fn missing_selected_key(mut next: aterm_spec::interp::State) -> aterm_spec::interp::State {
    let lab = Lab::with_enrolled(&[Role::Writer, Role::Reader], 1);
    let a = lab.bootstrap(0);
    let session = Session::open(&a).unwrap();
    let dirs = [lab._home.path().join("a"), lab._home.path().join("b")];
    let mut store = Store::create(&session, &dirs[0]).unwrap();
    let scope = store.scope();
    let receipt = store.commit_catalog(&session, b"selected stop").unwrap();
    let wire = store.wire(&session, &receipt.id).unwrap().unwrap().to_vec();
    drop((store, session));
    lab.admit(&[Role::Writer, Role::Reader]);
    let b = lab.bootstrap(1);
    let journal = Journal::open_existing(a.journal_dir(), a.trust()).unwrap();
    let access = a.access_in(&journal).unwrap().unwrap();
    let handoff = a
        .history_handoff(
            &journal,
            &access,
            a.trust().genesis,
            b.request().unwrap().binding(),
        )
        .unwrap();
    drop(journal);
    let journal = Journal::open_existing(b.journal_dir(), b.trust()).unwrap();
    let access = b.access_in(&journal).unwrap().unwrap();
    b.import_history(&journal, &access, handoff.bytes())
        .unwrap();
    drop(journal);
    let session = Session::open(&b).unwrap();
    Store::join(&session, &dirs[1], scope)
        .unwrap()
        .ingest(&session, &wire)
        .unwrap();
    drop(session);
    super::super::journals::with_own(
        std::sync::Arc::new(super::super::journals::Journals::new(
            1,
            64 * 1024,
            64 * 1024,
        )),
        || {
            // Retain successful payload facts before taking the real history key away. Both a fresh
            // writing replay and payload materialization must still ask for that key. An observer
            // may retain the witnessed bytes without treating them as readable evidence.
            Store::observe(&Session::observe(&b).unwrap(), &dirs[1], scope).unwrap();
            for file in std::fs::read_dir(lab.paths[1].dir.join("commons/history")).unwrap() {
                std::fs::remove_file(file.unwrap().path()).unwrap();
            }
            next.insert(
                "writable",
                i64::from(Store::open(&Session::open(&b).unwrap(), &dirs[1], scope).is_ok()),
            );
            let session = Session::observe(&b).unwrap();
            if let Ok(store) = Store::observe(&session, &dirs[1], scope) {
                next.insert("opened", 1);
                next.insert(
                    "retained",
                    store.inventory(&session).unwrap().count() as i64,
                );
                next.insert("visible", store.catalog(&session).unwrap().len() as i64);
                next.insert(
                    "offered",
                    store.replication_inventory(&session).unwrap().count() as i64,
                );
                next.insert(
                    "complete",
                    i64::from(store.liability_evidence(&session).is_ok()),
                );
            }
            next
        },
    )
}

fn quarantine_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let names = [
        "Intact",
        "ForeignDiscarded",
        "OwnDiscarded",
        "DiscardedTail",
        "ChangedWitness",
        "MissingSelectedKey",
        "RemoteDiscarded",
        "SelectedDependent",
        "ChangedObservation",
        "MissingParent",
        "RepairedParent",
    ];
    let scenario = names.iter().position(|n| *n == action).unwrap() + 1;
    let mut next = state.clone();
    next.insert("scenario", scenario as i64);
    if action == "MissingSelectedKey" {
        return missing_selected_key(next);
    }
    let f = Fixture::new(
        action == "OwnDiscarded",
        action == "DiscardedTail",
        matches!(
            action,
            "RemoteDiscarded" | "MissingParent" | "RepairedParent"
        ),
    );
    if !matches!(action, "Intact" | "MissingParent" | "RepairedParent") {
        f.recover();
    }
    let a = f.lab.bootstrap(0);
    let session = Session::open(&a).unwrap();
    if action == "ChangedWitness" {
        let claim_dir = super::super::claim_dir(&session).unwrap();
        let path = claim_dir.join(format!(
            "{}.claim",
            aterm_codec::hex::encode_lower(&f.scope.digest())
        ));
        let mut claim = std::fs::read(&path).unwrap();
        *claim.last_mut().unwrap() ^= 1;
        std::fs::write(path, claim).unwrap();
    }
    let Ok(mut store) = Store::open(&session, &f.dirs[0], f.scope) else {
        return next;
    };
    if action == "ChangedObservation" {
        drop(session);
        f.lab.rotate(&[Role::Writer, Role::Writer]);
        let session = Session::open(&a).unwrap();
        next.insert("opened", i64::from(store.inventory(&session).is_ok()));
        return next;
    }
    next.insert("opened", 1);
    if action == "RemoteDiscarded" {
        next.insert(
            "received",
            i64::from(store.ingest(&session, &f.wire).is_ok()),
        );
    }
    if action == "SelectedDependent" {
        let wire = session
            .seal(
                f.scope,
                Causal {
                    sequence: 1,
                    previous: None,
                    parents: &[f.receipt.id],
                },
                b"depends on discarded source",
            )
            .unwrap();
        store.ingest(&session, &wire).unwrap();
        drop(store);
        store = Store::open(&session, &f.dirs[0], f.scope).unwrap();
    }
    if matches!(action, "MissingParent" | "RepairedParent") {
        let b = f.lab.bootstrap(1);
        let peer = Session::open(&b).unwrap();
        let mut source = Store::open(&peer, &f.dirs[1], f.scope).unwrap();
        let child = source.commit_catalog(&peer, b"later receipt").unwrap();
        store
            .ingest(&session, source.wire(&peer, &child.id).unwrap().unwrap())
            .unwrap();
        if action == "RepairedParent" {
            store.ingest(&session, &f.wire).unwrap();
        }
        drop(store);
        store = Store::open(&session, &f.dirs[0], f.scope).unwrap();
    }
    let q = store.quarantined(&session).unwrap();
    next.insert("quarantined", i64::from(!q.is_empty()));
    next.insert(
        "retained",
        store.inventory(&session).unwrap().count() as i64,
    );
    next.insert(
        "offered",
        store.replication_inventory(&session).unwrap().count() as i64,
    );
    next.insert(
        "complete",
        i64::from(store.liability_evidence(&session).is_ok()),
    );
    if next["complete"] == 0 {
        assert!(
            store
                .commit_liability(&session, b"must preserve liabilities")
                .is_err()
        );
    }
    next.insert("visible", store.catalog(&session).unwrap().len() as i64);
    if action != "RemoteDiscarded" {
        let before = store.catalog(&session).unwrap();
        if !matches!(action, "Intact" | "RepairedParent") {
            assert!(before.is_empty());
        }
        next.insert(
            "writable",
            i64::from(store.commit_catalog(&session, b"fresh catalog").is_ok()),
        );
    }
    next
}

#[test]
fn witnessed_quarantine_conforms_and_unwitnessed_promotion_is_detected() {
    let model = aterm_models_db::table_quarantine_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    let initial = model.init_state();
    for action in &model.actions {
        let actual = quarantine_transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&actual),
            "{}: {actual:?}",
            action.name
        );
    }
    let f = Fixture::new(false, true, false);
    f.recover();
    let a = f.lab.bootstrap(0);
    let session = Session::open(&a).unwrap();
    let catalog = f.scope == crate::catalog::scope(f.scope.trust());
    assert!(
        session
            .with_read_batch(f.scope, |batch| {
                restore(
                    batch,
                    f.receipt,
                    Identified::new(f.wire.clone()),
                    0,
                    catalog,
                    false,
                )
            })
            .is_err()
    );
    // Negative control: lying about the verified-prefix boundary promotes the
    // exact same unwitnessed tail. The model must reject that unsafe composition.
    let promoted = session
        .with_read_batch(f.scope, |batch| {
            restore(batch, f.receipt, Identified::new(f.wire), 1, catalog, false)
        })
        .unwrap();
    assert!(promoted.payload.quarantine().is_some());
    let mut bad = initial.clone();
    bad.insert("scenario", 4);
    bad.insert("opened", 1);
    assert!(!model.check_invariant("UnsafeSourcesClosed", &bad));
    drop(session);

    let f = Fixture::new(false, false, false);
    f.recover();
    let a = f.lab.bootstrap(0);
    let session = Session::open(&a).unwrap();
    let store = Store::open(&session, &f.dirs[0], f.scope).unwrap();
    let mut visible = initial;
    visible.insert("scenario", 2);
    visible.insert(
        "visible",
        store.index().unwrap().project().ordered.len() as i64,
    );
    assert!(!model.check_invariant("NoQuarantineProjection", &visible));
}

fn rotation_with_facts(lab: &Lab, extras: &[aterm_secrets::commons::constraints::Fact]) -> Node {
    let a = lab.bootstrap(0);
    let b = lab.bootstrap(1);
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let journal = Journal::open_existing(a.journal_dir(), a.trust()).unwrap();
    let draft = admin
        .prepare_commons_rotation(
            &journal,
            vec![
                Grant::recorded("m1", a.request().unwrap().binding().clone(), Role::Writer),
                Grant::recorded("m2", b.request().unwrap().binding().clone(), Role::Writer),
            ],
        )
        .unwrap();
    if extras.is_empty() {
        return draft;
    }
    let identity = client::machine_identity(&lab.paths[0]).unwrap();
    let payload = draft.open(&identity).unwrap();
    let mut facts = draft.facts().to_vec();
    facts.extend_from_slice(extras);
    admin
        .seal_commons_restricted(
            a.trust().network,
            Edit::Update {
                parent: draft.parents()[0],
                hosts: &["m1".into(), "m2".into()],
                payload: payload.as_slice(),
            },
            &facts,
        )
        .unwrap()
}

#[test]
fn quarantined_cutoff_frontiers_do_not_change_projection_between_replicas() {
    use aterm_secrets::commons::constraints::{Fact, Frontier};
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let a = lab.bootstrap(0);
    let b = lab.bootstrap(1);
    let dirs = [lab._home.path().join("a"), lab._home.path().join("b")];
    let first = Session::open(&a).unwrap();
    let mut store = Store::create(&first, &dirs[0]).unwrap();
    let scope = store.scope();
    let actor = first.actor();
    let r1 = store
        .commit_catalog(&first, b"selected safety stop")
        .unwrap();
    let wire = store.wire(&first, &r1.id).unwrap().unwrap().to_vec();
    let second = Session::open(&b).unwrap();
    Store::join(&second, &dirs[1], scope)
        .unwrap()
        .ingest(&second, &wire)
        .unwrap();
    drop((store, first, second));

    let cutoff = |sequence, digest| Fact::Cutoff {
        table: scope.digest(),
        actor,
        through: Some(Frontier { sequence, digest }),
    };
    let left = rotation_with_facts(&lab, &[]);
    let right = rotation_with_facts(&lab, &[cutoff(1, r1.id)]);
    ingest(&lab, &left);
    let session = Session::open(&a).unwrap();
    let mut store = Store::open(&session, &dirs[0], scope).unwrap();
    let q2 = store.commit_catalog(&session, b"discarded debit").unwrap();
    drop((store, session));
    let left_cutoff = rotation_with_facts(&lab, &[cutoff(2, q2.id)]);
    ingest(&lab, &left_cutoff);
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let recovery = admin
        .seal_commons_restricted(
            a.trust().network,
            Edit::Recover {
                heads: &[left_cutoff.id(), right.id()],
                selected: right.id(),
            },
            &[],
        )
        .unwrap();
    ingest(&lab, &right);
    ingest(&lab, &recovery);
    lab.rotate(&[Role::Writer, Role::Writer]);

    let first = Session::open(&a).unwrap();
    let second = Session::open(&b).unwrap();
    let store = Store::open(&first, &dirs[0], scope).unwrap();
    let peer = Store::open(&second, &dirs[1], scope).unwrap();
    assert_eq!(store.inventory(&first).unwrap().count(), 2);
    assert_eq!(peer.inventory(&second).unwrap().count(), 1);
    assert_eq!(store.quarantined(&first).unwrap().len(), 1);
    assert!(store.catalog(&first).unwrap().is_empty());
    assert!(peer.catalog(&second).unwrap().is_empty());
    assert!(store.liability_evidence(&first).is_err());
    assert!(
        peer.liability_evidence(&second).is_err(),
        "a known missing cutoff frontier is incomplete evidence"
    );
    // Negative control: the old unfiltered local graph treated the opaque
    // frontier as positive proof, while the peer could never receive it.
    assert!(
        first
            .journal
            .constraints()
            .unwrap()
            .catalog(scope.digest(), store.index().unwrap())
            .permits(&r1.id)
    );
    assert!(
        !second
            .journal
            .constraints()
            .unwrap()
            .catalog(scope.digest(), peer.index().unwrap())
            .permits(&r1.id)
    );
}

#[test]
fn discarded_source_never_launders_a_selected_fork() {
    let f = Fixture::new(true, false, false);
    f.recover();
    let a = f.lab.bootstrap(0);
    let session = Session::open(&a).unwrap();
    let mut store = Store::open(&session, &f.dirs[0], f.scope).unwrap();
    let fork = session
        .seal(
            f.scope,
            Causal {
                sequence: 1,
                previous: None,
                parents: &[],
            },
            b"selected conflicting receipt",
        )
        .unwrap();
    store.ingest(&session, &fork).unwrap();
    assert_eq!(store.inventory(&session).unwrap().count(), 2);
    assert!(store.catalog(&session).unwrap().is_empty());
    assert_eq!(store.replication_inventory(&session).unwrap().count(), 0);
    assert!(store.liability_evidence(&session).is_err());
    assert!(
        store
            .commit_liability(&session, b"must retain the fork")
            .is_err()
    );
    assert!(
        store
            .commit_catalog(&session, b"cannot resolve a fork by filtering")
            .is_err()
    );
}

#[test]
fn recovered_replicas_exchange_live_records_without_offering_quarantined_bytes() {
    use crate::control::{Endpoint, Repair, Side};
    use aterm_net::lan::{Connection, Scope as LinkScope};
    use std::{net::TcpListener, thread};

    let f = Fixture::new(false, false, false);
    f.recover();
    let a = f.lab.bootstrap(0);
    let session = Session::open(&a).unwrap();
    let mut store = Store::open(&session, &f.dirs[0], f.scope).unwrap();
    let fresh = store
        .commit_catalog(&session, b"current account metadata")
        .unwrap();
    assert_eq!(
        store.quarantined(&session).unwrap()[0].header.id,
        f.receipt.id
    );
    assert_eq!(
        store.quarantined(&session).unwrap()[0].authored_ms,
        f.authored_ms
    );
    assert!(store.liability_evidence(&session).is_err());
    drop((store, session));
    let endpoints: Vec<_> = (0..2)
        .map(|n| {
            Endpoint::from_bootstrap(&f.lab.bootstrap(n))
                .unwrap()
                .unwrap()
                .with_tables(f.lab.paths[n].clone(), vec![(f.scope, f.dirs[n].clone())])
                .unwrap()
        })
        .collect();
    let (mut ra, mut rb) = (Repair::default(), Repair::default());
    for round in 0..2 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::scope(|threads| {
            let server = threads.spawn(|| {
                endpoints[1].exchange(
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
            endpoints[0]
                .exchange(
                    crate::replica::tests::fixture_connect(address, std::time::Instant::now())
                        .unwrap(),
                    Side::Initiator,
                    &mut ra,
                )
                .unwrap();
            server.join().unwrap().unwrap();
        });
        assert_eq!(ra.application().unwrap().sent, usize::from(round == 0));
        assert_eq!(rb.application().unwrap().received, usize::from(round == 0));
        assert_eq!(ra.application().unwrap().deferred, 0);
        assert_eq!(rb.application().unwrap().deferred, 0);
    }
    let b = f.lab.bootstrap(1);
    let session = Session::open(&b).unwrap();
    let mut store = Store::open(&session, &f.dirs[1], f.scope).unwrap();
    assert_eq!(store.inventory(&session).unwrap().count(), 2);
    assert_eq!(store.catalog(&session).unwrap()[0].header().id, fresh.id);
    assert_eq!(
        store.quarantined(&session).unwrap()[0].header.id,
        f.receipt.id
    );
    assert!(
        store
            .commit_catalog(&session, b"cannot reset old writer")
            .is_err()
    );
    assert_eq!(store.quarantined(&session).unwrap()[0].receipt, f.receipt);
    assert!(store.liability_evidence(&session).is_err());
}
