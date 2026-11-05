// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;
const ROUTES: &str = "fixture route: page stopped";

#[cfg(unix)]
#[test]
fn admitting_empty_unknown_custody_is_quiet_but_retiring_current_guidance_wakes() {
    use std::os::unix::fs::MetadataExt as _;
    for raw_slot in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let store = Store::at_for(root.path(), Cli::Claude, root.path());
        let path = root.path().join("roster.toml");
        let mut row = Row {
            label: "work".into(),
            ..Row::default()
        };
        std::fs::write(&path, row_text(&row).unwrap()).unwrap();
        store
            .transact_logins(NOW, &mut None, |_, logins| {
                let events = note_wrong_account(logins, "work", NOW);
                if raw_slot {
                    note_sign_in_task(logins, "work", SignInAction::Browser, NOW);
                }
                Ok(events)
            })
            .unwrap();
        let wake = || match std::fs::metadata(store.wake_path()) {
            Ok(metadata) => Some(metadata.ino()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => panic!("wake metadata: {error}"),
        };
        let before = wake();
        store
            .transact_pool_logins_within(&[], &path, NOW, &mut None, |_, _, logins| {
                admit_workflow(logins, "work")?;
                Ok(Vec::new())
            })
            .unwrap();
        assert_eq!(
            wake(),
            before,
            "empty historical custody has no guidance to withdraw"
        );
        let before = wake();
        store
            .transact_pool_logins_within(&[], &path, NOW, &mut None, |_, _, logins| {
                admit_workflow(logins, "work")?;
                note_sign_in_task(logins, "work", SignInAction::Browser, NOW);
                Ok(Vec::new())
            })
            .unwrap();
        assert_eq!(
            wake() == before,
            !raw_slot,
            "only an existing historical slot changes reader applicability"
        );
        assert!(current_sign_in_task(&row, &store.load_logins(NOW).unwrap().0).is_some());
        let before = wake();
        row.browser = Some("replacement".into());
        std::fs::write(&path, row_text(&row).unwrap()).unwrap();
        store
            .transact_pool_logins_within(&[], &path, NOW, &mut None, |_, _, logins| {
                admit_workflow(logins, "work")?;
                Ok(Vec::new())
            })
            .unwrap();
        assert_ne!(
            wake(),
            before,
            "retiring actual current guidance wakes its readers"
        );
        assert!(current_sign_in_task(&row, &store.load_logins(NOW).unwrap().0).is_none());
    }
}

type History = (Option<SignInTask>, Option<SignInAttempt>);

fn history(logins: &Logins, label: &str) -> History {
    let binding = logins.get(label).unwrap();
    (binding.sign_in_task, binding.sign_in_attempt.clone())
}

struct Fixture {
    _root: aterm_tempfile::TempDir,
    store: Store,
    path: PathBuf,
    cli: Cli,
    label: String,
    original_row: Row,
    original: History,
    original_token: WorkflowToken,
}

impl Fixture {
    fn new(provider: i64) -> Self {
        Self::with_custody(provider, true)
    }

