// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use activity::tests::Fixture;
use aterm_agent::harness::accounts_sync::{LoginActivity, LoginDiagnostic, LoginIdentity};

const SID: &str = "s-progress";
const CONVERSATION: &str = "conversation-a";
const BORN: u64 = 100;
const NOW: i64 = 200;

pub(super) struct Prepared {
    pub(super) fixture: Fixture,
    pub(super) old: accounts::Row,
    pub(super) current: accounts::Row,
    pub(super) retained: AccountProgress,
    pub(super) context: AccountActivityContext,
}

pub(super) fn set_off(fixture: &Fixture, off: bool) {
    publish_off(
        &fixture.host.shared,
        &Workers::default(),
        &HashMap::new(),
        &OffSessions {
            words: HashMap::from([(
                SID.into(),
                if off {
                    Autopilot::Off
                } else {
                    Autopilot::Cruise
                },
            )]),
            default: Autopilot::Cruise,
        },
        &fixture.host.hooks,
    );
}

// 0 keeps the normal policy; 1 is Off; 2 resumed without another account judgment.
pub(super) fn prepare(retirement: u8, execution_until: Option<Instant>) -> Prepared {
    let fixture = Fixture::new();
    fixture.refresh(false);
    let old = fixture.row();
    let retained = {
        let mut kept = fixture.kept.lock().unwrap();
        let mut value = AccountProgress::unknown(NOW - 20);
        value.account = Some(old.label.clone());
        value.known = execution_until.is_none().then_some(1);
        value.eligible = execution_until.is_none().then_some(1);
        value.phase = ProgressPhase::Waiting;
        value.wall = Some("weekly".into());
        value.details.push("captured account finding".into());
        value.next_at = Some(NOW + 500);
        let bound = BoundProgress {
            snapshot: kept.snapshot.clone().unwrap(),
            generation: Arc::clone(&kept.account.landing),
            born: Some(BORN),
            state: Some(fixture.seat.state.clone()),
            labels: BTreeSet::from([old.label.clone()]),
            rows: vec![old.clone()].into(),
            policy: Arc::clone(&kept.account.progress_policy),
            execution_until,
            value: value.clone(),
        };
        if execution_until.is_some() {
            kept.account.execution_progress = Some(bound);
        } else {
            kept.account.progress = Some(bound);
        }
        value
    };
    if retirement != 0 {
        set_off(&fixture, true);
        if retirement == 2 {
            set_off(&fixture, false);
        }
    }
    let next_dir = fixture.seat.state.parent().unwrap().join("login-b");
    let store = accounts::Store::at(&fixture.seat.state);
    store
        .transact_logins_within(&[], NOW, &mut None, |_, logins| {
            logins.labels.entry(old.label.clone()).or_default().dir =
                Some(next_dir.to_str().unwrap().into());
            Ok(Vec::new())
        })
        .unwrap();
    let records = store
        .read_account_records(fixture.seat.roster.as_deref(), NOW)
        .unwrap();
    assert!(records.notes.is_empty());
    let current = records.roster.row(&old.label).unwrap().clone();
    assert_ne!(
        old.dir, current.dir,
        "the admitted binding changed the captured row"
    );
    fixture.refresh(true);
    assert_eq!(fixture.row(), current);
    let context = fixture
        .kept
        .lock()
        .unwrap()
        .account
        .activity_context
        .clone()
        .unwrap();
    {
        let kept = fixture.kept.lock().unwrap();
        assert!(Arc::ptr_eq(&context.policy, &kept.account.progress_policy));
        let decision = kept
            .account
            .execution_progress
            .as_ref()
            .or(kept.account.progress.as_ref())
            .unwrap();
        assert_eq!(
            decision.policy_is(&kept.account.progress_policy),
            retirement == 0
        );
        assert_eq!(decision.rows.as_ref(), std::slice::from_ref(&old));
    }
    Prepared {
        fixture,
        old,
        current,
        retained,
        context,
    }
}

pub(super) fn entry(row: &accounts::Row, active: bool) -> LoginActivity {
    LoginActivity {
        queued: None,
        label: row.label.clone(),
        route: Some("mailbox".into()),
        active,
        observed_at: NOW,
        next_at: Some(NOW + 60),
        diagnostic: Some(LoginDiagnostic::from_exit_for_test(row, NOW - 1, 1, None)),
        identity: Some(LoginIdentity::for_test(row)),
    }
}

pub(super) fn read(
    prepared: &Prepared,
    clock: Instant,
    entries: Option<Vec<LoginActivity>>,
) -> Result<ProgressRead, ()> {
    prepared.fixture.host.account_progress_read_with_activity(
        SID,
        4242,
        Some(BORN),
        Some(CONVERSATION),
        || (clock, Some(NOW)),
        |store, cli, now| {
            assert_eq!(
                store.dir,
                accounts::Store::at(&prepared.fixture.seat.state).dir
            );
            assert_eq!(cli, accounts::Cli::Claude);
            assert_eq!(now, NOW);
            entries
        },
    )
}

