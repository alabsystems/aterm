// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `TrustFactsReuse`: the whole-tree Trust lane's facts are judged only
//! from this tree's and this prover's measurements, against these floors — every one
//! held, whoever recorded it — a floors-only change reuses them, reused facts never
//! verify a floor lowered without a new reason or re-labelled the way the merge
//! contract refuses, and what the clock left undecided is measured again on its own
//! key. Tier-1 lives in `crates/aterm-release/tests/it/trust_facts_model.rs`.

use crate::walk::{committed_actions, walk};
use aterm_spec::derive::{Model, trust_facts_reuse_model};
use aterm_spec::verify;

fn answer(m: &Model, path: &[&str]) -> (i64, i64) {
    let s = walk(m, path);
    (s["found"], s["verdict"])
}

#[test]
fn trust_facts_prove_and_catch_every_defect_class() {
    let model = trust_facts_reuse_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the facts contract must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "trust facts reuse");
    assert!(
        verify::uncaught_invariants(&model).is_empty(),
        "ghost invariants: {:?}",
        verify::uncaught_invariants(&model)
    );
    // Every mutant is a reader or writer the committed machine never runs, live
    // alone and caught alone.
    let committed = committed_actions(&model);
    let mutants: Vec<&str> = model
        .actions
        .iter()
        .map(|action| action.name)
        .filter(|name| !committed.contains(name))
        .collect();
    assert_eq!(
        mutants,
        [
            "RemeasureAcrossKeys",
            "AssembleSansSource",
            "AssembleByTrustcCommit",
            "AssembleStoredVerdict",
            "AssembleWholeTree",
            "AssembleUncheckedReuse",
            "AssembleNewProverExcused",
            "AssembleLapseExcused",
            "AssembleRebaseUnreasoned",
        ]
    );
    assert_eq!(
        verify::audit_dead_negative_controls(&model, &mutants),
        Ok(mutants.len()),
        "{mutants:?}"
    );
}

/// The case this exists for, walked: a run's facts pass their floors; a
/// floors-only change that raises them finds the SAME facts and fails; lowering
/// back to the run's floor passes again, still from them; lowering BELOW the run's
/// floor by hand is refused although the facts are found — the merge contract's
/// up-only rule, held on the reuse path; a source change has nothing to reuse until
/// a run.
#[test]
fn a_floors_only_change_is_judged_again_from_the_same_facts() {
    let m = trust_facts_reuse_model();
    assert_eq!(
        answer(&m, &["EditSource", "RaiseFloor", "Run", "Assemble"]),
        (1, 1),
        "1 proved, floor 1: PASS"
    );
    assert_eq!(
        answer(
            &m,
            &["EditSource", "RaiseFloor", "Run", "RaiseFloor", "Assemble"]
        ),
        (1, 2),
        "the same facts, floor 2: FAIL, no run"
    );
    assert_eq!(
        answer(
            &m,
            &[
                "EditSource",
                "RaiseFloor",
                "Run",
                "RaiseFloor",
                "Assemble",
                "LowerFloor",
                "Assemble",
            ],
        ),
        (1, 1),
        "back to the run's floor: PASS, no run"
    );
    assert_eq!(
        answer(
            &m,
            &[
                "EditSource",
                "RaiseFloor",
                "RaiseFloor",
                "Run",
                "LowerFloor",
                "Assemble",
            ],
        ),
        (1, 2),
        "below the run's floor, by hand: found, and refused"
    );
    assert_eq!(
        answer(&m, &["Run", "EditSource", "Assemble"]),
        (0, 2),
        "another tree: nothing to reuse"
    );
    assert_eq!(
        answer(&m, &["Run", "SwapProver", "Assemble"]),
        (0, 2),
        "another prover: nothing to reuse"
    );
}

/// NO FLOOR EXCUSES, walked. A floor another prover recorded is HELD: 0 proved
/// under another prover's floor of 2 is refused. It goes down onto the measuring
/// prover from the same facts only with a reason; with none, it is refused however
/// far it went. Re-labelling the measuring prover's floor to another prover is
/// refused too.
#[test]
fn another_provers_floor_is_held_and_lowered_only_with_a_reason() {
    let m = trust_facts_reuse_model();
    assert_eq!(
        answer(
            &m,
            &[
                "RaiseFloor",
                "RaiseFloor",
                "RelabelFloor",
                "Run",
                "Assemble"
            ]
        ),
        (1, 2),
        "another prover's floor of 2 over 0 proved: held, and refused"
    );
    assert_eq!(
        answer(
            &m,
            &[
                "RaiseFloor",
                "RelabelFloor",
                "Run",
                "RebaseFloor",
                "Assemble"
            ]
        ),
        (1, 2),
        "re-based onto the measuring prover at what it measured, with no reason: refused"
    );
    assert_eq!(
        answer(
            &m,
            &[
                "RaiseFloor",
                "RelabelFloor",
                "Run",
                "RebaseFloorWithReason",
                "Assemble",
            ]
        ),
        (1, 1),
        "…and with its reason: PASS from the same facts"
    );
    assert_eq!(
        answer(
            &m,
            &[
                "RaiseFloor",
                "RaiseFloor",
                "RelabelFloor",
                "Run",
                "RebaseFloorWithReason",
                "Assemble",
            ]
        ),
        (1, 2),
        "a reason lowers the floor to 1, which 0 proved still do not meet"
    );
    assert_eq!(
        answer(&m, &["RaiseFloor", "Run", "RelabelFloor", "Assemble"]),
        (1, 2),
        "the measuring prover's floor re-labelled away: found, and refused"
    );
}

/// The clock, walked: a run whose lapsed budgets cost it the proof its floor needs
/// is not a pass; measuring it again on the same key is, from one record. A
/// re-measure after the tree moved has no record of its key to complete.
#[test]
fn a_shortfall_the_clock_may_have_caused_is_measured_again_not_excused() {
    let m = trust_facts_reuse_model();
    assert_eq!(
        answer(
            &m,
            &["EditSource", "RaiseFloor", "RunUnderLoad", "Assemble"]
        ),
        (1, 2),
        "0 proved with one lapse against a floor of 1: refused"
    );
    assert_eq!(
        answer(
            &m,
            &[
                "EditSource",
                "RaiseFloor",
                "RunUnderLoad",
                "Assemble",
                "Remeasure",
                "Assemble",
            ]
        ),
        (1, 1),
        "measured again on the same key: PASS"
    );
    let moved = walk(&m, &["EditSource", "RunUnderLoad", "EditSource"]);
    let mut again = moved.clone();
    assert!(
        !m.fire("Remeasure", &mut again),
        "another tree's record is never completed: {moved:?}"
    );
}
