// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::{Arc, Mutex};

const NOW: i64 = 1_792_660_000;

struct Fixture {
    _root: aterm_tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    path: PathBuf,
    raw: Roster,
    store: accounts::Store,
    request: accounts::PinRequest,
}

impl Fixture {
    fn new(cli: Cli) -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let state = root.path().join("state");
        let path = root.path().join(cli.accounts().roster_file);
        // A case's held lock stays held through the admission, which refuses at the first take.
        let store = accounts::Store::at_for(&state, cli, &home).bounded_by(&[]);
        let raw = Roster {
            rows: ["work", "other"]
                .into_iter()
                .map(|label| Row {
                    label: label.into(),
                    dir: Some(root.path().join(label).to_str().unwrap().into()),
                    email: Some(format!("{label}@example.test")),
                    vendor: accounts::Vendor {
                        region: (cli == Cli::Kimi).then_some(crate::ai::connector::Region::Global),
                        ..accounts::Vendor::default()
                    },
                    ..Row::default()
                })
                .collect(),
        };
        fs::write(
            &path,
            accounts::append_rows_for(cli, "", &raw.rows).unwrap(),
        )
        .unwrap();
        let browser = home.join(
            ".cache/aterm/browser.noindex/chrome-headless-shell-fixture/chrome-headless-shell",
        );
        fs::create_dir_all(browser.parent().unwrap()).unwrap();
        fs::write(browser, b"inert browser fixture").unwrap();
        let accounts::PinSelection::Renew(request) = store
            .select_account_for(cli, &raw, &path, "work", NOW)
            .unwrap()
        else {
            panic!("fixture needs a sign-in")
        };
        Self {
            _root: root,
            home,
            state,
            path,
            raw,
            store,
            request,
        }
    }

    fn records(&self) -> [Option<Vec<u8>>; 8] {
        [
            self.store.state_file(),
            self.store.logins_file(),
            self.store.state_witness_file(),
            self.store.logins_witness_file(),
            self.store.ledger_path(),
            self.store.wake_path(),
            self.store.dir.join("sync-commit.json"),
            self.path.clone(),
        ]
        .map(|path| fs::read(path).ok())
    }

    fn queue_claude(
        &self,
        guard: &Arc<Mutex<AutoRenewGuard>>,
        spawn: &SpawnRenewal,
        run: RunAttempt,
    ) -> RenewalDispatch {
        let opener = self.home.join(".local/bin/aterm-open-url");
        fs::create_dir_all(opener.parent().unwrap()).unwrap();
        fs::write(opener, b"inert opener fixture").unwrap();
        let ladder = LadderCtx {
            roster_path: self.path.clone(),
            find_browser: &|_| true,
            vault_sync: |_| super::super::sign_in_ladder::Vault::NotMember,
            spawn_vault: &|_| panic!("no vault in an explicit selection"),
            mailbox: &|_, _, _| (super::super::sign_in_ladder::Mailbox::None, None),
            log: Arc::new(|_| {}),
            run_attempt: Some(run),
            shared: shared_renewal::Effects::default(),
            hosts: 0,
        };
        spawn_row_renewal(
            Cli::Claude,
            &self.home,
            &self.state,
            &self.path,
            self.raw.clone(),
            &accounts::Logins::default(),
            self.raw.rows[0].clone(),
            (Some(self.request.clone()), true),
            NOW,
            guard,
            spawn,
            Some(&ladder),
        )
    }
}

