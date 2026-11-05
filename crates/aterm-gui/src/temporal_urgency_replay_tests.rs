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

/// A View demand that arrives inside the idle writer's judge ([`IdleJudge`]), from the replay that restores the chain it judges, ends the judge at the boundary of the pass under way: that pass's verdicts are posted, the job is parked with the judge still due, and the writer's role then polls Responsive. Once the demand drops, the parked job judges the rest.
#[test]
fn a_demand_arriving_inside_replay_is_seen_at_subsequent_step_boundaries() {
    let demands = urgency::Demands::new();
    let pool = RewindPool::new(usize::MAX);
    let mut live = Terminal::new(24, 80);
    let recorder = Arc::new(Mutex::new(TemporalRecorder::new()));
    recorder
        .lock()
        .unwrap()
        .record_keyframe(keyframe_of(&live, usize::MAX).expect("keyable"));
    pool.join(1, &recorder);
    {
        // A closed chain a judge folds in three passes, two witnesses each.
        let mut rec = recorder.lock().unwrap();
        let slice = format!("{}\r\n", "x".repeat(78)).repeat(51);
        for n in 1..=48 {
            let at = ClockReading::now();
            live.process_at(slice.as_bytes(), at);
            rec.apply(TemporalMsg::RawIn(InputSlice::at(slice.as_bytes(), at)));
            if n % 8 == 0 {
                let seen = live.checkpoint_visible().expect("visible");
                rec.apply(TemporalMsg::Witness(Arc::new(seen)));
            }
        }
        rec.record_keyframe(keyframe_of(&live, usize::MAX).expect("keyable"));
        assert_eq!(rec.pending(), 6, "fixture: every witness pending");
    }
    let mut writer = urgency::WriterRole::new(demands.clone());
    let mut requested = Vec::new();
    let mut apply = |role| {
        requested.push(role);
        RoleObservation {
            requested: role,
            observed: Some(role),
            class: None,
            set_errno: Some(0),
            read_errno: Some(0),
        }
    };
    writer.poll_with(true, &mut apply);
    let held = Rc::new(RefCell::new(None));
    let _hook = replay_hook::install({
        let demands = demands.clone();
        let held = held.clone();
        move || {
            *held.borrow_mut() = Some(demands.acquire(urgency::Reason::View));
        }
    });
    let mut judge = IdleJudge::default();
    let mut boundaries = 0;
    judge.run(&pool, || {
        boundaries += 1;
        demands.is_urgent()
    });
    assert_eq!(
        boundaries, 2,
        "the judge looked for the demand before its first pass and stopped at the next boundary"
    );
    assert_eq!(
        recorder.lock().unwrap().witnessed(),
        2,
        "the pass under way posted its verdicts"
    );
    assert!(judge.job.is_some(), "the job is parked");
    assert!(pool.judge_due(), "the judge stays due");
    writer.poll_with(true, &mut apply);
    assert_eq!(requested, [Role::Background, Role::Responsive]);
    assert_eq!(
        demands.snapshot().observation.map(|o| o.requested),
        Some(Role::Responsive)
    );
    drop(held.borrow_mut().take());
    assert!(!demands.is_urgent());
    judge.run(&pool, || demands.is_urgent());
    let rec = recorder.lock().unwrap();
    assert_eq!(
        (rec.pending(), rec.witnessed(), rec.diverged()),
        (0, 6, 0),
        "the parked job judged the rest"
    );
}
