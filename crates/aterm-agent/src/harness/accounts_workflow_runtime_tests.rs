// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;

struct Fixture {
    _root: aterm_tempfile::TempDir,
    store: accounts::Store,
    path: PathBuf,
    raw: accounts::Row,
}

impl Fixture {
    fn new(cli: Cli, case: i64) -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let store = accounts::Store::at_for(&root.path().join("state"), cli, &home);
        let path = root.path().join("roster.toml");
        let raw = accounts::Row {
            label: "work".into(),
            email: Some("owner-a@example.test".into()),
            dir: Some(root.path().join("login").display().to_string()),
            vendor: accounts::Vendor {
                region: (cli == Cli::Kimi).then_some(Region::Global),
                ..Default::default()
            },
            ..Default::default()
        };
        fs::write(
            &path,
            accounts::append_rows_for(cli, "", std::slice::from_ref(&raw)).unwrap(),
        )
        .unwrap();
        store
            .transact_pool_logins_within(&[], &path, NOW, &mut None, |_, _, logins| {
                accounts::admit_workflow(logins, &raw.label)?;
                if matches!(case, 3..=6 | 8..=14) {
                    accounts::note_sign_in_task(
                        logins,
                        "work",
                        if case >= 9 {
                            accounts::SignInAction::Login
                        } else {
                            accounts::SignInAction::Browser
                        },
                        if case >= 9 {
                            NOW - PERSON_BACKOFF_SECS
                        } else {
                            NOW - 2
                        },
                    );
                }
                let events = if matches!(case, 1 | 2 | 7) {
                    vec![
                        accounts::Event::new(NOW, accounts::Kind::SignIn, "work")
                            .because("provider browser bot-check"),
                    ]
                } else if matches!(case, 5 | 6) {
                    vec![
                        accounts::Event::new(NOW, accounts::Kind::SignIn, "work")
                            .because("provider window done session-kept"),
                    ]
                } else {
                    Vec::new()
                };
                Ok(events)
            })
            .unwrap();
        Self {
            _root: root,
            store,
            path,
            raw,
        }
    }

    fn records(&self) -> accounts::AccountRecords {
        self.store
            .read_account_records(Some(&self.path), NOW)
            .unwrap()
    }

    fn replace(&mut self, email: &str) {
        let before = self.records();
        let history = before
            .logins
            .get("work")
            .map(|binding| binding.sign_in_task);
        self.raw.email = Some(email.into());
        fs::write(
            &self.path,
            accounts::append_rows_for(self.store.cli(), "", std::slice::from_ref(&self.raw))
                .unwrap(),
        )
        .unwrap();
        let effective = accounts::bound(
            &accounts::Roster {
                rows: vec![self.raw.clone()],
            },
            &before.logins,
        );
        let refreshed = prepare_retry_workflow(
            &self.store,
            &self.path,
            &self.raw,
            effective.row("work").unwrap(),
            &before.logins,
            NOW,
        )
        .unwrap()
        .expect("a changed policy is admitted before retrying it");
        assert!(accounts::current_workflow(&self.raw, &refreshed.logins).is_some());
        assert_eq!(
            refreshed
                .logins
                .get("work")
                .map(|binding| binding.sign_in_task),
            history
        );
    }

    fn task_and_completion(&self, completed: bool) {
        self.store
            .transact_pool_logins_within(&[], &self.path, NOW, &mut None, |_, _, logins| {
                accounts::admit_workflow(logins, &self.raw.label)?;
                accounts::note_sign_in_task(
                    logins,
                    "work",
                    accounts::SignInAction::Browser,
                    NOW - 1,
                );
                Ok(if completed {
                    vec![
                        accounts::Event::new(NOW, accounts::Kind::SignIn, "work")
                            .because("provider window done session-kept"),
                    ]
                } else {
                    Vec::new()
                })
            })
            .unwrap();
    }

    fn images(&self) -> [Option<Vec<u8>>; 10] {
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
        ]
        .map(|path| fs::read(path).ok())
    }
}

