// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 bindings for the durable operator's delivery, actuator-WAL,
//! fleet-fault, and leadership machines.
//!
//! These tests drive `DurableQueue`, including its real file lock and WAL replay,
//! then project each observed transition onto the same derived models checked at
//! Tier 0. Every transition is admitted by the in-process interpreter and, when
//! installed, by `ty trace validate`. Each machine also carries a deliberately
//! forged successor that the model must reject, so a green binding is non-vacuous.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use aterm_agent::operator::{
    AckOutcome, AttentionCondition, DurableQueue, EnqueueOutcome, EventGeneration, EventStatus,
    FaultLatchOutcome, FleetFaultReason, FleetGateStatus, NewEvent, OperatorError, QueueConfig,
    Resolution,
};
use aterm_spec::derive::{
    Model, operator_event_delivery_model, operator_fleet_fault_model, operator_leadership_model,
    operator_wal_actuator_model,
};
use aterm_spec::verify;
use sha2::{Digest as _, Sha256};

type State = BTreeMap<&'static str, i64>;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let nonce = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "aterm-operator-conformance-{label}-{}-{nonce}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn state_after(before: &State, changes: &[(&'static str, i64)]) -> State {
    let mut after = before.clone();
    for (name, value) in changes {
        after.insert(name, *value);
    }
    after
}

fn assert_transition(model: &Model, before: &State, after: &State, action: &str, label: &str) {
    let (accepted, diagnostics) = verify::validate_transition_tiered(
        model,
        &[("Buggy", 0)],
        before,
        after,
        Some(action),
        label,
    );
    assert!(
        accepted,
        "real {label} transition must be admitted as {action}\n{diagnostics}"
    );
}

fn assert_rejected(model: &Model, before: &State, forged: &State, action: &str, label: &str) {
    let (accepted, diagnostics) = verify::validate_transition_tiered(
        model,
        &[("Buggy", 0)],
        before,
        forged,
        Some(action),
        label,
    );
    assert!(
        !accepted,
        "{label} negative control was accepted as {action}; binding is vacuous\n{diagnostics}"
    );
}

fn delivery_config() -> QueueConfig {
    QueueConfig {
        capacity: 8,
        visibility_timeout: Duration::from_millis(10),
        max_cumulative_extension: Duration::from_millis(100),
        // The bounded model deliberately uses two rather than the production
        // default of three so the cap is reachable in a short trace.
        redelivery_cap: 2,
        max_wal_bytes: 2 * 1024 * 1024,
    }
}

fn event(value: u8) -> NewEvent {
    let evidence = format!("ready generation {value}");
    NewEvent::new(
        "sid-a",
        EventGeneration::new(
            1,
            false,
            u64::from(value),
            Sha256::digest(evidence.as_bytes()).into(),
        ),
        AttentionCondition::Ready,
        evidence,
    )
}

fn open_event(directory: &Path, value: u8) -> (DurableQueue, aterm_agent::operator::EventId) {
    let queue = DurableQueue::open(directory, 1, delivery_config()).expect("open operator queue");
    assert!(queue.manage_sid("sid-a").expect("manage sid"));
    let EnqueueOutcome::Enqueued(id) = queue.enqueue(event(value)).expect("enqueue event") else {
        panic!("fresh generation must enqueue");
    };
    (queue, id)
}

