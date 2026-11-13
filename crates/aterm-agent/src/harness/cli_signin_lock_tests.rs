// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The tests gated `cfg(unix)` here start a manual sign-in or select a row that needs one,
// which only Unix queues (its private FIFO, `accounts::manual_signin_capability`).
#![cfg_attr(
    not(unix),
    expect(
        dead_code,
        reason = "some helpers serve only the tests gated `cfg(unix)`"
    )
)]

use std::cell::{Cell, RefCell};

use super::*;

// accounts::Lock::drop explicitly unlocks the open file description, so these
// release assertions remain immediate even if a forked child inherited its FD.
pub(crate) fn lock_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let tmp = tmp_dir("signin-lock-conformance");
    let env = env_at(&tmp);
    let a_store = env.accounts_store();
    let b_store = env.accounts_store_for(Cli::Kimi).unwrap();
    assert_eq!(a_store.shared_dir(), b_store.shared_dir());
    let mut a = (state["a"] == 1).then(|| a_store.signin_now().unwrap().unwrap());
    let mut b = (state["b"] == 1).then(|| b_store.signin_now().unwrap().unwrap());
    let mut next = state.clone();
    match action {
        "TakeA" => {
            a = a_store.signin_now().unwrap();
            next.insert("tried_a", 1);
        }
        "TakeB" => {
            b = b_store.signin_now().unwrap();
            next.insert("tried_b", 1);
        }
        "ReleaseA" => {
            drop(a.take());
        }
        "ReleaseB" => {
            drop(b.take());
        }
        "RetryA" => {
            next.insert("tried_a", 0);
        }
        "RetryB" => {
            next.insert("tried_b", 0);
        }
        _ => unreachable!(),
    }
    assert_eq!(
        a_store.signin_now().unwrap().is_some(),
        a.is_none() && b.is_none()
    );
    next.insert("a", i64::from(a.is_some()));
    next.insert("b", i64::from(b.is_some()));
    next.insert(
        "owner",
        if a.is_some() {
            1
        } else if b.is_some() {
            2
        } else {
            0
        },
    );
    next
}

#[test]
fn provider_signin_locks_conform_and_overlap_or_orphans_are_rejected() {
    let model = aterm_models_harness::account_signin_lock_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = lock_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {next:?}",
                    action.name
                );
            }
        }
    }
    let mut overlap = lock_transition(&model.init_state(), "TakeA");
    overlap.insert("b", 1);
    assert!(!model.check_invariant("Exclusive", &overlap));
    let mut orphan = model.init_state();
    orphan.insert("owner", 1);
    assert!(!model.check_invariant("NoOrphan", &orphan));
}

struct LockHost {
    peer: accounts::Store,
    login_calls: Cell<usize>,
    check_calls: Cell<usize>,
    first: Result<i32, String>,
    next: i32,
    now: i64,
    check_error: bool,
    /// The actual signer that retires after the manual queue observation.
    running: RefCell<Option<accounts::SigninTurn>>,
    /// What that sign-in leaves in the records as it ends.
    left: RefCell<Option<Box<dyn FnOnce()>>>,
    agent: bool,
    tty: bool,
    mailbox: Option<PathBuf>,
    automatic: bool,
    contended: Cell<bool>,
    positions: RefCell<Vec<accounts::SigninPosition>>,
    work: Option<crate::harness::account_work::Scope>,
    close_on_queue: Option<crate::harness::account_work::Lifetime>,
    paused: RefCell<Option<crate::harness::account_work::Pause>>,
}

