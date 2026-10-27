// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn replace_during_identity_read(idle: &mut WorkerIdle) {
    let original = Arc::clone(&idle.hooks.acts.account_env);
    let kept = Arc::clone(&idle.kept);
    let replaced = AtomicBool::new(false);
    idle.hooks.acts.account_env = Arc::new(move |snap| {
        let env = original(snap);
        if !replaced.swap(true, Ordering::SeqCst) {
            let mut k = kept.lock().unwrap();
            k.account.landing = Arc::default();
            k.account.came_back = Some(Some("/example/newer-return".into()));
            let mut process = k.snapshot.clone().unwrap();
            process.pid = u32::MAX;
            process.start = "newer-process".into();
            k.account.lost = Some(LostSeen { process, at: 7 });
        }
        env
    });
}

fn assert_newer_bookkeeping(idle: &WorkerIdle) {
    let k = idle.kept.lock().unwrap();
    assert_eq!(
        k.account.came_back.as_ref().and_then(|s| s.as_deref()),
        Some("/example/newer-return")
    );
    let lost = k.account.lost.as_ref().unwrap();
    assert_eq!(lost.process.pid, u32::MAX);
    assert_eq!(lost.process.start, "newer-process");
    assert_eq!(lost.at, 7);
}

#[test]
fn stale_signout_cannot_erase_a_newer_return_marker() {
    let a = Accts::new();
    a.spawn("s-local-signout", ACCT_TALK_A, "main");
    let mut idle = a.worker("s-local-signout", &on());
    replace_during_identity_read(&mut idle);
    idle.account_answered(AccountBad::SignedOut);
    assert_newer_bookkeeping(&idle);
}

#[test]
fn stale_advice_cannot_rewrite_a_newer_login_loss() {
    let a = Accts::new();
    a.spawn("s-local-advice", ACCT_TALK_A, "main");
    let mut idle = a.worker("s-local-advice", &on());
    replace_during_identity_read(&mut idle);
    let _ = idle.account(login_wall());
    assert_newer_bookkeeping(&idle);
}

fn untold() -> (Arc<Accts>, WorkerIdle, AccountLook) {
    let (a, idle) = account_landing::pending(false);
    assert_eq!(
        account_land(&idle.sid, &idle.kept, &idle.hooks, Landing::Asked),
        Applied::Yes
    );
    let look = idle.account_look(&a.roster()).unwrap();
    (a, idle, look)
}

#[test]
fn a_notice_cannot_be_read_by_an_older_generation() {
    let (a, idle, look) = untold();
    idle.kept.lock().unwrap().account.landing = Arc::default();
    assert_eq!(idle.account_untold(&a.seat(), &look, true), None);
    assert!(
        idle.kept
            .lock()
            .unwrap()
            .account
            .moved
            .as_ref()
            .unwrap()
            .untold
    );
}

#[test]
fn an_old_process_cannot_retire_the_current_notice() {
    let (a, idle, look) = untold();
    idle.kept
        .lock()
        .unwrap()
        .snapshot
        .as_mut()
        .unwrap()
        .argv
        .push("--new-auth-context".into());
    assert_eq!(idle.account_untold(&a.seat(), &look, false), None);
    assert!(
        idle.kept
            .lock()
            .unwrap()
            .account
            .moved
            .as_ref()
            .unwrap()
            .untold
    );
}

#[test]
fn current_notice_readers_can_retell_or_retire_their_notice() {
    let (a, idle, mut look) = untold();
    assert_eq!(
        idle.account_untold(&a.seat(), &look, true).as_deref(),
        Some("work-2")
    );
    assert!(
        idle.kept
            .lock()
            .unwrap()
            .account
            .moved
            .as_ref()
            .unwrap()
            .untold
    );
    assert_eq!(idle.account_untold(&a.seat(), &look, false), None);
    assert!(
        !idle
            .kept
            .lock()
            .unwrap()
            .account
            .moved
            .as_ref()
            .unwrap()
            .untold
    );

    idle.kept
        .lock()
        .unwrap()
        .account
        .moved
        .as_mut()
        .unwrap()
        .untold = true;
    look.snap.pid += 1;
    idle.kept.lock().unwrap().snapshot = Some(look.snap.clone());
    assert_eq!(idle.account_untold(&a.seat(), &look, true), None);
    assert!(
        !idle
            .kept
            .lock()
            .unwrap()
            .account
            .moved
            .as_ref()
            .unwrap()
            .untold
    );
}