#[test]
fn operator_event_delivery_real_claim_cas_matches_model() {
    let directory = TestDir::new("delivery-cas");
    let (queue, event_id) = open_event(directory.path(), 1);
    let model = operator_event_delivery_model();
    let initial = model.init_state();

    let first = queue.claim_at(100).expect("claim").expect("queued event");
    assert_eq!(first.event.id, event_id);
    assert!(matches!(first.event.status, EventStatus::Delivered { .. }));
    let claimed_first = state_after(&initial, &[("phase", 1), ("token", 1), ("next_token", 2)]);
    assert_transition(
        &model,
        &initial,
        &claimed_first,
        "Claim",
        "operator delivery first claim",
    );

    // Deadline passage is the environment half of expiry. The real CAS refuses
    // an acknowledgement at the deadline before reclaim mutates the queue.
    assert!(matches!(
        queue.ack_at(event_id, &first.token, Resolution::NoAction, 110),
        Err(OperatorError::ClaimExpired(found)) if found == event_id
    ));
    let expired_first = state_after(&claimed_first, &[("expired", 1)]);
    assert_transition(
        &model,
        &claimed_first,
        &expired_first,
        "Expire",
        "operator delivery deadline",
    );

    let reclaimed = queue.reclaim_expired_at(110).expect("reclaim first claim");
    assert_eq!(reclaimed.len(), 1);
    assert!(!reclaimed[0].escalated);
    assert!(matches!(
        queue.status(event_id).expect("status").status,
        EventStatus::Queued
    ));
    let queued_again = state_after(
        &expired_first,
        &[
            ("phase", 0),
            ("token", 0),
            ("stale_token", 1),
            ("expired", 0),
            ("redeliveries", 1),
        ],
    );
    assert_transition(
        &model,
        &expired_first,
        &queued_again,
        "ReclaimForRetry",
        "operator delivery first reclaim",
    );

    let second = queue.claim_at(200).expect("reclaim").expect("redelivery");
    assert_ne!(
        first.token.expose(),
        second.token.expose(),
        "a redelivery must mint a fresh opaque claimant identity"
    );
    let claimed_second = state_after(
        &queued_again,
        &[("phase", 1), ("token", 2), ("next_token", 3)],
    );
    assert_transition(
        &model,
        &queued_again,
        &claimed_second,
        "Claim",
        "operator delivery second claim",
    );

    // The genuine atomic CAS refuses the old claimant after a newer claimant owns
    // the event. The healthy model represents that refusal as a no-op transition.
    assert!(matches!(
        queue.ack_at(event_id, &first.token, Resolution::NoAction, 201),
        Err(OperatorError::StaleClaim(found)) if found == event_id
    ));
    assert_transition(
        &model,
        &claimed_second,
        &claimed_second,
        "StaleAck",
        "operator delivery stale claimant refusal",
    );

    // NEGATIVE CONTROL: reproduce the forbidden stale-token regression. This is
    // exactly the `Buggy=1` successor and must not be admitted by healthy Next.
    let forged_stale_resolution = state_after(
        &claimed_second,
        &[
            ("phase", 2),
            ("resolution_token", 1),
            ("stale_regression", 1),
        ],
    );
    assert_rejected(
        &model,
        &claimed_second,
        &forged_stale_resolution,
        "StaleAck",
        "operator delivery stale-token regression",
    );

    assert_eq!(
        queue
            .ack_at(event_id, &second.token, Resolution::NoAction, 201)
            .expect("current claimant resolves"),
        AckOutcome::Resolved
    );
    let resolved = state_after(&claimed_second, &[("phase", 2), ("resolution_token", 2)]);
    assert_transition(
        &model,
        &claimed_second,
        &resolved,
        "ResolveCurrent",
        "operator delivery current claimant resolution",
    );
    assert_eq!(
        queue
            .ack_at(event_id, &second.token, Resolution::NoAction, 202)
            .expect("idempotent acknowledgement"),
        AckOutcome::AlreadyResolved
    );
    assert_transition(
        &model,
        &resolved,
        &resolved,
        "AckSame",
        "operator delivery idempotent acknowledgement",
    );
}

