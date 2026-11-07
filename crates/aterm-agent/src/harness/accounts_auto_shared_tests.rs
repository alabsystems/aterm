// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[path = "accounts_auto_shared_identity_tests.rs"]
mod identity_tests;

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

#[path = "accounts_auto_shared_diagnostic_tests.rs"]
mod diagnostic_tests;
const EMAIL: &str = "owner@example.test";
const SOURCE: accounts::shared::Source = accounts::shared::Source {
    network: [1; 32],
    account: [2; 32],
};

fn native_preparation_id(key: impl Into<String>) -> super::super::native_keys::PreparationId {
    super::super::native_keys::PreparationId {
        key: key.into(),
        revision: [1; 32],
    }
}

#[test]
fn native_cancellation_after_preclaim_before_task_return_retires_the_retry_claim() {
    use super::super::native_keys::{self, PreparationId, PreparationTask};
    let bed = Bed::new(None);
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let lifetime = crate::harness::account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let paused = Arc::new(Mutex::new(None));
    let cancelled = paused.clone();
    let owner = lifetime.clone();
    let discovery = Box::new(
        move |_: &Path,
              _: &Path,
              _: &Path,
              admit: &mut dyn FnMut(&PreparationId) -> bool|
              -> Result<Option<PreparationTask>, String> {
            assert!(admit(&native_preparation_id("native:prestart")));
            *cancelled.lock().unwrap() = owner.pause();
            Err("synthetic discovery error after preclaim".to_owned())
        },
    );
    native_keys::with_test_discovery(discovery, || {
        let log: SignInLog = Arc::new(|_| {});
        assert!(!shared_renewal::prepare_native(
            &bed.home,
            &bed.state,
            &config_beside(&bed.roster),
            &guard,
            &|_| panic!("discovery returned no task"),
            &log,
            Some(&scope)
        ));
    });
    assert!(scope.cancelled());
    assert!(!guard.lock().unwrap().in_flight);
    assert!(
        !guard
            .lock()
            .unwrap()
            .last_attempt
            .contains_key("native:prestart")
    );
    drop(scope);
    let pause = paused.lock().unwrap().take().unwrap();
    assert!(
        pause
            .try_wait()
            .expect("preclaim retired without a worker")
            .resume()
            .is_ok()
    );
}

#[test]
fn native_admission_preserves_busy_candidates_and_skips_longer_guard_cooldowns() {
    use super::super::native_keys::{self, PreparationQueue, PreparationTask};
    let bed = Bed::new(None);
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let queue = Arc::new(Mutex::new(PreparationQueue::default()));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let clock = Arc::new(AtomicU64::new(0));
    let start = Instant::now();
    let key = |id: u8| format!("native:{id}");
    let discovery = {
        let queue = queue.clone();
        let calls = calls.clone();
        let clock = clock.clone();
        let guard = guard.clone();
        Box::new(
            move |home: &Path, _: &Path, _: &Path, admit: &mut dyn FnMut(&super::super::native_keys::PreparationId) -> bool| {
                let id = queue.lock().unwrap().pick_if(
                    home,
                    &[[1; 32], [2; 32]],
                    start + Duration::from_secs(clock.load(Ordering::SeqCst)),
                    |id| admit(&native_preparation_id(key(id[0]))),
                );
                Ok(id.map(|id| {
                    let calls = calls.clone();
                    let guard = guard.clone();
                    PreparationTask {
                        preparation: native_preparation_id(key(id[0])),
                        run: Box::new(move || {
                            assert!(guard.lock().unwrap().in_flight);
                            calls.lock().unwrap().push(id[0]);
                            Ok(())
                        }),
                    }
                }))
            },
        )
    };
    native_keys::with_test_discovery(discovery, || {
        let log: SignInLog = Arc::new(|why| panic!("{why}"));
        let run = || {
            shared_renewal::prepare_native(
                &bed.home,
                &bed.state,
                &config_beside(&bed.roster),
                &guard,
                &inline,
                &log,
                None,
            )
        };
        let held = AutoRenewPermit::claim(&guard, "other", start, false).unwrap();
        assert!(!run());
        assert!(calls.lock().unwrap().is_empty());
        drop(held);
        assert!(run());
        assert_eq!(*calls.lock().unwrap(), [1]);
        clock.store(60, Ordering::SeqCst);
        assert!(
            run(),
            "a mature queue key denied by the longer guard must not block the next key"
        );
        assert_eq!(*calls.lock().unwrap(), [1, 2]);
        assert!(guard.lock().unwrap().last_attempt.remove(&key(1)).is_some());
        assert!(run(), "denied key must retain eligibility in the queue");
        assert_eq!(*calls.lock().unwrap(), [1, 2, 1]);
        assert!(!guard.lock().unwrap().in_flight);
    });
}

#[test]
fn native_tasks_need_the_exact_admitted_identity_before_they_can_run() {
    use super::super::native_keys::{self, PreparationTask};
    let bed = Bed::new(None);
    for mismatch in 0..3 {
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let discovery = Box::new(
            move |_: &Path, _: &Path, _: &Path, admit: &mut dyn FnMut(&super::super::native_keys::PreparationId) -> bool| {
                if mismatch > 0 {
                    let mut id = native_preparation_id(if mismatch == 1 {"native:other"} else {"native:task"});
                    if mismatch == 2 { id.revision = [2; 32]; }
                    assert!(admit(&id));
                }
                Ok(Some(PreparationTask {
                    preparation: native_preparation_id("native:task"),
                    run: Box::new(|| panic!("unadmitted native task ran")),
                }))
            },
        );
        native_keys::with_test_discovery(discovery, || {
            let log: SignInLog = Arc::new(|_| {});
            assert!(!shared_renewal::prepare_native(
                &bed.home,
                &bed.state,
                &config_beside(&bed.roster),
                &guard,
                &inline,
                &log,
                None,
            ));
            assert!(!guard.lock().unwrap().in_flight);
        });
    }
}

/// What native preparation says is logged once per change, per guard: the same refusal on
/// every upkeep pass writes one line, another refusal writes its own, and the same one after a
/// pass that said nothing (nothing to prepare, or an admitted task) writes again. A fresh guard
/// (another process's upkeep) says its own first line. Mutants: logging each pass's refusal
/// (the code before) writes five lines at the first assertion; never forgetting the last line
/// on a pass that said nothing loses the repeats after a recovery; gating only the refusal arm
/// writes two lines for the unadmitted task.
#[test]
fn native_preparation_logs_a_lasting_refusal_once_per_change() {
    use super::super::native_keys::{self, PreparationId, PreparationTask};
    const EXCLUDED: &str = "Commons unavailable (control-excluded): x";
    const UNINITIALIZED: &str = "application table is not initialized";
    const NOTHING: usize = 2;
    const UNADMITTED: usize = 3;
    const ADMITTED: usize = 4;
    let bed = Bed::new(None);
    let mode = Arc::new(AtomicUsize::new(0));
    let ran = Arc::new(AtomicUsize::new(0));
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let discovery = {
        let mode = mode.clone();
        let ran = ran.clone();
        Box::new(
            move |_: &Path, _: &Path, _: &Path, admit: &mut dyn FnMut(&PreparationId) -> bool| {
                let ran = ran.clone();
                let task = |key: &str| PreparationTask {
                    preparation: native_preparation_id(key),
                    run: Box::new(move || {
                        ran.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }),
                };
                match mode.load(Ordering::SeqCst) {
                    0 => Err(EXCLUDED.to_owned()),
                    1 => Err(UNINITIALIZED.to_owned()),
                    NOTHING => Ok(None),
                    UNADMITTED => Ok(Some(task("native:unadmitted"))),
                    _ => {
                        assert!(admit(&native_preparation_id("native:admitted")));
                        Ok(Some(task("native:admitted")))
                    }
                }
            },
        )
    };
    native_keys::with_test_discovery(discovery, || {
        let log: SignInLog = {
            let lines = lines.clone();
            Arc::new(move |line| lines.lock().unwrap().push(line.to_owned()))
        };
        let pass = |guard: &Arc<Mutex<AutoRenewGuard>>, now: usize, times: usize| {
            mode.store(now, Ordering::SeqCst);
            for _ in 0..times {
                assert_eq!(
                    shared_renewal::prepare_native(
                        &bed.home,
                        &bed.state,
                        &config_beside(&bed.roster),
                        guard,
                        &inline,
                        &log,
                        None,
                    ),
                    now == ADMITTED
                );
            }
        };
        // A line is told apart by the refusal the discovery stand-in returned, never by the
        // wording around it; the unadmitted task's line carries neither refusal.
        let causes = || -> Vec<&'static str> {
            lines
                .lock()
                .unwrap()
                .iter()
                .map(|line| {
                    [EXCLUDED, UNINITIALIZED]
                        .into_iter()
                        .find(|why| line.ends_with(why))
                        .unwrap_or("unadmitted")
                })
                .collect()
        };
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        pass(&guard, 0, 5);
        assert_eq!(causes(), [EXCLUDED]);
        pass(&guard, 1, 2);
        pass(&guard, NOTHING, 1);
        pass(&guard, 1, 1);
        assert_eq!(
            causes(),
            [EXCLUDED, UNINITIALIZED, UNINITIALIZED],
            "a refusal that returns after a pass that said nothing is said again"
        );
        pass(&guard, UNADMITTED, 2);
        pass(&guard, ADMITTED, 1);
        assert_eq!(ran.load(Ordering::SeqCst), 1);
        pass(&guard, UNADMITTED, 1);
        assert_eq!(
            causes()[3..],
            ["unadmitted", "unadmitted"],
            "an unadmitted task is said once, and again after an admitted one"
        );
        {
            let lines = lines.lock().unwrap();
            assert_eq!(
                lines[3], lines[4],
                "the unadmitted task says one line each time"
            );
        }
        let other = Arc::new(Mutex::new(AutoRenewGuard::default()));
        pass(&other, 0, 2);
        pass(&guard, 0, 1);
        pass(&guard, 0, 1);
        assert_eq!(
            causes()[5..],
            [EXCLUDED, EXCLUDED],
            "each guard says its own first line, and neither repeats it"
        );
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    });
}

