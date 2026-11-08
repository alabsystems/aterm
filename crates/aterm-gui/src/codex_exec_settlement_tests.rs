// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_agent::harness::{accounts, codex_renewal as renewal};
use aterm_types::update_boundary::{Boundary, GuardedItem};
use std::sync::atomic::{AtomicUsize, Ordering};

#[path = "codex_exec_settlement_model_tests.rs"]
mod model;

static CUSTODY_BOUNDARY: Boundary = Boundary::new();
static NEXT_SESSION: AtomicUsize = AtomicUsize::new(900_000);

struct Fixture {
    _tmp: aterm_tempfile::TempDir,
    _handle: crate::session_store::SessionHandle,
    env: Env,
    receipt: renewal::Receipt,
    execution: Execution,
    settlement: Settlement,
    drain: crate::session_timeline::NativeDrain,
    boundary: Boundary,
}

impl Fixture {
    fn new(attempted: bool, granted: bool) -> Self {
        let fixture = Self::unarmed(attempted, granted);
        assert!(fixture.execution.lock().activate());
        fixture
    }

    fn unarmed(attempted: bool, granted: bool) -> Self {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let mut env = Env::from_process().unwrap();
        env.home = Some(tmp.path().join("home"));
        env.state = tmp.path().join("state");
        env.config = Some(tmp.path().join("aterm.toml"));
        let receipt = renewal::Receipt {
            id: "ab".repeat(16),
            intent: renewal::Intent {
                purpose: renewal::Purpose::WeeklyQuota,
                thread: "019a4b54-5fa1-7123-8123-123456789abc".into(),
                wall: "019a4b54-5fa2-7123-8123-123456789abc".into(),
                identity: renewal::Identity::from_native("source", "user"),
            },
            destination: Some(renewal::Identity::from_native("destination", "user")),
            sid: "s-fixture".into(),
            old_pid: 10,
            old_birth: 11,
            claim_revision: 3,
            human_seq: 4,
            old_auth: "/fixture/old/auth.json".into(),
            new_auth: "/fixture/new/auth.json".into(),
            generation: 2,
            row_stamp: "selected-row".into(),
            native_prepare: "fixture-native-binding".into(),
            expires_ms: 120_000,
            phase: if attempted {
                renewal::Phase::Committing
            } else {
                renewal::Phase::Ready
            },
            successor_pid: None,
            successor_birth: None,
            accepted_turn: None,
            launch_attempted: attempted,
            candidate: None,
            current_attempt: None,
            revoked: false,
        };
        renewal::with_receipt_writer(&env, |_| renewal::write_receipt(&env, &receipt))
            .unwrap()
            .unwrap();
        let handle = crate::session_store::test_handle_on(
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed) as u64,
            -1,
        );
        let authority = handle.ctx.meta.lock().unwrap().supervisor_authority(0);
        let custody = Arc::new(
            handle
                .ctx
                .fenced_writes
                .reserve_native(
                    3,
                    &receipt.id,
                    CUSTODY_BOUNDARY
                        .try_hold(GuardedItem::HarnessNativeMutation)
                        .unwrap(),
                )
                .unwrap(),
        );
        let drain = custody.drain();
        let mut tx = Transaction::new(1);
        assert!(tx.reserve(1));
        if granted {
            assert!(tx.execute(1));
        }
        let (execution, settlement) = Execution::new(
            env.clone(),
            "destination".into(),
            Initial {
                receipt: receipt.clone(),
                authority,
                source_connection: 5,
                human_seq: 4,
                deadline: u64::MAX,
                tx,
                custody,
            },
        );
        Self {
            _tmp: tmp,
            _handle: handle,
            env,
            receipt,
            execution,
            settlement,
            drain,
            boundary: Boundary::new(),
        }
    }

    fn poll(&self, gone: bool, now: Instant) -> Poll {
        self.settlement
            .poll_using(None, gone, false, now, 100, &self.boundary)
    }

    fn consumed(&self) -> renewal::Receipt {
        let mut consumed = self.receipt.clone();
        consumed.phase = renewal::Phase::Consumed;
        consumed.accepted_turn = Some(consumed.intent.wall.clone());
        consumed.successor_pid = Some(10);
        consumed.successor_birth = Some(11);
        renewal::with_receipt_writer(&self.env, |_| renewal::write_receipt(&self.env, &consumed))
            .unwrap()
            .unwrap();
        consumed
    }

    fn reserve_record(&self) {
        let binding = codex_rotation::receipt_binding(&self.receipt).unwrap();
        self.env
            .accounts_store_for(accounts::Cli::Codex)
            .unwrap()
            .transact_logins(100, &mut None, |_, logins| {
                logins
                    .native_rotations
                    .insert(self.receipt.id.clone(), binding);
                Ok(Vec::new())
            })
            .unwrap();
    }

    fn record_retained(&self) -> bool {
        self.env
            .accounts_store_for(accounts::Cli::Codex)
            .unwrap()
            .load_logins(100)
            .unwrap()
            .0
            .native_rotations
            .contains_key(&self.receipt.id)
    }
}