#[test]
fn operator_event_delivery_real_redelivery_cap_matches_model() {
    let directory = TestDir::new("delivery-cap");
    let (queue, event_id) = open_event(directory.path(), 2);
    let model = operator_event_delivery_model();
    let initial = model.init_state();

    let first = queue.claim_at(10).unwrap().unwrap();
    let claim_one = state_after(&initial, &[("phase", 1), ("token", 1), ("next_token", 2)]);
    let expire_one = state_after(&claim_one, &[("expired", 1)]);
    let retry = state_after(
        &expire_one,
        &[
            ("phase", 0),
            ("token", 0),
            ("stale_token", 1),
            ("expired", 0),
            ("redeliveries", 1),
        ],
    );
    queue.reclaim_expired_at(first.expires_at_ms).unwrap();
    let second = queue.claim_at(30).unwrap().unwrap();
    let claim_two = state_after(&retry, &[("phase", 1), ("token", 2), ("next_token", 3)]);
    let expire_two = state_after(&claim_two, &[("expired", 1)]);
    let outcomes = queue.reclaim_expired_at(second.expires_at_ms).unwrap();
    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].escalated);
    let snapshot = queue.status(event_id).unwrap();
    assert!(snapshot.escalated);
    assert_eq!(snapshot.redelivery_count, 2);
    assert_eq!(snapshot.condition, AttentionCondition::Escalation);
    assert!(matches!(snapshot.status, EventStatus::Queued));

    let escalation = state_after(
        &expire_two,
        &[
            ("phase", 3),
            ("token", 0),
            ("stale_token", 2),
            ("expired", 0),
            ("redeliveries", 2),
            ("escalated", 1),
        ],
    );
    assert_transition(
        &model,
        &expire_two,
        &escalation,
        "ReclaimAsEscalation",
        "operator delivery redelivery-cap conversion",
    );

    // The cap conversion is not itself terminal. Shipping code must surface the
    // escalation to a final human claimant, with a fresh opaque token.
    let final_claim = queue.claim_at(50).unwrap().unwrap();
    assert_eq!(final_claim.event.id, event_id);
    assert_eq!(final_claim.event.condition, AttentionCondition::Escalation);
    assert!(final_claim.event.escalated);
    assert_ne!(final_claim.token.expose(), second.token.expose());
    let escalation_delivered = state_after(
        &escalation,
        &[("phase", 4), ("token", 3), ("next_token", 4)],
    );
    assert_transition(
        &model,
        &escalation,
        &escalation_delivered,
        "ClaimEscalation",
        "operator delivery final escalation claim",
    );

    assert!(matches!(
        queue.ack_at(
            event_id,
            &final_claim.token,
            Resolution::NoAction,
            final_claim.expires_at_ms,
        ),
        Err(OperatorError::ClaimExpired(found)) if found == event_id
    ));
    let escalation_expired = state_after(&escalation_delivered, &[("expired", 1)]);
    assert_transition(
        &model,
        &escalation_delivered,
        &escalation_expired,
        "Expire",
        "operator delivery escalation deadline",
    );

    let final_outcome = queue.reclaim_expired_at(final_claim.expires_at_ms).unwrap();
    assert_eq!(final_outcome.len(), 1);
    assert!(matches!(
        queue.status(event_id).unwrap().status,
        EventStatus::InDoubt { token: Some(ref token), .. }
            if token == &final_claim.token
    ));
    let final_in_doubt = state_after(
        &escalation_expired,
        &[("phase", 5), ("expired", 0), ("in_doubt", 1)],
    );
    assert_transition(
        &model,
        &escalation_expired,
        &final_in_doubt,
        "ExpiredEscalationInDoubt",
        "operator delivery expired human escalation",
    );
}

fn wal_config() -> QueueConfig {
    QueueConfig {
        capacity: 8,
        visibility_timeout: Duration::from_secs(10),
        max_cumulative_extension: Duration::from_secs(60),
        redelivery_cap: 3,
        max_wal_bytes: 2 * 1024 * 1024,
    }
}

#[test]
fn operator_wal_core_orphan_recovery_and_human_reconciliation_match_model() {
    let directory = TestDir::new("wal-actuator");
    let queue = DurableQueue::open(directory.path(), 1, wal_config()).unwrap();
    queue.manage_sid("sid-a").unwrap();
    let EnqueueOutcome::Enqueued(event_id) = queue.enqueue(event(3)).unwrap() else {
        panic!("fresh event must enqueue");
    };
    let claim = queue.claim_at(100).unwrap().unwrap();
    let action_hash = "ab".repeat(32);
    let model = operator_wal_actuator_model();
    let initial = model.init_state();

    queue
        .begin_action_at(event_id, &claim.token, "turn", &action_hash, 101)
        .expect("intent is durable before mutation");
    assert!(matches!(
        queue.status(event_id).unwrap().status,
        EventStatus::ActionInFlight { .. }
    ));
    let intent = state_after(&initial, &[("phase", 1), ("intent_durable", 1)]);
    assert_transition(
        &model,
        &initial,
        &intent,
        "PersistIntent",
        "operator actuator durable intent",
    );

    // Simulate process loss after intent but before the actuator is known to have
    // run. This is only the durable queue's recovery binding; the genuine
    // `MutateOnce` binding lives beside the shipping GUI transaction helper.
    drop(queue);
    let (reopened, report) =
        DurableQueue::open_with_report(directory.path(), 2, wal_config()).unwrap();
    assert!(
        report.records_replayed >= 4,
        "epoch/manage/enqueue/claim/intent replayed"
    );
    assert!(matches!(
        reopened.status(event_id).unwrap().status,
        EventStatus::InDoubt { token: Some(_), .. }
    ));
    let in_doubt = state_after(&intent, &[("phase", 3), ("in_doubt", 1)]);
    assert_transition(
        &model,
        &intent,
        &in_doubt,
        "CrashAfterIntent",
        "operator actuator unmatched-intent recovery",
    );

    // The real queue rejects another action attempt. Only an explicitly human,
    // token-scoped reconciliation may make the record terminal.
    assert!(matches!(
        reopened.begin_action_at(event_id, &claim.token, "turn", &action_hash, 200),
        Err(OperatorError::EventInDoubt(found)) if found == event_id
    ));
    assert_eq!(
        reopened
            .reconcile_in_doubt_at(
                event_id,
                &claim.token,
                Resolution::Acted,
                "human verified the external session outcome",
                201,
            )
            .unwrap(),
        AckOutcome::Resolved
    );
    let resolved = state_after(&in_doubt, &[("phase", 4), ("resolved", 1)]);
    assert_transition(
        &model,
        &in_doubt,
        &resolved,
        "ResolveInDoubt",
        "operator human reconciliation of an orphan intent",
    );
    drop(reopened);

    let replayed = DurableQueue::open(directory.path(), 3, wal_config()).unwrap();
    assert!(matches!(
        replayed.status(event_id).unwrap().status,
        EventStatus::Resolved {
            resolution: Resolution::Acted,
            reconciliation_note: Some(ref note),
            ..
        } if note == "human verified the external session outcome"
    ));
}

