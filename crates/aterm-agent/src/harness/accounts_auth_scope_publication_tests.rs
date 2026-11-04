// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;

struct Publication {
    _root: aterm_tempfile::TempDir,
    store: Store,
    path: PathBuf,
    before: String,
    after: String,
    auxiliary: PathBuf,
}

impl Publication {
    fn new(account: bool) -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let store = Store::at_for(root.path(), Cli::Claude, root.path());
        let path = root.path().join("roster.toml");
        let roster = |email: &str| {
            append_rows_for(
                Cli::Claude,
                "",
                &[Row {
                    label: "work".into(),
                    email: Some(email.into()),
                    ..Default::default()
                }],
            )
            .unwrap()
        };
        let before = roster("owner-a@example.test");
        let after = roster("owner-b@example.test");
        std::fs::write(&path, &before).unwrap();
        let auxiliary = root.path().join("auxiliary-published");
        let held = store.acquire().unwrap();
        store
            .transact_sync_held(
                &held,
                NOW,
                &mut None,
                SyncTarget {
                    path: &path,
                    before: Some(&before),
                    after: Some(&before),
                    mode: None,
                },
                |state, logins| {
                    if account {
                        let events =
                            note_auth_in_context(state, logins, "work", Auth::SignedOut, NOW);
                        state.accounts.get_mut("work").unwrap().windows.insert(
                            SESSION_LIMIT.into(),
                            WindowFig {
                                pct_x10: 420,
                                reset: NOW + 3_600,
                                at: NOW,
                                by: "fixture".into(),
                                source: SOURCE_TAP.into(),
                                epoch_at: None,
                            },
                        );
                        Ok(events)
                    } else {
                        Ok(Vec::new())
                    }
                },
                || Ok(()),
            )
            .unwrap();
        drop(held);
        Self {
            _root: root,
            store,
            path,
            before,
            after,
            auxiliary,
        }
    }

    fn images(&self) -> Vec<(Option<Vec<u8>>, Option<std::time::SystemTime>)> {
        [
            self.store.state_file(),
            self.store.logins_file(),
            self.store.ledger_path(),
            self.store.state_witness_file(),
            self.store.logins_witness_file(),
            self.store.wake_path(),
            self.store.machine_file(),
            self.path.clone(),
            self.store.dir.join("sync-commit.json"),
            self.store.dir.join("sync-commit.seen"),
            self.auxiliary.clone(),
        ]
        .map(|path| {
            (
                std::fs::read(&path).ok(),
                std::fs::metadata(path)
                    .ok()
                    .and_then(|metadata| metadata.modified().ok()),
            )
        })
        .into()
    }

    fn change(&self, operation: &str, after: &str) -> Result<Vec<Event>, String> {
        let held = self.store.acquire().unwrap();
        self.store.transact_sync_held(
            &held,
            NOW + 1,
            &mut None,
            SyncTarget {
                path: &self.path,
                before: Some(&self.before),
                after: Some(after),
                mode: None,
            },
            |state, logins| {
                Ok(match operation {
                    "entry-failure" => note_auth(state, "work", Auth::SignedOut, NOW + 1),
                    "scoped-failure" => {
                        note_auth_in_context(state, logins, "work", Auth::SignedOut, NOW + 1)
                    }
                    "entry-displaced" | "scoped-displaced" => {
                        let mut events = if operation == "entry-displaced" {
                            note_auth(state, "work", Auth::SignedOut, NOW + 1)
                        } else {
                            note_auth_in_context(state, logins, "work", Auth::SignedOut, NOW + 1)
                        };
                        events.extend(note_displaced(
                            state,
                            logins,
                            "work",
                            "new-default",
                            NOW + 1,
                        ));
                        events
                    }
                    "observed-failure" | "observed-success" => {
                        let observed = Roster::parse_for(Cli::Claude, &self.after).unwrap();
                        note_auth_for_observed_row(
                            state,
                            logins,
                            observed.row("work").unwrap(),
                            if operation == "observed-success" {
                                Auth::Ok
                            } else {
                                Auth::SignedOut
                            },
                            NOW + 1,
                        )
                    }
                    "checked" => note_checked_sign_in(state, logins, "work", NOW + 1),
                    "default-made" => note_default_login_made(state, logins, "work", None, NOW + 1),
                    "renewed" => note_renewed(
                        state,
                        logins,
                        "work",
                        None,
                        self.store.logins_dir().join("new-login").to_str().unwrap(),
                        None,
                        NOW + 1,
                    ),
                    "retained" => Vec::new(),
                    _ => unreachable!(),
                })
            },
            || std::fs::write(&self.auxiliary, b"published").map_err(|why| why.to_string()),
        )
    }
}

