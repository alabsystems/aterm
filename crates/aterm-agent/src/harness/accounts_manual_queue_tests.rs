// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::{Arc, Mutex};

const NOW: i64 = 1_800_000_000;

#[cfg(unix)]
#[test]
fn a_missing_row_intent_retires_only_after_its_owned_worker_validates_the_roster() {
    for case in 0..4 {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let state = root.path().join("state");
        let store = accounts::Store::at(&state);
        let path = root.path().join("accounts.toml");
        let ready = root.path().join("ready");
        fs::create_dir(&ready).unwrap();
        fs::write(ready.join(".credentials.json"), b"{}").unwrap();
        let row = Row {
            label: "ready".into(),
            dir: Some(ready.to_str().unwrap().into()),
            email: Some("ready@example.test".into()),
            ..Row::default()
        };
        let roster = accounts::append_rows_for(Cli::Claude, "", &[row]).unwrap();
        fs::write(&path, &roster).unwrap();
        let request = store.request_pin("missing", NOW).unwrap();
        let lifetime = crate::harness::account_work::Lifetime::default();
        let slot = Mutex::new(None);
        let spawn = |work: Box<dyn FnOnce() + Send>| {
            *slot.lock().unwrap() = Some(work);
            Ok(())
        };
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let current = Roster::load(&path).unwrap();
        assert!(
            matches!(
                ManualSelectionOwner::claim(
                    &guard,
                    &store,
                    Cli::Claude,
                    request.clone().into(),
                    current.row("ready"),
                    NOW
                ),
                Err(RenewalDispatch::Fenced)
            ),
            "a present different row cannot supply this saved request's identity"
        );
        let mut fx = live_effects();
        fx.scope = lifetime.enter();
        fx.spawn_renewal = &spawn;
        fx.renew_guard = &guard;
        assert_eq!(
            reattach_manual_selection(
                Cli::Claude,
                &home,
                &state,
                &path,
                request.clone().into(),
                NOW,
                &fx
            ),
            RenewalDispatch::Started
        );
        {
            let held = guard.lock().unwrap();
            assert_eq!(held.manual_selections.len(), 1);
            assert!(
                held.manual_selections
                    .values()
                    .next()
                    .unwrap()
                    .identity
                    .is_none(),
                "an absent row supplies no projected account identity"
            );
        }
        assert_eq!(
            store.load(NOW).unwrap().0.pending_pin,
            Some(request.clone().into())
        );
        assert!(!store.wake_path().exists(), "enqueue is not retirement");
        let newer = (case == 1).then(|| store.request_pin("missing", NOW + 1).unwrap());
        let paused = (case == 2).then(|| lifetime.pause().unwrap());
        if case == 3 {
            fs::write(&path, b"[").unwrap();
        }
        let work = slot.lock().unwrap().take().unwrap();
        work(); // Missing or unreadable row cannot dispatch a provider.
        assert!(slot.lock().unwrap().is_none());
        assert!(guard.lock().unwrap().manual_selections.is_empty());
        assert!(
            store.signin_now().unwrap().is_some(),
            "original worker signer retired"
        );
        assert_eq!(store.wake_path().exists(), case == 0);
        let pending = store.load(NOW + 1).unwrap().0.pending_pin;
        assert_eq!(
            pending,
            if case == 0 {
                None
            } else {
                Some(newer.unwrap_or(request).into())
            }
        );
        if case == 0 {
            let next = store.request_pin("ready", NOW + 2).unwrap();
            assert_eq!(
                manual_selection_with(Cli::Claude, &home, &state, &path, next.into(), NOW + 2, &fx),
                RenewalDispatch::Recovered,
                "a later settled pass can complete another eligible account in the same pool"
            );
            assert_eq!(
                store.load(NOW + 2).unwrap().0.active.as_deref(),
                Some("ready")
            );
            assert!(
                slot.lock().unwrap().is_none(),
                "enrolled account starts no provider"
            );
        }
        drop(fx);
        paused
            .unwrap_or_else(|| lifetime.pause().unwrap())
            .try_wait()
            .expect("queued identity and actual signer retired before operation completion");
    }
}