struct NativeRevisionFixture {
    bed: Bed,
    guard: Arc<Mutex<AutoRenewGuard>>,
    revision: Arc<AtomicU64>,
    clock: Arc<AtomicU64>,
    calls: Arc<AtomicUsize>,
    future: Arc<AtomicBool>,
    attempted: bool,
    clock_held: bool,
    original: Option<Instant>,
}

impl NativeRevisionFixture {
    fn run(&self) {
        let log: SignInLog = Arc::new(|why| panic!("{why}"));
        shared_renewal::prepare_native(
            &self.bed.home,
            &self.bed.state,
            &config_beside(&self.bed.roster),
            &self.guard,
            &inline,
            &log,
            None,
        );
    }

    fn step(&mut self, action: &str) {
        match action {
            "Replace" => self.revision.store(2, Ordering::SeqCst),
            "Blocked" => self.run(),
            "Try" => {
                self.attempted = true;
                self.clock_held = false;
                self.clock.store(1, Ordering::SeqCst);
                self.run();
            }
            "Repeat" => {
                self.clock_held = false;
                self.clock.store(61, Ordering::SeqCst);
                self.run();
            }
            "Future" => {
                self.clock_held = true;
                self.future.store(true, Ordering::SeqCst);
                self.run();
                self.future.store(false, Ordering::SeqCst);
            }
            "Person" | "Bot" | "Allow" => {
                let mut guard = self.guard.lock().unwrap();
                guard.in_flight = false;
                guard.parked.clear();
                guard.bot_checked.clear();
                if action == "Person" {
                    guard
                        .parked
                        .insert("native:revision".into(), Instant::now());
                }
                if action == "Bot" {
                    guard
                        .bot_checked
                        .insert("native:revision".into(), Instant::now());
                }
            }
            _ => panic!("unknown native revision action"),
        }
    }

    fn observe(&self) -> aterm_spec::interp::State {
        let guard = self.guard.lock().unwrap();
        let calls = self.calls.load(Ordering::SeqCst) - 1;
        if calls == 0 {
            assert_eq!(
                guard.last_attempt.get("native:revision"),
                self.original.as_ref()
            );
        }
        let held = if guard.in_flight {
            1
        } else if guard.parked.contains_key("native:revision") {
            2
        } else if guard.bot_checked.contains_key("native:revision") {
            3
        } else {
            0
        };
        aterm_spec::interp::State::from([
            (
                "revision",
                i64::try_from(self.revision.load(Ordering::SeqCst)).unwrap(),
            ),
            (
                "history",
                i64::from(guard.native_preparations["native:revision"][0]),
            ),
            ("held", held),
            ("calls", i64::try_from(calls).unwrap()),
            ("attempted", i64::from(self.attempted)),
            ("clock_held", i64::from(self.clock_held)),
        ])
    }
}

fn with_native_revision<T>(run: impl FnOnce(&mut NativeRevisionFixture) -> T) -> T {
    use super::super::native_keys::{self, PreparationId, PreparationQueue, PreparationTask};
    let mut fixture = NativeRevisionFixture {
        bed: Bed::new(None),
        guard: Arc::new(Mutex::new(AutoRenewGuard::default())),
        revision: Arc::new(AtomicU64::new(1)),
        clock: Arc::new(AtomicU64::new(0)),
        calls: Arc::new(AtomicUsize::new(0)),
        future: Arc::new(AtomicBool::new(false)),
        attempted: false,
        clock_held: false,
        original: None,
    };
    let revision = fixture.revision.clone();
    let clock = fixture.clock.clone();
    let calls = fixture.calls.clone();
    let future = fixture.future.clone();
    let queue = Mutex::new(PreparationQueue::default());
    let start = Instant::now();
    let discovery = Box::new(
        move |home: &Path, _: &Path, _: &Path, admit: &mut dyn FnMut(&PreparationId) -> bool| {
            let revision = [u8::try_from(revision.load(Ordering::SeqCst)).unwrap(); 32];
            let candidates = BTreeMap::from([([1; 32], revision)]);
            let selected = queue.lock().unwrap().pick_revisions_if(
                home,
                &candidates,
                if future.load(Ordering::SeqCst) {
                    start - Duration::from_secs(1)
                } else {
                    start + Duration::from_secs(clock.load(Ordering::SeqCst))
                },
                |_, revision| {
                    admit(&PreparationId {
                        key: "native:revision".into(),
                        revision,
                    })
                },
            );
            Ok(selected.map(|(_, revision)| {
                let calls = calls.clone();
                PreparationTask {
                    preparation: PreparationId {
                        key: "native:revision".into(),
                        revision,
                    },
                    run: Box::new(move || {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }),
                }
            }))
        },
    );
    native_keys::with_test_discovery(discovery, || {
        fixture.run();
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
        {
            let mut guard = fixture.guard.lock().unwrap();
            fixture.original = guard.last_attempt.get("native:revision").copied();
            guard.in_flight = true;
        }
        run(&mut fixture)
    })
}

pub(crate) fn native_revision_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    with_native_revision(|fixture| {
        if state["revision"] == 2 {
            fixture.step("Replace");
        }
        match state["held"] {
            0 => fixture.step("Allow"),
            1 => {}
            2 => fixture.step("Person"),
            3 => fixture.step("Bot"),
            _ => panic!("unknown native hold"),
        }
        if state["calls"] == 1 {
            fixture.step("Try");
        }
        if state["clock_held"] == 1 {
            fixture.step("Future");
        }
        assert_eq!(&fixture.observe(), state, "reconstruct the declared input");
        fixture.step(action);
        fixture.observe()
    })
}

#[test]
fn native_revision_admission_conforms_and_preserves_held_history() {
    let model = aterm_models_harness::native_preparation_revision_model();
    assert_eq!(model.mutant_dials(), (1..=5).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 0);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let observed = native_revision_transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&observed));
                let mut wrong = observed.clone();
                wrong.insert("history", 3 - observed["history"]);
                assert!(!model.successors(action.name, &state).contains(&wrong));
            }
        }
    }
    with_native_revision(|fixture| {
        let mut state = fixture.observe();
        for action in [
            "Replace", "Blocked", "Person", "Blocked", "Bot", "Blocked", "Allow", "Future", "Try",
            "Repeat", "Repeat",
        ] {
            assert!(model.action_enabled(action, &state));
            fixture.step(action);
            let observed = fixture.observe();
            assert!(model.successors(action, &state).contains(&observed));
            state = observed;
        }
    });
}

