// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::resources::{
    Proposal, RequestBinding,
    tests::{allocation, resources, rounds},
};
use crate::table::tests::Lab;
use aterm_secrets::commons::roster::Role;
use std::time::Duration;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(60)
}
fn key(n: u8) -> RequestKey {
    RequestKey::new([51; 32], [n; 16]).unwrap()
}
fn request(key: RequestKey, allocation: &Allocation, revision: u8) -> BoundRequest {
    BoundRequest::new(
        RequestBinding {
            principal: allocation.terms().resource,
            account: [61; 32],
            revision: [revision; 32],
            credential: [63; 32],
            policy: [64; 32],
            adapter: "example-v1".into(),
        },
        key.ledger_id(),
        1,
        b"private request",
    )
    .unwrap()
}

#[test]
fn request_identity_is_stable_and_separates_producers() {
    assert!(RequestKey::new([0; 32], [1; 16]).is_err());
    assert!(RequestKey::new([1; 32], [0; 16]).is_err());
    let a = key(1);
    assert_eq!(a, RequestKey::new(a.producer(), a.logical()).unwrap());
    assert_ne!(a.ledger_id(), key(2).ledger_id());
    assert_ne!(
        a.ledger_id(),
        RequestKey::new([52; 32], a.logical()).unwrap().ledger_id()
    );
}

#[test]
fn claims_survive_reopen_and_reject_policy_and_window_rebinding() {
    let lab = Lab::new(&[Role::Writer]);
    let allocation = allocation(&lab, &[3]);
    let registry = RequestRegistry::at(lab.paths[0].clone());
    assert!(registry.lookup_until(key(1), deadline()).is_err());
    let first = registry
        .bind_until(
            key(1),
            &allocation,
            &request(key(1), &allocation, 62),
            deadline(),
        )
        .unwrap();
    assert!(first.fresh);
    let registry = RequestRegistry::at(lab.paths[0].clone());
    let retry = registry
        .bind_until(
            key(1),
            &allocation,
            &request(key(1), &allocation, 62),
            deadline(),
        )
        .unwrap();
    assert!(!retry.fresh);
    assert_eq!(retry.claim.receipt, first.claim.receipt);
    assert_eq!(
        registry
            .lookup_until(key(1), deadline())
            .unwrap()
            .unwrap()
            .receipt,
        first.claim.receipt
    );
    assert!(
        registry
            .bind_until(
                key(1),
                &allocation,
                &request(key(1), &allocation, 65),
                deadline()
            )
            .is_err()
    );
    let mut terms = allocation.terms().clone();
    terms.window = [99; 32];
    let proposal = Proposal::prepare(allocation.trust(), terms).unwrap();
    let other =
        Allocation::verify(proposal.bytes(), proposal.fingerprint(), allocation.trust()).unwrap();
    assert!(
        registry
            .bind_until(key(1), &other, &request(key(1), &other, 62), deadline())
            .is_err()
    );
    assert!(
        resources(&lab, 0, &allocation).snapshot().is_err(),
        "claim did not enroll or charge a resource"
    );
    assert!(
        registry
            .bind_until(
                key(2),
                &allocation,
                &request(key(1), &allocation, 62),
                deadline()
            )
            .is_err()
    );
}

#[test]
fn offline_writers_keep_separate_request_namespaces_through_astream_repair() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let allocation = allocation(&lab, &[2, 2]);
    let a = RequestRegistry::at(lab.paths[0].clone());
    let b = RequestRegistry::at(lab.paths[1].clone());
    let left = a
        .bind_until(
            key(1),
            &allocation,
            &request(key(1), &allocation, 62),
            deadline(),
        )
        .unwrap();
    let right = b
        .bind_until(
            key(1),
            &allocation,
            &request(key(1), &allocation, 65),
            deadline(),
        )
        .unwrap();
    assert_ne!(left.claim.writer, right.claim.writer);
    rounds(&lab);
    assert_eq!(
        a.lookup_until(key(1), deadline()).unwrap().unwrap().receipt,
        left.claim.receipt
    );
    assert_eq!(
        b.lookup_until(key(1), deadline()).unwrap().unwrap().receipt,
        right.claim.receipt
    );
    assert!(
        !a.bind_until(
            key(1),
            &allocation,
            &request(key(1), &allocation, 62),
            deadline()
        )
        .unwrap()
        .fresh
    );
    assert!(
        !b.bind_until(
            key(1),
            &allocation,
            &request(key(1), &allocation, 65),
            deadline()
        )
        .unwrap()
        .fresh
    );
}

