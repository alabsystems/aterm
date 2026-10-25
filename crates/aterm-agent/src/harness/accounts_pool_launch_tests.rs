// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

pub(crate) fn launch_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    assert_eq!(action, "Choose");
    let world = World::new("launch-admission");
    let case = state["case"];
    if case == 1 {
        let store = world.seat().store(Cli::Codex);
        store.ensure().unwrap();
        std::fs::write(store.state_file(), b"unrelated old cache").unwrap();
        std::fs::write(store.logins_file(), b"unrelated old bindings").unwrap();
    } else {
        let work = world.two(Cli::Codex);
        world.on_main(Cli::Codex, &[]);
        wall(&world, Cli::Codex, "main", NOW + HOUR);
        let store = world.seat().store(Cli::Codex);
        match case {
            3 => std::fs::write(store.state_file(), b"not JSON").unwrap(),
            4 => std::fs::write(store.logins_file(), b"not JSON").unwrap(),
            5 => wall(&world, Cli::Codex, "work", NOW + HOUR),
            6 => {
                let home = world.home.join(".codex");
                std::fs::create_dir_all(&home).unwrap();
                std::fs::write(home.join("state_5.sqlite"), b"shared").unwrap();
                std::fs::write(work.join("state_5.sqlite"), b"own").unwrap();
            }
            7 => std::fs::write(world.cfg.join("accounts-codex.toml"), b"[bad").unwrap(),
            8 => std::fs::write(
                world.cfg.join("accounts-codex.toml"),
                format!(
                    "[[account]]\nlabel='away'\nclass='dedicated'\ndir='{}'\n",
                    world.root.join("no-login").display()
                ),
            )
            .unwrap(),
            9 => {
                store
                    .transact_within(&accounts::LOCK_LADDER, NOW, &mut None, |s| {
                        s.active = Some("missing".to_owned());
                        s.pinned = true;
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            _ => {}
        }
    }
    let store = world.seat().store(Cli::Codex);
    let before = (case == 10).then(|| std::fs::read(store.state_file()).unwrap());
    let _held = (case == 10).then(|| store.acquire().unwrap());
    let read = |_: Cli, _: &Path, _: i64| {
        assert!(
            !matches!(case, 3 | 4 | 7 | 8 | 9),
            "unreadable records must not probe usage"
        );
        (case == 10).then(|| Usage {
            sample: near_wall(),
            wall: Some(("five_hour", Some(NOW + HOUR))),
        })
    };
    let outcome = match super::super::launch_env(&world.seat(), Cli::Codex, &read) {
        Ok(None) => 0,
        Ok(Some(pick)) => {
            assert_eq!(pick.label, "work");
            1
        }
        Err(why) => {
            if case == 6 {
                assert!(why.contains("holds its own"), "{why}");
            }
            if case == 10 {
                assert!(why.contains("could not record fresh usage"), "{why}");
            }
            2
        }
    };
    if let Some(before) = before {
        assert_eq!(std::fs::read(store.state_file()).unwrap(), before);
    }
    let mut next = state.clone();
    next.insert("phase", 2);
    next.insert("outcome", outcome);
    next
}

#[test]
fn intentional_unmanaged_launches_are_successful_no_ops() {
    let no_read = |_: Cli, _: &Path, _: i64| -> Option<Usage> { panic!("unmanaged usage read") };
    let off = World::new("launch-off-result");
    off.two(Cli::Codex);
    off.on_main(Cli::Codex, &[]);
    std::fs::write(off.cfg.join("aterm.toml"), "[harness]\naccounts='off'\n").unwrap();
    assert_eq!(
        super::super::launch_env(&off.seat(), Cli::Codex, &no_read),
        Ok(None)
    );

    let lone = World::new("launch-lone-result");
    std::fs::write(
        lone.cfg.join("accounts-codex.toml"),
        "[[account]]\nlabel='main'\nclass='dedicated'\n",
    )
    .unwrap();
    assert_eq!(
        super::super::launch_env(&lone.seat(), Cli::Codex, &no_read),
        Ok(None)
    );

    let keyring = World::new("launch-keyring-result");
    keyring.two(Cli::Codex);
    keyring.on_main(Cli::Codex, &[]);
    let home = keyring.home.join(".codex");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("config.toml"),
        "cli_auth_credentials_store='keyring'\n",
    )
    .unwrap();
    std::fs::write(
        keyring.seat().store(Cli::Codex).state_file(),
        "unrelated cache",
    )
    .unwrap();
    assert_eq!(
        super::super::launch_env(&keyring.seat(), Cli::Codex, &no_read),
        Ok(None)
    );
}

#[test]
fn pins_refuse_bad_rows_and_new_walls_release_the_override() {
    for mode in ["off", "early"] {
        for missing in [false, true] {
            let world = World::new("launch-invalid-pin");
            world.two(Cli::Codex);
            let store = world.seat().store(Cli::Codex);
            store
                .transact_within(&accounts::LOCK_LADDER, NOW, &mut None, |s| {
                    s.active = Some(if missing { "missing" } else { "main" }.to_owned());
                    s.pinned = true;
                    s.accounts.entry("main".to_owned()).or_default().auth =
                        accounts::Auth::SignedOut;
                    Ok(Vec::new())
                })
                .unwrap();
            std::fs::write(
                world.cfg.join("aterm.toml"),
                format!("[harness]\naccounts='{mode}'\n"),
            )
            .unwrap();
            let no_read =
                |_: Cli, _: &Path, _: i64| -> Option<Usage> { panic!("invalid pin usage read") };
            assert!(super::super::launch_env(&world.seat(), Cli::Codex, &no_read).is_err());
        }
        let world = World::new("launch-limit-pin");
        world.two(Cli::Codex);
        world.on_main(Cli::Codex, &[]);
        wall(&world, Cli::Codex, "main", NOW + HOUR);
        pin(&world, Cli::Codex, "main").unwrap();
        let later = Seat {
            now: NOW + 2,
            ..world.seat()
        };
        let store = later.store(Cli::Codex);
        store
            .transact_within(&accounts::LOCK_LADDER, NOW + 1, &mut None, |s| {
                Ok(accounts::note_wall(
                    s,
                    "main",
                    "five_hour",
                    Some(NOW + HOUR),
                    NOW + 1,
                    NOW + 1,
                ))
            })
            .unwrap();
        let roster = later.bound_roster(Cli::Codex).unwrap();
        assert!(
            !store.load(later.now).unwrap().0.pinned,
            "a new wall releases the pin"
        );
        assert!(matches!(
            accounts::condition(
                roster.row("main").unwrap(),
                &store.load(later.now).unwrap().0,
                true,
                later.now
            ),
            Condition::Limited { .. }
        ));
        std::fs::write(
            world.cfg.join("aterm.toml"),
            format!("[harness]\naccounts='{mode}'\n"),
        )
        .unwrap();
        let selected = super::super::launch_env(&later, Cli::Codex, &no_usage)
            .unwrap()
            .map(|row| row.label);
        assert_eq!(
            selected.as_deref(),
            if mode == "off" { None } else { Some("work") }
        );
    }
}

#[test]
fn managed_launch_refusals_do_not_become_unmanaged_starters() {
    let model = aterm_models_harness::account_pool_launch_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        if model.action_enabled("Choose", &state) {
            let next = launch_transition(&state, "Choose");
            assert!(
                model.successors("Choose", &state).contains(&next),
                "{state:?}: {next:?}"
            );
            if state["case"] == 3 {
                let mut escaped = next;
                escaped.insert("outcome", 0);
                assert!(!model.check_invariant("UnreadHeld", &escaped));
            }
        }
    }
}

#[test]
fn a_failed_state_reload_after_usage_cannot_return_a_login() {
    let world = World::new("launch-reload");
    world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let path = world.seat().store(Cli::Codex).state_file();
    let read = |_: Cli, _: &Path, _: i64| {
        std::fs::write(&path, b"unreadable after usage").unwrap();
        None
    };
    assert!(super::super::launch_env(&world.seat(), Cli::Codex, &read).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"unreadable after usage");
}

#[cfg(unix)]
#[test]
fn a_dangling_roster_is_an_error_and_never_an_empty_pool() {
    let world = World::new("launch-roster-link");
    let path = world.cfg.join("accounts-codex.toml");
    let target = world.root.join("absent-roster");
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(world.seat().roster(Cli::Codex).is_err());
    assert!(super::super::launch_env(&world.seat(), Cli::Codex, &no_usage).is_err());
    assert_eq!(std::fs::read_link(&path).unwrap(), target);
    assert!(!target.exists());
}
