// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

pub(in crate::harness) fn environment(
    root: &Path,
    home: &Path,
    name: &str,
) -> super::super::cli::Env {
    let config = root.join(format!("{name}.toml"));
    std::fs::write(&config, "").unwrap();
    super::super::cli::Env {
        state: root.join(name),
        aterm_state: Some(home.join(".aterm")),
        cwd: home.into(),
        home: Some(home.into()),
        claude_dir: None,
        claude_config_unusable: None,
        claude_profile: None,
        claude_custom_oauth: false,
        claude_session: None,
        claude_pid: None,
        codex_thread: None,
        codex_home: None,
        xdg_config_home: None,
        codex_login: None,
        agy_conversation: None,
        now: 1_800_000_000,
        sid: "s-native-admission-fixture".into(),
        sock: None,
        config: Some(config),
        claude_tmp: None,
        claude_running: None,
        tz_s: 0,
    }
}
pub(in crate::harness) fn reservation(
    env: &super::super::cli::Env,
    label: &str,
    operation: &str,
) -> Arc<NativeBrowserTurn> {
    reservation_using(env, label, operation, false)
}
/// Explicit historical phase fixture: no global catalogue authority.
pub(in crate::harness) fn historical_reservation(
    env: &super::super::cli::Env,
    label: &str,
    operation: &str,
) -> Arc<NativeBrowserTurn> {
    reservation_using(env, label, operation, true)
}
fn reservation_using(
    env: &super::super::cli::Env,
    label: &str,
    operation: &str,
    historical: bool,
) -> Arc<NativeBrowserTurn> {
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let request = codex_enrollment::Request {
        id: operation.repeat(32),
        caller: "d".repeat(64),
        label: label.into(),
        revision: "e".repeat(64),
    };
    store
        .transact_logins(env.now, &mut None, |_, logins| {
            if historical {
                logins
                    .enrollment
                    .admit_historical_native_fixture(&request)?;
            } else {
                logins.enrollment.admit_native(&request)?;
            }
            Ok(Vec::new())
        })
        .unwrap();
    let roster = accounts::Roster::parse_for(
        accounts::Cli::Codex,
        &format!(
            "[[account]]\nlabel='{label}'\nclass='dedicated'\ndir='{}'\n",
            env.state.join("login").display()
        ),
    )
    .unwrap();
    let turn =
        NativeBrowserTurn::acquire_isolated_fixture(&store, &request, &roster.rows[0], env.now)
            .unwrap();
    turn.bind_source_home(env.home.as_deref().unwrap()).unwrap();
    turn
}
pub(in crate::harness) fn callback_test_turn() -> super::CallbackTestTurn {
    super::callback_test_turn().unwrap()
}

#[test]
fn native_exchange_is_shared_across_state_roots_in_one_machine_home() {
    let _callback_turn = callback_test_turn();
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let other_home = root.path().join("other-home");
    platform::ensure_private_dir(&home).unwrap();
    platform::ensure_private_dir(&other_home).unwrap();
    let a = environment(root.path(), &home, "state-a");
    let b = environment(root.path(), &home, "state-b");
    let other = environment(root.path(), &other_home, "state-c");
    assert_ne!(a.config, b.config);
    assert_ne!(
        a.accounts_store_for(accounts::Cli::Codex)
            .unwrap()
            .shared_dir(),
        b.accounts_store_for(accounts::Cli::Codex)
            .unwrap()
            .shared_dir()
    );
    let first = reservation(&a, "first", "a");
    let second = reservation(&b, "second", "b");
    first
        .enter_exchange(
            super::super::wake_deadline::WakeDeadline::after(std::time::Duration::from_secs(1)),
            None,
        )
        .unwrap();
    // An existing-source inspection does not reacquire shared S, even when
    // the existing holder is exclusive. It is metadata, not source authority.
    let other_source =
        super::super::codex_source_gate::PublicationLock::for_home(&other_home).unwrap();
    let isolated = RefreshExchangeLease::for_env(&other).unwrap();
    isolated.current().unwrap();
    assert!(RefreshExchangeLease::for_env(&a).is_err());
    assert!(RefreshExchangeLease::for_env(&b).is_err());
    assert!(
        second
            .enter_exchange(
                super::super::wake_deadline::WakeDeadline::after(std::time::Duration::from_millis(
                    10
                )),
                None
            )
            .is_err()
    );
    first.check_exchange().unwrap();
    drop(first);
    second
        .enter_exchange(
            super::super::wake_deadline::WakeDeadline::after(std::time::Duration::from_secs(1)),
            None,
        )
        .unwrap();
    assert!(RefreshExchangeLease::for_env(&a).is_err());
    drop(second);
    let shared_source = super::super::codex_source_gate::SourceWorkLease::for_home(&home).unwrap();
    let refresh_a = RefreshExchangeLease::for_env(&a).unwrap();
    let refresh_b = RefreshExchangeLease::for_env(&b).unwrap();
    refresh_a.current().unwrap();
    refresh_b.current().unwrap();
    assert!(
        a.accounts_store_for(accounts::Cli::Codex)
            .unwrap()
            .signin_now()
            .unwrap()
            .is_none()
    );
    assert!(
        b.accounts_store_for(accounts::Cli::Codex)
            .unwrap()
            .signin_now()
            .unwrap()
            .is_none()
    );
    let mut missing_home = a.clone();
    missing_home.home = None;
    assert!(RefreshExchangeLease::for_env(&missing_home).is_err());
    drop((refresh_a, refresh_b, shared_source, isolated, other_source));
}

