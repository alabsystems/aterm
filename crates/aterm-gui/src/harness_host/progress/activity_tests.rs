// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::accounts_sync::{LoginActivity, LoginDiagnostic, LoginIdentity};

const SID: &str = "s-progress";
const CONVERSATION: &str = "conversation-a";
const BORN: u64 = 100;
const NOW: i64 = 100;

fn fixture() -> (HostHandle, Arc<Mutex<Kept>>, accounts::Row) {
    let (host, kept) = lifecycle_tests::fixture();
    let row = accounts::Row {
        label: "alternate".into(),
        email: Some("expected@example.test".into()),
        dir: Some("/synthetic/login".into()),
        ..Default::default()
    };
    {
        let mut kept = kept.lock().unwrap();
        kept.account.progress = None;
        kept.account.execution_progress = None;
        kept.account.activity_context = Some(AccountActivityContext {
            snapshot: lifecycle_tests::snapshot(),
            generation: Arc::clone(&kept.account.landing),
            born: BORN,
            state: "/synthetic/state".into(),
            labels: BTreeSet::from([row.label.clone()]),
            rows: vec![row.clone()].into(),
            policy: Arc::clone(&kept.account.progress_policy),
        });
    }
    (host, kept, row)
}

fn activity(row: &accounts::Row, active: bool) -> LoginActivity {
    LoginActivity {
        label: row.label.clone(),
        identity: Some(LoginIdentity::for_test(row)),
        route: Some("mailbox".into()),
        active,
        observed_at: 90,
        next_at: Some(150),
        diagnostic: Some(LoginDiagnostic::from_exit_for_test(row, 80, 1, None)),
    }
}

fn read(host: &HostHandle, entries: Vec<LoginActivity>) -> Result<Option<AccountProgress>, ()> {
    host.account_progress_with_activity(
        SID,
        4242,
        Some(BORN),
        Some(CONVERSATION),
        || (Instant::now(), Some(NOW)),
        |store, cli, now| {
            assert_eq!(store.dir, PathBuf::from("/synthetic/state/accounts"));
            assert_eq!(cli, accounts::Cli::Claude);
            assert_eq!(now, NOW);
            Some(entries)
        },
    )
}

fn assert_no_decision(value: &AccountProgress) {
    assert_eq!(value.observed_at, 0, "activity is not an account judgment");
    assert_eq!(value.account, None);
    assert_eq!(value.known, None);
    assert_eq!(value.eligible, None);
    assert_eq!(value.next_at, None);
    assert_eq!(value.own_claim, None);
    assert_eq!(value.wall, None);
    assert!(value.login_diagnostics.is_empty());
}

#[test]
fn a_context_shows_actual_sign_in_before_any_account_decision() {
    let (host, kept, row) = fixture();
    let (snapshot, generation, policy) = {
        let kept = kept.lock().unwrap();
        let context = kept.account.activity_context.as_ref().unwrap();
        (
            context.snapshot.clone(),
            Arc::clone(&context.generation),
            Arc::clone(&context.policy),
        )
    };
    let value = read(&host, vec![activity(&row, true)]).unwrap().unwrap();
    assert_eq!(
        value.phase,
        ProgressPhase::SigningIn {
            label: row.label.clone(),
            route: Some("mailbox".into()),
        }
    );
    assert_no_decision(&value);
    assert!(!value.forecasts_paused);
    let observed = aterm_agent::harness::usage::rfc3339_utc(90);
    assert!(
        value
            .details
            .iter()
            .any(|detail| detail.contains(&observed))
    );
    let kept = kept.lock().unwrap();
    assert!(kept.account.progress.is_none());
    assert!(kept.account.execution_progress.is_none());
    let context = kept.account.activity_context.as_ref().unwrap();
    assert_eq!(context.snapshot, snapshot);
    assert!(Arc::ptr_eq(&context.generation, &generation));
    assert!(Arc::ptr_eq(&context.policy, &policy));
    assert_eq!(context.rows.as_ref(), std::slice::from_ref(&row));
}

