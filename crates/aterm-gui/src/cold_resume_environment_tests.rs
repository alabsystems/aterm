// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::accounts::{self, Cli, Store};
use aterm_agent::harness::accounts_pool::Seat;

fn trial(case: i64, carried: i64, custom_home: bool) -> [i64; 6] {
    trial_program(case, carried, custom_home, true)
}

fn trial_program(case: i64, carried: i64, custom_home: bool, supported: bool) -> [i64; 6] {
    use std::os::unix::fs::PermissionsExt as _;

    const SESSION: &str = "0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c";
    const NOW: i64 = 1_800_000_000;
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let cfg = root.join("cfg");
    let state = root.join("state");
    let home = root.join("home ' $(printf literal)");
    let default = home.join(".codex");
    let work = root.join("work ' $(printf literal)");
    for dir in [&cfg, &state, &default, &work] {
        std::fs::create_dir_all(dir).unwrap();
    }
    if case != 2 {
        std::fs::write(default.join("auth.json"), "{}\n").unwrap();
    }
    std::fs::write(work.join("auth.json"), "{}\n").unwrap();
    std::fs::write(
        cfg.join(Cli::Codex.accounts().roster_file),
        format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n[[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = {:?}\n",
            work.to_str().unwrap()
        ),
    )
    .unwrap();
    let seat = Seat {
        config_dir: &cfg,
        state: &state,
        home: &home,
        now: NOW,
    };
    let store = Store::at_for(&state, Cli::Codex, &home);
    store.machine_id("0123456789abcdef").unwrap();
    if case == 1 {
        store
            .transact_within(&accounts::LOCK_LADDER, NOW, &mut None, |state| {
                Ok(accounts::note_wall(
                    state,
                    "work",
                    "5h",
                    Some(NOW + 3_600),
                    NOW,
                    NOW,
                ))
            })
            .unwrap();
    }
    let before = store.load(NOW).unwrap().0;
    let exe = aterm_agent::harness::codex_alab::lay_package(
        &root.join("codex ' $(printf literal)"),
        true,
    );
    let body = concat!(
        "printf '%s\\000' \"${CODEX_AUTH_PATH-unset}\" \"${CODEX_API_KEY-unset}\" ",
        "\"${CODEX_ACCESS_TOKEN-unset}\" \"${OPENAI_API_KEY-unset}\" ",
        "\"${KEEP_ME-unset}\" \"${CODEX_HOME-unset}\" \"$@\"\n"
    );
    std::fs::write(
        &exe,
        format!(
            "# {}\n{body}",
            if supported {
                aterm_agent::harness::codex_alab::AUTH_PATH_MARK
            } else {
                "old executable"
            }
        ),
    )
    .unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        aterm_agent::harness::codex_alab::auth_path_unread(&exe).is_none(),
        supported
    );
    let argv = vec![
        exe.to_str().unwrap().to_string(),
        "--no-alt-screen".into(),
        "-C".into(),
        root.join("cwd ' $(printf literal)")
            .to_str()
            .unwrap()
            .into(),
        "resume".into(),
        SESSION.into(),
    ];
    let restore = AgentRestore {
        pid: 4242,
        start: "fixture".into(),
        // The recorded self-claim's argv, not this outer process label, is executed.
        program: "/absent/outer-codex".into(),
        argv: argv.clone(),
        session: Some(SESSION.into()),
        cwd: root.to_str().unwrap().into(),
        version: None,
        home: None,
        login: None,
        word: "codex".into(),
        cause: None,
        tab: None,
        run: Some(RunRestore::Codex(CodexRestore {
            home: default.to_str().unwrap().into(),
            home_env: None,
            embedded: true,
            started_ms: 0,
            carried: None,
            alab: Some(AlabRestore {
                argv: argv.clone(),
                session: SESSION.into(),
                codex_home: custom_home.then(|| default.to_str().unwrap().into()),
                codex_auth_path: (case != 2)
                    .then(|| work.join("auth.json").to_str().unwrap().into()),
                cwd: Some(root.to_str().unwrap().into()),
            }),
        })),
    };
    let original = restore.clone();
    if case == 1 && !custom_home {
        let mut oversized = restore.clone();
        let Some(RunRestore::Codex(run)) = &mut oversized.run else {
            unreachable!();
        };
        run.alab.as_mut().unwrap().argv[3] = format!("/{}", "x".repeat(64 * 1024));
        assert!(
            oversized.resume_line_at(Some(&seat)).is_none(),
            "an unrenderable routed command cannot fall back to the original renderer"
        );
        let mut empty_home = restore.clone();
        let Some(RunRestore::Codex(run)) = &mut empty_home.run else {
            unreachable!();
        };
        run.alab.as_mut().unwrap().codex_home = Some(String::new());
        let empty_home_line = empty_home
            .resume_line_at(Some(&seat))
            .expect("explicitly empty CODEX_HOME still routes through the pool");
        assert!(
            empty_home_line.contains("-u 'CODEX_HOME'") && !empty_home_line.contains("CODEX_HOME="),
            "routed command clears CODEX_HOME instead of conflicting with an empty assignment: {empty_home_line}"
        );
    }
    let Some(line) = restore.resume_line_at(Some(&seat)) else {
        return [2, 0, 0, 0, 0, 0];
    };
    let mut command = aterm_uds::spawnfd::command("/bin/sh");
    command
        .args(["-f", "-c", &line])
        .current_dir(&root)
        .env_remove("CODEX_AUTH_PATH")
        .env_remove("CODEX_API_KEY")
        .env_remove("CODEX_ACCESS_TOKEN")
        .env_remove("CODEX_HOME")
        .env("OPENAI_API_KEY", "tool fixture")
        .env("KEEP_ME", "retained");
    if carried == 1 {
        command
            .env("CODEX_HOME", "/inherited/home")
            .env("CODEX_AUTH_PATH", "/inherited/auth.json")
            .env("CODEX_API_KEY", "key fixture")
            .env("CODEX_ACCESS_TOKEN", "token fixture");
    }
    let output = aterm_uds::deadline::output_until(
        &mut command,
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut words: Vec<_> = output
        .stdout
        .split(|b| *b == 0)
        .map(|word| String::from_utf8(word.to_vec()).unwrap())
        .collect();
    assert_eq!(words.pop().as_deref(), Some(""));
    assert_eq!(words.len(), 6 + argv.len() - 1);
    let login = if words[0] == "unset" && words[5] == "unset" {
        0
    } else if words[0] == work.join("auth.json").to_str().unwrap() {
        1
    } else {
        2
    };
    [
        login,
        i64::from(words[1] == "unset" && words[2] == "unset"),
        i64::from(
            words[3] == "tool fixture"
                && words[4] == "retained"
                && words[5]
                    == if custom_home {
                        default.to_str().unwrap()
                    } else if case == 0 && carried == 1 {
                        "/inherited/home"
                    } else {
                        "unset"
                    },
        ),
        i64::from(words[6..] == argv[1..]),
        i64::from(restore == original && store.load(NOW).unwrap().0 == before),
        1,
    ]
}

pub(crate) fn environment_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "NextCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Contaminate" => {
            next.insert("carried", 1);
        }
        "Render" => {
            for (name, value) in ["login", "clean", "kept", "argv", "unchanged", "rendered"]
                .into_iter()
                .zip(trial(state["case"], state["carried"], false))
            {
                next.insert(name, value);
            }
            next.insert("phase", 1);
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn customized_codex_home_keeps_the_original_environment_even_when_limited() {
    assert_eq!(trial(1, 1, true), [1, 0, 1, 1, 1, 1]);
}

#[test]
fn unsupported_recorded_executable_uses_default_or_offers_no_command() {
    assert_eq!(trial_program(1, 1, false, false), [0, 1, 1, 1, 1, 1]);
    assert_eq!(
        trial_program(2, 1, false, false)[5],
        1,
        "an unchanged default command needs no named isolation"
    );
    assert_eq!(
        trial_program(1, 1, true, false)[5],
        0,
        "an unsupported named original cannot be offered as a fallback"
    );
}

#[test]
fn cold_resume_environment_conforms_in_a_contaminated_shell() {
    let model = aterm_models_harness::cold_resume_environment_model();
    assert_eq!(model.mutant_dials(), (1..=7).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let mut rendered = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = environment_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
                rendered += usize::from(action.name == "Render");
            }
        }
    }
    assert_eq!(rendered, 6);
    let mut state = model.init_state();
    state.insert("case", 1);
    state.insert("carried", 1);
    let after = environment_transition(&state, "Render");
    for (field, value, law) in [
        ("login", 2, "SelectedLogin"),
        ("clean", 0, "ManagedOverridesCleared"),
        ("kept", 0, "UnrelatedRetained"),
        ("argv", 0, "PreservedArguments"),
        ("unchanged", 0, "AdviceOnly"),
        ("rendered", 0, "CommandRendered"),
    ] {
        let mut broken = after.clone();
        broken.insert(field, value);
        assert!(!model.check_invariant(law, &broken), "{law}");
    }
}