#[test]
fn actual_native_factory_owns_two_registered_callbacks_and_preserves_legacy_exclusion() {
    let _callback_turn = callback_test_turn();
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    platform::ensure_private_dir(&home).unwrap();
    let store = accounts::Store::at_for(&root.path().join("state"), accounts::Cli::Codex, &home);
    let roster = accounts::Roster::parse_for(accounts::Cli::Codex, &format!(
        "[[account]]\nlabel='a'\nclass='dedicated'\ndir='{}'\n[[account]]\nlabel='b'\nclass='dedicated'\ndir='{}'\n[[account]]\nlabel='c'\nclass='dedicated'\ndir='{}'\n",
        root.path().join("login-a").display(), root.path().join("login-b").display(), root.path().join("login-c").display()
    )).unwrap();
    let requests: Vec<_> = ["a", "b", "c"]
        .into_iter()
        .map(|label| codex_enrollment::Request {
            id: label.repeat(32),
            caller: "d".repeat(64),
            label: label.into(),
            revision: "e".repeat(64),
        })
        .collect();
    for request in &requests {
        store
            .transact_logins(1_800_000_000, &mut None, |_, logins| {
                logins.enrollment.admit_native(request)?;
                Ok(Vec::new())
            })
            .unwrap();
    }
    // These are the ACTUAL production constructor and the two registered
    // callback endpoints. No browser or provider request is started.
    let a =
        NativeBrowserTurn::acquire(&store, &requests[0], &roster.rows[0], 1_800_000_000).unwrap();
    let b =
        NativeBrowserTurn::acquire(&store, &requests[1], &roster.rows[1], 1_800_000_000).unwrap();
    assert_ne!(a.profile, b.profile);
    let callback = a.callback().unwrap();
    assert_ne!(
        callback.local_addr().unwrap(),
        b.callback().unwrap().local_addr().unwrap()
    );
    assert!(store.signin_now().unwrap().is_none());
    assert!(
        NativeBrowserTurn::acquire(&store, &requests[2], &roster.rows[2], 1_800_000_000).is_err()
    );
    drop(a);
    // The callback worker's real duplicated listener outlives its original
    // constructor, so its endpoint remains unavailable until that worker retires.
    assert!(
        NativeBrowserTurn::acquire(&store, &requests[2], &roster.rows[2], 1_800_000_000).is_err()
    );
    drop(callback);
    let c =
        NativeBrowserTurn::acquire(&store, &requests[2], &roster.rows[2], 1_800_000_000).unwrap();
    assert!(store.signin_now().unwrap().is_none());
    drop((b, c));
    let legacy = store.signin_now().unwrap().unwrap();
    assert!(
        NativeBrowserTurn::acquire(&store, &requests[0], &roster.rows[0], 1_800_000_000).is_err()
    );
    drop(legacy);
    NativeBrowserTurn::acquire(&store, &requests[0], &roster.rows[0], 1_800_000_000).unwrap();
}

