// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::{
    align::Captured,
    cli::{
        AccountsHost, RenewBrowser,
        shared_renewal::{Authority, Grant},
    },
    profile,
};
use super::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

const NOW: i64 = 1_793_000_000;
const EMAIL: &str = "owner@example.test";
const SOURCE: accounts::shared::Source = accounts::shared::Source {
    network: [1; 32],
    account: [2; 32],
};

struct Approval {
    live: AtomicBool,
    generation: AtomicU64,
    browser: Option<String>,
}

impl Authority for Approval {
    fn prepare(&self, _: &Row) -> Result<Grant, String> {
        if !self.live.load(Ordering::SeqCst) {
            return Err("root preparation stopped".into());
        }
        let generation = self.generation.load(Ordering::SeqCst);
        Ok(Grant {
            source: SOURCE,
            decision: [generation as u8; 32],
            generation,
            configuration: [4; 32],
            identity: EMAIL.into(),
            expected_principal: profile::principal_id("approved-provider"),
            required_browser_profile: self.browser.clone(),
        })
    }
    fn current(&self, grant: &Grant, row: &Row) -> Result<(), String> {
        if self.prepare(row)? == *grant {
            Ok(())
        } else {
            Err("root head changed".into())
        }
    }
}

type Mutation = Box<dyn FnOnce() + Send>;

struct Provider {
    approval: Arc<Approval>,
    calls: AtomicUsize,
    stop: AtomicBool,
    wrong_email: AtomicBool,
    fail: AtomicBool,
    status_error: AtomicBool,
    flow_effect: Mutex<Option<Mutation>>,
    status_effect: Mutex<Option<Mutation>>,
}

