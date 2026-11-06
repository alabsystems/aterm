// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::table::tests::Lab;

#[test]
fn local_replica_retention_is_bounded_and_preserves_negative_evidence_before_data() {
    let lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let original = lab._home.path().join("original-with-restrictions");
    let mut source = Store::create(&session, &original).unwrap();
    let scope = source.scope();
    for payload in [b"one".as_slice(), b"two", b"three"] {
        source.commit_catalog(&session, payload).unwrap();
    }
    let mut restricted = Vec::new();
    for actor in 1..3 {
        let bootstrap = lab.bootstrap(actor);
        let signer = Session::open(&bootstrap).unwrap();
        for payload in [b"left".as_slice(), b"right"] {
            let wire = signer
                .seal(
                    scope,
                    Causal {
                        sequence: 1,
                        previous: None,
                        parents: &[],
                    },
                    payload,
                )
                .unwrap();
            source.ingest(&session, &wire).unwrap();
            restricted.push(wire);
        }
    }
    let original_claim = std::fs::read(source.claim_dir.join(&source.claim_name)).unwrap();
    let original_log = std::fs::read(original.join("table.astream")).unwrap();
    let recovery_path = bootstrap
        .paths()
        .dir
        .join("commons/recovery/tables")
        .join(aterm_codec::hex::encode_lower(&scope.digest()));
    let mut recovery = Store::join_recovery(&session, &recovery_path, scope).unwrap();
    let recovery_claim = std::fs::read(recovery.claim_dir.join(&recovery.claim_name)).unwrap();
    let recovery_log = std::fs::read(recovery_path.join("table.astream")).unwrap();
    let mut combined = Store::observe_recovery(&session, &recovery_path, scope)
        .unwrap()
        .with_observed_primary(
            &session,
            Store::observe(&session, &original, scope).unwrap(),
        )
        .unwrap();
    assert!(combined.is_recovery());
    assert_eq!(combined.catalog(&session).unwrap().len(), 3);
    assert_eq!(combined.metadata_evidence(&session).unwrap().len(), 7);
    assert_eq!(combined.proof_inventory(&session).unwrap().len(), 2);
    assert!(combined.liability_evidence(&session).is_err());
    assert!(combined.custody_id(&session).is_err());
    assert!(
        combined
            .commit_catalog(&session, b"a-combined-read-is-not-a-writer")
            .is_err()
    );
    assert_eq!(
        std::fs::read(recovery.claim_dir.join(&recovery.claim_name)).unwrap(),
        recovery_claim
    );
    assert_eq!(
        std::fs::read(recovery_path.join("table.astream")).unwrap(),
        recovery_log
    );
    assert!(!recovery.retain_replica_batch(&session, &source, 1).unwrap());
    assert!(
        combined
            .inventory(&session)
            .err()
            .is_some_and(|error| error.is_busy())
    );
    assert_eq!(recovery.proof_inventory(&session).unwrap().len(), 1);
    assert_eq!(recovery.inventory(&session).unwrap().count(), 0);
    for expected in 1..=3 {
        assert!(recovery.retain_replica_batch(&session, &source, 1).unwrap());
        assert_eq!(recovery.proof_inventory(&session).unwrap().len(), 2);
        assert_eq!(recovery.inventory(&session).unwrap().count(), expected);
        assert_eq!(
            recovery.retains_replica_data(&session, &source).unwrap(),
            expected == 3
        );
    }
    assert!(recovery.ingest(&session, &restricted[0]).is_err());
    assert!(
        recovery
            .commit_catalog(&session, b"still-no-authoring")
            .is_err()
    );
    assert_eq!(
        std::fs::read(source.claim_dir.join(&source.claim_name)).unwrap(),
        original_claim
    );
    assert_eq!(
        std::fs::read(original.join("table.astream")).unwrap(),
        original_log
    );
    drop(combined);
    let combined = Store::observe_recovery(&session, &recovery_path, scope)
        .unwrap()
        .with_observed_primary(
            &session,
            Store::observe(&session, &original, scope).unwrap(),
        )
        .unwrap();
    source
        .commit_catalog(&session, b"a-new-primary-receipt")
        .unwrap();
    assert!(
        combined
            .catalog(&session)
            .err()
            .is_some_and(|error| error.is_busy())
    );
}

