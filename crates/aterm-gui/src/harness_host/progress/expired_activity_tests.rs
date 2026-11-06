// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness_host::tests::{ProgressExecutionFixture, progress_execution_fixture};
use aterm_agent::harness::account_progress::{
    EXECUTION_HISTORY_PREFIX, EXECUTION_PREPARED_FINDING, EXECUTION_REFRESH_FINDING,
};
use aterm_agent::harness::accounts_sync::{LoginActivity, LoginDiagnostic, LoginIdentity};

static FIXTURES: AtomicU64 = AtomicU64::new(0);

pub(super) struct Prepared {
    pub(super) fixture: ProgressExecutionFixture,
    pub(super) sid: String,
    pub(super) conversation: String,
    pub(super) old: accounts::Row,
    pub(super) current: accounts::Row,
    pub(super) until: Instant,
}

pub(super) struct Image {
    execution: BoundProgress,
    normal: Option<BoundProgress>,
    context: Option<AccountActivityContext>,
    files: Vec<(PathBuf, Option<Vec<u8>>)>,
    account_calls: [usize; 3],
}

pub(super) fn prepare(changed: bool, off: bool) -> Prepared {
    let sid = format!(
        "s-expired-activity-{}",
        FIXTURES.fetch_add(1, Ordering::SeqCst)
    );
    let fixture = progress_execution_fixture(&sid);
    let store = accounts::Store::at(&fixture.seat.state);
    store
        .transact(fixture.seat.now, &mut None, |state| {
            state.prepared_accounts.insert("main".into());
            Ok(Vec::new())
        })
        .unwrap();
    let until = fixture.idle.account_execution_hold().unwrap();
    let (old, conversation) = {
        let kept = fixture.idle.kept.lock().unwrap();
        let execution = kept.account.execution_progress.as_ref().unwrap();
        assert_eq!(execution.execution_until, Some(until));
        assert_eq!(execution.value.phase, ProgressPhase::Waiting);
        assert!(
            execution
                .value
                .details
                .iter()
                .any(|detail| detail == EXECUTION_PREPARED_FINDING)
        );
        (
            execution
                .rows
                .iter()
                .find(|row| row.label == "work-2")
                .unwrap()
                .clone(),
            execution.snapshot.session.clone().unwrap(),
        )
    };
    if off {
        publish_off(
            &fixture.host.shared,
            &Workers::default(),
            &HashMap::new(),
            &OffSessions {
                words: HashMap::from([(sid.clone(), Autopilot::Off)]),
                default: Autopilot::Cruise,
            },
            &fixture.host.hooks,
        );
    }
    // Repair publishes store news but does not run the real execution gate again.
    store
        .transact(fixture.seat.now, &mut None, |state| {
            state.prepared_accounts.remove("main");
            Ok(Vec::new())
        })
        .unwrap();
    if changed {
        let next_dir = fixture.seat.state.parent().unwrap().join("login-b");
        store
            .transact_logins_within(&[], fixture.seat.now, &mut None, |_, logins| {
                logins.labels.entry(old.label.clone()).or_default().dir =
                    Some(next_dir.to_str().unwrap().into());
                Ok(Vec::new())
            })
            .unwrap();
    }
    let records = store
        .read_account_records(fixture.seat.roster.as_deref(), fixture.seat.now)
        .unwrap();
    assert!(records.notes.is_empty());
    let current = records.roster.row(&old.label).unwrap().clone();
    assert_eq!(current != old, changed);
    let capture = activity::ActivityCapture::capture(&fixture.idle.kept.lock().unwrap()).unwrap();
    let observed = capture.read_for_test(&fixture.seat, fixture.born);
    assert!(capture.publish_for_test(
        &fixture.idle.kept,
        &fixture.host.hooks,
        &fixture.seat,
        observed,
        Some(fixture.born),
    ));
    let kept = fixture.idle.kept.lock().unwrap();
    let context = kept.account.activity_context.as_ref().unwrap();
    assert!(Arc::ptr_eq(&context.policy, &kept.account.progress_policy));
    assert_eq!(context.rows.as_ref(), records.roster.rows.as_slice());
    drop(kept);
    Prepared {
        fixture,
        sid,
        conversation,
        old,
        current,
        until,
    }
}

fn files(prepared: &Prepared) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    let store = accounts::Store::at(&prepared.fixture.seat.state);
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
        prepared.fixture.seat.roster.clone().unwrap(),
    ]
    .into_iter()
    .map(|path| {
        let contents = std::fs::read(&path).ok();
        (path, contents)
    })
    .collect()
}

