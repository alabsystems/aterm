// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;

const NOW: i64 = 1_790_000_000;

struct Fixture {
    _root: aterm_tempfile::TempDir,
    store: accounts::Store,
    path: PathBuf,
    row: Row,
    logins: accounts::Logins,
}

impl Fixture {
    fn new(state: &State, variation: usize) -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let store = accounts::Store::at(&root.path().join("state"));
        let path = root.path().join("accounts.toml");
        let row = Row {
            label: "current".into(),
            email: Some("old@example.test".into()),
            identity_email: Some("old-provider@example.test".into()),
            browser: Some("Profile 1".into()),
            dir: Some(root.path().join("login").to_string_lossy().into_owned()),
            ..Row::default()
        };
        let raw = Roster {
            rows: vec![row.clone()],
        };
        fs::write(&path, format_roster_toml(&raw).unwrap()).unwrap();
        store
            .transact_logins(NOW, &mut None, |_, logins| {
                logins
                    .labels
                    .insert(row.label.clone(), accounts::Binding::default());
                Ok(Vec::new())
            })
            .unwrap();
        let logins = store.load_logins(NOW).unwrap().0;
        if state["parent"] == 1 {
            store
                .transact_logins(NOW, &mut None, |_, current| {
                    accounts::note_proof(current, &row.label, accounts::Proof::Unchecked);
                    Ok(Vec::new())
                })
                .unwrap();
            let current = store.load_logins(NOW).unwrap().0;
            assert!(
                !accounts::RenewalParent::capture(&logins, &row.label)
                    .matches(&current, &row.label)
            );
            assert_eq!(accounts::bound(&raw, &current).row(&row.label), Some(&row));
        }
        if state["row"] == 1 {
            let mut edited = row.clone();
            match variation {
                0 => edited.email = Some("new@example.test".into()),
                1 => edited.identity_email = Some("new-provider@example.test".into()),
                2 => edited.browser = Some("Profile 2".into()),
                3 => {
                    edited.dir = Some(root.path().join("new-login").to_string_lossy().into_owned())
                }
                _ => unreachable!(),
            }
            fs::write(
                &path,
                format_roster_toml(&Roster { rows: vec![edited] }).unwrap(),
            )
            .unwrap();
        }
        if state["readable"] == 0 {
            fs::write(&path, "[[invalid\n").unwrap();
        }
        Self {
            _root: root,
            store,
            path,
            row,
            logins,
        }
    }

    fn files(&self) -> [Option<Vec<u8>>; 3] {
        [
            self.store.state_file(),
            self.store.logins_file(),
            self.store.ledger_path(),
        ]
        .map(|path| fs::read(path).ok())
    }

    fn record(&self) -> bool {
        note_attempt(
            &self.store,
            (&self.path, &self.row, &self.logins),
            accounts::AttemptOutcome::Failed,
            "fixture route",
            NOW,
        )
    }
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    transition_for(state, action, 0)
}

fn transition_for(state: &State, action: &str, variation: usize) -> State {
    let mut next = state.clone();
    match action {
        "RowA" | "RowB" => {
            next.insert("row", i64::from(action == "RowB"));
        }
        "Replace" => {
            next.insert("parent", 1);
        }
        "Unreadable" | "Readable" => {
            next.insert("readable", i64::from(action == "Readable"));
        }
        "Assess" => {
            let f = Fixture::new(state, variation);
            let before = f.files();
            let admitted =
                with_current_attempt(&f.store, (&f.path, &f.row, &f.logins), NOW, |_| Vec::new())
                    .is_ok();
            assert_eq!(f.files(), before);
            next.insert("okay", i64::from(admitted));
            next.insert("phase", 1);
            next.insert("observed", 1);
            return next;
        }
        "Record" => {
            let f = Fixture::new(state, variation);
            let before = f.files();
            let admitted = state["okay"] == 1 && f.record();
            let recorded = f
                .store
                .load_logins(NOW)
                .unwrap()
                .0
                .get(&f.row.label)
                .is_some_and(|binding| binding.sign_in_attempt.is_some());
            assert_eq!(recorded, admitted);
            if !admitted {
                assert_eq!(f.files(), before);
            }
            next.insert("written", i64::from(recorded));
            next.insert("phase", 2);
        }
        _ => panic!("unknown attempt action {action}"),
    }
    next.insert("observed", 0);
    next
}

#[test]
fn attempt_observations_conform_to_current_row_and_parent() {
    let model = aterm_models_harness::account_attempt_policy_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(model.mutant_dials(), (1..=7).collect::<Vec<_>>());
    let states = aterm_spec::interp::reachable_states(&model);
    for variation in 0..4 {
        for state in &states {
            for action in &model.actions {
                if model.action_enabled(action.name, state) {
                    let next = if variation == 0 {
                        transition(state, action.name)
                    } else {
                        transition_for(state, action.name, variation)
                    };
                    assert!(
                        model.successors(action.name, state).contains(&next),
                        "variation {variation}, {}: {state:?} -> {next:?}",
                        action.name
                    );
                }
            }
        }
    }
    let state = states
        .iter()
        .find(|s| {
            s["phase"] == 1
                && s["okay"] == 1
                && s["row"] == 1
                && s["parent"] == 0
                && s["readable"] == 1
        })
        .unwrap();
    let f = Fixture::new(state, 0);
    // The real legacy transaction checks the parent but never reads the roster.
    let old =
        f.store
            .transact_logins_within(&accounts::LOCK_LADDER, NOW, &mut None, |_, current| {
                accounts::RenewalParent::capture(&f.logins, &f.row.label)
                    .require_current(current, &f.row.label)?;
                Ok(accounts::note_sign_in_attempt(
                    current,
                    &f.row.label,
                    accounts::AttemptOutcome::Failed,
                    "fixture route",
                    NOW,
                ))
            });
    assert!(old.is_ok());
    assert!(
        f.store
            .load_logins(NOW)
            .unwrap()
            .0
            .get(&f.row.label)
            .unwrap()
            .sign_in_attempt
            .is_some()
    );
    let mut bad = model.successors("Record", state)[0].clone();
    bad.insert("written", 1);
    assert!(!model.successors("Record", state).contains(&bad));
    assert!(!model.check_invariant("CurrentRow", &bad));
}

#[test]
fn a_duplicate_current_attempt_remains_admitted_without_new_history() {
    let model = aterm_models_harness::account_attempt_policy_model();
    let state = aterm_spec::interp::reachable_states(&model)
        .into_iter()
        .find(|s| s["phase"] == 0 && s["row"] == 0 && s["parent"] == 0 && s["readable"] == 1)
        .unwrap();
    let f = Fixture::new(&state, 0);
    assert!(f.record());
    let before = f.files();
    assert!(f.record(), "a no-op is still an admitted observation");
    assert_eq!(f.files(), before);
    fs::write(
        &f.path,
        format_roster_toml(&Roster {
            rows: vec![Row {
                email: Some("replacement@example.test".into()),
                ..f.row.clone()
            }],
        })
        .unwrap(),
    )
    .unwrap();
    assert!(
        !f.record(),
        "an identical old summary must still recheck its row"
    );
    assert_eq!(f.files(), before);
}
