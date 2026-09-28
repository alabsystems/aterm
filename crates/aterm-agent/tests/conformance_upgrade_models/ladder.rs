// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! HarnessModelLadder, Tier-1: WHEN the live upgrade takes a due model move
//! (`aterm_agent::harness::upgrade_models::model_moves_now`), driven over
//! every reachable state of the derived model and projected back onto it.
//!
//! The model is a hand-written description; this is what makes it a statement
//! about the code that compiled. At every reachable readable state the REAL
//! decision must agree with the model's guards — `VisitMoves` enabled exactly
//! where `model_moves_now` answers a move, `VisitWaits` exactly where it waits
//! — and each observed transition is validated against the model on every
//! installed tier. The negative controls: forged successors the model must
//! refuse, and the incident's own rule (move only on a cold cache), which must
//! disagree with the model somewhere reachable — or the bind proves nothing.

use std::collections::{BTreeSet, VecDeque};

use aterm_agent::harness::upgrade_models::{MODEL_WARM_MAX_S, model_moves_now};
use aterm_spec::derive::{Model, harness_model_ladder_model};

use super::{Vars, assert_forged_rejected, assert_transition};

/// The model's `Warm`: readable visits of being due before the bound.
const WARM: i64 = 3;

/// Every state the model can reach, breadth first.
fn reachable(model: &Model) -> Vec<Vars> {
    let mut seen: BTreeSet<Vars> = BTreeSet::new();
    let mut out = Vec::new();
    let mut q = VecDeque::from([model.init_state()]);
    while let Some(s) = q.pop_front() {
        if !seen.insert(s.clone()) {
            continue;
        }
        for a in &model.actions {
            for ns in model.successors(a.name, &s) {
                q.push_back(ns);
            }
        }
        out.push(s);
    }
    out
}

/// The model's clock (readable visits due) as the real code's seconds due:
/// `Warm` visits is exactly `MODEL_WARM_MAX_S`.
fn due_for_s(clock: i64) -> u64 {
    u64::try_from(clock).expect("clock >= 0") * MODEL_WARM_MAX_S
        / u64::try_from(WARM).expect("warm")
}

/// The real decision at a model state.
fn real_moves(s: &Vars) -> bool {
    model_moves_now(s["cold"] == 1, s["restart"] == 1, due_for_s(s["clock"])).is_some()
}

#[test]
fn the_real_ladder_is_the_models_ladder_at_every_reachable_state() {
    let model = harness_model_ladder_model();
    let states = reachable(&model);
    let mut decided = 0usize;
    let (mut moves, mut waits) = (0usize, 0usize);
    for s in &states {
        if s["moved"] == 1 || s["unknown"] == 1 {
            // Nothing to decide: moved already, or this visit cannot read the
            // live model — and the model offers no visit either way.
            assert!(model.successors("VisitMoves", s).is_empty(), "{s:?}");
            assert!(model.successors("VisitWaits", s).is_empty(), "{s:?}");
            continue;
        }
        decided += 1;
        let model_moves = model.successors("VisitMoves", s);
        let model_waits = model.successors("VisitWaits", s);
        if real_moves(s) {
            moves += 1;
            assert_eq!(
                model_moves.len(),
                1,
                "the real ladder moves, the model does not: {s:?}"
            );
            assert!(
                model_waits.is_empty(),
                "the model waits where the real ladder moves: {s:?}"
            );
            let mut after = s.clone();
            after.insert("moved", 1);
            assert_transition(&model, s, &after, "VisitMoves", "model_moves_now (move)");
            // Forged: a "move" that also advanced the wait.
            assert_forged_rejected(
                &model,
                s,
                &after,
                "VisitMoves",
                &[("clock", s["clock"] + 1)],
                "a move that also counted a wait",
            );
        } else {
            waits += 1;
            assert!(
                model_moves.is_empty(),
                "the model moves where the real ladder waits: {s:?}"
            );
            assert_eq!(
                model_waits.len(),
                1,
                "the real ladder waits, the model cannot: {s:?}"
            );
            let mut after = s.clone();
            after.insert("clock", s["clock"] + 1);
            assert_transition(&model, s, &after, "VisitWaits", "model_moves_now (wait)");
            // Forged: a wait that silently moved the model too.
            assert_forged_rejected(
                &model,
                s,
                &after,
                "VisitWaits",
                &[("moved", 1)],
                "a wait that also moved",
            );
        }
    }
    // Non-vacuity: the bind saw both answers, and at every rung.
    assert!(
        moves > 0 && waits > 0,
        "one-sided bind: {moves} moves, {waits} waits"
    );
    for (cold, restart, clock) in [(1, 0, 0), (0, 1, 0), (0, 0, WARM)] {
        assert!(
            states.iter().any(|s| s["moved"] == 0
                && s["unknown"] == 0
                && s["cold"] == cold
                && s["restart"] == restart
                && s["clock"] == clock),
            "rung cold={cold} restart={restart} clock={clock} never reached"
        );
    }
    assert!(decided > 0);
}

/// THE CAUGHT NEGATIVE CONTROL: the incident's rule — a due move taken only on
/// a cold cache — must disagree with the model at a reachable state, and the
/// disagreement must be the incident itself (warm, a build restart in hand).
/// A bind the old code would also have passed would prove nothing.
#[test]
fn the_incidents_cold_only_rule_is_caught_by_the_bind() {
    let model = harness_model_ladder_model();
    let cold_only = |s: &Vars| s["cold"] == 1;
    let caught: Vec<Vars> = reachable(&model)
        .into_iter()
        .filter(|s| s["moved"] == 0 && s["unknown"] == 0)
        .filter(|s| cold_only(s) != !model.successors("VisitMoves", s).is_empty())
        .collect();
    assert!(
        !caught.is_empty(),
        "the cold-only rule agrees with the model everywhere"
    );
    assert!(
        caught.iter().any(|s| s["cold"] == 0 && s["restart"] == 1),
        "the incident (warm, a build restart in hand) is not among the catches: {caught:?}"
    );
    // And the real ladder is NOT caught there.
    for s in &caught {
        assert_eq!(
            real_moves(s),
            !model.successors("VisitMoves", s).is_empty(),
            "{s:?}"
        );
    }
}