pub(super) fn image(prepared: &Prepared) -> Image {
    let kept = prepared.fixture.idle.kept.lock().unwrap();
    Image {
        execution: kept.account.execution_progress.clone().unwrap(),
        normal: kept.account.progress.clone(),
        context: kept.account.activity_context.clone(),
        files: files(prepared),
        account_calls: prepared.fixture.account_calls(),
    }
}

fn same_decision(a: &BoundProgress, b: &BoundProgress) -> bool {
    a.snapshot == b.snapshot
        && Arc::ptr_eq(&a.generation, &b.generation)
        && Arc::ptr_eq(&a.policy, &b.policy)
        && a.born == b.born
        && a.state == b.state
        && a.labels == b.labels
        && a.rows == b.rows
        && a.execution_until == b.execution_until
        && a.value == b.value
}

fn same_context(a: &AccountActivityContext, b: &AccountActivityContext) -> bool {
    a.snapshot == b.snapshot
        && Arc::ptr_eq(&a.generation, &b.generation)
        && Arc::ptr_eq(&a.policy, &b.policy)
        && a.born == b.born
        && a.state == b.state
        && a.labels == b.labels
        && a.rows == b.rows
}

pub(super) fn unchanged(prepared: &Prepared, before: &Image) -> bool {
    let after = image(prepared);
    same_decision(&after.execution, &before.execution)
        && match (&after.normal, &before.normal) {
            (Some(a), Some(b)) => same_decision(a, b),
            (None, None) => true,
            _ => false,
        }
        && match (&after.context, &before.context) {
            (Some(a), Some(b)) => same_context(a, b),
            (None, None) => true,
            _ => false,
        }
        && after.files == before.files
        && after.account_calls == before.account_calls
}

pub(super) fn entry(prepared: &Prepared, row: &accounts::Row, active: bool) -> LoginActivity {
    let value = prepared
        .fixture
        .idle
        .kept
        .lock()
        .unwrap()
        .account
        .execution_progress
        .as_ref()
        .unwrap()
        .value
        .clone();
    LoginActivity {
        queued: None,
        label: row.label.clone(),
        route: Some("mailbox".into()),
        active,
        observed_at: value.observed_at,
        next_at: Some(value.next_at.unwrap().saturating_add(60)),
        diagnostic: Some(LoginDiagnostic::from_exit_for_test(
            row,
            value.observed_at,
            1,
            None,
        )),
        identity: Some(LoginIdentity::for_test(row)),
    }
}

pub(super) fn read(
    prepared: &Prepared,
    mono: Instant,
    entries: Option<Vec<LoginActivity>>,
) -> ProgressRead {
    let wall = prepared
        .fixture
        .idle
        .kept
        .lock()
        .unwrap()
        .account
        .execution_progress
        .as_ref()
        .unwrap()
        .value
        .observed_at;
    prepared
        .fixture
        .host
        .account_progress_read_with_activity(
            &prepared.sid,
            std::process::id().try_into().unwrap(),
            Some(prepared.fixture.born),
            Some(&prepared.conversation),
            || (mono, Some(wall)),
            |store, cli, now| {
                assert_eq!(
                    store.dir,
                    accounts::Store::at(&prepared.fixture.seat.state).dir
                );
                assert_eq!(cli, accounts::Cli::Claude);
                assert_eq!(now, wall);
                entries
            },
        )
        .unwrap()
}

fn historical(value: &AccountProgress) -> bool {
    value
        .details
        .iter()
        .any(|detail| detail == &format!("{EXECUTION_HISTORY_PREFIX}{EXECUTION_PREPARED_FINDING}"))
        && !value
            .details
            .iter()
            .any(|detail| detail == EXECUTION_PREPARED_FINDING)
        && value
            .details
            .iter()
            .any(|detail| detail == EXECUTION_REFRESH_FINDING)
}

