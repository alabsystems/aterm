// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::account_progress::ActiveSignIn;
use aterm_agent::harness::accounts_sync::{LoginActivity, LoginActivityBatch, LoginDiagnostic};
use retired_activity_tests::{Prepared, assert_retained, entry, files, prepare, read};

pub(super) fn primary(value: &AccountProgress) -> AccountProgress {
    let mut primary = value.clone();
    primary.independent_work.clear();
    primary
}

pub(super) fn expected_work(row: &accounts::Row, observed_at: i64) -> ActiveSignIn {
    ActiveSignIn {
        label: row.label.clone(),
        route: Some("mailbox".into()),
        observed_at,
    }
}

fn batch_read(
    prepared: &Prepared,
    batch: impl FnOnce([Option<&accounts::Store>; 2], accounts::Cli, i64) -> Option<LoginActivityBatch>,
) -> Result<ProgressRead, ()> {
    prepared.fixture.host.account_progress_read_with_batch(
        "s-progress",
        4242,
        Some(100),
        Some("conversation-a"),
        || (Instant::now(), Some(200)),
        batch,
    )
}

fn scoped_read(
    prepared: &Prepared,
    primary_entries: Vec<LoginActivity>,
    current_entries: Vec<LoginActivity>,
) -> ProgressRead {
    let primary_store = accounts::Store::at(&prepared.fixture.seat.state);
    let current_store = accounts::Store::at(&prepared.context.state);
    if primary_store.dir == current_store.dir {
        assert_eq!(
            primary_entries, current_entries,
            "one scope has one immutable snapshot"
        );
    }
    batch_read(prepared, |requested, cli, now| {
        assert_eq!(cli, accounts::Cli::Claude);
        assert_eq!(now, 200);
        Some(LoginActivityBatch::for_test(
            requested.map(|store| {
                store.map(|store| {
                    let entries = if store.dir == primary_store.dir {
                        primary_entries.clone()
                    } else {
                        assert_eq!(store.dir, current_store.dir);
                        current_entries.clone()
                    };
                    (store, entries)
                })
            }),
            cli,
        ))
    })
    .unwrap()
}

fn second_namespace(prepared: &mut Prepared) -> accounts::Store {
    let row = prepared.current.clone();
    let store = second_namespace_with_row(prepared, &row);
    let current = accounts::Store::at(&prepared.fixture.seat.state)
        .read_account_records(prepared.fixture.seat.roster.as_deref(), 200)
        .unwrap();
    assert_eq!(
        current.roster.rows.as_slice(),
        prepared.context.rows.as_ref()
    );
    store
}

fn second_namespace_with_row(prepared: &mut Prepared, row: &accounts::Row) -> accounts::Store {
    let mut seat = prepared.fixture.seat.clone();
    seat.state = seat.state.parent().unwrap().join("independent-state");
    let store = accounts::Store::at(&seat.state);
    store
        .transact_logins_within(&[], 200, &mut None, |_, logins| {
            logins.labels.entry(row.label.clone()).or_default().dir = row.dir.clone();
            Ok(Vec::new())
        })
        .unwrap();
    let records = store
        .read_account_records(seat.roster.as_deref(), 200)
        .unwrap();
    assert!(records.notes.is_empty());
    assert_eq!(records.roster.row(&row.label), Some(row));
    *prepared.fixture.live_seat.lock().unwrap() = Some(seat.clone());
    let capture =
        activity::ActivityCapture::capture(&prepared.fixture.kept.lock().unwrap()).unwrap();
    let context = capture.read_for_test(&seat, 100);
    capture.publish_for_test(
        &prepared.fixture.kept,
        &prepared.fixture.host.hooks,
        &seat,
        context,
        Some(100),
    );
    prepared.context = prepared
        .fixture
        .kept
        .lock()
        .unwrap()
        .account
        .activity_context
        .clone()
        .unwrap();
    assert_eq!(prepared.context.state, seat.state);
    store
}

fn store_images(store: &accounts::Store, roster: &Path) -> Vec<Option<Vec<u8>>> {
    [
        store.state_file(),
        store.state_witness_file(),
        store.logins_file(),
        store.logins_witness_file(),
        store.ledger_path(),
        store.wake_path(),
        store.machine_file(),
        store.dir.join("sync-commit.json"),
        store.dir.join("sync-commit.seen"),
        roster.to_path_buf(),
    ]
    .map(|path| std::fs::read(path).ok())
    .into()
}

