// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State as ModelState;

fn reading(case: i64) -> Usage {
    let session = if case == 6 || case == 7 { 99.1 } else { 100.0 };
    let session = if case >= 8 { 99.1 } else { session };
    let week = if case == 5 || case >= 7 { 30.0 } else { 100.0 };
    let (session_reset, week_reset) = if case == 2 {
        (Some(NOW + 2 * HOUR), Some(NOW + HOUR))
    } else {
        (
            (case != 3).then_some(NOW + HOUR),
            (case != 4).then_some(NOW + 2 * HOUR),
        )
    };
    let mut windows = [
        aterm_json::json!({"usedPercent":session,"windowDurationMins":300,"resetsAt":session_reset}),
        aterm_json::json!({"usedPercent":week,"windowDurationMins":10080,"resetsAt":week_reset}),
    ];
    if case == 1 {
        windows.swap(0, 1);
    }
    let usage = super::super::super::codex_app_login::usage_of(
        &aterm_json::json!({
            "ordinaryUsageAllowed":case != 8,
            "rateLimits":{
                "rateLimitReachedType":(case == 9).then_some("workspace_member_credits_depleted"),
                "primary":windows[0],"secondary":windows[1]
            }
        }),
        NOW,
    )
    .unwrap();
    let expected = match case {
        0..=4 => vec![
            (accounts::SESSION_LIMIT, session_reset),
            (accounts::WEEK_LIMIT, week_reset),
        ],
        5 | 8 | 9 => vec![(accounts::SESSION_LIMIT, session_reset)],
        6 => vec![(accounts::WEEK_LIMIT, week_reset)],
        7 => Vec::new(),
        _ => unreachable!(),
    };
    assert_eq!(usage.walls, expected, "case {case}: keep each exact reset");
    let backend_window = |window: &aterm_json::Value| {
        aterm_json::json!({
            "used_percent":window["usedPercent"],
            "limit_window_seconds":window["windowDurationMins"].as_i64().unwrap() * 60,
            "reset_at":window["resetsAt"]
        })
    };
    let backend = super::super::super::codex_app_login::usage_from_backend(
        &aterm_json::json!({
            "rate_limit":{
                "allowed":case != 8,
                "primary_window":backend_window(&windows[0]),
                "secondary_window":backend_window(&windows[1])
            },
            "rate_limit_reached_type":(case == 9).then_some("workspace_member_credits_depleted")
        }),
        NOW,
    )
    .unwrap();
    assert_eq!(usage, backend, "case {case}: backend normalization");
    usage
}

fn mask<'a>(names: impl Iterator<Item = &'a str>) -> i64 {
    names.fold(0, |mask, name| {
        mask | match name {
            accounts::SESSION_LIMIT => 1,
            accounts::WEEK_LIMIT => 2,
            other => panic!("unexpected wall {other}"),
        }
    })
}

fn recorded(case: i64, first_only: bool) -> (i64, i64) {
    let world = World::new("quota-walls");
    world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let mut usage = reading(case);
    if first_only {
        usage.walls.truncate(1);
    }
    let roster = world.seat().bound_roster(Cli::Codex).unwrap();
    let enrolled = accounts::enrolled_in(Cli::Codex, &roster, &world.home);
    record_usage(
        &world.seat(),
        Cli::Codex,
        &roster,
        &enrolled,
        &|_: Cli, _: &Path, _: Option<&str>, _: i64| Some(usage.clone()),
    )
    .unwrap();
    let state = world.load(Cli::Codex);
    assert_eq!(
        state
            .account("main")
            .walls
            .iter()
            .map(|(name, wall)| (name.as_str(), wall.until))
            .collect::<std::collections::BTreeMap<_, _>>(),
        usage
            .walls
            .iter()
            .copied()
            .collect::<std::collections::BTreeMap<_, _>>(),
        "each persisted wall keeps its own reset, including None"
    );
    let stored = mask(state.account("main").walls.keys().map(String::as_str));
    let blocked = if case < 3 {
        let later = Seat {
            now: NOW + HOUR + accounts::RESET_GRACE_S + 1,
            ..world.seat()
        };
        // Reload and attempt a real selection while both live usage reads are unavailable.
        let result = launch_env(&later, Cli::Codex, &no_usage);
        if first_only && case == 0 {
            assert!(
                result.is_some(),
                "negative control must actually launch an exhausted account"
            );
        }
        let reloaded = later.store(Cli::Codex).load(later.now).unwrap().0;
        i64::from(
            result.is_none()
                && accounts::condition(roster.row("main").unwrap(), &reloaded, true, later.now)
                    == Condition::Limited {
                        until: NOW + 2 * HOUR + accounts::RESET_GRACE_S,
                    },
        )
    } else {
        0
    };
    (stored, blocked)
}

pub(crate) fn quota_transition(state: &ModelState, action: &str) -> ModelState {
    let mut next = state.clone();
    match action {
        "Case" => {
            next.insert("case", state["case"] + 1);
        }
        "Parse" => {
            let usage = reading(state["case"]);
            next.insert("phase", 1);
            next.insert("parsed", mask(usage.walls.iter().map(|(name, _)| *name)));
        }
        "Record" => {
            let (stored, blocked) = recorded(state["case"], false);
            next.insert("phase", 2);
            next.insert("stored", stored);
            next.insert("blocked", blocked);
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn simultaneous_quota_walls_conform_through_durable_reload_and_unread_launch() {
    let model = aterm_models_harness::codex_usage_walls_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = quota_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    // Removing all but the first wall reproduces the old first-match parser and
    // a persistence loop that drops the second wall. Both must violate the model.
    let mut state = model.init_state();
    state = quota_transition(&state, "Parse");
    let (stored, blocked) = recorded(0, true);
    state.insert("phase", 2);
    state.insert("stored", stored);
    state.insert("blocked", blocked);
    assert!(!model.check_invariant("AllWallsPersist", &state));
    assert!(!model.check_invariant("LaterResetStillBlocks", &state));
    state.insert("parsed", stored);
    assert!(!model.check_invariant("ExactParsing", &state));
}
