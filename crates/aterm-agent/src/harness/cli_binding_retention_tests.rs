// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn saved_bindings(store: &accounts::Store, now: i64) -> accounts::Logins {
    let mut bindings = accounts::Logins::default();
    accounts::note_renewed(
        &mut accounts::State::default(),
        &mut bindings,
        "main",
        None,
        &store.logins_dir().join("main+1").to_string_lossy(),
        None,
        now,
    );
    bindings
}

pub(crate) fn binding_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let tmp = tmp_dir("binding-retention");
    let env = env_at(&tmp);
    three_accounts(&tmp, &env);
    let store = env.accounts_store();
    store.ensure().unwrap();
    let saved = saved_bindings(&store, env.now);
    if state["witness"] == 1 {
        std::fs::write(store.logins_witness_file(), "").unwrap();
    }
    if state["present"] == 1 {
        std::fs::write(store.logins_file(), saved.to_json()).unwrap();
    }
    let mut next = state.clone();
    match action {
        "Save" | "Restore" => {
            store.save_logins(&saved).unwrap();
            assert_eq!(store.load_logins(env.now), Ok((saved, None)));
            next.insert("phase", if action == "Save" { 1 } else { 4 });
            next.insert("attempted", 1);
            if action == "Restore" {
                next.insert("restored", 1);
            }
        }
        "InterruptedSave" => {
            std::fs::create_dir(store.logins_file()).unwrap();
            assert!(store.save_logins(&saved).is_err());
            assert!(store.logins_file().is_dir());
            std::fs::rename(store.logins_file(), tmp.path().join("blocked-bindings")).unwrap();
            next.insert("phase", 1);
            next.insert("attempted", 1);
        }
        "Legacy" => {
            std::fs::write(store.logins_file(), saved.to_json()).unwrap();
            next.insert("phase", 1);
        }
        "Fresh" => {
            // Existing state is compatible with a store that never published bindings.
            store.save(&accounts::State::default()).unwrap();
            next.insert("phase", 1);
        }
        "LoseBindings" => {
            std::fs::rename(store.logins_file(), tmp.path().join("retained-bindings")).unwrap();
            next.insert("phase", 2);
        }
        "Read" => {
            let before = every_file(tmp.path());
            let accepted = store.load_logins(env.now).is_ok();
            assert_eq!(store.check_signin_state(env.now).is_ok(), accepted);
            for json in [false, true] {
                let (code, out, err) = acc_quiet(AccountsCmd::Report { json }, &env);
                assert_eq!(code, exit(if accepted { 0 } else { 1 }), "{err}");
                if !accepted && json {
                    let doc: Value = aterm_json::from_str(&out).unwrap();
                    assert!(doc["refused"].as_str().unwrap().contains("logins.seen"));
                    assert!(doc["accounts"].as_array().unwrap().is_empty());
                    assert!(doc.get("next").is_none());
                }
            }
            assert_eq!(every_file(tmp.path()), before, "admission is read-only");
            next.insert("phase", 3);
            next.insert("accepted", i64::from(accepted));
        }
        "Edit" => {
            let mut edited = false;
            let before = every_file(tmp.path());
            let result = store.transact_logins(env.now, &mut None, |_, bindings| {
                edited = true;
                *bindings = saved;
                accounts::note_login_notice(bindings, "main", 3, env.now);
                Ok(Vec::new())
            });
            assert_eq!(result.is_ok(), state["accepted"] == 1);
            if !edited {
                // Taking the store lock may create its empty file; no account data changes.
                let mut after = every_file(tmp.path());
                after.retain(|(path, _)| *path != store.dir.join("accounts.lock"));
                assert_eq!(after, before);
                assert!(!store.logins_file().exists());
            }
            next.insert("phase", 4);
            next.insert("edited", i64::from(edited));
            if edited {
                next.insert("attempted", 1);
            }
        }
        _ => unreachable!(),
    }
    next.insert("witness", i64::from(store.logins_witness_file().is_file()));
    next.insert("present", i64::from(store.logins_file().is_file()));
    next
}

#[test]
fn binding_retention_conforms_and_loss_cannot_be_mistaken_for_fresh() {
    let model = aterm_models_harness::account_binding_retention_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = binding_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut save = binding_transition(&model.init_state(), "Save");
    save.insert("witness", 0);
    assert!(!model.check_invariant("AttemptRetained", &save));
    save.insert("witness", 1);
    let lost = binding_transition(&save, "LoseBindings");
    let mut read = binding_transition(&lost, "Read");
    read.insert("accepted", 1);
    assert!(!model.check_invariant("LostBindingsRefused", &read));
}