#[test]
fn independent_pool_work_is_visible_without_replacing_a_current_account_decision() {
    let prepared = prepare(0, None);
    let value = read(
        &prepared,
        Instant::now(),
        Some(vec![entry(&prepared.current, true)]),
    )
    .unwrap()
    .progress
    .unwrap();
    assert_eq!(
        primary(&value),
        prepared.retained,
        "independent work is not a new account judgment"
    );
    assert_eq!(
        value.independent_work,
        [expected_work(&prepared.current, 200)]
    );
    let kept = prepared.fixture.kept.lock().unwrap();
    assert_eq!(
        kept.account.progress.as_ref().unwrap().value,
        prepared.retained
    );
    assert_eq!(
        kept.account.activity_context.as_ref().unwrap().rows,
        prepared.context.rows,
    );
    drop(kept);
    let text = aterm_agent::harness::footer::account_progress_text(&value);
    assert!(
        text.to_ascii_lowercase().contains("sign")
            && text.contains(&prepared.current.label)
            && text.contains("mailbox"),
        "the primary decision hid current independent sign-in work: {text}"
    );
    for private in [
        prepared.current.email.as_deref(),
        prepared.current.dir.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        assert!(
            !text.contains(private),
            "independent work exposed private account metadata"
        );
    }
}

#[test]
fn a_completed_independent_sign_in_keeps_its_exact_diagnostic_beside_a_current_decision() {
    let prepared = prepare(0, None);
    let images = files(&prepared);
    let completed = entry(&prepared.current, false);
    let diagnostic = completed.diagnostic.clone().unwrap();
    let finished_at = diagnostic.completed_at;
    let clock = Instant::now();
    let observation = read(&prepared, clock, Some(vec![completed])).unwrap();
    let value = observation.progress.as_ref().unwrap();
    let mut without_history = value.clone();
    without_history.login_diagnostics.clear();
    assert_eq!(
        without_history, prepared.retained,
        "a completed B invocation cannot change A's recorded decision or authority"
    );
    assert!(value.independent_work.is_empty());
    assert!(!observation.activity_contended);
    assert!(!observation.retry());
    assert_eq!(observation.execution_until, None);
    assert_eq!(observation.next_poll(clock), None);
    assert_retained(&prepared);
    assert_eq!(files(&prepared), images);
    assert!(
        prepared
            .fixture
            .kept
            .lock()
            .unwrap()
            .account
            .execution_progress
            .is_none()
    );
    assert_eq!(
        value.login_diagnostics.as_slice(),
        std::slice::from_ref(&diagnostic),
        "the current B outcome is available without replacing A's captured roster"
    );
    assert_eq!(value.login_diagnostics[0].completed_at, finished_at);
}

fn retain_history(prepared: &mut Prepared, diagnostic: LoginDiagnostic) {
    prepared.retained.login_diagnostics.push(diagnostic.clone());
    let mut kept = prepared.fixture.kept.lock().unwrap();
    let decision = if kept.account.execution_progress.is_some() {
        kept.account.execution_progress.as_mut().unwrap()
    } else {
        kept.account.progress.as_mut().unwrap()
    };
    decision.value.login_diagnostics.push(diagnostic);
}

fn assert_only_history_differs(value: &AccountProgress, recorded: &AccountProgress) {
    let mut authority = value.clone();
    authority
        .login_diagnostics
        .clone_from(&recorded.login_diagnostics);
    assert_eq!(
        &authority, recorded,
        "only the historical outcome vector may differ"
    );
}

