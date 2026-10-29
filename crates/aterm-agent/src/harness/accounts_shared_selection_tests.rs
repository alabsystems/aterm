// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::{Arc, Mutex};

const NOW: i64 = 1_793_000_000;

struct Bed {
    _root: aterm_tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    path: PathBuf,
    raw: Roster,
    store: accounts::Store,
    request: accounts::PinRequest,
    token: std::cell::RefCell<Option<accounts::shared::Token>>,
}

impl Bed {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let state = root.path().join("state");
        let path = root.path().join("accounts.toml");
        let store = accounts::Store::at_for(&state, Cli::Claude, &home);
        let raw = Roster {
            rows: ["work", "other"]
                .into_iter()
                .map(|label| Row {
                    label: label.into(),
                    email: Some(format!("{label}@example.test")),
                    dir: Some(root.path().join(label).to_str().unwrap().into()),
                    ..Row::default()
                })
                .collect(),
        };
        fs::write(
            &path,
            accounts::append_rows_for(Cli::Claude, "", &raw.rows).unwrap(),
        )
        .unwrap();
        // A selection saved before this installation learned the shared source.
        let accounts::PinSelection::Renew(request) =
            store.select_account(&raw, &path, "work", NOW).unwrap()
        else {
            panic!("fixture requires sign-in")
        };
        Self {
            _root: root,
            home,
            state,
            path,
            raw,
            store,
            request,
            token: Default::default(),
        }
    }

    fn shared(&self, outcome: accounts::shared::Outcome) {
        let staging = self.store.logins_dir().join("work+1");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("retained-work"), b"unfinished sign-in").unwrap();
        self.store
            .transact_logins(NOW, &mut None, |_, logins| {
                *self.token.borrow_mut() = Some(logins.shared.begin(
                    accounts::shared::Intent {
                        source: accounts::shared::Source {
                            network: [1; 32],
                            account: [2; 32],
                        },
                        decision: [3; 32],
                        generation: 1,
                        configuration: [4; 32],
                        request: [5; 16],
                        label: "work".into(),
                        identity: "work@example.test".into(),
                        staging: staging.to_str().unwrap().into(),
                        before_generation: None,
                        begun_at: NOW,
                    },
                    Some([6; 32]),
                )?);
                Ok(Vec::new())
            })
            .unwrap();
        if matches!(outcome, accounts::shared::Outcome::Published { .. }) {
            self.publish();
        } else if outcome != accounts::shared::Outcome::Pending {
            self.store
                .transact_logins(NOW, &mut None, |_, logins| {
                    logins.shared.finish(
                        self.token.borrow_mut().take().unwrap(),
                        outcome,
                        None,
                        NOW,
                    )?;
                    Ok(Vec::new())
                })
                .unwrap();
        }
    }

    fn publish(&self) {
        let staging = self.store.logins_dir().join("work+1");
        fs::write(staging.join(".credentials.json"), b"{}").unwrap();
        self.store
            .transact_logins(NOW, &mut None, |state, logins| {
                let generation = logins.generation_clock + 1;
                logins.shared.finish(
                    self.token.borrow_mut().take().unwrap(),
                    accounts::shared::Outcome::Published { generation },
                    Some(("work@example.test", [6; 32])),
                    NOW,
                )?;
                Ok(accounts::note_renewed(
                    state,
                    logins,
                    "work",
                    self.raw.rows[0].dir.as_deref(),
                    staging.to_str().unwrap(),
                    None,
                    NOW,
                ))
            })
            .unwrap();
    }

    fn dispatch(&self, request: &accounts::PinRequest) -> RenewalDispatch {
        let (result, workers) = self.dispatch_observed(request);
        assert_eq!(workers, 0, "shared request must not spawn");
        result
    }

    fn dispatch_observed(&self, request: &accounts::PinRequest) -> (RenewalDispatch, usize) {
        let workers = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&workers);
        let result = renew_requested_pin_for(
            Cli::Claude,
            &self.home,
            &self.state,
            &self.path,
            request.clone(),
            NOW,
            &Arc::default(),
            &move |_| {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
        );
        (result, workers.load(std::sync::atomic::Ordering::SeqCst))
    }
}

#[test]
fn shared_selection_needing_login_preserves_the_previous_request() {
    for outcome in [
        accounts::shared::Outcome::Pending,
        accounts::shared::Outcome::Unknown,
    ] {
        let bed = Bed::new();
        bed.shared(outcome);
        let accounts::PinSelection::Renew(previous) = bed
            .store
            .select_account(&bed.raw, &bed.path, "other", NOW)
            .unwrap()
        else {
            panic!("other requires sign-in")
        };
        let before = bed.store.load_logins(NOW).unwrap().0;
        assert!(
            bed.store
                .select_account(&bed.raw, &bed.path, "work", NOW)
                .is_err()
        );
        assert_eq!(bed.store.load(NOW).unwrap().0.pending_pin, Some(previous));
        assert_eq!(bed.store.load_logins(NOW).unwrap().0, before);
    }
}