#[test]
fn missing_registry_custody_never_creates_fresh_request_history() {
    let lab = Lab::new(&[Role::Writer]);
    let allocation = allocation(&lab, &[2]);
    let registry = RequestRegistry::at(lab.paths[0].clone());
    let first = registry
        .bind_until(
            key(1),
            &allocation,
            &request(key(1), &allocation, 62),
            deadline(),
        )
        .unwrap();
    let bootstrap = lab.bootstrap(0);
    let scope = descriptor(&bootstrap).unwrap().scope;
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let path = directory.path(scope).unwrap();
    let preserved = lab._home.path().join("preserved-request-history");
    std::fs::rename(&path, &preserved).unwrap();
    assert!(
        registry
            .bind_until(
                key(1),
                &allocation,
                &request(key(1), &allocation, 62),
                deadline()
            )
            .is_err()
    );
    assert!(registry.lookup_until(key(1), deadline()).is_err());
    assert!(!path.exists());
    std::fs::rename(&preserved, &path).unwrap();
    assert_eq!(
        registry
            .lookup_until(key(1), deadline())
            .unwrap()
            .unwrap()
            .receipt,
        first.claim.receipt
    );
}

#[test]
fn registry_pin_survives_data_and_claim_loss_and_explicit_reenrollment() {
    let lab = Lab::new(&[Role::Writer]);
    let allocation = allocation(&lab, &[2]);
    let registry = RequestRegistry::at(lab.paths[0].clone());
    registry
        .bind_until(
            key(1),
            &allocation,
            &request(key(1), &allocation, 62),
            deadline(),
        )
        .unwrap();
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let scope = descriptor(&bootstrap).unwrap().scope;
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let data = directory.path(scope).unwrap();
    let claim = crate::table::store::claim_dir(&session)
        .unwrap()
        .join(crate::table::store::claim_name(scope));
    drop(session);
    std::fs::rename(&data, data.with_extension("preserved")).unwrap();
    std::fs::rename(&claim, claim.with_extension("preserved")).unwrap();
    assert!(
        registry
            .bind_until(
                key(1),
                &allocation,
                &request(key(1), &allocation, 62),
                deadline()
            )
            .is_err()
    );
    assert!(registry.lookup_until(key(1), deadline()).is_err());
    let session = Session::open(&bootstrap).unwrap();
    directory.enroll(&session, scope).unwrap();
    drop(session);
    assert!(
        registry
            .bind_until(
                key(1),
                &allocation,
                &request(key(1), &allocation, 62),
                deadline()
            )
            .is_err()
    );
    assert!(registry.lookup_until(key(1), deadline()).is_err());
}

#[test]
fn lost_registry_pin_cannot_turn_known_history_into_first_use() {
    let lab = Lab::new(&[Role::Writer]);
    let allocation = allocation(&lab, &[2]);
    let registry = RequestRegistry::at(lab.paths[0].clone());
    let first = registry
        .bind_until(
            key(1),
            &allocation,
            &request(key(1), &allocation, 62),
            deadline(),
        )
        .unwrap();
    let pin = pin_path(&lab.paths[0]);
    std::fs::rename(&pin, pin.with_extension("preserved")).unwrap();
    assert!(
        registry
            .bind_until(
                key(2),
                &allocation,
                &request(key(2), &allocation, 62),
                deadline()
            )
            .is_err()
    );
    assert!(registry.lookup_until(key(2), deadline()).is_err());
    assert_eq!(
        registry
            .lookup_until(key(1), deadline())
            .unwrap()
            .unwrap()
            .receipt,
        first.claim.receipt
    );
    assert!(!pin.exists());
}

#[test]
fn interrupted_first_publication_resumes_but_pending_pin_cannot_hide_old_claims() {
    let lab = Lab::new(&[Role::Writer]);
    let allocation = allocation(&lab, &[2]);
    let registry = RequestRegistry::at(lab.paths[0].clone());
    registry.test_pending_enrollment(deadline()).unwrap();
    let scope = descriptor(&lab.bootstrap(0)).unwrap().scope.digest();
    assert!(matches!(
        read_pin(&lab.paths[0], scope),
        Ok(Some(Pin::Pending(_)))
    ));
    let first = registry
        .bind_until(
            key(1),
            &allocation,
            &request(key(1), &allocation, 62),
            deadline(),
        )
        .unwrap();
    assert!(first.fresh);
    assert!(matches!(
        read_pin(&lab.paths[0], scope),
        Ok(Some(Pin::Bound(_)))
    ));
    assert!(
        !registry
            .bind_until(
                key(1),
                &allocation,
                &request(key(1), &allocation, 62),
                deadline()
            )
            .unwrap()
            .fresh
    );
    let path = pin_path(&lab.paths[0]);
    std::fs::rename(&path, path.with_extension("committed")).unwrap();
    write_pending(&lab.paths[0], scope).unwrap();
    assert!(
        registry
            .bind_until(
                key(2),
                &allocation,
                &request(key(2), &allocation, 62),
                deadline()
            )
            .is_err()
    );
    assert!(matches!(
        read_pin(&lab.paths[0], scope),
        Ok(Some(Pin::Pending(_)))
    ));
}

