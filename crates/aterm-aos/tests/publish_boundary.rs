// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Boundary publication: the repair protocol end to end, the queue during a pause, and every
//! refusal `pause`, `prepare` and `commit` can return, each leaving the boundary untouched.

use std::cell::RefCell;
use std::rc::Rc;

use aterm_aos::contract::{Contract, Verdict};
use aterm_aos::effects::{EffectDivergence, EffectRequest, NoEffects, RecordingPort};
use aterm_aos::evidence::EvidenceGrade;
use aterm_aos::publish::{Admit, PublishRefusal};
use aterm_aos::replay::{Divergence, Run};
use aterm_aos::task::{Code, Schema, StepRefusal, state_digest};
use aterm_aos::{ArtifactId, ArtifactKind, Digest, Revision};

use crate::toy::{
    TICK_CLEARS, ToyInput, ToyObs, ToySchema, ToyState, Variant, adds, apply, attached,
    boundary_over, code, contract, feed, fold, identity, judged_by, live, live_with,
    mislabelled_code, other_contract,
};

use ToyInput::{Add, Tick};

fn before() -> Vec<ToyInput> {
    vec![Add(3), Tick, Add(4), Tick]
}

#[test]
fn a_repair_replays_from_the_ancestor_and_publishes_one_new_pair_then_delivers_the_queue_once_in_order()
 {
    let mut b = attached(Variant::Stranded, 4);
    let c0 = b.checkpoint().unwrap().clone();
    feed(&mut b, &before());
    assert!(
        b.pair().state.hot,
        "the stranded tick left the accumulator hot"
    );
    let head = b.pause().unwrap();
    assert_eq!(head.input_cursor, 4);
    assert_eq!(head.state_digest, state_digest(b.schema(), &b.pair().state));
    assert_eq!(b.paused_head(), Some(&head));
    assert_eq!(b.admit(&Add(5)), Admit::Queued);
    assert_eq!(b.admit(&Tick), Admit::Skipped);
    assert_eq!(b.admit(&Add(6)), Admit::Queued);
    assert_eq!(b.queued(), 2);

    let p = b.prepare(code(Variant::Good)).unwrap();
    let w = p.witness();
    let repaired = fold(Variant::Good, ToyState::default(), &before());
    assert_eq!(w.grade(), EvidenceGrade::Tested);
    assert!(w.same_code_reproduced());
    assert!(!w.assumptions().is_empty());
    assert_eq!(w.head(), &head);
    assert_eq!(p.expected(), &head);
    assert_eq!(w.ancestor(), c0.digest());
    assert_eq!(w.range(), (0, 4));
    assert_eq!(w.log_epoch(), 1);
    assert_eq!(
        w.trace_digest(),
        b.log().unwrap().trace_digest(b.schema(), 0, 4).unwrap()
    );
    assert_eq!(w.contract(), contract().id());
    assert_eq!(w.live_code(), code(Variant::Stranded).id());
    assert_eq!(w.candidate_code(), code(Variant::Good).id());
    assert!(matches!(
        w.live_verdict(),
        Verdict::Fail {
            cursor: 1,
            law: TICK_CLEARS,
            ..
        }
    ));
    assert_eq!(w.candidate_verdict(), &Verdict::Pass);
    assert_eq!(w.candidate_state_digest(), repaired.state_digest);
    assert_eq!(w.candidate_observation_chain(), repaired.observation_chain);
    assert_eq!(
        w.live_observation_chain(),
        fold(Variant::Stranded, ToyState::default(), &before()).observation_chain
    );
    assert_eq!(p.state(), &repaired.state);
    assert_eq!(p.candidate().id(), code(Variant::Good).id());
    // Prepare replayed on private values: the live pair, the pause and the queue are as they
    // were.
    assert!(b.is_paused());
    assert_eq!(b.pair().code.id(), code(Variant::Stranded).id());
    assert_eq!(b.queued(), 2);

    let commit = b.commit(p).unwrap();
    assert_eq!(commit.revision, Revision(1));
    assert_eq!(
        commit.delivered,
        vec![
            Ok(ToyObs::Added { total: 12 }),
            Ok(ToyObs::Added { total: 18 })
        ]
    );
    assert!(!b.is_paused());
    assert_eq!(b.queued(), 0);
    assert_eq!(b.pair().code.id(), code(Variant::Good).id());
    assert_eq!(b.identity(), &identity(Variant::Good));
    assert_eq!(b.pair().head.revision, Revision(1));
    assert_eq!(b.pair().head.identity, identity(Variant::Good).digest());
    assert_eq!(b.pair().head.input_cursor, 6);
    let mut all = before();
    all.extend(adds(&[5, 6]));
    assert_eq!(
        b.pair().state,
        fold(Variant::Good, ToyState::default(), &all).state
    );

    // The log continues from a fresh checkpoint of the accepted state, in a new epoch.
    let c1 = b.checkpoint().unwrap();
    assert_eq!(c1.provenance().parent, Some(c0.digest()));
    assert_eq!(c1.provenance().revision, Revision(1));
    assert_eq!(c1.provenance().log_epoch, 2);
    assert_eq!(c1.provenance().input_cursor, 4);
    assert_eq!(c1.provenance().identity, identity(Variant::Good));
    assert_eq!(c1.state_digest(), repaired.state_digest);
    assert_eq!(b.log().unwrap().header().first_cursor, 4);
    assert_eq!(b.log().unwrap().inputs(), &adds(&[5, 6])[..]);
    let s = b.stats();
    assert_eq!(
        (
            s.publications,
            s.replays,
            s.rejections,
            s.queued_max,
            s.skipped
        ),
        (1, 2, 0, 2, 1)
    );

    // Fresh input reaches the new code through the live path, with no further replay.
    assert_eq!(
        live(&mut b, &Tick).1,
        Some(Ok(ToyObs::Ticked { hot: false }))
    );
    assert_eq!(b.stats().replays, 2);
}

