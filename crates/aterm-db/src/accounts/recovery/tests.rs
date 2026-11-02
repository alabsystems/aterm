// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::table::tests::{Lab, repair_commons_at};
use aterm_secrets::commons::roster::Role;

const NOW: u64 = 20_000 * 86_400 + 100;

#[path = "cooldown_tests.rs"]
mod cooldown;
#[path = "decision_tests.rs"]
mod decision;
#[path = "dispatch_tests.rs"]
mod dispatch;

fn prepared() -> (
    Lab,
    aterm_secrets::client::Admin,
    crate::accounts::activations::Activations,
    Digest,
    aterm_secrets::commons::accounts::Record,
) {
    use crate::accounts::{activations::Activations, proposals::Proposals};
    use aterm_secrets::commons::accounts::IntentRef;

    let lab = Lab::new(&[Role::Writer]);
    Proposals::at(lab.paths[0].clone())
        .write(
            [1; 16],
            vec![Change {
                collection: "accounts".into(),
                key: "one".into(),
                value: Some(json!({"provider":"anthropic","method":"oauth","label":"ready"})),
            }],
        )
        .unwrap();
    let admin = aterm_secrets::client::admin_open(&lab.paths[0]).unwrap();
    let db = Activations::at(lab.paths[0].clone());
    let request = db
        .intent_configuration(&admin, [7; 32], "one", vec![])
        .unwrap();
    let (intent, _) = db.issue(&admin, [2; 16], request).unwrap();
    let binding = lab.bootstrap(0).request().unwrap().binding().clone();
    let child = IntentRef {
        account: [7; 32],
        decision: intent.id(),
    }
    .device_account(&binding);
    let (old, _) = db.materialize_next_intent(&admin).unwrap().unwrap();
    (lab, admin, db, child, old)
}

#[test]
fn a_delayed_old_decision_cannot_hide_the_current_recovery_request() {
    use crate::accounts::activations::State;
    let (lab, admin, db, child, old) = prepared();
    let binding = lab.bootstrap(0).request().unwrap().binding().clone();
    let (current, _) = db
        .reissue_intent_preparation(&admin, child, old.id(), &binding, [3; 16])
        .unwrap();
    let requests = Recovery::at(lab.paths[0].clone());
    requests.request(child, current.id(), [4; 16]).unwrap();
    requests.request(child, old.id(), [5; 16]).unwrap();

    let mut queue = Queue {
        cooldown: Duration::ZERO,
        ..Queue::default()
    };
    assert_eq!(
        queue
            .step(
                &lab.paths[0],
                &admin,
                Instant::now() + Duration::from_secs(60),
            )
            .unwrap(),
        1,
        "the delayed old notice cannot hide the current decision's request"
    );
    let State::Ready(recovered) = db.state(child).unwrap() else {
        panic!("current recovery did not remain ready")
    };
    assert_ne!(recovered, current.id());
    assert_ne!(recovered, old.id());
    queue
        .step(
            &lab.paths[0],
            &admin,
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();
    assert_eq!(db.state(child).unwrap(), State::Ready(recovered));
}

#[test]
fn fulfilled_requests_neither_dispatch_again_nor_count_as_new_work() {
    use crate::accounts::activations::State;
    let (lab, admin, db, child, first) = prepared();
    let requests = Recovery::at(lab.paths[0].clone());
    let mut queue = Queue {
        cooldown: Duration::ZERO,
        ..Queue::default()
    };
    let step = |queue: &mut Queue| {
        queue
            .step(
                &lab.paths[0],
                &admin,
                Instant::now() + Duration::from_secs(60),
            )
            .unwrap()
    };
    requests.request(child, first.id(), [3; 16]).unwrap();
    assert_eq!(step(&mut queue), 1);
    let State::Ready(second) = db.state(child).unwrap() else {
        panic!("first recovery did not produce a successor")
    };
    requests.request(child, second, [4; 16]).unwrap();
    requests.request(child, first.id(), [3; 16]).unwrap();
    assert_eq!(step(&mut queue), 1, "only the current decision advances");
    let State::Ready(third) = db.state(child).unwrap() else {
        panic!("second recovery did not produce a successor")
    };
    assert_ne!(third, second);
    assert_eq!(step(&mut queue), 0, "fulfilled requests are no new work");
    assert!(queue.due.is_empty(), "fulfilled requests consume no slots");
    assert_eq!(db.state(child).unwrap(), State::Ready(third));
}

#[test]
fn recovery_is_own_signed_metadata_and_same_attempt_retransmits_without_new_authority() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let writer = Recovery::at(lab.paths[0].clone());
    let reader = Recovery::at(lab.paths[1].clone());
    writer.request_at([1; 32], [2; 32], [3; 16], NOW).unwrap();
    writer.request_at([1; 32], [2; 32], [3; 16], NOW).unwrap();
    let first = writer.pending_at(NOW).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].account(), [1; 32]);
    assert_eq!(
        first[0].requester().actor(),
        lab.bootstrap(0).request().unwrap().binding().actor()
    );
    repair_commons_at(&lab, 3, &[NOW, NOW]);
    let received = reader.pending_at(NOW).unwrap();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].issue_nonce(), first[0].issue_nonce());
    assert_eq!(received[0].requester(), first[0].requester());
    writer
        .request_at([1; 32], [2; 32], [3; 16], NOW + 86_400)
        .unwrap();
    assert_eq!(
        writer.pending_at(NOW + 86_400).unwrap()[0].issue_nonce(),
        first[0].issue_nonce()
    );
}