#[test]
fn unrelated_peer_payloads_are_retained_without_poisoning_local_requests() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let allocation = allocation(&lab, &[2, 2]);
    let a = RequestRegistry::at(lab.paths[0].clone());
    a.bind_until(
        key(1),
        &allocation,
        &request(key(1), &allocation, 62),
        deadline(),
    )
    .unwrap();
    rounds(&lab);
    // The peer's invalid application payload has no authority over our namespace.
    let peer = lab.bootstrap(1);
    let peer_session = Session::open(&peer).unwrap();
    let scope = descriptor(&peer).unwrap().scope;
    let peer_dir = Directory::new(&lab.paths[1], peer.trust());
    let mut peer_store = Store::open(&peer_session, &peer_dir.path(scope).unwrap(), scope).unwrap();
    let receipt = peer_store
        .commit_liability(&peer_session, b"unknown-protocol")
        .unwrap();
    let wire = peer_store
        .wire(&peer_session, &receipt.id)
        .unwrap()
        .unwrap()
        .to_vec();
    drop((peer_store, peer_session));
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let mut recovery =
        Store::join_recovery(&session, &directory.recovery().path(scope).unwrap(), scope).unwrap();
    recovery.ingest(&session, &wire).unwrap();
    drop((recovery, session));
    assert!(
        a.bind_until(
            key(2),
            &allocation,
            &request(key(2), &allocation, 62),
            deadline()
        )
        .is_ok()
    );
    assert!(a.lookup_until(key(1), deadline()).unwrap().is_some());
    let b = RequestRegistry::at(lab.paths[1].clone());
    assert!(
        b.bind_until(
            key(2),
            &allocation,
            &request(key(2), &allocation, 62),
            deadline()
        )
        .is_err()
    );
    let bootstrap = lab.bootstrap(0);
    let session = Session::observe(&bootstrap).unwrap();
    let primary = Store::observe(&session, &directory.path(scope).unwrap(), scope).unwrap();
    assert!(
        primary
            .inventory(&session)
            .unwrap()
            .any(|id| *id == receipt.id)
    );
}

#[test]
fn own_recovery_only_bad_payload_prevents_new_claims() {
    let lab = Lab::new(&[Role::Writer]);
    let allocation = allocation(&lab, &[2]);
    let registry = RequestRegistry::at(lab.paths[0].clone());
    let first = registry
        .bind_until(
            key(1),
            &allocation,
            &request(key(1), &allocation, 62),
            deadline(),
        )
        .unwrap();
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let scope = descriptor(&bootstrap).unwrap().scope;
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let primary = Store::observe(&session, &directory.path(scope).unwrap(), scope).unwrap();
    let sequence = primary
        .catalog(&session)
        .unwrap()
        .into_iter()
        .find(|r| r.header().id == first.claim.receipt.id)
        .unwrap()
        .header()
        .sequence
        + 1;
    let wire = crate::table::tests::seal(
        &session,
        scope,
        crate::crypto::Causal {
            sequence,
            previous: Some(first.claim.receipt.id),
            parents: &[first.claim.receipt.id],
        },
        b"unknown-protocol",
    )
    .unwrap();
    drop(primary);
    let mut recovery =
        Store::join_recovery(&session, &directory.recovery().path(scope).unwrap(), scope).unwrap();
    recovery.ingest(&session, &wire).unwrap();
    drop((recovery, session));
    assert!(
        registry
            .bind_until(
                key(2),
                &allocation,
                &request(key(2), &allocation, 62),
                deadline()
            )
            .is_err()
    );
    assert!(registry.lookup_until(key(1), deadline()).is_err());
}

#[test]
fn writer_succession_keeps_old_requests_held_without_blocking_new_ids() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let allocation = allocation(&lab, &[2, 2]);
    let registry = RequestRegistry::at(lab.paths[0].clone());
    let first = registry
        .bind_until(
            key(1),
            &allocation,
            &request(key(1), &allocation, 62),
            deadline(),
        )
        .unwrap();
    rounds(&lab);
    let config = lab.paths[0].home.join("harness.toml");
    aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
    let selected = crate::test_complete_writer_successor(
        lab._home.path(),
        &lab.paths[0],
        &lab.paths[1],
        &config,
        deadline(),
    )
    .unwrap();
    let inherited = registry.lookup_until(key(1), deadline()).unwrap().unwrap();
    assert_eq!(inherited.writer, first.claim.writer);
    let selected_registry = RequestRegistry::at(selected);
    assert_eq!(
        selected_registry
            .lookup_until(key(1), deadline())
            .unwrap()
            .unwrap()
            .writer,
        first.claim.writer
    );
    assert!(
        selected_registry
            .bind_until(
                key(1),
                &allocation,
                &request(key(1), &allocation, 62),
                deadline()
            )
            .is_err()
    );
    assert!(
        registry
            .bind_until(
                key(1),
                &allocation,
                &request(key(1), &allocation, 62),
                deadline()
            )
            .is_err()
    );
    assert!(
        registry
            .bind_until(
                key(2),
                &allocation,
                &request(key(2), &allocation, 62),
                deadline()
            )
            .unwrap()
            .fresh
    );
}
