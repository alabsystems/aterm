// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn fixture() -> (Arc<Accts>, WorkerJob, Hooks, Snapshot) {
    let a = Accts::new();
    let cfg = on();
    let mut roster = a.roster();
    roster.rows.retain(|row| row.label == "work-2");
    std::fs::write(
        a.roster_path(),
        accounts::append_rows_for(accounts::Cli::Claude, "", &roster.rows).unwrap(),
    )
    .unwrap();
    a.store()
        .transact(a.clock(), &mut None, |state| {
            state.home_accounts.insert("work-2".into());
            Ok(Vec::new())
        })
        .unwrap();
    let snap = a.spawn("s-claude-live", ACCT_TALK_A, "work-2");
    let idle = a.worker(&snap.tab, &cfg);
    let job = acct_job(&snap.tab, &idle);
    assert!(job.left.hand());
    let hooks = pool_hooks(&a);
    (a, job, hooks, snap)
}

fn pool_hooks(a: &Arc<Accts>) -> Hooks {
    crate::test_scratch::write_config(
        &a.tmp.path().join("config/aterm.toml"),
        format!("[harness]\naccounts='{}'\n", on().accounts.as_str()),
    );
    let mut hooks = a.hooks();
    let accounts = Arc::clone(a);
    hooks.acts.account_cold = Arc::new(move |snapshot, mode, _| {
        let seat = accounts.seat();
        account_cold_at(&seat, snapshot, mode, || {
            Some(vec![
                format!("{}={}", accounts::HOMEDIR_ENV, seat.home.display()),
                "PATH=/usr/bin".into(),
            ])
        })
        .map(relaunch::ColdAdmission::immediate)
    });
    hooks
}

fn two_accounts(a: &Accts) {
    let mut roster = a.roster();
    roster
        .rows
        .retain(|row| matches!(row.label.as_str(), "main" | "work-2"));
    std::fs::write(
        a.roster_path(),
        accounts::append_rows_for(accounts::Cli::Claude, "", &roster.rows).unwrap(),
    )
    .unwrap();
    a.store()
        .transact(a.clock(), &mut None, |state| {
            state.home_accounts.extend(["main".into(), "work-2".into()]);
            Ok(Vec::new())
        })
        .unwrap();
}

fn hold(a: &Accts, held: bool) {
    let mut roster = a.roster();
    roster.rows[0].held_by = held.then(|| "another-machine".into());
    std::fs::write(
        a.roster_path(),
        accounts::append_rows_for(accounts::Cli::Claude, "", &roster.rows).unwrap(),
    )
    .unwrap();
}

fn fresh_attempt(
    job: &WorkerJob,
    hooks: &Hooks,
    snap: &mut Snapshot,
    act: impl FnOnce(&Snapshot) -> String,
) -> String {
    account_relaunch(job, hooks, snap, |original, admit| {
        let chosen = match admit.map(|admit| admit(original)).transpose() {
            Ok(Some(relaunch::ColdAdmission::Immediate(chosen))) => chosen,
            Ok(None) => None,
            Ok(Some(relaunch::ColdAdmission::Deferred(_))) => {
                panic!("this injected environment fixture must admit immediately")
            }
            Err(_) => return COLD_ACCOUNT_WAIT.into(),
        };
        act(chosen.as_deref().unwrap_or(original))
    })
}

#[test]
fn claude_live_relaunch_rechecks_a_foreign_hold_before_executing() {
    let (a, job, hooks, mut snap) = fixture();
    assert_eq!(
        fresh_attempt(&job, &hooks, &mut snap, |_| "adopted".into()),
        "adopted"
    );
    hold(&a, true);
    let acted = AtomicBool::new(false);
    let step = fresh_attempt(&job, &hooks, &mut snap, |_| {
        acted.store(true, Ordering::SeqCst);
        "adopted".into()
    });
    assert_eq!(step, COLD_ACCOUNT_WAIT);
    assert!(
        !acted.load(Ordering::SeqCst),
        "a foreign-held account must not launch"
    );
}