pub(super) fn assert_retained(prepared: &Prepared) {
    let kept = prepared.fixture.kept.lock().unwrap();
    let decision = kept
        .account
        .execution_progress
        .as_ref()
        .or(kept.account.progress.as_ref())
        .unwrap();
    assert_eq!(decision.value, prepared.retained);
    assert_eq!(decision.rows.as_ref(), std::slice::from_ref(&prepared.old));
    let context = kept.account.activity_context.as_ref().unwrap();
    assert_eq!(context.snapshot, prepared.context.snapshot);
    assert!(Arc::ptr_eq(
        &context.generation,
        &prepared.context.generation
    ));
    assert!(Arc::ptr_eq(&context.policy, &prepared.context.policy));
    assert_eq!(context.born, prepared.context.born);
    assert_eq!(context.state, prepared.context.state);
    assert_eq!(context.labels, prepared.context.labels);
    assert_eq!(context.rows, prepared.context.rows);
}

pub(super) fn files(prepared: &Prepared) -> Vec<(PathBuf, Option<Vec<u8>>)> {
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
        let image = std::fs::read(&path).ok();
        (path, image)
    })
    .collect()
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

/// Native current/retired normal decisions for the execution visibility adapter.
pub(in crate::harness_host) fn retired_activity_observation(
    normal: i64,
    context_present: bool,
    activity_kind: i64,
    resumed: bool,
    diagnostic_variant: u8,
) -> [i64; 9] {
    assert!(matches!(normal, 1 | 2));
    let retirement = if normal == 1 {
        0
    } else if resumed {
        2
    } else {
        1
    };
    let prepared = prepare(retirement, None);
    if !context_present {
        prepared
            .fixture
            .kept
            .lock()
            .unwrap()
            .account
            .activity_context = None;
    }
    let (before_decision, before_context) = {
        let kept = prepared.fixture.kept.lock().unwrap();
        (
            kept.account.progress.clone().unwrap(),
            kept.account.activity_context.clone(),
        )
    };
    let images = files(&prepared);
    let mut entries = match activity_kind {
        0 => Some(Vec::new()),
        1 => Some(vec![entry(&prepared.current, true)]),
        2 => None,
        3 => Some(vec![entry(&prepared.current, false)]),
        _ => unreachable!(),
    };
    if let Some(entries) = &mut entries {
        for entry in entries {
            match diagnostic_variant {
                0 => {}
                1 => entry.diagnostic = None,
                2 => entry.diagnostic.as_mut().unwrap().completed_at = 0,
                3 => entry.diagnostic.as_mut().unwrap().completed_at = NOW + 1,
                4 => {
                    entry.diagnostic = Some(LoginDiagnostic::from_exit_for_test(
                        &prepared.old,
                        NOW - 1,
                        1,
                        None,
                    ))
                }
                _ => unreachable!(),
            }
        }
    }
    let result = read(&prepared, Instant::now(), entries);
    let unavailable = result.is_err();
    let value = result.ok().and_then(|read| read.progress);
    let unchanged = {
        let kept = prepared.fixture.kept.lock().unwrap();
        same_decision(kept.account.progress.as_ref().unwrap(), &before_decision)
            && kept.account.execution_progress.is_none()
            && match (&kept.account.activity_context, &before_context) {
                (Some(after), Some(before)) => same_context(after, before),
                (None, None) => true,
                _ => false,
            }
    } && files(&prepared) == images;
    let phase = value.as_ref().map_or(0, |value| match value.phase {
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
    });
    [
        phase,
        i64::from(value.as_ref().is_some_and(|value| value.eligible.is_some())),
        i64::from(value.as_ref().is_some_and(|value| value.next_at.is_some())),
        i64::from(value.as_ref().is_some_and(|value| value.forecasts_paused)),
        i64::from(unchanged),
        i64::try_from(
            value
                .as_ref()
                .map_or(0, |value| value.login_diagnostics.len()),
        )
        .unwrap(),
        i64::from(unavailable),
        i64::from(value.as_ref().is_none_or(|value| {
            value.account == prepared.retained.account && value.known == prepared.retained.known
        })),
        i64::from(
            value
                .as_ref()
                .is_some_and(|value| !value.independent_work.is_empty()),
        ),
    ]
}

#[test]
fn a_retired_normal_decision_does_not_hide_its_current_contexts_actual_sign_in() {
    for retirement in [1, 2] {
        let prepared = prepare(retirement, None);
        let images = files(&prepared);
        let clock = Instant::now();
        let read = read(&prepared, clock, Some(vec![entry(&prepared.current, true)])).unwrap();
        let value = read.progress.unwrap();
        assert_eq!(
            value.phase,
            ProgressPhase::SigningIn {
                label: prepared.current.label.clone(),
                route: Some("mailbox".into()),
            }
        );
        assert!(value.forecasts_paused);
        assert_eq!(value.eligible, None);
        assert_eq!(value.next_at, None);
        assert!(value.login_diagnostics.is_empty());
        assert_eq!(value.observed_at, prepared.retained.observed_at);
        assert_eq!(value.account, prepared.retained.account);
        assert_eq!(value.known, prepared.retained.known);
        assert_eq!(value.wall, prepared.retained.wall);
        assert!(
            value.independent_work.is_empty(),
            "the primary phase already represents this same-scope worker"
        );
        assert_eq!(read.execution_until, None);
        assert_retained(&prepared);
        assert_eq!(files(&prepared), images);
    }
}