#[test]
fn operator_fleet_fault_real_gate_and_human_clear_match_model() {
    let directory = TestDir::new("fleet-fault");
    let queue = DurableQueue::open(directory.path(), 1, wal_config()).unwrap();
    queue.manage_sid("sid-a").unwrap();
    queue.manage_sid("sid-b").unwrap();
    let EnqueueOutcome::Enqueued(event_id) = queue.enqueue(event(7)).unwrap() else {
        panic!("fresh event must enqueue");
    };
    let claim = queue.claim_at(100).unwrap().unwrap();
    let action_hash = "ab".repeat(32);
    queue
        .begin_action_at(event_id, &claim.token, "turn", &action_hash, 101)
        .unwrap();

    let model = operator_fleet_fault_model();
    let healthy = model.init_state();
    assert!(matches!(
        queue
            .latch_fault_at(FleetFaultReason::ObserverOverflow, 102)
            .unwrap(),
        FaultLatchOutcome::Latched(_)
    ));
    assert!(matches!(
        queue.fleet_gate().unwrap(),
        FleetGateStatus::Faulted(_)
    ));
    let faulted = state_after(&healthy, &[("phase", 2), ("marker", 1)]);
    assert_transition(
        &model,
        &healthy,
        &faulted,
        "LatchFault",
        "operator durable fleet-fault latch",
    );

    // The real core checks the durable gate before inspecting queue contents.
    assert!(matches!(
        queue.claim_at(103),
        Err(OperatorError::FleetFaulted(
            FleetFaultReason::ObserverOverflow
        ))
    ));
    assert_transition(
        &model,
        &faulted,
        &faulted,
        "AttemptActuateBlocked",
        "operator faulted claim refusal",
    );
    let forged_egress = state_after(&faulted, &[("actions", 1), ("blocked_egress", 1)]);
    assert_rejected(
        &model,
        &faulted,
        &forged_egress,
        "AttemptActuateBlocked",
        "operator faulted actuator egress",
    );

    assert_eq!(
        queue.begin_fault_clear_at(104).unwrap(),
        vec!["sid-a".to_string(), "sid-b".to_string()]
    );
    let reconciling = state_after(&faulted, &[("phase", 3), ("pending", 2), ("in_doubt", 1)]);
    assert_transition(
        &model,
        &faulted,
        &reconciling,
        "BeginClearWithInDoubt",
        "operator fault clear discovers action ambiguity",
    );

    let evidence = "fresh owner-confirmed baseline";
    queue
        .enqueue_rebaseline(NewEvent::new(
            "sid-a",
            EventGeneration::new(2, false, 1, Sha256::digest(evidence.as_bytes()).into()),
            AttentionCondition::Changed,
            evidence,
        ))
        .unwrap();
    let one_pending = state_after(&reconciling, &[("pending", 1)]);
    assert_transition(
        &model,
        &reconciling,
        &one_pending,
        "BaselineOne",
        "operator fault clear fresh baseline",
    );
    queue.unmanage_sid_at("sid-b", 105).unwrap();
    let roster_reconciled = state_after(&one_pending, &[("pending", 0)]);
    assert_transition(
        &model,
        &one_pending,
        &roster_reconciled,
        "BaselineOne",
        "operator fault clear explicit unmanage",
    );
    assert!(matches!(
        queue.complete_fault_clear_at(106),
        Err(OperatorError::EventInDoubt(found)) if found == event_id
    ));

    queue
        .reconcile_in_doubt_at(
            event_id,
            &claim.token,
            Resolution::NoAction,
            "owner verified no submission landed",
            107,
        )
        .unwrap();
    let ambiguity_resolved = state_after(&roster_reconciled, &[("in_doubt", 0)]);
    assert_transition(
        &model,
        &roster_reconciled,
        &ambiguity_resolved,
        "HumanReconcile",
        "operator fault clear human ambiguity reconciliation",
    );
    queue.complete_fault_clear_at(108).unwrap();
    assert_eq!(queue.fleet_gate().unwrap(), FleetGateStatus::Healthy);
    assert_transition(
        &model,
        &ambiguity_resolved,
        &healthy,
        "CompleteClear",
        "operator fault clear durable completion",
    );
}

