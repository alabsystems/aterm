// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Independent lifecycle and scheduler-observation controls for writer urgency.

use super::urgency::{Demands, Reason, WriterRole};
use crate::qos::{Role, RoleObservation};
use std::sync::Arc;
use std::sync::atomic::Ordering;

fn observed(role: Role) -> RoleObservation {
    RoleObservation {
        requested: role,
        observed: Some(role),
        class: None,
        set_errno: Some(0),
        read_errno: Some(0),
    }
}

#[test]
fn independent_demands_keep_every_reason_until_its_last_owner_leaves() {
    let demands = Demands::new();
    let drain = demands.acquire(Reason::Drain);
    let first_view = demands.acquire(Reason::View);
    let second_view = demands.acquire(Reason::View);
    let pressure = demands.acquire(Reason::Pressure);
    assert_eq!(demands.snapshot().counts, [1, 2, 1]);

    drop(first_view);
    assert_eq!(demands.snapshot().counts, [1, 1, 1]);
    drop(drain);
    drop(pressure);
    assert!(
        demands.is_urgent(),
        "the second view still needs the writer"
    );
    assert_eq!(demands.snapshot().counts, [0, 1, 0]);
    drop(second_view);
    assert!(!demands.is_urgent());
    assert_eq!(demands.snapshot().counts, [0, 0, 0]);
}

/// The writer cannot promote itself until it runs. A producer must donate priority while
/// the writer is still waiting, and retain that donation through overlapping demands.
#[cfg(target_os = "macos")]
#[test]
fn a_demand_donates_priority_before_the_writer_runs_again() {
    let (ready, started) = std::sync::mpsc::channel();
    let (release, held) = std::sync::mpsc::channel();
    let worker = Arc::new(std::thread::spawn(move || {
        crate::qos::set_self(Role::Background);
        ready.send(()).unwrap();
        held.recv_timeout(std::time::Duration::from_secs(60))
            .unwrap();
    }));
    started
        .recv_timeout(std::time::Duration::from_secs(60))
        .unwrap();
    let demands = Demands::new();
    let pressure = demands.acquire(Reason::Pressure);
    assert_eq!(demands.snapshot().priority_override, None);
    demands.adopt_worker(worker.clone());
    assert_eq!(demands.snapshot().priority_override, Some(true));
    let view = demands.acquire(Reason::View);
    drop(pressure);
    assert_eq!(demands.snapshot().priority_override, Some(true));
    drop(view);
    assert_eq!(demands.snapshot().priority_override, Some(false));
    let drain = demands.acquire(Reason::Drain);
    assert_eq!(demands.snapshot().priority_override, Some(true));
    drop(drain);
    assert_eq!(demands.snapshot().priority_override, Some(false));
    drop(demands);
    release.send(()).unwrap();
    Arc::try_unwrap(worker).unwrap().join().unwrap();
}

#[test]
fn writer_changes_role_only_when_the_combined_demand_changes() {
    let demands = Demands::new();
    let mut writer = WriterRole::new(demands.clone());
    let mut applied = Vec::new();
    let mut poll = |writer: &mut WriterRole| {
        writer.poll_with(true, |role| {
            applied.push(role);
            observed(role)
        });
    };
    poll(&mut writer);
    poll(&mut writer);
    let view = demands.acquire(Reason::View);
    poll(&mut writer);
    let drain = demands.acquire(Reason::Drain);
    poll(&mut writer);
    drop(view);
    poll(&mut writer);
    drop(drain);
    poll(&mut writer);
    poll(&mut writer);
    assert_eq!(
        applied,
        [Role::Background, Role::Responsive, Role::Background]
    );
    assert_eq!(
        demands.snapshot().observation.unwrap().observed,
        Some(Role::Background)
    );
}

#[test]
fn non_dynamic_platform_preserves_the_existing_responsive_role() {
    let demands = Demands::new();
    let mut writer = WriterRole::new(demands.clone());
    let mut applied = Vec::new();
    writer.poll_with(false, |role| {
        applied.push(role);
        observed(role)
    });
    let view = demands.acquire(Reason::View);
    writer.poll_with(false, |role| {
        applied.push(role);
        observed(role)
    });
    drop(view);
    writer.poll_with(false, |role| {
        applied.push(role);
        observed(role)
    });
    assert_eq!(applied, [Role::Responsive]);
}

