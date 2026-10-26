// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

pub(crate) fn environment_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Provider" => {
            next.insert("provider", state["provider"] + 1);
        }
        "Own" => {
            next.insert("own", 1);
        }
        "Carried" => {
            next.insert("carried", state["carried"] + 1);
        }
        "Apply" => {
            let cli = Cli::ALL[state["provider"] as usize];
            let own = state["own"] == 1;
            let carried = state["carried"];
            let var = cli.accounts().seam.var();
            let pick = LoginEnv {
                label: "chosen".into(),
                var,
                value: own.then(|| "/chosen/login".into()),
            };
            let mut starter = Starter {
                exe: PathBuf::from("/opt/fixture"),
                env: vec![
                    ("KEEP_ME".into(), "kept".into()),
                    ("OPENAI_API_KEY".into(), "tool fixture".into()),
                ],
                args: Ok(Vec::new()),
                unset: Vec::new(),
            };
            let ahead = aterm_agent::harness::relaunch::ahead_env(cli);
            if carried > 0 {
                starter.env.extend([
                    (var.into(), "/old/login".into()),
                    (var.into(), "/duplicate/login".into()),
                ]);
                starter.env.extend(
                    ahead
                        .iter()
                        .map(|name| ((*name).into(), "inline fixture".into())),
                );
                if cli == Cli::Grok {
                    starter
                        .env
                        .push(("GROK_LEADER_SOCKET".into(), "/old/socket".into()));
                }
            }
            let home = Path::new("/home/fixture");
            if carried == 2 {
                let previous = LoginEnv {
                    value: (!own).then(|| "/previous/login".into()),
                    ..pick.clone()
                };
                starter = pool_onto(starter, cli, &previous, home).unwrap();
            }
            let onto = pool_onto(starter, cli, &pick, home).unwrap();
            let values: Vec<_> = onto
                .env
                .iter()
                .filter(|(key, _)| key == var)
                .map(|(_, value)| value.as_str())
                .collect();
            let login = if own && values == ["/chosen/login"] && !onto.unset.contains(&var) {
                1
            } else if !own && values.is_empty() && onto.unset.contains(&var) {
                0
            } else {
                2
            };
            let clean = ahead.iter().all(|name| {
                onto.unset.contains(name) && !onto.env.iter().any(|(key, _)| key.as_str() == *name)
            }) && (cli != Cli::Grok
                || !own
                || (onto.unset.contains(&"GROK_LEADER_SOCKET")
                    && !onto.env.iter().any(|(key, _)| key == "GROK_LEADER_SOCKET")));
            let kept =
                onto.env
                    .iter()
                    .any(|pair| pair == &("KEEP_ME".into(), "kept".into()))
                    && onto
                        .env
                        .iter()
                        .any(|pair| pair == &("OPENAI_API_KEY".into(), "tool fixture".into()))
                    && (cli != Cli::Grok || own || !onto.unset.contains(&"GROK_LEADER_SOCKET"))
                    && (cli != Cli::Grok
                        || own
                        || carried != 1
                        || onto.env.iter().any(|pair| {
                            pair == &("GROK_LEADER_SOCKET".into(), "/old/socket".into())
                        }));
            let env: Vec<_> = onto
                .env
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect();
            let writable = [Dialect::Bash, Dialect::Zsh, Dialect::Fish]
                .into_iter()
                .all(|dialect| {
                    aterm_agent::harness::upgrade::task_line_env_clear(
                        dialect,
                        &env,
                        &onto.unset,
                        &onto.exe,
                        onto.args.as_ref().unwrap(),
                        "",
                    )
                    .is_ok()
                });
            next.insert("phase", 1);
            next.insert("login", login);
            next.insert("clean", i64::from(clean));
            next.insert("kept", i64::from(kept));
            next.insert("writable", i64::from(writable));
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn worker_environment_conforms_for_each_provider_and_reapplication() {
    let model = aterm_models_harness::account_worker_environment_model();
    assert_eq!(model.mutant_dials(), (1..=6).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let mut applied = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = environment_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
                applied += usize::from(action.name == "Apply");
            }
        }
    }
    assert_eq!(applied, Cli::ALL.len() * 2 * 3);
    let mut state = model.init_state();
    state.insert("carried", 1);
    let after = environment_transition(&state, "Apply");
    for (field, value, law) in [
        ("login", 2, "SelectedLogin"),
        ("clean", 0, "OverridesCleared"),
        ("kept", 0, "UnrelatedRetained"),
        ("writable", 0, "CommandWritable"),
    ] {
        let mut broken = after.clone();
        broken.insert(field, value);
        assert!(!model.check_invariant(law, &broken), "{law}");
    }
}