#[test]
fn operator_leadership_real_lock_and_epoch_fence_match_model() {
    let directory = TestDir::new("leadership");
    let model = operator_leadership_model();
    let initial = model.init_state();

    let leader_a = DurableQueue::open(directory.path(), 1, wal_config()).unwrap();
    assert_eq!(leader_a.durable_epoch().unwrap(), 1);
    let a_live = state_after(&initial, &[("a_live", 1), ("epoch", 1), ("a_epoch", 1)]);
    assert_transition(
        &model,
        &initial,
        &a_live,
        "StartA",
        "operator leadership first acquisition",
    );

    assert!(leader_a.manage_sid("leader-a-write").unwrap());
    let a_wrote = state_after(&a_live, &[("accepted_epoch", 1)]);
    assert_transition(
        &model,
        &a_live,
        &a_wrote,
        "CurrentWriteA",
        "operator leadership current A write",
    );

    // A failed liveness opinion cannot steal a kernel-held lock. The failed open
    // leaves the real leader and its durable epoch unchanged.
    assert!(matches!(
        DurableQueue::open(directory.path(), 2, wal_config()),
        Err(OperatorError::LockContended(_))
    ));
    assert_eq!(leader_a.durable_epoch().unwrap(), 1);
    assert_transition(
        &model,
        &a_wrote,
        &a_wrote,
        "AttemptTakeoverWhileLive",
        "operator leadership live-lock takeover refusal",
    );

    drop(leader_a);
    let a_lost = state_after(
        &a_wrote,
        &[("a_live", 0), ("stale_epoch", 1), ("accepted_epoch", 0)],
    );
    assert_transition(
        &model,
        &a_wrote,
        &a_lost,
        "LoseA",
        "operator leadership A process exit",
    );

    let (leader_b, report) = DurableQueue::open_next_epoch(directory.path(), wal_config()).unwrap();
    assert_eq!(report.durable_epoch, 2);
    let b_live = state_after(&a_lost, &[("b_live", 1), ("epoch", 2), ("b_epoch", 2)]);
    assert_transition(
        &model,
        &a_lost,
        &b_live,
        "TakeoverB",
        "operator leadership post-release takeover",
    );

    assert!(leader_b.manage_sid("leader-b-write").unwrap());
    let b_wrote = state_after(&b_live, &[("accepted_epoch", 2)]);
    assert_transition(
        &model,
        &b_live,
        &b_wrote,
        "CurrentWriteB",
        "operator leadership current B write",
    );

    // An old-epoch process cannot re-enter while B owns the lock. The model's
    // stale-write action is consequently a no-op under the healthy constant.
    assert!(matches!(
        DurableQueue::open(directory.path(), 1, wal_config()),
        Err(OperatorError::LockContended(_))
    ));
    assert_transition(
        &model,
        &b_wrote,
        &b_wrote,
        "StaleWriteAfterTakeover",
        "operator leadership stale writer refusal",
    );

    // NEGATIVE CONTROL: accepting A's old epoch after takeover is rejected.
    let forged_stale_write = state_after(&b_wrote, &[("accepted_epoch", 1), ("stale_accepted", 1)]);
    assert_rejected(
        &model,
        &b_wrote,
        &forged_stale_write,
        "StaleWriteAfterTakeover",
        "operator leadership stale epoch acceptance",
    );

    drop(leader_b);
    assert!(matches!(
        DurableQueue::open(directory.path(), 1, wal_config()),
        Err(OperatorError::EpochRegression {
            requested: 1,
            durable: 2
        })
    ));
}
