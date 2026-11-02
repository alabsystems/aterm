// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Writer-owned scheduling refreshes during actual replay and fallback.

use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::qos::{Role, RoleObservation};

#[test]
fn a_lost_temporary_queue_publishes_shared_failure_before_its_refund() {
    let demands = urgency::Demands::new();
    let (tx, rx) = queue_in_with_demands(
        Arc::default(),
        1,
        usize::MAX,
        RewindPool::new(usize::MAX),
        demands,
    );
    assert!(tx.send(TemporalMsg::Reply));
    let mut queued = rx.rx.try_recv().expect("temporary work");
    let shared_gone = AtomicBool::new(false);
    queued.completion.begin_processing();
    queued.completion.abandon(Some(&shared_gone));
    assert!(shared_gone.load(Ordering::Acquire));
    assert_eq!(tx.state.completion_snapshot(), (0, 0, true));
    drop(queued);
    drop(rx);
    drop(tx);
    assert!(
        shared_gone.load(Ordering::Acquire),
        "failure outlives the lost queue"
    );
}

#[test]
fn overlapping_reset_and_reader_queues_remain_visible_after_temporary_queue_drops() {
    let recorder = Arc::new(Mutex::new(TemporalRecorder::new()));
    let (sender, receiver) = std::sync::mpsc::channel();
    let demands = urgency::Demands::new();
    let pool = RewindPool::new(usize::MAX);
    let open = || {
        open_to(
            &recorder,
            sender.clone(),
            Arc::new(QueueState::with_demands(pool.clone(), demands.clone())),
            100,
        )
    };
    let reader = open();
    let reset = open();
    for (queue, bytes) in [(&reader, 5), (&reset, 7)] {
        assert!(queue.send(TemporalMsg::RawIn(InputSlice::at(
            &vec![b'x'; bytes],
            ClockReading::now(),
        ))));
    }
    let status = || recorder.lock().unwrap().writer_status();
    assert!(status().contains("writer_in_flight=2 writer_queued_bytes=12"));
    let reader_pending = receiver.try_recv().expect("reader input");
    let mut reset_pending = receiver.try_recv().expect("temporary reset input");
    reset_pending.completion.written();
    drop(reset_pending);
    drop(reset);
    assert!(status().contains("writer_queue=present writer_in_flight=1 writer_queued_bytes=5"));
    drop(reader_pending);
    assert!(status().contains("writer_queue=present writer_in_flight=0 writer_queued_bytes=0"));
    drop(reader);
    assert!(status().contains("writer_queue=absent"));
    let next = open();
    assert_eq!(
        recorder.lock().unwrap().queues.len(),
        1,
        "expired registrations are pruned"
    );
    drop(next);
}

#[test]
fn a_demand_arriving_inside_replay_is_seen_at_subsequent_step_boundaries() {
    for known in [true, false] {
        let demands = urgency::Demands::new();
        let held = Rc::new(RefCell::new(None));
        let _hook = replay_hook::install({
            let demands = demands.clone();
            let held = held.clone();
            move || {
                *held.borrow_mut() = Some(demands.acquire(urgency::Reason::View));
            }
        });
        let mut live = Terminal::new(6, 40);
        let mut recorder = TemporalRecorder::new();
        recorder.record_keyframe(live.checkpoint());
        for text in [b"one\r\n".as_slice(), b"two\r\n", b"three\r\n"] {
            let clock = ClockReading::now();
            live.process_at(text, clock);
            let mut input = InputSlice::at(text, clock);
            input.parser_ground = known.then_some(true);
            assert!(recorder.apply(TemporalMsg::RawIn(input)).is_none());
        }
        let plan = recorder.replay_plan(None).expect("recorded plan");
        let mut writer = urgency::WriterRole::new(demands.clone());
        let mut requested = Vec::new();
        let mut observations = Vec::new();
        let folded = plan.fold_for_rekey_with_poll(&mut || {
            writer.poll_with(true, |role| {
                requested.push(role);
                RoleObservation {
                    requested: role,
                    observed: Some(role),
                    class: None,
                    set_errno: Some(0),
                    read_errno: Some(0),
                }
            });
            observations.push(demands.snapshot().observation.unwrap().requested);
        });
        assert_eq!(folded.term.checkpoint_visible(), live.checkpoint_visible());
        assert_eq!(requested, [Role::Background, Role::Responsive]);
        assert!(
            observations.len() >= 5,
            "each replayed step refreshes the role"
        );
        assert_eq!(observations.last(), Some(&Role::Responsive));
        drop(held.borrow_mut().take());
        assert!(!demands.is_urgent());
    }
}
