// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for the real roster body/signature redo transaction.
//!
//! The derived [`RosterPairRedo`](aterm_spec::derive::roster_pair_redo_model)
//! model is proved exhaustively in `aterm-spec`.  These tests bind its decisions
//! to the shipping `atpkg-keys` filesystem operations: byte-exact snapshot CAS,
//! end-to-end pair publication, recovery from every durable crash cut, fail-closed
//! handling of an unrelated half, and the read-only lock's refusal to replay.
//!
//! Every crash premise is produced by the production commit path
//! (`commit_roster_pair`, then dropping the `CommittedRoster` instead of completing
//! it) — exactly what a killed publisher leaves on disk — so nothing in this file
//! spells the redo directory's layout, and a format change cannot leave the bind
//! quietly exercising a replica.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use aterm_spec::derive::{Model, roster_pair_redo_model};
use aterm_spec::interp::{self, State};
use aterm_spec::verify::validate_transition_tiered;
use atpkg_keys::provision::{
    RosterLock, RosterSnapshot, commit_roster_pair, lock_roster, lock_roster_read_only,
    publish_roster_locked,
};

const PREDECESSOR_BODY: &[u8] = b"predecessor body";
const PREDECESSOR_SIGNATURE: &[u8] = b"predecessor signature";
const TARGET_BODY: &[u8] = b"target body";
const TARGET_SIGNATURE: &[u8] = b"target signature";
const FOREIGN_BODY: &[u8] = b"foreign body";
const FOREIGN_SIGNATURE: &[u8] = b"foreign signature";

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    roster: PathBuf,
}