#[test]
fn a_saved_shared_selection_is_retired_before_spawning_a_worker() {
    for outcome in [
        accounts::shared::Outcome::Pending,
        accounts::shared::Outcome::Unknown,
        accounts::shared::Outcome::Published { generation: 0 },
    ] {
        let bed = Bed::new();
        bed.shared(outcome);
        if matches!(outcome, accounts::shared::Outcome::Published { .. }) {
            bed.store
                .transact(NOW, &mut None, |state| {
                    Ok(accounts::note_auth(
                        state,
                        "work",
                        accounts::Auth::SignedOut,
                        NOW,
                    ))
                })
                .unwrap();
        }
        let before = bed.store.load_logins(NOW).unwrap().0;
        assert_eq!(bed.dispatch(&bed.request), RenewalDispatch::Failed);
        assert!(bed.store.load(NOW).unwrap().0.pending_pin.is_none());
        assert_eq!(bed.store.load_logins(NOW).unwrap().0, before);
        assert_eq!(
            fs::read(bed.store.logins_dir().join("work+1/retained-work")).unwrap(),
            b"unfinished sign-in"
        );
    }
}

#[test]
fn a_renamed_shared_identity_needing_login_cannot_queue_ordinary_renewal() {
    let mut bed = Bed::new();
    bed.shared(accounts::shared::Outcome::Unknown);
    bed.raw.rows[0].label = "renamed".into();
    fs::write(
        &bed.path,
        accounts::append_rows_for(Cli::Claude, "", &bed.raw.rows).unwrap(),
    )
    .unwrap();
    assert!(
        bed.store
            .select_account(&bed.raw, &bed.path, "renamed", NOW)
            .is_err()
    );
    assert_eq!(
        bed.store.load(NOW).unwrap().0.pending_pin,
        Some(bed.request)
    );
}

#[test]
fn a_usable_shared_account_is_selectable_and_supersedes_a_pending_request() {
    let bed = Bed::new();
    bed.shared(accounts::shared::Outcome::Published { generation: 0 });
    let _ = bed
        .store
        .select_account(&bed.raw, &bed.path, "other", NOW)
        .unwrap();
    let before = bed.store.load_logins(NOW).unwrap().0;
    assert_eq!(
        bed.store
            .select_account(&bed.raw, &bed.path, "work", NOW)
            .unwrap(),
        accounts::PinSelection::Pinned
    );
    let state = bed.store.load(NOW).unwrap().0;
    assert_eq!(state.active.as_deref(), Some("work"));
    assert!(state.pending_pin.is_none());
    assert_eq!(bed.store.load_logins(NOW).unwrap().0, before);
}

#[test]
fn stale_shared_dispatch_preserves_a_newer_request_and_unreadable_state() {
    let bed = Bed::new();
    bed.shared(accounts::shared::Outcome::Unknown);
    let accounts::PinSelection::Renew(newer) = bed
        .store
        .select_account(&bed.raw, &bed.path, "other", NOW)
        .unwrap()
    else {
        panic!("other requires sign-in")
    };
    assert_eq!(bed.dispatch(&bed.request), RenewalDispatch::Superseded);
    assert_eq!(bed.store.load(NOW).unwrap().0.pending_pin, Some(newer));
    let before = fs::read(bed.store.state_file()).unwrap();
    fs::write(bed.store.state_file(), b"{").unwrap();
    assert_eq!(bed.dispatch(&bed.request), RenewalDispatch::Fenced);
    assert_eq!(fs::read(bed.store.state_file()).unwrap(), b"{");
    fs::write(bed.store.state_file(), before).unwrap();
}

fn progress(bed: &Bed) -> (bool, usize) {
    let store = &bed.store;
    let opener = bed.home.join(".local/bin/aterm-open-url");
    fs::create_dir_all(opener.parent().unwrap()).unwrap();
    fs::write(opener, b"inert opener").unwrap();
    fs::create_dir_all(super::super::headless_oauth::browser_profile_dir(
        &store.dir,
        Some("other"),
    ))
    .unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let cooldown = Mutex::new(None);
    let rediscovery = Mutex::new(Rediscovery::default());
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let fx = UpkeepEffects {
        fetch: &|_, _| panic!("no external source in this fixture"),
        spawn_renewal: &|work| {
            work();
            Ok(())
        },
        quota_refresh: &|_, _, _, _| {},
        sync_cooldown: &cooldown,
        rediscovery: &rediscovery,
        renew_guard: &guard,
        find_browser: &|_| true,
        vault_sync: |_| panic!("no vault needed"),
        spawn_vault: &|_| panic!("no vault worker needed"),
        default_check: |_: &Path| crate::harness::cli::LoginCheck::CouldNotRun(String::new()),
        mailbox: &|_, _, _| (super::super::sign_in_ladder::Mailbox::None, None),
        log: Arc::new(|_| {}),
        shared: shared_renewal::Effects::default(),
        run_attempt: Some(Arc::new(move |_, _| {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            super::super::cli::Renewal::AuthFailed
        })),
    };
    (
        auto_renew_next_account(&bed.home, &bed.state, &bed.path, NOW, &fx),
        calls.load(std::sync::atomic::Ordering::SeqCst),
    )
}