#[test]
fn a_candidate_prepared_against_a_head_that_moved_is_refused_as_stale() {
    let mut b = attached(Variant::Stranded, 4);
    feed(&mut b, &[Add(1), Tick]);
    let h0 = b.pause().unwrap();
    let p = b.prepare(code(Variant::Good)).unwrap();
    assert!(b.reject().is_empty());
    live(&mut b, &Add(2));
    let h1 = b.pause().unwrap();
    assert_ne!(h0, h1);
    assert_eq!(
        b.commit(p).err(),
        Some(PublishRefusal::StaleHead {
            expected: Box::new(h0),
            live: Box::new(h1.clone())
        })
    );
    // Untouched: still paused over the old pair at the same head.
    assert!(b.is_paused());
    assert_eq!(b.pair().code.id(), code(Variant::Stranded).id());
    assert_eq!(b.head(), &h1);
    // Rerunning against the new head publishes.
    let again = b.prepare(code(Variant::Good)).unwrap();
    assert_eq!(again.expected(), &h1);
    assert!(b.commit(again).is_ok());
}

#[test]
fn a_second_candidate_prepared_against_the_same_head_is_refused_after_the_first_commits() {
    let mut b = attached(Variant::Stranded, 4);
    feed(&mut b, &[Add(1), Tick]);
    let h0 = b.pause().unwrap();
    let first = b.prepare(code(Variant::Good)).unwrap();
    let second = b.prepare(code(Variant::Good)).unwrap();
    let third = b.prepare(code(Variant::Good)).unwrap();
    let won = b.commit(first).unwrap();
    assert_eq!(b.commit(second).err(), Some(PublishRefusal::NotPaused));
    let h1 = b.pause().unwrap();
    assert_eq!(h1.revision, won.revision);
    assert_eq!(
        b.commit(third).err(),
        Some(PublishRefusal::StaleHead {
            expected: Box::new(h0),
            live: Box::new(h1)
        })
    );
    assert_eq!(b.stats().publications, 1);
}

#[test]
fn a_full_queue_rejects_the_update_before_dropping_anything_and_delivers_each_input_once_in_order()
{
    let mut b = attached(Variant::Stranded, 2);
    feed(&mut b, &[Add(1)]);
    b.pause().unwrap();
    let p = b.prepare(code(Variant::Good)).unwrap();
    assert_eq!(b.admit(&Add(2)), Admit::Queued);
    assert_eq!(b.admit(&Tick), Admit::Skipped);
    assert_eq!(b.admit(&Add(3)), Admit::Queued);
    assert_eq!(b.admit(&Add(4)), Admit::Rejected { delivered_after: 2 });
    assert!(!b.is_paused());
    assert_eq!(
        b.take_delivered(),
        vec![
            Ok(ToyObs::Added { total: 3 }),
            Ok(ToyObs::Added { total: 6 }),
            Ok(ToyObs::Added { total: 10 })
        ]
    );
    assert!(b.take_delivered().is_empty());
    // The old pair received all four, once each, in order (`seen` is an order-sensitive
    // fold, so a loss, a repeat or a reorder would show).
    let all = adds(&[1, 2, 3, 4]);
    assert_eq!(b.pair().code.id(), code(Variant::Stranded).id());
    assert_eq!(
        b.pair().state,
        fold(Variant::Stranded, ToyState::default(), &all).state
    );
    assert_eq!(b.log().unwrap().inputs(), &all[..]);
    assert_eq!(b.pair().head.input_cursor, 4);
    let s = b.stats();
    assert_eq!(
        (s.rejections, s.delivered, s.skipped, s.queued_max),
        (1, 4, 1, 2)
    );
    assert_eq!(b.commit(p).err(), Some(PublishRefusal::NotPaused));
}

#[test]
fn a_zero_capacity_queue_rejects_at_the_first_queued_input_and_delivers_it() {
    let mut b = attached(Variant::Stranded, 0);
    b.pause().unwrap();
    assert_eq!(b.admit(&Tick), Admit::Skipped);
    assert_eq!(b.admit(&Add(7)), Admit::Rejected { delivered_after: 0 });
    assert_eq!(b.take_delivered(), vec![Ok(ToyObs::Added { total: 7 })]);
    assert_eq!(b.pair().state.total, 7);
}

