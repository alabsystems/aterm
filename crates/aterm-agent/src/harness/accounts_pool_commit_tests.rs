// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

struct Obstruction {
    path: PathBuf,
    retained: Option<PathBuf>,
}

impl Obstruction {
    fn install(path: PathBuf) -> Self {
        let retained = path.exists().then(|| {
            let retained = path.with_extension("retained");
            std::fs::rename(&path, &retained).unwrap();
            retained
        });
        std::fs::create_dir(&path).unwrap();
        Self { path, retained }
    }

    fn restore(self) {
        std::fs::rename(&self.path, self.path.with_extension("obstruction")).unwrap();
        if let Some(retained) = self.retained {
            std::fs::rename(retained, self.path).unwrap();
        }
    }
}

fn run_operation(world: &World, cli: Cli, operation: i64, mut arm: impl FnMut()) -> bool {
    let mut s = session(
        cli,
        Supervisor::Aterm,
        Mode::Early,
        Path::new("/opt/provider"),
    );
    if operation == 8 {
        s.sample = None;
    }
    match operation {
        1 => super::super::session_move_using(&world.seat(), &s, |row| {
            let env = login_env(cli, row, &world.home).unwrap();
            let line = relaunch::pool_line(&s.relaunch, cli, &env).unwrap();
            arm();
            Some(line)
        })
        .line
        .is_some(),
        2..=4 => {
            let before = super::super::pool_snapshot(&world.seat(), cli).unwrap();
            let enrolled = accounts::enrolled_for(cli, &before.roster);
            let home_path = world.home.join(cli.accounts().default_home);
            arm();
            super::super::poll_usage(&world.seat(), cli, &before, &enrolled, &|_, home, _| {
                (home == home_path).then(|| Usage {
                    sample: near_wall(),
                    wall: None,
                })
            })
            .is_ok()
        }
        5..=9 => {
            let before = super::super::pool_snapshot(&world.seat(), cli).unwrap();
            let who = accounts::ProcessLogin {
                login: None,
                default_home: true,
                stored_login: true,
            };
            let row = before.roster.row("main").unwrap();
            arm();
            super::super::record_session(&world.seat(), &s, &who, row).is_ok()
        }
        _ => unreachable!(),
    }
}

fn observe(operation: i64, fault: i64) -> [bool; 6] {
    let cli = match operation {
        1 | 2 | 5 | 8 | 9 => Cli::Codex,
        3 | 6 => Cli::Grok,
        4 | 7 => Cli::Kimi,
        _ => unreachable!(),
    };
    let world = World::new("pool-commit");
    let work = world.two(cli);
    world.on_main(cli, &[]);
    let store = world.seat().store(cli);
    if operation == 9 {
        store
            .transact(NOW, &mut None, |state| {
                state.proven = false;
                Ok(Vec::new())
            })
            .unwrap();
    }
    let path = world.cfg.join(cli.accounts().roster_file);
    let files = || {
        [
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            path.clone(),
        ]
        .map(|path| std::fs::read(path).ok())
    };
    let mut before = None;
    let mut obstruction = None;
    let accepted = run_operation(&world, cli, operation, || {
        assert!(before.is_none(), "one preparation before each commit");
        before = Some(files());
        let blocked = match fault {
            1 => None,
            2 => Some(store.ledger_path()),
            3 => Some(store.state_witness_file()),
            4 => Some(store.wake_path()),
            _ => unreachable!(),
        };
        obstruction = blocked.map(Obstruction::install);
    });
    let before = before.unwrap();
    let readable = store.load(NOW).is_ok() && store.load_logins(NOW).is_ok();
    let pending = fault == 3 || (fault == 4 && operation == 9);
    let mut guarded = if pending { !readable } else { readable };
    if let Some(obstruction) = obstruction {
        obstruction.restore();
    }
    if fault == 2 {
        // A refusal before admission changes no record. A fresh invocation can retry.
        guarded &= files() == before;
        assert!(run_operation(&world, cli, operation, || {}));
    } else if pending {
        // Operational reads complete the admitted edit without executing it again.
        super::super::pool_snapshot(&world.seat(), cli).unwrap();
    }
    if operation == 1 {
        let snapshot = super::super::pool_snapshot(&world.seat(), cli).unwrap();
        let s = session(
            cli,
            Supervisor::Aterm,
            Mode::Early,
            Path::new("/opt/provider"),
        );
        let who = accounts::ProcessLogin {
            login: None,
            default_home: true,
            stored_login: true,
        };
        super::super::record_session(
            &world.seat(),
            &s,
            &who,
            snapshot.roster.row("main").unwrap(),
        )
        .unwrap();
    }
    let state = store.load(NOW).unwrap().0;
    let logins = store.load_logins(NOW).unwrap().0;
    let kind = match operation {
        1 => Some(accounts::Kind::Rotate),
        8 => None,
        9 => Some(accounts::Kind::Proven),
        _ => Some(accounts::Kind::Sample),
    };
    let account = "main";
    let event_count = store
        .tail(100)
        .unwrap()
        .iter()
        .filter(|event| kind.is_none_or(|kind| event.kind == kind) && event.account == account)
        .count();
    let coherent = if operation == 1 {
        state.active.as_deref() == Some("work")
            && state.rotations == vec![NOW]
            && !state.proven
            && !state.canary_holds(NOW + accounts::CANARY_HOLD_S + 1)
            && state
                .canary
                .as_ref()
                .is_some_and(|canary| canary.sid == "s-one")
    } else {
        (if operation == 8 {
            state.account(account).sample.is_none()
        } else {
            state
                .account(account)
                .sample
                .as_ref()
                .is_some_and(|sample| sample.at == NOW - MIN)
        }) && (operation != 9 || state.proven)
            && (operation <= 4
                || state
                    .sessions
                    .get("s-one")
                    .is_some_and(|row| row.account == "main"))
    };
    let after = files();
    let receipt = std::fs::read(store.dir.join("sync-commit.json")).unwrap();
    let once = event_count == usize::from(operation != 8)
        && logins.generation_clock == 0
        && !store.recover_sync_within(&[], &path).unwrap()
        && files() == after
        && std::fs::read(store.dir.join("sync-commit.json")).unwrap() == receipt;
    let preserved = after[1] == before[1]
        && after[3] == before[3]
        && std::fs::read(work.join(cli.connector().stores[0])).unwrap() == b"{}";
    let notification = store.wake_path().exists() == (operation == 9);
    [accepted, guarded, coherent, once, preserved, notification]
}