#[test]
fn loss_time_tracks_authentication_context_and_allows_record_metadata() {
    let a = Accts::new();
    a.spawn("s-local-loss", ACCT_TALK_A, "main");
    let idle = a.worker("s-local-loss", &on());
    let old = idle.account_look(&a.roster()).unwrap();
    assert_eq!(idle.lost_seen(&old, login_wall(), 10), Some(10));
    let mut next = AccountLook {
        snap: old.snap.clone(),
        who: old.who.clone(),
        label: old.label.clone(),
        generation: Arc::clone(&old.generation),
    };
    next.snap.argv.push("--new-auth-context".into());
    idle.kept.lock().unwrap().snapshot = Some(next.snap.clone());
    assert_eq!(idle.lost_seen(&old, login_wall(), 20), None);
    assert_eq!(idle.lost_seen(&next, login_wall(), 20), Some(20));
    next.snap.cwd = "/example/new-directory".into();
    next.snap.session = Some("new-record".into());
    next.snap.version = Some("new-version".into());
    idle.kept.lock().unwrap().snapshot = Some(next.snap.clone());
    assert_eq!(idle.lost_seen(&next, login_wall(), 30), Some(20));

    {
        let mut k = idle.kept.lock().unwrap();
        k.account.lost = None;
        k.account.came_back = Some(next.snap.login.clone());
    }
    assert_eq!(idle.lost_seen(&next, login_wall(), 40), Some(i64::MIN));
    assert!(idle.kept.lock().unwrap().account.came_back.is_none());
}

fn local_case(case: i64) -> [i64; 3] {
    let job = left_job("s-local-model", Program::Claude, &on());
    let base = snap(&job.sid);
    let mut look = {
        let mut k = job.kept.lock().unwrap();
        k.snapshot = Some(base.clone());
        k.account.came_back = Some(None);
        k.account.lost = Some(LostSeen {
            process: base.clone(),
            at: 17,
        });
        AccountLook {
            who: accounts::ProcessLogin {
                login: None,
                default_home: true,
                stored_login: true,
            },
            label: None,
            snap: base,
            generation: Arc::clone(&k.account.landing),
        }
    };
    {
        let mut k = job.kept.lock().unwrap();
        match case {
            2 => k.account.landing = Arc::default(),
            3 => k.snapshot = None,
            4 => k
                .snapshot
                .as_mut()
                .unwrap()
                .argv
                .push("--other-auth".into()),
            5 => look.snap.home = Some("/example/other-home".into()),
            6 => {
                let snap = k.snapshot.as_mut().unwrap();
                snap.cwd = "/example/new-directory".into();
                snap.session = Some("new-record".into());
                snap.version = Some("new-version".into());
            }
            7 => {
                look.snap.cwd = "/example/new-directory".into();
                look.snap.session = Some("new-record".into());
                look.snap.version = Some("new-version".into());
            }
            8 => k.snapshot.as_mut().unwrap().pid += 1,
            1 => {}
            _ => unreachable!(),
        }
    }
    let before = job.kept.lock().unwrap().snapshot.clone();
    let calls = std::cell::Cell::new(0);
    let result = look.with_current(&job.kept, |k| {
        calls.set(calls.get() + 1);
        k.account.came_back = None;
        k.account.lost = None;
        123
    });
    let k = job.kept.lock().unwrap();
    assert_eq!(
        k.snapshot, before,
        "an edit does not rewrite the observed process"
    );
    let changed = k.account.came_back.is_none() && k.account.lost.is_none();
    if !changed {
        assert_eq!(k.account.came_back, Some(None));
        assert_eq!(k.account.lost.as_ref().unwrap().at, 17);
    }
    [
        calls.get(),
        i64::from(changed),
        i64::from(result == Some(123)),
    ]
}

pub(crate) fn local_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Choose" => {
            next.insert("phase", 1);
        }
        "Apply" => {
            next.insert("phase", 2);
            for (key, value) in ["calls", "changed", "returned"]
                .into_iter()
                .zip(local_case(state["case"]))
            {
                next.insert(key, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn local_observation_conforms_before_any_callback_mutation() {
    let model = aterm_models_harness::account_local_observation_model();
    assert_eq!(model.mutant_dials(), (1..=4).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 8);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = local_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (case, key, value, law) in [
        (2, "calls", 1, "CurrentCalls"),
        (3, "changed", 1, "GuardedEdit"),
        (4, "returned", 1, "HonestReturn"),
        (1, "calls", 2, "AtMostOnce"),
    ] {
        let mut state = model.init_state();
        state.insert("phase", 1);
        state.insert("case", case);
        let mut bad = local_transition(&state, "Apply");
        bad.insert(key, value);
        assert!(!model.check_invariant(law, &bad));
    }
}