#[test]
fn failed_role_request_is_reported_as_observed_not_as_applied() {
    let demands = Demands::new();
    let mut writer = WriterRole::new(demands.clone());
    let mut calls = 0;
    writer.poll_with(true, |requested| {
        calls += 1;
        RoleObservation {
            requested,
            observed: Some(Role::Responsive),
            class: Some(25),
            set_errno: Some(1),
            read_errno: Some(0),
        }
    });
    writer.poll_with(true, |_| {
        panic!("unchanged demand must not spin on a failed setter")
    });
    assert_eq!(calls, 1);
    let snapshot = demands.snapshot();
    let reading = snapshot.observation.unwrap();
    assert_eq!(reading.requested, Role::Background);
    assert_eq!(reading.observed, Some(Role::Responsive));
    assert_eq!(reading.set_errno, Some(1));
    let fields = snapshot.fields();
    assert!(fields.contains("writer_role_requested=utility"));
    assert!(fields.contains("writer_qos_observed=responsive"));
    assert!(fields.contains("writer_qos_set_errno=1"));

    let _view = demands.acquire(Reason::View);
    writer.poll_with(true, |role| {
        calls += 1;
        observed(role)
    });
    assert_eq!(calls, 2, "a changed request gets a fresh observation");
}

#[test]
fn unwinding_a_consumer_releases_only_its_own_demand() {
    let demands = Demands::new();
    let view = demands.acquire(Reason::View);
    let result = std::panic::catch_unwind(|| {
        let _drain = demands.acquire(Reason::Drain);
        assert_eq!(demands.snapshot().counts, [1, 1, 0]);
        panic!("simulated consumer failure");
    });
    assert!(result.is_err());
    assert_eq!(demands.snapshot().counts, [0, 1, 0]);
    drop(view);
    assert!(!demands.is_urgent());
}

#[test]
fn demand_handoff_during_the_role_read_never_looks_idle() {
    let demands = Demands::new();
    let mut view = Some(demands.acquire(Reason::View));
    let mut drain = None;
    assert!(demands.is_urgent_with(|| {
        drain = Some(demands.acquire(Reason::Drain));
        drop(view.take());
    }));
    assert_eq!(demands.snapshot().counts, [1, 0, 0]);
    drop(drain);
    assert!(!demands.is_urgent());
}

fn raw(bytes: usize) -> super::TemporalMsg {
    super::TemporalMsg::RawIn(super::InputSlice::at(
        &vec![b'x'; bytes],
        super::ClockReading::now(),
    ))
}

#[test]
fn sustained_message_pressure_keeps_later_queued_work_urgent() {
    let demands = Demands::new();
    let (tx, rx) = super::queue_in_with_demands(
        Arc::default(),
        4,
        usize::MAX,
        super::RewindPool::new(usize::MAX),
        demands.clone(),
    );
    for _ in 0..2 {
        assert!(tx.send(super::TemporalMsg::Reply));
        assert!(!demands.is_urgent());
    }
    assert!(tx.send(super::TemporalMsg::Reply));
    assert_eq!(demands.snapshot().counts, [0, 0, 1]);
    assert!(tx.send(super::TemporalMsg::Reply));
    assert_eq!(demands.snapshot().counts, [0, 0, 2]);
    assert!(!tx.send(super::TemporalMsg::Reply), "the cap is unchanged");

    assert!(rx.try_recv().is_some());
    assert!(tx.send(super::TemporalMsg::Reply));
    assert_eq!(demands.snapshot().counts, [0, 0, 3]);
    for _ in 0..3 {
        assert!(rx.try_recv().is_some());
        assert!(demands.is_urgent(), "the last pressure message still waits");
    }
    assert!(rx.try_recv().is_some());
    assert!(!demands.is_urgent());
    assert!(tx.drain().is_finished());
}