#[test]
fn a_rejection_resumes_the_old_pair_and_delivers_the_queue_once_in_order() {
    let mut b = attached(Variant::Stranded, 4);
    feed(&mut b, &[Add(1)]);
    b.pause().unwrap();
    assert_eq!(b.admit(&Add(2)), Admit::Queued);
    assert_eq!(b.admit(&Add(3)), Admit::Queued);
    let p = b.prepare(code(Variant::Good)).unwrap();
    assert_eq!(
        b.reject(),
        vec![
            Ok(ToyObs::Added { total: 3 }),
            Ok(ToyObs::Added { total: 6 })
        ]
    );
    let all = adds(&[1, 2, 3]);
    assert_eq!(b.pair().code.id(), code(Variant::Stranded).id());
    assert_eq!(
        b.pair().state,
        fold(Variant::Stranded, ToyState::default(), &all).state
    );
    assert_eq!(b.log().unwrap().inputs(), &all[..]);
    assert_eq!(b.commit(p).err(), Some(PublishRefusal::NotPaused));
    assert!(b.reject().is_empty(), "running: nothing to reject");
    assert_eq!(b.stats().rejections, 1);
}

#[test]
fn a_queued_input_the_old_code_refuses_is_reported_and_left_out_of_the_log() {
    let mut b = attached(Variant::Good, 4);
    b.pause().unwrap();
    let emit = ToyInput::Emit {
        target: 3,
        op: 1,
        payload: b"x".to_vec(),
    };
    assert_eq!(b.admit(&emit), Admit::Queued);
    assert_eq!(b.admit(&Add(2)), Admit::Queued);
    let out = b.reject();
    assert!(matches!(
        &out[0],
        Err(StepRefusal {
            law: "toy.effect",
            ..
        })
    ));
    assert_eq!(out[1], Ok(ToyObs::Added { total: 2 }));
    assert_eq!(b.log().unwrap().inputs(), &adds(&[2])[..]);
    assert_eq!(b.pair().head.input_cursor, 1);
    assert_eq!((b.stats().delivered, b.stats().refused), (2, 1));
}

#[test]
fn a_live_head_the_log_does_not_reproduce_is_refused() {
    let mut b = attached(Variant::Good, 4);
    assert_eq!(b.admit(&Add(1)), Admit::Run);
    // The host applies another input than the one it admitted: the log is not the history
    // that produced the head.
    let schema = b.schema().clone();
    let (live_code, state) = b.pair_mut();
    live_code
        .step(&schema, state, &Add(2), &mut NoEffects)
        .unwrap();
    b.advance();
    let h = b.pause().unwrap();
    let replayed = fold(Variant::Good, ToyState::default(), &adds(&[1])).state_digest;
    assert_eq!(
        b.prepare(code(Variant::Good)).err(),
        Some(PublishRefusal::LiveNotReproduced(Divergence::State {
            a: replayed,
            b: h.state_digest
        }))
    );
    assert!(b.is_paused());
}

#[test]
fn a_candidate_that_fails_the_contract_is_refused_with_its_verdict() {
    let mut b = attached(Variant::Good, 4);
    feed(&mut b, &[Add(1), Tick]);
    b.pause().unwrap();
    let got = b.prepare(code(Variant::Stranded)).err();
    assert!(matches!(
        got,
        Some(PublishRefusal::CandidateFailed(Verdict::Fail {
            cursor: 1,
            law: TICK_CLEARS,
            ..
        }))
    ));
}

#[test]
fn a_candidate_whose_entry_invariant_the_ancestor_breaks_is_refused() {
    let negative = ToyState {
        total: -5,
        ..ToyState::default()
    };
    let mut b = boundary_over(Variant::Good, negative, 4);
    b.attach(1, 64).unwrap();
    feed(&mut b, &[Add(1)]);
    b.pause().unwrap();
    let got = b.prepare(code(Variant::Strict)).err();
    assert!(matches!(
        got,
        Some(PublishRefusal::Entry(aterm_aos::task::EntryRefusal {
            law: "toy.non-negative",
            ..
        }))
    ));
}

#[test]
fn a_candidate_of_another_schema_is_refused() {
    let mut b = attached(Variant::Good, 4);
    b.pause().unwrap();
    let other = code(Variant::OtherSchema);
    let schema = ToySchema::new();
    assert_ne!(other.schema_digest(&schema), schema.id().digest);
    assert_eq!(
        b.prepare(other.clone()).err(),
        Some(PublishRefusal::SchemaChanged {
            live: schema.id().digest,
            candidate: other.schema_digest(&schema)
        })
    );
}

/// A boundary constructed with a contract of another artifact than the identity names (the
/// same judge, relabelled) refuses every prepare, naming both digests.
#[test]
fn a_boundary_constructed_with_another_contract_than_the_identity_names_refuses_every_prepare() {
    let mut b = judged_by(Variant::Stranded, ToyState::default(), 4, other_contract());
    b.attach(1, 64).unwrap();
    feed(&mut b, &[Add(1), Tick]);
    b.pause().unwrap();
    for candidate in [Variant::Good, Variant::Stranded] {
        assert_eq!(
            b.prepare(code(candidate)).err(),
            Some(PublishRefusal::ContractMismatch {
                live: contract().id().digest,
                asked: other_contract().id().digest
            })
        );
    }
    assert!(b.is_paused());
}

/// What a judge was shown of one run: its observation count, first and last cursors,
/// observation chain and state digest.
type Shown = (usize, Option<u64>, Option<u64>, Digest, Digest);

