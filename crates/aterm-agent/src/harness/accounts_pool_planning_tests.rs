// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn trial(case: i64, action: &str) -> [i64; 7] {
    let timed = trial_with_stamp(case, action, true);
    if case >= 9 {
        assert_eq!(trial_with_stamp(case, action, false), timed);
    }
    timed
}

fn trial_with_stamp(case: i64, action: &str, stamped: bool) -> [i64; 7] {
    let world = World::new("planning");
    let cli = Cli::Codex;
    world.two(cli);
    world.on_main(cli, &[]);
    let store = world.seat().store(cli);
    store
        .transact_logins(NOW, &mut None, |_, logins| {
            for label in ["main", "work"] {
                logins
                    .labels
                    .insert(label.into(), accounts::Binding::default());
            }
            Ok(Vec::new())
        })
        .unwrap();
    let mut s = session(cli, Supervisor::Aterm, Mode::Early, Path::new("/opt/codex"));
    if case >= 9 {
        s.point = Point::Wall {
            window: "five_hour".into(),
            at: stamped.then_some(NOW),
        };
    }
    let plan = super::super::session_plan(&world.seat(), &s, |note| note.record());
    let debug = format!("{plan:?}");
    assert!(!debug.contains(plan.prepared.as_ref().unwrap().line.as_str()));
    assert!(!debug.contains("/opt/codex"));
    assert!(matches!(plan.advice(), Advice::Move { to, rotates: true, .. } if to == "work"));
    let after_plan = world.load(cli);
    assert_eq!(after_plan.active.as_deref(), Some("main"));
    assert!(after_plan.proven);
    assert!(after_plan.canary.is_none());
    assert!(after_plan.rotations.is_empty());
    assert!(after_plan.accounts["main"].sample.is_some());
    let change = !matches!(action, "Plan" | "DropEarly");
    if change {
        match case {
            1 => {
                std::fs::write(
                    world.home.join(".codex/config.toml"),
                    "cli_auth_credentials_store = 'keyring'\n",
                )
                .unwrap();
            }
            2 => {
                std::fs::write(
                    world.cfg.join("aterm.toml"),
                    "[harness]\naccounts = 'off'\n",
                )
                .unwrap();
            }
            3 => {
                let mut roster = world.seat().roster(cli).unwrap();
                roster
                    .rows
                    .iter_mut()
                    .find(|r| r.label == "work")
                    .unwrap()
                    .email = Some("changed@example.test".into());
                std::fs::write(
                    world.cfg.join(cli.accounts().roster_file),
                    accounts::append_rows_for(cli, "", &roster.rows).unwrap(),
                )
                .unwrap();
            }
            4 | 5 => {
                store
                    .transact_logins(NOW, &mut None, |_, logins| {
                        // The store allocates a new generation even at the same timestamp and path.
                        logins
                            .labels
                            .get_mut(if case == 4 { "main" } else { "work" })
                            .unwrap()
                            .renewal_generation += 1;
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            6 => {
                let other = super::super::session_plan(&world.seat(), &s, |note| note.record());
                assert!(other.commit(NOW).line.is_some());
            }
            11 | 12 => {
                store
                    .transact(NOW + MIN, &mut None, |state| {
                        Ok(accounts::note_wall(
                            state,
                            "main",
                            if case == 11 { "seven_day" } else { "five_hour" },
                            (case == 12).then_some(NOW + HOUR),
                            NOW + MIN,
                            NOW + MIN,
                        ))
                    })
                    .unwrap();
            }
            _ => {}
        }
    }
    let now = match case {
        7 if change => NOW + 4 * HOUR,
        8 if change => NOW - 1,
        9 | 11 | 12 if change => NOW + accounts::NO_RESET_WALL_S[0],
        10 if change => NOW + accounts::NO_RESET_WALL_S[0] - 1,
        _ => NOW,
    };
    let before = store.load(now).unwrap().0;
    let committed = if action == "Finish" {
        plan.commit(now).line.is_some()
    } else {
        drop(plan);
        false
    };
    let after = store.load(now).unwrap().0;
    if !committed {
        assert_eq!(after.active, before.active);
        assert_eq!(after.canary, before.canary);
        assert_eq!(after.rotations, before.rotations);
    } else {
        assert_eq!(after.canary.as_ref().unwrap().at, now);
    }
    [
        i64::from(committed),
        i64::try_from(after.rotations.len()).unwrap(),
        i64::from(after.active.as_deref() == Some("work")),
        i64::from(after.canary.is_some()),
        i64::from(after.accounts["main"].sample.is_some()),
        i64::from(committed),
        i64::from(change && case == 6),
    ]
}

pub(crate) fn planning_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    if action == "AnotherCase" {
        next.insert("case", state["case"] + 1);
        return next;
    }
    let native = if action == "Drop" && state["phase"] == 1 {
        "DropEarly"
    } else {
        action
    };
    for (key, value) in [
        "committed",
        "count",
        "active",
        "canary",
        "evidence",
        "line",
        "external",
    ]
    .into_iter()
    .zip(trial(state["case"], native))
    {
        next.insert(key, value);
    }
    next.insert(
        "phase",
        match action {
            "Plan" => 1,
            "Change" => 2,
            _ => 3,
        },
    );
    if action == "Finish" {
        next.insert("finished", 1);
    }
    next
}

#[test]
fn planning_conforms_without_allocating_a_rotation() {
    let model = aterm_models_harness::account_pool_planning_model();
    assert_eq!(model.mutant_dials(), (1..=17).collect::<Vec<_>>());
    aterm_spec::interp::prove_and_catch(&model);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = planning_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut state = model.init_state();
    let mut planned = planning_transition(&state, "Plan");
    planned.insert("count", 1);
    assert!(!model.check_invariant("Planning", &planned));
    for case in (1..=7).chain([9]) {
        state.insert("case", case);
        let mut denied = planning_transition(&state, "Finish");
        assert_eq!(denied["committed"], 0);
        denied.insert("committed", 1);
        assert!(!model.check_invariant("Admission", &denied));
    }
}

#[test]
fn a_nonmoving_plan_commits_without_touching_the_store_again() {
    let world = World::new("planning-off");
    let cli = Cli::Codex;
    world.two(cli);
    world.on_main(cli, &[]);
    std::fs::write(
        world.cfg.join("aterm.toml"),
        "[harness]\naccounts = 'off'\n",
    )
    .unwrap();
    let s = session(cli, Supervisor::Aterm, Mode::Off, Path::new("/opt/codex"));
    let plan = super::super::session_plan(&world.seat(), &s, |note| note.record());
    assert!(!matches!(plan.advice(), Advice::Move { .. }));
    let store = world.seat().store(cli);
    let paths = [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
    ];
    let before = paths.each_ref().map(|p| std::fs::read(p).ok());
    assert!(plan.commit(NOW + HOUR).line.is_none());
    assert_eq!(paths.each_ref().map(|p| std::fs::read(p).ok()), before);
}