    fn with_custody(provider: i64, tracked: bool) -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let cli = match provider {
            1 => Cli::Claude,
            2 => Cli::Codex,
            3 => Cli::Kimi,
            _ => unreachable!(),
        };
        let store = Store::at_for(root.path(), cli, root.path());
        let path = root.path().join("roster.toml");
        let row = Row {
            label: "work".into(),
            email: Some("login@example.test".into()),
            identity_email: (cli != Cli::Kimi).then(|| "owner@example.test".into()),
            browser: Some("Profile 1".into()),
            dir: Some(root.path().join("old-login").display().to_string()),
            vendor: Vendor {
                region: (cli == Cli::Kimi).then_some(Region::Global),
                ..Vendor::default()
            },
            ..Row::default()
        };
        std::fs::write(
            &path,
            append_rows_for(cli, "", std::slice::from_ref(&row)).unwrap(),
        )
        .unwrap();
        if tracked {
            store
                .transact_pool_logins_within(&[], &path, NOW, &mut None, |_, _, logins| {
                    logins.entry(&row.label).unwrap().dir.clone_from(&row.dir);
                    admit_workflow(logins, &row.label)?;
                    note_sign_in_task(logins, &row.label, SignInAction::Browser, NOW);
                    Ok(note_sign_in_attempt(
                        logins,
                        &row.label,
                        AttemptOutcome::Blocked,
                        ROUTES,
                        NOW,
                    ))
                })
                .unwrap();
        } else {
            let mut legacy = Logins::default();
            legacy.labels.insert(
                row.label.clone(),
                Binding {
                    dir: row.dir.clone(),
                    sign_in_task: Some(SignInTask {
                        action: SignInAction::Browser,
                        at: NOW,
                    }),
                    sign_in_attempt: Some(SignInAttempt {
                        outcome: AttemptOutcome::Blocked,
                        routes: ROUTES.into(),
                        since: NOW,
                        tried_at: NOW,
                    }),
                    ..Binding::default()
                },
            );
            store.save_logins(&legacy).unwrap();
        }
        let records = store.read_account_records(Some(&path), NOW).unwrap();
        let raw = records.raw_roster.row(&row.label).unwrap();
        assert!(!records.state.accounts.contains_key(&row.label));
        assert_eq!(
            current_sign_in_task(raw, &records.logins).is_some(),
            tracked
        );
        assert_eq!(current_sign_in_blocked(raw, &records.logins), tracked);
        let original = history(&records.logins, &row.label);
        let original_token =
            current_workflow(raw, &records.logins).unwrap_or_else(|| WorkflowToken::from_value(0));
        Self {
            _root: root,
            store,
            path,
            cli,
            label: row.label.clone(),
            original_row: row,
            original,
            original_token,
        }
    }

    fn records(&self) -> AccountRecords {
        self.store
            .read_account_records(Some(&self.path), NOW)
            .unwrap()
    }

    fn images(&self) -> Vec<Option<Vec<u8>>> {
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
        .map(|path| std::fs::read(path).ok())
        .into()
    }

    fn replace_raw(&mut self, row: Row, relabel: bool) {
        let before = std::fs::read_to_string(&self.path).unwrap();
        let after = append_rows_for(self.cli, "", std::slice::from_ref(&row)).unwrap();
        let held = self.store.acquire().unwrap();
        self.store
            .transact_sync_held(
                &held,
                NOW,
                &mut None,
                SyncTarget {
                    path: &self.path,
                    before: Some(&before),
                    after: Some(&after),
                    mode: None,
                },
                |state, logins| {
                    if relabel {
                        move_account(state, logins, &self.label, &row.label)?;
                    }
                    Ok(Vec::new())
                },
                || Ok(()),
            )
            .unwrap();
        self.label = row.label;
    }

    fn transform(&mut self, edit: i64) {
        if edit == 3 {
            self.store
                .transact_pool_logins_within(&[], &self.path, NOW, &mut None, |_, _, logins| {
                    logins.labels.get_mut(&self.label).unwrap().dir = Some(
                        self.store
                            .logins_dir()
                            .join("replacement")
                            .display()
                            .to_string(),
                    );
                    Ok(Vec::new())
                })
                .unwrap();
        } else if edit <= 9 {
            let mut row = self.records().raw_roster.row(&self.label).unwrap().clone();
            match edit {
                1 => row.email = Some("other-login@example.test".into()),
                2 => row.identity_email = Some("other-owner@example.test".into()),
                4 => {
                    row.dir = Some(
                        self.store
                            .logins_dir()
                            .join("other-own")
                            .display()
                            .to_string(),
                    )
                }
                5 => row.browser = Some("Profile 2".into()),
                6 => row.class = Class::Pool,
                7 => row.held_by = Some("pool".into()),
                8 => {
                    assert_eq!(self.cli, Cli::Codex);
                    row.vendor.flow = Some(Flow::Device);
                }
                9 => row.label = "renamed".into(),
                _ => unreachable!(),
            }
            self.replace_raw(row, edit == 9);
        } else if edit == 10 {
            let before = self.records();
            let generation = before.logins.get(&self.label).unwrap().renewal_generation;
            self.store
                .transact_pool_logins_within(&[], &self.path, NOW, &mut None, |_, _, logins| {
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
                            label: self.label.clone(),
                            identity: "owner@example.test".into(),
                            staging: self
                                .store
                                .logins_dir()
                                .join("staging")
                                .display()
                                .to_string(),
                            before_generation: Some(generation),
                            begun_at: NOW,
                        },
                        None,
                    )?;
                    Ok(Vec::new())
                })
                .unwrap();
            assert!(
                self.records()
                    .logins
                    .get(&self.label)
                    .unwrap()
                    .renewal_generation
                    > generation
            );
        } else if edit == 11 {
            let before = self.records().logins;
            self.store = Store::at_for(self._root.path(), self.cli, self._root.path());
            assert_eq!(self.records().logins, before);
        } else if edit == 12 {
            self.store
                .transact_pool_logins_within(&[], &self.path, NOW, &mut None, |_, _, logins| {
                    note_served_login(logins, &self.label, NOW);
                    Ok(Vec::new())
                })
                .unwrap();
        } else {
            assert_eq!(edit, 13);
            self.activity();
        }
    }

    fn activity(&self) {
        self.store
            .transact_pool_logins_within(&[], &self.path, NOW, &mut None, |_, state, logins| {
                note_served_login(logins, &self.label, NOW);
                note_login_notice(logins, &self.label, 1, NOW);
                Ok(note_auth_in_context(
                    state,
                    logins,
                    &self.label,
                    Auth::Ok,
                    NOW,
                ))
            })
            .unwrap();
    }

    fn fresh(&self, task: bool) {
        self.store
            .transact_pool_logins_within(&[], &self.path, NOW, &mut None, |_, _, logins| {
                admit_workflow(logins, &self.label)?;
                if task {
                    note_sign_in_task(logins, &self.label, SignInAction::Browser, NOW);
                    Ok(Vec::new())
                } else {
                    Ok(note_sign_in_attempt(
                        logins,
                        &self.label,
                        AttemptOutcome::Blocked,
                        ROUTES,
                        NOW,
                    ))
                }
            })
            .unwrap();
    }

    fn back(&mut self, edit: i64) {
        if edit == 3 {
            self.store
                .transact_pool_logins_within(&[], &self.path, NOW, &mut None, |_, _, logins| {
                    logins
                        .labels
                        .get_mut(&self.label)
                        .unwrap()
                        .dir
                        .clone_from(&self.original_row.dir);
                    Ok(Vec::new())
                })
                .unwrap();
        } else {
            self.replace_raw(self.original_row.clone(), false);
        }
    }

    fn stale(&self, task: bool) -> (bool, bool) {
        let before = self.images();
        let accepted = self
            .store
            .transact_pool_logins_within(&[], &self.path, NOW, &mut None, |_, _, logins| {
                admit_workflow(logins, &self.label)?;
                let events = if task {
                    note_sign_in_task(logins, &self.label, SignInAction::Browser, NOW);
                    Vec::new()
                } else {
                    note_sign_in_attempt(logins, &self.label, AttemptOutcome::Blocked, ROUTES, NOW)
                };
                logins.labels.get_mut(&self.label).unwrap().dir = Some(
                    self.store
                        .logins_dir()
                        .join("later-replacement")
                        .display()
                        .to_string(),
                );
                Ok(events)
            })
            .is_ok();
        (accepted, self.images() == before)
    }

    fn observe(&self) -> Vec<(&'static str, i64)> {
        let before = self.images();
        let records = self.records();
        let raw = records.raw_roster.row(&self.label).unwrap();
        let token = current_workflow(raw, &records.logins);
        vec![
            (
                "task",
                i64::from(current_sign_in_task(raw, &records.logins).is_some()),
            ),
            (
                "attempt",
                i64::from(current_sign_in_attempt(raw, &records.logins).is_some()),
            ),
            (
                "blocked",
                i64::from(current_sign_in_blocked(raw, &records.logins)),
            ),
            (
                "token_changed",
                i64::from(token.is_some_and(|token| token != self.original_token)),
            ),
            (
                "history",
                i64::from(history(&records.logins, &self.label) == self.original),
            ),
            ("pure", i64::from(self.images() == before)),
        ]
    }
}