fn observe(cli: Cli, case: i64) -> (i64, [bool; 3]) {
    let fixture = Fixture::new(cli);
    let captured = fixture.store.load_logins(NOW).unwrap().0;
    let mut current_raw = fixture.raw.clone();
    if case == 2 || case == 3 {
        fixture
            .store
            .select_account_for(
                cli,
                &fixture.raw,
                &fixture.path,
                if case == 2 { "other" } else { "work" },
                NOW,
            )
            .unwrap();
    } else if case == 4 {
        fixture
            .store
            .cancel_pin_request(&fixture.request, NOW)
            .unwrap();
    } else if matches!(case, 7 | 13) {
        current_raw.rows[usize::from(case == 13)].email = Some("changed@example.test".into());
        fs::write(
            &fixture.path,
            accounts::append_rows_for(cli, "", &current_raw.rows).unwrap(),
        )
        .unwrap();
    } else if case == 8 {
        fs::write(&fixture.path, "").unwrap();
    } else if case == 14 {
        fixture
            .store
            .transact_logins(NOW, &mut None, |state, logins| {
                Ok(accounts::note_renewed(
                    state,
                    logins,
                    "work",
                    None,
                    fixture.raw.rows[0].dir.as_deref().unwrap(),
                    None,
                    NOW,
                ))
            })
            .unwrap();
    }
    let wanted = fixture.store.load(NOW).unwrap().0.pending_pin;
    let damaged = match case {
        5 => Some((fixture.store.state_file(), false)),
        6 => Some((fixture.store.logins_file(), false)),
        12 => Some((
            fixture.store.shared_dir().join(accounts::SIGNIN_LOCK_FILE),
            true,
        )),
        15 => Some((fixture.store.ledger_path(), true)),
        16 => Some((fixture.store.dir.join("sync-commit.json"), false)),
        _ => None,
    };
    let damaged = damaged.map(|(path, directory)| {
        aterm_types::fs_restricted::ensure_private_dir(path.parent().unwrap()).unwrap();
        let bytes = fs::read(&path).ok();
        if directory {
            if path.exists() {
                fs::rename(&path, path.with_extension("retained")).unwrap();
            }
            fs::create_dir(&path).unwrap();
        } else {
            fs::write(&path, b"{").unwrap();
        }
        (path, bytes, directory)
    });
    let held_writer = (case == 9).then(|| fixture.store.acquire_within(&[]).unwrap().unwrap());
    let held_signin = (case == 11).then(|| fixture.store.signin_now().unwrap().unwrap());
    let before = fixture.records();
    let outcome = if (10..=12).contains(&case) {
        match renewal_signin_admission(&fixture.store) {
            Ok(guard) => {
                drop(guard);
                1
            }
            Err(RenewalDispatch::Held) => 2,
            Err(RenewalDispatch::Fenced) => 3,
            other => panic!("unexpected sign-in admission: {other:?}"),
        }
    } else {
        i64::from(
            fixture
                .store
                .admit_renewal_attempt(
                    accounts::RenewalPolicy {
                        roster_path: &fixture.path,
                        roster: &fixture.raw,
                        row: &fixture.raw.rows[0],
                        logins: &captured,
                    },
                    Some(&fixture.request),
                    NOW,
                )
                .unwrap_or(false),
        )
    };
    let untouched = fixture.records() == before;
    drop(held_writer);
    drop(held_signin);
    if let Some((path, bytes, _)) = damaged {
        fs::rename(&path, path.with_extension("obstruction")).unwrap();
        if let Some(bytes) = bytes {
            fs::write(path, bytes).unwrap();
        }
    }
    if case == 8 {
        fs::write(
            &fixture.path,
            accounts::append_rows_for(cli, "", &fixture.raw.rows).unwrap(),
        )
        .unwrap();
    }
    let retained = fixture.store.load(NOW).unwrap().0.pending_pin == wanted;
    let retry = if (10..=12).contains(&case) {
        renewal_signin_admission(&fixture.store).is_ok()
    } else {
        let fresh = fixture.store.load_logins(NOW).unwrap().0;
        let rows = accounts::bound(&current_raw, &fresh);
        let request = wanted.as_ref().unwrap_or(&fixture.request);
        fixture
            .store
            .admit_renewal_attempt(
                accounts::RenewalPolicy {
                    roster_path: &fixture.path,
                    roster: &current_raw,
                    row: rows.row(request.label()).unwrap(),
                    logins: &fresh,
                },
                Some(request),
                NOW,
            )
            .unwrap()
            == (case != 4)
    };
    (outcome, [retained, untouched, retry])
}

