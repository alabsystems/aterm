// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The effect frontier: a shadow compares each request with the next record before handing
//! back its result, submits nothing live, and stops for good at the first divergence.

use std::cell::Cell;

use aterm_aos::effects::{
    EffectDivergence, EffectPort, EffectRecord, EffectRequest, NoEffects, RecordingPort, ShadowPort,
};
use aterm_aos::replay::replay;
use aterm_aos::task::StepRefusal;

use crate::toy::{ToyInput, ToyObs, ToySchema, ToyState, Variant, code};

fn req(target: u32, op: u16, payload: &[u8]) -> EffectRequest {
    EffectRequest {
        target,
        op,
        payload: payload.to_vec(),
    }
}

/// Three recorded effects, each answered with its payload reversed.
fn recorded() -> Vec<EffectRecord> {
    let mut port =
        RecordingPort::new(|r: &EffectRequest| r.payload.iter().rev().copied().collect());
    for r in [req(1, 10, b"ab"), req(2, 20, b"cd"), req(3, 30, b"ef")] {
        port.submit(r).unwrap();
    }
    port.into_records()
}

fn emit(target: u32, op: u16, payload: &[u8]) -> ToyInput {
    ToyInput::Emit {
        target,
        op,
        payload: payload.to_vec(),
    }
}

#[test]
fn a_recording_keeps_each_request_its_payload_digest_and_the_result_observed() {
    let recs = recorded();
    assert_eq!(recs.len(), 3);
    for (k, rec) in recs.iter().enumerate() {
        assert_eq!(rec.seq, k as u64);
    }
    assert_eq!(recs[1].target, 2);
    assert_eq!(recs[1].op, 20);
    assert_eq!(recs[1].payload, req(2, 20, b"cd").payload_digest());
    assert_eq!(recs[1].result, b"dc".to_vec());
    let mut later = RecordingPort::starting_at(40, |_: &EffectRequest| Vec::new());
    later.submit(req(1, 1, b"")).unwrap();
    assert_eq!(later.records()[0].seq, 40);
}

#[test]
fn a_shadow_of_the_same_requests_hands_back_each_recorded_result() {
    let recs = recorded();
    let mut shadow = ShadowPort::new(&recs);
    assert_eq!(shadow.submit(req(1, 10, b"ab")), Ok(b"ba".to_vec()));
    assert_eq!(shadow.submit(req(2, 20, b"cd")), Ok(b"dc".to_vec()));
    assert_eq!(shadow.submit(req(3, 30, b"ef")), Ok(b"fe".to_vec()));
    assert_eq!(shadow.matched(), 3);
    assert_eq!(shadow.frontier(), None);
    assert_eq!(shadow.finish(), Ok(3));
}

#[test]
fn a_request_past_the_recording_is_an_insertion() {
    let recs = recorded();
    let mut shadow = ShadowPort::new(&recs[..1]);
    shadow.submit(req(1, 10, b"ab")).unwrap();
    assert_eq!(
        shadow.submit(req(2, 20, b"cd")),
        Err(EffectDivergence::Inserted { position: 1 })
    );
}

#[test]
fn a_changed_target_op_or_payload_stops_at_the_frontier_before_any_result() {
    let recs = recorded();
    let mut t = ShadowPort::new(&recs);
    assert_eq!(
        t.submit(req(9, 10, b"ab")),
        Err(EffectDivergence::TargetDiverged {
            position: 0,
            recorded: 1,
            asked: 9
        })
    );
    let mut o = ShadowPort::new(&recs);
    assert_eq!(
        o.submit(req(1, 11, b"ab")),
        Err(EffectDivergence::OpDiverged {
            position: 0,
            recorded: 10,
            asked: 11
        })
    );
    let mut p = ShadowPort::new(&recs);
    assert_eq!(
        p.submit(req(1, 10, b"abc")),
        Err(EffectDivergence::PayloadDiverged {
            position: 0,
            recorded: recs[0].payload,
            asked: req(1, 10, b"abc").payload_digest()
        })
    );
}

#[test]
fn a_reordered_pair_of_requests_diverges_at_the_first_of_them() {
    let recs = recorded();
    let mut shadow = ShadowPort::new(&recs);
    shadow.submit(req(1, 10, b"ab")).unwrap();
    assert_eq!(
        shadow.submit(req(3, 30, b"ef")),
        Err(EffectDivergence::TargetDiverged {
            position: 1,
            recorded: 2,
            asked: 3
        })
    );
}

#[test]
fn a_deleted_effect_is_refused_at_finish_as_unreached() {
    let recs = recorded();
    let mut shadow = ShadowPort::new(&recs);
    shadow.submit(req(1, 10, b"ab")).unwrap();
    assert_eq!(
        shadow.finish(),
        Err(EffectDivergence::Unreached {
            position: 1,
            remaining: 2
        })
    );
}

