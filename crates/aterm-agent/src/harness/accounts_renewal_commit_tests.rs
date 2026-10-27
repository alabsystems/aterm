// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;

fn commit_case(cli: Cli, case: i64) -> [bool; 7] {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at_for(root.path(), cli, &root.path().join("home"));
    let path = root.path().join("roster.toml");
    let row = Row {
        label: "account".into(),
        email: Some("owner@example.test".into()),
        vendor: Vendor {
            region: (cli == Cli::Kimi).then_some(Region::Global),
            ..Vendor::default()
        },
        ..Row::default()
    };
    let text = append_rows_for(cli, "", &[row]).unwrap();
    std::fs::write(&path, &text).unwrap();
    let raw = Roster::load_for(cli, &path).unwrap();
    store
        .transact_logins(NOW - 1, &mut None, |state, logins| {
            let mut events = note_renewed(
                state,
                logins,
                "account",
                None,
                root.path().join("old").to_str().unwrap(),
                None,
                NOW - 2,
            );
            events.extend(note_auth(state, "account", Auth::SignedOut, NOW - 1));
            Ok(events)
        })
        .unwrap();
    let mut before = store.load_logins(NOW).unwrap().0;
    if case == 6 {
        before.generation_clock = u64::MAX;
        store.save_logins(&before).unwrap();
    }
    let row = bound(&raw, &before).row("account").unwrap().clone();
    let policy = RenewalPolicy {
        roster_path: &path,
        roster: &raw,
        row: &row,
        logins: &before,
    };
    let staging = store.claim_staging_dir("account").unwrap();
    let credential = staging.join(cli.connector().stores[0]);
    std::fs::create_dir_all(credential.parent().unwrap()).unwrap();
    std::fs::write(&credential, b"isolated credential fixture").unwrap();
    let files = || {
        [
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            path.clone(),
        ]
        .map(|p| std::fs::read(p).ok())
    };
    let original = files();
    let blocked = match case {
        1 | 4 | 5 | 8 => Some(store.state_witness_file()),
        2 => Some(store.logins_witness_file()),
        3 => Some(store.wake_path()),
        _ => None,
    };
    if let Some(blocked) = &blocked {
        std::fs::rename(blocked, blocked.with_extension("retained")).unwrap();
        std::fs::create_dir(blocked).unwrap();
    }
    let result = store.transact_renewal_within(&[], NOW, &mut None, policy, |state, logins| {
        if case == 7 {
            return Ok(Vec::new());
        }
        write_renewal_checkpoint_for(
            cli,
            row.vendor.region,
            "account",
            "owner@example.test",
            staging.to_str().unwrap(),
            None,
            Some(Proof::Status),
            NOW,
            logins,
        )
        .map_err(|why| why.to_string())?;
        Ok(note_renewed(
            state,
            logins,
            "account",
            None,
            staging.to_str().unwrap(),
            None,
            NOW,
        ))
    });
    assert_eq!(result.is_ok(), case == 7);
    if case != 7 {
        let actual = std::fs::canonicalize(&staging).unwrap();
        let checkpoint = read_renewal_checkpoint_for(cli, &staging, &actual, NOW).unwrap();
        assert_eq!(
            checkpoint.parent,
            RenewalParent::capture(&before, "account")
        );
    }
    let mut fenced = blocked.is_none()
        || (store.load(NOW).is_err()
            && store.load_logins(NOW).is_err()
            && store.check_signin_state(NOW).is_err());
    if let Some(blocked) = &blocked {
        std::fs::rename(blocked, blocked.with_extension("blocked")).unwrap();
        std::fs::rename(blocked.with_extension("retained"), blocked).unwrap();
    }
    if case == 4 {
        std::fs::write(&path, text.replace("owner@", "changed@")).unwrap();
    }
    let pre_recovery = files();
    let recovered = if case == 8 {
        let mut edited = false;
        assert!(
            store
                .transact_renewal_within(&[], NOW, &mut None, policy, |_, _| {
                    edited = true;
                    Ok(Vec::new())
                })
                .is_err()
        );
        assert!(
            !edited,
            "recovery must expire the captured parent before its next edit"
        );
        store.load_logins(NOW).is_ok()
    } else {
        let target = if case == 5 {
            path.with_file_name("other.toml")
        } else {
            path.clone()
        };
        store.recover_sync_within(&[], &target).unwrap_or(false)
    };
    if matches!(case, 4 | 5) {
        fenced &= store.load(NOW).is_err() && store.load_logins(NOW).is_err();
    }
    let after = files();
    let exact = if recovered {
        let logins = store.load_logins(NOW).unwrap().0;
        let state = store.load(NOW).unwrap().0;
        logins.get("account").unwrap().dir.as_deref() == staging.to_str()
            && state.accounts["account"].auth == Auth::Ok
            && state.accounts["account"].auth_at == Some(NOW)
            && std::fs::read_to_string(&path).unwrap() == text
    } else if matches!(case, 6 | 7) {
        after == original
    } else {
        after == pre_recovery
    };
    let mut parent = true;
    let mut once = true;
    if recovered {
        let logins = store.load_logins(NOW).unwrap().0;
        parent = RenewalParent::capture(&before, "account")
            .require_current(&logins, "account")
            .is_err();
        once = logins.generation_clock == before.generation_clock + 1
            && store
                .tail(100)
                .unwrap()
                .iter()
                .filter(|e| e.kind == Kind::Renewed)
                .count()
                == 2;
        assert!(!store.recover_sync_within(&[], &path).unwrap());
        once &= files() == after;
    }
    let preserved = std::fs::read(&credential).unwrap() == b"isolated credential fixture"
        && (recovered || files() == pre_recovery);
    [
        recovered,
        fenced,
        exact,
        once,
        parent,
        preserved,
        case != 6 || !store.sync_needs_recovery(),
    ]
}

pub(crate) fn commit_transition(
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
        "Commit" => {
            let cli = [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi]
                [usize::try_from(state["provider"] - 1).unwrap()];
            let [recovered, fenced, exact, once, parent, preserved, admission] =
                commit_case(cli, state["case"]);
            assert!(
                admission,
                "exhausted generations must fail before an intent is admitted"
            );
            next.insert("phase", 1);
            for (field, value) in [
                ("recovered", recovered),
                ("fenced", fenced),
                ("exact", exact),
                ("once", once),
                ("parent", parent),
                ("preserved", preserved),
            ] {
                next.insert(field, i64::from(value));
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn renewal_commit_native_conforms_to_interruption_model() {
    let model = aterm_models_harness::account_renewal_commit_model();
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
    let completed = commit_transition(&model.init_state(), "Commit");
    for (field, law) in [
        ("recovered", "AdmittedRenewalCompletes"),
        ("fenced", "IncompleteRenewalFenced"),
        ("exact", "ExactStateAndBinding"),
        ("once", "LedgerAndGenerationOnce"),
        ("parent", "CapturedParentExpires"),
        ("preserved", "CredentialsAndConflictsPreserved"),
    ] {
        let mut bad = completed.clone();
        bad.insert(field, 0);
        assert!(!model.check_invariant(law, &bad));
    }
    let mut bad = completed;
    bad.insert("case", 4);
    assert!(!model.check_invariant("ConflictsRefuse", &bad));
}
