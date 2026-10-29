// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;

fn key(value: i64) -> VaultContext {
    [u8::try_from(value).unwrap(); 32]
}

/// Project actual cache operations; `needed` records an observed invalidation obligation.
pub(crate) fn transition(state: &State, action: &str) -> State {
    let mut next = state.clone();
    match action {
        "ContextA" | "ContextB" | "Unreadable" => {
            next.insert(
                "context",
                match action {
                    "ContextA" => 1,
                    "ContextB" => 2,
                    _ => 0,
                },
            );
            next.insert("result", 0);
            next.insert("moved", 0);
            return next;
        }
        "Expire" => {
            next.insert("expired", 1);
            next.insert("result", 0);
            next.insert("moved", 0);
            return next;
        }
        _ => {}
    }
    let start = Instant::now();
    let now = if state["expired"] == 1 {
        start + BACKOFF_MAX + Duration::from_secs(1)
    } else {
        start
    };
    let mut ladder = Ladder::default();
    if state["phase"] > 0 {
        ladder.vault = Some(VaultRun {
            context: key(state["origin"]),
            invalidated: state["dirty"] == 1,
            at: start,
            wait: if state["phase"] == 2 {
                BACKOFF_FIRST
            } else {
                Duration::ZERO
            },
            seen: (state["phase"] == 2).then_some(Vault::Unchanged(7)),
        });
    }
    match action {
        "Lookup" => {
            let context = (state["context"] > 0).then(|| key(state["context"]));
            let result = ladder.vault_turn(context, now);
            let code = match result {
                VaultTurn::Due => 1,
                VaultTurn::Running => 2,
                VaultTurn::Seen(_) => 3,
                VaultTurn::Unavailable => 4,
            };
            next.insert("result", code);
            next.insert("moved", 0);
            if state["phase"] == 1 && state["context"] != state["origin"] {
                next.insert("needed", 1);
            } else if code == 1 {
                next.insert("needed", 0);
            }
            next.insert(
                "jobs",
                if code == 1 {
                    state["jobs"] + 1
                } else if code == 2 {
                    state["jobs"]
                } else {
                    0
                },
            );
        }
        "Complete" => {
            ladder.vault_done(Vault::Unchanged(7));
            next.insert("jobs", 0);
            next.insert("result", 0);
            next.insert("moved", 0);
        }
        "Move" => {
            ladder.moved(now);
            next.insert(
                "jobs",
                i64::from(ladder.vault.as_ref().is_some_and(|run| run.seen.is_none())),
            );
            next.insert("needed", 1);
            next.insert("result", 0);
            next.insert("moved", 1);
        }
        _ => panic!("unexpected cache action {action}"),
    }
    if let Some(run) = &ladder.vault {
        next.insert("phase", if run.seen.is_some() { 2 } else { 1 });
        next.insert("origin", i64::from(run.context[0]));
        next.insert("dirty", i64::from(run.invalidated));
        next.insert(
            "expired",
            i64::from(now.saturating_duration_since(run.at) >= run.wait && run.seen.is_some()),
        );
    } else {
        next.insert("phase", 0);
        next.insert("dirty", 0);
        next.insert("expired", 0);
    }
    next
}