#[test]
fn an_activity_context_alone_does_not_publish_history_or_a_decision() {
    let (host, kept, row) = fixture();
    for entries in [Vec::new(), vec![activity(&row, false)]] {
        assert_eq!(read(&host, entries).unwrap(), None);
        let kept = kept.lock().unwrap();
        assert!(kept.account.progress.is_none());
        assert!(kept.account.execution_progress.is_none());
    }
}

#[test]
fn context_only_sign_in_requires_one_exact_captured_row() {
    for case in 0..8 {
        let (host, kept, row) = fixture();
        let mut entry = activity(&row, true);
        {
            let mut kept = kept.lock().unwrap();
            let context = kept.account.activity_context.as_mut().unwrap();
            let mut current = row.clone();
            match case {
                0 => {}
                1 => current.email = Some("replacement@example.test".into()),
                2 => current.dir = Some("/synthetic/replacement-login".into()),
                3 => current.identity_email = Some("different-principal@example.test".into()),
                4 => current.browser = Some("Profile 2".into()),
                5 => entry.identity = None,
                6 => entry.label = "another-label".into(),
                7 => {}
                _ => unreachable!(),
            }
            context.rows = if case == 7 {
                vec![current.clone(), current].into()
            } else {
                vec![current].into()
            };
        }
        let value = read(&host, vec![entry]).unwrap();
        if case == 0 {
            let value = value.unwrap();
            assert!(matches!(value.phase, ProgressPhase::SigningIn { .. }));
            assert_no_decision(&value);
        } else {
            assert_eq!(value, None, "case {case} attached unconfirmed activity");
        }
    }
}

#[test]
fn activity_contexts_fence_process_birth_conversation_and_authentication() {
    let (host, _, row) = fixture();
    for (pid, born, conversation) in [
        (4243, Some(BORN), Some(CONVERSATION)),
        (4242, Some(BORN + 1), Some(CONVERSATION)),
        (4242, None, Some(CONVERSATION)),
        (4242, Some(BORN), Some("another-conversation")),
        (4242, Some(BORN), None),
    ] {
        let value = host
            .account_progress_with_activity(
                SID,
                pid,
                born,
                conversation,
                || (Instant::now(), Some(NOW)),
                |_, _, _| panic!("a mismatched query must not borrow an activity context"),
            )
            .unwrap();
        assert_eq!(value, None);
    }
    assert!(read(&host, vec![activity(&row, true)]).unwrap().is_some());
    for case in 0..8 {
        let (host, kept, _) = fixture();
        {
            let mut kept = kept.lock().unwrap();
            match case {
                0 => kept.snapshot.as_mut().unwrap().pid = 4243,
                1 => kept.snapshot.as_mut().unwrap().start = "reused-pid".into(),
                2 => kept.snapshot.as_mut().unwrap().session = Some("another-conversation".into()),
                3 => kept.snapshot.as_mut().unwrap().login = Some("/synthetic/other-login".into()),
                4 => kept.snapshot.as_mut().unwrap().home = Some("/synthetic/other-home".into()),
                5 => kept.snapshot.as_mut().unwrap().stored_login = true,
                6 => kept.account.landing = Arc::new(Mutex::new(())),
                7 => kept.account.activity_context = None,
                _ => unreachable!(),
            }
        }
        let value = host
            .account_progress_with_activity(
                SID,
                4242,
                Some(BORN),
                Some(CONVERSATION),
                || (Instant::now(), Some(NOW)),
                |_, _, _| panic!("case {case} retained a stale activity context"),
            )
            .unwrap();
        assert_eq!(value, None);
    }
}