pub(crate) fn admission_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherProvider" => {
            next.insert("provider", state["provider"] + 1);
        }
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Check" | "Lock" => {
            let cli = Cli::ALL[usize::try_from(state["provider"] - 1).unwrap()];
            let (outcome, flags) = observe(cli, state["case"]);
            next.insert("phase", 1);
            next.insert("outcome", outcome);
            for (field, value) in ["retained", "untouched", "retry"].into_iter().zip(flags) {
                next.insert(field, i64::from(value));
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn queued_selection_conforms_through_local_refusal_and_repair() {
    let model = aterm_models_harness::account_queued_selection_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = admission_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let good = admission_transition(&model.init_state(), "Check");
    for (field, law) in [
        ("outcome", "CurrentSelectionRuns"),
        ("retained", "KeepsCurrentIntent"),
        ("untouched", "AdmissionEditsNoRecords"),
        ("retry", "RepairOrFreshSnapshotWorks"),
    ] {
        let mut bad = good.clone();
        bad.insert(field, 0);
        assert!(!model.check_invariant(law, &bad));
    }
    for (case, outcome, law) in [
        (2, 1, "StaleOrUnreadableDoesNotRun"),
        (10, 0, "HealthyLockRuns"),
        (11, 4, "BusyLockWaits"),
        (12, 4, "UnreadableLockFences"),
    ] {
        let mut bad = good.clone();
        bad.insert("case", case);
        bad.insert("outcome", outcome);
        assert!(!model.check_invariant(law, &bad));
    }
}

/// An unreadable sign-in lock fences a saved selection of each CLI whose renewal starts in the
/// background, keeping it and dispatching nothing. A Grok row's starts none, whatever the lock
/// (`Cli::renews_headlessly`, pinned in `accounts_sync_cli_tests.rs`).
#[test]
fn unreadable_signin_lock_preserves_saved_selection_without_dispatch() {
    let mut failures = Vec::new();
    for cli in Cli::ALL.into_iter().filter(|cli| cli.renews_headlessly()) {
        let fixture = Fixture::new(cli);
        let lock = fixture.store.shared_dir().join(accounts::SIGNIN_LOCK_FILE);
        aterm_types::fs_restricted::ensure_private_dir(&fixture.store.shared_dir()).unwrap();
        fs::create_dir(&lock).unwrap();
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let refuse = |_: Box<dyn FnOnce() + Send>| {
            panic!("unreadable sign-in lock cannot dispatch authentication")
        };
        let fx = UpkeepEffects {
            renew_guard: &guard,
            spawn_renewal: &refuse,
            ..live_effects()
        };
        let result =
            resume_pending_pin_for(cli, &fixture.home, &fixture.state, &fixture.path, NOW, &fx);
        let retained = fixture.store.load(NOW).unwrap().0.pending_pin;
        if result != Some(RenewalDispatch::Fenced) || retained.as_ref() != Some(&fixture.request) {
            failures.push(format!(
                "{cli:?}: {result:?}, retained={}",
                retained.is_some()
            ));
        }
        assert!(!guard.lock().unwrap().in_flight);
        assert!(guard.lock().unwrap().last_attempt.is_empty());
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn a_replaced_queued_selection_starts_no_authentication() {
    for superseded in [false, true] {
        queued_selection(superseded);
    }
}

fn queued_selection(superseded: bool) {
    let fixture = Fixture::new(Cli::Claude);
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    type Work = Box<dyn FnOnce() + Send>;
    let queued: Arc<Mutex<Vec<Work>>> = Arc::default();
    let into = Arc::clone(&queued);
    let spawn = move |work| {
        into.lock().unwrap().push(work);
        Ok(())
    };
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let called = Arc::clone(&calls);
    assert_eq!(
        fixture.queue_claude(
            &guard,
            &spawn,
            Arc::new(move |_, _| {
                called.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                super::super::cli::Renewal::AuthFailed
            }),
        ),
        RenewalDispatch::Started
    );
    let latest = if superseded {
        let accounts::PinSelection::Renew(latest) = fixture
            .store
            .select_account(&fixture.raw, &fixture.path, "other", NOW)
            .unwrap()
        else {
            panic!("second selection needs a sign-in")
        };
        Some(latest)
    } else {
        None
    };
    let work = queued.lock().unwrap().pop().unwrap();
    work();
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        usize::from(!superseded)
    );
    assert_eq!(fixture.store.load(NOW).unwrap().0.pending_pin, latest);
    assert!(!guard.lock().unwrap().in_flight);
    assert!(fixture.store.signin_now().unwrap().is_some());
}

#[test]
fn fallback_admission_failure_keeps_the_current_request() {
    for case in 0..5 {
        let fixture = Fixture::new(Cli::Claude);
        fs::create_dir_all(super::super::headless_oauth::browser_profile_dir(
            &fixture.store.dir,
            Some("work"),
        ))
        .unwrap();
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        type Work = Box<dyn FnOnce() + Send>;
        let queued: Arc<Mutex<Vec<Work>>> = Arc::default();
        let into = Arc::clone(&queued);
        let spawn = move |work| {
            into.lock().unwrap().push(work);
            Ok(())
        };
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = Arc::clone(&calls);
        let path = fixture.path.clone();
        let store = fixture.store.clone();
        let dir = fixture.raw.rows[0].dir.clone().unwrap();
        let mut updated = fixture.raw.clone();
        updated.rows[0].email = Some("changed@example.test".into());
        let updated = accounts::append_rows_for(Cli::Claude, "", &updated.rows).unwrap();
        assert_eq!(
            fixture.queue_claude(
                &guard,
                &spawn,
                Arc::new(move |_, _| {
                    let previous = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if case == 1 {
                        fs::write(&path, &updated).unwrap();
                    } else if case == 2 {
                        store
                            .transact_logins(NOW, &mut None, |state, logins| {
                                Ok(accounts::note_renewed(
                                    state, logins, "work", None, &dir, None, NOW,
                                ))
                            })
                            .unwrap();
                    } else if case == 3 && previous == 1 {
                        return super::super::cli::Renewal::Busy;
                    } else if case == 4 {
                        return super::super::cli::Renewal::Deferred;
                    }
                    super::super::cli::Renewal::AuthFailed
                })
            ),
            RenewalDispatch::Started
        );
        let work = queued.lock().unwrap().pop().unwrap();
        work();
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            if matches!(case, 1 | 2 | 4) { 1 } else { 2 },
            "case {case}"
        );
        assert_eq!(
            fixture.store.load(NOW).unwrap().0.pending_pin,
            (case != 0).then_some(fixture.request),
            "case {case}"
        );
        assert!(!guard.lock().unwrap().in_flight);
    }
}

/// A vendor's renewal worker keeps its saved selection when its second take of the sign-in lock
/// finds it unavailable. Codex's and Kimi's: no Grok worker starts (`Cli::renews_headlessly`).
#[test]
fn vendor_worker_keeps_selection_when_its_second_lock_take_is_unavailable() {
    for cli in [Cli::Codex, Cli::Kimi] {
        let fixture = Fixture::new(cli);
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        type Work = Box<dyn FnOnce() + Send>;
        let queued: Arc<Mutex<Vec<Work>>> = Arc::default();
        let into = Arc::clone(&queued);
        let spawn = move |work| {
            into.lock().unwrap().push(work);
            Ok(())
        };
        assert_eq!(
            renew_requested_pin_for(
                cli,
                &fixture.home,
                &fixture.state,
                &fixture.path,
                fixture.request.clone(),
                NOW,
                &guard,
                &spawn
            ),
            RenewalDispatch::Started
        );
        let lock = fixture.store.shared_dir().join(accounts::SIGNIN_LOCK_FILE);
        fs::rename(&lock, lock.with_extension("retained")).unwrap();
        fs::create_dir(&lock).unwrap();
        let before = fixture.records();
        let work = queued.lock().unwrap().pop().unwrap();
        work();
        assert_eq!(
            fixture.store.load(NOW).unwrap().0.pending_pin,
            Some(fixture.request.clone())
        );
        assert!(!guard.lock().unwrap().in_flight);
        let after = fixture.records();
        for index in [0, 1, 2, 3, 4, 6, 7] {
            assert_eq!(after[index], before[index], "{cli:?} record {index}");
        }
        assert!(
            !fixture.raw.rows[0]
                .dir
                .as_deref()
                .map(Path::new)
                .unwrap()
                .exists()
        );
    }
}