fn transition(state: &aterm_spec::interp::State, action: &str) -> aterm_spec::interp::State {
    let root = aterm_tempfile::tempdir().unwrap();
    let directory = root.path().join("private");
    let open = |name| Held::open(&directory, name).unwrap();
    let mut legacy =
        (state["legacy"] == 1).then(|| open(accounts::SIGNIN_LOCK_FILE).exclusive().unwrap());
    let mut a =
        (state["browser_a"] == 1).then(|| open(accounts::SIGNIN_LOCK_FILE).shared().unwrap());
    let mut b =
        (state["browser_b"] == 1).then(|| open(accounts::SIGNIN_LOCK_FILE).shared().unwrap());
    let mut post = (state["post"] == 1).then(|| open(EXCHANGE).exclusive().unwrap());
    let mut ra = (state["refresh_a"] == 1).then(|| open(EXCHANGE).shared().unwrap());
    let mut rb = (state["refresh_b"] == 1).then(|| open(EXCHANGE).shared().unwrap());
    match action {
        "BrowserA" => a = open(accounts::SIGNIN_LOCK_FILE).shared().ok(),
        "BrowserB" => b = open(accounts::SIGNIN_LOCK_FILE).shared().ok(),
        "Legacy" => legacy = open(accounts::SIGNIN_LOCK_FILE).exclusive().ok(),
        "Post" => post = open(EXCHANGE).exclusive().ok(),
        "RefreshA" => ra = open(EXCHANGE).shared().ok(),
        "RefreshB" => rb = open(EXCHANGE).shared().ok(),
        "EndBrowserA" => drop(a.take()),
        "EndBrowserB" => drop(b.take()),
        "EndLegacy" => drop(legacy.take()),
        "EndPost" => drop(post.take()),
        "EndRefreshA" => drop(ra.take()),
        "EndRefreshB" => drop(rb.take()),
        _ => unreachable!(),
    }
    let mut next = state.clone();
    for (name, held) in [
        ("browser_a", a.is_some()),
        ("browser_b", b.is_some()),
        ("legacy", legacy.is_some()),
        ("post", post.is_some()),
        ("refresh_a", ra.is_some()),
        ("refresh_b", rb.is_some()),
    ] {
        next.insert(name, i64::from(held));
    }
    next
}

#[test]
fn actual_kernel_roles_refine_the_native_signin_phase_model() {
    let model = aterm_models_harness::native_signin_phase_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let states = aterm_spec::interp::reachable_states(&model);
    assert!(
        states
            .iter()
            .any(|state| state["browser_a"] == 1 && state["browser_b"] == 1)
    );
    assert!(
        states
            .iter()
            .any(|state| state["refresh_a"] == 1 && state["refresh_b"] == 1)
    );
    for state in states {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {next:?}",
                    action.name
                );
            }
        }
    }
    let mut bad = model.init_state();
    bad.insert("post", 1);
    bad.insert("browser_a", 1);
    bad.insert("refresh_a", 1);
    assert!(!model.check_invariant("UnknownPostExclusive", &bad));
    bad = model.init_state();
    bad.insert("legacy", 1);
    bad.insert("browser_a", 1);
    assert!(!model.check_invariant("LegacyExclusive", &bad));
}

