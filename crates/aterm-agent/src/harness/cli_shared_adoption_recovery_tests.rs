// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn preclaimed_observation() -> [i64; 5] {
    let (_tmp, env, approval, mut host) = fixture(0);
    let own = candidate(&approval).row;
    let other = accounts::Row {
        label: "other".into(),
        email: Some("other@example.test".into()),
        dir: Some(
            env.accounts_store()
                .logins_dir()
                .join("shared-approved+1")
                .to_str()
                .unwrap()
                .into(),
        ),
        ..Default::default()
    };
    let text = accounts::append_rows_for(Cli::Claude, "", &[own, other]).unwrap();
    fs::write(&host.roster, &text).unwrap();
    host.before = Some(text.into_bytes());
    env.accounts_store()
        .transact(NOW, &mut None, |state| {
            Ok(accounts::note_auth(
                state,
                "shared-approved",
                accounts::Auth::SignedOut,
                NOW,
            ))
        })
        .unwrap();
    assert!(run(&env, &host, &approval).is_err());
    assert_eq!(host.calls.load(Ordering::SeqCst), 0);
    let unconsumed = env
        .accounts_store()
        .load_logins(NOW)
        .unwrap()
        .0
        .shared
        .is_empty();
    assert!(unconsumed);
    [
        host.calls.load(Ordering::SeqCst) as i64,
        approval.recoveries.load(Ordering::SeqCst) as i64,
        approval.recovery_accepted.load(Ordering::SeqCst) as i64,
        0,
        i64::from(unconsumed),
    ]
}

fn principal_observation(case: i64) -> [i64; 5] {
    let (_tmp, env, approval, mut host) = fixture(0);
    let store = env.accounts_store();
    if case == 22 {
        assert_eq!(run(&env, &host, &approval), Ok(true));
        host.before = Some(fs::read(&host.roster).unwrap());
        store
            .transact(NOW + 2, &mut None, |state| {
                Ok(accounts::note_auth(
                    state,
                    "shared-approved",
                    accounts::Auth::SignedOut,
                    NOW + 2,
                ))
            })
            .unwrap();
        approval.generation.store(2, Ordering::SeqCst);
        host.calls.store(0, Ordering::SeqCst);
    } else if case == 23 {
        let staging = store.claim_staging_dir("other").unwrap();
        store
            .transact_logins(NOW - 2, &mut None, |_, logins| {
                let token = logins.shared.begin(
                    accounts::shared::Intent {
                        source: accounts::shared::Source {
                            network: [1; 32],
                            account: [9; 32],
                        },
                        decision: [9; 32],
                        generation: 1,
                        configuration: [9; 32],
                        request: [9; 16],
                        label: "other".into(),
                        identity: "other@example.test".into(),
                        staging: staging.to_str().unwrap().into(),
                        before_generation: None,
                        begun_at: NOW - 2,
                    },
                    profile::principal_id("another-provider-uuid"),
                )?;
                logins
                    .shared
                    .finish(token, accounts::shared::Outcome::Unknown, None, NOW - 1)?;
                Ok(Vec::new())
            })
            .unwrap();
    }
    approval.unpinned.store(true, Ordering::SeqCst);
    host.mode = if case == 24 { 0 } else { 8 };
    let result = if case == 24 {
        run_with_provider(
            &env,
            &host,
            &approval,
            Arc::new(Observed {
                case,
                approval: approval.clone(),
            }),
        )
    } else {
        run(&env, &host, &approval)
    };
    assert!(result.is_err(), "case {case}");
    assert_eq!(approval.recoveries.load(Ordering::SeqCst), 1);
    let source = approval.prepare(&candidate(&approval).row).unwrap().source;
    assert_eq!(
        store
            .load_logins(NOW)
            .unwrap()
            .0
            .shared
            .latest(source)
            .unwrap()
            .outcome,
        accounts::shared::Outcome::Pending
    );
    effects(&env, &approval, host.calls.load(Ordering::SeqCst))
}

