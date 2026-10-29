// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The retired ring, prepare on a clone of a paused boundary, and the registry's timeline.

use aterm_aos::contract::Verdict;
use aterm_aos::effects::NoEffects;
use aterm_aos::publish::{PublishRefusal, RETIRED_CAP, Retire, Retired};
use aterm_aos::registry::{Judged, Listing, Registry, Row, TaskKind, Timeline};
use aterm_aos::replay::replay;
use aterm_aos::task::{Code, state_digest};

use crate::toy::{
    ToyCode, ToyInput, ToySchema, ToyState, Variant, adds, attached, boundary_over, code, feed,
    identity,
};

/// Replays a retired recording from its checkpoint under the code that ran it.
fn replays_to_its_end(r: &Retired<ToySchema, ToyCode>) -> bool {
    let schema = ToySchema::new();
    let start = r
        .checkpoint
        .restore(&schema, &r.checkpoint.provenance().identity)
        .expect("a retired checkpoint restores under its own identity");
    let run = replay(
        &schema,
        &r.code,
        start,
        r.log.inputs(),
        r.log.header().first_cursor,
        &mut NoEffects,
    );
    run.refused.is_none() && run.state_digest == r.end_state
}

#[test]
fn each_full_log_reanchor_retires_the_recording_it_replaced_with_its_code_and_end_state() {
    let mut b = boundary_over(Variant::Good, ToyState::default(), 4);
    b.attach(3, 2).unwrap();
    let mut anchors = vec![b.checkpoint().unwrap().clone()];
    for n in 1..=7 {
        feed(&mut b, &adds(&[n]));
        if b.checkpoint().unwrap() != anchors.last().unwrap() {
            anchors.push(b.checkpoint().unwrap().clone());
        }
    }
    assert_eq!(b.stats().reanchors, 3);
    assert_eq!((b.stats().retired, b.retired_len()), (3, 3));
    let retired = b.drain_retired();
    assert_eq!(b.retired_len(), 0, "a drain takes every retired recording");
    for (k, r) in retired.iter().enumerate() {
        assert_eq!(r.cause, Retire::Full, "{k}");
        assert_eq!(r.code.id(), code(Variant::Good).id(), "{k}");
        assert_eq!(&r.checkpoint, &anchors[k], "{k}: oldest first");
        assert_eq!(r.log.len(), 2, "{k}: a full log");
        assert_eq!(
            r.end_state,
            anchors[k + 1].state_digest(),
            "{k}: it ends in the state the next checkpoint starts from"
        );
        assert!(replays_to_its_end(r), "{k}: its replay reaches its end");
    }
    feed(&mut b, &adds(&[8]));
    assert_eq!(
        b.retired_len(),
        0,
        "negative control: an admit with room retires nothing"
    );
}

#[test]
fn the_ring_keeps_the_newest_retired_recordings_and_counts_the_ones_it_dropped() {
    let mut b = boundary_over(Variant::Good, ToyState::default(), 4);
    b.attach(1, 1).unwrap();
    let extra = 3u64;
    let reanchors = RETIRED_CAP as u64 + extra;
    let inputs: Vec<i64> = (1..=reanchors as i64 + 1).collect();
    feed(&mut b, &adds(&inputs));
    assert_eq!(b.stats().reanchors, reanchors);
    assert_eq!(b.stats().retired, reanchors);
    assert_eq!(b.stats().retired_dropped, extra);
    let kept = b.drain_retired();
    assert_eq!(kept.len(), RETIRED_CAP);
    let epochs: Vec<u64> = kept.iter().map(|r| r.log.header().log_epoch).collect();
    let first = 1 + extra;
    assert_eq!(
        epochs,
        (first..first + RETIRED_CAP as u64).collect::<Vec<_>>(),
        "the oldest went first"
    );
}

#[test]
fn a_broken_log_is_retired_as_broken_when_the_next_input_reanchors() {
    let mut b = attached(Variant::Good, 4);
    feed(&mut b, &adds(&[1, 2]));
    b.mark_broken("the host lost an input");
    feed(&mut b, &adds(&[3]));
    let retired = b.drain_retired();
    assert_eq!(retired.len(), 1);
    assert_eq!(retired[0].cause, Retire::Broken);
    assert!(retired[0].log.broken().is_some());
}

#[test]
fn a_restore_retires_the_recording_it_replaced_ending_in_the_state_before_it() {
    let mut b = attached(Variant::Good, 4);
    let c0 = b.checkpoint().unwrap().clone();
    feed(&mut b, &adds(&[4, 5]));
    let before = state_digest(b.schema(), &b.pair().state);
    b.restore(&c0).unwrap();
    let retired = b.drain_retired();
    assert_eq!(retired.len(), 1);
    let r = &retired[0];
    assert_eq!(r.cause, Retire::Restored);
    assert_eq!(r.checkpoint, c0);
    assert_eq!(r.log.inputs(), &adds(&[4, 5])[..]);
    assert_eq!(r.end_state, before);
    assert!(replays_to_its_end(r));
}

#[test]
fn a_commit_retires_the_recording_it_replaced_with_the_code_it_replaced() {
    let mut b = attached(Variant::Stranded, 4);
    feed(&mut b, &[ToyInput::Add(3), ToyInput::Tick]);
    let head = b.pause().unwrap();
    let p = b.prepare(code(Variant::Good)).unwrap();
    b.commit(p).unwrap();
    let retired = b.drain_retired();
    assert_eq!(retired.len(), 1);
    let r = &retired[0];
    assert_eq!(r.cause, Retire::Committed);
    assert_eq!(r.code.id(), code(Variant::Stranded).id(), "the old code");
    assert_eq!(r.end_state, head.state_digest, "the paused head");
    assert_eq!(r.log.len(), 2);
    assert!(replays_to_its_end(r));
}