#[test]
fn retiring_a_shared_selection_allows_another_account_in_the_same_upkeep() {
    let bed = Bed::new();
    bed.shared(accounts::shared::Outcome::Unknown);
    let before = bed.store.load_logins(NOW).unwrap().0.shared;
    assert_eq!(progress(&bed), (true, 1));
    assert!(bed.store.load(NOW).unwrap().0.pending_pin.is_none());
    assert_eq!(bed.store.load_logins(NOW).unwrap().0.shared, before);
}

pub(super) fn selection_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let bed = Bed::new();
    if state["shared"] == 1 {
        bed.shared(if state["ready"] == 1 {
            accounts::shared::Outcome::Published { generation: 0 }
        } else {
            accounts::shared::Outcome::Pending
        });
    }
    if state["pending"] == 2 {
        bed.store
            .select_account(&bed.raw, &bed.path, "other", NOW)
            .unwrap();
    }
    if state["phase"] >= 3 {
        bed.dispatch(&bed.request);
    }
    let before = bed.store.load_logins(NOW).unwrap().0.shared;
    let staged = bed.store.logins_dir().join("work+1/retained-work");
    let before_staged = fs::read(&staged).ok();
    let mut next = state.clone();
    match action {
        "Queue" => {
            next.insert("phase", 1);
        }
        "Reserve" => {
            bed.shared(accounts::shared::Outcome::Pending);
            next.insert("phase", 2);
            next.insert("shared", 1);
        }
        "TryShared" => {
            let refused = bed
                .store
                .select_account(&bed.raw, &bed.path, "work", NOW)
                .is_err();
            next.insert("tried", 1);
            next.insert("refused", i64::from(refused));
        }
        "Ready" => {
            bed.publish();
            next.insert("ready", 1);
        }
        "Replace" => {
            bed.store
                .select_account(&bed.raw, &bed.path, "other", NOW)
                .unwrap();
            next.insert("newer", 1);
        }
        "Resolve" => {
            let (_, workers) = bed.dispatch_observed(&bed.request);
            next.insert("workers", workers as i64);
            next.insert("phase", 3);
        }
        "Progress" => {
            let (progressed, calls) = progress(&bed);
            next.insert("phase", 4);
            next.insert("progressed", i64::from(progressed && calls == 1));
        }
        _ => unreachable!(),
    }
    let after = bed.store.load(NOW).unwrap().0;
    let pending = after
        .pending_pin
        .as_ref()
        .map(|request| if request.label() == "work" { 1 } else { 2 })
        .unwrap_or(0);
    next.insert("pending", pending);
    next.insert("active", i64::from(after.active.as_deref() == Some("work")));
    if !matches!(action, "Reserve" | "Ready") {
        next.insert(
            "preserved",
            i64::from(
                before == bed.store.load_logins(NOW).unwrap().0.shared
                    && fs::read(staged).ok() == before_staged,
            ),
        );
    }
    next
}

#[test]
fn shared_selection_conforms_through_custody_arrival_and_upkeep_progress() {
    let model = aterm_models_harness::account_shared_selection_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 7);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = selection_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{} {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let queued = selection_transition(&model.init_state(), "Queue");
    let shared = selection_transition(&queued, "Reserve");
    let mut refused = selection_transition(&shared, "TryShared");
    refused.insert("refused", 0);
    assert!(!model.check_invariant("RefusesUnsupported", &refused));
    let resolved = selection_transition(&shared, "Resolve");
    for (field, value, law) in [
        ("pending", 1, "NoBlockedQueue"),
        ("preserved", 0, "RetainsCustody"),
        ("workers", 1, "NoSharedWorker"),
    ] {
        let mut bad = resolved.clone();
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad));
    }
    let mut progressed = selection_transition(&resolved, "Progress");
    progressed.insert("progressed", 0);
    assert!(!model.check_invariant("UsefulProgress", &progressed));
    let replaced = selection_transition(&shared, "Replace");
    let mut newer = selection_transition(&replaced, "Resolve");
    newer.insert("pending", 0);
    assert!(!model.check_invariant("RetainsNewer", &newer));
    let ready = selection_transition(&shared, "Ready");
    let mut usable = selection_transition(&ready, "Resolve");
    usable.insert("active", 0);
    assert!(!model.check_invariant("UsefulLogin", &usable));
}