/// The corrected contract, keeping what it was shown of every run it judged.
#[derive(Clone, Debug)]
struct Watching {
    judge: crate::toy::TickClears,
    shown: Rc<RefCell<Vec<Shown>>>,
}

impl Contract<ToySchema> for Watching {
    fn id(&self) -> &ArtifactId {
        self.judge.id()
    }

    fn judge(&self, schema: &ToySchema, inputs: &[ToyInput], run: &Run<ToySchema>) -> Verdict {
        self.shown.borrow_mut().push(shown(run));
        self.judge.judge(schema, inputs, run)
    }
}

fn shown(run: &Run<ToySchema>) -> Shown {
    (
        run.observations.len(),
        run.observations.first().map(|(c, _)| *c),
        run.observations.last().map(|(c, _)| *c),
        run.observation_chain,
        run.state_digest,
    )
}

/// Prepare judges the live run as soon as it is checked and lets its observations go before
/// the candidate replays: the contract still sees each complete run exactly once, live first,
/// and the witness carries the verdicts and chains the straight folds give, for a failing
/// live run (stranded) and a passing one (good, republished).
#[test]
fn prepare_judges_the_complete_live_run_then_the_complete_candidate_run_once_each() {
    let schema = ToySchema::new();
    let trace = [Add(3), Tick, Add(4), Tick, Add(5), Tick];
    let corrected = contract();
    for live_variant in [Variant::Stranded, Variant::Good] {
        let shown_runs = Rc::new(RefCell::new(Vec::new()));
        let watching = Watching {
            judge: contract(),
            shown: Rc::clone(&shown_runs),
        };
        let mut b = judged_by(live_variant, ToyState::default(), 4, watching);
        b.attach(1, 64).unwrap();
        for i in &trace {
            let schema = b.schema().clone();
            assert_eq!(b.admit(i), Admit::Run);
            let (code, state) = b.pair_mut();
            code.step(&schema, state, i, &mut NoEffects).unwrap();
            b.advance();
        }
        b.pause().unwrap();
        let p = b.prepare(code(Variant::Good)).unwrap();
        let w = p.witness();
        let live_run = fold(live_variant, ToyState::default(), &trace);
        let candidate_run = fold(Variant::Good, ToyState::default(), &trace);
        assert_eq!(live_run.observations.len(), trace.len());
        assert_eq!(
            *shown_runs.borrow(),
            vec![shown(&live_run), shown(&candidate_run)],
            "{live_variant:?}: one judgement of each complete run, live first"
        );
        assert_eq!(
            w.live_verdict(),
            &corrected.judge(&schema, &trace, &live_run)
        );
        assert_eq!(
            w.candidate_verdict(),
            &corrected.judge(&schema, &trace, &candidate_run)
        );
        assert_eq!(w.live_observation_chain(), live_run.observation_chain);
        assert_eq!(
            w.candidate_observation_chain(),
            candidate_run.observation_chain
        );
        assert_eq!(w.candidate_state_digest(), candidate_run.state_digest);
        assert_eq!(p.state(), &candidate_run.state);
        assert_eq!(
            w.live_verdict().passed(),
            live_variant == Variant::Good,
            "non-vacuity: the stranded live run fails and the good one passes"
        );
    }
}

/// A judge that passes every run, declaring whatever contract artifact it is given.
#[derive(Clone, Debug)]
struct Lenient(ArtifactId);

impl Contract<ToySchema> for Lenient {
    fn id(&self) -> &ArtifactId {
        &self.0
    }

    fn judge(&self, _: &ToySchema, _: &[ToyInput], _: &Run<ToySchema>) -> Verdict {
        Verdict::Pass
    }
}

/// The boundary judges by the contract it was constructed with, and `prepare` takes only the
/// candidate, so a caller holding a lenient judge under the identity's own contract id has no
/// way to hand it over: the stranded candidate fails the corrected contract however often it
/// is prepared, before and after a reject and a fresh pause, and the commit of the good one
/// carries a witness naming the boundary's own contract. The lenient judge would have passed
/// the stranded run (the negative control), and a boundary *constructed* with it publishes
/// that run: the constructor is the trusted party.
#[test]
fn a_candidate_failing_the_boundarys_contract_is_refused_whatever_judge_the_caller_holds() {
    let schema = ToySchema::new();
    let lenient = Lenient(contract().id().clone());
    let trace = [Add(1), Tick];
    let stranded_run = fold(Variant::Stranded, ToyState::default(), &trace);
    assert_eq!(
        lenient.judge(&schema, &trace, &stranded_run),
        Verdict::Pass,
        "negative control: the caller's judge passes the stranded run"
    );
    let mut b = attached(Variant::Good, 4);
    feed(&mut b, &trace);
    let failed = |b: &mut crate::toy::ToyBoundary| {
        matches!(
            b.prepare(code(Variant::Stranded)).err(),
            Some(PublishRefusal::CandidateFailed(Verdict::Fail {
                cursor: 1,
                law: TICK_CLEARS,
                ..
            }))
        )
    };
    b.pause().unwrap();
    assert!(failed(&mut b) && failed(&mut b), "refused at every prepare");
    assert!(b.reject().is_empty());
    feed(&mut b, &[Add(2)]);
    b.pause().unwrap();
    assert!(failed(&mut b), "refused after a reject and a fresh pause");
    let p = b.prepare(code(Variant::Good)).unwrap();
    assert_eq!(p.witness().contract(), b.contract().id());
    assert_eq!(p.witness().contract(), contract().id());
    let committed = b.commit(p).unwrap();
    assert_eq!(committed.witness.contract(), contract().id());

    // The trusted constructor: given the lenient judge under the identity's id, the boundary
    // publishes what that judge passes.
    let mut trusted = judged_by(Variant::Good, ToyState::default(), 4, lenient);
    trusted.attach(1, 64).unwrap();
    for i in &trace {
        assert_eq!(trusted.admit(i), Admit::Run);
        let schema = trusted.schema().clone();
        let (c, state) = trusted.pair_mut();
        c.step(&schema, state, i, &mut NoEffects).unwrap();
        trusted.advance();
    }
    trusted.pause().unwrap();
    assert!(trusted.prepare(code(Variant::Stranded)).is_ok());
}