#[test]
fn settled_independent_outcomes_preserve_prior_history_and_live_gate_authority() {
    for execution in [false, true] {
        for (exit, finished) in [(1, 199), (0, 200)] {
            let clock = Instant::now();
            let until = execution.then_some(clock + Duration::from_secs(500));
            let mut prepared = prepare(0, until);
            let prior = LoginDiagnostic::from_exit_for_test(&prepared.old, 150, 3, None);
            retain_history(&mut prepared, prior.clone());
            let mut completed = entry(&prepared.current, false);
            let diagnostic =
                LoginDiagnostic::from_exit_for_test(&prepared.current, finished, exit, None);
            completed.diagnostic = Some(diagnostic.clone());
            let images = files(&prepared);
            let got = read(&prepared, clock, Some(vec![completed])).unwrap();
            let value = got.progress.as_ref().unwrap();
            assert_eq!(value.login_diagnostics, [prior, diagnostic]);
            assert_only_history_differs(value, &prepared.retained);
            assert!(value.independent_work.is_empty());
            assert!(!got.retry());
            assert!(!got.activity_contended);
            assert_eq!(got.execution_until, until);
            assert_eq!(got.next_poll(clock), until);
            assert_retained(&prepared);
            assert_eq!(files(&prepared), images);
        }
    }
}

#[test]
fn settled_independent_history_requires_a_valid_diagnostic_and_current_exact_context() {
    for case in 0..10 {
        let mut prepared = prepare(0, None);
        let prior = LoginDiagnostic::from_exit_for_test(&prepared.old, 150, 3, None);
        retain_history(&mut prepared, prior);
        let mut completed = entry(&prepared.current, false);
        match case {
            0 => completed.identity = None,
            1 => {
                prepared
                    .fixture
                    .kept
                    .lock()
                    .unwrap()
                    .account
                    .activity_context
                    .as_mut()
                    .unwrap()
                    .rows = vec![prepared.current.clone(), prepared.current.clone()].into();
            }
            2 => {
                prepared
                    .fixture
                    .kept
                    .lock()
                    .unwrap()
                    .account
                    .activity_context
                    .as_mut()
                    .unwrap()
                    .policy = Arc::new(())
            }
            3 => {
                prepared
                    .fixture
                    .kept
                    .lock()
                    .unwrap()
                    .account
                    .activity_context
                    .as_mut()
                    .unwrap()
                    .generation = Arc::default()
            }
            4 => {
                prepared
                    .fixture
                    .kept
                    .lock()
                    .unwrap()
                    .account
                    .activity_context
                    .as_mut()
                    .unwrap()
                    .born += 1
            }
            5 => {
                completed.diagnostic = Some(LoginDiagnostic::from_exit_for_test(
                    &prepared.current,
                    201,
                    1,
                    None,
                ))
            }
            6 => {
                completed.diagnostic = Some(LoginDiagnostic::from_exit_for_test(
                    &prepared.current,
                    0,
                    1,
                    None,
                ))
            }
            7 => {
                completed.diagnostic = Some(LoginDiagnostic::from_exit_for_test(
                    &prepared.old,
                    199,
                    1,
                    None,
                ))
            }
            8 => completed.diagnostic = None,
            9 => completed.label = "unlisted".into(),
            _ => unreachable!(),
        }
        prepared.context = prepared
            .fixture
            .kept
            .lock()
            .unwrap()
            .account
            .activity_context
            .clone()
            .unwrap();
        let images = files(&prepared);
        let clock = Instant::now();
        let got = read(&prepared, clock, Some(vec![completed])).unwrap();
        assert_eq!(
            got.progress.as_ref(),
            Some(&prepared.retained),
            "case {case}"
        );
        assert!(!got.retry());
        assert_eq!(got.next_poll(clock), None);
        assert_retained(&prepared);
        assert_eq!(files(&prepared), images);
    }
}

#[test]
fn empty_unobserved_and_active_results_do_not_fabricate_settled_outcomes() {
    let mut prepared = prepare(0, None);
    let prior = LoginDiagnostic::from_exit_for_test(&prepared.old, 150, 3, None);
    retain_history(&mut prepared, prior.clone());
    let images = files(&prepared);
    let mut unobserved = entry(&prepared.current, false);
    unobserved.diagnostic = None;
    for entries in [Vec::new(), vec![unobserved]] {
        let clock = Instant::now();
        let got = read(&prepared, clock, Some(entries)).unwrap();
        assert_eq!(got.progress.as_ref(), Some(&prepared.retained));
        assert!(!got.retry());
        assert_eq!(got.next_poll(clock), None);
    }
    let active = read(
        &prepared,
        Instant::now(),
        Some(vec![entry(&prepared.current, true)]),
    )
    .unwrap();
    let value = active.progress.as_ref().unwrap();
    assert_eq!(value.login_diagnostics, [prior]);
    assert_eq!(primary(value), prepared.retained);
    assert_eq!(
        value.independent_work,
        [expected_work(&prepared.current, 200)]
    );
    assert_retained(&prepared);
    assert_eq!(files(&prepared), images);
}

