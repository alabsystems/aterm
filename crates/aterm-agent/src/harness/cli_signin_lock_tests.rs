// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

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
