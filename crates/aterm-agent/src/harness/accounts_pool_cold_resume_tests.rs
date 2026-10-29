// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn trial(case: i64) -> [i64; 3] {
    use crate::harness::codex_alab;
    let world = World::new("cold-capability");
    let work = world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let on_work = case >= 5 && case != 10;
    if !on_work || case == 9 {
        std::fs::remove_file(world.home.join(".codex/auth.json")).unwrap();
    }
    if case == 5 {
        wall(&world, Cli::Codex, "work", NOW + HOUR);
    }
    if matches!(case, 8 | 12) {
        wall(&world, Cli::Codex, "main", NOW + HOUR);
    }
    if matches!(case, 7 | 10) {
        pin(&world, Cli::Codex, "work").unwrap();
    } else if case == 12 {
        pin(&world, Cli::Codex, "main").unwrap();
    }
    let supported = matches!(case, 0 | 10 | 11);
    let program = codex_alab::lay_package(&world.root.join("package"), supported || case == 4);
    if case == 2 {
        std::fs::remove_file(world.root.join("package/alab-build.json")).unwrap();
    } else if case == 3 {
        std::fs::remove_file(&program).unwrap();
    } else if case == 4 {
        assert_eq!(codex_alab::auth_path_unread(&program), None);
        let _ = codex_alab::lay_package(&world.root.join("package"), false);
    }
    let auth = work.join("auth.json").to_str().unwrap().to_owned();
    let env = if on_work {
        vec![(codex_alab::AUTH_PATH_ENV, auth.as_str())]
    } else {
        Vec::new()
    };
    let store = world.seat().store(Cli::Codex);
    let before = (
        store.load(NOW).unwrap().0,
        store.load_logins(NOW).unwrap().0,
    );
    // The advice must not wait for, or acquire, the account writer lock.
    let _held = store.acquire_within(&[]).unwrap().unwrap();
    let pick = cold_resume_env(&world.seat(), Cli::Codex, &env, &program);
    let (login, routed) = match pick {
        Err(_) => (2, 0),
        Ok(None) => (i64::from(on_work), 0),
        Ok(Some(pick)) => (i64::from(pick.value.is_some()), 1),
    };
    let after = (
        store.load(NOW).unwrap().0,
        store.load_logins(NOW).unwrap().0,
    );
    [login, routed, i64::from(before == after)]
}

pub(crate) fn capability_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    if action == "NextCase" {
        next.insert("case", state["case"] + 1);
    } else {
        for (key, value) in ["login", "routed", "unchanged"]
            .into_iter()
            .zip(trial(state["case"]))
        {
            next.insert(key, value);
        }
        next.insert("phase", 1);
    }
    next
}

#[test]
fn cold_resume_advice_conforms_to_executable_capability() {
    let model = aterm_models_harness::cold_resume_capability_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(model.mutant_dials(), (1..=9).collect::<Vec<_>>());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = capability_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (case, key, value, law) in [
        (1, "login", 1, "NamedIsolation"),
        (7, "login", 0, "PinRespected"),
        (8, "login", 0, "DefaultAvailable"),
        (0, "login", 0, "SupportedProgress"),
        (5, "login", 2, "DefaultFallback"),
        (11, "routed", 1, "HealthyRetained"),
        (0, "unchanged", 0, "AdviceOnly"),
    ] {
        let mut state = model.init_state();
        state.insert("case", case);
        let mut bad = capability_transition(&state, "Advise");
        bad.insert(key, value);
        assert!(!model.check_invariant(law, &bad), "negative control: {law}");
    }
}