#[test]
fn fresh_auth_before_raw_identity_replacement_refuses_every_publication_image() {
    for account in [false, true] {
        for operation in [
            "entry-failure",
            "scoped-failure",
            "entry-displaced",
            "scoped-displaced",
            "checked",
            "default-made",
            "renewed",
        ] {
            let fixture = Publication::new(account);
            let before = fixture.images();
            let state = fixture.store.load(NOW).unwrap().0;
            let logins = fixture.store.load_logins(NOW).unwrap().0;
            assert_eq!(state.accounts.contains_key("work"), account);
            assert!(
                fixture.change(operation, &fixture.after).is_err(),
                "{operation}, prior={account}"
            );
            assert_eq!(fixture.images(), before, "{operation}, prior={account}");
            assert_eq!(fixture.store.load(NOW + 1).unwrap().0, state);
            let after = fixture.store.load_logins(NOW + 1).unwrap().0;
            assert_eq!(after, logins, "{operation}, prior={account}");
            assert_eq!(after.generation_clock, logins.generation_clock);
            assert_eq!(after.sign_in_clock, logins.sign_in_clock);
            assert_eq!(after.sign_in_receipts, logins.sign_in_receipts);
            assert!(!fixture.auxiliary.exists());
            assert!(!fixture.store.sync_needs_recovery());
        }
    }
}

#[test]
fn an_explicit_observed_row_publishes_only_while_its_policy_is_still_the_target() {
    for account in [false, true] {
        for operation in ["observed-failure", "observed-success"] {
            let matched = Publication::new(account);
            matched.change(operation, &matched.after).unwrap();
            let records = matched
                .store
                .read_account_records(Some(&matched.path), NOW + 1)
                .unwrap();
            let expected = if operation == "observed-success" {
                Auth::Ok
            } else {
                Auth::SignedOut
            };
            assert!(matches!(
                records.state.accounts["work"].auth_projection,
                AuthProjection::Legacy
            ));
            assert_eq!(
                records.state.accounts["work"].current_auth(records.roster.row("work").unwrap()),
                expected
            );
            assert_eq!(records.state.accounts["work"].auth_at, Some(NOW + 1));
            assert_eq!(
                records.raw_roster.row("work").unwrap().email.as_deref(),
                Some("owner-b@example.test")
            );
            assert!(matched.auxiliary.is_file());

            let retargeted = Publication::new(account);
            let mut later = Roster::parse_for(Cli::Claude, &retargeted.after).unwrap();
            later.rows[0].email = Some("owner-c@example.test".into());
            let final_target = append_rows_for(Cli::Claude, "", &later.rows).unwrap();
            let images = retargeted.images();
            let state = retargeted.store.load(NOW).unwrap().0;
            let logins = retargeted.store.load_logins(NOW).unwrap().0;
            assert!(retargeted.change(operation, &final_target).is_err());
            assert_eq!(retargeted.images(), images);
            assert_eq!(retargeted.store.load(NOW + 1).unwrap().0, state);
            assert_eq!(retargeted.store.load_logins(NOW + 1).unwrap().0, logins);
            assert!(!retargeted.auxiliary.exists());
            assert!(!retargeted.store.sync_needs_recovery());
        }
    }
}

#[test]
fn fresh_auth_publishes_when_the_observed_raw_identity_stays_the_same() {
    for account in [false, true] {
        for operation in [
            "entry-failure",
            "scoped-failure",
            "checked",
            "default-made",
            "renewed",
        ] {
            let fixture = Publication::new(account);
            let events = fixture.change(operation, &fixture.before).unwrap();
            let records = fixture
                .store
                .read_account_records(Some(&fixture.path), NOW + 1)
                .unwrap();
            let current = &records.state.accounts["work"];
            let success = matches!(operation, "checked" | "default-made" | "renewed");
            assert_eq!(
                current.current_auth(records.roster.row("work").unwrap()),
                if success { Auth::Ok } else { Auth::SignedOut }
            );
            assert_eq!(current.auth_at, Some(NOW + 1));
            assert_eq!(
                events.is_empty(),
                account && !success,
                "{operation}, prior={account}"
            );
            assert!(fixture.auxiliary.is_file());
            assert_eq!(
                std::fs::read_to_string(&fixture.path).unwrap(),
                fixture.before
            );
            if matches!(operation, "checked" | "default-made") {
                assert!(records.logins.sign_in_clock > 0);
                assert!(records.logins.sign_in_receipts.contains_key("work"));
            }
            if operation == "renewed" {
                let binding = records.logins.get("work").unwrap();
                assert!(binding.renewal_generation > 0);
                assert_eq!(binding.last_success_generation, binding.renewal_generation);
            }
        }
    }
}