fn runtime_case(cli: Cli, case: i64) -> (i64, i64) {
    let mut fixture = Fixture::new(cli, case);
    let original = fixture.records();
    let original_raw = fixture.raw.clone();
    let key = format!("{}:work", cli.id());
    let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
    if matches!(case, 3 | 4 | 8) {
        let mut state = guard.lock().unwrap();
        state.parked.insert(key.clone(), Instant::now());
        state.last_attempt.insert(key.clone(), Instant::now());
        state.activity_owners.insert(
            key.clone(),
            LoginOwner::new(&fixture.store, cli, "work", NOW)
                .for_row(original.roster.row("work").unwrap())
                .for_workflow(&original_raw, &original.logins)
                .observing(&original.logins),
        );
    }
    if matches!(case, 2 | 3 | 5..=8) {
        fixture.replace("owner-b@example.test");
    }
    if case == 7 {
        fixture.replace("owner-a@example.test");
    }
    if matches!(case, 5 | 6) {
        fixture.task_and_completion(case == 6);
        let records = fixture.records();
        let mut state = guard.lock().unwrap();
        state.parked.insert(key.clone(), Instant::now());
        state.activity_owners.insert(
            key.clone(),
            LoginOwner::new(&fixture.store, cli, "work", NOW)
                .for_row(records.roster.row("work").unwrap())
                .for_workflow(&fixture.raw, &records.logins)
                .observing(&records.logins),
        );
    }
    let records = fixture.records();
    let before = fixture.images();
    let held = match case {
        1 | 2 | 7 => {
            bot_check_on_record_at(&fixture.store, &fixture.raw, &records.logins).is_some()
        }
        3 | 4 => {
            reconcile_workflow_retries(
                &guard,
                &fixture.store,
                cli,
                &fixture.path,
                (
                    &fixture.raw,
                    records.roster.row("work").unwrap(),
                    &records.logins,
                ),
                NOW,
            );
            AutoRenewPermit::claim(&guard, &key, Instant::now(), false).is_none()
        }
        5 | 6 => {
            let token = workflow_browser_window_token(
                &fixture.store,
                &fixture.raw,
                &records.logins,
                NOW - 1,
            );
            reconcile_browser_window_unpark(
                &guard,
                &key,
                token,
                &fixture.raw,
                &records.logins,
                (&fixture.store, cli, &fixture.path),
            );
            let held = guard.lock().unwrap().parked.contains_key(&key);
            assert_eq!(
                unaddressed_sign_in_task_at(&fixture.store, &records.logins, &fixture.raw)
                    .is_some(),
                held
            );
            if case == 6 {
                guard
                    .lock()
                    .unwrap()
                    .parked
                    .insert(key.clone(), Instant::now());
                reconcile_browser_window_unpark(
                    &guard,
                    &key,
                    token,
                    &fixture.raw,
                    &records.logins,
                    (&fixture.store, cli, &fixture.path),
                );
                assert!(
                    guard.lock().unwrap().parked.contains_key(&key),
                    "a completion lifts its workflow once"
                );
            }
            held
        }
        8 => {
            let token = std::sync::Arc::new(());
            {
                let mut state = guard.lock().unwrap();
                state.parked.remove(&key);
                state.in_flight = true;
                state.active_login = Some(ActiveLogin {
                    owner: LoginOwner::new(&fixture.store, cli, "work", NOW)
                        .for_row(original.roster.row("work").unwrap())
                        .for_workflow(&original_raw, &original.logins),
                    route: None,
                    token: token.clone(),
                });
            }
            let claims = LoginClaims::capture(&guard);
            note_vendor_renewal_end(
                &guard,
                &claims,
                &fixture.store,
                cli,
                (&key, "work"),
                (&fixture.path, &original_raw, &original.logins),
                crate::harness::cli::Renewal::NeedsPerson,
            );
            let state = guard.lock().unwrap();
            assert!(state.in_flight);
            assert!(std::sync::Arc::ptr_eq(
                &state.active_login.as_ref().unwrap().token,
                &token
            ));
            state.parked.contains_key(&key)
        }
        9..=14 => {
            let clock = Instant::now();
            let owner = LoginOwner::new(&fixture.store, cli, "work", NOW)
                .for_row(records.roster.row("work").unwrap())
                .for_workflow(&fixture.raw, &records.logins)
                .observing(&records.logins);
            let captured_owner = owner.token.clone();
            let active = std::sync::Arc::new(());
            {
                let mut state = guard.lock().unwrap();
                state.parked.insert(key.clone(), clock);
                if case != 12 {
                    state.activity_owners.insert(key.clone(), owner.clone());
                }
                if case == 10 {
                    state.bot_checked.insert(key.clone(), clock);
                }
                if case == 11 {
                    state.last_attempt.insert(key.clone(), clock);
                }
                if case == 14 {
                    state.in_flight = true;
                    state.active_login = Some(ActiveLogin {
                        owner: owner.clone(),
                        route: None,
                        token: active.clone(),
                    });
                }
            }
            assert_eq!(
                accounts::current_sign_in_task(&fixture.raw, &records.logins)
                    .unwrap()
                    .at,
                NOW - PERSON_BACKOFF_SECS,
            );
            assert!(
                AutoRenewPermit::claim(&guard, &key, clock, false).is_none(),
                "CONTROL: the monotonic-only gate reproduces the sleep hold"
            );
            if case == 9 {
                reconcile_workflow_retries(
                    &guard,
                    &fixture.store,
                    cli,
                    &fixture.path,
                    (
                        &fixture.raw,
                        records.roster.row("work").unwrap(),
                        &records.logins,
                    ),
                    NOW - 1,
                );
                assert_eq!(guard.lock().unwrap().parked.get(&key), Some(&clock));
            }
            let newer = std::cell::RefCell::new(None);
            reconcile_workflow_retries_with_read(
                &guard,
                &fixture.store,
                cli,
                &fixture.path,
                (
                    &fixture.raw,
                    records.roster.row("work").unwrap(),
                    &records.logins,
                ),
                NOW,
                || {
                    if case == 13 {
                        let replacement = LoginOwner::new(&fixture.store, cli, "work", NOW)
                            .for_row(records.roster.row("work").unwrap())
                            .for_workflow(&fixture.raw, &records.logins)
                            .observing(&records.logins);
                        *newer.borrow_mut() = Some(replacement.token.clone());
                        let mut state = guard.lock().unwrap();
                        state.activity_owners.insert(key.clone(), replacement);
                        state.parked.insert(key.clone(), clock);
                    }
                },
            );
            {
                let state = guard.lock().unwrap();
                if matches!(case, 9..=11) {
                    assert!(!state.parked.contains_key(&key));
                } else {
                    assert_eq!(state.parked.get(&key), Some(&clock));
                }
                if case == 10 {
                    assert_eq!(state.bot_checked.get(&key), Some(&clock));
                }
                if case == 11 {
                    assert_eq!(state.last_attempt.get(&key), Some(&clock));
                }
                if case == 12 {
                    assert!(!state.activity_owners.contains_key(&key));
                } else if case == 13 {
                    let newer = newer.borrow();
                    let newer = newer.as_ref().unwrap();
                    assert!(!std::sync::Arc::ptr_eq(&captured_owner, newer));
                    assert!(std::sync::Arc::ptr_eq(
                        &state.activity_owners[&key].token,
                        newer,
                    ));
                } else {
                    assert!(std::sync::Arc::ptr_eq(
                        &state.activity_owners[&key].token,
                        &captured_owner,
                    ));
                }
                if case == 14 {
                    assert!(state.in_flight);
                    assert!(std::sync::Arc::ptr_eq(
                        &state.active_login.as_ref().unwrap().token,
                        &active,
                    ));
                }
            }
            AutoRenewPermit::claim(&guard, &key, clock, false).is_none()
        }
        _ => unreachable!(),
    };
    (i64::from(held), i64::from(fixture.images() == before))
}