#[cfg(unix)]
#[test]
fn closing_work_while_the_retirement_writer_is_held_preserves_the_request() {
    let root = aterm_tempfile::tempdir().unwrap();
    let store = accounts::Store::at(root.path());
    let selection: accounts::PendingSelection = store.request_pin("work", NOW).unwrap().into();
    let turn = store.signin_now().unwrap().unwrap();
    let writer = store.acquire_within(&[]).unwrap().unwrap();
    let lifetime = crate::harness::account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let (release, proceed) = std::sync::mpsc::channel();
    let worker_store = store.clone();
    let worker_selection = selection.clone();
    let worker = std::thread::spawn(move || {
        let observer = |entered: bool| {
            tx.send(entered).unwrap();
            if !entered {
                proceed.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
            }
        };
        let result = retire_manual_selection(
            &worker_store,
            &worker_selection,
            &turn,
            &scope,
            NOW,
            Some(&observer),
        );
        drop(turn);
        result
    });
    let ready = rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    let paused = lifetime.pause().unwrap();
    drop(writer);
    let released = release.send(());
    let retired = worker.join().unwrap();
    let entered = rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    assert!(!ready.unwrap(), "initial work check preceded closure");
    released.unwrap();
    assert!(entered.unwrap(), "the actual writer decision was reached");
    assert_eq!(retired, RenewalDispatch::Held);
    assert_eq!(store.load(NOW).unwrap().0.pending_pin, Some(selection));
    assert!(!store.wake_path().exists());
    paused
        .try_wait()
        .expect("the writer and original operation retired");
}

#[cfg(unix)]
#[test]
fn a_retained_or_unreadable_detached_manual_result_does_not_wake_itself() {
    for case in 0..4 {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let state = root.path().join("state");
        let store = accounts::Store::at_for(&state, Cli::Grok, &home);
        let path = root.path().join("accounts-grok.toml");
        let row = Row {
            label: "work".into(),
            email: Some("work@example.test".into()),
            ..Row::default()
        };
        fs::write(
            &path,
            accounts::append_rows_for(Cli::Grok, "", &[row]).unwrap(),
        )
        .unwrap();
        let request = store.request_pin("work", NOW).unwrap();
        let original = fs::read(store.state_file()).unwrap();
        let lifetime = crate::harness::account_work::Lifetime::default();
        let paused = Mutex::new(None);
        let spawn = |work: Box<dyn FnOnce() + Send>| {
            match case {
                1 => fs::write(store.state_file(), b"{").unwrap(),
                2 => *paused.lock().unwrap() = Some(lifetime.pause().unwrap()),
                3 => {
                    store.request_pin("work", NOW + 1).unwrap();
                }
                _ => {}
            }
            work(); // Grok has no unattended provider route.
            Ok(())
        };
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let mut fx = live_effects();
        fx.scope = lifetime.enter();
        fx.renew_guard = &guard;
        fx.spawn_renewal = &spawn;
        assert_eq!(
            reattach_manual_selection(
                Cli::Grok,
                &home,
                &state,
                &path,
                request.clone().into(),
                NOW,
                &fx
            ),
            RenewalDispatch::Started
        );
        assert_eq!(store.wake_path().exists(), case == 0, "case={case}");
        assert!(
            store.signin_now().unwrap().is_some(),
            "wake follows original signer retirement"
        );
        if case == 1 {
            fs::write(store.state_file(), &original).unwrap();
        }
        let after = store.load(NOW + 1).unwrap().0.pending_pin;
        if case == 0 {
            assert!(after.is_none());
        } else if case == 3 {
            assert_ne!(after, Some(request.into()));
        } else {
            assert_eq!(after, Some(request.into()));
        }
        drop(fx);
        paused
            .into_inner()
            .unwrap()
            .unwrap_or_else(|| lifetime.pause().unwrap())
            .try_wait()
            .expect("actual detached result retired before operation completion");
    }
}