#[test]
fn a_candidate_whose_artifact_is_not_code_is_refused() {
    let mut b = attached(Variant::Good, 4);
    b.pause().unwrap();
    assert_eq!(
        b.prepare(mislabelled_code()).err(),
        Some(PublishRefusal::NotCode {
            kind: ArtifactKind::Contract
        })
    );
}

#[test]
fn a_candidate_that_refuses_a_recorded_input_is_refused_naming_its_cursor() {
    let mut b = attached(Variant::Good, 4);
    feed(&mut b, &adds(&[2, 13, 4]));
    b.pause().unwrap();
    assert_eq!(
        b.prepare(code(Variant::Picky)).err(),
        Some(PublishRefusal::CandidateRefused { cursor: 1 })
    );
}

#[test]
fn pause_and_prepare_refuse_outside_their_states() {
    let mut b = boundary_over(Variant::Good, ToyState::default(), 4);
    assert_eq!(b.pause().err(), Some(PublishRefusal::NotAttached));
    assert_eq!(
        b.prepare(code(Variant::Good)).err(),
        Some(PublishRefusal::NotAttached)
    );
    b.attach(1, 64).unwrap();
    assert_eq!(
        b.prepare(code(Variant::Good)).err(),
        Some(PublishRefusal::NotPaused)
    );
    assert_eq!(b.admit(&Add(1)), Admit::Run);
    assert_eq!(b.pause().err(), Some(PublishRefusal::MidStep));
    let schema = b.schema().clone();
    let (live_code, state) = b.pair_mut();
    live_code
        .step(&schema, state, &Add(1), &mut NoEffects)
        .unwrap();
    b.advance();
    b.pause().unwrap();
    assert_eq!(b.pause().err(), Some(PublishRefusal::AlreadyPaused));
    assert!(b.prepare(code(Variant::Good)).is_ok());
}

#[test]
fn a_recording_break_refuses_the_repair_across_it_and_the_next_input_reanchors() {
    let mut b = attached(Variant::Stranded, 4);
    feed(&mut b, &[Add(1)]);
    b.mark_broken("the host lost an input");
    b.pause().unwrap();
    assert_eq!(
        b.prepare(code(Variant::Good)).err(),
        Some(PublishRefusal::TraceIncomplete("the host lost an input"))
    );
    b.reject();
    feed(&mut b, &[Add(2), Tick]);
    assert_eq!(b.stats().reanchors, 1);
    assert_eq!(b.stats().breaks, 1);
    assert_eq!(b.checkpoint().unwrap().provenance().input_cursor, 1);
    b.pause().unwrap();
    let p = b.prepare(code(Variant::Good)).unwrap();
    assert_eq!(p.witness().range(), (1, 3));
}

/// An input admitted while the previous one is pending (stepped, neither advanced nor
/// withdrawn) breaks the log and captures nothing: the ancestor stays C0, the second input is
/// not recorded, and a repair across the break refuses. The next input admitted at a true
/// boundary re-anchors at a checkpoint whose cursor is the head's and whose state holds both
/// stepped inputs, and a repair from it replays.
#[test]
fn an_input_admitted_twice_without_advance_breaks_the_log_without_a_mid_step_capture() {
    let mut b = attached(Variant::Good, 4);
    let c0 = b.checkpoint().unwrap().clone();
    assert_eq!(b.admit(&Add(1)), Admit::Run);
    apply(&mut b, &Add(1)).unwrap();
    assert_eq!(b.admit(&Add(2)), Admit::Run);
    assert_eq!((b.stats().breaks, b.stats().reanchors), (1, 0));
    assert_eq!(b.checkpoint(), Some(&c0), "nothing is captured mid-step");
    assert_eq!(b.log().unwrap().inputs(), &adds(&[1])[..]);
    apply(&mut b, &Add(2)).unwrap();
    b.advance();
    b.pause().unwrap();
    assert_eq!(
        b.prepare(code(Variant::Good)).err(),
        Some(PublishRefusal::TraceIncomplete(
            "an admitted input was neither advanced nor withdrawn"
        ))
    );
    assert!(b.reject().is_empty());
    feed(&mut b, &[Add(4)]);
    assert_eq!(b.stats().reanchors, 1);
    let c1 = b.checkpoint().unwrap();
    assert_eq!(c1.provenance().parent, Some(c0.digest()));
    assert_eq!(
        c1.provenance().input_cursor,
        1,
        "the head's cursor at the boundary"
    );
    assert_eq!(
        c1.state_digest(),
        fold(Variant::Good, ToyState::default(), &adds(&[1, 2])).state_digest
    );
    b.pause().unwrap();
    let p = b.prepare(code(Variant::Good)).unwrap();
    assert_eq!(p.witness().range(), (1, 2));
}

