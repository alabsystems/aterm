// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use accounts::shared::Outcome;

struct RevokeAfterCheck(Arc<Approval>);
impl Authority for RevokeAfterCheck {
    fn prepare(&self, row: &accounts::Row) -> Result<Grant, String> {
        self.0.prepare(row)
    }
    fn current(&self, grant: &Grant, row: &accounts::Row) -> Result<(), String> {
        self.0.current(grant, row)?;
        self.0.live.store(false, Ordering::SeqCst);
        Ok(())
    }
    fn request_recovery(
        &self,
        grant: &Grant,
        row: &accounts::Row,
        intent: &accounts::shared::Intent,
    ) -> Result<(), String> {
        self.0.request_recovery(grant, row, intent)
    }
}

fn observations(case: i64) -> Vec<aterm_spec::interp::State> {
    let (_tmp, env, approval, mut host) = fixture(0);
    let unstarted = (4..=6).contains(&case) || case == 16;
    let provider = Arc::new(FirstResult {
        calls: AtomicUsize::new(0),
        unknown: !unstarted,
        suspend: None,
    });
    assert!(run_with_provider(&env, &host, &approval, provider.clone()).is_err());
    approval.recoveries.store(0, Ordering::SeqCst);
    approval.recovery_accepted.store(0, Ordering::SeqCst);
    let store = env.accounts_store();
    let grant = approval.prepare(&candidate(&approval).row).unwrap();
    let original = store
        .load_logins(NOW)
        .unwrap()
        .0
        .shared
        .latest(grant.source)
        .unwrap()
        .clone();
    assert_eq!(
        original.outcome,
        if unstarted {
            Outcome::NotStarted
        } else {
            Outcome::Pending
        }
    );
    let stage = Path::new(&original.intent.staging);
    fs::write(
        stage.join("retained-evidence"),
        b"private retained preparation",
    )
    .unwrap();
    let kept = store.dir.join("retained-stage");
    fs::rename(stage, &kept).unwrap();
    let evidence = fs::read(kept.join("retained-evidence")).unwrap();
    match case {
        7 => approval.live.store(false, Ordering::SeqCst),
        8 => fs::write(
            config_beside_for_test(&host.roster),
            "[harness]\nlogins = 'off'\n",
        )
        .unwrap(),
        9 => fs::write(
            &host.roster,
            accounts::append_rows_for(
                Cli::Claude,
                "",
                &[accounts::Row {
                    label: original.intent.label.clone(),
                    email: Some("changed@example.test".into()),
                    ..Default::default()
                }],
            )
            .unwrap(),
        )
        .unwrap(),
        10 => {
            store
                .transact_logins(NOW, &mut None, |state, logins| {
                    accounts::forget_account(state, logins, &original.intent.label);
                    Ok(Vec::new())
                })
                .unwrap();
        }
        11 => fs::write(
            &host.roster,
            accounts::append_rows_for(
                Cli::Claude,
                "",
                &[accounts::Row {
                    label: "other".into(),
                    email: Some("other@example.test".into()),
                    dir: Some(original.intent.staging.clone()),
                    ..Default::default()
                }],
            )
            .unwrap(),
        )
        .unwrap(),
        12 | 13 => approval.recovery_unavailable.store(true, Ordering::SeqCst),
        14 => fs::write(stage, b"not a directory").unwrap(),
        16 => host.mode = 7,
        17 | 22 | 23 => {
            let retained_parent = store.dir.join("retained-parent");
            fs::rename(store.logins_dir(), &retained_parent).unwrap();
            #[cfg(unix)]
            if case == 22 {
                std::os::unix::fs::symlink(&retained_parent, store.logins_dir()).unwrap();
            } else if case == 23 {
                std::os::unix::fs::symlink(retained_parent.join("absent"), store.logins_dir())
                    .unwrap();
            }
        }
        #[cfg(unix)]
        24 => {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(store.logins_dir(), fs::Permissions::from_mode(0o755)).unwrap();
        }
        #[cfg(unix)]
        20 => std::os::unix::fs::symlink(&kept, stage).unwrap(),
        #[cfg(unix)]
        21 => std::os::unix::fs::symlink(kept.join("absent"), stage).unwrap(),
        _ => {}
    }
    let files = [
        host.roster.clone(),
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.state_witness_file(),
        store.logins_witness_file(),
        store.wake_path(),
    ];
    let before = files.each_ref().map(|path| fs::read(path).ok());
    let snapshot = |phase| {
        let logins = store.load_logins(NOW).unwrap().0;
        let current = logins.shared.latest(grant.source).unwrap();
        let published = matches!(current.outcome, Outcome::Published { .. });
        if !published {
            assert_eq!(current, &original);
            if !matches!(case, 14 | 20 | 21 | 22 | 23 | 24) {
                assert!(!stage.exists());
            }
        }
        aterm_spec::interp::State::from([
            ("case", phase),
            ("calls", provider.calls.load(Ordering::SeqCst) as i64),
            (
                "requests",
                approval.recoveries.load(Ordering::SeqCst) as i64,
            ),
            (
                "accepted",
                approval.recovery_accepted.load(Ordering::SeqCst) as i64,
            ),
            ("published", i64::from(published)),
            (
                "unchanged",
                i64::from(files.each_ref().map(|path| fs::read(path).ok()) == before),
            ),
            (
                "kept",
                i64::from(fs::read(kept.join("retained-evidence")).unwrap() == evidence),
            ),
        ])
    };
    let mut result = vec![snapshot(0)];
    let run_missing = || {
        if case == 15 {
            let mut next = candidate(&approval);
            next.authority = Arc::new(RevokeAfterCheck(approval.clone()));
            next.provider = provider.clone();
            let lock = adoption_sign_in(&store, &host.roster, &next);
            super::super::shared_adoption::adopt(&env, &host, &store, &lock, &host.roster, next)
        } else {
            run_with_provider(&env, &host, &approval, provider.clone())
        }
    };
    assert!(run_missing().is_err());
    let first = match case {
        2 | 3 => 1,
        5 | 6 => 4,
        13 => 12,
        _ => case,
    };
    result.push(snapshot(first));
    if matches!(case, 2 | 3 | 5 | 6 | 13) {
        approval.recovery_unavailable.store(false, Ordering::SeqCst);
        assert!(run_missing().is_err());
        result.push(snapshot(match case {
            3 => 2,
            6 => 5,
            _ => case,
        }));
    }
    if matches!(case, 3 | 6) {
        approval.generation.store(2, Ordering::SeqCst);
        assert_eq!(
            run_with_provider(&env, &host, &approval, provider.clone()),
            Ok(true)
        );
        result.push(snapshot(case));
        let logins = store.load_logins(NOW).unwrap().0;
        assert_ne!(
            logins.shared.latest(grant.source).unwrap().intent.request,
            original.intent.request
        );
        assert!(store.load(NOW).unwrap().0.active.is_none());
    }
    result
}