#[test]
fn cache_transitions_conform_to_observed_vault_context() {
    let model = aterm_models_harness::vault_refresh_context_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(model.mutant_dials(), (1..=7).collect::<Vec<_>>());
    let states = aterm_spec::interp::reachable_states(&model);
    for state in &states {
        for action in &model.actions {
            if model.action_enabled(action.name, state) {
                let next = transition(state, action.name);
                assert!(
                    model.successors(action.name, state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let check =
        |requirements: &[(&str, i64)], action: &str, changes: &[(&'static str, i64)], law: &str| {
            let before = states
                .iter()
                .find(|state| {
                    requirements
                        .iter()
                        .all(|(name, value)| state[*name] == *value)
                })
                .unwrap();
            let mut bad = transition(before, action);
            for (field, value) in changes {
                bad.insert(field, *value);
            }
            assert!(
                !model.successors(action, before).contains(&bad),
                "negative projection still conforms: {law}"
            );
            assert!(!model.check_invariant(law, &bad), "negative control: {law}");
        };
    check(
        &[("phase", 0), ("context", 1)],
        "Lookup",
        &[("jobs", 2)],
        "OneRefresh",
    );
    check(
        &[("phase", 1), ("context", 1)],
        "Lookup",
        &[("result", 3)],
        "CompleteBeforeReuse",
    );
    check(
        &[("phase", 2), ("origin", 1), ("context", 2)],
        "Lookup",
        &[("phase", 2), ("origin", 1), ("result", 3)],
        "SameContext",
    );
    check(
        &[("phase", 2), ("expired", 1)],
        "Lookup",
        &[("phase", 2), ("expired", 1), ("result", 3)],
        "FreshBeforeReuse",
    );
    check(
        &[("phase", 2), ("needed", 1)],
        "Lookup",
        &[("phase", 2), ("needed", 1), ("result", 3)],
        "ObservedChangeInvalidates",
    );
    check(
        &[("phase", 0), ("context", 0)],
        "Lookup",
        &[("result", 1)],
        "ReadableBeforeStart",
    );
    check(
        &[("phase", 2)],
        "Move",
        &[("phase", 2), ("dirty", 0)],
        "MovementInvalidates",
    );
}

#[test]
fn observed_aba_waits_for_the_old_refresh_then_discards_it() {
    let mut ladder = Ladder::default();
    let now = Instant::now();
    assert_eq!(ladder.vault_turn(Some(key(1)), now), VaultTurn::Due);
    assert_eq!(ladder.vault_turn(Some(key(2)), now), VaultTurn::Running);
    assert_eq!(ladder.vault_turn(Some(key(1)), now), VaultTurn::Running);
    ladder.vault_done(Vault::Unchanged(7));
    assert_eq!(ladder.vault_result(key(1), now), None);
    assert_eq!(ladder.vault_turn(Some(key(1)), now), VaultTurn::Due);
    assert_eq!(ladder.vault.as_ref().unwrap().wait, Duration::ZERO);
    ladder.vault_done(Vault::Unchanged(8));
    assert_eq!(
        ladder.vault_turn(Some(key(1)), now),
        VaultTurn::Seen(Vault::Unchanged(8))
    );
}

#[test]
fn an_unreadable_context_neither_reuses_nor_overlaps_a_refresh() {
    let mut ladder = Ladder::default();
    let now = Instant::now();
    assert_eq!(ladder.vault_turn(None, now), VaultTurn::Unavailable);
    assert_eq!(ladder.vault_turn(Some(key(1)), now), VaultTurn::Due);
    assert_eq!(ladder.vault_turn(None, now), VaultTurn::Running);
    ladder.vault_done(Vault::Unchanged(7));
    assert_eq!(ladder.vault_turn(None, now), VaultTurn::Unavailable);
    assert_eq!(ladder.vault_turn(Some(key(1)), now), VaultTurn::Due);
}

#[test]
fn a_completion_without_a_pending_refresh_creates_no_cache() {
    let mut ladder = Ladder::default();
    ladder.vault_done(Vault::Unchanged(7));
    assert!(ladder.vault.is_none());
}

#[test]
fn a_fresh_vault_after_absence_forgets_other_rows_waits() {
    let mut ladder = Ladder::default();
    let now = Instant::now();
    assert_eq!(ladder.vault_turn(None, now), VaultTurn::Unavailable);
    ladder.failed("m2", "unreadable", now);
    assert!(ladder.held("m2", "unreadable", now));
    // A second row starts the now-readable vault. The first row must still observe
    // a return to unreadable, rather than its old wait hiding that invalidation.
    assert_eq!(ladder.vault_turn(Some(key(2)), now), VaultTurn::Due);
    assert!(!ladder.held("m2", "unreadable", now));
    assert_eq!(ladder.vault_turn(None, now), VaultTurn::Running);
    ladder.vault_done(Vault::Unchanged(7));
    assert_eq!(ladder.vault_result(key(2), now), None);
    assert_eq!(ladder.vault_turn(Some(key(2)), now), VaultTurn::Due);
}

#[test]
fn a_post_refresh_context_change_forgets_other_rows_waits() {
    let mut ladder = Ladder::default();
    let now = Instant::now();
    assert_eq!(ladder.vault_turn(Some(key(1)), now), VaultTurn::Due);
    ladder.vault_done(Vault::Unchanged(7));
    ladder.failed("m2", "source A", now);
    assert!(ladder.held("m2", "source A", now));
    ladder.invalidate_vault();
    assert!(!ladder.held("m2", "source A", now));
    assert_eq!(ladder.vault_turn(Some(key(2)), now), VaultTurn::Due);
    assert!(!ladder.held("m2", "source A", now));
}