#[test]
fn settlement_retries_each_failed_barrier_after_owner_and_source_loss() {
    for barrier in 0..4 {
        let fixture = Fixture::new(true, true);
        let now = Instant::now();
        let failure = aterm_durable::seam::FailingDirs::after(barrier, aterm_durable::seam::EIO);
        let first = fixture.poll(true, now);
        drop(failure);
        assert!(!first.done);
        assert!(
            fixture.drain.is_drained(),
            "source closure drains only native custody"
        );
        {
            let state = fixture.execution.lock();
            assert_eq!(state.tx.stage(), Stage::Cancelled);
            assert!(!state.pending.as_ref().unwrap().acknowledged);
            assert_eq!(
                state.receipt, fixture.receipt,
                "failed writes are not acknowledgments"
            );
        }
        assert!(fixture.poll(true, now + Duration::from_secs(31)).done);
        let retired = renewal::read_receipt(&fixture.env, &fixture.receipt.id).unwrap();
        assert_eq!(retired.phase, renewal::Phase::Retired);
        assert!(retired.revoked && retired.launch_attempted);
        assert!(!fixture.execution.lock().tx.execute(1));
    }
}

#[test]
fn settlement_busy_store_keeps_original_environment_and_first_cause() {
    let fixture = Fixture::new(true, true);
    let now = Instant::now();
    let store = fixture
        .env
        .accounts_store_for(accounts::Cli::Codex)
        .unwrap();
    let lock = store.acquire_bounded().unwrap().unwrap();
    assert!(!fixture.poll(false, now).done);
    let original = fixture.execution.lock().environment(999);
    assert_eq!(original.state, fixture.env.state);
    assert_eq!(original.home, fixture.env.home);
    assert_eq!(original.config, fixture.env.config);
    assert_eq!(original.now, 999);
    fixture
        .execution
        .lock()
        .close(renewal::CancellationKind::NewPersonTask, 99, false, now);
    drop(lock);
    assert!(fixture.poll(true, now + Duration::from_secs(31)).done);
    let cancellation = renewal::read_cancellation(&fixture.env, &fixture.receipt)
        .unwrap()
        .unwrap();
    assert_eq!(cancellation.reason, renewal::CancellationKind::OwnerStop);
    assert_eq!(cancellation.human_seq, 4);
    assert_eq!(cancellation.reached_seq, 4);
}

#[test]
fn settlement_consumed_history_does_not_transfer_live_native_claim() {
    let fixture = Fixture::new(true, true);
    assert!(fixture.execution.lock().tx.load(2));
    let consumed = fixture.consumed();
    assert!(fixture.poll(true, Instant::now()).done);
    assert_eq!(
        renewal::read_receipt(&fixture.env, &fixture.receipt.id),
        Some(consumed)
    );
    assert_eq!(fixture.execution.lock().tx.stage(), Stage::Cancelled);
    assert!(!fixture.execution.lock().tx.consume(2));
    assert!(
        renewal::read_cancellation(&fixture.env, &fixture.receipt)
            .unwrap()
            .is_none()
    );
}

#[test]
fn settlement_prospective_attempt_is_not_go_or_reclaimable_history() {
    let fixture = Fixture::new(true, false);
    let now = Instant::now();
    let result = fixture.poll(false, now);
    assert!(!result.done);
    assert!(!result.terminate);
    assert!(
        !fixture.drain.is_drained(),
        "a checkpoint writer is still alive"
    );
    let retired = fixture.execution.lock().receipt.clone();
    assert!(retired.launch_attempted);
    assert!(renewal::retire_pool_record(&fixture.env, &retired).is_err());
    assert!(!fixture.execution.lock().tx.execute(1));
    assert!(fixture.poll(true, now).done);
}