#[test]
fn repeated_independent_diagnostics_are_one_fact_in_the_same_scope() {
    let prepared = prepare(0, None);
    let completed = entry(&prepared.current, false);
    let diagnostic = completed.diagnostic.clone().unwrap();
    let images = files(&prepared);
    let got = read(
        &prepared,
        Instant::now(),
        Some(vec![completed.clone(), completed]),
    )
    .unwrap();
    let value = got.progress.as_ref().unwrap();
    assert_eq!(
        value.login_diagnostics.as_slice(),
        std::slice::from_ref(&diagnostic)
    );
    assert_only_history_differs(value, &prepared.retained);
    assert!(!got.retry());
    assert_retained(&prepared);
    assert_eq!(files(&prepared), images);
}

#[test]
fn a_diagnostic_already_observed_for_the_same_primary_row_is_not_duplicated() {
    let mut prepared = prepare(0, None);
    let store = accounts::Store::at(&prepared.fixture.seat.state);
    store
        .transact_logins_within(&[], 200, &mut None, |_, logins| {
            logins
                .labels
                .entry(prepared.old.label.clone())
                .or_default()
                .dir = prepared.old.dir.clone();
            Ok(Vec::new())
        })
        .unwrap();
    prepared.fixture.refresh(true);
    prepared.current = prepared.fixture.row();
    prepared.context = prepared
        .fixture
        .kept
        .lock()
        .unwrap()
        .account
        .activity_context
        .clone()
        .unwrap();
    assert_eq!(prepared.current, prepared.old);
    let completed = entry(&prepared.current, false);
    let diagnostic = completed.diagnostic.clone().unwrap();
    let saved = prepared
        .fixture
        .kept
        .lock()
        .unwrap()
        .account
        .activity_context
        .take();
    let baseline = read(&prepared, Instant::now(), Some(vec![completed.clone()])).unwrap();
    prepared
        .fixture
        .kept
        .lock()
        .unwrap()
        .account
        .activity_context = saved;
    let images = files(&prepared);
    let got = read(&prepared, Instant::now(), Some(vec![completed])).unwrap();
    assert_eq!(got.progress, baseline.progress);
    assert_eq!(
        got.progress.as_ref().unwrap().login_diagnostics.as_slice(),
        std::slice::from_ref(&diagnostic)
    );
    assert!(!got.retry());
    assert_retained(&prepared);
    assert_eq!(files(&prepared), images);
}

#[test]
fn equal_diagnostics_from_different_store_namespaces_remain_separate_facts() {
    let mut prepared = prepare(0, None);
    let row = prepared.old.clone();
    let store_b = second_namespace_with_row(&mut prepared, &row);
    let images_a = files(&prepared);
    let images_b = store_images(&store_b, prepared.fixture.seat.roster.as_deref().unwrap());
    let completed = entry(&row, false);
    let diagnostic = completed.diagnostic.clone().unwrap();
    let primary_only = scoped_read(&prepared, vec![completed.clone()], Vec::new());
    let both = scoped_read(&prepared, vec![completed.clone()], vec![completed]);
    let value = both.progress.as_ref().unwrap();
    assert_eq!(value.login_diagnostics, [diagnostic.clone(), diagnostic]);
    assert_only_history_differs(value, primary_only.progress.as_ref().unwrap());
    assert!(value.independent_work.is_empty());
    assert!(!both.retry());
    assert_retained(&prepared);
    assert_eq!(files(&prepared), images_a);
    assert_eq!(
        store_images(&store_b, prepared.fixture.seat.roster.as_deref().unwrap()),
        images_b
    );
}

#[test]
fn independent_work_keeps_its_original_stamp_deduplicates_and_arms_only_observation() {
    let prepared = prepare(0, None);
    let images = files(&prepared);
    let mut active = entry(&prepared.current, true);
    active.observed_at = 177;
    let entries = vec![active.clone(), active];
    let got = scoped_read(&prepared, entries.clone(), entries);
    let value = got.progress.as_ref().unwrap();
    assert_eq!(
        value.independent_work,
        [expected_work(&prepared.current, 177)]
    );
    assert_eq!(primary(value), prepared.retained);
    assert!(got.retry());
    assert!(got.next_poll(Instant::now()).is_some());
    assert_eq!(value.next_at, prepared.retained.next_at);
    assert_retained(&prepared);
    assert_eq!(files(&prepared), images);
}

