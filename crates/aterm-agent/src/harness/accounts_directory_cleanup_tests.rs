// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State as ModelState;

struct Mutation;

impl Drop for Mutation {
    fn drop(&mut self) {
        DIRECTORY_CLEANUP_MUTANT.set(0);
        accounts::SYNC_COMMIT_CUT.set(None);
    }
}

pub(crate) fn transition(state: &ModelState, action: &str) -> ModelState {
    transition_with(state, action, 0)
}

fn transition_with(state: &ModelState, action: &str, mutant: u8) -> ModelState {
    let mut next = state.clone();
    if action == "Cut" {
        next.insert("cut", state["cut"] + 1);
        return next;
    }
    assert_eq!(action, "Sync");
    let root = aterm_tempfile::tempdir().unwrap();
    let store = accounts::Store::at(&root.path().join("state"));
    let roster_path = root.path().join(accounts::ACCOUNTS_FILE);
    let vacated = store.login_dir("m8-2").unwrap();
    fs::create_dir_all(&vacated).unwrap();
    let before = Roster {
        rows: vec![
            Row {
                label: "m8".into(),
                email: Some("second@example.test".into()),
                ..Row::default()
            },
            Row {
                label: "m8-2".into(),
                email: Some("first@example.test".into()),
                dir: Some(vacated.to_str().unwrap().into()),
                ..Row::default()
            },
        ],
    };
    fs::write(&roster_path, format_roster_toml(&before).unwrap()).unwrap();
    let fleet = parse_accounting_csv(
        "email,computer,claude\nfirst@example.test,m8,1\nsecond@example.test,m8,1\n",
        "cleanup fixture",
    )
    .unwrap();
    let cut = usize::try_from(state["cut"]).unwrap();
    DIRECTORY_CLEANUP_MUTANT.set(mutant);
    accounts::SYNC_COMMIT_CUT.set((cut < 7).then_some(cut));
    let _mutation = Mutation;
    let result = persist_accounts_sync(
        &store,
        &roster_path,
        (&fleet, OptIn::Named),
        ("m8", Some("second@example.test")),
        &BTreeMap::new(),
        1_800_000_000,
        (&[], &[]),
    );
    assert_eq!(
        result.is_ok(),
        cut == 7,
        "cut{cut}: {:?}",
        result.as_ref().err()
    );
    next.insert("done", 1);
    next.insert("completed", i64::from(result.is_ok()));
    next.insert("retained", i64::from(vacated.is_dir()));
    if cut < 7 {
        assert!(store.sync_needs_recovery());
        if cut == 0 {
            assert_eq!(Roster::load(&roster_path).unwrap(), before);
        }
        let held = store.acquire().unwrap();
        assert!(store.recover_sync_held(&held, &roster_path).unwrap());
        assert!(!store.recover_sync_held(&held, &roster_path).unwrap());
    }
    assert_eq!(
        Roster::load(&roster_path)
            .unwrap()
            .row_for_login(None)
            .unwrap()
            .label,
        "m8-2"
    );
    next
}

#[test]
fn cleanup_follows_every_receipt_replay_cut() {
    let model = aterm_models_harness::account_sync_cleanup_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    let mut checked = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} -> {next:?}"
                );
                checked += usize::from(action.name == "Sync");
            }
        }
    }
    assert_eq!(checked, 8);
    for (mutant, cut, law) in [
        (1, 0, "InterruptedRetainsDirectory"),
        (2, 7, "CompletedCleansDirectory"),
    ] {
        let mut state = model.init_state();
        state.insert("cut", cut);
        let bad = transition_with(&state, "Sync", mutant);
        assert!(!model.check_invariant(law, &bad), "mutant{mutant}: {law}");
        assert!(!model.successors("Sync", &state).contains(&bad));
    }
}