fn live_case(case: i64) -> [i64; 4] {
    let (a, job, mut hooks, mut snap) = fixture();
    let original_dir = a.login_dir("work-2");
    let renewed = a.login_dir("work-2+1");
    snap.profile = Some(aterm_agent::harness::profile::Context {
        plain: a.tmp.path().join("home/.claude.json"),
        records: a.tmp.path().join("home/.claude"),
        selection: aterm_agent::harness::profile::Selection::Standard,
        home: Some(a.tmp.path().join("home").display().to_string()),
        config: None,
        login: Some(original_dir.clone()),
    });
    let before = snap.clone();
    {
        let mut kept = job.kept.lock().unwrap();
        kept.snapshot = Some(before.clone());
        kept.owed = Some(OwedRelaunch {
            snap: before.clone(),
            asked: None,
        });
    }
    match case {
        1 | 6 => hold(&a, true),
        2 => {
            a.store()
                .transact(a.clock(), &mut None, |state| {
                    state.prepared_accounts.insert("work-2".into());
                    Ok(Vec::new())
                })
                .unwrap();
        }
        3 => std::fs::write(a.store().logins_file(), "{").unwrap(),
        4 => {
            std::fs::create_dir_all(&renewed).unwrap();
            std::fs::write(Path::new(&renewed).join("a-login-file"), "{}").unwrap();
            a.store()
                .transact_logins(a.clock(), &mut None, |state, logins| {
                    Ok(accounts::note_renewed(
                        state,
                        logins,
                        "work-2",
                        Some(&original_dir),
                        &renewed,
                        None,
                        a.clock(),
                    ))
                })
                .unwrap();
        }
        5 => {
            a.store()
                .transact_logins(a.clock(), &mut None, |_, logins| {
                    logins.labels.insert(
                        "work-2".into(),
                        accounts::Binding {
                            dir: Some(original_dir.clone()),
                            ..Default::default()
                        },
                    );
                    Ok(Vec::new())
                })
                .unwrap();
            std::fs::write(a.roster_path(), "").unwrap();
        }
        _ => {}
    }
    if case == 6 {
        let mut cfg = on();
        cfg.set("accounts", "off").unwrap();
        job.switches.set(&cfg, false);
        crate::test_scratch::write_config(
            &a.tmp.path().join("config/aterm.toml"),
            "[harness]\naccounts='off'\n",
        );
    }
    if matches!(case, 7..=9) {
        let gate = Arc::clone(&hooks.acts.account_cold);
        let switches = Arc::clone(&job.switches);
        let revoked = Arc::clone(&job.revoked);
        let left = Arc::clone(&job.left);
        hooks.acts.account_cold = Arc::new(move |snapshot, mode, end| {
            let result = gate(snapshot, mode, end);
            if case == 7 {
                let mut cfg = on();
                cfg.set("accounts", "off").unwrap();
                switches.set(&cfg, false);
            } else if case == 8 {
                revoked.store(true, Ordering::SeqCst);
            } else {
                assert!(left.back());
            }
            result
        });
    }
    let mut executed = false;
    let mut binding = true;
    let step = fresh_attempt(&job, &hooks, &mut snap, |chosen| {
        executed = true;
        let expected = if case == 4 { &renewed } else { &original_dir };
        binding =
            chosen.login.as_deref() == Some(expected.as_str()) && chosen.profile == before.profile;
        "adopted".into()
    });
    let kept = job.kept.lock().unwrap();
    let captured = snap == before
        && kept.snapshot.as_ref() == Some(&before)
        && kept.owed.as_ref().is_some_and(|owed| owed.snap == before);
    [
        i64::from(executed),
        i64::from(step == COLD_ACCOUNT_WAIT),
        i64::from(binding),
        i64::from(captured),
    ]
}

pub(crate) fn live_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    if action == "NextCase" {
        next.insert("case", state["case"] + 1);
    } else {
        let [executed, waited, binding, captured] = live_case(state["case"]);
        next.insert("executed", executed);
        next.insert("waited", waited);
        next.insert("binding", binding);
        next.insert("captured", captured);
        next.insert("phase", 1);
    }
    next
}

#[test]
fn claude_live_admission_conforms_to_current_custody_and_preserves_capture() {
    let model = aterm_models_harness::claude_live_admission_model();
    aterm_spec::interp::prove_and_catch(&model);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = live_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} -> {next:?}"
                );
            }
        }
    }
    let mut denied = model.init_state();
    denied.insert("case", 1);
    denied = live_transition(&denied, "Attempt");
    denied.insert("executed", 1);
    assert!(!model.check_invariant("Admission", &denied));
    denied.insert("waited", 0);
    assert!(!model.check_invariant("RetainedWait", &denied));
}