#[test]
fn identical_rows_in_other_namespaces_cannot_supply_current_work() {
    let mut prepared = prepare(0, None);
    let store_b = second_namespace(&mut prepared);
    let images_a = files(&prepared);
    let images_b = store_images(&store_b, prepared.fixture.seat.roster.as_deref().unwrap());
    let active = entry(&prepared.current, true);
    let wrong_scope = scoped_read(&prepared, vec![active.clone()], Vec::new());
    let value = wrong_scope.progress.as_ref().unwrap();
    assert!(value.independent_work.is_empty());
    assert_eq!(primary(value), prepared.retained);
    let current = scoped_read(&prepared, Vec::new(), vec![active]);
    let value = current.progress.as_ref().unwrap();
    assert_eq!(
        value.independent_work,
        [expected_work(&prepared.current, 200)]
    );
    assert_eq!(primary(value), prepared.retained);
    assert!(current.retry());
    assert_retained(&prepared);
    assert_eq!(files(&prepared), images_a);
    assert_eq!(
        store_images(&store_b, prepared.fixture.seat.roster.as_deref().unwrap()),
        images_b
    );
}

#[test]
fn a_primary_signer_cannot_deduplicate_equal_work_from_a_different_namespace() {
    let mut prepared = prepare(0, None);
    let row = prepared.old.clone();
    let store_b = second_namespace_with_row(&mut prepared, &row);
    let images_a = files(&prepared);
    let images_b = store_images(&store_b, prepared.fixture.seat.roster.as_deref().unwrap());
    let active = entry(&row, true);
    let primary_only = scoped_read(&prepared, vec![active.clone()], Vec::new());
    let both = scoped_read(&prepared, vec![active.clone()], vec![active]);
    let value = both.progress.as_ref().unwrap();
    assert!(
        matches!(&value.phase, ProgressPhase::SigningIn { label, route }
        if label == &row.label && route.as_deref() == Some("mailbox"))
    );
    assert_eq!(
        primary(value),
        primary(primary_only.progress.as_ref().unwrap())
    );
    assert_eq!(value.independent_work, [expected_work(&row, 200)]);
    assert!(
        primary_only
            .progress
            .as_ref()
            .unwrap()
            .independent_work
            .is_empty()
    );
    assert_retained(&prepared);
    assert_eq!(files(&prepared), images_a);
    assert_eq!(
        store_images(&store_b, prepared.fixture.seat.roster.as_deref().unwrap()),
        images_b
    );
}

#[test]
fn missing_scope_and_busy_observations_withdraw_work_until_the_same_scope_recovers() {
    let mut prepared = prepare(0, None);
    second_namespace(&mut prepared);
    let images = files(&prepared);
    let active = entry(&prepared.current, true);
    assert_eq!(
        scoped_read(&prepared, Vec::new(), vec![active.clone()])
            .progress
            .unwrap()
            .independent_work
            .len(),
        1
    );
    let completed = entry(&prepared.current, false);
    let diagnostic = completed.diagnostic.clone().unwrap();
    for (entries, history) in [
        (Vec::new(), Vec::new()),
        (vec![completed], vec![diagnostic]),
    ] {
        let idle = scoped_read(&prepared, Vec::new(), entries);
        assert!(idle.progress.as_ref().unwrap().independent_work.is_empty());
        let mut expected = prepared.retained.clone();
        expected.login_diagnostics = history;
        assert_eq!(idle.progress.as_ref(), Some(&expected));
        assert!(!idle.retry());
    }
    let primary_store = accounts::Store::at(&prepared.fixture.seat.state);
    let missing = batch_read(&prepared, |_, cli, _| {
        Some(LoginActivityBatch::for_test(
            [Some((&primary_store, Vec::new())), None],
            cli,
        ))
    })
    .unwrap();
    assert!(
        missing
            .progress
            .as_ref()
            .unwrap()
            .independent_work
            .is_empty()
    );
    assert!(missing.retry());
    assert!(batch_read(&prepared, |_, _, _| None).is_err());
    let recovered = scoped_read(&prepared, Vec::new(), vec![active]);
    assert_eq!(
        recovered.progress.as_ref().unwrap().independent_work,
        [expected_work(&prepared.current, 200)]
    );
    assert_eq!(
        primary(recovered.progress.as_ref().unwrap()),
        prepared.retained
    );
    assert_retained(&prepared);
    assert_eq!(files(&prepared), images);
}

