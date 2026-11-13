// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn no_route_backoff_has_a_deadline_without_an_active_worker() {
    let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
    let store = accounts::Store::at(Path::new("/synthetic/activity-a"));
    let clock = Instant::now();
    {
        let mut state = guard.lock().unwrap();
        state.ladder.failed("row", "no-route", clock);
        state.ladder_owners.insert(
            "row".into(),
            LoginOwner::new(&store, Cli::Claude, "row", 900),
        );
    }
    let rows = observed(&guard, &store, Cli::Claude, clock);
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].active);
    assert_eq!(rows[0].route, None);
    assert_eq!(rows[0].next_at, Some(1900));
    // The same changed-input path that admits a fresh route clears attribution.
    {
        let mut state = guard.lock().unwrap();
        assert!(!state.ladder.held("row", "new-mailbox", clock));
        state.ladder_owners.remove("row");
    }
    assert!(observed(&guard, &store, Cli::Claude, clock).is_empty());
}

fn observed(
    guard: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    store: &accounts::Store,
    cli: Cli,
    clock: Instant,
) -> Vec<LoginActivity> {
    login_activity_of(&guard.lock().unwrap(), store, cli, 1000, clock)
}

#[test]
fn queued_and_failed_spawns_are_not_active_routes() {
    let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
    let store = accounts::Store::at(Path::new("/synthetic/activity-a"));
    let clock = Instant::now();
    let permit = AutoRenewPermit::claim(&guard, "row", clock, false).unwrap();
    permit.attribute(
        &store,
        Cli::Claude,
        "row",
        900,
        &accounts::Logins::default(),
    );
    let queued = std::sync::Arc::new(std::sync::Mutex::new(None));
    let receiver = queued.clone();
    let worker_guard = guard.clone();
    start_auto_renew(
        permit,
        move || {
            let store = accounts::Store::at(Path::new("/synthetic/activity-a"));
            let _activity = LoginActivityGuard::begin(
                &worker_guard,
                &store,
                Cli::Claude,
                "row",
                Some("profile"),
            );
            let rows = observed(&worker_guard, &store, Cli::Claude, clock);
            assert!(rows[0].active);
            assert_eq!(rows[0].route.as_deref(), Some("profile"));
        },
        move |work| {
            *receiver.lock().unwrap() = Some(work);
            Ok(())
        },
    )
    .unwrap();
    assert!(!observed(&guard, &store, Cli::Claude, clock)[0].active);
    let work = queued.lock().unwrap().take().unwrap();
    work();
    let rows = observed(&guard, &store, Cli::Claude, clock);
    assert!(!rows[0].active);
    assert_eq!(rows[0].route, None);
    assert_eq!(rows[0].next_at, Some(1900));
    let permit = AutoRenewPermit::claim(&guard, "row", clock, true).unwrap();
    permit.attribute(
        &store,
        Cli::Claude,
        "row",
        901,
        &accounts::Logins::default(),
    );
    assert!(
        start_auto_renew(
            permit,
            || panic!("failed spawn ran work"),
            |_| Err(io::Error::other("spawn failed"))
        )
        .is_err()
    );
    assert!(!observed(&guard, &store, Cli::Claude, clock)[0].active);
    assert!(!guard.lock().unwrap().in_flight);
}

#[test]
fn activity_is_scoped_to_the_attributed_store_and_cli_and_expires() {
    let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
    let a = accounts::Store::at(Path::new("/synthetic/activity-a"));
    let b = accounts::Store::at(Path::new("/synthetic/activity-b"));
    let clock = Instant::now();
    let permit = AutoRenewPermit::claim(&guard, "row", clock, false).unwrap();
    permit.attribute(&a, Cli::Claude, "row", 900, &accounts::Logins::default());
    let active = LoginActivityGuard::begin(&guard, &a, Cli::Claude, "row", None);
    assert!(observed(&guard, &a, Cli::Claude, clock)[0].active);
    assert!(observed(&guard, &a, Cli::Grok, clock).is_empty());
    assert!(observed(&guard, &b, Cli::Claude, clock).is_empty());
    drop(active);
    drop(permit);
    let permit = AutoRenewPermit::claim(&guard, "row", clock, true).unwrap();
    permit.attribute(&b, Cli::Claude, "row", 901, &accounts::Logins::default());
    assert!(observed(&guard, &a, Cli::Claude, clock).is_empty());
    assert_eq!(observed(&guard, &b, Cli::Claude, clock).len(), 1);
    assert!(observed(&guard, &b, Cli::Claude, clock + Duration::from_secs(901)).is_empty());
}

