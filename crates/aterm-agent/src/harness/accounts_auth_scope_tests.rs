// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;

struct FailedLogin {
    _root: aterm_tempfile::TempDir,
    store: Store,
    roster: PathBuf,
    failed: Account,
}

impl FailedLogin {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let store = Store::at_for(root.path(), Cli::Claude, root.path());
        let roster = root.path().join("roster.toml");
        std::fs::write(
            &roster,
            append_rows_for(
                Cli::Claude,
                "",
                &[Row {
                    label: "work".into(),
                    ..Default::default()
                }],
            )
            .unwrap(),
        )
        .unwrap();
        let login = root.path().join("login");
        store
            .transact_logins(NOW, &mut None, |state, logins| {
                let events = note_renewed(
                    state,
                    logins,
                    "work",
                    None,
                    login.to_str().unwrap(),
                    None,
                    NOW,
                );
                state.accounts.get_mut("work").unwrap().windows.insert(
                    SESSION_LIMIT.into(),
                    WindowFig {
                        pct_x10: 200,
                        reset: NOW + 3_600,
                        at: NOW,
                        by: "fixture".into(),
                        source: SOURCE_TAP.into(),
                        epoch_at: None,
                    },
                );
                Ok(events)
            })
            .unwrap();
        store
            .transact_logins(NOW, &mut None, |state, logins| {
                Ok(note_auth_in_context(
                    state,
                    logins,
                    "work",
                    Auth::SignedOut,
                    NOW,
                ))
            })
            .unwrap();
        let records = store.read_account_records(Some(&roster), NOW).unwrap();
        let failed = records.state.accounts["work"].clone();
        assert!(matches!(failed.auth_projection, AuthProjection::Legacy));
        assert_eq!(
            failed.current_auth(records.roster.row("work").unwrap()),
            Auth::SignedOut
        );
        Self {
            _root: root,
            store,
            roster,
            failed,
        }
    }

    fn begin_attempt(&self) {
        let before = self.store.load_logins(NOW).unwrap().0;
        let generation = before.get("work").unwrap().renewal_generation;
        self.store
            .transact_logins(NOW, &mut None, |_, logins| {
                logins.shared.begin(
                    shared::Intent {
                        source: shared::Source {
                            network: [1; 32],
                            account: [2; 32],
                        },
                        decision: [3; 32],
                        generation: 1,
                        configuration: [4; 32],
                        request: [5; 16],
                        label: "work".into(),
                        identity: "owner@example.test".into(),
                        staging: self
                            .store
                            .logins_dir()
                            .join("staging")
                            .to_str()
                            .unwrap()
                            .into(),
                        before_generation: Some(generation),
                        begun_at: NOW,
                    },
                    None,
                )?;
                Ok(Vec::new())
            })
            .unwrap();
        let after = self.store.load_logins(NOW).unwrap().0;
        let old = before.get("work").unwrap();
        let new = after.get("work").unwrap();
        assert!(new.renewal_generation > generation);
        assert_eq!(new.dir, old.dir);
        assert_eq!(new.proved_email, old.proved_email);
        assert_eq!(new.proved_dir, old.proved_dir);
    }

    fn replace(&self, case: i64) -> bool {
        self.store
            .transact_logins(NOW, &mut None, |_, logins| {
                match case {
                    12 => {
                        logins.labels.get_mut("work").unwrap().dir = Some(
                            self.store
                                .logins_dir()
                                .join("replacement")
                                .to_str()
                                .unwrap()
                                .into(),
                        )
                    }
                    13 => note_identity(logins, "work", Proof::Status, Some("owner@example.test")),
                    _ => unreachable!(),
                }
                Ok(Vec::new())
            })
            .is_ok()
    }

    fn records(&self) -> AccountRecords {
        self.store
            .read_account_records(Some(&self.roster), NOW)
            .unwrap()
    }

    fn bytes(&self) -> [Option<Vec<u8>>; 5] {
        [
            self.store.state_file(),
            self.store.logins_file(),
            self.store.ledger_path(),
            self.store.wake_path(),
            self.roster.clone(),
        ]
        .map(|path| std::fs::read(path).ok())
    }

    fn history_kept(&self, state: &State, label: &str) -> bool {
        let account = &state.accounts[label];
        account.auth == self.failed.auth
            && account.auth_at == self.failed.auth_at
            && account.windows == self.failed.windows
            && account.sample == self.failed.sample
    }
}

fn auth_read(state: &State, logins: &Logins, label: &str) -> [bool; 4] {
    let roster = bound(
        &Roster {
            rows: vec![Row {
                label: label.into(),
                ..Default::default()
            }],
        },
        logins,
    );
    let row = roster.row(label).unwrap();
    let account = &state.accounts[label];
    let enrolled = [label.to_owned()].into();
    let selected = crate::harness::accounts_pool::launch_now(
        (Cli::Claude, false, Mode::Early),
        &roster,
        state,
        &enrolled,
        None,
        NOW,
    )
    .ok()
    .flatten()
    .is_some_and(|picked| picked.label == label);
    [
        account.current_auth(row) == Auth::SignedOut,
        account.current_auth(row) == Auth::Unseen,
        login_state(row, logins, state, NOW) == LoginState::Expired,
        condition(row, state, true, NOW).runs() && selected,
    ]
}

