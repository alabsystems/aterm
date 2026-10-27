// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn pending_renewal(world: &World, cli: Cli, staging: &Path, create: bool) {
    let store = world.seat().store(cli);
    let path = world.cfg.join(cli.accounts().roster_file);
    let witness = store.state_witness_file();
    std::fs::rename(&witness, witness.with_extension("retained")).unwrap();
    std::fs::create_dir(&witness).unwrap();
    let old = std::fs::read_to_string(&path).unwrap();
    let text = if create {
        accounts::append_rows_for(
            cli,
            "",
            &[Row {
                label: "work".into(),
                dir: Some(staging.to_str().unwrap().into()),
                ..Row::default()
            }],
        )
        .unwrap()
    } else {
        old.clone()
    };
    let held = store.acquire().unwrap();
    assert!(
        store
            .transact_sync_held(
                &held,
                NOW,
                &mut None,
                accounts::SyncTarget {
                    path: &path,
                    before: Some(&old),
                    after: Some(&text),
                    mode: None,
                },
                |state, logins| {
                    Ok(accounts::note_renewed(
                        state,
                        logins,
                        "work",
                        None,
                        staging.to_str().unwrap(),
                        None,
                        NOW,
                    ))
                },
                || Ok(())
            )
            .is_err()
    );
    drop(held);
    assert!(store.load_logins(NOW).is_err());
    std::fs::rename(&witness, witness.with_extension("obstruction")).unwrap();
    std::fs::rename(witness.with_extension("retained"), &witness).unwrap();
}

fn observe(cli: Cli, case: i64) -> (i64, [bool; 4]) {
    observe_read(cli, case, false)
}

fn observe_read(cli: Cli, case: i64, policy_only: bool) -> (i64, [bool; 4]) {
    let world = World::new("pool-recovery");
    let path = world.cfg.join(cli.accounts().roster_file);
    let empty = matches!(case, 2 | 5 | 6 | 9);
    if empty {
        std::fs::write(&path, "").unwrap();
    } else {
        world.two(cli);
    }
    let store = world.seat().store(cli);
    if case != 5 {
        world.on_main(cli, &[]);
        wall(&world, cli, "main", NOW + HOUR);
    }
    let staging = if matches!(case, 5 | 6) {
        world.root.join("unused-staging")
    } else {
        let staging = store.claim_staging_dir("work").unwrap();
        let credential = staging.join(cli.connector().stores[0]);
        std::fs::create_dir_all(credential.parent().unwrap()).unwrap();
        std::fs::write(credential, "{}").unwrap();
        staging
    };
    if (1..=4).contains(&case) {
        pending_renewal(&world, cli, &staging, case == 2);
    }
    if case == 3 {
        let raw = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, raw + "\n# conflicting policy edit\n").unwrap();
    }
    if case == 6 {
        std::fs::write(store.dir.join("sync-commit.json"), "damaged receipt").unwrap();
    }
    let files = || {
        [
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            path.clone(),
            store.dir.join("sync-commit.json"),
        ]
        .map(|path| std::fs::read(path).ok())
    };
    let before = files();
    let _held = (case == 4).then(|| store.acquire().unwrap());
    let reads = AtomicUsize::new(0);
    let injected = AtomicBool::new(false);
    let read = |_: Cli, _: &Path, _: i64| {
        reads.fetch_add(1, Ordering::SeqCst);
        if case == 8 && !injected.swap(true, Ordering::SeqCst) {
            pending_renewal(&world, cli, &staging, false);
        }
        None
    };
    let prepare = |row: &Row| {
        let env = login_env(cli, row, &world.home)?;
        if case == 7 {
            pending_renewal(&world, cli, &staging, false);
        }
        Ok(env)
    };
    let outcome = if policy_only {
        match world.seat().bound_roster(cli) {
            Ok(roster) if roster.rows.is_empty() => 1,
            Ok(roster) => {
                assert_eq!(roster.row("work").unwrap().dir.as_deref(), staging.to_str());
                2
            }
            Err(_) => 0,
        }
    } else {
        let result = if case == 9 {
            super::super::pool_snapshot_using(&world.seat(), cli, || {
                let raw = world.seat().roster(cli)?;
                pending_renewal(&world, cli, &staging, true);
                Ok(raw)
            })
            .and_then(|snapshot| {
                assert_eq!(
                    snapshot.roster.row("work").unwrap().dir.as_deref(),
                    staging.to_str()
                );
                super::super::launch_env_using(&world.seat(), cli, &read, prepare)
            })
        } else {
            super::super::launch_env_using(&world.seat(), cli, &read, prepare)
        };
        match result {
            Ok(Some(env)) => {
                assert_eq!(env.label, "work");
                let expected = match cli.accounts().seam {
                    crate::ai::connector::Seam::LoginFile { file, .. } => staging.join(file),
                    _ => staging.clone(),
                };
                assert_eq!(env.value.as_deref(), expected.to_str());
                2
            }
            Ok(None) => 1,
            Err(_) => 0,
        }
    };
    let recovered = matches!(case, 1 | 2 | 7..=9);
    let (coherent, once) = if recovered {
        let logins = store.load_logins(NOW).unwrap().0;
        let state = store.load(NOW).unwrap().0;
        let coherent = logins.get("work").unwrap().dir.as_deref() == staging.to_str()
            && state.account("work").auth == accounts::Auth::Ok;
        let after = files();
        let once = logins.generation_clock == 1
            && store
                .tail(100)
                .unwrap()
                .iter()
                .filter(|event| event.kind == accounts::Kind::Renewed)
                .count()
                == 1
            && !store.recover_sync_within(&[], &path).unwrap()
            && files() == after;
        (coherent, once)
    } else if case == 5 {
        (!store.dir.exists(), true)
    } else {
        (store.load_logins(NOW).is_err(), true)
    };
    let preserved = (!matches!(case, 3 | 4 | 6) || files() == before)
        && (matches!(case, 5 | 6) || staging.join(cli.connector().stores[0]).is_file());
    let unpolled = !(policy_only || matches!(case, 3..=6)) || reads.load(Ordering::SeqCst) == 0;
    (outcome, [coherent, once, preserved, unpolled])
}