#[test]
fn foreign_key_and_changed_retry_identity_never_enter_the_request_queue() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let paths = &lab.paths[0];
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(paths, bootstrap.trust());
    let period = RECOVERIES.epoch_of(NOW);
    let mut store = directory
        .enroll_ring(&session, &RECOVERIES, period, NOW)
        .unwrap();
    let foreign = lab.bootstrap(1).request().unwrap().binding().actor();
    let forged = command(foreign, [1; 32], [2; 32], [3; 16], period);
    store
        .commit_catalog(&session, &forged.encode().unwrap())
        .unwrap();
    let mut malformed = command(session.actor(), [1; 32], [2; 32], [3; 16], period);
    if let Command::Write { request, .. } = &mut malformed {
        *request = [9; 16];
    }
    assert!(decode(&malformed.encode().unwrap()).is_err());
    drop(store);
    drop(session);
    assert!(
        Recovery::at(paths.clone())
            .pending_at(NOW)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn an_unattempted_batch_remains_ready_after_dispatch_is_interrupted() {
    let lab = Lab::new(&[Role::Writer]);
    let db = Recovery::at(lab.paths[0].clone());
    db.request_at([1; 32], [2; 32], [3; 16], NOW).unwrap();
    db.request_at([4; 32], [5; 32], [6; 16], NOW).unwrap();
    let requests = db.pending_at(NOW).unwrap();
    let start = Instant::now();
    let mut queue = Queue::default();
    assert!(queue.ready(requests.clone(), start, 2).is_empty());
    assert_eq!(
        queue
            .ready(requests.clone(), start + Duration::from_secs(60), 2)
            .len(),
        2
    );
    // The issuer's deadline or opt-out can end the batch before any attempt.
    assert_eq!(
        queue
            .ready(requests, start + Duration::from_secs(61), 2)
            .len(),
        2,
        "selection alone must not restart either request's cooldown"
    );
}

#[test]
fn recovery_cooldown_is_bounded_monotonic_and_does_not_retry_each_scan() {
    let lab = Lab::new(&[Role::Writer]);
    let db = Recovery::at(lab.paths[0].clone());
    db.request_at([1; 32], [2; 32], [3; 16], NOW).unwrap();
    let requests = db.pending_at(NOW).unwrap();
    let start = Instant::now();
    let mut queue = Queue::default();
    assert!(queue.ready(requests.clone(), start, 1).is_empty());
    assert!(
        queue
            .ready(requests.clone(), start + Duration::from_secs(59), 1)
            .is_empty()
    );
    let at = start + Duration::from_secs(60);
    assert_eq!(
        queue
            .dispatch(requests.clone(), at, 1, || Ok(Some(at)), |_| true)
            .unwrap(),
        (1, false)
    );
    assert!(
        queue
            .ready(requests.clone(), start + Duration::from_secs(61), 1)
            .is_empty()
    );
    assert_eq!(
        queue
            .ready(requests, start + Duration::from_secs(120), 1)
            .len(),
        1
    );
    assert!(
        queue
            .ready(Vec::new(), start + Duration::from_secs(121), 1)
            .is_empty()
    );
    assert!(queue.due.is_empty());
}

#[test]
fn another_account_and_an_unchanged_retry_cannot_hide_pending_recovery() {
    let lab = Lab::new(&[Role::Writer]);
    let db = Recovery::at(lab.paths[0].clone());
    db.request_at([1; 32], [2; 32], [3; 16], NOW).unwrap();
    db.request_at([4; 32], [5; 32], [6; 16], NOW).unwrap();
    db.request_at([1; 32], [2; 32], [3; 16], NOW).unwrap();
    let requests = db.pending_at(NOW).unwrap();
    assert_eq!(requests.len(), 2);
    let start = Instant::now();
    let mut queue = Queue::default();
    assert!(queue.ready(requests.clone(), start, 1).is_empty());
    let first = queue.ready(requests.clone(), start + Duration::from_secs(60), 1);
    let at = start + Duration::from_secs(60);
    queue
        .dispatch(requests.clone(), at, 1, || Ok(Some(at)), |_| false)
        .unwrap();
    let second = queue.ready(requests.clone(), start + Duration::from_secs(120), 1);
    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert_ne!(first[0].account(), second[0].account());
    assert_eq!(db.pending_at(NOW).unwrap().len(), 2);
}

#[test]
fn an_invalid_unrelated_intent_does_not_block_a_current_recovery() {
    use crate::accounts::{
        activations::{Activations, State},
        proposals::Proposals,
    };
    use aterm_secrets::commons::accounts::IntentRef;
    let lab = Lab::new(&[Role::Writer]);
    let proposals = Proposals::at(lab.paths[0].clone());
    let value = json!({"provider":"anthropic","method":"oauth","label":"ready"});
    proposals
        .write(
            [1; 16],
            vec![
                Change {
                    collection: "accounts".into(),
                    key: "one".into(),
                    value: Some(value.clone()),
                },
                Change {
                    collection: "accounts".into(),
                    key: "two".into(),
                    value: Some(value),
                },
            ],
        )
        .unwrap();
    let admin = aterm_secrets::client::admin_open(&lab.paths[0]).unwrap();
    let db = Activations::at(lab.paths[0].clone());
    let request = db
        .intent_configuration(&admin, [7; 32], "one", vec![])
        .unwrap();
    let (intent, _) = db.issue(&admin, [2; 16], request).unwrap();
    let binding = lab.bootstrap(0).request().unwrap().binding().clone();
    let child = IntentRef {
        account: [7; 32],
        decision: intent.id(),
    }
    .device_account(&binding);
    let (first, _) = db.materialize_next_intent(&admin).unwrap().unwrap();
    let request = db
        .intent_configuration(&admin, [8; 32], "two", vec![])
        .unwrap();
    db.issue(&admin, [3; 16], request).unwrap();
    proposals
        .write(
            [4; 16],
            vec![Change {
                collection: "accounts".into(),
                key: "two".into(),
                value: Some(json!({"provider":"openai","method":"device","label":"changed"})),
            }],
        )
        .unwrap();
    assert!(db.materialize_next_intent(&admin).is_err());
    Recovery::at(lab.paths[0].clone())
        .request(child, first.id(), [5; 16])
        .unwrap();
    let mut queue = Queue {
        cooldown: Duration::ZERO,
        ..Queue::default()
    };
    assert!(queue.step(&lab.paths[0], &admin, Instant::now()).is_err());
    assert_eq!(db.state(child).unwrap(), State::Ready(first.id()));
    assert_eq!(
        queue
            .step(
                &lab.paths[0],
                &admin,
                Instant::now() + Duration::from_secs(60)
            )
            .unwrap(),
        1
    );
    let State::Ready(recovered) = db.state(child).unwrap() else {
        panic!("child stayed ready")
    };
    assert_ne!(recovered, first.id());
}
