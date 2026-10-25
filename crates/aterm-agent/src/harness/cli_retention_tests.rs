// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

pub(crate) fn retention_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let tmp = tmp_dir("account-retention");
    let env = env_at(&tmp);
    three_accounts(&tmp, &env);
    let store = env.accounts_store();
    store.ensure().unwrap();
    let flag = |name| state.get(name).copied().unwrap_or_default();
    let saved = accounts::State {
        pinned: true,
        ..accounts::State::default()
    };
    if flag("witness") == 1 {
        std::fs::write(store.state_witness_file(), "").unwrap();
    }
    if flag("ledger") == 1 {
        std::fs::write(store.ledger_path(), "retained legacy history\n").unwrap();
    }
    if flag("present") == 1 {
        std::fs::write(store.state_file(), saved.to_json()).unwrap();
    }
    if flag("kind") == 3 {
        store.machine_id("0011223344556677").unwrap();
    }
    let mut next = state.clone();
    match action {
        "Save" => {
            store.save(&saved).unwrap();
            next.insert("phase", 1);
            next.insert("kind", 1);
            next.insert("attempted", 1);
        }
        "InterruptedSave" => {
            std::fs::create_dir(store.state_file()).unwrap();
            assert!(store.save(&saved).is_err());
            assert!(store.state_file().is_dir());
            std::fs::rename(store.state_file(), tmp.path().join("blocked-snapshot")).unwrap();
            next.insert("phase", 1);
            next.insert("kind", 1);
            next.insert("attempted", 1);
        }
        "Legacy" => {
            std::fs::write(store.state_file(), saved.to_json()).unwrap();
            std::fs::write(store.ledger_path(), "retained legacy history\n").unwrap();
            next.insert("phase", 1);
            next.insert("kind", 2);
        }
        "StatsOnly" => {
            store.machine_id("0011223344556677").unwrap();
            next.insert("phase", 1);
            next.insert("kind", 3);
        }
        "EventOnly" => {
            store
                .transact(env.now, &mut None, |_| {
                    Ok(vec![accounts::Event::new(
                        env.now,
                        accounts::Kind::SignIn,
                        "main",
                    )])
                })
                .unwrap();
            next.insert("phase", 1);
            next.insert("kind", 4);
            next.insert("attempted", 1);
        }
        "LoseSnapshot" => {
            std::fs::rename(store.state_file(), tmp.path().join("retained-snapshot")).unwrap();
            next.insert("phase", 2);
        }
        "Read" => {
            let before = every_file(tmp.path());
            let accepted = store.load(env.now).is_ok();
            for json in [false, true] {
                let (code, out, err) = acc_quiet(AccountsCmd::Report { json }, &env);
                assert_eq!(code, exit(if accepted { 0 } else { 1 }), "{err}");
                if !accepted && json {
                    let doc: Value = aterm_json::from_str(&out).unwrap();
                    assert!(doc["refused"].as_str().is_some());
                    assert!(doc["accounts"].as_array().unwrap().is_empty());
                    assert!(doc.get("next").is_none());
                }
            }
            assert_eq!(every_file(tmp.path()), before);
            next.insert("phase", 3);
            next.insert("accepted", i64::from(accepted));
        }
        "Edit" => {
            let mut edited = false;
            let result = store.transact(env.now, &mut None, |saved| {
                edited = true;
                saved.pinned = !saved.pinned;
                Ok(Vec::new())
            });
            assert_eq!(result.is_ok(), flag("accepted") == 1);
            next.insert("phase", 4);
            next.insert("edited", i64::from(edited));
            if edited {
                next.insert("attempted", 1);
            }
        }
        _ => unreachable!(),
    }
    next.insert("witness", i64::from(store.state_witness_file().is_file()));
    next.insert("ledger", i64::from(store.ledger_path().is_file()));
    next.insert("present", i64::from(store.state_file().is_file()));
    next
}

#[test]
fn snapshot_retention_conforms_and_lost_evidence_cannot_be_hidden() {
    let model = aterm_models_harness::account_state_retention_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = retention_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {next:?}",
                    action.name
                );
            }
        }
    }
    let mut save = retention_transition(&model.init_state(), "Save");
    save.insert("witness", 0);
    assert!(!model.check_invariant("AttemptRetained", &save));
    save.insert("witness", 1);
    let lost = retention_transition(&save, "LoseSnapshot");
    let mut read = retention_transition(&lost, "Read");
    read.insert("accepted", 1);
    assert!(!model.check_invariant("WitnessLossRefused", &read));
    let mut event = retention_transition(&model.init_state(), "EventOnly");
    event.insert("attempted", 0);
    event.insert("witness", 0);
    event.insert("present", 0);
    assert!(!model.check_invariant("FirstEventPublished", &event));
}