#[test]
fn bound_roster_recovers_policy_without_reading_unrelated_budget() {
    let mut failures = Vec::new();
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        for case in 1..=6 {
            let (outcome, guards) = observe_read(cli, case, true);
            assert_eq!(
                outcome,
                match case {
                    1 | 2 => 2,
                    5 => 1,
                    _ => 0,
                },
                "{cli:?} case={case}"
            );
            assert_eq!(
                guards, [true; 4],
                "{cli:?} case={case}: coherent/once/preserved/unpolled"
            );
        }

        let world = World::new("policy-with-unreadable-budget");
        world.two(cli);
        world.on_main(cli, &[]);
        let seat = world.seat();
        let store = seat.store(cli);
        let staging = store.claim_staging_dir("work").unwrap();
        let credential = staging.join(cli.connector().stores[0]);
        std::fs::create_dir_all(credential.parent().unwrap()).unwrap();
        std::fs::write(&credential, "{}").unwrap();
        store
            .transact_logins(NOW, &mut None, |state, logins| {
                Ok(accounts::note_renewed(
                    state,
                    logins,
                    "work",
                    None,
                    staging.to_str().unwrap(),
                    None,
                    NOW,
                ))
            })
            .unwrap();
        let expected = accounts::bound(
            &seat.roster(cli).unwrap(),
            &store.load_logins(NOW).unwrap().0,
        );
        assert_eq!(
            expected.row("work").unwrap().dir.as_deref(),
            staging.to_str()
        );
        std::fs::write(store.state_file(), "not JSON").unwrap();
        let files = || {
            [
                store.state_file(),
                store.logins_file(),
                store.ledger_path(),
                world.cfg.join(cli.accounts().roster_file),
                store.dir.join("sync-commit.json"),
            ]
            .map(|path| std::fs::read(path).ok())
        };
        let before = files();
        match seat.bound_roster(cli) {
            Ok(roster) => assert_eq!(roster, expected),
            Err(why) => failures.push(format!(
                "{cli:?}: policy classification borrowed unreadable budget state: {why}"
            )),
        }
        assert_eq!(
            files(),
            before,
            "a policy read never repairs unrelated malformed budget bytes"
        );
        let reads = AtomicUsize::new(0);
        let read = |_: Cli, _: &Path, _: i64| {
            reads.fetch_add(1, Ordering::SeqCst);
            None
        };
        assert!(
            super::super::launch_env(&seat, cli, &read).is_err(),
            "managed launch still requires valid budget state"
        );
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        assert_eq!(files(), before);
        assert_eq!(std::fs::read(&credential).unwrap(), b"{}");
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn session_move_recovers_a_commit_and_discards_the_stale_prepared_line() {
    let cli = Cli::Codex;
    let world = World::new("move-pending-renewal");
    world.two(cli);
    world.on_main(cli, &[]);
    let store = world.seat().store(cli);
    let staging = store.claim_staging_dir("work").unwrap();
    std::fs::write(staging.join(cli.connector().stores[0]), "{}").unwrap();
    let s = session(
        cli,
        Supervisor::Aterm,
        Mode::Early,
        Path::new("/opt/provider"),
    );
    let mut prepared = false;
    let result = super::super::session_move_using(&world.seat(), &s, |row| {
        let env = login_env(cli, row, &world.home).unwrap();
        let line = relaunch::pool_line(&s.relaunch, cli, &env).unwrap();
        pending_renewal(&world, cli, &staging, false);
        prepared = true;
        Some(line)
    });
    assert!(prepared);
    assert_eq!(result, Moved::stay(StayWhy::NothingDue));
    assert!(world.load(cli).rotations.is_empty());
    let logins = store.load_logins(NOW).unwrap().0;
    assert_eq!(logins.generation_clock, 1);
    assert_eq!(logins.get("work").unwrap().dir.as_deref(), staging.to_str());
    assert_eq!(
        world
            .seat()
            .bound_roster(cli)
            .unwrap()
            .row("work")
            .unwrap()
            .dir
            .as_deref(),
        staging.to_str()
    );
}

pub(crate) fn recovery_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherProvider" => {
            next.insert("provider", state["provider"] + 1);
        }
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Observe" => {
            let cli =
                [Cli::Codex, Cli::Grok, Cli::Kimi][usize::try_from(state["provider"] - 1).unwrap()];
            let (outcome, [coherent, once, preserved, unpolled]) = observe(cli, state["case"]);
            next.insert("phase", 1);
            next.insert("outcome", outcome);
            for (field, value) in [
                ("coherent", coherent),
                ("once", once),
                ("preserved", preserved),
                ("unpolled", unpolled),
            ] {
                next.insert(field, i64::from(value));
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn pool_recovery_conforms_before_launch_and_at_final_admission() {
    let model = aterm_models_harness::account_pool_recovery_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = recovery_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let good = recovery_transition(&model.init_state(), "Observe");
    for (field, law) in [
        ("outcome", "RecoveredLaunchAllowed"),
        ("coherent", "CoherentRecoveredSnapshot"),
        ("once", "GenerationAndLedgerOnce"),
        ("preserved", "ConflictsAndCredentialsPreserved"),
        ("unpolled", "RefusalBeforeProviderUsage"),
    ] {
        let mut bad = good.clone();
        bad.insert(field, 0);
        assert!(!model.check_invariant(law, &bad));
    }
    let mut bad = good.clone();
    bad.insert("case", 3);
    assert!(!model.check_invariant("NoUnsafeLaunch", &bad));
    bad.insert("outcome", 1);
    assert!(!model.check_invariant("OnlyIntentionalUnmanaged", &bad));
    bad.insert("case", 5);
    bad.insert("outcome", 0);
    assert!(!model.check_invariant("EmptyPoolRemainsUsable", &bad));
}