#[cfg(unix)]
#[test]
fn delayed_manual_failure_retires_only_its_exact_uncancelled_intent() {
    for next in [false, true] {
        for case in 0..5 {
            let root = aterm_tempfile::tempdir().unwrap();
            let store = accounts::Store::at(root.path());
            let selection = if next {
                accounts::PendingSelection::Next(store.request_next(NOW).unwrap())
            } else {
                store.request_pin("work", NOW).unwrap().into()
            };
            let lifetime = crate::harness::account_work::Lifetime::default();
            let scope = lifetime.enter().unwrap();
            let turn = store.signin_now().unwrap().unwrap();
            let newer = if case == 1 {
                Some(if next {
                    accounts::PendingSelection::Next(store.request_next(NOW + 1).unwrap())
                } else {
                    store.request_pin("work", NOW + 1).unwrap().into()
                })
            } else {
                None
            };
            let paused = (case == 2).then(|| lifetime.pause().unwrap());
            let other = accounts::Store::at(&root.path().join("other"));
            let foreign = other.signin_now().unwrap().unwrap();
            let observer = |entered| {
                assert!(
                    store.signin_now().unwrap().is_none(),
                    "same-nonce contenders remain excluded through retirement"
                );
                if entered && case == 4 {
                    let state = store.state_file();
                    fs::rename(&state, state.with_extension("retained")).unwrap();
                    fs::create_dir(state).unwrap();
                }
            };
            let result = retire_manual_selection(
                &store,
                &selection,
                if case == 3 { &foreign } else { &turn },
                &scope,
                NOW + 2,
                Some(&observer),
            );
            assert_eq!(
                result,
                match case {
                    0 => RenewalDispatch::Retired,
                    1 => RenewalDispatch::Superseded,
                    2 => RenewalDispatch::Held,
                    _ => RenewalDispatch::Fenced,
                },
                "next={next}, case={case}"
            );
            assert!(
                !store.wake_path().exists(),
                "only post-owner completion may publish wake"
            );
            if case != 4 {
                assert_eq!(
                    store.load(NOW + 2).unwrap().0.pending_pin,
                    if case == 0 {
                        None
                    } else {
                        Some(newer.unwrap_or(selection))
                    }
                );
            }
            // Publication refusal after the local edit may retain a durable
            // transaction; it is not evidence that no edit was admitted.
            drop(foreign);
            drop(turn);
            drop(scope);
            paused
                .unwrap_or_else(|| lifetime.pause().unwrap())
                .try_wait()
                .expect("settlement remains within the counted operation");
        }
    }
}