#[test]
fn byte_pressure_and_refund_follow_payloads_not_only_message_count() {
    let demands = Demands::new();
    let (tx, rx) = super::queue_in_with_demands(
        Arc::default(),
        usize::MAX,
        8,
        super::RewindPool::new(usize::MAX),
        demands.clone(),
    );
    assert!(tx.send(raw(4)));
    assert!(!demands.is_urgent());
    assert!(tx.send(raw(2)));
    assert_eq!(demands.snapshot().counts, [0, 0, 1]);
    assert!(tx.send(raw(2)));
    assert_eq!(demands.snapshot().counts, [0, 0, 2]);
    assert!(!tx.send(raw(1)), "the byte admission cap is unchanged");
    assert_eq!(tx.state.bytes.load(Ordering::Acquire), 8);
    drop(rx);
    assert_eq!(tx.state.bytes.load(Ordering::Acquire), 0);
    assert_eq!(tx.state.in_flight.load(Ordering::Acquire), 0);
    assert_eq!(demands.snapshot().counts, [0, 0, 0]);
}

#[test]
fn shared_pool_pressure_is_visible_to_the_other_queue() {
    let demands = Demands::new();
    let pool = super::RewindPool::new(4 * super::MIN_QUEUE_BYTES);
    let (first, first_rx) = super::queue_in_with_demands(
        Arc::default(),
        usize::MAX,
        usize::MAX,
        pool.clone(),
        demands.clone(),
    );
    let (second, second_rx) = super::queue_in_with_demands(
        Arc::default(),
        usize::MAX,
        usize::MAX,
        pool.clone(),
        demands.clone(),
    );
    assert!(first.send(raw(super::MIN_QUEUE_BYTES / 2)));
    assert!(!demands.is_urgent());
    assert!(second.send(raw(super::MIN_QUEUE_BYTES / 4)));
    assert_eq!(demands.snapshot().counts, [0, 0, 1]);
    let mut writer = WriterRole::new(demands.clone());
    writer.poll_with(true, |role| {
        assert_eq!(role, Role::Responsive);
        observed(role)
    });
    assert!(first_rx.try_recv().is_some());
    assert!(
        demands.is_urgent(),
        "the second queue still owns its pressure"
    );
    assert!(second_rx.try_recv().is_some());
    assert_eq!(pool.queued(), 0);
    assert!(!demands.is_urgent());
}

#[test]
fn a_pending_drain_keeps_urgency_after_its_stack_wait_ends() {
    let demands = Demands::new();
    let (tx, rx) = super::queue_in_with_demands(
        Arc::default(),
        100,
        usize::MAX,
        super::RewindPool::new(usize::MAX),
        demands.clone(),
    );
    assert!(tx.send(super::TemporalMsg::Reply));
    let drain = tx.drain();
    let waiting = drain.waiting();
    assert_eq!(demands.snapshot().counts, [2, 0, 0]);
    drop(waiting);
    assert_eq!(demands.snapshot().counts, [1, 0, 0]);
    assert!(!drain.is_finished());
    assert!(rx.try_recv().is_some());
    assert!(drain.is_finished());
    assert!(!demands.is_urgent());
}

#[test]
fn a_stack_wait_remains_urgent_across_a_transient_empty_queue() {
    let demands = Demands::new();
    let (tx, rx) = super::queue_in_with_demands(
        Arc::default(),
        100,
        usize::MAX,
        super::RewindPool::new(usize::MAX),
        demands.clone(),
    );
    assert!(tx.send(super::TemporalMsg::Reply));
    let drain = tx.drain();
    let waiting = drain.waiting();
    assert!(rx.try_recv().is_some());
    assert!(drain.is_finished());
    assert_eq!(demands.snapshot().counts, [1, 0, 0]);
    assert!(tx.send(super::TemporalMsg::Reply));
    assert!(!drain.is_finished());
    assert!(demands.is_urgent());
    assert!(rx.try_recv().is_some());
    drop(waiting);
    assert!(!demands.is_urgent());
}