struct Approval {
    live: AtomicBool,
    generation: AtomicU64,
    browser: Option<String>,
    recoveries: Mutex<Vec<accounts::shared::Intent>>,
    recovery_unavailable: AtomicBool,
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
    fn request_recovery(
        &self,
        grant: &Grant,
        row: &Row,
        intent: &accounts::shared::Intent,
    ) -> Result<(), String> {
        self.current(grant, row)?;
        self.recoveries.lock().unwrap().push(intent.clone());
        if self.recovery_unavailable.load(Ordering::SeqCst) {
            Err("recovery publication unavailable".into())
        } else {
            Ok(())
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
    fn current_exe(&self) -> Option<PathBuf> {
        Some(std::env::current_exe().unwrap())
    }
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
    fn preparations(
        &self,
        discover: &shared_renewal::Discovery,
        spawn: &SpawnRenewal<'_>,
        guard: &Arc<Mutex<AutoRenewGuard>>,
    ) {
        self.with_preparations(discover, spawn, guard, |fx| {
            shared_renewal::adopt_first(&self.home, &self.state, &self.roster, NOW, fx);
        });
    }

    fn with_preparations<T>(
        &self,
        discover: &shared_renewal::Discovery,
        spawn: &SpawnRenewal<'_>,
        guard: &Arc<Mutex<AutoRenewGuard>>,
        run: impl FnOnce(&UpkeepEffects<'_>) -> T,
    ) -> T {
        let fx = UpkeepEffects {
            scope: None,
            fetch: &|_, _| panic!("no source fetch"),
            spawn_renewal: spawn,
            sync_cooldown: &Mutex::new(None),
            rediscovery: &Mutex::new(Rediscovery::default()),
            renew_guard: guard,
            find_browser: &|_| false,
            vault_sync: |_, _scope| super::super::sign_in_ladder::Vault::NotMember,
            default_check: |_, _scope| super::super::cli::LoginCheck::CouldNotRun(String::new()),
            spawn_vault: &inline,
            mailbox: &|_, _, _| (super::super::sign_in_ladder::Mailbox::Ready, None),
            log: Arc::new(|_| {}),
            run_attempt: None,
            save_oauth: |_, _| Ok("inert".into()),
            shared: shared_renewal::Effects {
                authority: None,
                checkpoint_authority: None,
                host: Some(self.provider.clone()),
                discover: Some(discover),
            },
        };
        run(&fx)
    }

    fn new(browser: Option<&str>) -> Self {
        Self::with_custody(browser, true)
    }

    fn with_custody(browser: Option<&str>, shared: bool) -> Self {
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
        if shared {
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
                    logins.shared.finish(
                        token,
                        accounts::shared::Outcome::Unknown,
                        None,
                        NOW - 10,
                    )?;
                    Ok(Vec::new())
                })
                .unwrap();
        }
        let approval = Arc::new(Approval {
            live: AtomicBool::new(true),
            generation: AtomicU64::new(2),
            browser: browser.map(str::to_owned),
            recoveries: Mutex::new(Vec::new()),
            recovery_unavailable: AtomicBool::new(false),
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
        spawn: &SpawnRenewal<'_>,
        mailbox: bool,
    ) -> Upkept {
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        self.upkeep_observed(factory, spawn, mailbox, &guard, Arc::new(|_| {}))
    }

    fn upkeep_observed(
        &self,
        factory: Option<&shared_renewal::Factory>,
        spawn: &SpawnRenewal<'_>,
        mailbox: bool,
        guard: &Arc<Mutex<AutoRenewGuard>>,
        log: SignInLog,
    ) -> Upkept {
        self.upkeep_discovered(factory, None, spawn, mailbox, guard, log)
    }

    fn upkeep_discovered(
        &self,
        factory: Option<&shared_renewal::Factory>,
        discover: Option<&shared_renewal::Discovery>,
        spawn: &SpawnRenewal<'_>,
        mailbox: bool,
        guard: &Arc<Mutex<AutoRenewGuard>>,
        log: SignInLog,
    ) -> Upkept {
        let fx = UpkeepEffects {
            scope: None,
            fetch: &|_, _| panic!("no source fetch"),
            spawn_renewal: spawn,
            sync_cooldown: &Mutex::new(None),
            rediscovery: &Mutex::new(Rediscovery::default()),
            renew_guard: guard,
            find_browser: &|_| false,
            vault_sync: |_, _scope| super::super::sign_in_ladder::Vault::NotMember,
            default_check: |_, _scope| super::super::cli::LoginCheck::CouldNotRun(String::new()),
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
            save_oauth: |_, _| Ok("inert".into()),
            shared: shared_renewal::Effects {
                authority: factory,
                checkpoint_authority: factory,
                host: Some(self.provider.clone()),
                discover,
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

struct RenewalAdmission {
    bed: Bed,
    guard: Arc<Mutex<AutoRenewGuard>>,
    held: Option<accounts::SigninTurn>,
    start: Instant,
}

impl RenewalAdmission {
    fn new() -> Self {
        let bed = Bed::new(None);
        // Keep the row eligible after its consumed attempt so Used reaches
        // the renewal gate instead of being filtered as already signed in.
        bed.provider.status_error.store(true, Ordering::SeqCst);
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let start = Instant::now();
        {
            let mut history = guard.lock().unwrap();
            history.last_attempt.insert("shared".into(), start);
            history
                .shared_preparations
                .insert("shared".into(), (SOURCE, [1; 32]));
        }
        let held = Some(bed.store.signin_now().unwrap().unwrap());
        Self {
            bed,
            guard,
            held,
            start,
        }
    }

    fn step(&mut self, action: &str) {
        match action {
            "Unlock" => drop(self.held.take()),
            "Blocked" | "Start" | "Used" => {
                let before = self.bed.approval.recoveries.lock().unwrap().len();
                self.bed.upkeep_observed(
                    Some(&self.bed.factory()),
                    &inline,
                    true,
                    &self.guard,
                    Arc::new(|_| {}),
                );
                if action == "Used" {
                    assert_eq!(
                        self.bed.approval.recoveries.lock().unwrap().len(),
                        before + 1,
                        "the consumed-preparation gate was reached"
                    );
                }
            }
            _ => panic!("unknown renewal admission action"),
        }
    }

    fn observe(&self) -> aterm_spec::interp::State {
        let history = self.guard.lock().unwrap();
        let (source, decision) = history.shared_preparations["shared"];
        assert_eq!(source, SOURCE);
        aterm_spec::interp::State::from([
            ("locked", i64::from(self.held.is_some())),
            ("history", i64::from(decision[0])),
            (
                "calls",
                i64::try_from(self.bed.provider.calls.load(Ordering::SeqCst)).unwrap(),
            ),
        ])
    }
}

#[test]
fn blocked_shared_renewal_keeps_the_last_admitted_preparation() {
    let mut fixture = RenewalAdmission::new();
    for _ in 0..3 {
        fixture.step("Blocked");
        assert_eq!(fixture.observe()["history"], 1);
        assert_eq!(fixture.observe()["calls"], 0);
        assert_eq!(
            fixture.guard.lock().unwrap().last_attempt["shared"],
            fixture.start
        );
    }
    fixture.step("Unlock");
    fixture.step("Start");
    fixture.step("Used");
    fixture.step("Used");
    assert_eq!(fixture.observe()["history"], 2);
    assert_eq!(fixture.observe()["calls"], 1);
}

pub(crate) fn renewal_admission_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut fixture = RenewalAdmission::new();
    if state["locked"] == 0 {
        fixture.step("Unlock");
    }
    if state["calls"] == 1 {
        fixture.step("Start");
    }
    assert_eq!(
        &fixture.observe(),
        state,
        "reconstruct the declared starting state"
    );
    fixture.step(action);
    fixture.observe()
}

#[test]
fn shared_renewal_admission_conforms_and_rejects_premature_history() {
    let model = aterm_models_harness::shared_renewal_admission_model();
    assert_eq!(model.mutant_dials(), (1..=6).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 0);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let observed = renewal_admission_transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&observed));
                let mut wrong = observed.clone();
                wrong.insert("history", 3 - observed["history"]);
                assert!(!model.successors(action.name, &state).contains(&wrong));
            }
        }
    }
    let mut fixture = RenewalAdmission::new();
    let mut state = fixture.observe();
    for action in ["Blocked", "Blocked", "Unlock", "Start", "Used", "Used"] {
        assert!(model.action_enabled(action, &state));
        fixture.step(action);
        let observed = fixture.observe();
        assert!(model.successors(action, &state).contains(&observed));
        state = observed;
    }
}

#[test]
fn shared_renewal_backoffs_preserve_admission_history() {
    for bot in [false, true] {
        let mut fixture = RenewalAdmission::new();
        fixture.step("Unlock");
        {
            let mut history = fixture.guard.lock().unwrap();
            if bot {
                &mut history.bot_checked
            } else {
                &mut history.parked
            }
            .insert("shared".into(), fixture.start);
        }
        fixture.step("Start");
        assert_eq!(fixture.observe()["history"], 1);
        assert_eq!(fixture.observe()["calls"], 0);
        assert_eq!(
            fixture.guard.lock().unwrap().last_attempt["shared"],
            fixture.start
        );
        {
            let mut history = fixture.guard.lock().unwrap();
            history.parked.clear();
            history.bot_checked.clear();
        }
        fixture.step("Start");
        assert_eq!(fixture.observe()["history"], 2);
        assert_eq!(fixture.observe()["calls"], 1);
    }
}

#[test]
fn a_new_shared_preparation_wakes_unchanged_unavailable_routes() {
    let bed = Bed::new(None);
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let lines = Arc::new(Mutex::new(Vec::new()));
    let logs = lines.clone();
    let log: SignInLog = Arc::new(move |line| logs.lock().unwrap().push(line.to_owned()));
    let run = || {
        bed.upkeep_observed(Some(&bed.factory()), &inline, false, &guard, log.clone());
    };
    let no_route = || {
        lines
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line.contains("tried again in"))
            .count()
    };
    run();
    assert_eq!(no_route(), 1);
    run();
    assert_eq!(
        no_route(),
        1,
        "unchanged preparation retains its route backoff"
    );
    bed.approval.generation.store(3, Ordering::SeqCst);
    run();
    assert_eq!(no_route(), 2, "new preparation wakes the route probe");
    assert_eq!(bed.provider.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn uncertain_shared_renewal_requests_recovery_without_replaying_the_provider() {
    let bed = Bed::new(None);
    bed.provider.status_error.store(true, Ordering::SeqCst);
    bed.approval
        .recovery_unavailable
        .store(true, Ordering::SeqCst);
    bed.upkeep(Some(&bed.factory()), &inline, true);
    let failed = bed.store.load_logins(NOW + 1).unwrap().0;
    let attempt = failed.shared.latest(SOURCE).unwrap();
    assert_eq!(attempt.outcome, accounts::shared::Outcome::Unknown);
    assert_eq!(bed.provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        bed.approval.recoveries.lock().unwrap().as_slice(),
        std::slice::from_ref(&attempt.intent)
    );

    bed.approval
        .recovery_unavailable
        .store(false, Ordering::SeqCst);
    bed.upkeep(Some(&bed.factory()), &inline, true);
    assert_eq!(bed.provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(bed.store.load_logins(NOW + 1).unwrap().0, failed);
    assert_eq!(
        *bed.approval.recoveries.lock().unwrap(),
        [attempt.intent.clone(), attempt.intent.clone()]
    );

    // A successful issuer response is a distinct preparation, never a refund.
    bed.approval.generation.store(3, Ordering::SeqCst);
    bed.provider.status_error.store(false, Ordering::SeqCst);
    bed.upkeep(Some(&bed.factory()), &inline, true);
    assert_eq!(bed.provider.calls.load(Ordering::SeqCst), 2);
    let renewed = bed.store.load_logins(NOW + 1).unwrap().0;
    assert_eq!(renewed.shared.latest(SOURCE).unwrap().intent.generation, 3);
    assert!(matches!(
        renewed.shared.latest(SOURCE).unwrap().outcome,
        accounts::shared::Outcome::Published { .. }
    ));
}

#[cfg(unix)]
#[test]
fn shared_renewal_activity_names_the_running_route_and_ends_with_the_attempt() {
    for fails in [false, true] {
        let bed = Bed::new(None);
        bed.provider.fail.store(fails, Ordering::SeqCst);
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let jobs = Arc::new(Mutex::new(Vec::new()));
        let queued = jobs.clone();
        let queue = move |work| {
            queued.lock().unwrap().push(work);
            Ok(())
        };
        let observed = || {
            login_activity_of(
                &guard.lock().unwrap(),
                &bed.store,
                Cli::Claude,
                NOW + 1,
                Instant::now(),
            )
        };
        std::thread::scope(|scope| {
            let (entered, reached) = std::sync::mpsc::sync_channel(1);
            let (resume, resumed) = std::sync::mpsc::sync_channel(1);
            *bed.provider.flow_effect.lock().unwrap() = Some(Box::new(move || {
                let _ = entered.send(());
                // Dropping the sender also releases this mock during assertion unwinding.
                match resumed.recv_timeout(aterm_spec::wait::MUST_HAPPEN) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        panic!("shared provider release hung")
                    }
                }
            }));
            bed.upkeep_observed(Some(&bed.factory()), &queue, true, &guard, Arc::new(|_| {}));
            assert_eq!(bed.provider.calls.load(Ordering::SeqCst), 0);
            let waiting = observed();
            let queued_row = waiting.iter().find(|row| row.label == "shared").unwrap();
            assert!(!queued_row.active);
            assert_eq!(queued_row.route, None);
            assert!(queued_row.next_at.is_some());
            let work = jobs
                .lock()
                .unwrap()
                .pop()
                .expect("shared renewal was queued");
            let running = scope.spawn(work);
            reached
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("shared provider was entered");
            let rows = observed();
            let row = rows.iter().find(|row| row.label == "shared").unwrap();
            assert!(row.active, "{rows:?}");
            assert_eq!(row.route.as_deref(), Some("mailbox"));
            assert!(
                login_activity_of(
                    &guard.lock().unwrap(),
                    &bed.store,
                    Cli::Codex,
                    NOW + 1,
                    Instant::now()
                )
                .is_empty()
            );
            resume
                .send(())
                .expect("shared provider remains blocked until released");
            running.join().unwrap();
        });
        assert_eq!(bed.provider.calls.load(Ordering::SeqCst), 1);
        assert!(jobs.lock().unwrap().is_empty());
        assert!(observed().iter().all(|row| !row.active));
        let (logins, _) = bed.store.load_logins(NOW + 1).unwrap();
        let attempt = logins.shared.latest(SOURCE).unwrap();
        assert_eq!(attempt.intent.generation, 2);
        if fails {
            assert_eq!(attempt.outcome, accounts::shared::Outcome::Unknown);
        } else {
            assert!(matches!(
                attempt.outcome,
                accounts::shared::Outcome::Published { .. }
            ));
        }
    }
}

fn observe_recovery(case: i64) -> (i64, i64, i64) {
    use accounts::shared::Outcome;
    let bed = Bed::new(None);
    bed.approval.generation.store(1, Ordering::SeqCst);
    if case == 15 || case == 16 {
        bed.store
            .transact_pool_logins_within(&[], &bed.roster, NOW, &mut None, |_, _, logins| {
                accounts::admit_workflow(logins, "shared")?;
                Ok(Vec::new())
            })
            .unwrap();
        let before = bed.store.load_logins(NOW).unwrap().0;
        let signin = bed.store.signin_now().unwrap().unwrap();
        bed.upkeep(Some(&bed.factory()), &inline, true);
        assert!(bed.approval.recoveries.lock().unwrap().is_empty());
        drop(signin);
        if case == 16 {
            bed.upkeep(Some(&bed.factory()), &inline, true);
        }
        let requests = bed.approval.recoveries.lock().unwrap();
        for requested in requests.iter() {
            assert_eq!(requested, &before.shared.latest(SOURCE).unwrap().intent);
        }
        return (
            requests.len() as i64,
            i64::from(!requests.is_empty()),
            i64::from(
                before == bed.store.load_logins(NOW).unwrap().0
                    && bed.provider.calls.load(Ordering::SeqCst) == 0,
            ),
        );
    }
    if [2, 4, 5, 17, 18, 19, 20, 21].contains(&case) {
        bed.approval.generation.store(2, Ordering::SeqCst);
        let mut intent = bed
            .store
            .load_logins(NOW)
            .unwrap()
            .0
            .shared
            .latest(SOURCE)
            .unwrap()
            .intent
            .clone();
        intent.generation = 2;
        intent.decision = [2; 32];
        intent.request = [6; 16];
        bed.store
            .transact_logins(NOW, &mut None, |_, logins| {
                let token = logins
                    .shared
                    .begin(intent, profile::principal_id("approved-provider"))?;
                match case {
                    4 => logins.shared.finish(token, Outcome::Held, None, NOW)?,
                    5 => logins.shared.not_started(&token, NOW)?,
                    _ => {}
                }
                Ok(Vec::new())
            })
            .unwrap();
    } else if case == 3 {
        bed.approval.generation.store(2, Ordering::SeqCst);
        bed.upkeep(Some(&bed.factory()), &inline, true);
        assert!(matches!(
            bed.store
                .load_logins(NOW)
                .unwrap()
                .0
                .shared
                .latest(SOURCE)
                .unwrap()
                .outcome,
            Outcome::Published { .. }
        ));
    }
    if case == 21 {
        let logins = bed.store.load_logins(NOW).unwrap().0;
        let dir = logins.shared.latest(SOURCE).unwrap().intent.staging.clone();
        let before = fs::read_to_string(&bed.roster).unwrap();
        let after = accounts::append_rows_for(
            Cli::Claude,
            &before,
            &[Row {
                label: "other".into(),
                email: Some("other@example.test".into()),
                dir: Some(dir),
                ..Row::default()
            }],
        )
        .unwrap();
        fs::write(&bed.roster, after).unwrap();
    }
    let records = bed
        .store
        .read_account_records(Some(&bed.roster), NOW)
        .unwrap();
    let row = records.roster.row("shared").unwrap();
    if (17..=21).contains(&case) {
        let _held = bed
            .store
            .acquire_within(&accounts::LOCK_LADDER)
            .unwrap()
            .unwrap();
        let staging = &records.logins.shared.latest(SOURCE).unwrap().intent.staging;
        if case != 18 {
            fs::write(Path::new(staging).join(".claude.json"), format!(r#"{{"oauthAccount":{{"emailAddress":"{EMAIL}","accountUuid":"approved-provider"}}}}"#)).unwrap();
        }
        accounts::write_renewal_checkpoint("shared", EMAIL, staging, None, NOW, &records.logins)
            .unwrap();
    }
    if case == 19 {
        bed.store
            .transact_logins(NOW + 1, &mut None, |state, _| {
                Ok(accounts::note_auth(
                    state,
                    "shared",
                    accounts::Auth::SignedOut,
                    NOW + 1,
                ))
            })
            .unwrap();
    }
    if case == 20 {
        bed.store
            .transact_logins(NOW, &mut None, |state, _| {
                Ok(accounts::note_wall(
                    state,
                    "shared",
                    "five_hour",
                    Some(NOW + 3600),
                    NOW,
                    NOW,
                ))
            })
            .unwrap();
        assert!(
            bed.store
                .load(NOW)
                .unwrap()
                .0
                .account("shared")
                .limited_until(NOW)
                .is_some()
        );
    }
    if case == 17 {
        let mut changed = row.clone();
        changed.identity_email = Some("replacement@example.test".into());
        assert!(
            !super::super::cli::shared_renewal::pending_checkpoint(
                Cli::Claude,
                &changed,
                &bed.approval.prepare(row).unwrap(),
                &records.raw_roster,
                &records.state,
                &records.logins,
                NOW
            ),
            "a checkpoint for the old identity must not suppress replacement"
        );
    }
    let policy = accounts::RenewalPolicy {
        roster_path: &bed.roster,
        roster: &records.raw_roster,
        row,
        logins: &records.logins,
    };
    let mut grant = bed.approval.prepare(row).unwrap();
    match case {
        6 => {
            bed.store
                .transact_logins(NOW, &mut None, |_, logins| {
                    accounts::note_proof(logins, "shared", accounts::Proof::Unchecked);
                    Ok(Vec::new())
                })
                .unwrap();
        }
        7 => grant.decision = [99; 32],
        8 => bed.approval.live.store(false, Ordering::SeqCst),
        9 | 10 => bed
            .approval
            .recovery_unavailable
            .store(true, Ordering::SeqCst),
        11 => {
            let mut changed = records.raw_roster.clone();
            changed.rows[0].email = Some("changed@example.test".into());
            fs::write(
                &bed.roster,
                accounts::append_rows_for(Cli::Claude, "", &changed.rows).unwrap(),
            )
            .unwrap();
        }
        12 => grant.identity = "wrong@example.test".into(),
        13 => grant.configuration = [99; 32],
        14 => grant.generation += 1,
        _ => {}
    }
    let before = bed.store.load_logins(NOW).unwrap().0;
    let roster_before = fs::read(&bed.roster).unwrap();
    let provider_before = bed.provider.calls.load(Ordering::SeqCst);
    let signin = bed.store.signin_now().unwrap().unwrap();
    let request = || {
        super::super::cli::shared_renewal::recover_consumed(
            bed.approval.as_ref(),
            &grant,
            &bed.store,
            policy,
            &signin,
            NOW,
        )
    };
    let mut accepted =
        i64::from(request().is_ok() && !bed.approval.recoveries.lock().unwrap().is_empty());
    if case == 10 {
        bed.approval
            .recovery_unavailable
            .store(false, Ordering::SeqCst);
        accepted += i64::from(request().is_ok());
    }
    let requests = bed.approval.recoveries.lock().unwrap();
    for requested in requests.iter() {
        assert_eq!(requested, &before.shared.latest(SOURCE).unwrap().intent);
    }
    let unchanged = before == bed.store.load_logins(NOW).unwrap().0
        && roster_before == fs::read(&bed.roster).unwrap()
        && provider_before == bed.provider.calls.load(Ordering::SeqCst);
    (requests.len() as i64, accepted, i64::from(unchanged))
}

pub(crate) fn recovery_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let case = match action {
        "Unknown" => 1,
        "Pending" => 2,
        "Published" => 3,
        "Held" => 4,
        "NotStarted" => 5,
        "ChangedBinding" => 6,
        "ChangedDecision" => 7,
        "Stopped" => 8,
        "Unavailable" => 9,
        "Retry" => 10,
        "ChangedRoster" => 11,
        "WrongIdentity" => 12,
        "WrongConfiguration" => 13,
        "WrongGeneration" => 14,
        "BusySigner" => 15,
        "AfterSigner" => 16,
        "Checkpoint" => 17,
        "MissingCredential" => 18,
        "NewerSignout" => 19,
        "QuotaCheckpoint" => 20,
        "ReferencedCheckpoint" => 21,
        _ => panic!("unknown recovery action {action}"),
    };
    let (requests, accepted, unchanged) = observe_recovery(case);
    let mut next = state.clone();
    next.insert("case", case);
    next.insert("requests", requests);
    next.insert("accepted", accepted);
    next.insert("unchanged", unchanged);
    next
}

#[test]
fn shared_recovery_conforms_and_detects_the_silent_noop() {
    let model = aterm_models_harness::shared_recovery_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 0);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let observed = recovery_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&observed),
                    "{} {state:?} -> {observed:?}",
                    action.name
                );
                if action.name == "Unknown" {
                    let mut lost = observed;
                    lost.insert("requests", 0);
                    lost.insert("accepted", 0);
                    assert!(
                        !model.successors(action.name, &state).contains(&lost),
                        "the former missing callback must fail conformance"
                    );
                }
            }
        }
    }
}