#[test]
fn only_typed_unsupported_retires_its_exact_selection_without_provider_history() {
    let root = aterm_tempfile::tempdir().unwrap();
    let store = accounts::Store::at(root.path());
    let first = store.request_next(NOW).unwrap();
    let logins = fs::read(store.logins_file()).ok();
    let selection = accounts::PendingSelection::Next(first.clone());
    let held = manual_queue_refused(
        &store,
        &selection,
        io::Error::from(io::ErrorKind::PermissionDenied),
        NOW,
    );
    assert_eq!(held, RenewalDispatch::Fenced);
    assert_eq!(
        store.load(NOW).unwrap().0.pending_pin.as_ref(),
        Some(&selection)
    );
    let newer = store.request_next(NOW + 1).unwrap();
    assert_eq!(
        manual_queue_refused(
            &store,
            &selection,
            io::Error::from(io::ErrorKind::Unsupported),
            NOW + 1
        ),
        RenewalDispatch::Unavailable
    );
    let newer = accounts::PendingSelection::Next(newer);
    assert_eq!(
        store.load(NOW + 1).unwrap().0.pending_pin.as_ref(),
        Some(&newer)
    );
    assert_eq!(
        manual_queue_refused(
            &store,
            &newer,
            io::Error::from(io::ErrorKind::Unsupported),
            NOW + 1
        ),
        RenewalDispatch::Unavailable
    );
    assert!(store.load(NOW + 1).unwrap().0.pending_pin.is_none());
    // An Account selection follows the same rule: upkeep resumes it before any other
    // sign-in, so one this platform can never run is retired, never kept for good.
    let older = store.request_pin("same", NOW + 2).unwrap();
    let older = accounts::PendingSelection::Account(older);
    assert_eq!(
        manual_queue_refused(
            &store,
            &older,
            io::Error::from(io::ErrorKind::PermissionDenied),
            NOW + 2
        ),
        RenewalDispatch::Fenced
    );
    assert_eq!(
        store.load(NOW + 2).unwrap().0.pending_pin.as_ref(),
        Some(&older),
        "a refusal that may pass keeps the Account intent for a later upkeep"
    );
    let request = store.request_pin("same", NOW + 2).unwrap();
    let account = accounts::PendingSelection::Account(request);
    assert_eq!(
        manual_queue_refused(
            &store,
            &older,
            io::Error::from(io::ErrorKind::Unsupported),
            NOW + 2
        ),
        RenewalDispatch::Unavailable
    );
    assert_eq!(
        store.load(NOW + 2).unwrap().0.pending_pin.as_ref(),
        Some(&account),
        "retiring an older Account intent keeps the newer one"
    );
    let mut expected = store.load(NOW + 2).unwrap().0;
    expected.pending_pin = None;
    assert_eq!(
        manual_queue_refused(
            &store,
            &account,
            io::Error::from(io::ErrorKind::Unsupported),
            NOW + 2
        ),
        RenewalDispatch::Unavailable
    );
    assert_eq!(
        store.load(NOW + 2).unwrap().0,
        expected,
        "only the exact Account intent is retired; durable pin metadata remain exact"
    );
    assert_eq!(
        fs::read(store.logins_file()).ok(),
        logins,
        "no provider observation was made"
    );
}

fn row() -> accounts::Row {
    Roster::parse("[[account]]\nlabel='same'\nclass='dedicated'\nemail='same@example.test'\n")
        .unwrap()
        .rows
        .remove(0)
}

#[test]
fn manual_owners_partition_same_labels_and_retire_only_the_exact_token() {
    let a = aterm_tempfile::tempdir().unwrap();
    let b = aterm_tempfile::tempdir().unwrap();
    let (a, b) = (accounts::Store::at(a.path()), accounts::Store::at(b.path()));
    let (first, other) = (
        a.request_pin("same", NOW).unwrap(),
        b.request_pin("same", NOW).unwrap(),
    );
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let first =
        ManualSelectionOwner::claim(&guard, &a, Cli::Claude, first.into(), Some(&row()), NOW)
            .unwrap();
    let other =
        ManualSelectionOwner::claim(&guard, &b, Cli::Claude, other.into(), Some(&row()), NOW)
            .unwrap();
    assert!(first.owns() && other.owns());
    let next = a.request_pin("same", NOW + 1).unwrap();
    let next =
        ManualSelectionOwner::claim(&guard, &a, Cli::Claude, next.into(), Some(&row()), NOW + 1)
            .unwrap();
    assert!(!first.owns());
    assert!(next.owns() && other.owns());
    drop(first);
    assert!(
        next.owns() && other.owns(),
        "old Drop cannot retire either current namespace"
    );
    drop(next);
    assert!(other.owns());
    assert_eq!(guard.lock().unwrap().manual_selections.len(), 1);
    drop(other);
    assert!(guard.lock().unwrap().manual_selections.is_empty());
}