impl Fixture {
    fn new(label: &str, predecessor_present: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "atpkg-roster-redo-model-{label}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create roster model fixture");
        let roster = root.join("aterm-machines.toml");
        let fixture = Self { root, roster };
        if predecessor_present {
            std::fs::write(&fixture.roster, PREDECESSOR_BODY).expect("write predecessor body");
            std::fs::write(fixture.signature(), PREDECESSOR_SIGNATURE)
                .expect("write predecessor signature");
        }
        fixture
    }

    fn roster_str(&self) -> &str {
        self.roster.to_str().expect("fixture path is UTF-8")
    }

    fn signature(&self) -> PathBuf {
        PathBuf::from(format!("{}.sig", self.roster.display()))
    }

    fn transaction(&self) -> PathBuf {
        PathBuf::from(format!("{}.atpkg-keys.txn", self.roster.display()))
    }

    fn predecessor(&self, present: bool) -> Option<RosterSnapshot> {
        present.then(|| RosterSnapshot {
            raw: PREDECESSOR_BODY.to_vec(),
            sig: PREDECESSOR_SIGNATURE.to_vec(),
        })
    }

    /// Run the production snapshot CAS + redo commit under a real writer lock and
    /// then DIE before completing it: the `CommittedRoster` is dropped, not
    /// completed, which is byte for byte what a killed publisher leaves behind.
    /// The lock is returned so a caller can promote canonical halves the way the
    /// dead process would have, before releasing it.
    fn commit_and_die(&self, predecessor: Option<&RosterSnapshot>) -> RosterLock {
        let lock = lock_roster(self.roster_str()).expect("acquire real roster writer lock");
        let committed = commit_roster_pair(
            &lock,
            self.roster_str(),
            predecessor,
            TARGET_BODY,
            TARGET_SIGNATURE,
        )
        .expect("production commit path records the exact redo pair");
        assert_eq!(
            Path::new(committed.transaction_path()),
            self.transaction(),
            "the projection must watch the transaction name the shipping writer uses"
        );
        drop(committed);
        lock
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[derive(Clone, Copy, Default)]
struct Events {
    writer_lock: bool,
    snapshot_checked: bool,
    result: i64,
    writer_writes: i64,
    readonly_checked: bool,
    readonly_refused: bool,
    readonly_writes: i64,
    foreign_seen: bool,
    foreign_overwritten: bool,
    crashes: i64,
}

fn classify(path: &Path, predecessor: Option<&[u8]>, target: &[u8]) -> i64 {
    match std::fs::read(path) {
        Ok(bytes) if bytes == target => 1,
        Ok(bytes) if predecessor.is_some_and(|expected| bytes == expected) => 0,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && predecessor.is_none() => 0,
        _ => 2,
    }
}

/// Project only observable shipping state: exact file identities, fixed redo-name
/// presence, held guard, and outcomes/counters from calls the test actually made.
fn project(
    model: &Model,
    fixture: &Fixture,
    predecessor: Option<&RosterSnapshot>,
    events: Events,
) -> State {
    let mut state = model.init_state();
    state.insert(
        "body",
        classify(
            &fixture.roster,
            predecessor.map(|snapshot| snapshot.raw.as_slice()),
            TARGET_BODY,
        ),
    );
    state.insert(
        "signature",
        classify(
            &fixture.signature(),
            predecessor.map(|snapshot| snapshot.sig.as_slice()),
            TARGET_SIGNATURE,
        ),
    );
    state.insert("redo", i64::from(fixture.transaction().exists()));
    state.insert("writer_lock", i64::from(events.writer_lock));
    state.insert("snapshot_checked", i64::from(events.snapshot_checked));
    state.insert("result", events.result);
    state.insert("writer_writes", events.writer_writes);
    state.insert("readonly_checked", i64::from(events.readonly_checked));
    state.insert("readonly_refused", i64::from(events.readonly_refused));
    state.insert("readonly_writes", events.readonly_writes);
    state.insert("foreign_seen", i64::from(events.foreign_seen));
    state.insert("foreign_overwritten", i64::from(events.foreign_overwritten));
    state.insert("crashes", events.crashes);
    state
}

fn assert_transition(model: &Model, before: &State, after: &State, action: &str, label: &str) {
    assert!(
        model.action_enabled(action, before),
        "{label}: model disabled {action} from {before:?}"
    );
    assert!(
        model.successors(action, before).contains(after),
        "{label}: model does not admit {before:?} -> {after:?} via {action}"
    );
    let (admitted, evidence) =
        validate_transition_tiered(model, &[], before, after, Some(action), label);
    assert!(admitted, "{label}: {evidence}");
}

#[test]
fn snapshot_cas_and_end_to_end_pair_publish_refine_the_model() {
    for predecessor_present in [false, true] {
        let fixture = Fixture::new(
            if predecessor_present {
                "publish-existing"
            } else {
                "publish-fresh"
            },
            predecessor_present,
        );
        let predecessor = fixture.predecessor(predecessor_present);
        let model = roster_pair_redo_model();
        let mut events = Events::default();
        let mut state = project(&model, &fixture, predecessor.as_ref(), events);

        let guard = lock_roster(fixture.roster_str()).expect("acquire real roster writer lock");
        events.writer_lock = true;
        let after_lock = project(&model, &fixture, predecessor.as_ref(), events);
        assert_transition(
            &model,
            &state,
            &after_lock,
            "AcquireWriter",
            "roster redo: acquire writer lock",
        );
        state = after_lock;

        guard
            .assert_snapshot(predecessor.as_ref())
            .expect("exact guarded snapshot is current");
        events.snapshot_checked = true;
        let after_snapshot = project(&model, &fixture, predecessor.as_ref(), events);
        assert_transition(
            &model,
            &state,
            &after_snapshot,
            "AcceptSnapshot",
            "roster redo: accept exact snapshot",
        );
        state = after_snapshot;

        publish_roster_locked(
            &guard,
            fixture.roster_str(),
            predecessor.as_ref(),
            TARGET_BODY,
            TARGET_SIGNATURE,
        )
        .expect("real pair publisher commits and retires redo");
        events.result = 1;
        events.writer_writes = 2;
        let after_publish = project(&model, &fixture, predecessor.as_ref(), events);
        assert_transition(
            &model,
            &state,
            &after_publish,
            "PublishExact",
            "roster redo: exact end-to-end publish",
        );
        assert_eq!(std::fs::read(&fixture.roster).unwrap(), TARGET_BODY);
        assert_eq!(
            std::fs::read(fixture.signature()).unwrap(),
            TARGET_SIGNATURE
        );
        assert!(!fixture.transaction().exists());

        drop(guard);
        events.writer_lock = false;
        let released = project(&model, &fixture, predecessor.as_ref(), events);
        assert_transition(
            &model,
            &after_publish,
            &released,
            "ReleaseSuccess",
            "roster redo: release writer after exact pair",
        );
    }
}

#[test]
fn every_durable_crash_cut_recovers_forward_through_real_lock_acquisition() {
    for (label, crash_action, promote_body, promote_signature, prior_writes) in [
        ("redo-only", "CrashAfterRedo", false, false, 0),
        ("body-only", "CrashAfterBody", true, false, 1),
        ("pair-before-retire", "CrashAfterPair", true, true, 2),
    ] {
        let fixture = Fixture::new(label, true);
        let predecessor = fixture.predecessor(true).unwrap();
        let model = roster_pair_redo_model();
        let mut events = Events::default();
        let initial = project(&model, &fixture, Some(&predecessor), events);

        // The snapshot CAS and the redo commit are ONE production call
        // (`commit_roster_pair`); the model names the accepted snapshot as its own
        // step and exposes the commit only through the durable crash cuts below.
        let lock = fixture.commit_and_die(Some(&predecessor));
        events.writer_lock = true;
        events.snapshot_checked = true;
        let mut accepted = initial.clone();
        assert!(model.fire("AcquireWriter", &mut accepted));
        assert!(model.fire("AcceptSnapshot", &mut accepted));

        // THE CRASH: the dead publisher had promoted zero, one, or both canonical
        // halves from the committed pair before it stopped.
        if promote_body {
            std::fs::write(&fixture.roster, TARGET_BODY).unwrap();
        }
        if promote_signature {
            std::fs::write(fixture.signature(), TARGET_SIGNATURE).unwrap();
        }
        drop(lock);
        events.writer_lock = false;
        events.writer_writes = prior_writes;
        events.crashes = 1;
        let interrupted = project(&model, &fixture, Some(&predecessor), events);
        assert_transition(
            &model,
            &accepted,
            &interrupted,
            crash_action,
            "roster redo: durable crash cut left by the production commit path",
        );
        assert!(fixture.transaction().exists());

        let guard = lock_roster(fixture.roster_str())
            .expect("writer lock acquisition completes committed redo");
        events.writer_lock = true;
        events.result = 1;
        events.writer_writes = 2;
        let recovered = project(&model, &fixture, Some(&predecessor), events);
        assert_transition(
            &model,
            &interrupted,
            &recovered,
            "RecoverKnown",
            "roster redo: real recovery converges known halves",
        );
        assert_eq!(std::fs::read(&fixture.roster).unwrap(), TARGET_BODY);
        assert_eq!(
            std::fs::read(fixture.signature()).unwrap(),
            TARGET_SIGNATURE
        );
        assert!(!fixture.transaction().exists());

        drop(guard);
        events.writer_lock = false;
        let released = project(&model, &fixture, Some(&predecessor), events);
        assert_transition(
            &model,
            &recovered,
            &released,
            "ReleaseSuccess",
            "roster redo: release recovered writer",
        );
    }
}

#[test]
fn read_only_and_writer_recovery_preserve_an_unrelated_half() {
    let fixture = Fixture::new("foreign-refusal", true);
    let predecessor = fixture.predecessor(true).unwrap();

    // The crash: committed through the production path, body promoted, then dead.
    // Taking the writer lock here also establishes the persistent rendezvous inode
    // that the read-only API deliberately refuses to create.
    let lock = fixture.commit_and_die(Some(&predecessor));
    std::fs::write(&fixture.roster, TARGET_BODY).unwrap();
    drop(lock);
    // While this machine was down, an unrelated signature landed beside the body.
    std::fs::write(fixture.signature(), FOREIGN_SIGNATURE).unwrap();

    let model = roster_pair_redo_model();
    let mut locked = model.init_state();
    assert!(model.fire("AcquireWriter", &mut locked));
    assert!(model.fire("AcceptSnapshot", &mut locked));
    let body_cut = model.successors("CrashAfterBody", &locked)[0].clone();
    let foreign = model.successors("ReplaceSignatureWhileDown", &body_cut)[0].clone();

    let mut events = Events {
        snapshot_checked: true,
        writer_writes: 1,
        foreign_seen: true,
        crashes: 1,
        ..Events::default()
    };
    assert_eq!(
        project(&model, &fixture, Some(&predecessor), events),
        foreign,
        "filesystem crash fixture must project to the modeled foreign-half state"
    );

    let readonly_error = lock_roster_read_only(fixture.roster_str())
        .err()
        .expect("check-only lock refuses a redo marker");
    assert!(readonly_error.contains("never replays"), "{readonly_error}");
    events.readonly_checked = true;
    events.readonly_refused = true;
    let after_readonly = project(&model, &fixture, Some(&predecessor), events);
    assert_transition(
        &model,
        &foreign,
        &after_readonly,
        "ReadOnlyRejectRedo",
        "roster redo: check-only refusal is non-mutating",
    );

    let writer_error = lock_roster(fixture.roster_str())
        .err()
        .expect("writer recovery refuses an unrelated half");
    assert!(
        writer_error.contains("newer or unrelated"),
        "{writer_error}"
    );
    events.result = 2;
    let refused = project(&model, &fixture, Some(&predecessor), events);
    assert_transition(
        &model,
        &after_readonly,
        &refused,
        "RejectForeignRecovery",
        "roster redo: foreign half is preserved",
    );
    assert_eq!(std::fs::read(&fixture.roster).unwrap(), TARGET_BODY);
    assert_eq!(
        std::fs::read(fixture.signature()).unwrap(),
        FOREIGN_SIGNATURE
    );
    assert!(fixture.transaction().exists());

    // NEGATIVE CONTROL: the historical unsafe result would replace the unrelated
    // signature and retire the marker. Healthy transition admission rejects it,
    // while Buggy=1 admits it and violates ForeignBytesAreNeverOverwritten.
    let buggy = interp::with_buggy(&model, 1);
    let unsafe_after = buggy.successors("BuggyOverwriteForeign", &after_readonly)[0].clone();
    let (healthy_admitted, evidence) = validate_transition_tiered(
        &model,
        &[],
        &after_readonly,
        &unsafe_after,
        Some("BuggyOverwriteForeign"),
        "roster redo: reject overwrite-foreign mutant",
    );
    assert!(
        !healthy_admitted,
        "healthy model admitted overwrite: {evidence}"
    );
    let (buggy_admitted, evidence) = validate_transition_tiered(
        &model,
        &[("Buggy", 1)],
        &after_readonly,
        &unsafe_after,
        Some("BuggyOverwriteForeign"),
        "roster redo: negative control is live",
    );
    assert!(
        buggy_admitted,
        "Buggy=1 did not admit overwrite: {evidence}"
    );
    assert!(!buggy.check_invariant("ForeignBytesAreNeverOverwritten", &unsafe_after));
}

#[test]
fn stale_snapshot_and_clean_read_only_decisions_refine_the_model() {
    for (label, changed_path, foreign_bytes, advance_action) in [
        ("stale-body", false, FOREIGN_BODY, "AdvanceBodyBeforeCas"),
        (
            "stale-signature",
            true,
            FOREIGN_SIGNATURE,
            "AdvanceSignatureBeforeCas",
        ),
    ] {
        let fixture = Fixture::new(label, true);
        let predecessor = fixture.predecessor(true).unwrap();
        let model = roster_pair_redo_model();
        let mut events = Events::default();
        let initial = project(&model, &fixture, Some(&predecessor), events);

        let changed = if changed_path {
            fixture.signature()
        } else {
            fixture.roster.clone()
        };
        std::fs::write(changed, foreign_bytes).unwrap();
        events.foreign_seen = true;
        let advanced = project(&model, &fixture, Some(&predecessor), events);
        assert_transition(
            &model,
            &initial,
            &advanced,
            advance_action,
            "roster redo: concurrent snapshot advance",
        );

        let guard = lock_roster(fixture.roster_str()).expect("acquire lock after external advance");
        events.writer_lock = true;
        let locked = project(&model, &fixture, Some(&predecessor), events);
        assert_transition(
            &model,
            &advanced,
            &locked,
            "AcquireWriter",
            "roster redo: acquire before snapshot CAS",
        );
        // The same CAS decision gates the production commit path: a stale premise
        // records nothing durable, so there is no redo marker to recover from.
        let error = guard.assert_snapshot(Some(&predecessor)).unwrap_err();
        assert!(error.contains("CHANGED ON DISK"), "{error}");
        let commit_error = commit_roster_pair(
            &guard,
            fixture.roster_str(),
            Some(&predecessor),
            TARGET_BODY,
            TARGET_SIGNATURE,
        )
        .err()
        .expect("a stale premise cannot commit a redo pair");
        assert!(commit_error.contains("CHANGED ON DISK"), "{commit_error}");
        events.result = 2;
        let refused = project(&model, &fixture, Some(&predecessor), events);
        assert_transition(
            &model,
            &locked,
            &refused,
            "RejectStaleSnapshot",
            "roster redo: stale snapshot refusal",
        );
        assert_eq!(events.writer_writes, 0);
        assert!(!fixture.transaction().exists());

        drop(guard);
        events.writer_lock = false;
        let released = project(&model, &fixture, Some(&predecessor), events);
        assert_transition(
            &model,
            &refused,
            &released,
            "ReleaseRefusal",
            "roster redo: release after stale refusal",
        );
    }

    let fixture = Fixture::new("readonly-clean", true);
    let predecessor = fixture.predecessor(true).unwrap();
    drop(lock_roster(fixture.roster_str()).expect("establish writer rendezvous"));
    let model = roster_pair_redo_model();
    let events = Events::default();
    let before = project(&model, &fixture, Some(&predecessor), events);
    drop(lock_roster_read_only(fixture.roster_str()).expect("clean check-only lock succeeds"));
    let after = project(
        &model,
        &fixture,
        Some(&predecessor),
        Events {
            readonly_checked: true,
            ..events
        },
    );
    assert_transition(
        &model,
        &before,
        &after,
        "ReadOnlyObserveClean",
        "roster redo: clean check-only observation",
    );
    assert_eq!(std::fs::read(&fixture.roster).unwrap(), PREDECESSOR_BODY);
    assert_eq!(
        std::fs::read(fixture.signature()).unwrap(),
        PREDECESSOR_SIGNATURE
    );
}