/// An attach between an admitted input (already stepped) and its advance is refused as
/// `MidStep` and changes nothing: no checkpoint is captured and the log is neither replaced
/// nor broken. After the advance, attach captures a checkpoint whose cursor counts the input
/// and whose state is the fold, and restoring it lands where uninterrupted execution is.
#[test]
fn an_attach_between_an_admitted_input_and_its_advance_is_refused_and_changes_nothing() {
    let mut b = attached(Variant::Good, 4);
    feed(&mut b, &[Add(5)]);
    let c0 = b.checkpoint().unwrap().clone();
    assert_eq!(b.admit(&Add(1)), Admit::Run);
    apply(&mut b, &Add(1)).unwrap();
    let stats = b.stats();
    assert_eq!(b.attach(2, 64), Err(PublishRefusal::MidStep));
    assert_eq!(b.checkpoint(), Some(&c0));
    let log = b.log().unwrap();
    assert_eq!(
        (log.inputs(), log.header().log_epoch, log.broken()),
        (&adds(&[5, 1])[..], 1, None)
    );
    assert_eq!(b.stats(), stats);
    b.advance();
    b.attach(2, 64).unwrap();
    let c1 = b.checkpoint().unwrap().clone();
    let straight = fold(Variant::Good, ToyState::default(), &adds(&[5, 1]));
    assert_eq!(
        (c1.provenance().input_cursor, c1.state_digest()),
        (2, straight.state_digest)
    );
    let restored = b.restore(&c1).unwrap();
    assert_eq!(
        (restored.input_cursor, restored.state_digest),
        (2, straight.state_digest)
    );
}

#[test]
fn an_advance_without_an_admitted_input_breaks_the_log() {
    let mut b = attached(Variant::Good, 4);
    b.advance();
    assert_eq!(b.stats().breaks, 1);
    assert_eq!(b.pair().head.input_cursor, 1);
    b.pause().unwrap();
    assert!(matches!(
        b.prepare(code(Variant::Good)).err(),
        Some(PublishRefusal::TraceIncomplete(_))
    ));
}

#[test]
fn a_withdrawn_input_leaves_the_log_and_the_head_where_they_were() {
    let mut b = attached(Variant::Picky, 4);
    assert_eq!(
        live(&mut b, &Add(1)).1,
        Some(Ok(ToyObs::Added { total: 1 }))
    );
    let (admit, r) = live(&mut b, &Add(13));
    assert_eq!(admit, Admit::Run);
    assert!(matches!(
        r,
        Some(Err(StepRefusal {
            law: "toy.no-thirteen",
            ..
        }))
    ));
    feed(&mut b, &[Add(2)]);
    assert_eq!(b.log().unwrap().inputs(), &adds(&[1, 2])[..]);
    assert_eq!(b.pair().head.input_cursor, 2);
    assert_eq!((b.stats().delivered, b.stats().refused), (3, 1));
    b.pause().unwrap();
    assert!(b.prepare(code(Variant::Good)).is_ok());
}

#[test]
fn a_full_log_reanchors_at_a_fresh_checkpoint_and_repair_replays_from_the_newest() {
    let mut b = boundary_over(Variant::Good, ToyState::default(), 4);
    b.attach(7, 2).unwrap();
    feed(&mut b, &adds(&[1, 2, 3, 4, 5]));
    assert_eq!(b.stats().reanchors, 2);
    let c = b.checkpoint().unwrap();
    assert_eq!(c.provenance().input_cursor, 4);
    assert_eq!(c.provenance().log_epoch, 9);
    assert_eq!(
        c.state_digest(),
        fold(Variant::Good, ToyState::default(), &adds(&[1, 2, 3, 4])).state_digest
    );
    assert_eq!(b.log().unwrap().inputs(), &adds(&[5])[..]);
    b.pause().unwrap();
    let p = b.prepare(code(Variant::Good)).unwrap();
    assert_eq!((p.witness().range(), p.witness().log_epoch()), ((4, 5), 9));
}