/// Real-gate observation shared with the execution-visibility conformance adapter.
pub(in crate::harness_host) fn expired_activity_observation(
    expired: bool,
    changed: bool,
    context_current: bool,
    activity_kind: i64,
    off: bool,
    diagnostic_variant: u8,
) -> [i64; 11] {
    let prepared = prepare(changed, off);
    if !context_current {
        prepared
            .fixture
            .idle
            .kept
            .lock()
            .unwrap()
            .account
            .activity_context = None;
    }
    let before = image(&prepared);
    let mono = if expired {
        prepared.until
    } else {
        prepared.until - Duration::from_nanos(1)
    };
    let mut entries = match activity_kind {
        0 => Some(Vec::new()),
        1 => Some(vec![entry(&prepared, &prepared.current, true)]),
        2 => None,
        3 => Some(vec![entry(&prepared, &prepared.current, false)]),
        _ => unreachable!(),
    };
    if let Some(entries) = &mut entries {
        for entry in entries {
            match diagnostic_variant {
                0 => {}
                1 => entry.diagnostic = None,
                2 => entry.diagnostic.as_mut().unwrap().completed_at = 0,
                3 => {
                    entry.diagnostic.as_mut().unwrap().completed_at =
                        entry.observed_at.saturating_add(1)
                }
                4 => {
                    entry.diagnostic = Some(LoginDiagnostic::from_exit_for_test(
                        &prepared.old,
                        entry.observed_at,
                        1,
                        None,
                    ))
                }
                _ => unreachable!(),
            }
        }
    }
    let read = read(&prepared, mono, entries);
    let value = read.progress.as_ref().unwrap();
    let phase = match value.phase {
        ProgressPhase::Waiting => 1,
        ProgressPhase::Disabled => 2,
        ProgressPhase::Watching => 3,
        ProgressPhase::Probing => 4,
        ProgressPhase::Switching { .. } => 5,
        ProgressPhase::Verifying { .. } => 6,
        ProgressPhase::SigningIn { .. } => 7,
        ProgressPhase::Unknown => 8,
        ProgressPhase::Paused => 9,
        ProgressPhase::Limited | ProgressPhase::Ready => unreachable!(),
    };
    [
        phase,
        i64::from(value.next_at.is_some()),
        i64::from(read.execution_until.is_some()),
        i64::from(historical(value)),
        i64::from(value.known.is_none() && value.eligible.is_none()),
        i64::from(value.forecasts_paused),
        i64::from(unchanged(&prepared, &before)),
        i64::try_from(value.login_diagnostics.len()).unwrap(),
        i64::from(read.retry()),
        i64::from(read.next_poll(mono).is_some()),
        i64::from(!value.independent_work.is_empty()),
    ]
}

#[test]
fn an_expired_execution_record_does_not_hide_current_work_after_its_binding_changes() {
    let prepared = prepare(true, true);
    let before = image(&prepared);
    for (mono, expired) in [
        (prepared.until - Duration::from_nanos(1), false),
        (prepared.until, true),
        (prepared.until + Duration::from_secs(1), true),
    ] {
        let read = read(
            &prepared,
            mono,
            Some(vec![entry(&prepared, &prepared.current, true)]),
        );
        let value = read.progress.as_ref().unwrap();
        if expired {
            assert_eq!(
                value.phase,
                ProgressPhase::SigningIn {
                    label: prepared.current.label.clone(),
                    route: Some("mailbox".into()),
                }
            );
            assert!(historical(value));
            assert_eq!(value.next_at, None);
            assert!(value.login_diagnostics.is_empty());
            assert_eq!(read.execution_until, None);
            assert_eq!(read.next_poll(mono), Some(mono + Duration::from_secs(1)));
            assert!(
                value.independent_work.is_empty(),
                "the primary phase already represents the same-scope worker"
            );
        } else {
            assert_eq!(value.phase, ProgressPhase::Waiting);
            assert_eq!(value.next_at, before.execution.value.next_at);
            assert!(!historical(value));
            assert_eq!(read.execution_until, Some(prepared.until));
            assert_eq!(read.next_poll(mono), Some(prepared.until));
            assert_eq!(
                value.independent_work,
                [independent_work_tests::expected_work(
                    &prepared.current,
                    entry(&prepared, &prepared.current, true).observed_at
                )]
            );
        }
        assert!(value.forecasts_paused);
        assert_eq!(value.known, None);
        assert_eq!(value.eligible, None);
        assert_eq!(value.account, before.execution.value.account);
        assert_eq!(value.observed_at, before.execution.value.observed_at);
        assert!(unchanged(&prepared, &before));
    }
}

