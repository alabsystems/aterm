// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::test_models::{evict_full_model, ring_model, subscribe_model};

/// A pin and a bound over the `Buggy` dial are refused as bounds, while an
/// ordinary one- or two-sided range passes.
#[test]
fn a_bound_must_be_a_range() {
    let var = || Box::new(Expr::Var("x"));
    let int = |n| Box::new(Expr::Int(n));
    let konst = |name| Box::new(Expr::ConstRef(name));
    let consts = [("Cap", 3), ("Buggy", 0)];
    let refused = |e: Expr| !not_range_bounds(&e, &consts).is_empty();

    assert!(refused(Expr::Le(var(), int(0))), "x <= 0 pins x");
    assert!(refused(Expr::Gt(int(1), var())), "1 > x pins x");
    assert!(
        refused(Expr::Le(var(), konst("Buggy"))),
        "Buggy is the dial"
    );
    assert!(
        refused(Expr::And(
            Box::new(Expr::Gt(var(), int(0))),
            Box::new(Expr::Le(var(), int(1))),
        )),
        "x > 0 && x <= 1 pins x to one"
    );
    assert!(
        refused(Expr::Le(var(), Box::new(Expr::Var("y")))),
        "a relation"
    );

    assert!(!refused(Expr::Le(var(), konst("Cap"))));
    assert!(!refused(Expr::And(
        Box::new(Expr::Gt(var(), int(0))),
        Box::new(Expr::Le(var(), Box::new(Expr::Sub(konst("Cap"), int(1))))),
    )));
    // Duplicate constant bindings resolve to the last entry (matching `eval_env`), and
    // extreme `i64` arithmetic refuses cleanly instead of panicking.
    let dup_consts = [("Cap", 0), ("Cap", 3), ("Buggy", 0)];
    assert!(
        not_range_bounds(&Expr::Le(var(), konst("Cap")), &dup_consts).is_empty(),
        "last duplicate constant binding wins"
    );
    assert!(refused(Expr::Gt(var(), int(i64::MAX))));
    assert!(refused(Expr::Gt(int(i64::MIN), var())));
    assert!(refused(Expr::Le(
        var(),
        Box::new(Expr::Add(int(i64::MAX), int(1)))
    )));
}

#[test]
fn sub_with_additive_rhs_parenthesizes_to_match_the_interpreter() {
    let env = BTreeMap::new();

    let e = sub(int(5), sub(int(2), int(1)));
    assert_eq!(e.to_tla(), "5 - (2 - 1)", "Sub RHS must be parenthesized");
    assert_eq!(e.eval(&env).as_int(), 4);

    let e2 = sub(int(5), add(int(2), int(1)));
    assert_eq!(
        e2.to_tla(),
        "5 - (2 + 1)",
        "Sub RHS Add must be parenthesized"
    );
    assert_eq!(e2.eval(&env).as_int(), 2);

    let e3 = sub(sub(int(5), int(2)), int(1));
    assert_eq!(e3.to_tla(), "5 - 2 - 1", "left-nested Sub needs no parens");
    assert_eq!(e3.eval(&env).as_int(), 2);
}

/// An ill-typed Expr panics, as `eval`'s unbound-identifier arm does, never evaluating to a
/// default.
#[test]
#[should_panic(expected = "expected Int, got Bool")]
fn ill_typed_int_position_fails_closed() {
    // Add's operands must be integer-typed; a Bool there is a model bug.
    add(bool_lit(true), int(1)).eval(&BTreeMap::new());
}

/// The boolean-position twin of `ill_typed_int_position_fails_closed`.
#[test]
#[should_panic(expected = "expected Bool, got Int")]
fn ill_typed_bool_position_fails_closed() {
    // And's operands must be boolean-typed; an Int there is a model bug.
    and_(int(1), bool_lit(true)).eval(&BTreeMap::new());
}

#[test]
fn transition_spec_opens_init_but_shares_the_action() {
    let m = ring_model();
    let tla = m.transition_spec();
    assert!(tla.contains("CONSTANT MaxSeq, Cap, Buggy\n"), "{tla}");
    assert!(tla.contains("Init == TRUE\n"), "{tla}");
    assert!(
        tla.contains("Push == seq =< MaxSeq - 1 /\\ seq' = seq + 1 /\\ lo' = (IF"),
        "{tla}"
    );
    let cfg = m.transition_cfg(&[("Cap", 65536), ("MaxSeq", 1_000_000)]);
    assert_eq!(
        cfg,
        "CONSTANT MaxSeq = 1000000\nCONSTANT Cap = 65536\nCONSTANT Buggy = 0\n\
         SPECIFICATION Spec\nCHECK_DEADLOCK FALSE\n"
    );
}

#[test]
fn subscribe_emits_parenthesized_disjunction_guard_and_eq() {
    let tla = subscribe_model().to_tla();
    assert!(tla.contains("---- MODULE Subscribe ----"), "{tla}");
    assert!(
        tla.contains(
            "PollDeliver == (Buggy = 1 \\/ lo =< cursor + 1) /\\ cursor' = seq /\\ \
             lost' = (IF lo > cursor + 1 THEN 1 ELSE lost) /\\ UNCHANGED << seq, lo >>"
        ),
        "{tla}"
    );
    assert!(
        tla.contains("Next == Grow \\/ PollGap \\/ PollDeliver"),
        "{tla}"
    );
    assert!(tla.contains("NoSilentLoss == lost = 0"), "{tla}");
}