pub(crate) fn runtime_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Read" => {
            let mut observed = None;
            for cli in [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi] {
                let outcome = runtime_case(cli, state["case"]);
                if let Some(expected) = observed {
                    assert_eq!(outcome, expected, "{cli:?}");
                }
                observed = Some(outcome);
            }
            let (held, preserved) = observed.unwrap();
            next.insert("phase", 1);
            next.insert("held", held);
            next.insert("preserved", preserved);
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn workflow_retry_and_completion_consumers_conform_across_restarts_and_replacement() {
    let model = aterm_models_harness::account_workflow_runtime_model();
    assert_eq!(model.mutant_dials(), (1..=15).collect::<Vec<_>>());
    assert!(aterm_spec::interp::bmc(&aterm_spec::interp::with_buggy(&model, 1)).is_err());
    let states = aterm_spec::interp::reachable_states(&model);
    assert_eq!(states.len(), 28);
    for state in states {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = runtime_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
}

/// A promise rests on a ledger that can say what the row met.
#[test]
fn a_page_row_is_promised_a_sign_in_only_on_a_ledger_that_can_be_read() {
    let fixture = Fixture::new(Cli::Codex, 3);
    let records = fixture.records();
    let promised =
        |cli| page_row_signs_in_unattended(cli, &fixture.store, &fixture.raw, &records.logins);
    assert!(
        promised(Cli::Codex),
        "CONTROL: nothing on record against it"
    );
    let ledger = fixture.store.ledger_path();
    let _ = std::fs::remove_file(&ledger);
    std::fs::create_dir(&ledger).unwrap();
    assert!(
        !promised(Cli::Codex),
        "a ledger that cannot be read says nothing"
    );
}

#[test]
fn unadmitted_prose_cannot_complete_or_block_an_admitted_task() {
    let fixture = Fixture::new(Cli::Kimi, 3);
    let before = fixture.records();
    let task = accounts::current_sign_in_task(&fixture.raw, &before.logins).unwrap();
    let token = accounts::current_workflow(&fixture.raw, &before.logins).unwrap();
    for why in [
        "ordinary sign-in text",
        "provider window done session-kept",
        "provider browser bot-check",
    ] {
        fixture
            .store
            .transact_pool_logins_within(&[], &fixture.path, NOW, &mut None, |_, _, _| {
                Ok(vec![
                    accounts::Event::new(NOW, accounts::Kind::SignIn, "work").because(why),
                ])
            })
            .unwrap();
        let records = fixture.records();
        let event = fixture.store.tail_validated(1).unwrap().pop().unwrap();
        assert_eq!(event.why, why);
        assert!(event.workflow.is_none());
        assert_eq!(
            accounts::current_workflow(&fixture.raw, &records.logins),
            Some(token)
        );
        assert_eq!(
            accounts::current_sign_in_task(&fixture.raw, &records.logins),
            Some(task)
        );
        assert!(
            workflow_browser_window_token(&fixture.store, &fixture.raw, &records.logins, task.at)
                .is_none()
        );
        assert!(bot_check_on_record_at(&fixture.store, &fixture.raw, &records.logins).is_none());
        assert_eq!(
            unaddressed_sign_in_task_at(&fixture.store, &records.logins, &fixture.raw),
            Some(task.at)
        );
    }
    fixture
        .store
        .transact_pool_logins_within(&[], &fixture.path, NOW, &mut None, |_, _, logins| {
            accounts::admit_workflow(logins, "work")?;
            Ok(vec![
                accounts::Event::new(NOW, accounts::Kind::SignIn, "work")
                    .because("provider browser bot-check"),
            ])
        })
        .unwrap();
    let records = fixture.records();
    assert_eq!(
        bot_check_on_record_at(&fixture.store, &fixture.raw, &records.logins),
        Some(NOW)
    );
    fixture
        .store
        .append(&[accounts::Event::new(
            NOW,
            accounts::Kind::WrongAccount,
            "work",
        )])
        .unwrap();
    assert_eq!(
        accounts::current_workflow(&fixture.raw, &fixture.records().logins),
        Some(token)
    );
    assert!(
        bot_check_on_record_at(&fixture.store, &fixture.raw, &records.logins).is_none(),
        "a committed independent terminal veto does not need positive workflow authority"
    );
}

#[test]
fn a_cleanup_read_cannot_erase_a_newer_workflows_claim_or_pending_task() {
    for cli in [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi] {
        for browser in [false, true] {
            let mut fixture = Fixture::new(cli, 6);
            fixture.replace("owner-b@example.test");
            fixture.task_and_completion(true);
            let records = fixture.records();
            let raw = fixture.raw.clone();
            let store = fixture.store.clone();
            let path = fixture.path.clone();
            let key = format!("{}:work", cli.id());
            let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
            let clock = Instant::now();
            if browser {
                let unattributed =
                    std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
                unattributed
                    .lock()
                    .unwrap()
                    .parked
                    .insert(key.clone(), clock);
                let completion =
                    workflow_browser_window_token(&store, &raw, &records.logins, NOW - 1);
                assert!(completion.is_some());
                reconcile_browser_window_unpark(
                    &unattributed,
                    &key,
                    completion,
                    &raw,
                    &records.logins,
                    (&store, cli, &path),
                );
                let held = unattributed.lock().unwrap();
                assert_eq!(held.parked.get(&key), Some(&clock));
                assert!(!held.browser_unparked.contains_key(&key));
            }
            let install = |raw: &accounts::Row, records: &accounts::AccountRecords| {
                let owner = LoginOwner::new(&store, cli, "work", NOW)
                    .for_row(records.roster.row("work").unwrap())
                    .for_workflow(raw, &records.logins)
                    .observing(&records.logins);
                let token = owner.token.clone();
                let mut state = guard.lock().unwrap();
                state.activity_owners.insert(key.clone(), owner.clone());
                state.ladder_owners.insert(key.clone(), owner);
                state.last_attempt.insert(key.clone(), clock);
                state.parked.insert(key.clone(), clock);
                state.bot_checked.insert(key.clone(), clock);
                state.ladder.failed(&key, "current C inputs", clock);
                token
            };
            let before_owner = install(&raw, &records);
            let newer = std::cell::RefCell::new(None);
            let images = std::cell::RefCell::new(None);
            let advance = || {
                fixture.replace("owner-c@example.test");
                fixture.task_and_completion(false);
                let current = fixture.records();
                *newer.borrow_mut() = Some(install(&fixture.raw, &current));
                *images.borrow_mut() = Some(fixture.images());
            };
            if browser {
                let token = workflow_browser_window_token(&store, &raw, &records.logins, NOW - 1);
                assert!(token.is_some());
                reconcile_browser_window_unpark_with_read(
                    &guard,
                    &key,
                    token,
                    &raw,
                    &records.logins,
                    (&store, cli, &path),
                    advance,
                );
            } else {
                reconcile_workflow_retries_with_read(
                    &guard,
                    &store,
                    cli,
                    &path,
                    (&raw, records.roster.row("work").unwrap(), &records.logins),
                    NOW,
                    advance,
                );
            }
            let state = guard.lock().unwrap();
            let token = newer.borrow();
            let token = token.as_ref().unwrap();
            assert!(!std::sync::Arc::ptr_eq(&before_owner, token));
            assert!(std::sync::Arc::ptr_eq(
                &state.activity_owners[&key].token,
                token
            ));
            assert!(std::sync::Arc::ptr_eq(
                &state.ladder_owners[&key].token,
                token
            ));
            assert_eq!(state.last_attempt.get(&key), Some(&clock));
            assert_eq!(state.parked.get(&key), Some(&clock));
            assert_eq!(state.bot_checked.get(&key), Some(&clock));
            assert!(state.ladder.has_backoff(&key));
            assert!(!state.browser_unparked.contains_key(&key));
            drop(state);
            assert_eq!(&fixture.images(), images.borrow().as_ref().unwrap());
            let current = fixture.records();
            assert!(accounts::current_sign_in_task(&fixture.raw, &current.logins).is_some());
        }
    }
}