fn set_work(kept: &mut Kept, row: &accounts::Row, kind: i32) {
    let origin = AccountContext {
        roster: None,
        state: "/synthetic/state".into(),
        home: "/synthetic/home".into(),
    };
    match kind {
        0 => {
            kept.account.probing = Some(Probing {
                at: 90,
                label: row.label.clone(),
                pid: 4242,
                start: "process-a".into(),
                row: row.clone(),
                who: accounts::ProcessLogin {
                    login: None,
                    default_home: true,
                    stored_login: true,
                },
                roster: "/synthetic/roster".into(),
                state: "/synthetic/state".into(),
            });
        }
        1 => {
            kept.account.flying = Some(Flying {
                origin,
                to: row.label.clone(),
                at: AccountAt::Wall,
                login: None,
                pid: 4242,
                start: "process-a".into(),
                rejected: None,
            });
        }
        2 => {
            kept.account.moved = Some(Moved {
                id: MoveId::default(),
                origin,
                to: row.label.clone(),
                at: AccountAt::Wall,
                login: None,
                process: kept.snapshot.clone().unwrap(),
                answered: false,
                untold: false,
            });
        }
        _ => unreachable!(),
    }
}

#[test]
fn context_only_projection_keeps_current_work_precedence() {
    for kind in 0..3 {
        for active in [false, true] {
            for rotation_off in [false, true] {
                let (host, kept, row) = fixture();
                set_work(&mut kept.lock().unwrap(), &row, kind);
                if rotation_off {
                    host.shared
                        .switches
                        .account_controls
                        .store(ACCOUNTS_OFF, Ordering::SeqCst);
                }
                let entries = if active {
                    vec![activity(&row, true)]
                } else {
                    Vec::new()
                };
                let value = read(&host, entries).unwrap().unwrap();
                assert_eq!(
                    value.phase,
                    match (kind, active) {
                        (0, false) => ProgressPhase::Probing,
                        (0, true) => ProgressPhase::SigningIn {
                            label: row.label.clone(),
                            route: Some("mailbox".into()),
                        },
                        (1, _) => ProgressPhase::Switching {
                            to: row.label.clone()
                        },
                        (2, _) => ProgressPhase::Verifying {
                            to: row.label.clone()
                        },
                        _ => unreachable!(),
                    }
                );
                assert_no_decision(&value);
            }
        }
    }
}

#[test]
fn off_and_retired_policy_preserve_actual_activity_without_forecasts() {
    for case in 0..3 {
        let (host, kept, row) = fixture();
        match case {
            0 => {
                let mut hooks = host.hooks.clone();
                hooks.note = Arc::new(|_| {});
                publish_off(
                    &host.shared,
                    &Workers::default(),
                    &HashMap::new(),
                    &OffSessions {
                        words: HashMap::from([(SID.into(), Autopilot::Off)]),
                        default: Autopilot::Cruise,
                    },
                    &hooks,
                );
            }
            1 => kept.lock().unwrap().account.progress_policy = Arc::new(()),
            2 => host
                .shared
                .switches
                .account_controls
                .store(ACCOUNTS_OFF, Ordering::SeqCst),
            _ => unreachable!(),
        }
        let value = read(&host, vec![activity(&row, true)]).unwrap().unwrap();
        assert!(matches!(value.phase, ProgressPhase::SigningIn { .. }));
        assert_eq!(value.forecasts_paused, case != 2);
        assert_no_decision(&value);
    }
}

#[test]
fn activity_contexts_do_not_replace_normal_decisions_or_execution_receipts() {
    for execution in [false, true] {
        let (host, kept, row) = fixture();
        let until = Instant::now() + Duration::from_secs(100);
        let retained = {
            let mut kept = kept.lock().unwrap();
            let mut value = AccountProgress::unknown(10);
            value.account = Some("current".into());
            value.known = Some(2);
            value.eligible = Some(0);
            value.phase = ProgressPhase::Limited;
            value.wall = Some("weekly".into());
            value.details.push("retained blocker".into());
            value.next_at = Some(160);
            let bound = BoundProgress {
                snapshot: kept.snapshot.clone().unwrap(),
                generation: Arc::clone(&kept.account.landing),
                born: Some(BORN),
                state: Some("/synthetic/state".into()),
                labels: BTreeSet::from([row.label.clone()]),
                rows: vec![row.clone()].into(),
                policy: Arc::clone(&kept.account.progress_policy),
                execution_until: execution.then_some(until),
                value: value.clone(),
            };
            if execution {
                kept.account.execution_progress = Some(bound);
            } else {
                kept.account.progress = Some(bound);
            }
            value
        };
        assert_eq!(read(&host, Vec::new()).unwrap(), Some(retained.clone()));
        let value = read(&host, vec![activity(&row, true)]).unwrap().unwrap();
        assert!(matches!(value.phase, ProgressPhase::SigningIn { .. }));
        assert_eq!(value.observed_at, retained.observed_at);
        assert_eq!(value.account, retained.account);
        assert_eq!(value.known, retained.known);
        assert_eq!(value.eligible, retained.eligible);
        assert_eq!(value.wall, retained.wall);
        assert_eq!(value.details.first(), retained.details.first());
        assert_eq!(value.next_at, Some(if execution { 160 } else { 150 }));
        let kept = kept.lock().unwrap();
        let bound = if execution {
            kept.account.execution_progress.as_ref().unwrap()
        } else {
            kept.account.progress.as_ref().unwrap()
        };
        assert_eq!(bound.value, retained);
    }
}