/// Every admit asks whether the log needs a re-anchor, and only a re-anchor takes the parent
/// checkpoint's digest. Over successive full logs, each fresh checkpoint still names the one
/// it replaced as its parent, each fresh log names the fresh checkpoint as its ancestor, and
/// an admit into a log with room re-anchors nothing.
#[test]
fn each_full_log_reanchor_names_the_checkpoint_it_replaced_and_an_admit_with_room_names_none() {
    let mut b = boundary_over(Variant::Good, ToyState::default(), 4);
    b.attach(3, 2).unwrap();
    let mut seen = vec![b.checkpoint().unwrap().clone()];
    for (i, n) in (1..=7).enumerate() {
        feed(&mut b, &adds(&[n]));
        let c = b.checkpoint().unwrap();
        if c != seen.last().unwrap() {
            seen.push(c.clone());
        }
        // Two inputs per log: the log after input i + 1 holds the inputs past the last
        // re-anchor, never more than its bound.
        assert_eq!(b.log().unwrap().len(), i % 2 + 1, "after input {n}");
        assert_eq!(b.stats().reanchors, i as u64 / 2, "after input {n}");
    }
    assert_eq!(seen.len(), 4, "three re-anchors over seven inputs");
    for pair in seen.windows(2) {
        assert_eq!(pair[1].provenance().parent, Some(pair[0].digest()));
        assert_eq!(
            pair[1].provenance().log_epoch,
            pair[0].provenance().log_epoch + 1
        );
    }
    let newest = seen.last().unwrap();
    assert_eq!(b.log().unwrap().header().ancestor, newest.digest());
    assert_eq!(newest.provenance().input_cursor, 6);
    assert_eq!(b.stats().breaks, 0);
    b.pause().unwrap();
    let p = b.prepare(code(Variant::Good)).unwrap();
    assert_eq!(p.witness().ancestor(), newest.digest());
    assert_eq!(p.witness().range(), (6, 7));
}

#[test]
fn a_live_effect_the_boundary_did_not_record_is_refused_at_prepare() {
    let mut b = attached(Variant::Good, 4);
    let mut sink = RecordingPort::new(|r: &EffectRequest| r.payload.clone());
    let emit = ToyInput::Emit {
        target: 1,
        op: 1,
        payload: b"x".to_vec(),
    };
    let (_, r) = live_with(&mut b, &emit, &mut sink);
    assert!(matches!(r, Some(Ok(ToyObs::Emitted { .. }))));
    b.pause().unwrap();
    assert_eq!(
        b.prepare(code(Variant::Good)).err(),
        Some(PublishRefusal::LiveEffects(EffectDivergence::Inserted {
            position: 0
        }))
    );
}

#[test]
fn detaching_while_paused_rejects_first_and_delivers_the_queue() {
    let mut b = attached(Variant::Good, 4);
    feed(&mut b, &[Add(1)]);
    b.pause().unwrap();
    assert_eq!(b.admit(&Add(2)), Admit::Queued);
    b.detach();
    assert!(!b.is_paused());
    assert_eq!(b.take_delivered(), vec![Ok(ToyObs::Added { total: 3 })]);
    assert!(b.checkpoint().is_none() && b.log().is_none());
    assert_eq!(b.pair().state.total, 3);
    // Detached, the live path still runs (nothing is recorded).
    assert_eq!(
        live(&mut b, &Add(1)),
        (Admit::Run, Some(Ok(ToyObs::Added { total: 4 })))
    );
}

#[test]
fn reattaching_reanchors_at_the_current_boundary_under_the_old_checkpoint() {
    let mut b = attached(Variant::Good, 4);
    let c0 = b.checkpoint().unwrap().clone();
    feed(&mut b, &[Add(1)]);
    b.attach(5, 64).unwrap();
    let c1 = b.checkpoint().unwrap();
    assert_eq!(c1.provenance().parent, Some(c0.digest()));
    assert_eq!(c1.provenance().input_cursor, 1);
    assert_eq!(c1.provenance().log_epoch, 5);
    assert!(b.log().unwrap().is_empty());
    // A pending pause is rejected first, its queue delivered.
    b.pause().unwrap();
    assert_eq!(b.admit(&Add(2)), Admit::Queued);
    b.attach(6, 64).unwrap();
    assert!(!b.is_paused());
    assert_eq!(b.take_delivered(), vec![Ok(ToyObs::Added { total: 3 })]);
    assert_eq!(b.checkpoint().unwrap().provenance().input_cursor, 2);
}

#[test]
fn the_head_brings_its_state_digest_up_to_date() {
    let mut b = attached(Variant::Good, 4);
    feed(&mut b, &adds(&[1, 2]));
    let fresh = state_digest(b.schema(), &b.pair().state);
    assert_eq!(b.head().state_digest, fresh);
    assert_eq!(b.pair().head.state_digest, fresh);
}

#[test]
fn the_contract_judges_the_faulty_code_failing_and_the_repair_passing_on_one_trace() {
    let schema = ToySchema::new();
    let trace = before();
    let faulty = fold(Variant::Stranded, ToyState::default(), &trace);
    let fixed = fold(Variant::Good, ToyState::default(), &trace);
    assert!(!contract().judge(&schema, &trace, &faulty).passed());
    assert!(contract().judge(&schema, &trace, &fixed).passed());
}