#[test]
fn a_prepare_made_on_a_clone_of_the_paused_boundary_commits_on_the_original() {
    let mut b = attached(Variant::Stranded, 4);
    feed(&mut b, &[ToyInput::Add(3), ToyInput::Tick]);
    b.pause().unwrap();
    let shadow = b.clone();
    #[expect(
        clippy::disallowed_methods,
        reason = "the test moves a clone to another thread; the substrate itself spawns none"
    )]
    let worker = std::thread::spawn(move || {
        let mut shadow = shadow;
        let p = shadow.prepare(code(Variant::Good));
        (p, shadow.stats().replays)
    });
    let (p, replays) = worker.join().expect("the prepare thread ran");
    let p = p.expect("the clone prepares");
    assert_eq!(replays, 2, "both replays ran on the clone");
    assert_eq!(b.stats().replays, 0, "none ran on the original");
    let commit = b.commit(p).expect("the original commits a clone's prepare");
    assert_eq!(b.pair().code.id(), code(Variant::Good).id());
    assert_eq!(b.identity(), &identity(Variant::Good));
    assert_eq!(commit.witness.candidate_verdict(), &Verdict::Pass);
}

#[test]
fn negative_control_a_head_moved_since_the_clone_is_refused_stale_or_by_its_witness() {
    let base = {
        let mut b = attached(Variant::Stranded, 4);
        feed(&mut b, &[ToyInput::Add(3), ToyInput::Tick]);
        b.pause().unwrap();
        b
    };

    let mut moved = base.clone();
    let p = base.clone().prepare(code(Variant::Good)).unwrap();
    moved.reject();
    feed(&mut moved, &adds(&[1]));
    moved.pause().unwrap();
    assert!(
        matches!(moved.commit(p), Err(PublishRefusal::StaleHead { .. })),
        "an input since the clone"
    );

    let mut reattached = base.clone();
    let p = base.clone().prepare(code(Variant::Good)).unwrap();
    reattached.attach(9, 64).unwrap();
    reattached.pause().unwrap();
    assert_eq!(
        reattached.commit(p).err(),
        Some(PublishRefusal::WitnessMismatch("ancestor")),
        "the same head over a fresh recording"
    );
    assert_eq!(reattached.stats().publications, 0);

    let mut resumed = base.clone();
    let p = base.clone().prepare(code(Variant::Good)).unwrap();
    resumed.reject();
    assert_eq!(resumed.commit(p).err(), Some(PublishRefusal::NotPaused));
}

#[test]
fn the_registry_lists_each_slot_once_in_order_with_its_timeline_rows() {
    let mut timeline = Timeline::new(2);
    for (k, judged) in [
        Judged::Pass,
        Judged::Fail {
            law: "toy.tick-clears",
            cursor: 9,
        },
        Judged::Unjudged("the log is broken"),
    ]
    .into_iter()
    .enumerate()
    {
        let from = 4 * k as u64;
        timeline.push(Row::Segment {
            cause: Retire::Full,
            code: "toy".into(),
            log_epoch: k as u64 + 1,
            from,
            to: from + 4,
            kept: judged.failed().then_some(0),
            judged,
        });
    }
    assert_eq!(
        (timeline.len(), timeline.dropped()),
        (2, 1),
        "the oldest went"
    );
    let listing = |slot: u64, timeline: Timeline| Listing {
        slot,
        kind: TaskKind::Effect,
        task: "toy-task".into(),
        code: "good".into(),
        attached: true,
        paused: false,
        revision: 0,
        log_epoch: 3,
        log_len: 1,
        waiting: 0,
        kept: 1,
        timeline,
    };
    let mut reg = Registry::new();
    reg.insert(listing(7, Timeline::default()));
    reg.insert(listing(2, timeline.clone()));
    reg.insert(listing(7, Timeline::default()));
    assert_eq!(
        reg.listings().iter().map(|l| l.slot).collect::<Vec<_>>(),
        vec![2, 7]
    );
    let lines = reg.lines();
    assert_eq!(lines[0], "tasks=2");
    assert_eq!(
        lines[1],
        "slot=2 kind=effect task=toy-task code=good attached=true paused=false rev=0 log_epoch=3 \
         log=1 waiting=0 kept=1 rows=2 dropped=1"
    );
    assert_eq!(
        lines[2],
        "slot=2 row=0 segment cause=full code=toy log_epoch=2 range=4..8 \
         judged=fail:toy.tick-clears@9 kept=0"
    );
    assert_eq!(
        lines[3],
        "slot=2 row=1 segment cause=full code=toy log_epoch=3 range=8..12 \
         judged=unjudged:the_log_is_broken kept=no"
    );
    assert!(lines[4].starts_with("slot=7 kind=effect "));
    assert_eq!(lines.len(), 5);
    assert_eq!(
        Row::Detached {
            why: "secure input"
        }
        .words(),
        "detached why=secure_input"
    );
}

#[test]
fn zero_capacity_attach_breaks_without_reanchoring_or_evicting_retired_segments() {
    let mut b = boundary_over(Variant::Good, ToyState::default(), 4);
    b.attach(1, 2).unwrap();
    feed(&mut b, &adds(&[1, 2, 3]));
    assert_eq!(b.retired_len(), 1);
    assert_eq!(b.stats().reanchors, 1);

    b.attach(10, 0).unwrap();
    feed(&mut b, &adds(&[4, 5, 6]));
    assert!(b.log().unwrap().broken().is_some());
    assert_eq!(
        b.stats().reanchors,
        1,
        "zero-capacity recording must not spin reanchors on each input"
    );
    assert_eq!(
        b.retired_len(),
        1,
        "earlier retired segment must not be evicted by empty zero-cap segments"
    );
}