#[test]
fn independent_metadata_does_not_replace_an_existing_decisions_account_context() {
    for execution in [false, true] {
        for case in 0..3 {
            let (host, kept, current) = fixture();
            let mut captured = current.clone();
            if case != 0 {
                captured.email = Some("earlier-principal@example.test".into());
            }
            let captured_state = if case == 1 {
                PathBuf::from("/synthetic/state")
            } else {
                PathBuf::from("/synthetic/decision-state")
            };
            let retained = {
                let mut kept = kept.lock().unwrap();
                let mut value = AccountProgress::unknown(10);
                value.phase = ProgressPhase::Waiting;
                value.account = Some("current".into());
                value.known = Some(2);
                value.eligible = Some(1);
                value.next_at = Some(160);
                let bound = BoundProgress {
                    snapshot: kept.snapshot.clone().unwrap(),
                    generation: Arc::clone(&kept.account.landing),
                    born: Some(BORN),
                    state: Some(captured_state.clone()),
                    labels: BTreeSet::from([captured.label.clone()]),
                    rows: vec![captured].into(),
                    policy: Arc::clone(&kept.account.progress_policy),
                    execution_until: execution.then(|| Instant::now() + Duration::from_secs(100)),
                    value: value.clone(),
                };
                if execution {
                    kept.account.execution_progress = Some(bound);
                } else {
                    kept.account.progress = Some(bound);
                }
                value
            };
            let value = host
                .account_progress_with_activity(
                    SID,
                    4242,
                    Some(BORN),
                    Some(CONVERSATION),
                    || (Instant::now(), Some(NOW)),
                    |store, _, _| {
                        assert_eq!(store.dir, captured_state.join("accounts"));
                        Some(
                            if store.dir.as_path() == Path::new("/synthetic/state/accounts") {
                                vec![activity(&current, true)]
                            } else {
                                Vec::new()
                            },
                        )
                    },
                )
                .unwrap();
            let mut value = value.unwrap();
            let work = std::mem::take(&mut value.independent_work);
            assert_eq!(value, retained, "primary context case {case}");
            assert_eq!(
                work,
                if case == 1 {
                    vec![ActiveSignIn {
                        label: current.label.clone(),
                        route: Some("mailbox".into()),
                        observed_at: 90,
                    }]
                } else {
                    Vec::new()
                },
                "independent context case {case}"
            );
            let kept = kept.lock().unwrap();
            let bound = if execution {
                kept.account.execution_progress.as_ref().unwrap()
            } else {
                kept.account.progress.as_ref().unwrap()
            };
            assert_eq!(bound.value, retained);
        }
    }
}

#[test]
fn a_busy_activity_observation_is_unavailable_instead_of_absent() {
    let (host, kept, _) = fixture();
    let value = host.account_progress_with_activity(
        SID,
        4242,
        Some(BORN),
        Some(CONVERSATION),
        || (Instant::now(), Some(NOW)),
        |_, _, _| None,
    );
    assert!(value.is_err());
    let kept = kept.lock().unwrap();
    assert!(kept.account.progress.is_none());
    assert!(kept.account.execution_progress.is_none());
}