#[test]
fn claude_live_account_wait_retries_without_losing_the_owed_conversation() {
    let (a, job, mut hooks, mut snap) = fixture();
    hold(&a, true);
    let before = snap.clone();
    job.kept.lock().unwrap().owed = Some(OwedRelaunch {
        snap: before.clone(),
        asked: None,
    });
    let pauses = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&pauses);
    hooks.pause = Arc::new(move |pause| {
        recorded.lock().unwrap().push(pause);
        Duration::ZERO
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    let accounts = Arc::clone(&a);
    let kept = Arc::clone(&job.kept);
    let gate = Arc::clone(&hooks.acts.account_cold);
    hooks.acts.account_cold = Arc::new(move |snapshot, mode, end| {
        let count = observed.fetch_add(1, Ordering::SeqCst) + 1;
        assert!(count <= 10, "a usable account must end the wait");
        {
            let kept = kept.lock().unwrap();
            assert!(kept.owed.as_ref().is_some_and(|owed| owed.snap == before));
            assert!(kept.relaunches.pending());
            assert_eq!(kept.relaunches.counts(), (false, false, 0, 0));
            assert!(!kept.relaunches.failing());
        }
        if count == 10 {
            hold(&accounts, false);
        }
        gate(snapshot, mode, end)
    });
    let mut executed = 0;
    relaunch_until(
        &job,
        &hooks,
        Duration::ZERO,
        &|| OnExit::Relaunch,
        None,
        || {
            fresh_attempt(&job, &hooks, &mut snap, |_| {
                executed += 1;
                "refused:test-complete".into()
            })
        },
    );
    assert_eq!(calls.load(Ordering::SeqCst), 10);
    assert_eq!(executed, 1);
    assert_eq!(
        *pauses.lock().unwrap(),
        [vec![Duration::ZERO], vec![relaunch::BUSY; 9]].concat(),
        "account recovery polls promptly without consuming the crash backoff"
    );
}

#[test]
fn claude_live_admission_rotates_from_a_signed_out_row_to_a_healthy_row() {
    let a = Accts::new();
    two_accounts(&a);
    let mut snap = a.spawn("s-live-cross-row", ACCT_TALK_A, "main");
    let idle = a.worker(&snap.tab, &on());
    let job = acct_job(&snap.tab, &idle);
    assert!(job.left.hand());
    let hooks = pool_hooks(&a);
    auth_at(&a, "main", accounts::Auth::SignedOut, a.clock());
    let before = snap.clone();
    let mut selected = None;
    let step = fresh_attempt(&job, &hooks, &mut snap, |chosen| {
        selected = chosen.login.clone();
        "adopted".into()
    });
    assert_eq!(step, "adopted");
    assert_eq!(selected, Some(a.login_dir("work-2")));
    assert_eq!(snap, before, "selection keeps the captured source");
}

#[test]
fn claude_live_failed_move_keeps_an_all_signed_out_conversation_pending() {
    let (a, idle) = moved_at_wall("s-live-failed-move");
    two_accounts(&a);
    let hooks = pool_hooks(&a);
    let job = acct_job(&idle.sid, &idle);
    assert!(job.left.hand());
    let mut snap = job.kept.lock().unwrap().snapshot.clone().unwrap();
    let before = snap.clone();
    job.kept.lock().unwrap().owed = Some(OwedRelaunch {
        snap: before.clone(),
        asked: None,
    });
    for label in ["main", "work-2"] {
        auth_at(&a, label, accounts::Auth::SignedOut, ACCT_NOW + 5);
    }
    let mut executed = 0;
    assert_eq!(
        fresh_attempt(&job, &hooks, &mut snap, |_| {
            executed += 1;
            "adopted".into()
        }),
        COLD_ACCOUNT_WAIT
    );
    assert_eq!(executed, 0);
    assert_eq!(snap, before);
    {
        let kept = job.kept.lock().unwrap();
        assert!(
            kept.account.moved.is_none(),
            "the failed move was reconciled"
        );
        assert_eq!(kept.owed.as_ref().unwrap().snap, before);
    }
    a.now.store(ACCT_NOW + 2 * 3600, Ordering::SeqCst);
    auth_at(&a, "work-2", accounts::Auth::Ok, a.clock());
    assert_eq!(
        fresh_attempt(&job, &hooks, &mut snap, |chosen| {
            executed += 1;
            assert_eq!(chosen.login, Some(a.login_dir("work-2")));
            "adopted".into()
        }),
        "adopted"
    );
    assert_eq!(executed, 1);
}

#[test]
fn claude_live_failed_move_rechecks_the_reverted_origin() {
    let (a, idle) = moved_at_wall("s-live-held-origin");
    two_accounts(&a);
    let mut roster = a.roster();
    roster
        .rows
        .iter_mut()
        .find(|row| row.label == "main")
        .unwrap()
        .held_by = Some("another-machine".into());
    std::fs::write(
        a.roster_path(),
        accounts::append_rows_for(accounts::Cli::Claude, "", &roster.rows).unwrap(),
    )
    .unwrap();
    let hooks = pool_hooks(&a);
    let job = acct_job(&idle.sid, &idle);
    assert!(job.left.hand());
    let mut snap = job.kept.lock().unwrap().snapshot.clone().unwrap();
    auth_at(&a, "work-2", accounts::Auth::SignedOut, ACCT_NOW + 5);
    let mut executed = false;
    assert_eq!(
        fresh_attempt(&job, &hooks, &mut snap, |_| {
            executed = true;
            "adopted".into()
        }),
        COLD_ACCOUNT_WAIT
    );
    assert!(!executed);
    assert_eq!(
        snap.login, None,
        "the return selected the original default login"
    );
    assert!(job.kept.lock().unwrap().account.moved.is_none());
}