#[test]
fn settlement_explicit_stop_finishes_only_after_source_closure() {
    let fixture = Fixture::new(false, false);
    let now = Instant::now();
    fixture
        .execution
        .lock()
        .close(renewal::CancellationKind::OwnerStop, 4, true, now);
    let result = fixture.poll(false, now);
    assert!(!result.done && result.terminate);
    assert!(fixture.poll(true, now).done);
}

#[test]
fn settlement_update_handoff_defers_metadata_without_losing_obligation() {
    let fixture = Fixture::new(true, true);
    let now = Instant::now();
    let latch = fixture.boundary.latch_handoff(Duration::from_secs(60));
    assert!(!fixture.poll(true, now).done);
    assert_eq!(
        renewal::read_receipt(&fixture.env, &fixture.receipt.id),
        Some(fixture.receipt.clone())
    );
    assert!(fixture.drain.is_drained());
    drop(latch);
    assert!(fixture.poll(true, now + Duration::from_secs(31)).done);
}

#[test]
fn settlement_live_consumption_needs_no_cancellation() {
    let fixture = Fixture::new(true, true);
    let consumed = fixture.consumed();
    {
        let mut state = fixture.execution.lock();
        assert!(state.tx.load(2));
        assert!(state.tx.consume(2));
        state.receipt = consumed.clone();
        state.custody.take();
    }
    assert!(fixture.poll(false, Instant::now()).done);
    assert!(fixture.execution.lock().pending.is_none());
    assert_eq!(
        renewal::read_receipt(&fixture.env, &fixture.receipt.id),
        Some(consumed)
    );
}

#[test]
fn settlement_real_go_preserves_rights_even_if_receipt_lost_its_marker() {
    let fixture = Fixture::new(false, true);
    fixture.reserve_record();
    assert!(fixture.poll(true, Instant::now()).done);
    assert!(fixture.execution.lock().receipt.launch_attempted);
    assert!(fixture.record_retained());
}

#[test]
fn settlement_unattempted_record_releases_after_each_failed_barrier() {
    let mut failures = 0;
    for barrier in 0..64 {
        let fixture = Fixture::new(false, false);
        fixture.reserve_record();
        let now = Instant::now();
        let failure = aterm_durable::seam::FailingDirs::after(barrier, aterm_durable::seam::EIO);
        let first = fixture.poll(true, now);
        drop(failure);
        if !first.done {
            failures += 1;
            assert!(fixture.poll(true, now + Duration::from_secs(31)).done);
        }
        assert!(!fixture.record_retained());
        if first.done {
            assert!(
                failures > 4,
                "exercise release barriers beyond cancellation's four"
            );
            return;
        }
    }
    panic!("unattempted settlement exceeded the bounded barrier sweep");
}

#[test]
fn settlement_snapshot_drops_the_gate_before_waiting_for_storage() {
    let fixture = Fixture::new(true, true);
    let store = fixture
        .env
        .accounts_store_for(accounts::Cli::Codex)
        .unwrap();
    let held = store.acquire_bounded().unwrap().unwrap();
    let now = Instant::now();
    let attempt = fixture
        .settlement
        .prepare(None, false, false, now, 100)
        .unwrap();
    let mut state = fixture
        .execution
        .0
        .try_lock()
        .expect("snapshot cannot retain the gate");
    assert_eq!(state.tx.stage(), Stage::Cancelled);
    state.close(renewal::CancellationKind::OwnerStop, 4, true, now);
    drop(state);
    drop(attempt);
    drop(held);
    assert!(fixture.poll(true, now).done);
}

#[test]
fn settlement_admission_stays_inactive_until_activated_and_cannot_reopen() {
    let fixture = Fixture::unarmed(false, false);
    let mut state = fixture.execution.lock();
    assert!(!state.active);
    assert!(state.activate());
    assert!(state.active);
    state.close(
        renewal::CancellationKind::ChangedRecord,
        4,
        false,
        Instant::now(),
    );
    assert!(!state.active);
    assert!(!state.activate());
}