#[test]
fn expired_execution_contexts_reject_unconfirmed_or_ambiguous_current_work() {
    for case in 0..7 {
        let prepared = prepare(true, true);
        let mut active = entry(&prepared, &prepared.current, true);
        if case == 1 {
            active = entry(&prepared, &prepared.old, true);
        }
        {
            let mut kept = prepared.fixture.idle.kept.lock().unwrap();
            match case {
                0 => active.identity = None,
                1 => {}
                2 => active.label = "unrelated".into(),
                3 => {
                    let context = kept.account.activity_context.as_mut().unwrap();
                    let mut rows = context.rows.to_vec();
                    rows.push(prepared.current.clone());
                    context.rows = rows.into();
                }
                4 => kept.account.activity_context.as_mut().unwrap().policy = Arc::new(()),
                5 => kept.account.activity_context = None,
                6 => kept.account.activity_context.as_mut().unwrap().born += 1,
                _ => unreachable!(),
            }
        }
        let before = image(&prepared);
        let baseline = read(&prepared, prepared.until, Some(Vec::new()));
        let got = read(&prepared, prepared.until, Some(vec![active]));
        assert_eq!(got.progress, baseline.progress, "case {case}");
        assert_eq!(got.progress.as_ref().unwrap().phase, ProgressPhase::Unknown);
        assert!(historical(got.progress.as_ref().unwrap()));
        assert_eq!(got.next_poll(prepared.until), None);
        assert!(unchanged(&prepared, &before));
    }
}

#[test]
fn inactive_new_context_work_cannot_renew_an_expired_execution_forecast() {
    let prepared = prepare(true, true);
    let before = image(&prepared);
    let baseline = read(&prepared, prepared.until, Some(Vec::new()));
    let completed = entry(&prepared, &prepared.current, false);
    let diagnostic = completed.diagnostic.clone().unwrap();
    let inactive = read(&prepared, prepared.until, Some(vec![completed]));
    let mut expected = baseline.progress.unwrap();
    expected.login_diagnostics.push(diagnostic.clone());
    assert_eq!(inactive.progress, Some(expected));
    let value = inactive.progress.as_ref().unwrap();
    assert_eq!(value.phase, ProgressPhase::Unknown);
    assert!(historical(value));
    assert_eq!(value.next_at, None);
    assert_eq!(
        value.login_diagnostics.as_slice(),
        std::slice::from_ref(&diagnostic)
    );
    assert_eq!(inactive.next_poll(prepared.until), None);
    assert!(unchanged(&prepared, &before));
}

#[test]
fn settled_current_history_keeps_a_real_gate_lease_and_its_expired_projection() {
    let prepared = prepare(true, true);
    let before = image(&prepared);
    for mono in [prepared.until - Duration::from_nanos(1), prepared.until] {
        let baseline = read(&prepared, mono, Some(Vec::new()));
        let completed = entry(&prepared, &prepared.current, false);
        let diagnostic = completed.diagnostic.clone().unwrap();
        let got = read(&prepared, mono, Some(vec![completed]));
        let mut expected = baseline.progress.clone().unwrap();
        expected.login_diagnostics.push(diagnostic);
        assert_eq!(got.progress, Some(expected));
        assert_eq!(got.execution_until, baseline.execution_until);
        assert_eq!(got.next_poll(mono), baseline.next_poll(mono));
        assert!(!got.retry());
        assert!(unchanged(&prepared, &before));
    }
}

#[test]
fn expired_new_context_activity_contention_retries_observation_without_a_gate_call() {
    let prepared = prepare(true, true);
    let before = image(&prepared);
    let busy = read(&prepared, prepared.until, None);
    assert!(busy.activity_contended);
    assert_eq!(
        busy.progress.as_ref().unwrap().phase,
        ProgressPhase::Unknown
    );
    assert_eq!(busy.execution_until, None);
    assert_eq!(
        busy.next_poll(prepared.until),
        Some(prepared.until + Duration::from_secs(1))
    );
    let recovered = read(
        &prepared,
        prepared.until,
        Some(vec![entry(&prepared, &prepared.current, true)]),
    );
    assert!(!recovered.activity_contended);
    assert!(matches!(
        recovered.progress.as_ref().unwrap().phase,
        ProgressPhase::SigningIn { .. }
    ));
    assert_eq!(recovered.progress.as_ref().unwrap().next_at, None);
    assert!(unchanged(&prepared, &before));
}

#[test]
fn the_same_roster_keeps_its_existing_expired_activity_deadline_even_while_off() {
    for off in [false, true] {
        let prepared = prepare(false, off);
        let before = image(&prepared);
        let active = entry(&prepared, &prepared.current, true);
        let next = active.next_at;
        let read = read(&prepared, prepared.until, Some(vec![active]));
        let value = read.progress.as_ref().unwrap();
        assert!(matches!(value.phase, ProgressPhase::SigningIn { .. }));
        assert!(historical(value));
        assert_eq!(value.next_at, next);
        assert!(value.independent_work.is_empty());
        assert_eq!(read.execution_until, None);
        assert_eq!(
            read.next_poll(prepared.until),
            Some(prepared.until + Duration::from_secs(1))
        );
        assert!(unchanged(&prepared, &before));
    }
}