pub(crate) fn transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let case = match action {
        "Missing" => 1,
        "Retry" => 2,
        "Reissue" => 3,
        "Unstarted" => 4,
        "RetryUnstarted" => 5,
        "ReissueUnstarted" => 6,
        "Revoked" => 7,
        "Disabled" => 8,
        "RowChanged" => 9,
        "BindingChanged" => 10,
        "Claimed" => 11,
        "PublicationUnavailable" => 12,
        "RetryPublication" => 13,
        "FileEntry" => 14,
        "RevokedDuringCheck" => 15,
        "UnstartedWithoutOpener" => 16,
        "MissingParent" => 17,
        _ => panic!("unknown staging action {action}"),
    };
    let steps = observations(case);
    let phase = if matches!(case, 3 | 6) {
        3
    } else if matches!(case, 2 | 5 | 13) {
        2
    } else {
        1
    };
    assert_eq!(&steps[phase - 1], state);
    steps[phase].clone()
}

#[test]
fn missing_shared_staging_conforms_and_catches_replay_or_lost_recovery() {
    let model = aterm_models_harness::shared_staging_recovery_model();
    assert_eq!(model.mutant_dials(), (1..=11).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 0);
    // Automatic Claude adoption links a headless opener (`cli::opener_link`) only on Unix; off
    // Unix `automatic_shared_adoption_refuses_where_aterm_links_no_opener` holds the refusal.
    if !cfg!(unix) {
        return;
    }
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let observed = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&observed),
                    "{} {state:?} -> {observed:?}",
                    action.name
                );
                let mut replay = observed.clone();
                replay.insert("calls", 99);
                assert!(!model.successors(action.name, &state).contains(&replay));
            }
        }
    }
}

#[test]
#[cfg(unix)]
fn unsafe_shared_staging_never_triggers_provider_or_recovery_requests() {
    for case in [20, 21, 22, 23, 24] {
        let states = observations(case);
        let last = states.last().unwrap();
        assert_eq!(last["calls"], 1);
        assert_eq!(last["requests"], 0);
        assert_eq!(last["kept"], 1);
        assert_eq!(last["unchanged"], 1);
    }
}