#[test]
fn writer_finishing_between_drain_check_and_arm_leaves_no_phantom_demand() {
    let demands = Demands::new();
    let state =
        super::QueueState::with_demands(super::RewindPool::new(usize::MAX), demands.clone());
    state.queue(0);
    state.request_drain_with(|| state.written(0));
    assert_eq!(state.in_flight.load(Ordering::Acquire), 0);
    assert_eq!(demands.snapshot().counts, [0, 0, 0]);
    assert!(!state.drain_armed.load(Ordering::Acquire));
}

#[test]
fn disconnect_after_admission_rolls_back_without_marking_processing_abandoned() {
    let demands = Demands::new();
    let (tx, rx) = super::queue_in_with_demands(
        Arc::default(),
        1,
        usize::MAX,
        super::RewindPool::new(usize::MAX),
        demands.clone(),
    );
    drop(rx);
    // Model the receiver disappearing just after admission read its old state.
    // The real mpsc send still takes its normal disconnected-result path.
    tx.state.closed.store(false, Ordering::Release);
    assert!(!tx.send(super::TemporalMsg::Reply));
    assert_eq!(tx.state.in_flight.load(Ordering::Acquire), 0);
    assert_eq!(tx.state.bytes.load(Ordering::Acquire), 0);
    assert_eq!(demands.snapshot().counts, [0, 0, 0]);
    assert!(!tx.state.abandoned.load(Ordering::Acquire));
    assert!(tx.drain().is_finished());
}

#[test]
fn abandoning_processing_never_looks_like_successful_capture() {
    let demands = Demands::new();
    let (tx, rx) = super::queue_in_with_demands(
        Arc::default(),
        1,
        usize::MAX,
        super::RewindPool::new(usize::MAX),
        demands.clone(),
    );
    assert!(tx.send(super::TemporalMsg::Reply));
    let mut queued = Some(rx.rx.try_recv().expect("queued work"));
    queued.as_mut().unwrap().completion.begin_processing();
    let seen = tx.state.completion_snapshot_with(|| drop(queued.take()));
    assert!(seen.0 != 0 || seen.1 != 0 || seen.2);
    assert_eq!(tx.state.completion_snapshot(), (0, 0, true));
    assert!(
        tx.drain().is_finished(),
        "ended work permits lifecycle recovery without claiming successful capture"
    );
    assert_eq!(demands.snapshot().counts, [0, 0, 0]);
}

#[test]
fn a_processing_panic_refunds_ownership_but_retains_the_failure() {
    let demands = Demands::new();
    let (tx, rx) = super::queue_in_with_demands(
        Arc::default(),
        1,
        8,
        super::RewindPool::new(usize::MAX),
        demands.clone(),
    );
    assert!(tx.send(raw(8)));
    let mut queued = rx.rx.try_recv().expect("queued work");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        queued.completion.begin_processing();
        panic!("simulated processing failure");
    }));
    assert!(result.is_err());
    assert_eq!(tx.state.completion_snapshot(), (0, 0, true));
    assert!(
        tx.drain().is_finished(),
        "a failed writer must not strand a completed lifecycle drain"
    );
    assert_eq!(tx.state.pool.queued(), 0);
    assert_eq!(demands.snapshot().counts, [0, 0, 0]);
}

#[cfg(target_os = "macos")]
#[test]
fn native_writer_thread_observes_utility_responsive_utility() {
    std::thread::Builder::new()
        .name("urgency-role-observation".into())
        .spawn(|| {
            let demands = Demands::new();
            let mut writer = WriterRole::new(demands.clone());
            let poll = |writer: &mut WriterRole, expected| {
                writer.poll_with(true, crate::qos::set_self_observed);
                let reading = demands.snapshot().observation.unwrap();
                assert_eq!(reading.requested, expected);
                assert_eq!(reading.observed, Some(expected));
                assert_eq!(reading.set_errno, Some(0));
                assert_eq!(reading.read_errno, Some(0));
                assert!(reading.class.is_some());
            };
            poll(&mut writer, Role::Background);
            let drain = demands.acquire(Reason::Drain);
            poll(&mut writer, Role::Responsive);
            let view = demands.acquire(Reason::View);
            drop(drain);
            poll(&mut writer, Role::Responsive);
            drop(view);
            poll(&mut writer, Role::Background);
        })
        .expect("start isolated native role thread")
        .join()
        .expect("native role thread succeeded");
}