/// Unix only, where aterm links the opener an unattended renewal's page is handed to
/// (`cli::opener_link`); off Unix the sign-in ladder's
/// `an_unattended_run_is_handed_nothing_where_aterm_links_no_opener` holds what runs.
#[cfg(unix)]
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
            !guard.lock().unwrap().last_attempt.contains_key("shared"),
            "policy stops must not retain the admission cooldown"
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

/// Unix only, where aterm links the opener an unattended renewal's page is handed to
/// (`cli::opener_link`); off Unix the sign-in ladder's
/// `an_unattended_run_is_handed_nothing_where_aterm_links_no_opener` holds what runs.
#[cfg(unix)]
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
            crate::test_config::write(config_beside(&bed.roster), "[harness]\nlogins = \"off\"\n")
                .unwrap();
        }
        if case == 5 {
            crate::test_config::write(
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

/// Unix only, where aterm links the opener an unattended renewal's page is handed to
/// (`cli::opener_link`); off Unix the sign-in ladder's
/// `an_unattended_run_is_handed_nothing_where_aterm_links_no_opener` holds what runs.
#[cfg(unix)]
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
            crate::test_config::write(config_beside(&bed.roster), "[harness]\nlogins = \"off\"\n")
                .unwrap();
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

/// Unix only, where aterm links the opener an unattended renewal's page is handed to
/// (`cli::opener_link`); off Unix the sign-in ladder's
/// `an_unattended_run_is_handed_nothing_where_aterm_links_no_opener` holds what runs.
#[cfg(unix)]
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

// For the unattended-renewal tests alone, which run only where aterm links the opener (Unix).
#[cfg(unix)]
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

struct ExistingMustUseRenewal;

struct ResumableAdoption {
    calls: AtomicUsize,
}

struct CandidateApproval(Grant);

impl Authority for CandidateApproval {
    fn prepare(&self, row: &Row) -> Result<Grant, String> {
        if row.email.as_deref() == Some(self.0.identity.as_str()) {
            Ok(self.0.clone())
        } else {
            Err("wrong candidate identity".into())
        }
    }

    fn current(&self, grant: &Grant, row: &Row) -> Result<(), String> {
        (self.prepare(row)? == *grant)
            .then_some(())
            .ok_or_else(|| "changed candidate".into())
    }
}

struct UnstartedCandidate {
    id: u8,
    calls: Arc<Mutex<Vec<u8>>>,
}

impl super::super::cli::shared_provider::Provider for UnstartedCandidate {
    fn execute(
        &self,
        _: &super::super::cli::shared_provider::Context<'_>,
        _: &Row,
        _: &Path,
        _: &RenewBrowser,
    ) -> Result<(), String> {
        panic!("typed execution required")
    }

    fn execute_attempt(
        &self,
        _: &super::super::cli::shared_provider::Context<'_>,
        _: &Row,
        _: &Path,
        _: &RenewBrowser,
    ) -> super::super::cli::shared_provider::Execution {
        self.calls.lock().unwrap().push(self.id);
        super::super::cli::shared_provider::Execution::NotStarted(
            "synthetic unavailable dependency".into(),
        )
    }

    fn observe(
        &self,
        _: &super::super::cli::shared_provider::Context<'_>,
        _: &Row,
        _: &Path,
    ) -> Result<
        Option<Box<dyn super::super::cli::shared_provider::LoginProof>>,
        super::super::cli::shared_provider::ObservationError,
    > {
        panic!("an unstarted provider has no credential to observe")
    }
}

pub(super) fn unstarted_candidate(
    id: u8,
    calls: &Arc<Mutex<Vec<u8>>>,
) -> shared_renewal::Candidate {
    let email = format!("candidate-{id}@example.test");
    shared_renewal::Candidate {
        row: Row {
            label: format!("candidate-{id}"),
            email: Some(email.clone()),
            ..Row::default()
        },
        cli: Cli::Claude,
        authority: Arc::new(CandidateApproval(Grant {
            source: accounts::shared::Source {
                network: [7; 32],
                account: [id; 32],
            },
            decision: [id + 10; 32],
            generation: 1,
            configuration: [id + 20; 32],
            identity: email,
            expected_principal: Some([id; 32]),
            required_browser_profile: None,
        })),
        provider: Arc::new(UnstartedCandidate {
            id,
            calls: calls.clone(),
        }),
        ordinary_renewal: true,
    }
}

#[test]
fn sparse_shared_preparation_passes_do_not_starve_later_candidates() {
    let bed = Bed::with_custody(None, false);
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let offered = calls.clone();
    let discover = move |_: &Path| {
        Ok(vec![
            unstarted_candidate(1, &offered),
            unstarted_candidate(2, &offered),
        ])
    };
    let start = Instant::now();
    // Each pass a spacing apart on the wall clock, so only the cooldown's clock decides.
    for (pass, seconds) in [0, 901, 1802, 2703].into_iter().enumerate() {
        bed.with_preparations(&discover, &inline, &guard, |fx| {
            assert!(shared_renewal::prepare_shared_with_clock(
                &bed.home,
                &bed.state,
                &bed.roster,
                NOW + crate::harness::sign_in_ladder::SPACING_S * i64::try_from(pass).unwrap(),
                fx,
                || start + Duration::from_secs(seconds),
            ));
        });
    }
    assert_eq!(*calls.lock().unwrap(), [1, 2, 1, 2]);
}

/// Two accounts' adoptions on one Mac start at least ten minutes apart on the wall clock, whatever
/// the cooldown's clock allows: the second waits, and starts once the spacing has passed.
#[test]
fn two_accounts_adoptions_start_ten_minutes_apart() {
    let bed = Bed::with_custody(None, false);
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let offered = calls.clone();
    let discover = move |_: &Path| {
        Ok(vec![
            unstarted_candidate(1, &offered),
            unstarted_candidate(2, &offered),
        ])
    };
    let start = Instant::now();
    for (now, admitted) in [
        (NOW, true),
        (NOW + crate::harness::sign_in_ladder::SPACING_S - 1, false),
        (NOW + crate::harness::sign_in_ladder::SPACING_S, true),
    ] {
        bed.with_preparations(&discover, &inline, &guard, |fx| {
            assert_eq!(
                shared_renewal::prepare_shared_with_clock(
                    &bed.home,
                    &bed.state,
                    &bed.roster,
                    now,
                    fx,
                    || start + Duration::from_secs(901),
                ),
                admitted,
                "at {} s",
                now - NOW
            );
        });
    }
    assert_eq!(*calls.lock().unwrap(), [1, 2]);
}

#[test]
fn shared_candidate_history_is_independent_across_networks() {
    for offsets in [[0, 901, 1802, 2703], [0, 1, 2, 3]] {
        let bed = Bed::with_custody(None, false);
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let offered = calls.clone();
        let discover = move |_: &Path| {
            Ok((1..=3)
                .map(|id| {
                    let mut candidate = unstarted_candidate(id, &offered);
                    let mut grant = candidate.authority.prepare(&candidate.row).unwrap();
                    grant.source.network = [id; 32];
                    grant.source.account = [42; 32];
                    candidate.authority = Arc::new(CandidateApproval(grant));
                    candidate
                })
                .collect())
        };
        let start = Instant::now();
        for (index, seconds) in offsets.into_iter().enumerate() {
            bed.with_preparations(&discover, &inline, &guard, |fx| {
                assert_eq!(
                    shared_renewal::prepare_shared_with_clock(
                        &bed.home,
                        &bed.state,
                        &bed.roster,
                        NOW + crate::harness::sign_in_ladder::SPACING_S
                            * i64::try_from(index).unwrap(),
                        fx,
                        || start + Duration::from_secs(seconds),
                    ),
                    index < 3 || seconds >= 900
                );
            });
        }
        let expected: &[u8] = if offsets[3] >= 900 {
            &[1, 2, 3, 1]
        } else {
            &[1, 2, 3]
        };
        assert_eq!(*calls.lock().unwrap(), expected);
        assert_eq!(guard.lock().unwrap().shared_preparations.len(), 3);
    }
}

#[test]
fn reissued_shared_candidate_runs_without_resetting_its_peer_cooldown() {
    let bed = Bed::with_custody(None, false);
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let offered = calls.clone();
    let reissued = Arc::new(AtomicBool::new(false));
    let revision = reissued.clone();
    let discover = move |_: &Path| {
        let mut b = unstarted_candidate(2, &offered);
        if revision.load(Ordering::SeqCst) {
            let mut grant = b.authority.prepare(&b.row).unwrap();
            grant.decision = [99; 32];
            grant.generation += 1;
            b.authority = Arc::new(CandidateApproval(grant));
        }
        Ok(vec![unstarted_candidate(1, &offered), b])
    };
    let start = Instant::now();
    for seconds in 0..3 {
        reissued.store(seconds == 2, Ordering::SeqCst);
        bed.with_preparations(&discover, &inline, &guard, |fx| {
            assert!(shared_renewal::prepare_shared_with_clock(
                &bed.home,
                &bed.state,
                &bed.roster,
                NOW + crate::harness::sign_in_ladder::SPACING_S * i64::try_from(seconds).unwrap(),
                fx,
                || start + Duration::from_secs(seconds),
            ));
        });
    }
    assert_eq!(*calls.lock().unwrap(), [1, 2, 2]);
    let key = shared_renewal::shared_key(accounts::shared::Source {
        network: [7; 32],
        account: [1; 32],
    });
    assert_eq!(guard.lock().unwrap().last_attempt[&key], start);
}

#[test]
fn held_shared_candidates_preserve_history_and_let_peers_run() {
    for bot in [false, true] {
        let bed = Bed::with_custody(None, false);
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let offered = calls.clone();
        let a = unstarted_candidate(1, &calls);
        let grant = a.authority.prepare(&a.row).unwrap();
        let key = shared_renewal::shared_key(grant.source);
        let start = Instant::now();
        {
            let mut state = guard.lock().unwrap();
            state.last_attempt.insert(key.clone(), start);
            state
                .shared_preparations
                .insert(key.clone(), (grant.source, [99; 32]));
            if bot {
                &mut state.bot_checked
            } else {
                &mut state.parked
            }
            .insert(key.clone(), start);
        }
        let discover = move |_: &Path| {
            Ok(vec![
                unstarted_candidate(1, &offered),
                unstarted_candidate(2, &offered),
            ])
        };
        bed.with_preparations(&discover, &inline, &guard, |fx| {
            assert!(shared_renewal::prepare_shared_with_clock(
                &bed.home,
                &bed.state,
                &bed.roster,
                NOW,
                fx,
                || start + Duration::from_secs(901),
            ));
        });
        assert_eq!(*calls.lock().unwrap(), [2]);
        let state = guard.lock().unwrap();
        assert_eq!(state.last_attempt[&key], start);
        assert_eq!(state.shared_preparations[&key], (grant.source, [99; 32]));
    }
}

struct RevokedAfterRanking {
    grant: Grant,
    prepares: AtomicUsize,
}

impl Authority for RevokedAfterRanking {
    fn prepare(&self, _: &Row) -> Result<Grant, String> {
        if self.prepares.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(self.grant.clone())
        } else {
            Err("fixture revoked after ranking".into())
        }
    }

    fn current(&self, _: &Grant, _: &Row) -> Result<(), String> {
        Err("fixture revoked after ranking".into())
    }
}

#[test]
fn shared_candidate_rank_does_not_bypass_live_authorization() {
    let bed = Bed::with_custody(None, false);
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let offered = calls.clone();
    let discover = move |_: &Path| {
        let mut revoked = unstarted_candidate(1, &offered);
        revoked.authority = Arc::new(RevokedAfterRanking {
            grant: revoked.authority.prepare(&revoked.row).unwrap(),
            prepares: AtomicUsize::new(0),
        });
        Ok(vec![revoked, unstarted_candidate(2, &offered)])
    };
    bed.with_preparations(&discover, &inline, &guard, |fx| {
        assert!(shared_renewal::prepare_shared_with_clock(
            &bed.home,
            &bed.state,
            &bed.roster,
            NOW,
            fx,
            Instant::now,
        ));
    });
    assert_eq!(*calls.lock().unwrap(), [2]);
    assert!(
        !guard
            .lock()
            .unwrap()
            .last_attempt
            .contains_key(&shared_renewal::shared_key(accounts::shared::Source {
                network: [7; 32],
                account: [1; 32],
            }))
    );
}

#[test]
fn shared_candidates_in_cooldown_do_not_block_an_unseen_candidate() {
    let bed = Bed::with_custody(None, false);
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let offered = calls.clone();
    let discover = move |_: &Path| {
        Ok(vec![
            unstarted_candidate(1, &offered),
            unstarted_candidate(2, &offered),
        ])
    };
    let start = Instant::now();
    for seconds in [0, 1] {
        bed.with_preparations(&discover, &inline, &guard, |fx| {
            assert!(shared_renewal::prepare_shared_with_clock(
                &bed.home,
                &bed.state,
                &bed.roster,
                NOW + crate::harness::sign_in_ladder::SPACING_S * i64::try_from(seconds).unwrap(),
                fx,
                || start + Duration::from_secs(seconds),
            ));
        });
    }
    assert_eq!(*calls.lock().unwrap(), [1, 2]);
}

#[test]
fn steady_native_preparations_give_shared_oauth_the_next_admission() {
    use super::super::native_keys::{self, PreparationTask};
    let bed = Bed::with_custody(None, false);
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let provider = Arc::new(ResumableAdoption {
        calls: AtomicUsize::new(0),
    });
    let actor = bed.approval.clone();
    let execution = provider.clone();
    let discover = move |_: &Path| {
        Ok(vec![shared_renewal::Candidate {
            row: Row {
                label: "discovered".into(),
                email: Some(EMAIL.into()),
                ..Row::default()
            },
            cli: Cli::Claude,
            authority: actor.clone(),
            provider: execution.clone(),
            ordinary_renewal: true,
        }])
    };
    let native_calls = Arc::new(AtomicUsize::new(0));
    let sequence = Arc::new(AtomicUsize::new(0));
    let native = {
        let calls = native_calls.clone();
        Box::new(
            move |_: &Path, _: &Path, _: &Path, admit: &mut dyn FnMut(&super::super::native_keys::PreparationId) -> bool| {
                let preparation = native_preparation_id(format!("native:{}", sequence.fetch_add(1, Ordering::SeqCst)));
                Ok(admit(&preparation).then(|| {
                    let calls = calls.clone();
                    PreparationTask {
                        preparation,
                        run: Box::new(move || {
                            calls.fetch_add(1, Ordering::SeqCst);
                            Ok(())
                        }),
                    }
                }))
            },
        )
    };
    native_keys::with_test_discovery(native, || {
        let queued = Arc::new(Mutex::new(None));
        let pending = queued.clone();
        let enqueue = move |work| {
            *pending.lock().unwrap() = Some(work);
            Ok(())
        };
        bed.preparations(&discover, &enqueue, &guard);
        assert_eq!(native_calls.load(Ordering::SeqCst), 0);
        bed.preparations(&discover, &inline, &guard);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        queued.lock().unwrap().take().unwrap()();
        bed.preparations(&discover, &inline, &guard);
        assert_eq!(native_calls.load(Ordering::SeqCst), 1);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        bed.preparations(&discover, &inline, &guard);
        let key = shared_renewal::shared_key(SOURCE);
        assert!(guard.lock().unwrap().last_attempt.remove(&key).is_some());
        bed.preparations(&discover, &inline, &guard);
    });
    assert_eq!(native_calls.load(Ordering::SeqCst), 2);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn unavailable_shared_preparation_falls_back_to_native_without_a_wasted_pass() {
    use super::super::native_keys::{self, PreparationTask};
    let bed = Bed::with_custody(None, false);
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let native_calls = Arc::new(AtomicUsize::new(0));
    let shared_reads = Arc::new(AtomicUsize::new(0));
    let reads = shared_reads.clone();
    let discover = move |_: &Path| {
        reads.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    };
    let calls = native_calls.clone();
    let sequence = AtomicUsize::new(0);
    let native = Box::new(
        move |_: &Path,
              _: &Path,
              _: &Path,
              admit: &mut dyn FnMut(&super::super::native_keys::PreparationId) -> bool| {
            let preparation = native_preparation_id(format!(
                "native:{}",
                sequence.fetch_add(1, Ordering::SeqCst)
            ));
            Ok(admit(&preparation).then(|| {
                let calls = calls.clone();
                PreparationTask {
                    preparation,
                    run: Box::new(move || {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }),
                }
            }))
        },
    );
    native_keys::with_test_discovery(native, || {
        for _ in 0..3 {
            bed.preparations(&discover, &inline, &guard);
        }
    });
    assert_eq!(native_calls.load(Ordering::SeqCst), 3);
    assert_eq!(shared_reads.load(Ordering::SeqCst), 2);
}

#[test]
fn unavailable_native_preparation_keeps_first_chance_while_shared_work_progresses() {
    use super::super::native_keys;
    let bed = Bed::with_custody(None, false);
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let provider = Arc::new(ResumableAdoption {
        calls: AtomicUsize::new(0),
    });
    let actor = bed.approval.clone();
    let execution = provider.clone();
    let discover = move |_: &Path| {
        Ok(vec![shared_renewal::Candidate {
            row: Row {
                label: "discovered".into(),
                email: Some(EMAIL.into()),
                ..Row::default()
            },
            cli: Cli::Claude,
            authority: actor.clone(),
            provider: execution.clone(),
            ordinary_renewal: true,
        }])
    };
    let native_reads = Arc::new(AtomicUsize::new(0));
    let reads = native_reads.clone();
    let native = Box::new(
        move |_: &Path,
              _: &Path,
              _: &Path,
              _: &mut dyn FnMut(&super::super::native_keys::PreparationId) -> bool| {
            reads.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        },
    );
    native_keys::with_test_discovery(native, || {
        bed.preparations(&discover, &inline, &guard);
        let key = shared_renewal::shared_key(SOURCE);
        assert!(guard.lock().unwrap().last_attempt.remove(&key).is_some());
        bed.preparations(&discover, &inline, &guard);
    });
    assert_eq!(native_reads.load(Ordering::SeqCst), 2);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
}
struct RecordedAdoption {
    path: PathBuf,
    bytes: Vec<u8>,
}
impl super::super::cli::shared_provider::LoginProof for RecordedAdoption {
    fn identity(&self) -> &str {
        EMAIL
    }
    fn principal(&self) -> [u8; 32] {
        profile::principal_id("approved-provider").unwrap()
    }
    fn subscription(&self) -> Option<&str> {
        Some("max")
    }
    fn check_current(&self) -> Result<(), String> {
        (fs::read(&self.path).ok().as_ref() == Some(&self.bytes))
            .then_some(())
            .ok_or_else(|| "changed profile".into())
    }
}
impl super::super::cli::shared_provider::Provider for ResumableAdoption {
    fn execute(
        &self,
        _: &super::super::cli::shared_provider::Context<'_>,
        _: &Row,
        _: &Path,
        _: &RenewBrowser,
    ) -> Result<(), String> {
        panic!("typed execution required")
    }
    fn execute_attempt(
        &self,
        _: &super::super::cli::shared_provider::Context<'_>,
        _: &Row,
        staging: &Path,
        _: &RenewBrowser,
    ) -> super::super::cli::shared_provider::Execution {
        use super::super::cli::shared_provider::Execution;
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Execution::NotStarted("dependency not ready".into());
        }
        fs::write(staging.join(".claude.json"), format!(r#"{{"oauthAccount":{{"emailAddress":"{EMAIL}","accountUuid":"approved-provider"}}}}"#)).unwrap();
        Execution::Completed
    }
    fn observe(
        &self,
        _: &super::super::cli::shared_provider::Context<'_>,
        _: &Row,
        staging: &Path,
    ) -> Result<
        Option<Box<dyn super::super::cli::shared_provider::LoginProof>>,
        super::super::cli::shared_provider::ObservationError,
    > {
        let path = staging.join(".claude.json");
        let bytes = fs::read(&path).map_err(|error| {
            super::super::cli::shared_provider::ObservationError::RecoveryRequired(
                error.to_string(),
            )
        })?;
        Ok(Some(Box::new(RecordedAdoption { path, bytes })))
    }
}

fn observe_adoption_progress(phase: i64) -> [i64; 3] {
    let bed = Bed::with_custody(None, false);
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let provider = Arc::new(ResumableAdoption {
        calls: AtomicUsize::new(0),
    });
    let actor = bed.approval.clone();
    let execution = provider.clone();
    let discover = move |_: &Path| {
        Ok(vec![shared_renewal::Candidate {
            row: Row {
                label: "discovered".into(),
                email: Some(EMAIL.into()),
                ..Row::default()
            },
            cli: Cli::Claude,
            authority: actor.clone(),
            provider: execution.clone(),
            ordinary_renewal: true,
        }])
    };
    let run = || {
        bed.upkeep_discovered(
            Some(&bed.factory()),
            Some(&discover),
            &inline,
            true,
            &guard,
            Arc::new(|_| {}),
        )
    };
    let held = bed.store.signin_now().unwrap().unwrap();
    run();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert!(guard.lock().unwrap().last_attempt.is_empty());
    assert!(guard.lock().unwrap().shared_preparations.is_empty());
    drop(held);
    let mut original = None;
    if phase >= 2 {
        run();
        let logins = bed.store.load_logins(NOW).unwrap().0;
        let attempt = logins.shared.latest(SOURCE).unwrap();
        assert_eq!(attempt.outcome, accounts::shared::Outcome::NotStarted);
        original = Some(attempt.intent.clone());
    }
    if phase >= 3 {
        run();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            bed.store
                .load_logins(NOW)
                .unwrap()
                .0
                .shared
                .latest(SOURCE)
                .unwrap()
                .outcome,
            accounts::shared::Outcome::NotStarted
        );
    }
    if phase >= 4 {
        let key = shared_renewal::shared_key(SOURCE);
        *guard.lock().unwrap().last_attempt.get_mut(&key).unwrap() =
            Instant::now() - Duration::from_secs(901);
        run();
    }
    if phase >= 5 {
        run();
    }
    let logins = bed.store.load_logins(NOW + 1).unwrap().0;
    let current = logins.shared.latest(SOURCE);
    let outcome = match current.map(|attempt| attempt.outcome) {
        None => 0,
        Some(accounts::shared::Outcome::NotStarted) => 1,
        Some(accounts::shared::Outcome::Published { .. }) => 2,
        other => panic!("unexpected adoption outcome {other:?}"),
    };
    let same = original.is_none_or(|intent| current.unwrap().intent == intent);
    [
        provider.calls.load(Ordering::SeqCst) as i64,
        outcome,
        i64::from(same),
    ]
}

pub(crate) fn adoption_progress_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let phase = match action {
        "Busy" => 1,
        "Unstarted" => 2,
        "Cooldown" => 3,
        "Resume" => 4,
        "Healthy" => 5,
        _ => panic!("unknown adoption progress action"),
    };
    let [calls, outcome, same] = observe_adoption_progress(phase);
    let mut next = state.clone();
    for (key, value) in [
        ("phase", phase),
        ("calls", calls),
        ("outcome", outcome),
        ("same", same),
    ] {
        next.insert(key, value);
    }
    next
}

#[test]
fn listed_first_adoption_resumes_the_same_attempt_when_admission_and_cooldown_allow() {
    let model = aterm_models_harness::shared_adoption_progress_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 0);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let observed = adoption_progress_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&observed),
                    "{action:?} {observed:?}"
                );
                if action.name == "Resume" {
                    let mut stuck = observed;
                    stuck.insert("calls", 1);
                    stuck.insert("outcome", 1);
                    assert!(!model.successors(action.name, &state).contains(&stuck));
                }
            }
        }
    }
}