impl AccountsHost for Provider {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("no default login probe")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("no ordinary login")
    }
    fn now(&self) -> i64 {
        NOW + 1
    }
    fn renew_flow(
        &self,
        staging: &str,
        email: &str,
        browser: &RenewBrowser,
    ) -> Result<i32, String> {
        assert_eq!(email, EMAIL);
        assert!(
            matches!(browser, RenewBrowser::Remote { .. }),
            "only the unattended route"
        );
        self.calls.fetch_add(1, Ordering::SeqCst);
        let profile_email = if self.wrong_email.load(Ordering::SeqCst) {
            "foreign@example.test"
        } else {
            EMAIL
        };
        fs::write(Path::new(staging).join(".claude.json"),
            format!(r#"{{"oauthAccount":{{"emailAddress":"{profile_email}","accountUuid":"approved-provider"}}}}"#)).unwrap();
        if self.stop.load(Ordering::SeqCst) {
            self.approval.live.store(false, Ordering::SeqCst);
        }
        if let Some(effect) = self.flow_effect.lock().unwrap().take() {
            effect();
        }
        Ok(i32::from(self.fail.load(Ordering::SeqCst)))
    }
    fn status_in_home(&self, _: &str) -> Result<Captured, String> {
        if let Some(effect) = self.status_effect.lock().unwrap().take() {
            effect();
        }
        if self.status_error.load(Ordering::SeqCst) {
            return Err("fixture status unavailable".into());
        }
        Ok(Captured { code: Some(0), stdout: format!(r#"{{"loggedIn":true,"authMethod":"claude.ai","email":"{EMAIL}","subscriptionType":"max"}}"#).into_bytes() })
    }
    fn claude_principal_in_home(&self, staging: &str) -> Option<[u8; 32]> {
        profile::principal_in_home(Path::new(staging))
    }

    fn claude_identity_in_home(&self, staging: &str, email: &str) -> profile::IdentityObservation {
        profile::identity_in_home(Path::new(staging), email)
    }
}

struct Bed {
    _root: aterm_tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    roster: PathBuf,
    store: accounts::Store,
    approval: Arc<Approval>,
    provider: Arc<Provider>,
}

fn inline(work: Box<dyn FnOnce() + Send>) -> io::Result<()> {
    work();
    Ok(())
}

impl Bed {
    fn new(browser: Option<&str>) -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let state = root.path().join("state");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&state).unwrap();
        let roster = state.join(accounts::ACCOUNTS_FILE);
        let store = accounts::Store::at_for(&state, Cli::Claude, &home);
        let row = Row {
            label: "shared".into(),
            email: Some(EMAIL.into()),
            dir: Some(home.join("prior-login").to_str().unwrap().into()),
            ..Row::default()
        };
        fs::write(
            &roster,
            accounts::append_rows_for(Cli::Claude, "", &[row]).unwrap(),
        )
        .unwrap();
        let staging = store.claim_staging_dir("shared").unwrap();
        store
            .transact_logins(NOW - 10, &mut None, |_, logins| {
                let token = logins.shared.begin(
                    accounts::shared::Intent {
                        source: SOURCE,
                        decision: [1; 32],
                        generation: 1,
                        configuration: [4; 32],
                        request: [5; 16],
                        label: "shared".into(),
                        identity: EMAIL.into(),
                        staging: staging.to_str().unwrap().into(),
                        before_generation: None,
                        begun_at: NOW - 10,
                    },
                    profile::principal_id("approved-provider"),
                )?;
                logins
                    .shared
                    .finish(token, accounts::shared::Outcome::Unknown, None, NOW - 10)?;
                Ok(Vec::new())
            })
            .unwrap();
        let approval = Arc::new(Approval {
            live: AtomicBool::new(true),
            generation: AtomicU64::new(2),
            browser: browser.map(str::to_owned),
        });
        let provider = Arc::new(Provider {
            approval: Arc::clone(&approval),
            calls: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
            wrong_email: AtomicBool::new(false),
            fail: AtomicBool::new(false),
            status_error: AtomicBool::new(false),
            flow_effect: Mutex::new(None),
            status_effect: Mutex::new(None),
        });
        Self {
            _root: root,
            home,
            state,
            roster,
            store,
            approval,
            provider,
        }
    }

    fn upkeep(
        &self,
        factory: Option<&shared_renewal::Factory>,
        spawn: &SpawnRenewal,
        mailbox: bool,
    ) -> Upkept {
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        self.upkeep_observed(factory, spawn, mailbox, &guard, Arc::new(|_| {}))
    }

    fn upkeep_observed(
        &self,
        factory: Option<&shared_renewal::Factory>,
        spawn: &SpawnRenewal,
        mailbox: bool,
        guard: &Arc<Mutex<AutoRenewGuard>>,
        log: SignInLog,
    ) -> Upkept {
        let fx = UpkeepEffects {
            fetch: &|_, _| panic!("no source fetch"),
            spawn_renewal: spawn,
            quota_refresh: &|_, _, _, _| {},
            sync_cooldown: &Mutex::new(None),
            rediscovery: &Mutex::new(Rediscovery::default()),
            renew_guard: guard,
            find_browser: &|_| false,
            vault_sync: |_| super::super::sign_in_ladder::Vault::NotMember,
            default_check: |_| super::super::cli::LoginCheck::CouldNotRun(String::new()),
            spawn_vault: &inline,
            mailbox: &move |_, _, _| {
                if mailbox {
                    (super::super::sign_in_ladder::Mailbox::Ready, None)
                } else {
                    (super::super::sign_in_ladder::Mailbox::None, None)
                }
            },
            log,
            run_attempt: Some(Arc::new(|_, _| panic!("no unguarded fallback"))),
            shared: shared_renewal::Effects {
                authority: factory,
                host: Some(self.provider.clone()),
            },
        };
        upkeep(
            &self.home,
            &self.state,
            (&self.roster, NOW, UpkeepStep::Renew),
            &fx,
        )
    }

    fn factory(
        &self,
    ) -> impl Fn(
        &Path,
        accounts::shared::Source,
        &Row,
    ) -> Result<shared_renewal::CheckedAuthority, String>
    + Send
    + Sync
    + 'static {
        let approval = Arc::clone(&self.approval);
        move |_, source, row| {
            assert_eq!(source, SOURCE);
            approval.prepare(row)?;
            Ok(approval.clone())
        }
    }
}

#[test]
fn a_shared_policy_stop_is_deferred_without_a_failure_or_retry_wait() {
    for during_provider in [false, true] {
        let bed = Bed::new(None);
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let lines = Arc::new(Mutex::new(Vec::new()));
        let logged = Arc::clone(&lines);
        let jobs = Arc::new(Mutex::new(Vec::new()));
        let queued = Arc::clone(&jobs);
        let queue = move |work| {
            queued.lock().unwrap().push(work);
            Ok(())
        };
        bed.provider.stop.store(during_provider, Ordering::SeqCst);
        bed.upkeep_observed(
            Some(&bed.factory()),
            &queue,
            true,
            &guard,
            Arc::new(move |line| logged.lock().unwrap().push(line.to_owned())),
        );
        if !during_provider {
            bed.approval.live.store(false, Ordering::SeqCst);
        }
        jobs.lock().unwrap().pop().unwrap()();
        assert_eq!(
            bed.provider.calls.load(Ordering::SeqCst),
            usize::from(during_provider)
        );
        let (logins, _) = bed.store.load_logins(NOW + 1).unwrap();
        let last = logins.shared.latest(SOURCE).unwrap();
        assert_eq!(last.outcome, accounts::shared::Outcome::Unknown);
        assert_eq!(last.intent.generation, if during_provider { 2 } else { 1 });
        assert!(
            logins
                .get("shared")
                .is_none_or(|binding| binding.dir.is_none())
        );
        assert!(
            guard
                .lock()
                .unwrap()
                .ladder
                .retry_at("shared", Instant::now())
                .is_none(),
            "a policy stop is not a failed provider login: {during_provider}"
        );
        assert!(
            !lines
                .lock()
                .unwrap()
                .iter()
                .any(|line| line.contains("failed")),
            "policy refusal must not be reported as a provider failure"
        );
    }
}

#[test]
fn upkeep_renews_a_pinned_row_once_per_new_root_generation_without_a_terminal() {
    let bed = Bed::new(None);
    assert!(matches!(
        bed.upkeep(Some(&bed.factory()), &inline, true),
        Upkept::Unconfigured { renewal: true, .. }
    ));
    assert_eq!(bed.provider.calls.load(Ordering::SeqCst), 1);
    let (logins, _) = bed.store.load_logins(NOW + 1).unwrap();
    let binding = logins.get("shared").unwrap();
    assert!(
        binding
            .dir
            .as_ref()
            .is_some_and(|dir| accounts::login_made(Path::new(dir)))
    );
    let attempt = logins.shared.latest(SOURCE).unwrap();
    assert_eq!(attempt.intent.generation, 2);
    assert_eq!(
        attempt.outcome,
        accounts::shared::Outcome::Published {
            generation: binding.renewal_generation
        }
    );
    let records = bed
        .store
        .read_account_records(Some(&bed.roster), NOW + 1)
        .unwrap();
    assert_eq!(records.state.accounts["shared"].auth, accounts::Auth::Ok);
    let mut roster = records.roster;
    roster.rows.push(Row {
        label: "walled".into(),
        ..Row::default()
    });
    let choices = accounts::candidates(
        &roster,
        &records.state,
        &accounts::enrolled(&roster),
        Some("walled"),
        &accounts::Point::Wall {
            window: String::new(),
            at: None,
        },
        NOW + 1,
    );
    assert!(matches!(choices.first(), Some((label, Ok(_))) if label == "shared"));
    // Make this row need renewal again; process-local cooldown is recreated on
    // every call, so only retained grant consumption can prevent another flow.
    bed.store
        .transact_logins(NOW + 2, &mut None, |state, _| {
            Ok(accounts::note_auth(
                state,
                "shared",
                accounts::Auth::SignedOut,
                NOW + 2,
            ))
        })
        .unwrap();
    bed.upkeep(Some(&bed.factory()), &inline, true);
    assert_eq!(bed.provider.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn shared_upkeep_refuses_missing_authority_routes_required_profiles_and_local_off_switches() {
    for case in 0..6 {
        let bed = Bed::new((case == 3).then_some("required-profile"));
        if case == 1 {
            bed.approval.live.store(false, Ordering::SeqCst);
        }
        if case == 4 {
            fs::write(config_beside(&bed.roster), "[harness]\nlogins = \"off\"\n").unwrap();
        }
        if case == 5 {
            fs::write(
                config_beside(&bed.roster),
                "[harness]\naccounts = \"off\"\n",
            )
            .unwrap();
        }
        bed.upkeep(
            (case != 0).then_some(&bed.factory() as &shared_renewal::Factory),
            &inline,
            case != 2,
        );
        assert_eq!(bed.provider.calls.load(Ordering::SeqCst), 0, "case {case}");
        let (logins, _) = bed.store.load_logins(NOW).unwrap();
        assert_eq!(logins.shared.latest(SOURCE).unwrap().intent.generation, 1);
        assert!(
            logins
                .get("shared")
                .is_none_or(|binding| binding.dir.is_none())
        );
    }
}

#[test]
fn a_stop_during_provider_work_consumes_the_attempt_but_publishes_no_login() {
    let bed = Bed::new(None);
    bed.provider.stop.store(true, Ordering::SeqCst);
    bed.upkeep(Some(&bed.factory()), &inline, true);
    assert_eq!(bed.provider.calls.load(Ordering::SeqCst), 1);
    let (logins, _) = bed.store.load_logins(NOW + 1).unwrap();
    assert!(
        logins
            .get("shared")
            .is_none_or(|binding| binding.dir.is_none())
    );
    assert_eq!(
        logins.shared.latest(SOURCE).unwrap().outcome,
        accounts::shared::Outcome::Unknown
    );
    // Even a later locally ready observation cannot reuse the consumed grant.
    bed.approval.live.store(true, Ordering::SeqCst);
    bed.provider.stop.store(false, Ordering::SeqCst);
    bed.upkeep(Some(&bed.factory()), &inline, true);
    assert_eq!(bed.provider.calls.load(Ordering::SeqCst), 1);
    bed.approval.generation.store(3, Ordering::SeqCst);
    bed.upkeep(Some(&bed.factory()), &inline, true);
    assert_eq!(bed.provider.calls.load(Ordering::SeqCst), 2);
    assert!(
        bed.store
            .load_logins(NOW + 1)
            .unwrap()
            .0
            .get("shared")
            .is_some_and(|binding| binding.dir.is_some())
    );
}

#[test]
fn queued_shared_work_rechecks_the_root_and_the_local_login_switch() {
    for stopped in [false, true] {
        let bed = Bed::new(None);
        let jobs = Arc::new(Mutex::new(Vec::new()));
        let queued = Arc::clone(&jobs);
        let queue = move |work| {
            queued.lock().unwrap().push(work);
            Ok(())
        };
        bed.upkeep(Some(&bed.factory()), &queue, true);
        assert_eq!(jobs.lock().unwrap().len(), 1);
        if stopped {
            bed.approval.live.store(false, Ordering::SeqCst);
        } else {
            fs::write(config_beside(&bed.roster), "[harness]\nlogins = \"off\"\n").unwrap();
        }
        let job = jobs.lock().unwrap().pop().unwrap();
        job();
        assert_eq!(bed.provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            bed.store
                .load_logins(NOW)
                .unwrap()
                .0
                .shared
                .latest(SOURCE)
                .unwrap()
                .intent
                .generation,
            1
        );
    }
}

#[test]
fn automatic_shared_renewal_refuses_a_profile_that_contradicts_the_successful_status() {
    let bed = Bed::new(None);
    bed.provider.wrong_email.store(true, Ordering::SeqCst);
    let (guard, lines) = observed_upkeep(&bed);
    assert!(
        guard
            .lock()
            .unwrap()
            .ladder
            .retry_at("shared", Instant::now())
            .is_some()
    );
    assert!(lines.iter().any(|line| line.contains("failed")));
    assert_eq!(bed.provider.calls.load(Ordering::SeqCst), 1);
    let (logins, _) = bed.store.load_logins(NOW + 1).unwrap();
    assert!(
        logins
            .get("shared")
            .is_none_or(|binding| binding.dir.is_none())
    );
    assert_eq!(
        logins.shared.latest(SOURCE).unwrap().outcome,
        accounts::shared::Outcome::Unknown
    );
}

fn observed_upkeep(bed: &Bed) -> (Arc<Mutex<AutoRenewGuard>>, Vec<String>) {
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let lines = Arc::new(Mutex::new(Vec::new()));
    let logged = Arc::clone(&lines);
    bed.upkeep_observed(
        Some(&bed.factory()),
        &inline,
        true,
        &guard,
        Arc::new(move |line| logged.lock().unwrap().push(line.to_owned())),
    );
    let lines = lines.lock().unwrap().clone();
    (guard, lines)
}

#[test]
fn changed_shared_policy_during_provider_or_failed_status_is_deferred() {
    for during_status in [false, true] {
        for change in 0..5 {
            let bed = Bed::new(None);
            let effect: Mutation = match change {
                0 => {
                    let approval = Arc::clone(&bed.approval);
                    Box::new(move || approval.live.store(false, Ordering::SeqCst))
                }
                1 => {
                    let path = bed.roster.clone();
                    Box::new(move || {
                        let mut rows =
                            Roster::parse_for(Cli::Claude, &fs::read_to_string(&path).unwrap())
                                .unwrap();
                        rows.rows[0].email = Some("replacement@example.test".into());
                        fs::write(
                            &path,
                            accounts::append_rows_for(Cli::Claude, "", &rows.rows).unwrap(),
                        )
                        .unwrap();
                    })
                }
                2 => {
                    let state = bed.state.clone();
                    Box::new(move || {
                        accounts::Store::at(&state)
                            .transact_logins(NOW + 1, &mut None, |_, logins| {
                                accounts::note_proof(logins, "shared", accounts::Proof::Unchecked);
                                Ok(Vec::new())
                            })
                            .unwrap();
                    })
                }
                3 => {
                    let path = bed.roster.clone();
                    Box::new(move || fs::write(&path, "[[unreadable\n").unwrap())
                }
                4 => {
                    let path = config_beside(&bed.roster);
                    Box::new(move || fs::write(&path, "[harness]\nlogins = \"off\"\n").unwrap())
                }
                _ => unreachable!(),
            };
            if during_status {
                *bed.provider.status_effect.lock().unwrap() = Some(effect);
                bed.provider.status_error.store(true, Ordering::SeqCst);
            } else {
                *bed.provider.flow_effect.lock().unwrap() = Some(effect);
            }
            let (guard, lines) = observed_upkeep(&bed);
            assert_eq!(bed.provider.calls.load(Ordering::SeqCst), 1);
            assert!(
                guard
                    .lock()
                    .unwrap()
                    .ladder
                    .retry_at("shared", Instant::now())
                    .is_none(),
                "change {change}, during status {during_status}"
            );
            assert!(
                !lines.iter().any(|line| line.contains("failed")),
                "{lines:?}"
            );
            let (logins, _) = bed.store.load_logins(NOW + 1).unwrap();
            assert_eq!(logins.shared.latest(SOURCE).unwrap().intent.generation, 2);
            assert_eq!(
                logins.shared.latest(SOURCE).unwrap().outcome,
                accounts::shared::Outcome::Unknown
            );
            assert!(
                logins
                    .get("shared")
                    .is_none_or(|binding| binding.dir.is_none())
            );
        }
    }
}
fn observe_disposition(before: bool, after: bool, provider: bool, replay: bool) -> [i64; 4] {
    let bed = Bed::new(None);
    bed.provider.stop.store(!after, Ordering::SeqCst);
    bed.provider.fail.store(!provider, Ordering::SeqCst);
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let lines = Arc::new(Mutex::new(Vec::new()));
    let logged = Arc::clone(&lines);
    let jobs = Arc::new(Mutex::new(Vec::new()));
    let queued = Arc::clone(&jobs);
    let queue = move |work| {
        queued.lock().unwrap().push(work);
        Ok(())
    };
    bed.upkeep_observed(
        Some(&bed.factory()),
        &queue,
        true,
        &guard,
        Arc::new(move |line| logged.lock().unwrap().push(line.to_owned())),
    );
    bed.approval.live.store(before, Ordering::SeqCst);
    jobs.lock().unwrap().pop().unwrap()();
    let failed = guard
        .lock()
        .unwrap()
        .ladder
        .retry_at("shared", Instant::now())
        .is_some();
    assert_eq!(
        failed,
        lines
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains("failed"))
    );
    let (logins, _) = bed.store.load_logins(NOW + 1).unwrap();
    let latest = logins.shared.latest(SOURCE).unwrap();
    let consumed = latest.intent.generation == 2;
    let bound = logins
        .get("shared")
        .is_some_and(|binding| binding.dir.is_some());
    if !bound {
        assert_eq!(latest.outcome, accounts::shared::Outcome::Unknown);
    }
    if replay {
        bed.approval.live.store(true, Ordering::SeqCst);
        bed.provider.stop.store(false, Ordering::SeqCst);
        bed.provider.fail.store(false, Ordering::SeqCst);
        // A fresh local scheduler cannot reuse an already consumed root grant.
        bed.upkeep(Some(&bed.factory()), &inline, true);
        let current = bed.store.load_logins(NOW + 1).unwrap().0;
        assert_eq!(
            current.shared.latest(SOURCE).unwrap().intent.generation,
            latest.intent.generation
        );
        assert_eq!(
            current.shared.latest(SOURCE).unwrap().outcome,
            latest.outcome
        );
        assert_eq!(
            current.get("shared").and_then(|b| b.dir.as_ref()),
            logins.get("shared").and_then(|b| b.dir.as_ref())
        );
    }
    [
        i64::from(consumed),
        bed.provider.calls.load(Ordering::SeqCst) as i64,
        i64::from(bound),
        i64::from(failed),
    ]
}

pub(crate) fn disposition_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "StopBefore" => {
            next.insert("before", 0);
        }
        "StopDuring" => {
            next.insert("after", 0);
        }
        "ProviderFail" => {
            next.insert("provider", 0);
        }
        "Run" | "Replay" => {
            let [consumed, entered, bound, failure] = observe_disposition(
                state["before"] == 1,
                state["after"] == 1,
                state["provider"] == 1,
                action == "Replay",
            );
            next.insert("phase", if action == "Run" { 1 } else { 2 });
            next.insert("consumed", consumed);
            next.insert("entered", entered);
            next.insert("bound", bound);
            next.insert("failure", failure);
        }
        _ => panic!("unknown action {action}"),
    }
    next
}

#[test]
fn shared_dispositions_conform_and_failure_on_policy_refusal_is_detected() {
    let model = aterm_models_harness::shared_disposition_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 0);
    let states = aterm_spec::interp::reachable_states(&model);
    for state in &states {
        for action in &model.actions {
            if model.action_enabled(action.name, state) {
                let observed = disposition_transition(state, action.name);
                assert!(
                    model.successors(action.name, state).contains(&observed),
                    "{} {state:?} -> {observed:?}",
                    action.name
                );
            }
        }
    }
    let stopped = states
        .iter()
        .find(|state| {
            state["phase"] == 0
                && state["before"] == 0
                && state["after"] == 1
                && state["provider"] == 1
        })
        .unwrap();
    let mut legacy = disposition_transition(stopped, "Run");
    let mut ladder = super::super::sign_in_ladder::Ladder::default();
    let now = Instant::now();
    ladder.failed("shared", "legacy", now);
    legacy.insert(
        "failure",
        i64::from(ladder.retry_at("shared", now).is_some()),
    );
    assert!(
        !model.successors("Run", stopped).contains(&legacy),
        "the former unconditional shared-failure backoff must fail conformance"
    );
}