/// A combined store's check reads its own claim after its primary's check, which can wait in a
/// generation guard: a witness a writer advances during that wait is that check's to see, not the
/// next one's.
///
/// NEGATIVE CONTROL (2026-11-05): with the claim read before `primary.check`, as it was, this fails
/// at "the advance went unseen: Ok(())".
#[test]
fn a_witness_advanced_while_a_primary_is_checked_is_seen_by_that_check() {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let original = lab._home.path().join("primary");
    let scope = Store::create(&session, &original).unwrap().scope();
    let recovery_path = bootstrap
        .paths()
        .dir
        .join("commons/recovery/tables")
        .join(aterm_codec::hex::encode_lower(&scope.digest()));
    drop(Store::join_recovery(&session, &recovery_path, scope).unwrap());
    let combined = Store::observe_recovery(&session, &recovery_path, scope)
        .unwrap()
        .with_observed_primary(
            &session,
            Store::observe(&session, &original, scope).unwrap(),
        )
        .unwrap();
    combined.check(&session).unwrap();

    let claim = combined.claim_dir.join(&combined.claim_name);
    let mut advanced = std::fs::read(&claim).unwrap();
    let count = Witness::decode(&advanced[WITNESS_AT..]).unwrap().count;
    advanced[WITNESS_AT..WITNESS_AT + 8].copy_from_slice(&(count + 1).to_le_bytes());
    let answer = with_primary_checked(
        move || aterm_secrets::fsx::write_atomic(&claim, &advanced, 0o600).unwrap(),
        || combined.check(&session),
    );
    assert!(
        answer.as_ref().is_err_and(Error::is_busy),
        "the advance went unseen: {answer:?}"
    );
}

#[test]
fn stronger_local_restrictions_and_positive_limits_do_not_block_negative_relay() {
    let lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let original = lab
        ._home
        .path()
        .join("primary-before-a-peer-reported-its-fork");
    let mut source = Store::create(&session, &original).unwrap();
    let scope = source.scope();
    let receipt = source
        .commit_catalog(&session, b"one-signed-prefix")
        .unwrap();
    let wire = source
        .wire(&session, &receipt.id)
        .unwrap()
        .unwrap()
        .to_vec();
    let contradictory = session
        .seal(
            scope,
            Causal {
                sequence: 1,
                previous: None,
                parents: &[],
            },
            b"another-signed-prefix",
        )
        .unwrap();
    let proof = Conflict::from_pair(&session, scope, &wire, &contradictory).unwrap();
    let other = lab.bootstrap(2);
    let signer = Session::open(&other).unwrap();
    let mut other_store =
        Store::join(&signer, &lab._home.path().join("other-writer"), scope).unwrap();
    let mut permitted = BTreeSet::new();
    for payload in [b"other-one".as_slice(), b"other-two"] {
        let receipt = other_store.commit_catalog(&signer, payload).unwrap();
        permitted.insert(receipt.id);
        source
            .ingest(
                &session,
                other_store.wire(&signer, &receipt.id).unwrap().unwrap(),
            )
            .unwrap();
    }
    let before = std::fs::read(original.join("table.astream")).unwrap();
    let recovery_path = bootstrap
        .paths()
        .dir
        .join("commons/recovery/tables")
        .join(aterm_codec::hex::encode_lower(&scope.digest()));
    let mut recovery = Store::join_recovery(&session, &recovery_path, scope).unwrap();
    recovery.ingest_conflict(&session, proof.wire()).unwrap();
    // The ordinary positive admission limit is full after one row; negative
    // evidence still has its reserved capacity and must remain relayable.
    recovery.capacity.operations = 1;
    for _ in 0..2 {
        assert!(
            recovery
                .retain_replica_batch(&session, &source, 16)
                .unwrap()
        );
        assert_eq!(recovery.proof_inventory(&session).unwrap().len(), 1);
        let retained: BTreeSet<_> = recovery.inventory(&session).unwrap().copied().collect();
        assert_eq!(retained.len(), 1);
        assert!(retained.is_subset(&permitted));
        assert!(!retained.contains(&receipt.id));
        assert!(!recovery.retains_replica_data(&session, &source).unwrap());
    }
    assert_eq!(
        std::fs::read(original.join("table.astream")).unwrap(),
        before
    );
}