#[test]
fn evict_full_emits_function_valued_tla() {
    let tla = evict_full_model().to_tla();
    assert!(tla.contains("VARIABLES seq, lo, live"), "{tla}");
    assert!(
        tla.contains("live = [n \\in 1..MaxSeq |-> FALSE]"),
        "function init: {tla}"
    );
    assert!(
        tla.contains("[n \\in 1..MaxSeq |->"),
        "function comprehension: {tla}"
    );
    assert!(
        tla.contains("[live EXCEPT ![seq + 1] = TRUE]"),
        "EXCEPT update: {tla}"
    );
    assert!(tla.contains("live[n]"), "function access: {tla}");
    assert!(tla.contains("n # lo"), "inequality: {tla}");
    assert!(
        tla.contains(
            "EvictOldestContiguous == (\\A n \\in 1..MaxSeq : (live[n] <=> (lo =< n /\\ n =< seq)))"
        ),
        "quantified iff invariant: {tla}"
    );
}

#[test]
fn mixed_boolean_operators_keep_their_ast_grouping() {
    assert_eq!(
        or_(and_(Expr::Bool(true), Expr::Bool(false)), Expr::Bool(true)).to_tla(),
        "((TRUE /\\ FALSE) \\/ TRUE)"
    );
    assert_eq!(
        and_(Expr::Bool(true), or_(Expr::Bool(false), Expr::Bool(true))).to_tla(),
        "(TRUE /\\ (FALSE \\/ TRUE))"
    );
}

#[test]
fn a_quantifier_cannot_swallow_what_follows_it() {
    let quantified = forall("i", int(0), int(1), le(var("i"), var("x")));
    assert_eq!(
        Expr::Iff(Box::new(quantified), Box::new(Expr::Bool(false))).to_tla(),
        "((\\A i \\in 0..1 : i =< x) <=> FALSE)"
    );
}

#[test]
fn gpu_frame_ring_model_proves_and_catches_mutant() {
    let m = gpu_frame_ring_model();
    let states = crate::interp::prove_and_catch(&m);
    assert!(states > 1, "must explore multiple states: {states}");
    assert_eq!(
        crate::interp::fired_actions(&m).len(),
        m.actions.len(),
        "all actions must fire at Buggy=0"
    );
    crate::verify::prove_and_catch_scalar(&m, "GpuFrameRing");
}

#[test]
fn single_hold_prediction_reconcile_model_proves_and_catches_mutant() {
    let m = single_hold_prediction_reconcile_model();
    let states = crate::interp::prove_and_catch(&m);
    assert!(states > 1, "must explore multiple states: {states}");
    assert_eq!(
        crate::interp::fired_actions(&m).len(),
        m.actions.len(),
        "all actions must fire at Buggy=0"
    );
    crate::verify::prove_and_catch_scalar(&m, "SingleHoldPredictionReconcile");
}

#[test]
fn rle_local_compact_model_proves_and_catches_mutant() {
    let m = rle_local_compact_model();
    let states = crate::interp::prove_and_catch(&m);
    assert!(states > 1, "must explore multiple states: {states}");
    assert_eq!(
        crate::interp::fired_actions(&m).len(),
        m.actions.len(),
        "all actions must fire at Buggy=0"
    );
    crate::verify::prove_and_catch_scalar(&m, "RleLocalCompactModel");
}

#[test]
fn ascii_grapheme_fast_path_model_proves_and_catches_mutant() {
    let m = ascii_grapheme_fast_path_model();
    let states = crate::interp::prove_and_catch(&m);
    assert!(states > 1, "must explore multiple states: {states}");
    assert_eq!(
        crate::interp::fired_actions(&m).len(),
        m.actions.len(),
        "all actions must fire at Buggy=0"
    );
    crate::verify::prove_and_catch_scalar(&m, "AsciiGraphemeFastPath");
}

#[test]
fn pty_batch_yield_fairness_model_proves_and_catches_mutant() {
    let m = pty_batch_yield_fairness_model();
    let states = crate::interp::prove_and_catch(&m);
    assert!(states > 1, "must explore multiple states: {states}");
    assert_eq!(
        crate::interp::fired_actions(&m).len(),
        m.actions.len(),
        "all actions must fire at Buggy=0"
    );
    crate::verify::prove_and_catch_scalar(&m, "PtyBatchYieldFairness");
}

#[test]
fn warm_tier_cumulative_offset_model_proves_and_catches_mutant() {
    let m = warm_tier_cumulative_offset_model();
    let states = crate::interp::prove_and_catch(&m);
    assert!(states > 1, "must explore multiple states: {states}");
    assert_eq!(
        crate::interp::fired_actions(&m).len(),
        m.actions.len(),
        "all actions must fire at Buggy=0"
    );
    crate::verify::prove_and_catch_scalar(&m, "WarmTierCumulativeOffset");
}