#[test]
fn publication_waits_for_source_without_spending_another_exchange() {
    // Kernel admission only: this fixture grants no credential authority.
    let home = aterm_tempfile::tempdir().unwrap();
    let held_source =
        super::super::codex_source_gate::SourceWorkLease::for_home(home.path()).unwrap();
    let lifetime = super::super::account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let path = home.path().to_path_buf();
    let (entered, waiting) = std::sync::mpsc::channel();
    let (done, result) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let once = std::sync::atomic::AtomicBool::new(false);
        done.send(PublicationTurn::acquire_within(
            &path,
            &["principal-fixture".into()],
            &scope,
            super::super::wake_deadline::WakeDeadline::after(std::time::Duration::from_secs(60)),
            || {
                if !once.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    entered.send(()).unwrap();
                }
                Ok(())
            },
        ))
        .unwrap();
    });
    waiting
        .recv_timeout(std::time::Duration::from_secs(60))
        .unwrap();
    assert!(matches!(
        result.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    drop(held_source);
    let publication = result
        .recv_timeout(std::time::Duration::from_secs(60))
        .unwrap()
        .unwrap();
    publication._source.current().unwrap();
    worker.join().unwrap();
    drop(publication);
}

#[test]
fn cancelled_publication_releases_partial_resources_and_unavailable_is_not_busy() {
    let home = aterm_tempfile::tempdir().unwrap();
    let source = super::super::codex_source_gate::SourceWorkLease::for_home(home.path()).unwrap();
    let directory = source
        .namespace()
        .unwrap()
        .directory()
        .unwrap()
        .to_path_buf();
    drop(source);
    let resources = directory.join("native-publication-resources");
    let blocked = Held::open(&resources, "b").unwrap().exclusive().unwrap();
    let lifetime = super::super::account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let cancel = scope.cancellation();
    let path = home.path().to_path_buf();
    let (entered, waiting) = std::sync::mpsc::channel();
    let (done, result) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let once = std::sync::atomic::AtomicBool::new(false);
        done.send(PublicationTurn::acquire_within(
            &path,
            &["a".into(), "b".into()],
            &scope,
            super::super::wake_deadline::WakeDeadline::after(std::time::Duration::from_secs(60)),
            || {
                if !once.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    entered.send(()).unwrap();
                }
                Ok(())
            },
        ))
        .unwrap();
    });
    waiting
        .recv_timeout(std::time::Duration::from_secs(60))
        .unwrap();
    cancel.cancel();
    assert!(
        result
            .recv_timeout(std::time::Duration::from_secs(60))
            .unwrap()
            .is_err()
    );
    worker.join().unwrap();
    let source = super::super::codex_source_gate::SourceWorkLease::for_home(home.path()).unwrap();
    let exchange = Held::open(&directory, EXCHANGE)
        .unwrap()
        .exclusive()
        .unwrap();
    let first = Held::open(&resources, "a").unwrap().exclusive().unwrap();
    drop((source, exchange, first, blocked));
    let scope = lifetime.enter().unwrap();
    let attempts = std::cell::Cell::new(0);
    assert!(
        PublicationTurn::acquire_within(
            Path::new("relative-native-home"),
            &[],
            &scope,
            super::super::wake_deadline::WakeDeadline::after(std::time::Duration::from_secs(60)),
            || {
                attempts.set(attempts.get() + 1);
                Ok(())
            },
        )
        .is_err()
    );
    assert_eq!(attempts.get(), 1, "unavailable namespace must not retry");
    assert!(
        PublicationTurn::acquire_within(
            home.path(),
            &[],
            &scope,
            super::super::wake_deadline::WakeDeadline::after(std::time::Duration::ZERO),
            || panic!("expired admission must not try kernel custody"),
        )
        .is_err()
    );
}

#[test]
fn browser_overlap_is_separate_from_legacy_and_unknown_post_exclusion() {
    let root = aterm_tempfile::tempdir().unwrap();
    let directory = root.path().join("private");
    let a = Held::open(&directory, accounts::SIGNIN_LOCK_FILE)
        .unwrap()
        .shared()
        .unwrap();
    let b = Held::open(&directory, accounts::SIGNIN_LOCK_FILE)
        .unwrap()
        .shared()
        .unwrap();
    assert!(
        Held::open(&directory, accounts::SIGNIN_LOCK_FILE)
            .unwrap()
            .exclusive()
            .is_err()
    );
    let post = Held::open(&directory, EXCHANGE)
        .unwrap()
        .exclusive()
        .unwrap();
    assert!(
        Held::open(&directory, EXCHANGE)
            .unwrap()
            .exclusive()
            .is_err()
    );
    assert!(Held::open(&directory, EXCHANGE).unwrap().shared().is_err());
    a.current().unwrap();
    b.current().unwrap();
    drop(post);
    let refresh_a = Held::open(&directory, EXCHANGE).unwrap().shared().unwrap();
    let refresh_b = Held::open(&directory, EXCHANGE).unwrap().shared().unwrap();
    assert!(
        Held::open(&directory, EXCHANGE)
            .unwrap()
            .exclusive()
            .is_err()
    );
    drop((refresh_a, refresh_b, a, b));
    let legacy = Held::open(&directory, accounts::SIGNIN_LOCK_FILE)
        .unwrap()
        .exclusive()
        .unwrap();
    assert!(
        Held::open(&directory, accounts::SIGNIN_LOCK_FILE)
            .unwrap()
            .shared()
            .is_err()
    );
    drop(legacy);
    Held::open(&directory, accounts::SIGNIN_LOCK_FILE)
        .unwrap()
        .shared()
        .unwrap();
}