impl LockHost {
    fn new(env: &Env, first: Result<i32, String>) -> Self {
        Self {
            peer: env.accounts_store_for(Cli::Kimi).unwrap(),
            login_calls: Cell::new(0),
            check_calls: Cell::new(0),
            first,
            next: 0,
            now: env.now,
            check_error: false,
            running: RefCell::new(None),
            left: RefCell::new(None),
            agent: false,
            tty: true,
            mailbox: None,
            automatic: false,
            contended: Cell::new(false),
            positions: RefCell::new(Vec::new()),
            work: None,
            close_on_queue: None,
            paused: RefCell::new(None),
        }
    }
    fn held(&self) {
        assert!(
            self.peer.signin_now().unwrap().is_none(),
            "a competing provider must stay excluded"
        );
    }
    fn login_result(&self) -> Result<i32, String> {
        self.held();
        let count = self.login_calls.get();
        self.login_calls.set(count + 1);
        if count == 0 {
            self.first.clone()
        } else {
            Ok(self.next)
        }
    }
}

impl AccountsHost for LockHost {
    fn stdin_is_terminal(&self) -> bool {
        self.tty
    }
    fn automatic_sign_in(&self) -> bool {
        self.automatic
    }
    fn work_scope(&self) -> Option<crate::harness::account_work::Scope> {
        self.work.clone()
    }
    fn work_cancelled(&self) -> bool {
        self.work
            .as_ref()
            .is_some_and(crate::harness::account_work::Scope::cancelled)
    }
    fn current_exe(&self) -> Option<PathBuf> {
        Some(std::env::current_exe().expect("the test executable"))
    }
    fn signin_queued(&self, position: accounts::SigninPosition) {
        self.positions.borrow_mut().push(position);
        let held = self.running.borrow_mut().take();
        self.contended.set(self.contended.get() || held.is_some());
        if let Some(left) = self.left.borrow_mut().take() {
            left();
        }
        drop(held);
        let unpaused = self.paused.borrow().is_none();
        if let Some(lifetime) = &self.close_on_queue
            && unpaused
        {
            *self.paused.borrow_mut() = lifetime.pause();
        }
    }
    fn machine_var(&self, name: &str) -> Option<std::ffi::OsString> {
        if name == crate::harness::open_url::MAILBOX_ENV {
            self.mailbox
                .as_ref()
                .map(|path| path.as_os_str().to_owned())
        } else {
            (self.agent && name == "CLAUDECODE").then(|| "1".into())
        }
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn now(&self) -> i64 {
        self.now
    }
    fn login(&self, dir: Option<&str>) -> Result<i32, String> {
        let result = self.login_result();
        if result == Ok(0) {
            std::fs::write(Path::new(dir.unwrap()).join(".claude.json"), b"{}").unwrap();
        }
        result
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        self.held();
        self.check_calls.set(self.check_calls.get() + 1);
        if self.check_error {
            Err("injected status failure".to_owned())
        } else {
            said(true)
        }
    }
    fn renew_flow(&self, home: &str, _: &str, _: &RenewBrowser) -> Result<i32, String> {
        let result = self.login_result();
        if result == Ok(0) {
            std::fs::write(
                Path::new(home).join(".claude.json"),
                r#"{"oauthAccount":{"emailAddress":"owner@example.test"}}"#,
            )
            .unwrap();
        }
        result
    }
    fn status_in_home(&self, _: &str) -> Result<Captured, String> {
        self.held();
        self.check_calls.set(self.check_calls.get() + 1);
        if self.check_error {
            return Err("injected status failure".to_owned());
        }
        Ok(Captured {
            stdout: br#"{"loggedIn":true,"authMethod":"claude.ai","email":"owner@example.test","subscriptionType":"max"}"#.to_vec(),
            code: Some(0),
        })
    }
}

fn fixture(tag: &str, renewal: bool) -> (Tmp, Env, AccountsCmd) {
    let tmp = tmp_dir(tag);
    let env = env_at(&tmp);
    let form = if renewal {
        std::fs::write(
            env.accounts_roster().unwrap(),
            "[[account]]\nlabel='main'\nclass='dedicated'\nemail='owner@example.test'\n",
        )
        .unwrap();
        AccountsCmd::Renew {
            labels: vec!["main".to_owned()],
            due: false,
            on: None,
        }
    } else {
        three_accounts(&tmp, &env);
        AccountsCmd::Login {
            label: "work-2".to_owned(),
        }
    };
    (tmp, env, form)
}

#[cfg(unix)]
#[test]
fn bounded_background_refusal_and_manual_provider_cleanup_share_machine_exclusion() {
    for renewal in [false, true] {
        let (tmp, env, form) = fixture("claude-signin-lock", renewal);
        let mut host = LockHost::new(&env, Ok(0));
        host.automatic = true;
        let held = host.peer.signin_now().unwrap().unwrap();
        let before = every_file(tmp.path());
        let (code, out, err) = acc(form.clone(), &env, &host);
        assert_eq!(code, exit(1), "{out}{err}");
        assert!(out.contains(accounts::SIGNIN_HELD), "{out}{err}");
        assert_eq!(host.login_calls.get(), 0);
        assert_eq!(host.check_calls.get(), 0);
        assert_eq!(
            every_file(tmp.path()),
            before,
            "busy sign-in creates no staging or credentials"
        );
        drop(held);
        host.automatic = false;
        let (code, out, err) = acc(form, &env, &host);
        assert_eq!(code, exit(0), "{out}{err}");
        assert_eq!(host.login_calls.get(), 1);
        assert_eq!(host.check_calls.get(), 1);
        assert!(
            host.peer.signin_now().unwrap().is_some(),
            "the next provider can sign in after completion"
        );
    }
}

/// An explicit manual command owns a FIFO place independently of terminal or
/// agent markers. This scheduling control starts no provider and changes no auth rules.
#[cfg(unix)]
#[test]
fn manual_fifo_waiting_is_independent_of_terminal_and_agent_markers() {
    for tty in [false, true] {
        for agent in [false, true] {
            let (_tmp, env, _) = fixture("claude-signin-person", false);
            let mut host = LockHost::new(&env, Ok(0));
            host.agent = agent;
            host.tty = tty;
            let held = host.peer.signin_now().unwrap().unwrap();
            host.running.replace(Some(held));
            let mut out = Vec::new();
            let (turn, waited) = signin_lock(&env.accounts_store(), &host, "work-2", &mut out)
                .unwrap()
                .unwrap();
            assert!(waited);
            assert_eq!(
                *host.positions.borrow(),
                [accounts::SigninPosition {
                    ahead: 0,
                    blocker: accounts::SigninBlocker::UnscheduledSignin
                }]
            );
            assert!(host.peer.signin_now().unwrap().is_none());
            assert_eq!(host.login_calls.get(), 0);
            drop(turn);
        }
    }
}

#[cfg(unix)]
#[test]
fn closing_the_actual_scope_at_queue_observation_starts_no_provider() {
    let (_tmp, env, _) = fixture("claude-signin-cancel-queued", false);
    let lifetime = crate::harness::account_work::Lifetime::default();
    let mut host = LockHost::new(&env, Ok(0));
    host.work = Some(lifetime.enter().unwrap());
    host.close_on_queue = Some(lifetime.clone());
    host.running.replace(host.peer.signin_now().unwrap());
    let mut out = Vec::new();
    let mut err = Vec::new();
    assert!(matches!(
        signin_guard(
            &env.accounts_store(),
            &host,
            "work-2",
            "login",
            &mut out,
            &mut err
        ),
        Err(Renewal::Deferred)
    ));
    assert_eq!(host.positions.borrow().len(), 1);
    assert_eq!(host.login_calls.get(), 0);
    assert_eq!(host.check_calls.get(), 0);
    assert!(host.peer.signin_now().unwrap().is_some());
    let paused = host.paused.borrow_mut().take().unwrap();
    drop(host.work.take());
    assert!(paused.try_wait().is_ok());
}

/// What a person's wait outlasted is what they go by: the sign-in they waited for renewed another
/// row, which moves the store's clock and so every parent captured before it, and theirs still
/// runs. Without the capture under the lock it is refused with nothing run.
#[cfg(unix)]
#[test]
fn a_person_goes_by_what_the_sign_in_they_waited_for_left() {
    for renewal in [false, true] {
        let (_tmp, env, form) = fixture("claude-signin-moved", renewal);
        let host = LockHost::new(&env, Ok(0));
        let held = host.peer.signin_now().unwrap().unwrap();
        host.running.replace(Some(held));
        let (store, now) = (env.accounts_store(), env.now);
        host.left.replace(Some(Box::new(move || {
            let (mut logins, _) = store.load_logins(now).unwrap();
            logins.generation_clock += 1;
            store.save_logins(&logins).unwrap();
        })));
        let (code, out, err) = acc(form, &env, &host);
        assert_eq!(code, exit(0), "{out}{err}");
        assert_eq!(host.login_calls.get(), 1, "{out}{err}");
    }
}

/// What a person waited for may be the very sign-in they came to make, so nothing runs a second
/// time. The test above is its control: a renewal of another row runs theirs.
#[cfg(unix)]
#[test]
fn a_person_is_not_signed_in_again_over_a_renewal_they_waited_for() {
    for renewal in [false, true] {
        let (_tmp, env, form) = fixture("claude-signin-renewed", renewal);
        let host = LockHost::new(&env, Ok(0));
        let held = host.peer.signin_now().unwrap().unwrap();
        host.running.replace(Some(held));
        let label = if renewal { "main" } else { "work-2" };
        let (store, now) = (env.accounts_store(), env.now);
        let dir = store.login_dir(label).unwrap().to_str().unwrap().to_owned();
        host.left.replace(Some(Box::new(move || {
            store
                .transact_logins(now, &mut None, |state, logins| {
                    Ok(accounts::note_renewed(
                        state, logins, label, None, &dir, None, now,
                    ))
                })
                .unwrap();
        })));
        let (code, out, err) = acc(form, &env, &host);
        assert_eq!(code, exit(0), "{out}{err}");
        assert_eq!(host.login_calls.get(), 0, "{out}{err}");
        assert!(out.contains(RENEWED_WHILE_WAITING), "{out}{err}");
    }
}

#[cfg(unix)]
#[test]
fn claude_signin_failures_release_the_machine_lock() {
    for renewal in [false, true] {
        for ended in [Ok(1), Ok(130), Err("injected launch failure".to_owned())] {
            let (_tmp, env, form) = fixture("claude-signin-unwind", renewal);
            let host = LockHost::new(&env, ended);
            let (code, _, _) = acc(form, &env, &host);
            assert_eq!(code, exit(1));
            assert_eq!(host.login_calls.get(), 1);
            assert_eq!(host.check_calls.get(), 0);
            assert!(
                host.peer.signin_now().unwrap().is_some(),
                "errors cannot strand sign-in ownership"
            );
        }
        let (_tmp, env, form) = fixture("claude-signin-check-unwind", renewal);
        let mut host = LockHost::new(&env, Ok(0));
        host.check_error = true;
        let (code, _, _) = acc(form, &env, &host);
        assert_eq!(code, exit(1));
        assert_eq!(host.check_calls.get(), 1);
        assert!(host.peer.signin_now().unwrap().is_some());
    }
}

#[cfg(unix)]
#[test]
fn an_unopenable_signin_lock_starts_no_claude_flow() {
    for renewal in [false, true] {
        let (tmp, env, form) = fixture("claude-signin-lock-io", renewal);
        let store = env.accounts_store();
        store.ensure().unwrap();
        let lock = store.shared_dir().join(accounts::SIGNIN_LOCK_FILE);
        std::fs::create_dir(&lock).unwrap();
        let before = every_file(tmp.path());
        let host = LockHost::new(&env, Ok(0));
        let (code, _, _) = acc(form, &env, &host);
        assert_eq!(code, exit(1));
        assert_eq!(host.login_calls.get(), 0);
        assert_eq!(host.check_calls.get(), 0);
        // Normal queue-aware admission can prune the released ticket, then
        // still reports the actual malformed signer instead of contention.
        assert!(store.signin_now().is_err());
        let gate = store.shared_dir().join("signin-queue/queue.lock");
        let mut after = every_file(tmp.path());
        assert_eq!(
            after
                .iter()
                .find(|(path, _)| path == &gate)
                .map(|(_, bytes)| bytes.as_slice()),
            Some([].as_slice())
        );
        after.retain(|(path, _)| path != &gate);
        assert_eq!(
            after, before,
            "only the empty scheduling gate may be created"
        );
        assert!(lock.is_dir());
    }
}

#[cfg(unix)]
struct Published<'a> {
    host: &'a LockHost,
    store: accounts::Store,
    seen: bool,
    bytes: Vec<u8>,
}

