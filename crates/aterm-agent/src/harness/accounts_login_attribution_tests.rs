// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

pub(crate) fn attribution_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherProvider" => {
            next.insert("provider", 1);
        }
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Observe" => {
            let cli = [Cli::Codex, Cli::Grok][state["provider"] as usize];
            let temp = aterm_tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let work = root.join("work");
            let roster = Roster {
                rows: vec![
                    Row {
                        label: "main".into(),
                        ..Row::default()
                    },
                    Row {
                        label: "work".into(),
                        dir: Some(work.to_str().unwrap().into()),
                        ..Row::default()
                    },
                ],
            };
            let owned = work.join("auth.json").to_str().unwrap().to_owned();
            let external = root.join("external/auth.json").to_str().unwrap().to_owned();
            let wrong = work.join("other.json").to_str().unwrap().to_owned();
            let var = cli.accounts().seam.var();
            let env: Vec<_> = match state["case"] {
                0 => vec![],
                1 => vec![(var, "")],
                2 => vec![(var, owned.as_str())],
                3 => vec![(var, "auth.json")],
                4 => vec![(var, "nested/auth.json")],
                5 => vec![(var, external.as_str())],
                6 => vec![(var, wrong.as_str())],
                7 => vec![(var, owned.as_str()), (var, external.as_str())],
                _ => unreachable!(),
            };
            let who = ProcessLogin::of_env_for(cli, env.iter().copied());
            let judged = match judged_label(&roster, &who) {
                None => 0,
                Some("main") => 1,
                Some("work") => 2,
                _ => unreachable!(),
            };
            let retained =
                !matches!(state["case"], 3 | 4 | 6) || who.login.as_deref() == Some(env[0].1);
            if matches!(state["case"], 3 | 4) {
                assert_eq!(who.me(&roster).label(), None, "no raw default attribution");
            }
            next.insert("phase", 1);
            next.insert("judged", judged);
            next.insert("stored", i64::from(who.stored_login));
            next.insert("retained", i64::from(retained));
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn file_login_attribution_requires_known_context() {
    let model = aterm_models_harness::account_login_attribution_model();
    assert_eq!(model.mutant_dials(), (1..=7).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let mut observed = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = attribution_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
                observed += usize::from(action.name == "Observe");
            }
        }
    }
    assert_eq!(observed, 16);
    let mut relative = model.init_state();
    relative.insert("case", 3);
    let after = attribution_transition(&relative, "Observe");
    for (field, value, law) in [
        ("judged", 1, "ExactAttribution"),
        ("stored", 1, "UnknownRefused"),
        ("retained", 0, "UnboundPathRetained"),
    ] {
        let mut bad = after.clone();
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad), "{law}");
    }
    relative.insert("case", 5);
    let mut bad = attribution_transition(&relative, "Observe");
    bad.insert("stored", 0);
    assert!(!model.check_invariant("KnownStored", &bad));
}

#[cfg(windows)]
#[test]
fn drive_relative_and_rooted_file_logins_are_not_machine_defaults() {
    for cli in [Cli::Codex, Cli::Grok] {
        let var = cli.accounts().seam.var();
        for value in [
            r"C:auth.json",
            r"C:nested\auth.json",
            r"\auth.json",
            r"\nested\auth.json",
            "/nested/auth.json",
        ] {
            let who = ProcessLogin::of_env_for(cli, [(var, value)]);
            assert!(!who.stored_login, "{value}");
            assert_eq!(who.login.as_deref(), Some(value));
            assert_eq!(judged_label(&Roster::own(), &who), None);
        }
    }
}