#[test]
fn actual_profile_and_callback_resources_remain_exclusive_until_retirement() {
    let root = aterm_tempfile::tempdir().unwrap();
    let a = root.path().join("a");
    let b = root.path().join("b");
    platform::ensure_private_dir(&a).unwrap();
    platform::ensure_private_dir(&b).unwrap();
    let profile_a = browser_window::ProfileLock::claim(&a).unwrap();
    let profile_b = browser_window::ProfileLock::claim(&b).unwrap();
    assert!(browser_window::ProfileLock::claim(&a).is_err());
    assert!(profile_a.current(&a));
    assert!(profile_b.current(&b));
    // Kernel ownership and transfer are tested without consuming the real
    // machine's registered callback endpoints. Live acquire uses ONLY them.
    let callback = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = callback.local_addr().unwrap();
    let worker = callback.try_clone().unwrap();
    drop(callback);
    assert!(TcpListener::bind(address).is_err());
    drop(worker);
    TcpListener::bind(address).unwrap();
    drop(profile_a);
    browser_window::ProfileLock::claim(&a).unwrap();
}

#[test]
fn reopened_permission_check_cannot_rebind_a_held_gate_during_rename() {
    let root = aterm_tempfile::tempdir().unwrap();
    let directory = root.path().join("private");
    let original = Held::open(&directory, EXCHANGE).unwrap().shared().unwrap();
    // Exact race cut: the initial held-FD/path check succeeds; then the
    // old single-link file is renamed before the permission reopen.
    assert!(astream_sys::file_matches_path(&original.file, &original.path).unwrap());
    std::fs::rename(&original.path, directory.join("old-gate")).unwrap();
    let replacement = Held::open(&directory, EXCHANGE).unwrap().shared().unwrap();
    let checked = aterm_uds::private_file::open_private_rw(&directory, EXCHANGE).unwrap();
    assert!(astream_sys::file_matches_path(&checked, &original.path).unwrap());
    assert!(
        original.matches_checked(&checked).is_err(),
        "both path checks succeeding cannot substitute a different descriptor"
    );
    assert!(original.current().is_err());
    replacement.matches_checked(&checked).unwrap();
    replacement.current().unwrap();
}

#[test]
fn replacement_and_aliases_invalidate_existing_exchange_admission() {
    let root = aterm_tempfile::tempdir().unwrap();
    let directory = root.path().join("private");
    let held = Held::open(&directory, EXCHANGE).unwrap().shared().unwrap();
    std::fs::rename(directory.join(EXCHANGE), directory.join("old")).unwrap();
    let replacement = Held::open(&directory, EXCHANGE).unwrap().shared().unwrap();
    assert!(held.current().is_err());
    replacement.current().unwrap();
    #[cfg(unix)]
    {
        std::fs::hard_link(directory.join(EXCHANGE), directory.join("alias")).unwrap();
        assert!(replacement.current().is_err());
        assert!(Held::open(&directory, EXCHANGE).is_err());
    }
}

#[test]
fn callback_fixtures_queue_until_the_holder_releases_custody() {
    let directory = aterm_tempfile::tempdir().unwrap();
    let holder = super::callback_test_turn_at(directory.path(), || {
        panic!("the first fixture must acquire free custody")
    })
    .unwrap();
    let (queued_tx, queued_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let path = directory.path().to_owned();
    let waiter = std::thread::spawn(move || {
        super::callback_test_turn_at(&path, || {
            queued_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        })
    });
    queued_rx
        .recv()
        .expect("a contender must wait for held custody");
    drop(holder);
    release_tx.send(()).unwrap();
    drop(waiter.join().unwrap().unwrap());
    super::callback_test_turn_at(directory.path(), || {
        panic!("the completed fixture must release custody")
    })
    .unwrap();
}