/// Replay the actual durable prefix; expected results come from native getters, not scope bits.
pub(crate) fn workflow_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    if action == "AnotherProvider" {
        next.insert("provider", 2);
        return next;
    }
    if action == "AnotherEdit" {
        next.insert(
            "edit",
            if state["provider"] == 1 && state["edit"] == 7 {
                9
            } else {
                state["edit"] + 1
            },
        );
        return next;
    }
    let mut fixture = Fixture::with_custody(state["provider"], state["phase"] != 9);
    if state["phase"] >= 2 && state["phase"] != 9 {
        fixture.transform(state["edit"]);
    }
    match state["phase"] {
        3 => fixture.fresh(true),
        4 => fixture.fresh(false),
        5 => fixture.activity(),
        6 => fixture.back(state["edit"]),
        _ => {}
    }
    match action {
        "Seed" => {
            next.insert("phase", 1);
            next.insert("read", 0);
        }
        "Legacy" => {
            next.insert("phase", 9);
            next.insert("read", 0);
        }
        "Transform" => {
            fixture.transform(state["edit"]);
            next.insert("phase", 2);
            next.insert("read", 0);
            next.insert(
                "history",
                i64::from(history(&fixture.records().logins, &fixture.label) == fixture.original),
            );
        }
        "FreshTask" | "FreshAttempt" => {
            fixture.fresh(action == "FreshTask");
            next.insert("phase", if action == "FreshTask" { 3 } else { 4 });
            next.insert("read", 0);
            next.insert(
                "history",
                i64::from(history(&fixture.records().logins, &fixture.label) == fixture.original),
            );
        }
        "Activity" => {
            fixture.activity();
            next.insert("phase", 5);
            next.insert("read", 0);
        }
        "Back" => {
            fixture.back(state["edit"]);
            next.insert("phase", 6);
            next.insert("read", 0);
        }
        "StaleTask" | "StaleAttempt" => {
            let (accepted, preserved) = fixture.stale(action == "StaleTask");
            next.insert("phase", if action == "StaleTask" { 7 } else { 8 });
            next.insert("read", 0);
            next.insert("accepted", i64::from(accepted));
            next.insert("preserved", i64::from(preserved));
        }
        "Read" => {
            next.insert("read", 1);
            for (field, value) in fixture.observe() {
                next.insert(field, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn workflow_observations_conform_independently_of_credentials_and_activity() {
    let model = aterm_models_harness::account_workflow_scope_model();
    assert_eq!(model.mutant_dials(), (1..=21).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = workflow_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
}

#[test]
fn removing_workflow_custody_keeps_raw_guidance_unknown() {
    for provider in 1..=2 {
        for edit in 1..=8 {
            if provider == 1 && edit == 8 {
                continue;
            }
            let mut fixture = Fixture::new(provider);
            fixture.transform(edit);
            let records = fixture.records();
            let raw = records.raw_roster.row(&fixture.label).unwrap();
            assert_eq!(history(&records.logins, &fixture.label), fixture.original);
            assert_eq!(current_sign_in_task(raw, &records.logins), None);
            assert_eq!(current_sign_in_attempt(raw, &records.logins), None);
            assert!(!current_sign_in_blocked(raw, &records.logins));
            let mut legacy = records.logins.clone();
            legacy.workflows = Default::default();
            assert_eq!(legacy.workflow_clock, records.logins.workflow_clock);
            assert_eq!(current_sign_in_task(raw, &legacy), None);
            assert_eq!(current_sign_in_attempt(raw, &legacy), None);
            assert!(!current_sign_in_blocked(raw, &legacy));
            assert_eq!(history(&legacy, &fixture.label), fixture.original);
        }
    }
}

#[test]
fn a_duplicate_current_workflow_observation_does_not_repeat_history() {
    for provider in 1..=2 {
        let fixture = Fixture::new(provider);
        let before = fixture.images();
        fixture.fresh(true);
        fixture.fresh(false);
        assert_eq!(fixture.images(), before);
    }
}

#[test]
fn the_same_failed_routes_on_a_replacement_workflow_start_their_own_history() {
    for provider in 1..=2 {
        for edit in [2, 3] {
            let mut fixture = Fixture::new(provider);
            fixture
                .store
                .transact_pool_logins_within(
                    &[],
                    &fixture.path,
                    NOW - 10,
                    &mut None,
                    |_, _, logins| {
                        admit_workflow(logins, &fixture.label)?;
                        Ok(note_sign_in_attempt(
                            logins,
                            &fixture.label,
                            AttemptOutcome::Failed,
                            ROUTES,
                            NOW - 10,
                        ))
                    },
                )
                .unwrap();
            assert_eq!(
                fixture
                    .records()
                    .logins
                    .get(&fixture.label)
                    .unwrap()
                    .sign_in_attempt
                    .as_ref()
                    .unwrap()
                    .since,
                NOW - 10
            );
            fixture.transform(edit);
            let retired = fixture.records();
            let raw = retired.raw_roster.row(&fixture.label).unwrap();
            assert_eq!(current_sign_in_attempt(raw, &retired.logins), None);
            let events = fixture
                .store
                .transact_pool_logins_within(&[], &fixture.path, NOW, &mut None, |_, _, logins| {
                    admit_workflow(logins, &fixture.label)?;
                    Ok(note_sign_in_attempt(
                        logins,
                        &fixture.label,
                        AttemptOutcome::Failed,
                        ROUTES,
                        NOW,
                    ))
                })
                .unwrap();
            assert_eq!(events.len(), 1);
            assert_eq!((events[0].kind, events[0].t), (Kind::SignIn, NOW));
            let fresh = fixture.records();
            let raw = fresh.raw_roster.row(&fixture.label).unwrap();
            assert_eq!(
                current_sign_in_attempt(raw, &fresh.logins),
                Some(&SignInAttempt {
                    outcome: AttemptOutcome::Failed,
                    routes: ROUTES.into(),
                    since: NOW,
                    tried_at: NOW,
                })
            );
            assert!(workflow_event_matches(raw, &fresh.logins, &events[0]));
            assert_eq!(
                fixture
                    .store
                    .tail(100)
                    .unwrap()
                    .iter()
                    .filter(|event| {
                        event.kind == Kind::SignIn
                            && workflow_event_matches(raw, &fresh.logins, event)
                    })
                    .count(),
                1
            );
            let before_repeat = fixture.images();
            let repeated = fixture
                .store
                .transact_pool_logins_within(
                    &[],
                    &fixture.path,
                    NOW + 1,
                    &mut None,
                    |_, _, logins| {
                        admit_workflow(logins, &fixture.label)?;
                        Ok(note_sign_in_attempt(
                            logins,
                            &fixture.label,
                            AttemptOutcome::Failed,
                            ROUTES,
                            NOW + 1,
                        ))
                    },
                )
                .unwrap();
            assert!(repeated.is_empty());
            assert_eq!(
                fixture
                    .records()
                    .logins
                    .get(&fixture.label)
                    .unwrap()
                    .sign_in_attempt
                    .as_ref()
                    .unwrap()
                    .since,
                NOW
            );
            assert_eq!(fixture.images(), before_repeat);
        }
    }
}

#[test]
fn a_read_during_policy_publication_cannot_use_the_entry_workflow_for_another_row() {
    for provider in 1..=2 {
        let fixture = Fixture::new(provider);
        let records = fixture.records();
        let original = records.raw_roster.row(&fixture.label).unwrap().clone();
        fixture
            .store
            .transact_pool_logins_within(&[], &fixture.path, NOW, &mut None, |_, _, logins| {
                let mut changed = original.clone();
                changed.browser = Some("Profile 2".into());
                assert!(current_sign_in_task(&original, logins).is_some());
                assert!(current_sign_in_attempt(&original, logins).is_some());
                assert_eq!(current_sign_in_task(&changed, logins), None);
                assert_eq!(current_sign_in_attempt(&changed, logins), None);
                assert_eq!(history(logins, &fixture.label), fixture.original);
                Ok(Vec::new())
            })
            .unwrap();
    }
}

#[test]
fn admitted_work_does_not_adopt_legacy_slots_or_unscoped_events() {
    for provider in 1..=2 {
        let mut fixture = Fixture::with_custody(provider, false);
        let legacy_token = fixture.original_token;
        assert_eq!(fixture.records().logins.workflow_clock, 0);
        let old_event = Event::new(NOW, Kind::SignIn, &fixture.label)
            .because("fixture window closed signed-in");
        fixture.store.machine_id("0123456789abcdef").unwrap();
        let untouched = fixture.images();
        fixture
            .store
            .transact_pool_logins_within(&[], &fixture.path, NOW, &mut None, |_, _, _| {
                Ok(Vec::new())
            })
            .unwrap();
        assert_eq!(
            fixture.images(),
            untouched,
            "an empty admission preserves legacy records"
        );
        fixture
            .store
            .transact_pool_logins_within(&[], &fixture.path, NOW, &mut None, |_, _, logins| {
                admit_workflow(logins, &fixture.label)?;
                Ok(Vec::new())
            })
            .unwrap();
        let admitted = fixture.records();
        let raw = admitted.raw_roster.row(&fixture.label).unwrap();
        assert!(current_workflow(raw, &admitted.logins).is_some());
        assert!(current_sign_in_task(raw, &admitted.logins).is_none());
        assert!(current_sign_in_attempt(raw, &admitted.logins).is_none());
        assert!(!workflow_event_matches(raw, &admitted.logins, &old_event));
        assert_eq!(history(&admitted.logins, &fixture.label), fixture.original);
        fixture
            .store
            .transact_pool_logins_within(&[], &fixture.path, NOW, &mut None, |_, _, logins| {
                admit_workflow(logins, &fixture.label)?;
                note_sign_in_task(logins, &fixture.label, SignInAction::Browser, NOW);
                Ok(Vec::new())
            })
            .unwrap();
        let adopted = fixture.records();
        let raw = adopted.raw_roster.row(&fixture.label).unwrap();
        let adopted_token = current_workflow(raw, &adopted.logins).unwrap();
        assert!(adopted.logins.workflow_clock > 0);
        assert_ne!(adopted_token, legacy_token);
        assert!(!workflow_token_current(raw, &adopted.logins, legacy_token));
        assert!(!workflow_event_matches(raw, &adopted.logins, &old_event));
        let mut scoped_event = old_event.clone();
        scoped_event.workflow = Some(adopted_token);
        assert!(workflow_event_matches(raw, &adopted.logins, &scoped_event));
        fixture.transform(9);
        let moved = fixture.records();
        let raw = moved.raw_roster.row(&fixture.label).unwrap();
        assert_eq!(current_workflow(raw, &moved.logins), Some(adopted_token));
        assert!(!workflow_token_current(raw, &moved.logins, legacy_token));
        assert!(!workflow_event_matches(raw, &moved.logins, &old_event));
        assert!(workflow_event_matches(raw, &moved.logins, &scoped_event));
        assert!(current_sign_in_task(raw, &moved.logins).is_some());
        assert!(current_sign_in_attempt(raw, &moved.logins).is_none());
        fixture.transform(2);
        let replaced = fixture.records();
        let raw = replaced.raw_roster.row(&fixture.label).unwrap();
        assert!(!workflow_token_current(raw, &replaced.logins, legacy_token));
        assert!(!workflow_token_current(
            raw,
            &replaced.logins,
            adopted_token
        ));
        assert!(!workflow_event_matches(raw, &replaced.logins, &old_event));
        let mut original = fixture.original_row.clone();
        original.label.clone_from(&fixture.label);
        fixture.replace_raw(original, false);
        let aba = fixture.records();
        let raw = aba.raw_roster.row(&fixture.label).unwrap();
        assert!(!workflow_token_current(raw, &aba.logins, legacy_token));
        assert!(!workflow_token_current(raw, &aba.logins, adopted_token));
        assert!(!workflow_event_matches(raw, &aba.logins, &old_event));
        assert_eq!(history(&aba.logins, &fixture.label), fixture.original);
    }
}

#[test]
fn erasing_workflow_custody_or_rolling_its_clock_back_refuses_the_whole_publication() {
    for provider in 1..=2 {
        for tamper in 0..3 {
            let fixture = Fixture::new(provider);
            assert!(fixture.records().logins.workflow_clock > 0);
            let before = fixture.images();
            let result = fixture.store.transact_pool_logins_within(
                &[],
                &fixture.path,
                NOW,
                &mut None,
                |_, _, logins| {
                    match tamper {
                        0 => logins.workflow_clock -= 1,
                        1 => logins.workflows = Default::default(),
                        2 => logins.workflow_contexts = WorkflowContexts::default(),
                        _ => unreachable!(),
                    }
                    Ok(Vec::new())
                },
            );
            assert!(result.is_err(), "tamper={tamper}");
            assert_eq!(fixture.images(), before);
        }
    }
}

#[test]
fn reusing_a_moved_from_label_cannot_reuse_its_old_workflow_even_for_the_same_policy() {
    for provider in 1..=2 {
        let mut fixture = Fixture::new(provider);
        let old_token = fixture.original_token;
        fixture.transform(9);
        fixture.replace_raw(fixture.original_row.clone(), false);
        let records = fixture.records();
        let raw = records.raw_roster.row(&fixture.label).unwrap();
        assert_ne!(current_workflow(raw, &records.logins), Some(old_token));
        assert!(!workflow_token_current(raw, &records.logins, old_token));
        assert_eq!(current_sign_in_task(raw, &records.logins), None);
        assert_eq!(current_sign_in_attempt(raw, &records.logins), None);
        assert!(!records.logins.labels.contains_key(&fixture.label));
        assert_eq!(history(&records.logins, "renamed"), fixture.original);
    }
}

#[test]
fn a_kimi_realm_replacement_retires_workflow_guidance_without_erasing_history() {
    let mut fixture = Fixture::new(3);
    let mut row = fixture
        .records()
        .raw_roster
        .row(&fixture.label)
        .unwrap()
        .clone();
    row.vendor.region = Some(Region::MainlandCn);
    fixture.replace_raw(row, false);
    let records = fixture.records();
    let raw = records.raw_roster.row(&fixture.label).unwrap();
    assert_eq!(current_sign_in_task(raw, &records.logins), None);
    assert_eq!(current_sign_in_attempt(raw, &records.logins), None);
    assert_eq!(history(&records.logins, &fixture.label), fixture.original);
}

#[test]
fn unknown_policy_history_needs_admission_and_a_fresh_observation() {
    for provider in 1..=2 {
        for new_login in [false, true] {
            let fixture = Fixture::with_custody(provider, false);
            let legacy = fixture.original_token;
            fixture
                .store
                .transact_logins(NOW, &mut None, |state, logins| {
                    if new_login {
                        Ok(note_renewed(
                            state,
                            logins,
                            &fixture.label,
                            fixture.original_row.dir.as_deref(),
                            fixture.original_row.dir.as_deref().unwrap(),
                            None,
                            NOW,
                        ))
                    } else {
                        note_sign_in_task(logins, &fixture.label, SignInAction::Browser, NOW);
                        Ok(Vec::new())
                    }
                })
                .unwrap();
            let unknown = fixture.records();
            let raw = unknown.raw_roster.row(&fixture.label).unwrap();
            assert_eq!(current_workflow(raw, &unknown.logins), None);
            assert_eq!(current_sign_in_task(raw, &unknown.logins), None);
            assert_eq!(current_sign_in_attempt(raw, &unknown.logins), None);
            let doc: Value = aterm_json::from_str(&unknown.logins.to_json()).unwrap();
            assert_eq!(
                doc["workflows"][fixture.label.as_str()]["policy"],
                Value::Null
            );
            let before = fixture.images();
            fixture
                .store
                .transact_pool_logins_within(&[], &fixture.path, NOW, &mut None, |_, _, _| {
                    Ok(Vec::new())
                })
                .unwrap();
            assert_eq!(fixture.images(), before, "new_login={new_login}");
            fixture
                .store
                .transact_pool_logins_within(&[], &fixture.path, NOW, &mut None, |_, _, logins| {
                    admit_workflow(logins, &fixture.label)?;
                    Ok(Vec::new())
                })
                .unwrap();
            let admitted = fixture.records();
            let raw = admitted.raw_roster.row(&fixture.label).unwrap();
            let token = current_workflow(raw, &admitted.logins).unwrap();
            assert_eq!(current_sign_in_task(raw, &admitted.logins), None);
            assert_eq!(current_sign_in_attempt(raw, &admitted.logins), None);
            fixture
                .store
                .transact_pool_logins_within(&[], &fixture.path, NOW, &mut None, |_, _, logins| {
                    admit_workflow(logins, &fixture.label)?;
                    note_sign_in_task(logins, &fixture.label, SignInAction::Login, NOW + 1);
                    Ok(Vec::new())
                })
                .unwrap();
            let observed = fixture.records();
            let raw = observed.raw_roster.row(&fixture.label).unwrap();
            assert_eq!(current_workflow(raw, &observed.logins), Some(token));
            assert!(!workflow_token_current(raw, &observed.logins, legacy));
            let doc: Value = aterm_json::from_str(&observed.logins.to_json()).unwrap();
            assert!(
                doc["workflows"][fixture.label.as_str()]["policy"]
                    .as_str()
                    .is_some()
            );
        }
    }
}

#[test]
fn v1_known_custody_requires_new_admission_without_adopting_its_old_slots() {
    let root = aterm_tempfile::tempdir().unwrap();
    let store = Store::at(root.path());
    let path = root.path().join("roster.toml");
    let text = "[[account]]\nlabel='work'\nclass='dedicated'\nemail='owner@example.test'\n";
    std::fs::write(&path, text).unwrap();
    let raw = Roster::parse(text).unwrap();
    let row = raw.row("work").unwrap();
    assert!(row.dir.is_none());
    store
        .transact_pool_logins_within(&[], &path, NOW, &mut None, |_, _, logins| {
            admit_workflow(logins, "work")?;
            note_sign_in_task(logins, "work", SignInAction::Browser, NOW);
            Ok(note_sign_in_attempt(
                logins,
                "work",
                AttemptOutcome::Blocked,
                "fixture route",
                NOW,
            ))
        })
        .unwrap();
    let current = store.read_account_records(Some(&path), NOW).unwrap();
    let token = current_workflow(row, &current.logins).unwrap();
    assert!(current_sign_in_task(row, &current.logins).is_some());
    assert!(current_sign_in_attempt(row, &current.logins).is_some());
    // Frozen v1 policy wire for the configured row above. In that format a
    // known map could have adopted slots it never observed; it is not v2 admission.
    let mut digest = aterm_digest::Sha256::new();
    digest.update(b"aterm.sign-in-workflow-policy.v1\0");
    digest.update(br#"{"browser":null,"class":"preferred","email":"owner@example.test","flow":null,"held_by":null,"identity_email":null,"own_dir":null,"region":null}"#);
    let mut wire: Value = aterm_json::from_str(&current.logins.to_json()).unwrap();
    let held = wire
        .as_object_mut()
        .unwrap()
        .get_mut("workflows")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .get_mut("work")
        .unwrap()
        .as_object_mut()
        .unwrap();
    held.insert(
        "policy".into(),
        Value::from(aterm_codec::hex::encode_lower(&digest.finalize())),
    );
    held.insert("legacy_origin".into(), Value::from("work"));
    let old = Logins::from_json(&wire.to_string(), NOW).unwrap();
    assert_eq!(old.labels, current.logins.labels);
    assert_eq!(old.workflow_clock, current.logins.workflow_clock);
    assert_eq!(current_workflow(row, &old), None);
    assert_eq!(current_sign_in_task(row, &old), None);
    assert_eq!(current_sign_in_attempt(row, &old), None);
    let mut event = Event::new(NOW, Kind::SignIn, "work");
    event.workflow = Some(token);
    assert!(!workflow_event_matches(row, &old, &event));
    #[cfg(unix)]
    let recovery = RenewalParent::capture(&current.logins, "work")
        .recovery_scope(row, &current.state, &current.logins)
        .unwrap();
    #[cfg(unix)]
    assert_eq!(
        RenewalParent::capture(&old, "work")
            .recovery_scope(row, &current.state, &old)
            .unwrap(),
        recovery
    );
    store.save_logins(&old).unwrap();
    store
        .transact_pool_logins_within(&[], &path, NOW, &mut None, |_, _, logins| {
            admit_workflow(logins, "work")?;
            Ok(Vec::new())
        })
        .unwrap();
    let admitted = store.read_account_records(Some(&path), NOW).unwrap();
    assert_ne!(current_workflow(row, &admitted.logins).unwrap(), token);
    assert_eq!(admitted.logins.labels, old.labels);
    assert_eq!(current_sign_in_task(row, &admitted.logins), None);
    assert_eq!(current_sign_in_attempt(row, &admitted.logins), None);
    #[cfg(unix)]
    assert_eq!(
        RenewalParent::capture(&admitted.logins, "work")
            .recovery_scope(row, &admitted.state, &admitted.logins)
            .unwrap(),
        recovery
    );
    store
        .transact_pool_logins_within(&[], &path, NOW, &mut None, |_, _, logins| {
            admit_workflow(logins, "work")?;
            note_sign_in_task(logins, "work", SignInAction::Login, NOW);
            Ok(Vec::new())
        })
        .unwrap();
    let fresh = store.read_account_records(Some(&path), NOW).unwrap();
    assert_eq!(
        current_sign_in_task(row, &fresh.logins).unwrap().action,
        SignInAction::Login
    );
    assert_eq!(current_sign_in_attempt(row, &fresh.logins), None);
    #[cfg(unix)]
    assert_eq!(
        RenewalParent::capture(&fresh.logins, "work")
            .recovery_scope(row, &fresh.state, &fresh.logins)
            .unwrap(),
        recovery
    );
}

#[test]
fn known_absent_row_custody_is_not_unknown_policy_and_its_codec_refuses_contradictions() {
    let root = aterm_tempfile::tempdir().unwrap();
    let store = Store::at(root.path());
    let path = root.path().join("roster.toml");
    std::fs::write(&path, "").unwrap();
    store
        .transact_pool_logins_within(&[], &path, NOW, &mut None, |_, _, _| {
            Ok(vec![
                Event::new(NOW, Kind::WrongAccount, "work").because("retained terminal veto"),
            ])
        })
        .unwrap();
    let records = store.read_account_records(Some(&path), NOW).unwrap();
    let row = Row {
        label: "work".into(),
        ..Row::default()
    };
    assert_eq!(current_workflow(&row, &records.logins), None);
    assert!(!records.logins.labels.contains_key("work"));
    let text = records.logins.to_json();
    assert_eq!(Logins::from_json(&text, NOW).unwrap(), records.logins);
    let doc: Value = aterm_json::from_str(&text).unwrap();
    assert_eq!(doc["workflows"]["work"]["row_absent"], Value::Bool(true));
    for (key, value) in [
        ("row_absent", Value::from("true")),
        (
            "policy",
            Value::from(aterm_codec::hex::encode_lower(&[0; 32])),
        ),
    ] {
        let mut bad = doc.clone();
        bad.as_object_mut()
            .unwrap()
            .get_mut("workflows")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .get_mut("work")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(key.into(), value);
        assert!(Logins::from_json(&bad.to_string(), NOW).is_err(), "{key}");
    }
    let mut retained = doc;
    retained
        .as_object_mut()
        .unwrap()
        .get_mut("workflows")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .get_mut("work")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("task_retired".into(), Value::Bool(true));
    store
        .save_logins(&Logins::from_json(&retained.to_string(), NOW).unwrap())
        .unwrap();
    let images = || {
        [
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            store.state_witness_file(),
            store.logins_witness_file(),
            store.wake_path(),
            store.machine_file(),
            path.clone(),
            store.dir.join("sync-commit.json"),
            store.dir.join("sync-commit.seen"),
        ]
        .map(|path| std::fs::read(path).ok())
    };
    let before = images();
    store
        .transact_pool_logins_within(&[], &path, NOW, &mut None, |_, _, _| Ok(Vec::new()))
        .unwrap();
    assert_eq!(
        images(),
        before,
        "absent guidance cannot cause admission normalization"
    );
}

#[test]
fn stored_policy_mismatch_is_not_published_by_a_generic_noop_but_explicit_admission_reconciles_it()
{
    for provider in 1..=2 {
        let fixture = Fixture::new(provider);
        let original = fixture.records();
        let mut row = original.raw_roster.row(&fixture.label).unwrap().clone();
        row.browser = Some("Profile 2".into());
        std::fs::write(
            &fixture.path,
            append_rows_for(fixture.cli, "", std::slice::from_ref(&row)).unwrap(),
        )
        .unwrap();
        let before = fixture.images();
        fixture
            .store
            .transact_pool_logins_within(&[], &fixture.path, NOW, &mut None, |_, _, _| {
                Ok(Vec::new())
            })
            .unwrap();
        assert_eq!(
            fixture.images(),
            before,
            "a rejected/read-only admission has no workflow effects"
        );
        let untouched = fixture.records();
        assert_eq!(untouched.logins, original.logins);
        assert_eq!(current_workflow(&row, &untouched.logins), None);
        fixture
            .store
            .transact_pool_logins_within(&[], &fixture.path, NOW, &mut None, |_, _, logins| {
                admit_workflow(logins, &fixture.label)?;
                Ok(Vec::new())
            })
            .unwrap();
        let reconciled = fixture.records();
        let token = current_workflow(&row, &reconciled.logins).unwrap();
        assert_ne!(token, fixture.original_token);
        assert_eq!(
            history(&reconciled.logins, &fixture.label),
            fixture.original
        );
        assert_eq!(current_sign_in_task(&row, &reconciled.logins), None);
        assert_eq!(current_sign_in_attempt(&row, &reconciled.logins), None);
    }
}

#[test]
fn workflow_admission_cannot_certify_a_later_route_policy_or_verified_login_replacement() {
    for provider in 1..=2 {
        for change in 0..3 {
            let fixture = Fixture::new(provider);
            let before = fixture.images();
            let result = if change == 2 {
                let raw_before = std::fs::read_to_string(&fixture.path).unwrap();
                let mut row = fixture.original_row.clone();
                row.browser = Some("Profile 2".into());
                let raw_after = append_rows_for(fixture.cli, "", &[row]).unwrap();
                let held = fixture.store.acquire().unwrap();
                fixture.store.transact_sync_held(
                    &held,
                    NOW,
                    &mut None,
                    SyncTarget {
                        path: &fixture.path,
                        before: Some(&raw_before),
                        after: Some(&raw_after),
                        mode: None,
                    },
                    |_, logins| {
                        admit_workflow(logins, &fixture.label)?;
                        Ok(Vec::new())
                    },
                    || Ok(()),
                )
            } else {
                fixture.store.transact_pool_logins_within(
                    &[],
                    &fixture.path,
                    NOW,
                    &mut None,
                    |_, state, logins| {
                        admit_workflow(logins, &fixture.label)?;
                        if change == 0 {
                            logins.labels.get_mut(&fixture.label).unwrap().dir = Some(
                                fixture
                                    .store
                                    .logins_dir()
                                    .join("later")
                                    .display()
                                    .to_string(),
                            );
                            Ok(Vec::new())
                        } else {
                            Ok(note_renewed(
                                state,
                                logins,
                                &fixture.label,
                                fixture.original_row.dir.as_deref(),
                                fixture.original_row.dir.as_deref().unwrap(),
                                None,
                                NOW,
                            ))
                        }
                    },
                )
            };
            assert!(result.is_err(), "change={change}");
            assert_eq!(fixture.images(), before, "change={change}");
        }
    }
}

#[test]
fn a_current_admission_cannot_launder_a_captured_event_after_policy_aba() {
    for provider in 1..=2 {
        let mut fixture = Fixture::new(provider);
        let mut event = Event::new(NOW, Kind::SignIn, &fixture.label)
            .because("fixture window done session-kept");
        event.workflow = Some(fixture.original_token);
        fixture.transform(2);
        fixture.back(2);
        let before = fixture.images();
        let result = fixture.store.transact_pool_logins_within(
            &[],
            &fixture.path,
            NOW,
            &mut None,
            |_, _, logins| {
                admit_workflow(logins, &fixture.label)?;
                Ok(vec![event])
            },
        );
        assert!(result.is_err());
        assert_eq!(fixture.images(), before);
    }
}