std::thread_local! {
    static HEARD: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

fn hear_activity() {
    let _ = watch_login_activity(Box::new(|| HEARD.with(|n| n.set(n.get() + 1))));
}

#[test]
fn published_login_clears_only_its_attributed_holds() {
    hear_activity();
    let store = accounts::Store::at(Path::new("/synthetic/publication-a"));
    let other = accounts::Store::at(Path::new("/synthetic/publication-b"));
    let owner = |scope| match scope {
        "local" => Some(LoginOwner::new(&store, Cli::Claude, "row", 900)),
        "store" => Some(LoginOwner::new(&other, Cli::Claude, "row", 900)),
        "cli" => Some(LoginOwner::new(&store, Cli::Codex, "row", 900)),
        "label" => Some(LoginOwner::new(&store, Cli::Claude, "other", 900)),
        "unattributed" => None,
        _ => unreachable!(),
    };
    for activity in ["local", "store", "cli", "label", "unattributed"] {
        for ladder in ["local", "store", "cli", "label", "unattributed"] {
            let clock = Instant::now();
            let token = std::sync::Arc::new(());
            let mut state = AutoRenewGuard {
                in_flight: true,
                ..AutoRenewGuard::default()
            };
            // Keys need not equal the account label; each domain has its own owner.
            state.last_attempt.insert("key".into(), clock);
            state.parked.insert("key".into(), clock);
            state.bot_checked.insert("key".into(), clock);
            state.ladder.failed("key", "inputs", clock);
            if let Some(owner) = owner(activity) {
                state.activity_owners.insert("key".into(), owner);
            }
            if let Some(owner) = owner(ladder) {
                state.ladder_owners.insert("key".into(), owner);
            }
            state.browser_unparked.insert("key".into(), (None, 42, 1));
            state.active_login = Some(ActiveLogin {
                owner: LoginOwner::new(&store, Cli::Claude, "row", 900),
                route: Some("profile".into()),
                token: token.clone(),
            });
            let guard = std::sync::Arc::new(std::sync::Mutex::new(state));
            let claims = LoginClaims::capture(&guard);
            let heard = HEARD.with(std::cell::Cell::get);
            note_published_login(&guard, &claims, &store, Cli::Claude, "row");
            assert_eq!(
                HEARD.with(std::cell::Cell::get) - heard,
                u32::from(activity == "local" || ladder == "local")
            );
            let state = guard.lock().unwrap();
            for held in [&state.last_attempt, &state.parked, &state.bot_checked] {
                assert_eq!(
                    held.contains_key("key"),
                    activity != "local",
                    "{activity}/{ladder}"
                );
            }
            assert_eq!(
                state.activity_owners.contains_key("key"),
                !matches!(activity, "local" | "unattributed"),
                "{activity}/{ladder}"
            );
            assert_eq!(
                state.ladder.retry_at("key", clock).is_some(),
                ladder != "local",
                "{activity}/{ladder}"
            );
            assert_eq!(
                state.ladder_owners.contains_key("key"),
                !matches!(ladder, "local" | "unattributed"),
                "{activity}/{ladder}"
            );
            assert!(state.in_flight);
            assert_eq!(state.browser_unparked.get("key"), Some(&(None, 42, 1)));
            assert!(std::sync::Arc::ptr_eq(
                &state.active_login.as_ref().unwrap().token,
                &token
            ));
        }
    }
}

#[test]
fn a_new_claim_survives_an_older_publication_at_the_same_wall_time() {
    let store = accounts::Store::at(Path::new("/synthetic/claim-replacement"));
    for activity_changed in [false, true] {
        for ladder_changed in [false, true] {
            let clock = Instant::now();
            let mut state = AutoRenewGuard::default();
            state.last_attempt.insert("row".into(), clock);
            state.ladder.failed("row", "input", clock);
            // A wall-clock jump cannot identify which claim was captured first.
            state.activity_owners.insert(
                "row".into(),
                LoginOwner::new(&store, Cli::Claude, "row", 5000),
            );
            state.ladder_owners.insert(
                "row".into(),
                LoginOwner::new(&store, Cli::Claude, "row", 5000),
            );
            let guard = std::sync::Arc::new(std::sync::Mutex::new(state));
            let claims = LoginClaims::capture(&guard);
            {
                let mut state = guard.lock().unwrap();
                if activity_changed {
                    state.activity_owners.insert(
                        "row".into(),
                        LoginOwner::new(&store, Cli::Claude, "row", 5000),
                    );
                }
                if ladder_changed {
                    state.ladder_owners.insert(
                        "row".into(),
                        LoginOwner::new(&store, Cli::Claude, "row", 5000),
                    );
                }
            }
            note_published_login(&guard, &claims, &store, Cli::Claude, "row");
            let state = guard.lock().unwrap();
            assert_eq!(state.last_attempt.contains_key("row"), activity_changed);
            assert_eq!(state.ladder.has_backoff("row"), ladder_changed);
        }
    }
}

#[test]
fn a_new_permit_keeps_its_retry_before_its_owner_is_attached() {
    let store = accounts::Store::at(Path::new("/synthetic/permit-replacement"));
    let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
    let clock = Instant::now();
    let first = AutoRenewPermit::claim(&guard, "key", clock, true).unwrap();
    first.attribute(
        &store,
        Cli::Claude,
        "row",
        900,
        &accounts::Logins::default(),
    );
    drop(first);
    let claims = LoginClaims::capture(&guard);
    let next = AutoRenewPermit::claim(&guard, "key", clock, true).unwrap();
    note_published_login(&guard, &claims, &store, Cli::Claude, "row");
    {
        let state = guard.lock().unwrap();
        assert!(state.in_flight);
        assert_eq!(state.last_attempt.get("key"), Some(&clock));
        assert!(!state.activity_owners.contains_key("key"));
    }
    next.attribute(
        &store,
        Cli::Claude,
        "row",
        900,
        &accounts::Logins::default(),
    );
    note_published_login(&guard, &claims, &store, Cli::Claude, "row");
    assert_eq!(guard.lock().unwrap().last_attempt.get("key"), Some(&clock));
    drop(next);
}

#[test]
fn publication_resets_an_expired_but_retained_retry_step() {
    let store = accounts::Store::at(Path::new("/synthetic/expired-retry"));
    let clock = Instant::now();
    let mut state = AutoRenewGuard::default();
    let first = state.ladder.failed("row", "input", clock);
    let later = clock + first;
    assert!(!state.ladder.held("row", "input", later));
    assert!(state.ladder.has_backoff("row"));
    state.ladder_owners.insert(
        "row".into(),
        LoginOwner::new(&store, Cli::Claude, "row", 900),
    );
    let guard = std::sync::Arc::new(std::sync::Mutex::new(state));
    let claims = LoginClaims::capture(&guard);
    note_published_login(&guard, &claims, &store, Cli::Claude, "row");
    let mut state = guard.lock().unwrap();
    assert!(!state.ladder.has_backoff("row"));
    assert_eq!(state.ladder.failed("row", "input", later), first);
}

#[test]
fn a_sign_in_that_begins_or_ends_is_heard_and_one_another_took_over_ends_unheard() {
    // The watch runs on the thread that moved the activity, so this thread's count is its own.
    hear_activity();
    let heard = || HEARD.with(std::cell::Cell::get);
    let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
    let store = accounts::Store::at(Path::new("/synthetic/activity-heard"));
    let first = LoginActivityGuard::begin(&guard, &store, Cli::Claude, "row", None);
    assert_eq!(heard(), 1, "a sign-in that began is heard");
    let second = LoginActivityGuard::begin(&guard, &store, Cli::Claude, "row", None);
    assert_eq!(heard(), 2);
    drop(first);
    assert_eq!(
        heard(),
        2,
        "the end of one another took over leaves the activity as it was"
    );
    drop(second);
    assert_eq!(heard(), 3, "a sign-in that ended is heard");
}