#[test]
fn stale_snapshot_refuses_before_pool_commit_admission() {
    let world = World::new("pool-commit-policy");
    let cli = Cli::Codex;
    world.two(cli);
    world.on_main(cli, &[]);
    let snapshot = super::super::pool_snapshot(&world.seat(), cli).unwrap();
    let store = world.seat().store(cli);
    let path = world.cfg.join(cli.accounts().roster_file);
    let mut changed = world.seat().roster(cli).unwrap();
    changed.rows[0].email = Some("changed@example.test".into());
    std::fs::write(
        &path,
        accounts::append_rows_for(cli, "", &changed.rows).unwrap(),
    )
    .unwrap();
    let files = || {
        [
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            store.dir.join("sync-commit.json"),
            store.wake_path(),
        ]
        .map(|path| std::fs::read(path).ok())
    };
    let before = files();
    let held = store.acquire().unwrap();
    let mut edited = false;
    let result =
        store.transact_pool_logins_held(&held, &path, NOW, &mut None, |raw, state, logins| {
            snapshot.require_policy(raw, logins)?;
            edited = true;
            state.proven = false;
            Ok(Vec::new())
        });
    assert!(result.is_err());
    assert!(!edited);
    assert_eq!(files(), before);
}

pub(crate) fn commit_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherOperation" => {
            next.insert("operation", state["operation"] + 1);
        }
        "AnotherFault" => {
            next.insert("fault", state["fault"] + 1);
        }
        "Rotate" | "Usage" | "Observe" => {
            let values = observe(state["operation"], state["fault"]);
            next.insert("phase", 1);
            for (key, value) in [
                "accepted",
                "guarded",
                "coherent",
                "once",
                "preserved",
                "notification",
            ]
            .into_iter()
            .zip(values)
            {
                next.insert(key, i64::from(value));
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn pool_commits_conform_through_admission_and_recovery() {
    let model = aterm_models_harness::account_pool_commit_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = commit_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let good = commit_transition(&model.init_state(), "Rotate");
    for (field, law) in [
        ("accepted", "SuccessfulWritesAvailable"),
        ("guarded", "PendingFenceAndAdmission"),
        ("coherent", "CoherentRecords"),
        ("once", "NoDuplicateEventsOrGenerations"),
        ("preserved", "RetainsPolicyAndCredentials"),
        ("notification", "NotificationFollowsEvents"),
    ] {
        let mut bad = good.clone();
        bad.insert(field, 0);
        assert!(!model.check_invariant(law, &bad));
    }
    let mut bad = good;
    bad.insert("fault", 2);
    assert!(!model.check_invariant("DurableAcknowledgment", &bad));
}