#[test]
fn raw_identity_replacement_without_a_new_observation_retires_only_old_auth() {
    let fixture = Publication::new(true);
    let before = fixture.store.load(NOW).unwrap().0.accounts["work"].clone();
    let raw = Roster::load(&fixture.path).unwrap();
    fixture
        .store
        .transact_pool_logins_within(&[], &fixture.path, NOW, &mut None, |_, _, logins| {
            admit_workflow(logins, "work")?;
            Ok(Vec::new())
        })
        .unwrap();
    let logins = fixture.store.load_logins(NOW).unwrap().0;
    let workflow = current_workflow(raw.row("work").unwrap(), &logins).unwrap();
    let mut scoped = Event::new(NOW, Kind::SignIn, "work");
    assert!(!workflow_event_matches(
        raw.row("work").unwrap(),
        &logins,
        &scoped
    ));
    scoped.workflow = Some(workflow);
    assert!(workflow_event_matches(
        raw.row("work").unwrap(),
        &logins,
        &scoped
    ));
    let ledger = std::fs::read(fixture.store.ledger_path()).unwrap();
    assert!(
        fixture
            .change("retained", &fixture.after)
            .unwrap()
            .is_empty()
    );
    let records = fixture
        .store
        .read_account_records(Some(&fixture.path), NOW + 1)
        .unwrap();
    let after = &records.state.accounts["work"];
    assert_eq!(after.auth, before.auth);
    assert_eq!(after.auth_at, before.auth_at);
    assert_eq!(after.windows, before.windows);
    assert_eq!(
        after.current_auth(records.roster.row("work").unwrap()),
        Auth::Unseen
    );
    let row = records.raw_roster.row("work").unwrap();
    assert!(!workflow_token_current(row, &records.logins, workflow));
    assert!(!workflow_event_matches(row, &records.logins, &scoped));
    let mut retained = records.logins.clone();
    retained.workflow_clock = logins.workflow_clock;
    retained.workflows = logins.workflows.clone();
    assert_eq!(
        retained, logins,
        "only independent workflow custody changes"
    );
    assert_eq!(std::fs::read(fixture.store.ledger_path()).unwrap(), ledger);
    assert_eq!(
        std::fs::read_to_string(&fixture.path).unwrap(),
        fixture.after
    );
    assert!(fixture.auxiliary.is_file());
}

/// Case 16 binds observation-before-raw-replacement to the actual sync publication boundary.
pub(crate) fn publication_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    assert_eq!(action, "Commit");
    let fixture = Publication::new(true);
    let before = fixture.images();
    let logins = fixture.store.load_logins(NOW).unwrap().0;
    let result = fixture.change("renewed", &fixture.after);
    let after = fixture.store.load_logins(NOW + 1).unwrap().0;
    let mut next = state.clone();
    next.insert("phase", 2);
    next.insert("accepted", i64::from(result.is_ok()));
    next.insert(
        "advanced",
        i64::from(after.generation_clock > logins.generation_clock),
    );
    next.insert("preserved", i64::from(before == fixture.images()));
    next
}

#[test]
fn mismatched_auth_publication_has_no_success_or_generation_to_leak() {
    let model = aterm_models_harness::account_renewal_generation_model();
    let mut state = model.init_state();
    state.insert("case", 16);
    state.insert("phase", 1);
    let next = publication_transition(&state, "Commit");
    assert!(model.successors("Commit", &state).contains(&next));
    for (field, value, law) in [
        ("accepted", 1, "Refused"),
        ("preserved", 0, "RefusalRetains"),
        ("advanced", 1, "OtherKeeps"),
    ] {
        let mut bad = next.clone();
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad), "{law}");
    }
}
