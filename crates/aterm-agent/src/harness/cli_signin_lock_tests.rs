// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::{Cell, RefCell};

use super::*;

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
    /// The sign-in that holds the lock, which ends at this host's first answer on retrying.
    running: RefCell<Option<accounts::Lock>>,
    /// What that sign-in leaves in the records as it ends.
    left: RefCell<Option<Box<dyn FnOnce()>>>,
    agent: bool,
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
        true
    }
    fn retries_sign_in_lock(&self, _: u32) -> bool {
        let ended = self.running.borrow_mut().take().is_some();
        if let Some(left) = self.left.borrow_mut().take() {
            left();
        }
        ended
    }
    fn machine_var(&self, name: &str) -> Option<std::ffi::OsString> {
        (self.agent && name == "CLAUDECODE").then(|| "1".into())
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

#[test]
fn claude_login_and_renewal_wait_for_other_providers_and_hold_through_checks() {
    for renewal in [false, true] {
        let (tmp, env, form) = fixture("claude-signin-lock", renewal);
        let host = LockHost::new(&env, Ok(0));
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

/// A person's own sign-in waits for the one that holds the lock, told so, since that one ends by
/// itself and they have just asked. NEGATIVE CONTROL: an agent's terminal is refused at once, by
/// the same lock, with the same host willing to retry.
#[test]
fn a_person_waits_out_the_sign_in_that_holds_the_lock() {
    for renewal in [false, true] {
        for agent in [false, true] {
            let (_tmp, env, form) = fixture("claude-signin-person", renewal);
            let mut host = LockHost::new(&env, Ok(0));
            host.agent = agent;
            let held = host.peer.signin_now().unwrap().unwrap();
            host.running.replace(Some(held));
            let (code, out, err) = acc(form, &env, &host);
            assert_eq!(code, exit(u8::from(agent)), "{out}{err}");
            assert_eq!(out.contains("waiting for it to end"), !agent, "{out}{err}");
            assert_eq!(out.contains(": held: "), agent, "{out}{err}");
            assert_eq!(host.login_calls.get(), usize::from(!agent));
        }
    }
}

/// What a person's wait outlasted is what they go by: the sign-in they waited for renewed another
/// row, which moves the store's clock and so every parent captured before it, and theirs still
/// runs. Without the capture under the lock it is refused with nothing run.
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

/// The wait outlasts the longest sign-in that can hold the lock, which is another CLI's and not
/// Claude Code's: the lock is one for every CLI ([`accounts::Store::shared_dir`]).
#[test]
fn a_persons_wait_outlasts_every_cli_s_sign_in() {
    let waits = u64::from(PERSON_LOCK_TRIES) * LOCK_TRY_S;
    assert!(waits > UNATTENDED_RENEWAL_BUDGET.as_secs());
    assert!(waits > crate::harness::login_drive::HANG.as_secs());
}

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
        assert_eq!(every_file(tmp.path()), before);
        assert!(lock.is_dir());
    }
}

struct Published<'a> {
    host: &'a LockHost,
    store: accounts::Store,
    seen: bool,
    bytes: Vec<u8>,
}

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