#[cfg(unix)]
impl Write for Published<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes.extend_from_slice(bytes);
        if !self.seen && String::from_utf8_lossy(&self.bytes).contains("main: renewed;") {
            self.host.held();
            assert!(
                self.store
                    .load_logins(self.host.now)
                    .unwrap()
                    .0
                    .get("main")
                    .is_some()
            );
            self.seen = true;
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(unix)]
#[test]
fn a_claude_fallback_keeps_the_lock_until_the_new_binding_is_published() {
    let (_tmp, env, form) = fixture("claude-fallback-lock", true);
    let store = env.accounts_store();
    let browser =
        env.home.as_ref().unwrap().join(
            ".cache/aterm/browser.noindex/chrome-headless-shell-fixture/chrome-headless-shell",
        );
    std::fs::create_dir_all(browser.parent().unwrap()).unwrap();
    std::fs::write(browser, "inert browser fixture").unwrap();
    let profile = crate::harness::headless_oauth::browser_profile_dir(&store.dir, Some("main"));
    std::fs::create_dir_all(profile).unwrap();
    let host = LockHost::new(&env, Ok(1));
    let mut out = Published {
        host: &host,
        store,
        seen: false,
        bytes: Vec::new(),
    };
    let mut err = Vec::new();
    assert_eq!(
        run_accounts(&form, &env, &host, &mut out, &mut err),
        ExitCode::SUCCESS,
        "{}",
        String::from_utf8_lossy(&err)
    );
    assert_eq!(host.login_calls.get(), 2);
    assert_eq!(host.check_calls.get(), 1);
    assert!(
        out.seen,
        "the binding was published while holding the guard"
    );
    assert!(host.peer.signin_now().unwrap().is_some());
}