impl super::super::cli::shared_provider::Provider for ExistingMustUseRenewal {
    fn execute(
        &self,
        _: &super::super::cli::shared_provider::Context<'_>,
        _: &Row,
        _: &Path,
        _: &RenewBrowser,
    ) -> Result<(), String> {
        panic!("existing shared row reached first-adoption provider execution")
    }
    fn observe(
        &self,
        _: &super::super::cli::shared_provider::Context<'_>,
        _: &Row,
        _: &Path,
    ) -> Result<
        Option<Box<dyn super::super::cli::shared_provider::LoginProof>>,
        super::super::cli::shared_provider::ObservationError,
    > {
        panic!("existing shared row reached first-adoption provider observation")
    }
}

fn observe_shared_progress(phase: i64) -> [i64; 4] {
    let bed = Bed::new(None);
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let approval = bed.approval.clone();
    let discover = move |_: &Path| {
        Ok(vec![shared_renewal::Candidate {
            row: Row {
                label: "discovered".into(),
                email: Some(EMAIL.into()),
                ..Row::default()
            },
            cli: Cli::Claude,
            authority: approval.clone(),
            provider: Arc::new(ExistingMustUseRenewal),
            ordinary_renewal: true,
        }])
    };
    let run = || {
        bed.upkeep_discovered(
            Some(&bed.factory()),
            Some(&discover),
            &inline,
            true,
            &guard,
            Arc::new(|_| {}),
        )
    };
    bed.provider.status_error.store(true, Ordering::SeqCst);
    run();
    if phase >= 2 {
        run();
    }
    if phase >= 3 {
        bed.approval.generation.store(3, Ordering::SeqCst);
        bed.provider.status_error.store(false, Ordering::SeqCst);
        run();
    }
    if phase >= 4 {
        bed.approval.generation.store(4, Ordering::SeqCst);
        run();
    }
    let logins = bed.store.load_logins(NOW + 1).unwrap().0;
    let attempt = logins.shared.latest(SOURCE).unwrap();
    let requests = bed.approval.recoveries.lock().unwrap().len();
    [
        bed.provider.calls.load(Ordering::SeqCst) as i64,
        requests as i64,
        attempt.intent.generation as i64,
        i64::from(matches!(
            attempt.outcome,
            accounts::shared::Outcome::Published { .. }
        )),
    ]
}