fn effects(env: &Env, approval: &Approval, calls: usize) -> [i64; 5] {
    let grant = approval
        .prepare(&accounts::Row {
            label: "shared-approved".into(),
            email: Some(EMAIL.into()),
            ..Default::default()
        })
        .unwrap();
    let logins = env.accounts_store().load_logins(NOW).unwrap().0;
    let attempt = logins.shared.latest(grant.source).unwrap();
    let retained = logins
        .shared
        .check_preparation(grant.source, grant.generation, grant.expected_principal)
        .is_err()
        && Path::new(&attempt.intent.staging).is_dir();
    [
        calls as i64,
        approval.recoveries.load(Ordering::SeqCst) as i64,
        approval.recovery_accepted.load(Ordering::SeqCst) as i64,
        i64::from(matches!(
            attempt.outcome,
            accounts::shared::Outcome::Published { .. }
        )),
        i64::from(retained),
    ]
}

struct UnknownAfter {
    complete: bool,
    calls: AtomicUsize,
}

impl shared_provider::Provider for UnknownAfter {
    fn execute(
        &self,
        _: &shared_provider::Context<'_>,
        _: &accounts::Row,
        _: &Path,
        _: &RenewBrowser,
    ) -> Result<(), String> {
        panic!("typed execution required")
    }
    fn execute_attempt(
        &self,
        context: &shared_provider::Context<'_>,
        row: &accounts::Row,
        staging: &Path,
        browser: &RenewBrowser,
    ) -> shared_provider::Execution {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.complete {
            shared_provider::ClaudeOauth
                .execute(context, row, staging, browser)
                .unwrap();
        }
        shared_provider::Execution::Unknown("provider reply was lost".into())
    }
    fn observe(
        &self,
        context: &shared_provider::Context<'_>,
        row: &accounts::Row,
        staging: &Path,
    ) -> Result<Option<Box<dyn shared_provider::LoginProof>>, String> {
        shared_provider::ClaudeOauth.observe(context, row, staging)
    }
}