#[test]
fn an_owner_inserted_before_transaction_failure_is_retired_by_its_provisional_guard() {
    let root = aterm_tempfile::tempdir().unwrap();
    let store = accounts::Store::at(root.path());
    let request = store.request_next(NOW).unwrap();
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let ledger = store.ledger_path();
    let retained = ledger.with_extension("retained");
    let existed = ledger.try_exists().unwrap();
    if existed {
        fs::rename(&ledger, &retained).unwrap();
    }
    fs::create_dir(&ledger).unwrap();
    assert!(matches!(
        ManualSelectionOwner::claim(
            &guard,
            &store,
            Cli::Claude,
            accounts::PendingSelection::Next(request.clone()),
            None,
            NOW
        ),
        Err(RenewalDispatch::Fenced)
    ));
    assert!(
        guard.lock().unwrap().manual_selections.is_empty(),
        "post-callback ledger failure retires the exact inserted owner"
    );
    fs::remove_dir(&ledger).unwrap();
    if existed {
        fs::rename(&retained, &ledger).unwrap();
    }
    let latest = store.request_next(NOW + 1).unwrap();
    let owner = ManualSelectionOwner::claim(
        &guard,
        &store,
        Cli::Claude,
        accounts::PendingSelection::Next(latest.clone()),
        None,
        NOW + 1,
    )
    .unwrap();
    assert!(matches!(
        ManualSelectionOwner::claim(
            &guard,
            &store,
            Cli::Claude,
            accounts::PendingSelection::Next(request),
            None,
            NOW + 1
        ),
        Err(RenewalDispatch::Superseded)
    ));
    assert!(owner.owns());
    assert_eq!(
        store.load(NOW + 1).unwrap().0.pending_pin,
        Some(accounts::PendingSelection::Next(latest))
    );
}

#[test]
fn unstarted_manual_work_retires_its_owner_before_the_operation_scope() {
    let root = aterm_tempfile::tempdir().unwrap();
    let store = accounts::Store::at(root.path());
    let request = store.request_next(NOW).unwrap();
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let owner = ManualSelectionOwner::claim(
        &guard,
        &store,
        Cli::Claude,
        accounts::PendingSelection::Next(request),
        None,
        NOW,
    )
    .unwrap();
    let lifetime = super::super::account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let queued = scope.wrap(move || drop(owner));
    let paused = lifetime.pause().unwrap();
    let paused = paused
        .try_wait()
        .expect_err("the queued whole owner is still counted");
    drop(queued);
    let drained = paused
        .try_wait()
        .expect("unstarted resources retired synchronously");
    assert!(guard.lock().unwrap().manual_selections.is_empty());
    assert!(drained.resume().is_ok());
}

#[test]
fn a_saved_next_intent_reattaches_without_a_new_nonce_and_no_candidate_retires_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let state = root.path().join("state");
    fs::create_dir(&home).unwrap();
    let store = accounts::Store::at(&state);
    let request = store.request_next(NOW).unwrap();
    let old = Arc::new(Mutex::new(AutoRenewGuard::default()));
    drop(
        ManualSelectionOwner::claim(
            &old,
            &store,
            Cli::Claude,
            accounts::PendingSelection::Next(request.clone()),
            None,
            NOW,
        )
        .unwrap(),
    );
    drop(old);
    let restarted = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let owner = ManualSelectionOwner::claim(
        &restarted,
        &store,
        Cli::Claude,
        accounts::PendingSelection::Next(request.clone()),
        None,
        NOW,
    )
    .unwrap();
    assert!(matches!(
        ManualSelectionOwner::claim(
            &restarted,
            &store,
            Cli::Claude,
            accounts::PendingSelection::Next(request),
            None,
            NOW
        ),
        Err(RenewalDispatch::Held)
    ));
    let roster = root.path().join("accounts.toml");
    fs::write(&roster, "").unwrap();
    let lifetime = super::super::account_work::Lifetime::default();
    let mut fx = live_effects();
    fx.renew_guard = &restarted;
    let result = with_scope_effects(lifetime.enter().unwrap(), fx, |fx| {
        manual_selection_owned(
            Cli::Claude,
            &home,
            &state,
            &roster,
            (owner, Gesture::Clicked),
            NOW,
            fx,
        )
    });
    assert_eq!(result, RenewalDispatch::Retired);
    assert!(store.load(NOW).unwrap().0.pending_pin.is_none());
    assert!(restarted.lock().unwrap().manual_selections.is_empty());
    assert!(lifetime.pause().unwrap().try_wait().is_ok());
}