#[test]
fn independent_work_requires_current_exact_unambiguous_active_identity() {
    for case in 0..11 {
        let mut prepared = prepare(0, None);
        let mut active = entry(&prepared.current, true);
        {
            let mut kept = prepared.fixture.kept.lock().unwrap();
            match case {
                0 => active.active = false,
                1 => active.identity = None,
                2 => active = entry(&prepared.old, true),
                3 => active.label = "unlisted".into(),
                4 => {
                    let context = kept.account.activity_context.as_mut().unwrap();
                    context.rows = vec![prepared.current.clone(), prepared.current.clone()].into();
                }
                5 => kept.account.activity_context.as_mut().unwrap().policy = Arc::new(()),
                6 => kept.account.activity_context.as_mut().unwrap().generation = Arc::default(),
                7 => kept.account.activity_context.as_mut().unwrap().born += 1,
                8 => {
                    kept.account
                        .activity_context
                        .as_mut()
                        .unwrap()
                        .snapshot
                        .session = Some("other-conversation".into())
                }
                9 => kept
                    .account
                    .activity_context
                    .as_mut()
                    .unwrap()
                    .snapshot
                    .argv
                    .push("--other-account".into()),
                10 => kept.account.activity_context = None,
                _ => unreachable!(),
            }
            if let Some(context) = &kept.account.activity_context {
                prepared.context = context.clone();
            }
        }
        let images = files(&prepared);
        let saved_context = prepared
            .fixture
            .kept
            .lock()
            .unwrap()
            .account
            .activity_context
            .take();
        let baseline = scoped_read(&prepared, vec![active.clone()], vec![active.clone()]);
        prepared
            .fixture
            .kept
            .lock()
            .unwrap()
            .account
            .activity_context = saved_context;
        let got = scoped_read(&prepared, vec![active.clone()], vec![active]);
        assert!(
            got.progress.as_ref().unwrap().independent_work.is_empty(),
            "case {case}"
        );
        let mut expected = primary(baseline.progress.as_ref().unwrap());
        if case == 0 {
            expected.login_diagnostics = vec![entry(&prepared.current, false).diagnostic.unwrap()];
        }
        assert_eq!(
            primary(got.progress.as_ref().unwrap()),
            expected,
            "case {case}"
        );
        assert_eq!(files(&prepared), images);
        assert_eq!(
            prepared
                .fixture
                .kept
                .lock()
                .unwrap()
                .account
                .progress
                .as_ref()
                .unwrap()
                .value,
            prepared.retained
        );
    }
}

#[test]
fn independent_work_is_visible_beside_a_real_live_gate_without_changing_its_lease() {
    let prepared = expired_activity_tests::prepare(true, true);
    let before = expired_activity_tests::image(&prepared);
    let mono = prepared.until - Duration::from_nanos(1);
    let baseline = expired_activity_tests::read(&prepared, mono, Some(Vec::new()));
    let got = expired_activity_tests::read(
        &prepared,
        mono,
        Some(vec![expired_activity_tests::entry(
            &prepared,
            &prepared.current,
            true,
        )]),
    );
    let value = got.progress.as_ref().unwrap();
    assert_eq!(primary(value), primary(baseline.progress.as_ref().unwrap()));
    assert_eq!(value.independent_work.len(), 1);
    assert_eq!(value.independent_work[0].label, prepared.current.label);
    assert_eq!(value.independent_work[0].route.as_deref(), Some("mailbox"));
    assert_eq!(
        value.independent_work[0].observed_at,
        expired_activity_tests::entry(&prepared, &prepared.current, true).observed_at
    );
    assert_eq!(got.execution_until, Some(prepared.until));
    assert_eq!(got.next_poll(mono), Some(prepared.until));
    assert!(got.retry());
    assert!(expired_activity_tests::unchanged(&prepared, &before));
}