/// Replay the real durable prefix for each typed transition: failure first, optional
/// preparation admission, context-only replacement, then metadata selection and a fresh failure.
pub(crate) fn generation_scope_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let fixture = FailedLogin::new();
    let mut next = state.clone();
    if state["attempted"] == 1 || action == "BeginAttempt" {
        fixture.begin_attempt();
    }
    if state["phase"] >= 2 || action == "Commit" {
        let before = fixture.store.load_logins(NOW).unwrap().0;
        let accepted = fixture.replace(state["case"]);
        let after = fixture.store.load_logins(NOW).unwrap().0;
        if action == "Commit" {
            let records = fixture.records();
            next.insert("phase", 2);
            next.insert("accepted", i64::from(accepted));
            next.insert(
                "advanced",
                i64::from(after.get("work").unwrap().renewal_generation > before.generation_clock),
            );
            next.insert("preserved", 1);
            next.insert(
                "failure_current",
                i64::from(
                    records.state.accounts["work"]
                        .current_auth(records.roster.row("work").unwrap())
                        == Auth::SignedOut,
                ),
            );
            next.insert(
                "auth_unknown",
                i64::from(
                    records.state.accounts["work"]
                        .current_auth(records.roster.row("work").unwrap())
                        == Auth::Unseen,
                ),
            );
            next.insert(
                "history_kept",
                i64::from(fixture.history_kept(&records.state, "work")),
            );
        }
    }
    match action {
        "RecordFailure" | "BeginAttempt" => {
            let records = fixture.records();
            next.insert(
                if action == "RecordFailure" {
                    "recorded"
                } else {
                    "attempted"
                },
                1,
            );
            next.insert(
                "failure_current",
                i64::from(
                    records.state.accounts["work"]
                        .current_auth(records.roster.row("work").unwrap())
                        == Auth::SignedOut,
                ),
            );
            next.insert(
                "auth_unknown",
                i64::from(
                    records.state.accounts["work"]
                        .current_auth(records.roster.row("work").unwrap())
                        == Auth::Unseen,
                ),
            );
        }
        "Choose" => {
            next.insert("phase", 1);
        }
        "Commit" => {}
        "ReadAuth" => {
            let before = fixture.bytes();
            let records = fixture.records();
            let [_, _, expired, selectable] = auth_read(&records.state, &records.logins, "work");
            next.insert("phase", 3);
            next.insert("selectable", i64::from(selectable));
            next.insert("expired", i64::from(expired));
            next.insert("read_only", i64::from(before == fixture.bytes()));
        }
        "FreshFailure" => {
            fixture
                .store
                .transact_logins(NOW, &mut None, |state, logins| {
                    Ok(note_auth_in_context(
                        state,
                        logins,
                        "work",
                        Auth::SignedOut,
                        NOW,
                    ))
                })
                .unwrap();
            let records = fixture.records();
            let [failure, unknown, expired, selectable] =
                auth_read(&records.state, &records.logins, "work");
            next.insert("phase", 4);
            next.insert("failure_current", i64::from(failure));
            next.insert("auth_unknown", i64::from(unknown));
            next.insert("expired", i64::from(expired));
            next.insert("selectable", i64::from(selectable));
            next.insert(
                "history_kept",
                i64::from(fixture.history_kept(&records.state, "work")),
            );
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn restoring_a_retired_failure_as_current_reproduces_the_real_selection_bug() {
    let model = aterm_models_harness::account_renewal_generation_model();
    for case in [12, 13] {
        let fixture = FailedLogin::new();
        assert!(fixture.replace(case));
        let records = fixture.records();
        assert_eq!(
            auth_read(&records.state, &records.logins, "work"),
            [false, true, false, true]
        );
        assert!(fixture.history_kept(&records.state, "work"));
        let mut stale = records.state.clone();
        stale.accounts.get_mut("work").unwrap().auth_scope = Default::default();
        let [failure, _, expired, selectable] = auth_read(&stale, &records.logins, "work");
        assert!(failure && expired && !selectable);
        let mut bad = model.init_state();
        bad.insert("case", case);
        bad.insert("recorded", 1);
        bad.insert("phase", 3);
        bad.insert("failure_current", i64::from(failure));
        bad.insert("expired", i64::from(expired));
        bad.insert("selectable", i64::from(selectable));
        assert!(!model.check_invariant("ReplacedFailureRetired", &bad));
        assert!(!model.check_invariant("ReplacementCanServe", &bad));
    }
}

#[test]
fn preparation_serving_and_pure_relabels_keep_the_observed_failure_current() {
    for control in ["attempt", "served", "move", "swap"] {
        let fixture = FailedLogin::new();
        if control == "swap" {
            fixture
                .store
                .transact_logins(NOW, &mut None, |state, logins| {
                    Ok(note_renewed(
                        state,
                        logins,
                        "other",
                        None,
                        fixture.store.logins_dir().join("other").to_str().unwrap(),
                        None,
                        NOW,
                    ))
                })
                .unwrap();
        }
        if control == "attempt" {
            fixture.begin_attempt();
        } else {
            fixture
                .store
                .transact_logins(NOW, &mut None, |state, logins| {
                    match control {
                        "served" => {
                            note_served_login(logins, "work", NOW);
                        }
                        "move" => move_account(state, logins, "work", "moved")?,
                        "swap" => {
                            move_accounts(state, logins, &[("work", "other"), ("other", "work")])?;
                        }
                        _ => unreachable!(),
                    }
                    Ok(Vec::new())
                })
                .unwrap();
        }
        let label = match control {
            "move" => "moved",
            "swap" => "other",
            _ => "work",
        };
        let state = fixture.store.load(NOW).unwrap().0;
        let logins = fixture.store.load_logins(NOW).unwrap().0;
        assert_eq!(
            auth_read(&state, &logins, label),
            [true, false, true, false],
            "{control}"
        );
        assert!(fixture.history_kept(&state, label), "{control}");
    }
}
