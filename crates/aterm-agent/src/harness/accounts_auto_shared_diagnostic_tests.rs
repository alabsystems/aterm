// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::cli::{Renewal, RenewalExit, RenewalFailure};
use crate::harness::open_url::{Met, MetWord};

const PRIVATE_FAILURE: &str =
    "capture failed: https://private.invalid/code-secret /private/auth-secret";

#[derive(Clone)]
enum Reply {
    Exit(i32, Option<Met>),
    Capture(Option<Met>),
}

struct DiagnosticHost {
    provider: Arc<Provider>,
    reply: Mutex<Reply>,
}

impl AccountsHost for DiagnosticHost {
    fn current_exe(&self) -> Option<PathBuf> {
        self.provider.current_exe()
    }
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("no default login read")
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

    fn renew_outcome(
        &self,
        staging: &str,
        email: &str,
        browser: &RenewBrowser,
    ) -> Result<RenewalExit, RenewalFailure> {
        assert_eq!(email, EMAIL);
        assert!(matches!(browser, RenewBrowser::Remote { opener: Some(_) }));
        self.provider.calls.fetch_add(1, Ordering::SeqCst);
        let reply = self.reply.lock().unwrap().clone();
        if matches!(reply, Reply::Exit(0, _)) {
            fs::write(Path::new(staging).join(".claude.json"), format!(r#"{{"oauthAccount":{{"emailAddress":"{EMAIL}","accountUuid":"approved-provider"}}}}"#)).unwrap();
        }
        if self.provider.stop.load(Ordering::SeqCst) {
            self.provider.approval.live.store(false, Ordering::SeqCst);
        }
        let effect = self.provider.flow_effect.lock().unwrap().take();
        if let Some(effect) = effect {
            effect();
        }
        match reply {
            Reply::Exit(code, met) => Ok(RenewalExit { code, met }),
            Reply::Capture(met) => Err(RenewalFailure::new(PRIVATE_FAILURE, met)),
        }
    }

    fn status_in_home(&self, home: &str) -> Result<Captured, String> {
        self.provider.status_in_home(home)
    }
    fn claude_principal_in_home(&self, home: &str) -> Option<[u8; 32]> {
        self.provider.claude_principal_in_home(home)
    }
    fn claude_identity_in_home(&self, home: &str, email: &str) -> profile::IdentityObservation {
        self.provider.claude_identity_in_home(home, email)
    }
}

struct Fixture {
    bed: Bed,
    host: Arc<DiagnosticHost>,
    guard: Arc<Mutex<AutoRenewGuard>>,
    lines: Arc<Mutex<Vec<String>>>,
}

impl Fixture {
    fn new(reply: Reply) -> Self {
        let bed = Bed::new(None);
        let host = Arc::new(DiagnosticHost {
            provider: bed.provider.clone(),
            reply: Mutex::new(reply),
        });
        Self {
            bed,
            host,
            guard: Arc::new(Mutex::new(AutoRenewGuard::default())),
            lines: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn upkeep(&self, spawn: &SpawnRenewal<'_>) {
        let factory = self.bed.factory();
        let lines = self.lines.clone();
        let fx = UpkeepEffects {
            scope: None,
            fetch: &|_, _| panic!("no source fetch"),
            spawn_renewal: spawn,
            sync_cooldown: &Mutex::new(None),
            rediscovery: &Mutex::new(Rediscovery::default()),
            renew_guard: &self.guard,
            find_browser: &|_| false,
            vault_sync: |_, _scope| crate::harness::sign_in_ladder::Vault::NotMember,
            default_check: |_, _scope| crate::harness::cli::LoginCheck::CouldNotRun(String::new()),
            spawn_vault: &inline,
            mailbox: &|_, _, _| (crate::harness::sign_in_ladder::Mailbox::Ready, None),
            log: Arc::new(move |line| lines.lock().unwrap().push(line.to_owned())),
            run_attempt: Some(Arc::new(|_, _| panic!("no unguarded fallback"))),
            shared: shared_renewal::Effects {
                authority: Some(&factory),
                checkpoint_authority: Some(&factory),
                host: Some(self.host.clone()),
                discover: None,
            },
        };
        upkeep(
            &self.bed.home,
            &self.bed.state,
            (&self.bed.roster, NOW, UpkeepStep::Renew),
            &fx,
        );
    }

    fn activity(&self, clock: Instant) -> Vec<LoginActivity> {
        login_activity_of(
            &self.guard.lock().unwrap(),
            &self.bed.store,
            Cli::Claude,
            NOW + 1,
            clock,
        )
    }

    fn records(&self) -> accounts::AccountRecords {
        self.bed
            .store
            .read_account_records(Some(&self.bed.roster), NOW + 1)
            .unwrap()
    }

    fn background(&self) -> crate::harness::cli::shared_renewal::BackgroundRenewal {
        let records = self.records();
        let row = records.roster.row("shared").unwrap();
        let signin = crate::harness::sign_in_ladder::begun_fixture(
            self.bed.store.signin_now().unwrap().unwrap(),
            Cli::Claude,
            "shared",
        );
        let env = quiet_env(
            &self.bed.home,
            &self.bed.state,
            &config_beside(&self.bed.roster),
            NOW,
        );
        crate::harness::cli::shared_renewal::background(
            &env,
            self.host.as_ref(),
            &self.bed.store,
            &signin,
            accounts::RenewalPolicy {
                roster_path: &self.bed.roster,
                roster: &records.raw_roster,
                row,
                logins: &records.logins,
            },
            &RenewBrowser::Remote {
                opener: self.host.current_exe(),
            },
            self.bed.approval.as_ref(),
        )
    }
}

#[derive(Debug, PartialEq, Eq)]
struct FailedPolicy {
    generation: u64,
    outcome: accounts::shared::Outcome,
    ordinary_binding: bool,
    ordinary_attempt: bool,
    staging_retained: bool,
    retries: bool,
    peer_ends_wait: bool,
    parked: bool,
    bot_checked: bool,
}

fn failed_policy(fixture: &Fixture) -> FailedPolicy {
    let records = fixture.records();
    let attempt = records.logins.shared.latest(SOURCE).unwrap();
    let clock = Instant::now();
    let mut guard = fixture.guard.lock().unwrap();
    let retries = guard.ladder.retry_at("shared", clock).is_some();
    guard.ladder.moved(clock);
    FailedPolicy {
        generation: attempt.intent.generation,
        outcome: attempt.outcome,
        ordinary_binding: records.logins.get("shared").is_some(),
        ordinary_attempt: records
            .logins
            .get("shared")
            .and_then(|binding| binding.sign_in_attempt.as_ref())
            .is_some(),
        staging_retained: Path::new(&attempt.intent.staging).exists(),
        retries,
        peer_ends_wait: guard.ladder.retry_at("shared", clock).is_none(),
        parked: guard.parked.contains_key("shared"),
        bot_checked: guard.bot_checked.contains_key("shared"),
    }
}

#[test]
fn shared_guard_keeps_typed_provider_facts_even_when_publication_is_deferred() {
    for capture in [false, true] {
        for stop in [false, true] {
            let met = Met::new(MetWord::BotCheck, Some("claude.ai"));
            let fixture = Fixture::new(if capture {
                Reply::Capture(Some(met.clone()))
            } else {
                Reply::Exit(1, Some(met.clone()))
            });
            let before = fixture.records();
            let prior_binding = before.logins.get("shared").unwrap();
            fixture.bed.provider.stop.store(stop, Ordering::SeqCst);
            let report = fixture.background();
            assert_eq!(
                report.renewal,
                if stop {
                    Renewal::Deferred
                } else {
                    Renewal::BotCheck
                }
            );
            let observation = report
                .observation
                .expect("a called provider has an observation");
            assert_eq!(
                observation.exit_code(),
                if capture { None } else { Some(1) }
            );
            assert_eq!(observation.capture_failed(), capture);
            assert_eq!(observation.met(), Some(&met));
            let words = observation.words();
            assert!(words.contains(&met.words()));
            assert!(!words.contains("private.invalid") && !words.contains("auth-secret"));
            assert_eq!(fixture.bed.provider.calls.load(Ordering::SeqCst), 1);
            let records = fixture.records();
            assert_eq!(
                records.logins.shared.latest(SOURCE).unwrap().outcome,
                accounts::shared::Outcome::Unknown
            );
            // A shared preparation advances its local generation fence even
            // without publishing credentials. Its bookkeeping row is expected.
            let binding = records.logins.get("shared").unwrap();
            assert!(binding.renewal_generation > prior_binding.renewal_generation);
            assert_eq!(
                records
                    .logins
                    .shared
                    .latest(SOURCE)
                    .unwrap()
                    .intent
                    .before_generation,
                Some(binding.renewal_generation)
            );
            assert_eq!(binding.dir, prior_binding.dir);
            assert_eq!(binding.renewed_at, prior_binding.renewed_at);
            assert_eq!(binding.last_success_generation, 0);
            assert_eq!(binding.proved_by, None);
            assert_eq!(binding.proved_email, None);
            assert_eq!(binding.proved_dir, None);
            assert_eq!(binding.sign_in_attempt, prior_binding.sign_in_attempt);
            assert_eq!(
                accounts::sign_in_tasks(&records.raw_roster, &records.logins),
                if stop {
                    Vec::new()
                } else {
                    vec![("shared".into(), accounts::SignInAction::Login)]
                }
            );
            let again = fixture.background();
            assert_eq!(again.renewal, Renewal::Deferred);
            assert!(
                again.observation.is_none(),
                "a preflight refusal cannot reuse prior facts"
            );
            assert_eq!(fixture.bed.provider.calls.load(Ordering::SeqCst), 1);
        }
    }
}

#[test]
fn a_host_cannot_put_a_raw_url_or_path_in_a_retained_browser_outcome() {
    let untrusted = Met {
        word: MetWord::BotCheck,
        host: Some(PRIVATE_FAILURE.into()),
    };
    let fixture = Fixture::new(Reply::Exit(1, Some(untrusted)));
    let report = fixture.background();
    let observation = report.observation.unwrap();
    assert_eq!(observation.exit_code(), Some(1));
    assert_eq!(observation.met(), None);
    assert!(!observation.words().contains("private.invalid"));
    assert!(!observation.words().contains("auth-secret"));
    assert!(!format!("{observation:?}").contains("private.invalid"));
    assert_eq!(fixture.bed.provider.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn shared_failure_diagnostics_do_not_change_custody_or_retry_policy() {
    for capture in [false, true] {
        let mut baseline = None;
        for met in [
            None,
            Some(Met::new(MetWord::BrowserControl, None)),
            Some(Met::new(MetWord::BotCheck, Some("claude.ai"))),
        ] {
            let reply = if capture {
                Reply::Capture(met.clone())
            } else {
                Reply::Exit(1, met.clone())
            };
            let fixture = Fixture::new(reply);
            fixture.upkeep(&inline);
            assert_eq!(fixture.bed.provider.calls.load(Ordering::SeqCst), 1);
            let rows = fixture.activity(Instant::now());
            let row = rows.iter().find(|row| row.label == "shared").unwrap();
            assert!(!row.active);
            let diagnostic = row
                .diagnostic
                .as_ref()
                .expect("actual shared provider failure is visible");
            assert!(diagnostic.completed_at > 0);
            let words = diagnostic.words();
            assert!(!words.contains("private.invalid") && !words.contains("auth-secret"));
            let debug = format!("{diagnostic:?}");
            assert!(!debug.contains("private.invalid") && !debug.contains("auth-secret"));
            if let Some(met) = &met {
                assert!(words.contains(&met.words()), "{words}");
            }
            let records = fixture.records();
            let recorded_row = records.roster.row("shared").unwrap();
            assert!(diagnostic.matches_row(recorded_row));
            let mut changed_row = recorded_row.clone();
            changed_row.email = Some("different@example.test".into());
            assert!(!diagnostic.matches_row(&changed_row));
            assert!(
                login_activity_of(
                    &fixture.guard.lock().unwrap(),
                    &fixture.bed.store,
                    Cli::Codex,
                    NOW + 1,
                    Instant::now()
                )
                .is_empty()
            );
            assert!(
                fixture
                    .activity(Instant::now() + Duration::from_secs(901))
                    .iter()
                    .all(|row| row.diagnostic.is_none())
            );
            fixture.upkeep(&inline);
            assert_eq!(
                fixture.bed.provider.calls.load(Ordering::SeqCst),
                1,
                "consumed shared preparation cannot replay"
            );
            let log = fixture.lines.lock().unwrap().join("\n");
            assert!(!log.contains("private.invalid") && !log.contains("auth-secret"));
            let policy = failed_policy(&fixture);
            assert_eq!(policy.outcome, accounts::shared::Outcome::Unknown);
            assert!(policy.staging_retained && policy.retries && policy.peer_ends_wait);
            assert!(!policy.parked && !policy.bot_checked && !policy.ordinary_attempt);
            if let Some(baseline) = &baseline {
                assert_eq!(&policy, baseline);
            } else {
                baseline = Some(policy);
            }
        }
    }
}

#[test]
fn a_new_shared_attempt_that_never_starts_does_not_reuse_the_previous_diagnostic() {
    let fixture = Fixture::new(Reply::Exit(
        1,
        Some(Met::new(MetWord::BrowserControl, None)),
    ));
    fixture.upkeep(&inline);
    assert!(
        fixture
            .activity(Instant::now())
            .iter()
            .any(|row| row.diagnostic.is_some())
    );
    fixture.bed.approval.generation.store(3, Ordering::SeqCst);
    let queued = Arc::new(Mutex::new(Vec::new()));
    let capture = queued.clone();
    fixture.upkeep(&move |work| {
        capture.lock().unwrap().push(work);
        Ok(())
    });
    fixture.bed.approval.live.store(false, Ordering::SeqCst);
    let work = queued
        .lock()
        .unwrap()
        .pop()
        .expect("new preparation is queued");
    work();
    assert_eq!(fixture.bed.provider.calls.load(Ordering::SeqCst), 1);
    assert!(
        fixture
            .activity(Instant::now())
            .iter()
            .all(|row| row.diagnostic.is_none())
    );
    let records = fixture.records();
    assert_eq!(
        records
            .logins
            .shared
            .latest(SOURCE)
            .unwrap()
            .intent
            .generation,
        2
    );
    assert_eq!(
        records.logins.shared.latest(SOURCE).unwrap().outcome,
        accounts::shared::Outcome::Unknown
    );
}

#[test]
fn successful_shared_publication_retires_prior_failure_evidence() {
    let fixture = Fixture::new(Reply::Exit(
        1,
        Some(Met::new(MetWord::BrowserControl, None)),
    ));
    fixture.upkeep(&inline);
    fixture.bed.approval.generation.store(3, Ordering::SeqCst);
    *fixture.host.reply.lock().unwrap() =
        Reply::Exit(0, Some(Met::new(MetWord::BotCheck, Some("claude.ai"))));
    fixture.upkeep(&inline);
    assert_eq!(fixture.bed.provider.calls.load(Ordering::SeqCst), 2);
    let records = fixture.records();
    assert_eq!(
        records
            .logins
            .shared
            .latest(SOURCE)
            .unwrap()
            .intent
            .generation,
        3
    );
    assert!(matches!(
        records.logins.shared.latest(SOURCE).unwrap().outcome,
        accounts::shared::Outcome::Published { .. }
    ));
    assert!(
        fixture
            .activity(Instant::now())
            .iter()
            .all(|row| row.diagnostic.is_none())
    );
}

#[test]
fn a_replaced_active_token_cannot_attach_or_clear_a_new_attempts_diagnostic() {
    let fixture = Fixture::new(Reply::Exit(1, None));
    let records = fixture.records();
    let row = records.roster.row("shared").unwrap();
    let permit = AutoRenewPermit::claim(&fixture.guard, "shared", Instant::now(), true).unwrap();
    permit.attribute(
        &fixture.bed.store,
        Cli::Claude,
        "shared",
        NOW,
        &records.logins,
    );
    let first = LoginActivityGuard::begin(
        &fixture.guard,
        &fixture.bed.store,
        Cli::Claude,
        "shared",
        Some("mailbox"),
    );
    let current = LoginActivityGuard::begin(
        &fixture.guard,
        &fixture.bed.store,
        Cli::Claude,
        "shared",
        Some("profile"),
    );
    let observation = crate::harness::cli::RenewalObservation::from_outcome(&Ok(RenewalExit {
        code: 1,
        met: Some(Met::new(MetWord::BrowserControl, None)),
    }));
    first.complete(row, NOW + 1, observation.clone());
    assert!(
        fixture
            .activity(Instant::now())
            .iter()
            .all(|activity| activity.diagnostic.is_none())
    );
    current.complete(row, NOW + 1, observation.clone());
    let activity = fixture.activity(Instant::now());
    assert!(
        activity
            .iter()
            .any(|activity| activity.diagnostic.is_some())
    );
    drop(first);
    assert!(
        fixture
            .activity(Instant::now())
            .iter()
            .any(|activity| activity.active && activity.route.as_deref() == Some("profile"))
    );

    let next = LoginActivityGuard::begin(
        &fixture.guard,
        &fixture.bed.store,
        Cli::Claude,
        "shared",
        Some("mailbox"),
    );
    current.complete(row, NOW + 2, observation);
    assert!(
        fixture
            .activity(Instant::now())
            .iter()
            .all(|activity| activity.diagnostic.is_none())
    );
    drop(current);
    assert!(
        fixture
            .activity(Instant::now())
            .iter()
            .any(|activity| activity.active && activity.route.as_deref() == Some("mailbox"))
    );
    drop(next);
    drop(permit);
}
