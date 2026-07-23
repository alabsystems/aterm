// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 and registry locks for the derived `BudgetedSearchResume` machine.
//!
//! The machine is the owner-level lifecycle twin of
//! `Terminal::search_budgeted`: a valid cursor preserves `search_id`; every fresh
//! or restarted logical stream exposes `reset` and a new ID; scan completion can
//! precede result-delta completion; and only the final drain retires the cursor.

use aterm_spec::derive::budgeted_search_resume_model;
use aterm_spec::{interp, verify};

#[test]
fn budgeted_search_resume_proves_and_catches_stale_continuation() {
    verify::prove_and_catch_tiered(
        &budgeted_search_resume_model(),
        "derived BudgetedSearchResume spec (identity/reset/delta-drain lifecycle)",
    );
}

#[test]
fn budgeted_search_resume_is_registered_for_global_verification() {
    let registered: std::collections::BTreeSet<_> = aterm_spec::xref::model_registry()
        .into_iter()
        .map(|model| model.name)
        .collect();
    assert!(
        registered.contains("BudgetedSearchResume"),
        "BudgetedSearchResume must resolve through the global spec registry"
    );
}

#[test]
fn budgeted_search_resume_action_set_is_pinned() {
    let model = budgeted_search_resume_model();
    let mut names: Vec<_> = model.actions.iter().map(|action| action.name).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "Cancel",
            "ContentRestart",
            "Drain",
            "DrainComplete",
            "FinishScan",
            "ForgedRestart",
            "QueryRestart",
            "RestartComplete",
            "Resume",
            "Start",
            "StartBacklog",
            "StartComplete",
            "Supersede",
        ],
        "BudgetedSearchResume action set drifted; update Tier-1 and this pin together"
    );
}

fn assert_invariants(
    model: &aterm_spec::derive::Model,
    state: &std::collections::BTreeMap<&'static str, i64>,
) {
    for invariant in [
        "LifecycleShape",
        "CursorMatchesSearchId",
        "ResetMintsFresh",
        "ResetStartsAtBeginning",
        "DeliveryShape",
        "IdentityIsLatest",
        "ValuesBounded",
    ] {
        assert!(
            model.check_invariant(invariant, state),
            "{invariant} violated at {state:?}"
        );
    }
}

fn fire(
    model: &aterm_spec::derive::Model,
    state: &mut std::collections::BTreeMap<&'static str, i64>,
    action: &str,
) {
    assert!(model.fire(action, state), "{action} must be enabled");
    assert_invariants(model, state);
}

