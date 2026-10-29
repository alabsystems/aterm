// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn trial(case: i64, action: &str) -> [i64; 5] {
    let world = World::new("canary-admission");
    let cli = Cli::Codex;
    world.two(cli);
    world.on_main(cli, &[NOW - MIN]);
    let store = world.seat().store(cli);
    store
        .transact(NOW, &mut None, |state| {
            state.active = Some("work".into());
            state.proven = case == 3;
            state.canary = (!matches!(case, 1 | 3)).then(|| accounts::Canary {
                sid: "silent".into(),
                at: if case == 2 {
                    NOW
                } else {
                    NOW - accounts::CANARY_HOLD_S - 1
                },
                took: false,
            });
            state.undo = vec![accounts::Undo::default()];
            Ok(Vec::new())
        })
        .unwrap();
    let mut s = session(cli, Supervisor::Aterm, Mode::Early, Path::new("/opt/codex"));
    s.sid = "first";
    s.sample = None;
    s.point = Point::Wall {
        window: "five_hour".into(),
        at: None,
    };
    let first = super::super::session_plan(&world.seat(), &s, |note| note.record());
    s.sid = "second";
    let second = super::super::session_plan(&world.seat(), &s, |note| note.record());
    assert_eq!(first.advice(), second.advice());
    match case {
        2 => assert_eq!(*first.advice(), Advice::Stay(StayWhy::Canary)),
        _ => assert!(matches!(first.advice(), Advice::Move {
            to, rotates: false, takes_canary, ..
        } if to == "work" && *takes_canary == (case != 3))),
    }
    let before = store.load(NOW).unwrap().0;
    assert!(before.canary.as_ref().is_none_or(|c| c.sid == "silent"));
    let attempt = matches!(action, "First" | "Second" | "DropLate");
    let now = if attempt && case == 4 {
        store
            .transact(NOW, &mut None, |state| {
                state.canary = Some(accounts::Canary {
                    sid: "replacement".into(),
                    at: NOW,
                    took: true,
                });
                Ok(Vec::new())
            })
            .unwrap();
        NOW + accounts::CANARY_HOLD_S + 1
    } else {
        NOW
    };
    let prior = store.load(now).unwrap().0;
    let first = if attempt {
        first.commit(now).line.is_some()
    } else {
        drop(first);
        false
    };
    let claimed = store.load(now).unwrap().0;
    if first && case <= 1 {
        assert_eq!(claimed.canary.as_ref().unwrap().sid, "first");
        assert!(claimed.canary.as_ref().unwrap().took);
        assert!(claimed.canary_holds(now));
        assert!(claimed.undo.is_empty());
    } else {
        assert_eq!(claimed.canary, prior.canary);
    }
    let second = if action == "Second" {
        second.commit(now).line.is_some()
    } else {
        drop(second);
        false
    };
    let after = store.load(now).unwrap().0;
    assert_eq!(after.rotations, before.rotations);
    let owner = match after.canary.as_ref().map(|c| c.sid.as_str()) {
        Some("first") => 1,
        Some("second") => 2,
        Some("replacement") => 3,
        _ => 0,
    };
    let events: Vec<_> = store
        .tail(100)
        .unwrap()
        .into_iter()
        .filter(|event| {
            event.kind == accounts::Kind::Rotate
                && matches!(event.sid.as_deref(), Some("first" | "second"))
        })
        .collect();
    for event in &events {
        assert_eq!(event.account, "work");
        assert_eq!(event.to.as_deref(), Some("work"));
        assert!(event.why.starts_with("canary:"));
    }
    [
        i64::from(first),
        i64::from(second),
        owner,
        i64::try_from(events.len()).unwrap(),
        0,
    ]
}

pub(crate) fn canary_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    if action == "AnotherCase" {
        next.insert("case", state["case"] + 1);
        return next;
    }
    let native = if action == "Drop" {
        if state["phase"] == 1 {
            "DropEarly"
        } else {
            "DropLate"
        }
    } else {
        action
    };
    for (key, value) in ["first", "second", "owner", "audit", "budget"]
        .into_iter()
        .zip(trial(state["case"], native))
    {
        next.insert(key, value);
    }
    next.insert(
        "phase",
        match action {
            "Plan" => 1,
            "First" => 2,
            _ => 3,
        },
    );
    if action == "First" {
        next.insert("attempted", 1);
    }
    next
}

#[test]
fn a_canary_takeover_is_claimed_before_releasing_a_prepared_command() {
    assert_eq!(trial(0, "Second"), [1, 0, 1, 1, 0]);
}

#[test]
fn canary_commands_conform_to_the_admission_model() {
    let model = aterm_models_harness::account_pool_canary_model();
    assert_eq!(model.mutant_dials(), (1..=10).collect::<Vec<_>>());
    aterm_spec::interp::prove_and_catch(&model);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = canary_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut bad = canary_transition(&model.init_state(), "Plan");
    bad.insert("first", 1);
    assert!(!model.check_invariant("Planning", &bad));
    let mut good = canary_transition(&model.init_state(), "First");
    good.insert("owner", 0);
    assert!(!model.check_invariant("CommandOwner", &good));
    for case in [2, 4] {
        let mut state = model.init_state();
        state.insert("case", case);
        let mut denied = canary_transition(&state, "First");
        denied.insert("first", 1);
        assert!(!model.check_invariant("Eligible", &denied));
    }
}