/// A step that breaks the refusal law (it moves the state, then refuses) delivered by the
/// boundary itself, through `reject` and through a full queue's overflow: the head's digest is
/// the moved state's, so `pause` captures it and `prepare` refuses the log that cannot
/// explain it (`LiveNotReproduced`), exactly as on the live path. Before the boundary marked
/// its digest stale around its own deliveries, the head kept the old digest, `prepare`'s
/// replay landed on it and the leak was published away.
#[test]
fn a_refused_delivery_that_moved_the_state_is_caught_through_reject_and_through_overflow() {
    for cap in [4usize, 0] {
        let mut b = attached(Variant::Leaky, cap);
        feed(&mut b, &[Add(1)]);
        b.pause().unwrap();
        let refused = if cap == 0 {
            assert_eq!(
                b.admit(&Add(13)),
                Admit::Rejected { delivered_after: 0 },
                "cap {cap}: the overflow delivers it"
            );
            b.take_delivered()
        } else {
            assert_eq!(b.admit(&Add(13)), Admit::Queued);
            b.reject()
        };
        assert!(
            matches!(
                refused.as_slice(),
                [Err(StepRefusal {
                    law: "toy.no-thirteen",
                    ..
                })]
            ),
            "cap {cap}: {refused:?}"
        );
        let real = state_digest(b.schema(), &b.pair().state);
        assert_ne!(
            real,
            fold(Variant::Leaky, ToyState::default(), &adds(&[1])).state_digest,
            "fixture: the refused step moved the state"
        );
        assert_eq!(b.head().state_digest, real, "cap {cap}: the head's digest");
        let head = b.pause().unwrap();
        assert_eq!(head.state_digest, real, "cap {cap}: the pause captures it");
        assert!(
            matches!(
                b.prepare(code(Variant::Good)).err(),
                Some(PublishRefusal::LiveNotReproduced(Divergence::State { .. }))
            ),
            "cap {cap}: the log does not explain the moved state"
        );
        assert!(b.is_paused(), "cap {cap}: the refusal held the pause");
    }
}

/// While paused, the last queued input is readable (a skipped one never queues); running,
/// nothing is queued.
#[test]
fn the_last_queued_input_is_the_one_queued_last() {
    let mut b = attached(Variant::Good, 4);
    assert_eq!(b.last_queued(), None);
    b.pause().unwrap();
    assert_eq!(b.last_queued(), None);
    assert_eq!(b.admit(&Add(5)), Admit::Queued);
    assert_eq!(b.admit(&ToyInput::Reset), Admit::Queued);
    assert_eq!(b.admit(&Tick), Admit::Skipped);
    assert_eq!(b.last_queued(), Some(&ToyInput::Reset));
    b.reject();
    assert_eq!(b.last_queued(), None);
}

/// Two boundaries attached once each, over different states, record headers equal in
/// identity, log epoch and first cursor: only the ancestor tells their logs apart. Window
/// B's log read beside window A's checkpoint is refused naming both; held in A's recording
/// (the fault-injection graft), `prepare` and `commit` refuse it (`AncestorMismatch`), and
/// each refusal leaves A as it was.
#[test]
fn a_log_paired_with_another_windows_checkpoint_is_refused() {
    let mut a = attached(Variant::Good, 4);
    let mut b = boundary_over(
        Variant::Good,
        ToyState {
            total: 5,
            ..ToyState::default()
        },
        4,
    );
    b.attach(1, 64).unwrap();
    feed(&mut a, &before());
    feed(&mut b, &before());
    let (ha, hb) = (a.log().unwrap().header(), b.log().unwrap().header());
    assert_eq!(
        (&ha.identity, ha.log_epoch, ha.first_cursor),
        (&hb.identity, hb.log_epoch, hb.first_cursor),
        "fixture: the old header fields cannot tell the windows apart"
    );
    let (ca, cb) = (
        a.checkpoint().unwrap().digest(),
        b.checkpoint().unwrap().digest(),
    );
    assert_ne!(ca, cb);
    assert_eq!((ha.ancestor, hb.ancestor), (ca, cb));
    let b_bytes = b.log().unwrap().encode(b.schema()).unwrap();
    assert_eq!(
        aterm_aos::log::EventLog::<ToySchema>::decode(
            &ToySchema::new(),
            &b_bytes,
            &identity(Variant::Good),
            &ca,
            64
        )
        .err(),
        Some(aterm_aos::log::LogFault::Ancestor {
            expected: ca,
            found: cb
        })
    );
    graft_refusals(&mut a, &b);
}

#[cfg(debug_assertions)]
fn graft_refusals(a: &mut crate::toy::ToyBoundary, b: &crate::toy::ToyBoundary) {
    let (ca, cb) = (
        a.checkpoint().unwrap().digest(),
        b.checkpoint().unwrap().digest(),
    );
    let mismatch = PublishRefusal::AncestorMismatch {
        checkpoint: ca,
        log: cb,
    };
    let (a_log, b_log) = (a.log().unwrap().clone(), b.log().unwrap().clone());
    a.pause().unwrap();
    a.graft_log(b_log.clone()).unwrap();
    assert_eq!(a.prepare(code(Variant::Good)).err(), Some(mismatch.clone()));
    a.graft_log(a_log.clone()).unwrap();
    let prepared = a.prepare(code(Variant::Good)).unwrap();
    a.graft_log(b_log).unwrap();
    assert_eq!(a.commit(prepared).err(), Some(mismatch));
    assert!(a.is_paused(), "the refusal held the pause");
    assert_eq!(a.stats().publications, 0);
    // The control: A's own log beside A's checkpoint prepares and commits.
    a.graft_log(a_log).unwrap();
    let prepared = a.prepare(code(Variant::Good)).unwrap();
    assert!(a.commit(prepared).is_ok());
}

#[cfg(not(debug_assertions))]
fn graft_refusals(_: &mut crate::toy::ToyBoundary, _: &crate::toy::ToyBoundary) {}