pub(crate) fn shared_progress_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let phase = match action {
        "Fail" => 1,
        "Retry" => 2,
        "Reissue" => 3,
        "Healthy" => 4,
        _ => panic!("unknown progress action"),
    };
    let [calls, requests, generation, healthy] = observe_shared_progress(phase);
    let mut next = state.clone();
    for (name, value) in [
        ("phase", phase),
        ("calls", calls),
        ("requests", requests),
        ("generation", generation),
        ("healthy", healthy),
    ] {
        next.insert(name, value);
    }
    next
}

#[test]
fn shared_progress_with_live_discovery_and_persistent_guard_conforms() {
    let model = aterm_models_harness::shared_progress_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 0);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let observed = shared_progress_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&observed),
                    "{} {state:?} -> {observed:?}",
                    action.name
                );
                if action.name == "Reissue" {
                    let mut stuck = observed;
                    stuck.insert("calls", 1);
                    stuck.insert("healthy", 0);
                    stuck.insert("generation", 2);
                    assert!(
                        !model.successors(action.name, &state).contains(&stuck),
                        "a cooldown which ignores new authority must fail conformance"
                    );
                }
            }
        }
    }
}

/// Unix only, where aterm links the opener an unattended renewal's page is handed to
/// (`cli::opener_link`); off Unix the sign-in ladder's
/// `an_unattended_run_is_handed_nothing_where_aterm_links_no_opener` holds what runs.
#[cfg(unix)]
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
                    Box::new(move || {
                        crate::test_config::write(&path, "[harness]\nlogins = \"off\"\n").unwrap()
                    })
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
            assert!(
                bed.approval.recoveries.lock().unwrap().is_empty(),
                "stale local or root policy must not request another preparation"
            );
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
// For the unattended-renewal tests alone, which run only where aterm links the opener (Unix).
#[cfg(unix)]
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

// For the unattended-renewal tests alone, which run only where aterm links the opener (Unix).
#[cfg(unix)]
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

/// Unix only, where aterm links the opener an unattended renewal's page is handed to
/// (`cli::opener_link`); off Unix the sign-in ladder's
/// `an_unattended_run_is_handed_nothing_where_aterm_links_no_opener` holds what runs.
#[cfg(unix)]
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
