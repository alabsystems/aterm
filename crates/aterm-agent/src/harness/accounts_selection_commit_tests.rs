// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;

fn exercise(cli: Cli, case: i64) -> (i64, [bool; 4]) {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at_for(root.path(), cli, &root.path().join("home"));
    let login = root.path().join("login");
    std::fs::create_dir(&login).unwrap();
    let credential = login.join(cli.connector().stores[0]);
    std::fs::create_dir_all(credential.parent().unwrap()).unwrap();
    std::fs::write(&credential, "{}").unwrap();
    let roster = Roster {
        rows: vec![Row {
            label: "work".into(),
            dir: Some(login.to_str().unwrap().into()),
            email: Some("owner@example.test".into()),
            vendor: Vendor {
                region: (cli == Cli::Kimi).then_some(Region::Global),
                ..Vendor::default()
            },
            ..Row::default()
        }],
    };
    let path = root.path().join("accounts.toml");
    std::fs::write(&path, append_rows_for(cli, "", &roster.rows).unwrap()).unwrap();
    store
        .transact(NOW, &mut None, |state| {
            note_wall(state, "work", "seven_day", Some(NOW + 3600), NOW, NOW);
            Ok(Vec::new())
        })
        .unwrap();
    let files = || {
        [
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            path.clone(),
            store.dir.join("sync-commit.json"),
            store.wake_path(),
        ]
        .map(|path| std::fs::read(path).ok())
    };
    if case == 3 {
        let mut changed = roster.clone();
        changed.rows[0].email = Some("changed@example.test".into());
        std::fs::write(&path, append_rows_for(cli, "", &changed.rows).unwrap()).unwrap();
    }
    let retained = root.path().join("retained-credential");
    let missing = case == 2 || case >= 8;
    if missing {
        std::fs::rename(&credential, &retained).unwrap();
    }
    let mut request = None;
    let mut guarded = true;
    if case >= 8 {
        let PinSelection::Renew(first) = store
            .select_account_for(cli, &roster, &path, "work", NOW)
            .unwrap()
        else {
            panic!("missing credential needs a request")
        };
        let current = if case == 8 {
            let PinSelection::Renew(current) = store
                .select_account_for(cli, &roster, &path, "work", NOW)
                .unwrap()
            else {
                panic!("second request")
            };
            let before_old = files();
            store.cancel_pin_request(&first, NOW).unwrap();
            guarded &= files() == before_old;
            guarded &= !store
                .complete_requested_pin_at(&path, &roster, &first, NOW)
                .unwrap();
            guarded &= files() == before_old;
            current
        } else {
            first
        };
        request = Some(current);
        if case != 11 {
            std::fs::rename(&retained, &credential).unwrap();
        }
        if case == 12 {
            let mut changed = roster.clone();
            changed.rows[0].email = Some("changed@example.test".into());
            std::fs::write(&path, append_rows_for(cli, "", &changed.rows).unwrap()).unwrap();
        }
    }
    let before = files();
    let blocked = match case {
        4 => Some(store.ledger_path()),
        5 => Some(store.state_witness_file()),
        6 | 10 => Some(store.wake_path()),
        _ => None,
    };
    let obstruction = blocked.map(|path| {
        let saved = path.exists().then(|| {
            let saved = path.with_extension("retained");
            std::fs::rename(&path, &saved).unwrap();
            saved
        });
        std::fs::create_dir(&path).unwrap();
        (path, saved)
    });
    let outcome = if case >= 8 {
        match store.complete_requested_pin_at(&path, &roster, request.as_ref().unwrap(), NOW) {
            Ok(true) => 3,
            Ok(false) => 4,
            Err(_) => 0,
        }
    } else {
        let asked = if case == 7 {
            if cli == Cli::Claude {
                Cli::Codex
            } else {
                Cli::Claude
            }
        } else {
            cli
        };
        match store.select_account_for(asked, &roster, &path, "work", NOW) {
            Ok(PinSelection::Pinned) => 1,
            Ok(PinSelection::Renew(_)) => 2,
            Err(_) => 0,
        }
    };
    let pending = matches!(case, 5 | 6 | 10);
    if pending {
        guarded &= super::super::accounts_sync::any_pending_pin(root.path(), NOW);
    }
    guarded &= store.load(NOW).is_err() == pending && store.load_logins(NOW).is_err() == pending;
    if let Some((path, saved)) = obstruction {
        std::fs::rename(&path, path.with_extension("obstruction")).unwrap();
        if let Some(saved) = saved {
            std::fs::rename(saved, &path).unwrap();
        }
    }
    if pending {
        guarded &= store.recover_sync_within(&[], &path).unwrap();
    } else if matches!(case, 3 | 4 | 7) {
        guarded &= files() == before;
    }
    let state = store.load(NOW).unwrap().0;
    let logins = store.load_logins(NOW).unwrap().0;
    let pinned = matches!(case, 1 | 5 | 6 | 8..=10);
    let coherent = if pinned {
        state.pinned
            && state.active.as_deref() == Some("work")
            && state.pending_pin.is_none()
            && state.account("work").walls.is_empty()
            && state.account("work").cleared_at == Some(NOW)
    } else {
        !state.pinned
            && state.active.is_none()
            && !state.account("work").walls.is_empty()
            && state.pending_pin.as_ref().map(PinRequest::label) == (case == 2).then_some("work")
    };
    let after = files();
    let once = store
        .tail(100)
        .unwrap()
        .iter()
        .filter(|event| event.kind == Kind::Use)
        .count()
        == usize::from(pinned)
        && logins.generation_clock == 0
        && !store.recover_sync_within(&[], &path).unwrap()
        && files() == after;
    let credential = if matches!(case, 2 | 11) {
        &retained
    } else {
        &credential
    };
    let preserved = before[1] == after[1]
        && before[3] == after[3]
        && std::fs::read(credential).unwrap() == b"{}";
    (outcome, [guarded, coherent, once, preserved])
}