#[test]
fn executable_model_walks_every_public_lifecycle_branch() {
    let model = budgeted_search_resume_model();
    let mut state = model.init_state();

    fire(&model, &mut state, "Start");
    assert_eq!(
        (
            state["live"],
            state["progress"],
            state["total"],
            state["scan_done"],
            state["complete"],
            state["cursor"],
            state["search_id"],
            state["issued"],
            state["reset"],
            state["delivery"],
        ),
        (1, 1, 3, 0, 0, 1, 1, 1, 1, 0),
        "a fresh partial call opens a reset-marked logical stream"
    );

    fire(&model, &mut state, "Resume");
    assert_eq!(
        (
            state["live"],
            state["progress"],
            state["cursor"],
            state["search_id"],
            state["reset"],
        ),
        (1, 2, 1, 1, 0),
        "valid resume preserves cursor/search identity and clears reset"
    );

    for action in [
        "ContentRestart",
        "QueryRestart",
        "ForgedRestart",
        "Supersede",
    ] {
        let old_id = state["search_id"];
        let old_issued = state["issued"];
        fire(&model, &mut state, action);
        assert_eq!(state["progress"], 1, "{action} resets progress");
        assert_eq!(state["issued"], old_issued + 1, "{action} mints once");
        assert_eq!(
            (
                state["reset"],
                state["prior_id"],
                state["cursor"],
                state["search_id"],
            ),
            (1, old_id, old_issued + 1, old_issued + 1),
            "{action} resets the host stream and retires the old identity"
        );
    }

    fire(&model, &mut state, "Cancel");
    assert_eq!(
        (
            state["live"],
            state["progress"],
            state["total"],
            state["complete"],
            state["cursor"],
            state["search_id"],
            state["issued"],
        ),
        (0, 0, 0, 0, 0, 0, 5),
        "cancel retires public state without reusing issued identities"
    );

    fire(&model, &mut state, "Start");
    fire(&model, &mut state, "Resume");
    let completing_id = state["search_id"];
    fire(&model, &mut state, "FinishScan");
    assert_eq!(
        (
            state["live"],
            state["progress"],
            state["complete"],
            state["cursor"],
            state["search_id"],
            state["reset"],
        ),
        (0, 3, 1, 0, completing_id, 0),
        "valid sparse completion retires cursor but retains search_id"
    );

    fire(&model, &mut state, "Start");
    let superseded_id = state["search_id"];
    fire(&model, &mut state, "RestartComplete");
    assert_eq!(
        (
            state["live"],
            state["complete"],
            state["cursor"],
            state["reset"],
            state["prior_id"],
            state["search_id"],
        ),
        (0, 1, 0, 1, superseded_id, 8),
        "one-turn restart has no cursor but still exposes reset + fresh search_id"
    );

    fire(&model, &mut state, "StartBacklog");
    let dense_id = state["search_id"];
    assert_eq!(
        (
            state["progress"],
            state["scan_done"],
            state["complete"],
            state["delivery"],
        ),
        (3, 1, 0, 1),
        "all rows may be scanned while result delivery remains incomplete"
    );
    fire(&model, &mut state, "Drain");
    assert_eq!(
        (
            state["progress"],
            state["cursor"],
            state["search_id"],
            state["delivery"],
        ),
        (3, dense_id, dense_id, 2),
        "drain-only resume preserves rows and identity"
    );
    fire(&model, &mut state, "DrainComplete");
    assert_eq!(
        (
            state["progress"],
            state["complete"],
            state["cursor"],
            state["search_id"],
            state["delivery"],
        ),
        (3, 1, 0, dense_id, 3),
        "only the final delta drain retires the cursor"
    );

    fire(&model, &mut state, "StartComplete");
    assert_eq!(
        (
            state["complete"],
            state["cursor"],
            state["search_id"],
            state["reset"],
            state["issued"],
        ),
        (1, 0, 10, 1, 10),
        "a fresh sparse search may also complete in one turn"
    );
}

#[test]
fn stale_resume_mutant_is_observably_rejected() {
    let model = interp::with_consts(&budgeted_search_resume_model(), &[("Buggy", 1)]);
    let mut state = model.init_state();
    assert!(model.fire("Start", &mut state));
    assert!(model.fire("Resume", &mut state));
    assert!(model.fire("ContentRestart", &mut state));
    assert!(
        !model.check_invariant("ResetMintsFresh", &state),
        "negative control: reset cannot preserve the stale logical identity"
    );
    assert!(
        !model.check_invariant("ResetStartsAtBeginning", &state),
        "negative control: partial restart cannot preserve stale row progress"
    );
}

#[test]
fn delivery_backlog_cannot_complete_or_stutter_early() {
    let model = budgeted_search_resume_model();
    let mut state = model.init_state();
    assert!(model.fire("StartBacklog", &mut state));

    let mut premature = state.clone();
    premature.insert("live", 0);
    premature.insert("complete", 1);
    premature.insert("cursor", 0);
    assert!(
        !model.check_invariant("DeliveryShape", &premature),
        "negative control: scan completion cannot hide an undrained result backlog"
    );

    let mut stutter = state.clone();
    stutter.insert("reset", 0);
    stutter.insert("prior_id", 0);
    assert!(
        !model.successors("Drain", &state).contains(&stutter),
        "negative control: a drain-only resume must advance delivery progress"
    );
}
