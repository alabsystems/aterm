// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Exercise the actual native driver up to its provider boundary in a private world.
use super::*;

struct LocalOnly(i64);
impl AccountsHost for LocalOnly {
    fn stdin_is_terminal(&self) -> bool {
        true
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn now(&self) -> i64 {
        self.0
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("no provider request")
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("no login replay")
    }
}

#[test]
fn actual_native_driver_prepares_publishes_and_changed_policy_never_dispatches() {
    for (changed_policy, loopback) in [(false, false), (true, false), (false, true)] {
        crate::harness::codex_grant_owner::with_preparation_fixture(
            |env, request, auth, turn, admission, artifact| {
                let store = env.accounts_store_for(Cli::Codex).unwrap();
                let roster = env.accounts_roster_for(Cli::Codex).unwrap();
                let default = env.home.as_ref().unwrap().join(".codex");
                aterm_secrets::fsx::ensure_private_dir(&default).unwrap();
                let default_auth = default.join("auth.json");
                std::fs::write(&default_auth, b"isolated current default remains untouched")
                    .unwrap();
                let default_before = std::fs::read(&default_auth).unwrap();
                let records = store.read_account_records(Some(&roster), env.now).unwrap();
                let row = records.roster.row(&request.label).unwrap();
                let staging = admission.staging().clone();
                let begun =
                    crate::harness::sign_in_ladder::asked_fixture(Cli::Codex, &request.label)
                        .begin_native(&request.label, turn.clone(), &store, env.now)
                        .unwrap();
                let route = Cli::Codex
                    .accounts()
                    .signin
                    .as_ref()
                    .unwrap()
                    .route(row.flow(Cli::Codex))
                    .unwrap();
                let mut plan = SignInPlan {
                    native_env: None,
                    native_row_occurrence: None,
                    native_row_admission: Some(admission),
                    native_required: true,
                    native_grant: None,
                    publication: crate::harness::codex_app_login::PublicationCustody::default(),
                    enrollment: Some(request.clone()),
                    artifact: Some(artifact),
                    recovered: None,
                    route,
                    hosts: route.hosts.of(row.vendor.region).unwrap(),
                    staging: staging.directory.clone(),
                    staging_text: staging.directory.to_str().unwrap().into(),
                    parent: staging.parent,
                    workflow: accounts::current_workflow(
                        records.raw_roster.row(&request.label).unwrap(),
                        &records.logins,
                    )
                    .unwrap(),
                    native_google: None,
                    native_recovery: None,
                    native_admission: None,
                    native_parent: None,
                    reauthentication: None,
                    _begun: begun,
                };
                if changed_policy {
                    let text = std::fs::read_to_string(&roster).unwrap();
                    let changed = text.replace("global-code@example.test", "changed@example.test");
                    assert_ne!(text, changed);
                    std::fs::write(&roster, changed).unwrap();
                }
                native_preparation_fixture::STOP.with(|stop| stop.set(!loopback));
                native_preparation_fixture::REACHED.with(|reached| reached.set(false));
                native_preparation_fixture::LOOPBACK.with(|mode| mode.set(loopback));
                native_preparation_fixture::EXCHANGES.with(|count| count.set(0));
                let lifetime = crate::harness::account_work::Lifetime::default();
                let (work, _) = crate::harness::codex_enrollment::owned_work(
                    request.clone(),
                    lifetime.enter().unwrap(),
                );
                let drove = work.with_native_context(env, |native| {
                    plan.drive(Cli::Codex, &LocalOnly(env.now), row, &store, Some(native))
                });
                native_preparation_fixture::STOP.with(|stop| stop.set(false));
                assert_eq!(
                    native_preparation_fixture::REACHED.with(std::cell::Cell::get),
                    !changed_policy
                );
                let (logins, warning) = store.load_logins(env.now).unwrap();
                assert!(warning.is_none());
                if changed_policy {
                    assert!(matches!(
                        drove.outcome,
                        crate::harness::login_drive::Outcome::PreparationFailed(
                            crate::harness::login_drive::NativePreparationStage::RowPolicy
                        )
                    ));
                    assert!(
                        logins
                            .enrollment
                            .catalogue_witness(request)
                            .unwrap()
                            .is_none()
                    );
                } else if !loopback {
                    assert!(matches!(
                        drove.outcome,
                        crate::harness::login_drive::Outcome::NotRun(_)
                    ));
                    assert_eq!(
                        logins
                            .enrollment
                            .catalogue_witness(request)
                            .unwrap()
                            .unwrap()
                            .phase,
                        crate::harness::codex_enrollment::CataloguePhase::Prepared
                    );
                } else {
                    assert!(matches!(
                        drove.outcome,
                        crate::harness::login_drive::Outcome::SignedIn { .. }
                    ));
                    assert!(plan.native_grant.is_some());
                    let mut error = Vec::new();
                    let published = bind_signed_in(
                        Cli::Codex,
                        env.now,
                        &LocalOnly(env.now),
                        Some(env),
                        &store,
                        (row, &roster),
                        records
                            .raw_roster
                            .row(&request.label)
                            .unwrap()
                            .dir
                            .as_deref(),
                        &plan,
                        accounts::Proof::Status,
                        "renew",
                        &mut Vec::new(),
                        &mut error,
                    );
                    assert_eq!(
                        published,
                        Renewal::Renewed,
                        "{}",
                        String::from_utf8_lossy(&error)
                    );
                    let current = store.load_logins(env.now).unwrap().0;
                    let applied = current.enrollment.lookup(request).unwrap().unwrap();
                    assert_eq!(
                        applied.phase,
                        crate::harness::codex_enrollment::Phase::Applied
                    );
                    assert!(applied.generation.is_some_and(|generation| generation > 0));
                    let (replay, _) = crate::harness::codex_enrollment::owned_work(
                        request.clone(),
                        lifetime.enter().unwrap(),
                    );
                    assert_eq!(
                        replay
                            .run_native_with_host(env, &LocalOnly(env.now))
                            .unwrap(),
                        applied
                    );
                }
                native_preparation_fixture::LOOPBACK.with(|mode| mode.set(false));
                assert_eq!(
                    native_preparation_fixture::EXCHANGES.with(std::cell::Cell::get),
                    usize::from(loopback)
                );
                assert_eq!(std::fs::read(default_auth).unwrap(), default_before);
                if !loopback {
                    assert!(!auth.exists(), "no provider response was materialized");
                    assert!(!auth.with_file_name("auth.json.refreshed").exists());
                }
            },
        );
    }
}