#[test]
fn the_frontier_is_sticky_even_for_a_request_that_would_match_a_later_record() {
    let recs = recorded();
    let mut shadow = ShadowPort::new(&recs);
    let first = shadow.submit(req(7, 10, b"ab")).unwrap_err();
    // These would match records 0 and 1; past the frontier nothing is answered from the
    // recording.
    assert_eq!(shadow.submit(req(1, 10, b"ab")), Err(first));
    assert_eq!(shadow.submit(req(2, 20, b"cd")), Err(first));
    assert_eq!(shadow.frontier(), Some(&first));
    assert_eq!(shadow.matched(), 0);
    assert_eq!(shadow.finish(), Err(first));
}

#[test]
fn a_task_that_declares_no_effects_has_every_submission_refused() {
    assert_eq!(
        NoEffects.submit(req(5, 1, b"x")),
        Err(EffectDivergence::Undeclared { target: 5 })
    );
}

#[test]
fn shadow_replay_of_a_recorded_run_reproduces_it_and_submits_nothing_live() {
    let schema = ToySchema::new();
    let inputs = vec![emit(1, 10, b"ab"), ToyInput::Add(2), emit(2, 20, b"cd")];
    let live_calls = Cell::new(0u32);
    let mut rec = RecordingPort::new(|r: &EffectRequest| {
        live_calls.set(live_calls.get() + 1);
        r.payload.iter().rev().copied().collect()
    });
    let recorded_run = replay(
        &schema,
        &code(Variant::Good),
        ToyState::default(),
        &inputs,
        0,
        &mut rec,
    );
    let records = rec.into_records();
    // The record pass did reach the live sink: the counter observes submissions.
    assert_eq!(live_calls.get(), 2);
    assert_eq!(records.len(), 2);

    let mut shadow = ShadowPort::new(&records);
    let shadow_run = replay(
        &schema,
        &code(Variant::Good),
        ToyState::default(),
        &inputs,
        0,
        &mut shadow,
    );
    assert_eq!(live_calls.get(), 2, "the shadow submitted nothing live");
    assert_eq!(shadow.finish(), Ok(2));
    assert_eq!(shadow_run.observations, recorded_run.observations);
    assert_eq!(shadow_run.state_digest, recorded_run.state_digest);
    assert_eq!(
        shadow_run.observations[2].1,
        ToyObs::Emitted {
            result: b"dc".to_vec()
        }
    );
}

#[test]
fn a_candidate_whose_effects_differ_halts_at_the_frontier_and_runs_nothing_after_it() {
    let schema = ToySchema::new();
    let inputs = vec![emit(1, 10, b"ab"), ToyInput::Add(2), emit(2, 20, b"cd")];
    let mut rec = RecordingPort::new(|r: &EffectRequest| r.payload.clone());
    replay(
        &schema,
        &code(Variant::Good),
        ToyState::default(),
        &inputs,
        0,
        &mut rec,
    );
    let records = rec.into_records();

    let cases = [
        (
            Variant::ShiftTarget,
            0,
            EffectDivergence::TargetDiverged {
                position: 0,
                recorded: 1,
                asked: 2,
            },
        ),
        (
            Variant::ShiftOp,
            0,
            EffectDivergence::OpDiverged {
                position: 0,
                recorded: 10,
                asked: 11,
            },
        ),
        (
            Variant::AlterPayload,
            0,
            EffectDivergence::PayloadDiverged {
                position: 0,
                recorded: records[0].payload,
                asked: req(1, 10, b"ab\xee").payload_digest(),
            },
        ),
        (
            Variant::ExtraEffect,
            0,
            EffectDivergence::TargetDiverged {
                position: 1,
                recorded: 2,
                asked: 1,
            },
        ),
    ];
    for (variant, cursor, want) in cases {
        let mut shadow = ShadowPort::new(&records);
        let run = replay(
            &schema,
            &code(variant),
            ToyState::default(),
            &inputs,
            0,
            &mut shadow,
        );
        let (at, refusal): (u64, StepRefusal) = run.refused.clone().unwrap();
        assert_eq!((at, refusal.law), (cursor, "toy.effect"), "{variant:?}");
        assert!(
            run.observations.is_empty(),
            "{variant:?}: nothing ran past the frontier"
        );
        assert_eq!(shadow.frontier(), Some(&want), "{variant:?}");
        assert_eq!(shadow.finish(), Err(want), "{variant:?}");
    }

    // Omitting an effect submits nothing new, and is still a divergence at finish.
    let mut shadow = ShadowPort::new(&records);
    let run = replay(
        &schema,
        &code(Variant::SkipEffect),
        ToyState::default(),
        &inputs,
        0,
        &mut shadow,
    );
    assert_eq!(run.refused, None);
    assert_eq!(shadow.frontier(), None);
    assert_eq!(
        shadow.finish(),
        Err(EffectDivergence::Unreached {
            position: 0,
            remaining: 2
        })
    );
}