fn unknown_observation(complete: bool) -> [i64; 5] {
    let (_tmp, env, approval, host) = fixture(0);
    let provider = Arc::new(UnknownAfter {
        complete,
        calls: AtomicUsize::new(0),
    });
    let result = run_with_provider(&env, &host, &approval, provider.clone());
    if complete {
        assert_eq!(result, Ok(true));
    } else {
        assert!(result.is_err());
    }
    assert_eq!(
        approval.recoveries.load(Ordering::SeqCst),
        usize::from(!complete)
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    effects(&env, &approval, provider.calls.load(Ordering::SeqCst))
}

fn checkpoint_observation() -> [i64; 5] {
    let (_tmp, env, approval, host) = fixture(1);
    assert_eq!(run(&env, &host, &approval), Ok(false));
    let store = env.accounts_store();
    let grant = approval.prepare(&candidate(&approval).row).unwrap();
    let before = store.load_logins(NOW).unwrap().0;
    let attempt = before.shared.latest(grant.source).unwrap();
    let staging = Path::new(&attempt.intent.staging);
    write_login(staging, EMAIL);
    accounts::write_renewal_checkpoint_for(
        Cli::Claude,
        None,
        &attempt.intent.label,
        EMAIL,
        &attempt.intent.staging,
        Some("max"),
        Some(accounts::Proof::Status),
        NOW,
        &before,
    )
    .unwrap();
    let checkpoint = staging.join(accounts::RENEWAL_CHECKPOINT_FILE);
    let bytes = fs::read(&checkpoint).unwrap();
    assert_eq!(run(&env, &host, &approval), Ok(true));
    assert_eq!(host.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fs::read(checkpoint).unwrap(), bytes);
    effects(&env, &approval, host.calls.load(Ordering::SeqCst))
}

fn superseded_checkpoint_observation() -> [i64; 5] {
    let (_tmp, env, approval, mut host) = fixture(1);
    assert_eq!(run(&env, &host, &approval), Ok(false));
    let store = env.accounts_store();
    let grant = approval.prepare(&candidate(&approval).row).unwrap();
    let before = store.load_logins(NOW).unwrap().0;
    let intent = &before.shared.latest(grant.source).unwrap().intent;
    let staging = Path::new(&intent.staging);
    write_login(staging, EMAIL);
    accounts::write_renewal_checkpoint_for(
        Cli::Claude,
        None,
        &intent.label,
        EMAIL,
        &intent.staging,
        Some("max"),
        Some(accounts::Proof::Status),
        NOW,
        &before,
    )
    .unwrap();
    let checkpoint = staging.join(accounts::RENEWAL_CHECKPOINT_FILE);
    let evidence = fs::read(&checkpoint).unwrap();
    let credential = fs::read(staging.join(".claude.json")).unwrap();
    approval.generation.store(2, Ordering::SeqCst);
    host.mode = 0;
    assert_eq!(run(&env, &host, &approval), Ok(true));
    assert_eq!(fs::read(checkpoint).unwrap(), evidence);
    assert_eq!(fs::read(staging.join(".claude.json")).unwrap(), credential);
    assert_ne!(
        store
            .load_logins(NOW)
            .unwrap()
            .0
            .shared
            .latest(grant.source)
            .unwrap()
            .intent
            .staging,
        intent.staging
    );
    effects(&env, &approval, host.calls.load(Ordering::SeqCst))
}

fn ownership_observation(unstarted: bool) -> [i64; 5] {
    let (_tmp, env, approval, mut host) = fixture(1);
    let provider = Arc::new(FirstResult {
        calls: AtomicUsize::new(0),
        unknown: false,
        suspend: None,
    });
    if unstarted {
        assert!(run_with_provider(&env, &host, &approval, provider.clone()).is_err());
    } else {
        assert_eq!(run(&env, &host, &approval), Ok(false));
    }
    let store = env.accounts_store();
    let grant = approval.prepare(&candidate(&approval).row).unwrap();
    let before = store.load_logins(NOW).unwrap().0;
    let attempt = before.shared.latest(grant.source).unwrap();
    if !unstarted {
        write_login(Path::new(&attempt.intent.staging), EMAIL);
    }
    let other = accounts::Row {
        label: "other".into(),
        email: Some("other@example.test".into()),
        dir: Some(attempt.intent.staging.clone()),
        ..Default::default()
    };
    let text = accounts::append_rows_for(Cli::Claude, "", &[other]).unwrap();
    fs::write(&host.roster, &text).unwrap();
    host.before = Some(text.into_bytes());
    host.mode = 0;
    let result = if unstarted {
        run_with_provider(&env, &host, &approval, provider.clone())
    } else {
        run(&env, &host, &approval)
    };
    assert!(result.is_err());
    assert_eq!(host.calls.load(Ordering::SeqCst), usize::from(!unstarted));
    assert_eq!(store.load_logins(NOW).unwrap().0, before);
    assert_eq!(fs::read(&host.roster).unwrap(), host.before.unwrap());
    if unstarted {
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }
    effects(
        &env,
        &approval,
        if unstarted {
            provider.calls.load(Ordering::SeqCst)
        } else {
            host.calls.load(Ordering::SeqCst)
        },
    )
}

struct Observed {
    case: i64,
    approval: Arc<Approval>,
}

struct DifferentEmail(Box<dyn shared_provider::LoginProof>);
impl shared_provider::LoginProof for DifferentEmail {
    fn identity(&self) -> &str {
        "other@example.test"
    }
    fn principal(&self) -> [u8; 32] {
        self.0.principal()
    }
    fn subscription(&self) -> Option<&str> {
        self.0.subscription()
    }
    fn check_current(&self) -> Result<(), String> {
        self.0.check_current()
    }
}

struct ZeroPrincipal(Box<dyn shared_provider::LoginProof>);
impl shared_provider::LoginProof for ZeroPrincipal {
    fn identity(&self) -> &str {
        self.0.identity()
    }
    fn principal(&self) -> [u8; 32] {
        [0; 32]
    }
    fn subscription(&self) -> Option<&str> {
        self.0.subscription()
    }
    fn check_current(&self) -> Result<(), String> {
        self.0.check_current()
    }
}

impl shared_provider::Provider for Observed {
    fn execute(
        &self,
        context: &shared_provider::Context<'_>,
        row: &accounts::Row,
        staging: &Path,
        browser: &RenewBrowser,
    ) -> Result<(), String> {
        shared_provider::ClaudeOauth.execute(context, row, staging, browser)
    }
    fn observe(
        &self,
        context: &shared_provider::Context<'_>,
        row: &accounts::Row,
        staging: &Path,
    ) -> Result<Option<Box<dyn shared_provider::LoginProof>>, String> {
        let proof = shared_provider::ClaudeOauth.observe(context, row, staging)?;
        match self.case {
            4 => return Ok(proof.map(|proof| Box::new(DifferentEmail(proof)) as _)),
            24 => return Ok(proof.map(|proof| Box::new(ZeroPrincipal(proof)) as _)),
            5 => fs::write(
                context.env.config.as_ref().unwrap(),
                "[harness]\nlogins = 'off'\n",
            )
            .unwrap(),
            6 => self.approval.live.store(false, Ordering::SeqCst),
            7 => fs::write(
                context.env.accounts_roster().unwrap(),
                "# edited during observation\n",
            )
            .unwrap(),
            8 => {
                context
                    .store
                    .transact_logins(NOW, &mut None, |state, logins| {
                        accounts::forget_account(state, logins, &row.label);
                        Ok(Vec::new())
                    })?;
            }
            9 => return Ok(None),
            12 => return Err("provider verification unavailable".into()),
            _ => {}
        }
        Ok(proof)
    }
}

fn observation(case: i64) -> [i64; 5] {
    match case {
        14 | 15 => return unknown_observation(case == 15),
        16 => return checkpoint_observation(),
        17 | 18 => return ownership_observation(case == 18),
        19 | 20 => {
            let cli = if case == 19 { Cli::Codex } else { Cli::Kimi };
            let (published, retained) =
                crate::harness::accounts_sync::shared_checkpoint_observation(cli);
            return [0, 0, 0, i64::from(published), i64::from(retained)];
        }
        21 => return preclaimed_observation(),
        27 => return superseded_checkpoint_observation(),
        22..=24 => return principal_observation(case),
        25 | 26 => {
            let (published, retained) =
                crate::harness::accounts_sync::shared_checkpoint_alias_observation(case == 26);
            return [0, 0, 0, i64::from(published), i64::from(retained)];
        }
        _ => {}
    }
    let mode = if matches!(case, 4 | 9 | 12 | 13) {
        0
    } else {
        8
    };
    let (_tmp, env, approval, mut host) = fixture(mode);
    let grant = approval.prepare(&candidate(&approval).row).unwrap();
    approval
        .recovery_unavailable
        .store(matches!(case, 10 | 11), Ordering::SeqCst);
    let provider = Arc::new(Observed {
        case,
        approval: approval.clone(),
    });
    let result = run_with_provider(&env, &host, &approval, provider);
    if case == 13 {
        assert_eq!(result, Ok(true));
    } else if case == 9 {
        assert_eq!(result, Ok(false));
    } else {
        assert!(result.is_err(), "case {case}: {result:?}");
    }
    let store = env.accounts_store();
    let before = store.load_logins(NOW).unwrap().0;
    let original = before.shared.latest(grant.source).unwrap().clone();
    let stage = Path::new(&original.intent.staging);
    let credential = fs::read(stage.join(".credentials.json")).unwrap();
    let metadata = fs::read(stage.join(".claude.json")).unwrap();
    assert!(
        before
            .shared
            .check_preparation(grant.source, grant.generation, grant.expected_principal)
            .is_err()
    );
    if matches!(case, 2 | 3 | 11) {
        let state_before = store.load(NOW).unwrap().0.to_json();
        if case == 11 {
            approval.recovery_unavailable.store(false, Ordering::SeqCst);
        }
        assert!(run(&env, &host, &approval).is_err());
        assert_eq!(store.load_logins(NOW).unwrap().0, before);
        assert_eq!(store.load(NOW).unwrap().0.to_json(), state_before);
    }
    if case == 3 {
        approval.generation.store(2, Ordering::SeqCst);
        host.mode = 0;
        assert_eq!(run(&env, &host, &approval), Ok(true));
    }
    let after = store.load_logins(NOW).unwrap().0;
    let latest = after.shared.latest(grant.source).unwrap();
    let published = matches!(latest.outcome, accounts::shared::Outcome::Published { .. });
    assert!(store.load(NOW).unwrap().0.active.is_none());
    if case == 3 {
        assert_ne!(latest.intent.request, original.intent.request);
        assert_ne!(latest.intent.staging, original.intent.staging);
    } else {
        assert_eq!(latest.intent, original.intent);
    }
    assert_eq!(
        fs::read(stage.join(".credentials.json")).unwrap(),
        credential
    );
    assert_eq!(fs::read(stage.join(".claude.json")).unwrap(), metadata);
    let retained = if published {
        true
    } else {
        latest.outcome == accounts::shared::Outcome::Pending
            && after
                .get(&latest.intent.label)
                .is_none_or(|binding| binding.dir.is_none())
    };
    [
        host.calls.load(Ordering::SeqCst) as i64,
        approval.recoveries.load(Ordering::SeqCst) as i64,
        approval.recovery_accepted.load(Ordering::SeqCst) as i64,
        i64::from(published),
        i64::from(retained),
    ]
}

pub(crate) fn transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let case = match action {
        "WrongPrincipal" => 1,
        "Retry" => 2,
        "Reissue" => 3,
        "WrongEmail" => 4,
        "Stopped" => 5,
        "Revoked" => 6,
        "ChangedRoster" => 7,
        "ChangedBinding" => 8,
        "MissingProof" => 9,
        "Unavailable" => 10,
        "RetryUnavailable" => 11,
        "ObservationError" => 12,
        "Healthy" => 13,
        "UnknownReply" => 14,
        "UnknownCompleted" => 15,
        "PriorCheckpoint" => 16,
        "ClaimedPending" => 17,
        "ClaimedUnstarted" => 18,
        "CodexCheckpoint" => 19,
        "KimiCheckpoint" => 20,
        "ClaimedBeforeBegin" => 21,
        "RetainedPrincipal" => 22,
        "OtherPrincipal" => 23,
        "ZeroPrincipal" => 24,
        "StagingAlias" => 25,
        "PrincipalAlias" => 26,
        "SupersededCheckpoint" => 27,
        _ => panic!("unknown adoption recovery action {action}"),
    };
    let [calls, requests, accepted, published, retained] = observation(case);
    let mut next = state.clone();
    for (key, value) in [
        ("case", case),
        ("calls", calls),
        ("requests", requests),
        ("accepted", accepted),
        ("published", published),
        ("retained", retained),
    ] {
        next.insert(key, value);
    }
    next
}

#[test]
fn adoption_recovery_conforms_and_catches_stuck_or_unfenced_requests() {
    let model = aterm_models_harness::shared_adoption_recovery_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 0);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let observed = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&observed),
                    "{} {state:?} -> {observed:?}",
                    action.name
                );
                let mut mutant = observed.clone();
                if action.name == "WrongPrincipal" {
                    mutant.insert("requests", 0);
                } else if action.name == "ChangedBinding" {
                    mutant.insert("requests", 1);
                } else {
                    continue;
                }
                assert!(!model.successors(action.name, &state).contains(&mutant));
            }
        }
    }
}
