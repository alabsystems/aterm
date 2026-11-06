// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;
const DAY: i64 = 86_400;

type ClockHistory = (Option<i64>, Option<i64>, Option<i64>, Option<i64>, i64);

fn history(logins: &Logins, label: &str) -> ClockHistory {
    let binding = logins.get(label).unwrap();
    (
        binding.renewed_at,
        binding.notice_end,
        binding.served_at,
        binding.ended_at,
        binding.lifetime_s,
    )
}

struct Fixture {
    _root: aterm_tempfile::TempDir,
    store: Store,
    roster: PathBuf,
    label: String,
    original: ClockHistory,
}

impl Fixture {
    fn new(has_auth: bool) -> Self {
        Self::with_times(has_auth, NOW - 2 * DAY, Some(NOW), Some(NOW - 1), true)
    }

    fn with_times(
        has_auth: bool,
        renewed: i64,
        served: Option<i64>,
        ended: Option<i64>,
        notice: bool,
    ) -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let store = Store::at_for(root.path(), Cli::Claude, root.path());
        let roster = root.path().join("roster.toml");
        let row = Row {
            label: "work".into(),
            email: Some("owner-a@example.test".into()),
            ..Default::default()
        };
        std::fs::write(&roster, append_rows_for(Cli::Claude, "", &[row]).unwrap()).unwrap();
        let old = root.path().join("old-login");
        store
            .transact_pool_logins_within(&[], &roster, NOW, &mut None, |_, state, logins| {
                let mut events = note_renewed(
                    state,
                    logins,
                    "work",
                    None,
                    old.to_str().unwrap(),
                    None,
                    renewed,
                );
                if notice {
                    note_login_notice(logins, "work", 1, NOW);
                }
                if let Some(at) = served {
                    note_served_login(logins, "work", at);
                }
                if let Some(at) = ended {
                    events.extend(note_login_end(state, logins, "work", at));
                }
                events.extend(note_auth_in_context(
                    state,
                    logins,
                    "work",
                    Auth::Ok,
                    ended.unwrap_or(NOW).max(NOW),
                ));
                if !has_auth {
                    state.accounts.remove("work");
                }
                Ok(events)
            })
            .unwrap();
        let records = store.read_account_records(Some(&roster), NOW).unwrap();
        assert_eq!(records.state.accounts.contains_key("work"), has_auth);
        if let Some(account) = records.state.accounts.get("work") {
            assert!(matches!(account.auth_projection, AuthProjection::Legacy));
        }
        let original = history(&records.logins, "work");
        Self {
            _root: root,
            store,
            roster,
            label: "work".into(),
            original,
        }
    }

    fn records(&self) -> AccountRecords {
        self.store
            .read_account_records(Some(&self.roster), NOW)
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
            self.roster.clone(),
            self.store.dir.join("sync-commit.json"),
            self.store.dir.join("sync-commit.seen"),
        ]
        .map(|path| std::fs::read(path).ok())
        .into()
    }

    fn transform(&mut self, transform: i64) {
        if transform == 1 {
            self.store
                .transact_pool_logins_within(&[], &self.roster, NOW, &mut None, |_, _, logins| {
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
        } else if transform == 3 {
            let before = self.store.load_logins(NOW).unwrap().0;
            let generation = before.get(&self.label).unwrap().renewal_generation;
            self.store
                .transact_pool_logins_within(&[], &self.roster, NOW, &mut None, |_, _, logins| {
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
                            identity: "owner-a@example.test".into(),
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
                self.store
                    .load_logins(NOW)
                    .unwrap()
                    .0
                    .get(&self.label)
                    .unwrap()
                    .renewal_generation
                    > generation
            );
        } else {
            let before = std::fs::read_to_string(&self.roster).unwrap();
            let mut raw = Roster::parse_for(Cli::Claude, &before).unwrap();
            if transform == 2 {
                raw.rows[0].email = Some("owner-b@example.test".into());
            } else {
                assert_eq!(transform, 4);
                raw.rows[0].label = "renamed".into();
            }
            let after = append_rows_for(Cli::Claude, "", &raw.rows).unwrap();
            let held = self.store.acquire().unwrap();
            self.store
                .transact_sync_held(
                    &held,
                    NOW,
                    &mut None,
                    SyncTarget {
                        path: &self.roster,
                        before: Some(&before),
                        after: Some(&after),
                        mode: None,
                    },
                    |state, logins| {
                        if transform == 4 {
                            move_account(state, logins, &self.label, "renamed")?;
                        }
                        Ok(Vec::new())
                    },
                    || Ok(()),
                )
                .unwrap();
            if transform == 4 {
                self.label = "renamed".into();
            }
        }
    }

    fn auth_only(&self) {
        self.store
            .transact_pool_logins_within(&[], &self.roster, NOW, &mut None, |_, state, logins| {
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

    fn fresh(&self, phase: i64) {
        self.store
            .transact_pool_logins_within(&[], &self.roster, NOW, &mut None, |_, state, logins| {
                match phase {
                    4 => {
                        note_served_login(logins, &self.label, NOW);
                        Ok(Vec::new())
                    }
                    5 => {
                        note_login_notice(logins, &self.label, 2, NOW);
                        Ok(Vec::new())
                    }
                    6 => {
                        let dir = logins.get(&self.label).unwrap().dir.clone().unwrap();
                        Ok(note_renewed(
                            state,
                            logins,
                            &self.label,
                            Some(&dir),
                            &dir,
                            None,
                            NOW,
                        ))
                    }
                    _ => unreachable!(),
                }
            })
            .unwrap();
    }

    fn observe(&self) -> Vec<(&'static str, i64)> {
        let before = self.images();
        let records = self.records();
        let row = records.roster.row(&self.label).unwrap();
        let by = login_forecast(row, &records.logins, &records.state);
        let status = login_state(row, &records.logins, &records.state, NOW);
        let enrolled = [self.label.clone()].into();
        let deadline = match by {
            None => 0,
            Some(at) if at == NOW + DAY => 1,
            Some(at) if at == NOW + 2 * DAY => 2,
            Some(at) if at == NOW + LIFETIME_MIN_S => 3,
            other => panic!("unexpected current forecast {other:?}"),
        };
        vec![
            ("forecast", i64::from(by.is_some())),
            (
                "served",
                i64::from(login_served_at(row, &records.logins, &records.state).is_some()),
            ),
            (
                "ended",
                i64::from(login_ended_at(row, &records.logins, &records.state).is_some()),
            ),
            ("deadline", deadline),
            ("due", i64::from(matches!(status, LoginState::Due { .. }))),
            ("ok", i64::from(status == LoginState::Ok)),
            (
                "renew",
                // The lifetime law alone: whether this Mac uses the login ([`used_here`]) is
                // the upkeep's separate condition, which this model does not hold.
                i64::from(without_use_condition(|| {
                    renew_due(
                        &records.roster,
                        &records.logins,
                        &records.state,
                        &crate::harness::accounts::OtherMacs::NONE,
                        NOW,
                    )
                    .contains(&self.label)
                })),
            ),
            (
                "closing",
                i64::from(none_working_within(
                    &records.roster,
                    &records.logins,
                    &records.state,
                    &enrolled,
                    NOW,
                    DUE_S,
                )),
            ),
            ("read_only", i64::from(before == self.images())),
        ]
    }
}

/// Replay the actual publication prefix for each finite model transition.
pub(crate) fn lifetime_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    if action == "AnotherTransform" {
        next.insert("transform", state["transform"] + 1);
        return next;
    }
    if action == "WithAuth" {
        next.insert("has_auth", 1);
        return next;
    }
    let mut fixture = Fixture::new(state["has_auth"] == 1);
    if state["phase"] >= 2 {
        fixture.transform(state["transform"]);
    }
    if state["phase"] >= 3 {
        fixture.auth_only();
    }
    if state["phase"] >= 4 {
        fixture.fresh(state["phase"]);
    }
    match action {
        "Seed" => {
            next.insert("phase", 1);
            next.insert("read", 0);
        }
        "Transform" => {
            fixture.transform(state["transform"]);
            next.insert("phase", 2);
            next.insert("read", 0);
            next.insert(
                "history",
                i64::from(history(&fixture.records().logins, &fixture.label) == fixture.original),
            );
        }
        "AuthOnly" => {
            fixture.auth_only();
            next.insert("phase", 3);
            next.insert("read", 0);
            next.insert(
                "history",
                i64::from(history(&fixture.records().logins, &fixture.label) == fixture.original),
            );
        }
        "Served" | "Notice" | "Renewed" => {
            let phase = match action {
                "Served" => 4,
                "Notice" => 5,
                "Renewed" => 6,
                _ => unreachable!(),
            };
            fixture.fresh(phase);
            next.insert("phase", phase);
            next.insert("read", 0);
        }
        "StaleServed" => {
            let before = fixture.images();
            let result = fixture.store.transact_pool_logins_within(
                &[],
                &fixture.roster,
                NOW,
                &mut None,
                |_, _, logins| {
                    note_served_login(logins, &fixture.label, NOW);
                    logins.labels.get_mut(&fixture.label).unwrap().dir = Some(
                        fixture
                            .store
                            .logins_dir()
                            .join("later-login")
                            .display()
                            .to_string(),
                    );
                    Ok(Vec::new())
                },
            );
            next.insert("phase", 7);
            next.insert("read", 0);
            next.insert("accepted", i64::from(result.is_ok()));
            next.insert("preserved", i64::from(before == fixture.images()));
        }
        "Read" => {
            next.insert("read", 1);
            for (key, value) in fixture.observe() {
                next.insert(key, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn legacy_lifetime_observations_conform_without_auth_or_clock_assumptions() {
    let model = aterm_models_harness::account_lifetime_scope_model();
    assert_eq!(model.mutant_dials(), (1..=20).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = lifetime_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut state = model.init_state();
    state.insert("phase", 3);
    state.insert("read", 1);
    for (field, value, law) in [
        ("forecast", 1, "OldForecastRetired"),
        ("served", 1, "OldServedRetired"),
        ("ended", 1, "OldEndRetired"),
        ("history", 0, "ReplacementRetainsRawHistory"),
        ("read_only", 0, "ReadsDoNotWrite"),
        ("renew", 1, "RenewalQueueUsesCurrentEvidence"),
        ("closing", 1, "RunwayUsesCurrentEvidence"),
    ] {
        let mut bad = state.clone();
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad), "{law}");
    }
    state.insert("phase", 7);
    state.insert("read", 0);
    let mut accepted = state.clone();
    accepted.insert("accepted", 1);
    assert!(!model.check_invariant("StaleClockPublicationRefused", &accepted));
    state.insert("preserved", 0);
    assert!(!model.check_invariant("RefusalPreservesPublicationImages", &state));
}

#[test]
fn restoring_retired_clock_authority_reproduces_the_real_renewal_bug() {
    for has_auth in [false, true] {
        for transform in [1, 2] {
            let mut fixture = Fixture::new(has_auth);
            fixture.transform(transform);
            fixture.auth_only();
            let records = fixture.records();
            let row = records.roster.row(&fixture.label).unwrap();
            assert_eq!(history(&records.logins, &fixture.label), fixture.original);
            assert_eq!(login_forecast(row, &records.logins, &records.state), None);
            assert_eq!(login_served_at(row, &records.logins, &records.state), None);
            assert_eq!(login_ended_at(row, &records.logins, &records.state), None);
            assert_eq!(
                login_state(row, &records.logins, &records.state, NOW),
                LoginState::Idle
            );
            let mut counterfactual = records.logins.clone();
            counterfactual
                .labels
                .get_mut(&fixture.label)
                .unwrap()
                .lifetime_scope = Default::default();
            assert_eq!(
                login_forecast(row, &counterfactual, &records.state),
                Some(NOW + DAY)
            );
            assert_eq!(
                login_served_at(row, &counterfactual, &records.state),
                Some(NOW)
            );
            assert_eq!(
                login_ended_at(row, &counterfactual, &records.state),
                Some(NOW - 1)
            );
            assert!(matches!(
                login_state(row, &counterfactual, &records.state, NOW),
                LoginState::Due { .. }
            ));
            assert_eq!(history(&counterfactual, &fixture.label), fixture.original);
        }
    }
}

#[test]
fn lifetime_scope_round_trips_and_malformed_presence_cannot_assert_authority() {
    let mut fixture = Fixture::new(false);
    let original = fixture.records();
    let row = original.roster.row(&fixture.label).unwrap();
    let legacy = original.logins.to_json();
    for key in [
        "renewal_retired",
        "notice_retired",
        "served_retired",
        "ended_retired",
    ] {
        assert!(!legacy.contains(key));
    }
    for bad in [
        Value::Null,
        Value::from("false"),
        Value::from(0),
        Value::Array(Vec::new()),
    ] {
        let mut document: Value = aterm_json::from_str(&legacy).unwrap();
        let binding = document
            .as_object_mut()
            .unwrap()
            .get_mut("labels")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .get_mut(&fixture.label)
            .unwrap()
            .as_object_mut()
            .unwrap();
        for key in [
            "renewal_retired",
            "notice_retired",
            "served_retired",
            "ended_retired",
        ] {
            binding.insert(key.into(), bad.clone());
        }
        let read = Logins::from_json(&document.to_string(), NOW).unwrap();
        assert_eq!(history(&read, &fixture.label), fixture.original);
        assert_eq!(login_forecast(row, &read, &original.state), None);
        assert_eq!(login_served_at(row, &read, &original.state), None);
        assert_eq!(login_ended_at(row, &read, &original.state), None);
        assert_eq!(Logins::from_json(&read.to_json(), NOW).unwrap(), read);
    }
    fixture.transform(1);
    let retired = fixture.records();
    let encoded = retired.logins.to_json();
    let decoded = Logins::from_json(&encoded, NOW).unwrap();
    assert_eq!(decoded, retired.logins);
    assert_eq!(history(&decoded, &fixture.label), fixture.original);
    let row = retired.roster.row(&fixture.label).unwrap();
    assert_eq!(login_forecast(row, &decoded, &retired.state), None);
    assert_eq!(login_served_at(row, &decoded, &retired.state), None);
    assert_eq!(login_ended_at(row, &decoded, &retired.state), None);
}

#[test]
fn fresh_lifetime_evidence_does_not_wait_for_an_auth_word() {
    for has_auth in [false, true] {
        for served in [false, true] {
            let mut fixture = Fixture::new(has_auth);
            fixture.transform(1);
            fixture
                .store
                .transact_pool_logins_within(
                    &[],
                    &fixture.roster,
                    NOW,
                    &mut None,
                    |_, _, logins| {
                        if served {
                            note_served_login(logins, &fixture.label, NOW);
                        } else {
                            note_login_notice(logins, &fixture.label, 1, NOW);
                        }
                        Ok(Vec::new())
                    },
                )
                .unwrap();
            let records = fixture.records();
            let row = records.roster.row(&fixture.label).unwrap();
            assert_eq!(
                records.state.account(&fixture.label).current_auth(row),
                Auth::Unseen
            );
            if served {
                assert_eq!(
                    login_state(row, &records.logins, &records.state, NOW),
                    LoginState::Ok
                );
                assert_eq!(login_forecast(row, &records.logins, &records.state), None);
            } else {
                assert_eq!(
                    login_state(row, &records.logins, &records.state, NOW),
                    LoginState::Due { by: NOW + DAY }
                );
                assert_eq!(login_served_at(row, &records.logins, &records.state), None);
            }
        }
    }
}

#[test]
fn a_fresh_served_turn_replaces_an_old_contexts_future_maximum() {
    let mut fixture = Fixture::with_times(true, NOW - 2 * DAY, Some(NOW + DAY), None, true);
    fixture.transform(1);
    fixture.auth_only();
    fixture
        .store
        .transact_pool_logins_within(&[], &fixture.roster, NOW, &mut None, |_, _, logins| {
            note_served_login(logins, &fixture.label, NOW - 1);
            Ok(Vec::new())
        })
        .unwrap();
    let records = fixture.records();
    let row = records.roster.row(&fixture.label).unwrap();
    assert_eq!(
        records.logins.get(&fixture.label).unwrap().served_at,
        Some(NOW - 1)
    );
    assert_eq!(
        login_served_at(row, &records.logins, &records.state),
        Some(NOW - 1)
    );
    assert_eq!(login_forecast(row, &records.logins, &records.state), None);
    assert_eq!(
        login_state(row, &records.logins, &records.state, NOW),
        LoginState::Ok
    );
}

#[test]
fn fresh_notice_and_end_never_calibrate_from_a_retired_renewal() {
    for end in [false, true] {
        // The first end was too early to calibrate. Pairing the replacement's fresh end or
        // notice with this old renewal would incorrectly lower the learned 21-day lifetime.
        let mut fixture =
            Fixture::with_times(true, NOW - 10 * DAY, None, Some(NOW - 9 * DAY), false);
        assert_eq!(fixture.original.4, LIFETIME_START_S);
        fixture.transform(1);
        fixture.auth_only();
        fixture
            .store
            .transact_pool_logins_within(
                &[],
                &fixture.roster,
                NOW,
                &mut None,
                |_, state, logins| {
                    if end {
                        Ok(note_login_end(state, logins, &fixture.label, NOW))
                    } else {
                        note_login_notice(logins, &fixture.label, 1, NOW);
                        Ok(Vec::new())
                    }
                },
            )
            .unwrap();
        let records = fixture.records();
        let row = records.roster.row(&fixture.label).unwrap();
        let binding = records.logins.get(&fixture.label).unwrap();
        assert_eq!(binding.lifetime_s, LIFETIME_START_S);
        assert_eq!(binding.renewed_at, Some(NOW - 10 * DAY));
        if end {
            assert_eq!(binding.ended_at, Some(NOW));
            assert_eq!(
                login_ended_at(row, &records.logins, &records.state),
                Some(NOW)
            );
            assert_eq!(
                login_state(row, &records.logins, &records.state, NOW),
                LoginState::Expired
            );
        } else {
            assert_eq!(
                login_forecast(row, &records.logins, &records.state),
                Some(NOW + DAY)
            );
            assert_eq!(login_ended_at(row, &records.logins, &records.state), None);
        }
    }
}

#[test]
fn same_path_same_time_renewal_never_invents_a_served_turn() {
    for serve_after in [false, true] {
        let fixture = Fixture::with_times(true, NOW, Some(NOW), None, true);
        fixture
            .store
            .transact_pool_logins_within(
                &[],
                &fixture.roster,
                NOW,
                &mut None,
                |_, state, logins| {
                    let dir = logins.get(&fixture.label).unwrap().dir.clone().unwrap();
                    let events =
                        note_renewed(state, logins, &fixture.label, Some(&dir), &dir, None, NOW);
                    if serve_after {
                        note_served_login(logins, &fixture.label, NOW);
                    }
                    Ok(events)
                },
            )
            .unwrap();
        let records = fixture.records();
        let row = records.roster.row(&fixture.label).unwrap();
        assert_eq!(
            records.logins.get(&fixture.label).unwrap().served_at,
            Some(NOW)
        );
        assert_eq!(
            login_forecast(row, &records.logins, &records.state),
            Some(NOW + LIFETIME_MIN_S)
        );
        assert_eq!(
            login_served_at(row, &records.logins, &records.state),
            serve_after.then_some(NOW)
        );
        assert_eq!(
            login_state(row, &records.logins, &records.state, NOW),
            if serve_after {
                LoginState::Ok
            } else {
                LoginState::Idle
            }
        );
    }
}

#[test]
fn an_entry_clock_result_cannot_certify_a_later_policy_even_when_its_value_did_not_change() {
    for has_auth in [false, true] {
        for notice in [false, true] {
            let fixture = Fixture::new(has_auth);
            let before = std::fs::read_to_string(&fixture.roster).unwrap();
            let mut raw = Roster::parse_for(Cli::Claude, &before).unwrap();
            raw.rows[0].email = Some("owner-b@example.test".into());
            let after = append_rows_for(Cli::Claude, "", &raw.rows).unwrap();
            let images = fixture.images();
            let held = fixture.store.acquire().unwrap();
            let result = fixture.store.transact_sync_held(
                &held,
                NOW,
                &mut None,
                SyncTarget {
                    path: &fixture.roster,
                    before: Some(&before),
                    after: Some(&after),
                    mode: None,
                },
                |_, logins| {
                    if notice {
                        note_login_notice(logins, &fixture.label, 1, NOW);
                    } else {
                        note_served_login(logins, &fixture.label, NOW);
                    }
                    Ok(Vec::new())
                },
                || Ok(()),
            );
            assert!(result.is_err());
            assert_eq!(fixture.images(), images);
            assert!(!fixture.store.sync_needs_recovery());
        }
    }
}

#[test]
fn an_explicit_clock_observation_follows_only_the_policy_actually_inspected() {
    for later in [false, true] {
        let fixture = Fixture::new(false);
        let before = std::fs::read_to_string(&fixture.roster).unwrap();
        let mut observed = Roster::parse_for(Cli::Claude, &before).unwrap();
        observed.rows[0].email = Some("owner-b@example.test".into());
        let mut target = observed.clone();
        if later {
            target.rows[0].email = Some("owner-c@example.test".into());
        }
        let after = append_rows_for(Cli::Claude, "", &target.rows).unwrap();
        let images = fixture.images();
        let held = fixture.store.acquire().unwrap();
        let result = fixture.store.transact_sync_held(
            &held,
            NOW,
            &mut None,
            SyncTarget {
                path: &fixture.roster,
                before: Some(&before),
                after: Some(&after),
                mode: None,
            },
            |_, logins| {
                let row = bound(&observed, logins).rows.remove(0);
                note_served_login_for_observed_row(logins, &row, NOW);
                Ok(Vec::new())
            },
            || Ok(()),
        );
        if later {
            assert!(result.is_err());
            assert_eq!(fixture.images(), images);
            assert!(!fixture.store.sync_needs_recovery());
        } else {
            result.unwrap();
            drop(held);
            let records = fixture.records();
            let row = records.roster.row(&fixture.label).unwrap();
            assert_eq!(
                login_served_at(row, &records.logins, &records.state),
                Some(NOW)
            );
            assert_eq!(login_forecast(row, &records.logins, &records.state), None);
        }
    }
}

#[test]
fn an_explicit_clock_observation_keeps_the_inspected_route_despite_a_different_binding() {
    for target in 0..3 {
        let fixture = Fixture::new(false);
        let records = fixture.records();
        let mut observed = records.roster.row(&fixture.label).unwrap().clone();
        let inspected = fixture
            .store
            .logins_dir()
            .join("actually-observed")
            .display()
            .to_string();
        observed.dir = Some(inspected.clone());
        let images = fixture.images();
        let result = fixture.store.transact_pool_logins_within(
            &[],
            &fixture.roster,
            NOW,
            &mut None,
            |_, _, logins| {
                note_served_login_for_observed_row(logins, &observed, NOW);
                if target > 0 {
                    logins.labels.get_mut(&fixture.label).unwrap().dir = Some(if target == 1 {
                        inspected.clone()
                    } else {
                        fixture
                            .store
                            .logins_dir()
                            .join("later-login")
                            .display()
                            .to_string()
                    });
                }
                Ok(Vec::new())
            },
        );
        if target == 1 {
            result.unwrap();
            let records = fixture.records();
            let row = records.roster.row(&fixture.label).unwrap();
            assert_eq!(row.dir, Some(inspected));
            assert_eq!(
                login_served_at(row, &records.logins, &records.state),
                Some(NOW)
            );
            assert_eq!(login_forecast(row, &records.logins, &records.state), None);
            assert_eq!(history(&records.logins, &fixture.label), fixture.original);
        } else {
            assert!(result.is_err());
            assert_eq!(fixture.images(), images);
            assert!(!fixture.store.sync_needs_recovery());
        }
    }
}

#[test]
fn reads_inside_an_admitted_edit_cannot_lend_entry_clocks_to_the_next_row() {
    for has_auth in [false, true] {
        let fixture =
            Fixture::with_times(has_auth, NOW - 2 * DAY, Some(NOW), Some(NOW + DAY), true);
        // Loading normalizes a future end to the read clock. An end at the sync instant
        // would still block adoption if the next row inherited its authority.
        assert_eq!(
            fixture
                .records()
                .logins
                .get(&fixture.label)
                .unwrap()
                .ended_at,
            Some(NOW)
        );
        let before = std::fs::read_to_string(&fixture.roster).unwrap();
        let old = Roster::parse_for(Cli::Claude, &before).unwrap();
        let mut next = old.clone();
        next.rows[0].email = Some("owner-b@example.test".into());
        let after = append_rows_for(Cli::Claude, "", &next.rows).unwrap();
        let held = fixture.store.acquire().unwrap();
        fixture
            .store
            .transact_sync_held(
                &held,
                NOW,
                &mut None,
                SyncTarget {
                    path: &fixture.roster,
                    before: Some(&before),
                    after: Some(&after),
                    mode: None,
                },
                |state, logins| {
                    let old_row = bound(&old, logins).rows.remove(0);
                    let next_row = bound(&next, logins).rows.remove(0);
                    assert_eq!(login_ended_at(&old_row, logins, state), Some(NOW));
                    assert_eq!(login_ended_at(&next_row, logins, state), None);
                    assert_eq!(logins.get(&fixture.label).unwrap().ended_at, Some(NOW));
                    Ok(Vec::new())
                },
                || Ok(()),
            )
            .unwrap();
        drop(held);
        let records = fixture.records();
        let row = records.roster.row(&fixture.label).unwrap();
        assert_eq!(login_ended_at(row, &records.logins, &records.state), None);
        assert_eq!(
            records.logins.get(&fixture.label).unwrap().ended_at,
            Some(NOW)
        );
    }
}