pub(crate) fn selection_transition(
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
        "Select" | "Complete" => {
            let cli = Cli::ALL[usize::try_from(state["provider"] - 1).unwrap()];
            let (outcome, flags) = exercise(cli, state["case"]);
            next.insert("phase", 1);
            next.insert("outcome", outcome);
            for (field, value) in ["guarded", "coherent", "once", "preserved"]
                .into_iter()
                .zip(flags)
            {
                next.insert(field, i64::from(value));
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn explicit_selection_conforms_through_policy_failure_and_recovery() {
    let model = aterm_models_harness::account_selection_commit_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = selection_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let good = selection_transition(&model.init_state(), "Select");
    for (field, law) in [
        ("outcome", "PinAvailable"),
        ("guarded", "CurrentPolicyAndFence"),
        ("coherent", "CoherentBudgetAndIntent"),
        ("once", "NoDuplicateUseOrGeneration"),
        ("preserved", "RetainsCredentialsAndPolicy"),
    ] {
        let mut bad = good.clone();
        bad.insert(field, 0);
        assert!(!model.check_invariant(law, &bad));
    }
    for (case, law) in [
        (2, "RequestAvailable"),
        (8, "CompletionAvailable"),
        (11, "InvalidCompletionCancelled"),
    ] {
        let mut bad = good.clone();
        bad.insert("case", case);
        bad.insert("outcome", 0);
        assert!(!model.check_invariant(law, &bad));
    }
    let mut bad = good;
    bad.insert("case", 3);
    assert!(!model.check_invariant("NoFalseAcknowledgment", &bad));
}

/// A Codex row with no directory is the CLI's default login, here only while the default home
/// holds its login file (`~/.codex/auth.json`), as a launch reads it: a pin requested while the
/// file is missing is not completed onto a row no launch would use, and the same request made
/// once the row is signed in completes.
#[test]
fn a_requested_pin_on_a_default_codex_row_completes_only_with_its_login_file() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let store = Store::at_for(root.path(), Cli::Codex, &home);
    let roster = Roster {
        rows: vec![Row {
            label: "main".into(),
            email: Some("owner@example.test".into()),
            ..Row::default()
        }],
    };
    let path = root.path().join("accounts.toml");
    std::fs::write(
        &path,
        append_rows_for(Cli::Codex, "", &roster.rows).unwrap(),
    )
    .unwrap();
    let request = || {
        let PinSelection::Renew(request) = store
            .select_account_for(Cli::Codex, &roster, &path, "main", NOW)
            .unwrap()
        else {
            panic!("a default row with no login file is signed in before it is pinned")
        };
        request
    };
    let missing = request();
    assert!(
        !store
            .complete_requested_pin_at(&path, &roster, &missing, NOW)
            .unwrap(),
        "no pin on a default row whose login file is missing"
    );
    let (state, _) = store.load(NOW).unwrap();
    assert!(state.active.is_none() && !state.pinned, "{state:?}");
    assert!(state.pending_pin.is_none(), "the request is spent");
    // CONTROL: signed in meanwhile, the request completes onto the row.
    let signed_in = request();
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::write(home.join(".codex").join("auth.json"), "{}").unwrap();
    assert!(
        store
            .complete_requested_pin_at(&path, &roster, &signed_in, NOW)
            .unwrap()
    );
    let (state, _) = store.load(NOW).unwrap();
    assert_eq!(state.active.as_deref(), Some("main"));
    assert!(state.pinned);
}