#[test]
fn a_policy_current_normal_decision_keeps_its_captured_account_priority() {
    let prepared = prepare(0, None);
    let clock = Instant::now();
    let current = read(&prepared, clock, Some(vec![entry(&prepared.current, true)])).unwrap();
    let value = current.progress.as_ref().unwrap();
    assert_eq!(independent_work_tests::primary(value), prepared.retained);
    assert_eq!(
        value.independent_work,
        [independent_work_tests::expected_work(
            &prepared.current,
            NOW
        )]
    );
    assert!(current.retry());
    let old = read(&prepared, clock, Some(vec![entry(&prepared.old, true)]))
        .unwrap()
        .progress
        .unwrap();
    assert!(matches!(old.phase, ProgressPhase::SigningIn { .. }));
    assert!(!old.forecasts_paused);
    assert_eq!(old.eligible, prepared.retained.eligible);
    assert_eq!(old.next_at, Some(NOW + 60));
    assert!(old.independent_work.is_empty());
    assert_retained(&prepared);
}

#[test]
fn retired_normal_activity_still_requires_a_current_exact_unambiguous_identity() {
    for case in 0..5 {
        let mut prepared = prepare(1, None);
        let mut active = entry(&prepared.current, true);
        match case {
            0 => active.identity = None,
            1 => active = entry(&prepared.old, true),
            2 => active.label = "unrelated".into(),
            3 => {
                let mut kept = prepared.fixture.kept.lock().unwrap();
                let context = kept.account.activity_context.as_mut().unwrap();
                context.rows = vec![prepared.current.clone(), prepared.current.clone()].into();
                prepared.context = context.clone();
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
                    .policy = Arc::new(());
                prepared.context = prepared
                    .fixture
                    .kept
                    .lock()
                    .unwrap()
                    .account
                    .activity_context
                    .clone()
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let clock = Instant::now();
        let baseline = read(&prepared, clock, Some(Vec::new()))
            .unwrap()
            .progress
            .unwrap();
        assert_eq!(baseline.phase, ProgressPhase::Paused);
        let value = read(&prepared, clock, Some(vec![active]))
            .unwrap()
            .progress
            .unwrap();
        assert_eq!(
            value, baseline,
            "case {case} attached unconfirmed current work"
        );
        assert_retained(&prepared);
    }
}

#[test]
fn settled_current_context_adds_only_its_diagnostic_to_paused_decision_history() {
    for retirement in [1, 2] {
        let prepared = prepare(retirement, None);
        let clock = Instant::now();
        let baseline = read(&prepared, clock, Some(Vec::new()))
            .unwrap()
            .progress
            .unwrap();
        assert_eq!(baseline.phase, ProgressPhase::Paused);
        assert!(baseline.forecasts_paused);
        assert_eq!(baseline.eligible, None);
        assert_eq!(baseline.next_at, None);
        let completed = entry(&prepared.current, false);
        let diagnostic = completed.diagnostic.clone().unwrap();
        let inactive = read(&prepared, clock, Some(vec![completed])).unwrap();
        let mut expected = baseline;
        expected.login_diagnostics.push(diagnostic);
        assert_eq!(inactive.progress, Some(expected));
        assert!(!inactive.retry());
        assert_eq!(inactive.next_poll(clock), None);
        assert_retained(&prepared);
    }
}

#[test]
fn a_retired_normal_context_reports_contention_for_the_existing_retry_path() {
    let prepared = prepare(1, None);
    let clock = Instant::now();
    assert!(read(&prepared, clock, None).is_err());
    let recovered = read(&prepared, clock, Some(vec![entry(&prepared.current, true)])).unwrap();
    assert!(matches!(
        recovered.progress.as_ref().unwrap().phase,
        ProgressPhase::SigningIn { .. }
    ));
    assert_eq!(
        recovered.next_poll(clock),
        Some(clock + Duration::from_secs(1))
    );
    assert_retained(&prepared);
}

#[test]
fn a_live_execution_lease_keeps_its_context_and_actual_deadline_while_off() {
    let clock = Instant::now();
    let until = clock + Duration::from_secs(500);
    let prepared = prepare(1, Some(until));
    let images = files(&prepared);
    let read = read(&prepared, clock, Some(vec![entry(&prepared.current, true)])).unwrap();
    let value = read.progress.as_ref().unwrap();
    assert_eq!(value.phase, ProgressPhase::Waiting);
    assert!(value.forecasts_paused);
    assert_eq!(value.eligible, None);
    assert_eq!(value.next_at, prepared.retained.next_at);
    assert_eq!(read.execution_until, Some(until));
    assert_eq!(
        value.independent_work,
        [independent_work_tests::expected_work(
            &prepared.current,
            NOW
        )]
    );
    assert_eq!(read.next_poll(clock), Some(clock + Duration::from_secs(1)));
    assert_retained(&prepared);
    assert_eq!(files(&prepared), images);
}