/// A non-TTY caller can acquire inside the existing bounded ladder after contention.
/// It must observe the completed renewal instead of treating its old capture as current.
#[cfg(unix)]
#[test]
fn a_non_tty_sign_in_rechecks_the_renewal_that_ended_its_contention() {
    for contended in [false, true] {
        let (tmp, env, form) = fixture("signin-non-tty-contention", true);
        let mut host = LockHost::new(&env, Ok(0));
        host.tty = false;
        let mailbox = tmp.path().join("synthetic-mailbox");
        std::fs::create_dir_all(&mailbox).unwrap();
        host.mailbox = Some(mailbox);
        if contended {
            host.running
                .replace(Some(host.peer.signin_now().unwrap().unwrap()));
            let store = env.accounts_store();
            let now = env.now;
            let dir = store
                .login_dir("main")
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            host.left.replace(Some(Box::new(move || {
                store
                    .transact_logins(now, &mut None, |state, logins| {
                        Ok(accounts::note_renewed(
                            state, logins, "main", None, &dir, None, now,
                        ))
                    })
                    .unwrap();
            })));
        }
        let (code, out, err) = acc(form, &env, &host);
        assert_eq!(
            host.contended.get(),
            contended,
            "the completion seam must follow a real failed lock take"
        );
        assert_eq!(code, exit(0), "{out}{err}");
        assert_eq!(out.contains(RENEWED_WHILE_WAITING), contended, "{out}{err}");
        assert_eq!(
            host.login_calls.get(),
            usize::from(!contended),
            "stale work must not start an external flow"
        );
        assert_eq!(host.check_calls.get(), usize::from(!contended));
        assert!(
            host.peer.signin_now().unwrap().is_some(),
            "the machine lock is released"
        );
    }
}