#[test]
fn queued_next_rechecks_cancellation_replacement_and_policy_before_admission() {
    for change in [0, 1, 2] {
        let replace = change == 1;
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let state = root.path().join("state");
        fs::create_dir(&home).unwrap();
        let roster = root.path().join("accounts.toml");
        let config = config_beside(&roster);
        fs::write(&roster, "").unwrap();
        let store = accounts::Store::at(&state);
        let request = store.request_next(NOW).unwrap();
        let logins = fs::read(store.logins_file()).ok();
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let owner = ManualSelectionOwner::claim(
            &guard,
            &store,
            Cli::Claude,
            accounts::PendingSelection::Next(request.clone()),
            None,
            NOW,
        )
        .unwrap();
        let mut head = Some(store.queue_manual_signin().unwrap());
        let lifetime = super::super::account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let into = guard.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("queued-next-control".into())
            .spawn(move || {
                let mut fx = live_effects();
                fx.renew_guard = &into;
                let result = with_scope_effects(scope, fx, |fx| {
                    manual_selection_owned(
                        Cli::Claude,
                        &home,
                        &state,
                        &roster,
                        (owner, Gesture::Clicked),
                        NOW,
                        fx,
                    )
                });
                let _ = tx.send(result);
            })
            .unwrap();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let observed = loop {
            if guard
                .lock()
                .unwrap()
                .manual_selections
                .values()
                .any(|entry| entry.position.is_some_and(|position| position.ahead == 1))
            {
                break true;
            }
            if std::time::Instant::now() >= until {
                break false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let newer = if replace {
            Some(store.request_next(NOW + 1))
        } else {
            None
        };
        let policy_write = if change == 2 {
            let written =
                aterm_uds::atomic_file::write(&config, b"[harness]\naccounts='off'\n", 0o600);
            drop(head.take());
            Some(written)
        } else {
            None
        };
        let paused = if change == 0 { lifetime.pause() } else { None };
        let returned = rx.recv_timeout(std::time::Duration::from_secs(60));
        // Even a broken waiter is cancelled, released and joined before assertions.
        let recovery = if paused.is_none() {
            lifetime.pause()
        } else {
            None
        };
        drop(head);
        let joined = worker.join();
        assert!(joined.is_ok());
        if let Some(written) = policy_write {
            written.unwrap();
        }
        assert!(
            observed,
            "the actual worker published its retained FIFO position"
        );
        let returned = returned.unwrap();
        if replace {
            // The worker either sees the committed replacement or refuses its
            // in-progress durable publication. Neither authorizes provider work.
            assert!(matches!(
                returned,
                RenewalDispatch::Superseded | RenewalDispatch::Fenced
            ));
        } else {
            assert_eq!(returned, RenewalDispatch::Held);
        }
        assert!(paused.or(recovery).unwrap().try_wait().is_ok());
        assert!(guard.lock().unwrap().manual_selections.is_empty());
        assert!(!guard.lock().unwrap().in_flight);
        assert_eq!(
            fs::read(store.logins_file()).ok(),
            logins,
            "queued refusal recorded no provider result"
        );
        let saved = store.load(NOW + 1).unwrap().0.pending_pin;
        assert_eq!(
            saved,
            Some(accounts::PendingSelection::Next(
                newer.map(|result| result.unwrap()).unwrap_or(request)
            ))
        );
        assert!(store.signin_now().unwrap().is_some());
    }
}