#[test]
fn recovery_receives_authenticated_history_without_resuming_or_replacing_the_original_writer() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let source = lab.bootstrap(0);
    let session = Session::open(&source).unwrap();
    let mut original = Store::create(&session, &lab._home.path().join("original")).unwrap();
    let scope = original.scope();
    let first = original
        .commit_catalog(&session, b"retained-on-a-peer")
        .unwrap();
    let wire = original
        .wire(&session, &first.id)
        .unwrap()
        .unwrap()
        .to_vec();
    let original_claim_path = original.claim_dir.join(&original.claim_name);
    let original_claim = std::fs::read(&original_claim_path).unwrap();
    let original_log = std::fs::read(original.data_dir.join("table.astream")).unwrap();
    let original_dir = original.data_dir.clone();
    drop(original);
    let displaced = lab._home.path().join("displaced");
    fsx::ensure_private_dir(&displaced).unwrap();
    assert!(Store::open(&session, &displaced, scope).is_err());

    let recovering_dir = source
        .paths()
        .dir
        .join("commons/recovery/tables")
        .join(aterm_codec::hex::encode_lower(&scope.digest()));
    let mut replica = Store::join_recovery(&session, &recovering_dir, scope).unwrap();
    assert!(replica.is_recovery());
    assert!(
        replica
            .commit_catalog(&session, b"must-not-start-at-one")
            .is_err()
    );
    assert_eq!(replica.ingest(&session, &wire).unwrap().id, first.id);
    assert_eq!(
        replica.catalog(&session).unwrap()[0].payload(),
        b"retained-on-a-peer"
    );
    assert!(
        replica
            .commit_catalog(&session, b"must-not-continue-a-peer-prefix")
            .is_err()
    );
    assert_eq!(replica.inventory(&session).unwrap().count(), 1);
    let recovery_claim = std::fs::read(replica.claim_dir.join(&replica.claim_name)).unwrap();
    assert_eq!(&recovery_claim[..5], b"ATSR1");
    drop(replica);
    assert!(Store::open(&session, &recovering_dir, scope).is_err());
    assert!(Store::join(&session, &recovering_dir, scope).is_err());
    assert_eq!(std::fs::read(&original_claim_path).unwrap(), original_claim);
    assert_eq!(
        std::fs::read(original_dir.join("table.astream")).unwrap(),
        original_log
    );
    let mut reopened = Store::open_recovery(&session, &recovering_dir, scope).unwrap();
    assert!(reopened.is_recovery());
    assert!(
        reopened
            .commit_catalog(&session, b"restart-does-not-unfence")
            .is_err()
    );
    assert_eq!(reopened.catalog(&session).unwrap()[0].header().id, first.id);
    drop((reopened, session));
    let observed = Session::observe(&source).unwrap();
    let replica = Store::observe_recovery(&observed, &recovering_dir, scope).unwrap();
    assert_eq!(replica.inventory(&observed).unwrap().count(), 1);
    drop((replica, observed));
    let session = Session::open(&source).unwrap();
    let mut original = Store::open(&session, &original_dir, scope).unwrap();
    let second = original
        .commit_catalog(&session, b"original-custody-still-continues")
        .unwrap();
    assert_eq!(original.index.get(&second.id).unwrap().sequence, 2);
}

pub(crate) fn recovery_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    recovery_once(state, action, false)
}

fn recovery_once(
    state: &aterm_spec::interp::State,
    action: &str,
    bypass_authoring_fence: bool,
) -> aterm_spec::interp::State {
    let scenario = match action {
        "Receive" => 1,
        "Reopen" => 2,
        "AuthorEmpty" => 3,
        "AuthorReceived" => 4,
        "AuthorRestart" => 5,
        "Promote" => 6,
        "Corrupt" => 7,
        "WrongScope" => 8,
        "ChangedControl" => 9,
        _ => panic!("unknown recovery action {action}"),
    };
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let original_dir = lab._home.path().join("original");
    let mut original = Store::create(&session, &original_dir).unwrap();
    let scope = original.scope();
    let receipt = original
        .commit_catalog(&session, b"original-actor-history")
        .unwrap();
    let wire = original
        .wire(&session, &receipt.id)
        .unwrap()
        .unwrap()
        .to_vec();
    let claim_path = original.claim_dir.join(&original.claim_name);
    let original_claim = std::fs::read(&claim_path).unwrap();
    let original_log = std::fs::read(original_dir.join("table.astream")).unwrap();
    drop(original);
    let recovering_dir = bootstrap
        .paths()
        .dir
        .join("commons/recovery/tables")
        .join(aterm_codec::hex::encode_lower(&scope.digest()));
    let mut replica = Store::join_recovery(&session, &recovering_dir, scope).unwrap();
    if action != "AuthorEmpty" {
        replica.ingest(&session, &wire).unwrap();
    }
    let mut next = state.clone();
    next.insert("scenario", scenario);
    match action {
        "Receive" => {
            let records = replica.catalog(&session).unwrap();
            next.insert(
                "readable",
                i64::from(records.len() == 1 && records[0].header().id == receipt.id),
            );
        }
        "Reopen" => {
            drop(replica);
            let reopened = Store::open_recovery(&session, &recovering_dir, scope).unwrap();
            let records = reopened.catalog(&session).unwrap();
            next.insert(
                "readable",
                i64::from(records.len() == 1 && records[0].header().id == receipt.id),
            );
        }
        "AuthorEmpty" | "AuthorReceived" | "AuthorRestart" => {
            if action == "AuthorRestart" {
                drop(replica);
                replica = Store::open_recovery(&session, &recovering_dir, scope).unwrap();
            }
            next.insert("readable", replica.catalog(&session).unwrap().len() as i64);
            // Negative control changes the actual Store branch used by commit_from,
            // so a failure to detect it would permit a real signed append.
            if bypass_authoring_fence {
                replica.region = Region::Primary;
            }
            next.insert(
                "authored",
                i64::from(
                    replica
                        .commit_catalog(&session, b"unsafe-succession")
                        .is_ok(),
                ),
            );
        }
        "Promote" => {
            drop(replica);
            let opened = Store::open(&session, &recovering_dir, scope).is_ok();
            let joined = Store::join(&session, &recovering_dir, scope).is_ok();
            next.insert("promoted", i64::from(opened || joined));
        }
        "Corrupt" => {
            let recovery_claim = replica.claim_dir.join(&replica.claim_name);
            drop(replica);
            let mut damaged = std::fs::read(&recovery_claim).unwrap();
            damaged[ANCHOR_LEN - 1] ^= 1;
            std::fs::write(&recovery_claim, &damaged).unwrap();
            next.insert(
                "readable",
                i64::from(Store::open_recovery(&session, &recovering_dir, scope).is_ok()),
            );
            assert_eq!(std::fs::read(&recovery_claim).unwrap(), damaged);
        }
        "WrongScope" => {
            drop(replica);
            next.insert(
                "readable",
                i64::from(
                    Store::open_recovery(&session, &recovering_dir, session.scope().unwrap())
                        .is_ok(),
                ),
            );
        }
        "ChangedControl" => {
            drop(session);
            lab.rotate(&[Role::Reader, Role::Writer]);
            let fresh_bootstrap = lab.bootstrap(0);
            let fresh = Session::observe(&fresh_bootstrap).unwrap();
            next.insert("readable", i64::from(replica.catalog(&fresh).is_ok()));
        }
        _ => unreachable!(),
    }
    next.insert(
        "original",
        i64::from(
            std::fs::read(&claim_path).unwrap() == original_claim
                && std::fs::read(original_dir.join("table.astream")).unwrap() == original_log,
        ),
    );
    next
}

