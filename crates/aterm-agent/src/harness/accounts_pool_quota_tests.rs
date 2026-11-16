// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State as ModelState;

/// The model's case `case`: 0 to 4 both windows spent (slots swapped, resets in either order or
/// one missing), 5 the five-hour window alone, 6 the week beside a five-hour share that rounds up
/// to 100% unspent, 7 nothing spent or refused, 8 an ordinary refusal and 9 a workspace's credits
/// beside that rounded share, 10 a reached spend control and 11 a workspace's usage limit beside
/// roomy windows (the five-hour window, then the week, the more used).
fn reading(case: i64) -> Usage {
    let session = match case {
        6..=9 => 99.1,
        10 => 20.0,
        11 => 10.0,
        _ => 100.0,
    };
    let week = match case {
        5 | 7..=9 => 30.0,
        10 => 10.0,
        11 => 60.0,
        _ => 100.0,
    };
    let reached = match case {
        9 => Some("workspace_member_credits_depleted"),
        11 => Some("workspace_owner_usage_limit_reached"),
        _ => None,
    };
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
                "rateLimitReachedType":reached,
                "spendControlReached":(case == 10).then_some(true),
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
        10 => vec![(accounts::SESSION_LIMIT, None)],
        11 => vec![(accounts::WEEK_LIMIT, None)],
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
            "rate_limit_reached_type":reached,
            "spend_control":(case == 10).then(|| aterm_json::json!({"reached":true}))
        }),
        NOW,
    )
    .unwrap();
    assert_eq!(usage, backend, "case {case}: backend normalization");
    usage
}

/// Whether a wall of `usage` names a reset of a window its sample shows unspent: a turnover that
/// says nothing of when the wall ends ([`accounts::Sample::reached_reset`]).
fn borrowed(usage: &Usage) -> i64 {
    i64::from(
        usage.walls.iter().any(|&(window, reset)| {
            reset.is_some() && usage.sample.reached_reset(window).is_none()
        }),
    )
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
            next.insert("borrowed", borrowed(&usage));
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
    // The old fallback borrowed the most-used window's turnover: a spend control's wall beside
    // roomy windows ended at the five-hour reset. That reading must violate the model too.
    for case in [10, 11] {
        let mut old = reading(case);
        let window = old.fallback_window();
        let turnover = if window == accounts::WEEK_LIMIT {
            old.sample.week_reset
        } else {
            old.sample.session_reset
        };
        old.walls = vec![(window, turnover)];
        let mut state = model.init_state();
        state.insert("case", case);
        state = quota_transition(&state, "Parse");
        assert!(
            model.check_invariant("NoBorrowedReset", &state),
            "case {case}"
        );
        state.insert("borrowed", borrowed(&old));
        assert!(
            !model.check_invariant("NoBorrowedReset", &state),
            "case {case}: a borrowed reset"
        );
    }
}
