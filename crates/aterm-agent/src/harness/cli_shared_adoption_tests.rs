// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::shared_renewal::{Authority, Grant};
use super::*;
use crate::harness::accounts_sync::shared_renewal::Candidate;
use std::fs;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

const EMAIL: &str = "owner@example.test";
const NOW: i64 = 1_800_000_000;

#[cfg(unix)]
#[path = "cli_shared_native_publication_tests.rs"]
mod native_publication;

struct Approval {
    cancel_on_current: std::sync::Mutex<Option<crate::harness::account_work::Lifetime>>,
    live: AtomicBool,
    generation: AtomicU64,
    recoveries: AtomicUsize,
    recovery_accepted: AtomicUsize,
    recovery_unavailable: AtomicBool,
    unpinned: AtomicBool,
}
impl Authority for Approval {
    fn prepare(&self, row: &accounts::Row) -> Result<Grant, String> {
        if !self.live.load(Ordering::SeqCst) {
            return Err("revoked".into());
        }
        if row.expected_identity_email() != Some(EMAIL) || row.email.as_deref() != Some(EMAIL) {
            return Err("wrong candidate".into());
        }
        let generation = self.generation.load(Ordering::SeqCst);
        Ok(Grant {
            source: accounts::shared::Source {
                network: [1; 32],
                account: [2; 32],
            },
            decision: [generation as u8; 32],
            generation,
            configuration: [4; 32],
            identity: EMAIL.into(),
            expected_principal: (!self.unpinned.load(Ordering::SeqCst))
                .then(|| profile::principal_id("provider-uuid").unwrap()),
            required_browser_profile: None,
        })
    }
    fn current(&self, grant: &Grant, row: &accounts::Row) -> Result<(), String> {
        let result = (self.prepare(row)? == *grant)
            .then_some(())
            .ok_or_else(|| "changed".into());
        if result.is_ok()
            && let Some(work) = self.cancel_on_current.lock().unwrap().take()
        {
            drop(work.pause().unwrap());
        }
        result
    }
    fn request_recovery(
        &self,
        grant: &Grant,
        row: &accounts::Row,
        intent: &accounts::shared::Intent,
    ) -> Result<(), String> {
        self.current(grant, row)?;
        assert_eq!(intent.source, grant.source);
        assert_eq!(intent.decision, grant.decision);
        assert_ne!(intent.request, [0; 16]);
        self.recoveries.fetch_add(1, Ordering::SeqCst);
        if self.recovery_unavailable.load(Ordering::SeqCst) {
            return Err("recovery request unavailable".into());
        }
        self.recovery_accepted.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct Host {
    work: Option<crate::harness::account_work::Scope>,
    cancel: Option<crate::harness::account_work::Lifetime>,
    status_calls: AtomicUsize,
    approval: Arc<Approval>,
    calls: AtomicUsize,
    online_calls: AtomicUsize,
    mode: u8,
    roster: PathBuf,
    before: Option<Vec<u8>>,
    state: PathBuf,
}
fn write_login(path: &Path, email: &str) {
    aterm_uds::atomic_file::write(
        &path.join(".claude.json"),
        format!(r#"{{"oauthAccount":{{"emailAddress":"{email}","accountUuid":"provider-uuid"}}}}"#)
            .as_bytes(),
        0o600,
    )
    .unwrap();
    let credential = path.join(".credentials.json");
    fs::write(
        &credential,
        r#"{"claudeAiOauth":{"accessToken":"synthetic-access","expiresAt":4102444800000}}"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&credential, fs::Permissions::from_mode(0o600)).unwrap();
    }
}
impl AccountsHost for Host {
    fn renew_outcome(
        &self,
        home: &str,
        email: &str,
        browser: &RenewBrowser,
    ) -> Result<RenewalExit, RenewalFailure> {
        let code = self.renew_flow(home, email, browser)?;
        if self.mode == 16 {
            Err(RenewalFailure::deferred())
        } else {
            Ok(code.into())
        }
    }
    fn work_cancelled(&self) -> bool {
        self.work
            .as_ref()
            .is_some_and(crate::harness::account_work::Scope::cancelled)
    }

    fn current_exe(&self) -> Option<PathBuf> {
        (self.mode != 7).then(|| std::env::current_exe().unwrap())
    }
    fn shared_claude_profile(
        &self,
        token: &str,
        _: std::time::Duration,
    ) -> Result<shared_provider::ClaudeProfile, shared_provider::ObservationError> {
        assert_eq!(token, "synthetic-access");
        self.online_calls.fetch_add(1, Ordering::SeqCst);
        if self.mode == 13 {
            drop(self.cancel.as_ref().unwrap().pause().unwrap());
        }
        if self.mode == 9 {
            return Err(shared_provider::ObservationError::Unavailable(
                "fixture canonical provider outage".into(),
            ));
        }
        if self.mode == 10 {
            return Err(shared_provider::ObservationError::RecoveryRequired(
                "fixture rejected token".into(),
            ));
        }
        Ok(shared_provider::ClaudeProfile {
            email: if self.mode == 6 {
                "other@example.test"
            } else {
                EMAIL
            }
            .into(),
            principal: profile::principal_id(if self.mode == 8 {
                "another-provider-uuid"
            } else {
                "provider-uuid"
            })
            .unwrap(),
        })
    }
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("staging only")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("named flow only")
    }
    fn now(&self) -> i64 {
        NOW + 1
    }
    fn renew_flow(&self, home: &str, _: &str, browser: &RenewBrowser) -> Result<i32, String> {
        let RenewBrowser::Remote {
            opener: Some(opener),
        } = browser
        else {
            panic!("automatic sign-in must use the headless opener");
        };
        // The opener links to the resolved executable (`opener_link` canonicalizes it), so a test
        // binary reached through a symlinked target directory compares by its resolved path.
        assert_eq!(
            fs::read_link(opener).unwrap(),
            fs::canonicalize(std::env::current_exe().unwrap()).unwrap()
        );
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            fs::read(&self.roster).ok(),
            self.before,
            "the roster stays unchanged during provider I/O"
        );
        if self.mode != 1 {
            write_login(
                Path::new(home),
                if self.mode == 3 {
                    "other@example.test"
                } else {
                    EMAIL
                },
            );
            if self.mode == 8 {
                fs::write(
                    Path::new(home).join(".claude.json"),
                    format!(r#"{{"oauthAccount":{{"emailAddress":"{EMAIL}","accountUuid":"another-provider-uuid"}}}}"#),
                )
                .unwrap();
            }
        }
        if self.mode == 11 {
            drop(self.cancel.as_ref().unwrap().pause().unwrap());
        }
        if self.mode == 2 {
            self.approval.live.store(false, Ordering::SeqCst);
        }
        if self.mode == 4 {
            fs::write(
                config_beside_for_test(&self.roster),
                "[harness]\nlogins = 'off'\n",
            )
            .unwrap();
        }
        if self.mode == 5 {
            accounts::Store::at(&self.state)
                .transact_logins(NOW, &mut None, |state, logins| {
                    accounts::forget_account(state, logins, "shared-approved");
                    Ok(Vec::new())
                })
                .unwrap();
        }
        Ok(0)
    }
    fn status_in_home(&self, _: &str) -> Result<Captured, String> {
        self.status_calls.fetch_add(1, Ordering::SeqCst);
        if self.mode == 12 {
            drop(self.cancel.as_ref().unwrap().pause().unwrap());
        }
        Ok(Captured { code: Some(0), stdout: format!(r#"{{"loggedIn":true,"authMethod":"claude.ai","email":"{EMAIL}","subscriptionType":"max"}}"#).into_bytes() })
    }
    fn claude_identity_in_home(&self, home: &str, email: &str) -> profile::IdentityObservation {
        profile::identity_in_home(Path::new(home), email)
    }
}
fn config_beside_for_test(roster: &Path) -> PathBuf {
    roster.parent().unwrap().join("aterm.toml")
}
fn candidate(approval: &Arc<Approval>) -> Candidate {
    Candidate {
        row: accounts::Row {
            label: "shared-approved".into(),
            email: Some(EMAIL.into()),
            ..Default::default()
        },
        authority: approval.clone(),
        cli: Cli::Claude,
        provider: Arc::new(super::super::shared_provider::ClaudeOauth),
        ordinary_renewal: false,
    }
}
fn fixture(mode: u8) -> (Tmp, Env, Arc<Approval>, Host) {
    let tmp = tmp_dir("shared-adoption");
    let mut env = env_at(&tmp);
    env.now = NOW;
    let approval = Arc::new(Approval {
        cancel_on_current: std::sync::Mutex::new(None),
        live: AtomicBool::new(true),
        generation: AtomicU64::new(1),
        recoveries: AtomicUsize::new(0),
        recovery_accepted: AtomicUsize::new(0),
        recovery_unavailable: AtomicBool::new(false),
        unpinned: AtomicBool::new(false),
    });
    let host = Host {
        work: None,
        cancel: None,
        status_calls: AtomicUsize::new(0),
        approval: approval.clone(),
        calls: AtomicUsize::new(0),
        online_calls: AtomicUsize::new(0),
        mode,
        roster: env.accounts_roster().unwrap(),
        before: None,
        state: env.state.clone(),
    };
    (tmp, env, approval, host)
}

/// Unix only, where aterm links the headless opener (`cli::opener_link`) and reads private
/// staged evidence (`shared_provider::read_staged_bytes`); off Unix
/// `automatic_shared_adoption_refuses_where_aterm_links_no_opener` holds the refusal.
#[cfg(unix)]
#[test]
fn an_unknown_attempt_requests_a_successor_without_repeating_the_consumed_action() {
    let (_tmp, env, approval, host) = fixture(0);
    let provider = Arc::new(FirstResult {
        calls: AtomicUsize::new(0),
        unknown: true,
        suspend: None,
    });
    assert!(run_with_provider(&env, &host, &approval, provider.clone()).is_err());
    assert_eq!(approval.recoveries.load(Ordering::SeqCst), 1);
    let source = approval.prepare(&candidate(&approval).row).unwrap().source;
    let before = env.accounts_store().load_logins(NOW).unwrap().0;
    let request = before.shared.latest(source).unwrap().intent.request;
    assert_eq!(
        run_with_provider(&env, &host, &approval, provider.clone()),
        Ok(false)
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(approval.recoveries.load(Ordering::SeqCst), 2);
    assert_eq!(
        env.accounts_store()
            .load_logins(NOW)
            .unwrap()
            .0
            .shared
            .latest(source)
            .unwrap()
            .intent
            .request,
        request
    );
    approval.live.store(false, Ordering::SeqCst);
    assert!(run_with_provider(&env, &host, &approval, provider.clone()).is_err());
    assert_eq!(approval.recoveries.load(Ordering::SeqCst), 2);
    approval.live.store(true, Ordering::SeqCst);
    approval.generation.store(2, Ordering::SeqCst);
    assert_eq!(
        run_with_provider(&env, &host, &approval, provider.clone()),
        Ok(true)
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_ne!(
        env.accounts_store()
            .load_logins(NOW)
            .unwrap()
            .0
            .shared
            .latest(source)
            .unwrap()
            .intent
            .request,
        request
    );
    assert!(env.accounts_store().load(NOW).unwrap().0.active.is_none());
}

/// Unix only, where aterm links the headless opener (`cli::opener_link`) and reads private
/// staged evidence (`shared_provider::read_staged_bytes`); off Unix
/// `automatic_shared_adoption_refuses_where_aterm_links_no_opener` holds the refusal.
#[cfg(unix)]
#[test]
fn a_verified_but_unapproved_principal_requests_recovery_without_replaying_it() {
    let (_tmp, env, approval, mut host) = fixture(8);
    let grant = approval.prepare(&candidate(&approval).row).unwrap();
    let error = run(&env, &host, &approval).unwrap_err();
    assert!(
        error.contains("contradicts the approved identity"),
        "{error}"
    );
    assert_eq!(approval.recoveries.load(Ordering::SeqCst), 1);
    assert_eq!(host.calls.load(Ordering::SeqCst), 1);
    assert!(!host.roster.exists());
    let before = env.accounts_store().load_logins(NOW).unwrap().0;
    let attempt = before.shared.latest(grant.source).unwrap().clone();
    assert_eq!(attempt.outcome, accounts::shared::Outcome::Pending);
    assert!(run(&env, &host, &approval).is_err());
    assert_eq!(approval.recoveries.load(Ordering::SeqCst), 2);
    assert_eq!(host.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        env.accounts_store()
            .load_logins(NOW)
            .unwrap()
            .0
            .shared
            .latest(grant.source),
        Some(&attempt)
    );
    approval.generation.store(2, Ordering::SeqCst);
    host.mode = 0;
    assert_eq!(run(&env, &host, &approval), Ok(true));
    assert_eq!(host.calls.load(Ordering::SeqCst), 2);
    let after = env.accounts_store().load_logins(NOW).unwrap().0;
    assert_ne!(
        after.shared.latest(grant.source).unwrap().intent.request,
        attempt.intent.request
    );
    assert!(env.accounts_store().load(NOW).unwrap().0.active.is_none());
}
fn run(env: &Env, host: &Host, approval: &Arc<Approval>) -> Result<bool, String> {
    run_with_provider(env, host, approval, Arc::new(shared_provider::ClaudeOauth))
}
fn run_with_provider(
    env: &Env,
    host: &Host,
    approval: &Arc<Approval>,
    provider: Arc<dyn shared_provider::Provider>,
) -> Result<bool, String> {
    let store = env.accounts_store();
    let mut candidate = candidate(approval);
    candidate.provider = provider;
    let roster = env.accounts_roster().unwrap();
    let signin = adoption_sign_in(&store, &roster, &candidate);
    super::super::shared_adoption::adopt(env, host, &store, &signin, &roster, candidate)
        .map_err(|failure| failure.to_string())
}

/// The started sign-in [`super::super::shared_adoption::adopt`] takes for `candidate`, naming
/// the row the upkeep's adoption names ([`crate::harness::accounts_sync::shared_renewal::adoption_label`]).
fn adoption_sign_in(
    store: &accounts::Store,
    roster: &Path,
    candidate: &Candidate,
) -> crate::harness::sign_in_ladder::Begun {
    let text = accounts::roster_text(roster)
        .ok()
        .flatten()
        .unwrap_or_default();
    let raw = Roster::parse_for(candidate.cli, &text)
        .unwrap_or_else(|_| Roster::parse_for(candidate.cli, "").unwrap());
    let label = crate::harness::accounts_sync::shared_renewal::adoption_label(candidate, &raw);
    crate::harness::sign_in_ladder::begun_fixture(
        store.signin_now().unwrap().unwrap(),
        candidate.cli,
        &label,
    )
}

#[cfg(unix)]
struct FirstResult {
    calls: AtomicUsize,
    unknown: bool,
    suspend: Option<Arc<Approval>>,
}
#[cfg(unix)]
impl shared_provider::Provider for FirstResult {
    fn execute(
        &self,
        _: &shared_provider::Context<'_>,
        _: &accounts::Row,
        _: &Path,
        _: &RenewBrowser,
    ) -> Result<(), String> {
        panic!("the typed attempt result must be used")
    }
    fn execute_attempt(
        &self,
        context: &shared_provider::Context<'_>,
        row: &accounts::Row,
        staging: &Path,
        browser: &RenewBrowser,
    ) -> shared_provider::Execution {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            if let Some(approval) = &self.suspend {
                approval.live.store(false, Ordering::SeqCst);
            }
            if self.unknown {
                shared_provider::Execution::Unknown("inert unknown result".into())
            } else {
                shared_provider::Execution::NotStarted("inert pre-spawn refusal".into())
            }
        } else {
            shared_provider::ClaudeOauth.execute_attempt(context, row, staging, browser)
        }
    }
    fn observe(
        &self,
        context: &shared_provider::Context<'_>,
        row: &accounts::Row,
        staging: &Path,
    ) -> Result<Option<Box<dyn shared_provider::LoginProof>>, shared_provider::ObservationError>
    {
        shared_provider::ClaudeOauth.observe(context, row, staging)
    }
}

/// Unix only, where aterm links the headless opener (`cli::opener_link`) and reads private
/// staged evidence (`shared_provider::read_staged_bytes`); off Unix
/// `automatic_shared_adoption_refuses_where_aterm_links_no_opener` holds the refusal.
#[cfg(unix)]
#[test]
fn a_proved_unstarted_attempt_resumes_its_original_request_but_an_unknown_one_does_not() {
    for (unknown, suspended) in [(false, false), (true, false), (false, true)] {
        let (_tmp, env, approval, host) = fixture(0);
        let grant = approval.prepare(&candidate(&approval).row).unwrap();
        let first = Arc::new(FirstResult {
            calls: AtomicUsize::new(0),
            unknown,
            suspend: suspended.then(|| approval.clone()),
        });
        assert!(run_with_provider(&env, &host, &approval, first.clone()).is_err());
        let before = env.accounts_store().load_logins(NOW).unwrap().0;
        let attempt = before.shared.latest(grant.source).unwrap();
        let request = attempt.intent.request;
        let staging = attempt.intent.staging.clone();
        assert_eq!(host.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            attempt.outcome,
            if unknown {
                accounts::shared::Outcome::Pending
            } else {
                accounts::shared::Outcome::NotStarted
            }
        );
        // Recreate the provider as after a process restart. Only persisted state
        // can distinguish a safe no-effect retry from an ambiguous provider call.
        approval.live.store(true, Ordering::SeqCst);
        let resumed = Arc::new(FirstResult {
            calls: AtomicUsize::new(1),
            unknown,
            suspend: None,
        });
        assert_eq!(
            run_with_provider(&env, &host, &approval, resumed.clone()),
            Ok(!unknown)
        );
        let after = env.accounts_store().load_logins(NOW).unwrap().0;
        let attempt = after.shared.latest(grant.source).unwrap();
        assert_eq!(attempt.intent.request, request);
        assert_eq!(attempt.intent.staging, staging);
        assert_eq!(host.calls.load(Ordering::SeqCst), usize::from(!unknown));
        assert_eq!(
            resumed.calls.load(Ordering::SeqCst),
            1 + usize::from(!unknown)
        );
        assert!(
            after
                .shared
                .check_preparation(grant.source, grant.generation, grant.expected_principal)
                .is_err()
        );
    }
}

/// Unix only, where aterm links the headless opener (`cli::opener_link`) and reads private
/// staged evidence (`shared_provider::read_staged_bytes`); off Unix
/// `automatic_shared_adoption_refuses_where_aterm_links_no_opener` holds the refusal.
#[cfg(unix)]
#[test]
fn first_shared_adoption_publishes_only_after_identity_and_retains_one_use_floor() {
    let (_tmp, env, approval, host) = fixture(0);
    assert_eq!(run(&env, &host, &approval), Ok(true));
    let roster = accounts::Roster::load(&host.roster).unwrap();
    assert_eq!(roster.rows.len(), 1);
    assert_eq!(roster.rows[0].label, "shared-approved");
    let (state, _) = env.accounts_store().load(NOW + 1).unwrap();
    assert!(state.active.is_none() && state.pending_pin.is_none());
    assert_eq!(
        state.lease_verdict("shared-approved", NOW + 1),
        Err(accounts::WhyNot::PreparationOnly)
    );
    assert!(
        accounts::launch_row(&roster, &state, &accounts::enrolled(&roster), NOW + 1).is_none(),
        "preparation alone must not grant paid dispatch"
    );
    let retained = accounts::State::from_json(&state.to_json(), NOW + 1).unwrap();
    assert_eq!(
        retained.lease_verdict("shared-approved", NOW + 1),
        Err(accounts::WhyNot::PreparationOnly)
    );
    let mut encoded: Value = aterm_json::from_str(&state.to_json()).unwrap();
    assert_eq!(
        encoded["schema"].as_u64(),
        Some(accounts::PREPARED_STATE_SCHEMA)
    );
    encoded
        .as_object_mut()
        .unwrap()
        .insert("schema".into(), Value::from(accounts::SCHEMA));
    assert!(
        accounts::State::from_json(&encoded.to_string(), NOW + 1).is_err(),
        "a legacy-schema writer must not erase execution holds"
    );
    assert_eq!(run(&env, &host, &approval), Ok(false));
    assert_eq!(host.calls.load(Ordering::SeqCst), 1);
    assert!(host.online_calls.load(Ordering::SeqCst) > 0);
}

#[cfg(unix)]
#[test]
fn first_adoption_extends_a_large_roster_after_verified_provider_work() {
    let (_tmp, env, approval, mut host) = fixture(0);
    let rows: Vec<_> = (0..96)
        .map(|i| accounts::Row {
            label: format!("local-{i}"),
            email: Some(format!("local-{i}@example.test")),
            dir: Some(
                env.state
                    .join(format!("local-{i}"))
                    .to_str()
                    .unwrap()
                    .into(),
            ),
            ..Default::default()
        })
        .collect();
    let text = accounts::append_rows_for(Cli::Claude, "", &rows).unwrap();
    fs::write(&host.roster, &text).unwrap();
    host.before = Some(text.into_bytes());
    assert_eq!(run(&env, &host, &approval), Ok(true));
    assert_eq!(host.calls.load(Ordering::SeqCst), 1);
    let stored = accounts::Roster::load(&host.roster).unwrap();
    assert_eq!(stored.rows.len(), 97);
    assert!(
        stored
            .rows
            .iter()
            .any(|row| row.expected_identity_email() == Some(EMAIL))
    );
}

#[test]
fn automatic_first_adoption_refuses_before_consumption_without_a_headless_opener() {
    let (_tmp, env, approval, host) = fixture(7);
    assert!(run(&env, &host, &approval).is_err());
    assert_eq!(host.calls.load(Ordering::SeqCst), 0);
    assert!(
        env.accounts_store()
            .load_logins(NOW)
            .unwrap()
            .0
            .shared
            .is_empty()
    );
    assert!(!host.roster.exists());
}

#[test]
fn automatic_first_adoption_does_not_substitute_a_headless_profile_for_a_required_one() {
    for pending in [false, true] {
        // Pending reconciliation first runs a headless sign-in (`cli::opener_link`), which runs
        // only on Unix; `automatic_shared_adoption_refuses_where_aterm_links_no_opener` holds
        // the off-Unix refusal.
        if pending && !cfg!(unix) {
            continue;
        }
        let (_tmp, env, approval, host) = fixture(u8::from(pending));
        let store = env.accounts_store();
        if pending {
            assert_eq!(run(&env, &host, &approval), Ok(false));
            let grant = approval.prepare(&candidate(&approval).row).unwrap();
            let logins = store.load_logins(NOW).unwrap().0;
            let attempt = logins.shared.latest(grant.source).unwrap();
            write_login(Path::new(&attempt.intent.staging), EMAIL);
        }
        let mut named = candidate(&approval);
        named.row.browser = Some("required-profile".into());
        let signin = adoption_sign_in(&store, &env.accounts_roster().unwrap(), &named);
        assert!(
            super::super::shared_adoption::adopt(
                &env,
                &host,
                &store,
                &signin,
                &env.accounts_roster().unwrap(),
                named,
            )
            .is_err()
        );
        assert_eq!(host.calls.load(Ordering::SeqCst), usize::from(pending));
        assert_eq!(
            store.load_logins(NOW).unwrap().0.shared.is_empty(),
            !pending
        );
        assert!(!host.roster.exists());
    }
}

/// Unix only, where aterm links the headless opener (`cli::opener_link`) and reads private
/// staged evidence (`shared_provider::read_staged_bytes`); off Unix
/// `automatic_shared_adoption_refuses_where_aterm_links_no_opener` holds the refusal.
#[cfg(unix)]
#[test]
fn first_shared_adoption_preserves_an_existing_default_login_and_selection() {
    let (_tmp, env, approval, mut host) = fixture(0);
    let old = accounts::Row {
        label: "owner".into(),
        email: Some("local@example.test".into()),
        ..Default::default()
    };
    let text = accounts::append_rows_for(Cli::Claude, "", std::slice::from_ref(&old)).unwrap();
    fs::write(&host.roster, &text).unwrap();
    host.before = Some(text.into_bytes());
    env.accounts_store()
        .transact(NOW, &mut None, |state| {
            state.active = Some("owner".into());
            Ok(Vec::new())
        })
        .unwrap();
    assert_eq!(run(&env, &host, &approval), Ok(true));
    let roster = accounts::Roster::load(&host.roster).unwrap();
    assert_eq!(roster.rows.len(), 2);
    assert_eq!(roster.row("owner"), Some(&old));
    assert!(
        env.accounts_store()
            .load_logins(NOW + 1)
            .unwrap()
            .0
            .get("shared-approved")
            .unwrap()
            .was
            .is_empty(),
        "a new shared row never owned the existing default home"
    );
    assert_eq!(
        env.accounts_store()
            .load(NOW + 1)
            .unwrap()
            .0
            .active
            .as_deref(),
        Some("owner")
    );
}

/// Unix only, where aterm links the headless opener (`cli::opener_link`) and reads private
/// staged evidence (`shared_provider::read_staged_bytes`); off Unix
/// `automatic_shared_adoption_refuses_where_aterm_links_no_opener` holds the refusal.
#[cfg(unix)]
#[test]
fn interrupted_first_adoption_reconciles_same_staging_without_another_provider_effect() {
    let (_tmp, env, approval, host) = fixture(1);
    assert_eq!(run(&env, &host, &approval), Ok(false));
    assert!(!host.roster.exists());
    let logins = env.accounts_store().load_logins(NOW).unwrap().0;
    let grant = approval.prepare(&candidate(&approval).row).unwrap();
    let pending = logins.shared.latest(grant.source).unwrap();
    let request = pending.intent.request;
    write_login(Path::new(&pending.intent.staging), EMAIL);
    assert_eq!(run(&env, &host, &approval), Ok(true));
    assert_eq!(host.calls.load(Ordering::SeqCst), 1);
    let logins = env.accounts_store().load_logins(NOW + 1).unwrap().0;
    assert_eq!(
        logins.shared.latest(grant.source).unwrap().intent.request,
        request
    );
}

#[cfg(unix)]
struct ChangeDuringProof {
    cancel: Option<crate::harness::account_work::Lifetime>,
    approval: Arc<Approval>,
    config: PathBuf,
    off: Option<&'static str>,
    checked: Arc<AtomicUsize>,
}

#[cfg(unix)]
struct ChangedProof {
    inner: Box<dyn shared_provider::LoginProof>,
    change: ChangeDuringProof,
}

#[cfg(unix)]
impl shared_provider::LoginProof for ChangedProof {
    fn identity(&self) -> &str {
        self.inner.identity()
    }
    fn principal(&self) -> [u8; 32] {
        self.inner.principal()
    }
    fn subscription(&self) -> Option<&str> {
        self.inner.subscription()
    }
    fn check_current(&self) -> Result<(), String> {
        self.inner.check_current()?;
        self.change.checked.fetch_add(1, Ordering::SeqCst);
        if let Some(work) = &self.change.cancel {
            drop(work.pause().unwrap());
        } else if let Some(off) = self.change.off {
            fs::write(&self.change.config, off).unwrap();
        } else {
            self.change.approval.live.store(false, Ordering::SeqCst);
        }
        Ok(())
    }
}

#[cfg(unix)]
impl shared_provider::Provider for ChangeDuringProof {
    fn execute(
        &self,
        context: &shared_provider::Context<'_>,
        row: &accounts::Row,
        staging: &Path,
        browser: &RenewBrowser,
    ) -> Result<(), String> {
        shared_provider::Provider::execute(
            &shared_provider::ClaudeOauth,
            context,
            row,
            staging,
            browser,
        )
    }
    fn observe(
        &self,
        context: &shared_provider::Context<'_>,
        row: &accounts::Row,
        staging: &Path,
    ) -> Result<Option<Box<dyn shared_provider::LoginProof>>, shared_provider::ObservationError>
    {
        Ok(shared_provider::Provider::observe(
            &shared_provider::ClaudeOauth,
            context,
            row,
            staging,
        )?
        .map(|inner| {
            Box::new(ChangedProof {
                inner,
                change: Self {
                    cancel: self.cancel.clone(),
                    approval: self.approval.clone(),
                    config: self.config.clone(),
                    off: self.off,
                    checked: self.checked.clone(),
                },
            }) as Box<dyn shared_provider::LoginProof>
        }))
    }
}

/// Unix only, where aterm links the headless opener (`cli::opener_link`) and reads private
/// staged evidence (`shared_provider::read_staged_bytes`); off Unix
/// `automatic_shared_adoption_refuses_where_aterm_links_no_opener` holds the refusal.
#[cfg(unix)]
#[test]
fn publication_rechecks_root_and_local_off_after_the_final_credential_read() {
    for off in [
        None,
        Some("[harness]\naccounts = 'off'\n"),
        Some("[harness]\nlogins = 'off'\n"),
        Some("[harness]\nenabled = false\n"),
    ] {
        let (_tmp, env, approval, host) = fixture(0);
        let checked = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(ChangeDuringProof {
            cancel: None,
            approval: approval.clone(),
            config: config_beside_for_test(&host.roster),
            off,
            checked: checked.clone(),
        });
        assert!(run_with_provider(&env, &host, &approval, provider).is_err());
        assert_eq!(
            checked.load(Ordering::SeqCst),
            1,
            "the final proof read reached the change"
        );
        assert_eq!(host.calls.load(Ordering::SeqCst), 1);
        assert!(
            !host.roster.exists(),
            "no row was published after authority changed"
        );
    }
}

/// Unix only, where aterm links the headless opener (`cli::opener_link`) and reads private
/// staged evidence (`shared_provider::read_staged_bytes`); off Unix
/// `automatic_shared_adoption_refuses_where_aterm_links_no_opener` holds the refusal.
#[cfg(unix)]
#[test]
fn a_new_root_generation_can_replace_its_own_unlisted_reservation() {
    let (_tmp, env, approval, mut host) = fixture(1);
    assert_eq!(run(&env, &host, &approval), Ok(false));
    approval.generation.store(2, Ordering::SeqCst);
    host.mode = 0;
    assert_eq!(run(&env, &host, &approval), Ok(true));
    assert_eq!(host.calls.load(Ordering::SeqCst), 2);
}

/// Unix only, where aterm links the headless opener (`cli::opener_link`); off Unix
/// `automatic_shared_adoption_refuses_where_aterm_links_no_opener` holds the refusal.
#[cfg(unix)]
#[test]
fn local_invalidation_never_resumes_the_older_pending_result() {
    let (_tmp, env, approval, mut host) = fixture(5);
    assert!(run(&env, &host, &approval).is_err());
    host.mode = 0;
    assert!(run(&env, &host, &approval).is_err());
    assert_eq!(host.calls.load(Ordering::SeqCst), 1);
    assert!(!host.roster.exists());
}

/// Off Unix aterm makes no symbolic-link opener (`cli::opener_link`), so automatic Claude
/// shared adoption refuses before staging, provider calls, or decision consumption.
#[cfg(not(unix))]
#[test]
fn automatic_shared_adoption_refuses_where_aterm_links_no_opener() {
    let (_tmp, env, approval, host) = fixture(0);
    let err = run(&env, &host, &approval).expect_err("no headless opener off Unix");
    assert_eq!(
        err,
        opener_link(&env.accounts_store(), &host)
            .unwrap_err()
            .to_string()
    );
    assert_eq!(host.calls.load(Ordering::SeqCst), 0);
    assert!(!env.accounts_store().logins_dir().exists());
    assert!(
        env.accounts_store()
            .load_logins(NOW)
            .unwrap()
            .0
            .shared
            .is_empty()
    );
    assert!(!host.roster.exists());
}

#[test]
fn an_existing_provider_identity_is_not_silently_duplicated_for_a_login_alias() {
    let (_tmp, env, approval, host) = fixture(0);
    let row = accounts::Row {
        label: "owner".into(),
        email: Some("alias@example.test".into()),
        identity_email: Some(EMAIL.into()),
        ..Default::default()
    };
    fs::write(
        &host.roster,
        accounts::append_rows_for(Cli::Claude, "", &[row]).unwrap(),
    )
    .unwrap();
    assert!(run(&env, &host, &approval).is_err());
    assert_eq!(host.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn revoked_wrong_identity_and_disabled_adoptions_never_publish_a_row() {
    for mode in [2, 3, 4, 5, 6] {
        let (_tmp, env, approval, host) = fixture(mode);
        assert!(run(&env, &host, &approval).is_err(), "mode {mode}");
        assert!(!host.roster.exists(), "mode {mode}");
    }
    let (_tmp, env, approval, host) = fixture(0);
    approval.live.store(false, Ordering::SeqCst);
    assert!(run(&env, &host, &approval).is_err());
    assert_eq!(host.calls.load(Ordering::SeqCst), 0);
}

/// Unix only, where aterm links the headless opener a first adoption needs before it records an
/// attempt to recover (`opener_link`); off Unix it is refused before it consumes anything, as
/// `automatic_shared_adoption_refuses_where_aterm_links_no_opener` holds.
#[cfg(unix)]
#[path = "cli_shared_adoption_recovery_tests.rs"]
mod recovery;

/// Unix only, for [`recovery`]'s reason: each case starts from a recorded first attempt.
#[cfg(unix)]
#[path = "cli_shared_adoption_staging_tests.rs"]
mod staging;

#[cfg(unix)]
struct AdmissionProbe {
    approval: Arc<Approval>,
    mode: u8,
    seen: std::sync::Mutex<Vec<crate::harness::login_drive::RecoveryAdmission>>,
}

#[cfg(unix)]
impl shared_provider::Provider for AdmissionProbe {
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
    ) -> Result<Option<Box<dyn shared_provider::LoginProof>>, shared_provider::ObservationError>
    {
        let admission = context
            .admission
            .expect("root adoption needs live native admission");
        admission.current(None).unwrap();
        admission
            .current(Some(profile::principal_id("provider-uuid").unwrap()))
            .unwrap();
        assert!(admission.current(Some([99; 32])).is_err());
        let count = {
            let mut seen = self.seen.lock().unwrap();
            seen.push(admission.clone());
            seen.len()
        };
        match self.mode {
            1 => self.approval.live.store(false, Ordering::SeqCst),
            2 => fs::write(context.env.accounts_roster().unwrap(), "# changed roster\n").unwrap(),
            3 => {
                context
                    .store
                    .transact_logins(NOW, &mut None, |state, logins| {
                        accounts::forget_account(state, logins, &row.label);
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            4 => self.approval.generation.store(2, Ordering::SeqCst),
            5 => fs::remove_dir_all(staging).unwrap(),
            _ => {}
        }
        if self.mode != 0 {
            assert!(admission.current(None).is_err());
            return Err(shared_provider::ObservationError::Unavailable(
                "admission changed".into(),
            ));
        }
        if count < 3 {
            return Err(shared_provider::ObservationError::Unavailable(
                "retain the exact attempt".into(),
            ));
        }
        shared_provider::ClaudeOauth.observe(context, row, staging)
    }
}

#[cfg(unix)]
#[test]
fn shared_native_admission_survives_observation_restart_but_not_reissue_or_publication() {
    for reissue in [false, true] {
        let (_tmp, env, approval, host) = fixture(0);
        let provider = Arc::new(AdmissionProbe {
            approval: approval.clone(),
            mode: 0,
            seen: Default::default(),
        });
        assert!(run_with_provider(&env, &host, &approval, provider.clone()).is_err());
        assert!(run_with_provider(&env, &host, &approval, provider.clone()).is_err());
        {
            let seen = provider.seen.lock().unwrap();
            assert_eq!(seen[0].key(), seen[1].key());
            seen[0].current(None).unwrap();
            if reissue {
                approval.generation.store(2, Ordering::SeqCst);
                assert!(seen[0].current(None).is_err());
            }
        }
        assert_eq!(
            run_with_provider(&env, &host, &approval, provider.clone()),
            Ok(true)
        );
        let seen = provider.seen.lock().unwrap();
        assert_eq!(seen[0].key() != seen[2].key(), reissue);
        assert!(
            seen.iter()
                .all(|admission| admission.current(None).is_err())
        );
        assert_eq!(
            host.calls.load(Ordering::SeqCst),
            if reissue { 2 } else { 1 }
        );
        assert_eq!(approval.recoveries.load(Ordering::SeqCst), 0);
    }
}

#[cfg(unix)]
#[test]
fn shared_native_admission_rechecks_root_roster_and_binding_during_observation() {
    for mode in 1..=5 {
        let (_tmp, env, approval, host) = fixture(0);
        let provider = Arc::new(AdmissionProbe {
            approval: approval.clone(),
            mode,
            seen: Default::default(),
        });
        assert!(run_with_provider(&env, &host, &approval, provider.clone()).is_err());
        assert_eq!(host.calls.load(Ordering::SeqCst), 1);
        assert_eq!(approval.recoveries.load(Ordering::SeqCst), 0);
        assert!(provider.seen.lock().unwrap()[0].current(None).is_err());
    }
}

#[cfg(unix)]
#[test]
fn cancelled_shared_adoption_retains_pending_custody_without_publishing_or_recovery() {
    for mode in [0, 11, 12, 13, 14, 15] {
        let (_tmp, env, approval, mut host) = fixture(mode);
        let lifetime = crate::harness::account_work::Lifetime::default();
        host.work = Some(lifetime.enter().unwrap());
        host.cancel = Some(lifetime.clone());
        let old = accounts::Row {
            label: "owner".into(),
            email: Some("local@example.test".into()),
            ..Default::default()
        };
        let text = accounts::append_rows_for(Cli::Claude, "", &[old]).unwrap();
        aterm_uds::atomic_file::write(&host.roster, text.as_bytes(), 0o600).unwrap();
        host.before = Some(text.into_bytes());
        let home = env.home.as_ref().unwrap();
        aterm_types::fs_restricted::ensure_private_dir(home).unwrap();
        write_login(home, "local@example.test");
        let original_profile = fs::read(home.join(".claude.json")).unwrap();
        let original_credential = fs::read(home.join(".credentials.json")).unwrap();
        let store = env.accounts_store();
        store
            .transact(NOW, &mut None, |state| {
                state.active = Some("owner".into());
                Ok(Vec::new())
            })
            .unwrap();
        let grant = approval.prepare(&candidate(&approval).row).unwrap();
        let checked = Arc::new(AtomicUsize::new(0));
        let provider: Arc<dyn shared_provider::Provider> = if mode == 14 {
            Arc::new(ChangeDuringProof {
                cancel: Some(lifetime.clone()),
                approval: approval.clone(),
                config: config_beside_for_test(&host.roster),
                off: None,
                checked: checked.clone(),
            })
        } else {
            Arc::new(shared_provider::ClaudeOauth)
        };
        if mode == 0 {
            drop(lifetime.pause().unwrap());
        }
        if mode == 15 {
            *approval.cancel_on_current.lock().unwrap() = Some(lifetime.clone());
        }
        assert!(run_with_provider(&env, &host, &approval, provider).is_err());
        assert!(host.work_cancelled());
        assert_eq!(
            host.calls.load(Ordering::SeqCst),
            usize::from(mode != 0 && mode != 15)
        );
        assert_eq!(
            host.status_calls.load(Ordering::SeqCst),
            usize::from((12..=14).contains(&mode))
        );
        assert_eq!(checked.load(Ordering::SeqCst), usize::from(mode == 14));
        assert_eq!(fs::read(&host.roster).ok(), host.before);
        assert_eq!(
            fs::read(home.join(".claude.json")).unwrap(),
            original_profile
        );
        assert_eq!(
            fs::read(home.join(".credentials.json")).unwrap(),
            original_credential
        );
        let (state, _) = store.load(NOW + 1).unwrap();
        assert_eq!(state.active.as_deref(), Some("owner"));
        let (logins, _) = store.load_logins(NOW + 1).unwrap();
        if mode == 0 || mode == 15 {
            assert!(logins.shared.latest(grant.source).is_none());
            if mode == 15 {
                assert_eq!(
                    fs::read_dir(store.logins_dir()).unwrap().count(),
                    0,
                    "known pre-begin cancellation left unretained staging"
                );
            }
        } else {
            let pending = logins.shared.latest(grant.source).unwrap();
            assert_eq!(pending.outcome, accounts::shared::Outcome::Pending);
            assert!(Path::new(&pending.intent.staging).exists());
        }
        assert_eq!(
            approval.recoveries.load(Ordering::SeqCst),
            0,
            "cancellation alone cannot request replacement authority"
        );
    }
}

#[cfg(unix)]
#[test]
fn typed_unavailable_issuer_keeps_the_consumed_shared_attempt_pending_without_replay() {
    let (_tmp, env, approval, mut host) = fixture(16);
    let lifetime = crate::harness::account_work::Lifetime::default();
    host.work = lifetime.enter();
    let store = env.accounts_store();
    let roster = env.accounts_roster().unwrap();
    let candidate = candidate(&approval);
    let grant = approval.prepare(&candidate.row).unwrap();
    let signin = adoption_sign_in(&store, &roster, &candidate);
    let failure =
        super::super::shared_adoption::adopt(&env, &host, &store, &signin, &roster, candidate)
            .unwrap_err();
    assert!(failure.is_deferred());
    assert!(
        !host.work_cancelled(),
        "remote refusal must not close the requester's App epoch"
    );
    assert_eq!(host.calls.load(Ordering::SeqCst), 1);
    assert_eq!(host.status_calls.load(Ordering::SeqCst), 0);
    assert_eq!(host.online_calls.load(Ordering::SeqCst), 0);
    assert!(!roster.exists());
    let pending = store
        .load_logins(NOW + 1)
        .unwrap()
        .0
        .shared
        .latest(grant.source)
        .cloned()
        .unwrap();
    assert_eq!(pending.outcome, accounts::shared::Outcome::Pending);
    assert!(Path::new(&pending.intent.staging).exists());
    assert_eq!(approval.recoveries.load(Ordering::SeqCst), 0);
}