#[test]
fn recovery_conforms_and_a_real_signed_append_without_the_authoring_fence_is_detected() {
    let model = aterm_models_db::table_recovery_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    let initial = model.init_state();
    for action in &model.actions {
        let actual = recovery_transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&actual),
            "{}: {actual:?}",
            action.name
        );
    }
    for action in ["AuthorEmpty", "AuthorReceived", "AuthorRestart"] {
        let unsafe_append = recovery_once(&initial, action, true);
        assert_eq!(
            unsafe_append["authored"], 1,
            "negative control must reach a real signed append"
        );
        assert!(!model.successors(action, &initial).contains(&unsafe_append));
        assert!(!model.check_invariant("AuthorshipFenced", &unsafe_append));
    }
}

#[test]
fn recovery_keeps_a_legacy_device_mismatch_fenced_without_inventing_a_volume_witness() {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let original_dir = lab._home.path().join("legacy");
    let mut original = Store::create(&session, &original_dir).unwrap();
    let scope = original.scope();
    let receipt = original
        .commit_catalog(&session, b"history-retained-elsewhere")
        .unwrap();
    let wire = original
        .wire(&session, &receipt.id)
        .unwrap()
        .unwrap()
        .to_vec();
    let claim_dir = original.claim_dir.clone();
    let name = original.claim_name.clone();
    let claim = std::fs::read(claim_dir.join(&name)).unwrap();
    let data = std::fs::read(original_dir.join("table.astream")).unwrap();
    drop(original);
    directory::with_probe(
        directory::Probe {
            dev_shift: 1,
            volume: Some([9; 16]),
        },
        || {
            assert!(Store::open(&session, &original_dir, scope).is_err());
            let recovered_dir = bootstrap
                .paths()
                .dir
                .join("commons/recovery/tables")
                .join(aterm_codec::hex::encode_lower(&scope.digest()));
            let mut recovery = Store::join_recovery(&session, &recovered_dir, scope).unwrap();
            recovery.ingest(&session, &wire).unwrap();
            assert_eq!(
                recovery.catalog(&session).unwrap()[0].header().id,
                receipt.id
            );
            assert!(recovery.commit_catalog(&session, b"no-reset").is_err());
            assert!(Store::open(&session, &original_dir, scope).is_err());
            assert!(directory::witness(&claim_dir, &name).unwrap().is_none());
            assert_eq!(std::fs::read(claim_dir.join(&name)).unwrap(), claim);
            assert_eq!(
                std::fs::read(original_dir.join("table.astream")).unwrap(),
                data
            );
        },
    );
}
