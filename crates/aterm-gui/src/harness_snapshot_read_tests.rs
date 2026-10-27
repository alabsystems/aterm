// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_restart_read_cannot_replace_a_newer_account_generation() {
    let a = Accts::new();
    let sid = "s-restart-read-retired";
    a.spawn(sid, ACCT_TALK_A, "main");
    a.wall(ACCT_TALK_A, ACCT_NOW - 10);
    let mut idle = a.worker(sid, &on());
    assert_eq!(moves(&idle.account(usage_wall())), Some("work-2"));
    let replacement = Snapshot {
        pid: 9898,
        session: Some(ACCT_TALK_B.into()),
        ..snap(sid)
    };
    let newer = replacement.clone();
    let kept = Arc::clone(&idle.kept);
    let original = Arc::clone(&idle.hooks.acts.snapshot);
    idle.hooks.acts.snapshot = Arc::new(move |sid| {
        let old = original(sid);
        let mut k = kept.lock().unwrap();
        k.account = AccountKept::default();
        k.snapshot = Some(newer.clone());
        old
    });
    assert_eq!(
        idle.restart(&move_to("work-2", AccountAt::Wall)).as_deref(),
        Some("wait:account-changed")
    );
    let k = idle.kept.lock().unwrap();
    assert_eq!(k.snapshot.as_ref(), Some(&replacement));
    assert!(k.account.flying.is_none());
    assert!(k.account.moved.is_none());
    assert_eq!(a.ledger(), ["wall", "rotate"]);
}

#[test]
fn a_missing_restart_read_does_not_follow_a_newer_process() {
    let a = Accts::new();
    let sid = "s-restart-read-missing";
    a.spawn(sid, ACCT_TALK_A, "main");
    let mut idle = a.worker(sid, &on());
    let replacement = Snapshot {
        pid: 9899,
        ..snap(sid)
    };
    let newer = replacement.clone();
    let kept = Arc::clone(&idle.kept);
    idle.hooks.acts.snapshot = Arc::new(move |_| {
        kept.lock().unwrap().snapshot = Some(newer.clone());
        None
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let followed = Arc::clone(&calls);
    idle.hooks.acts.follow = Arc::new(move |_, _| {
        followed.fetch_add(1, Ordering::SeqCst);
        Foreground::Other
    });
    assert!(!refresh_snapshot(sid, &idle.kept, &idle.hooks));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        idle.kept.lock().unwrap().snapshot.as_ref(),
        Some(&replacement)
    );
}

#[test]
fn an_idle_read_cannot_cross_a_generation_with_the_same_snapshot() {
    let a = Accts::new();
    let sid = "s-point-read-retired";
    let before = a.spawn(sid, ACCT_TALK_A, "main");
    let mut idle = a.worker(sid, &on());
    let kept = Arc::clone(&idle.kept);
    idle.hooks.acts.snapshot = Arc::new(move |sid| {
        kept.lock().unwrap().account = AccountKept::default();
        Some(Snapshot {
            pid: 9900,
            ..snap(sid)
        })
    });
    let read = start_point_read(sid, &idle.kept, &idle.hooks, None);
    assert!(read.wait(aterm_spec::wait::MUST_HAPPEN));
    assert_eq!(idle.kept.lock().unwrap().snapshot.as_ref(), Some(&before));
}

/// Each case changes the context inside real snapshot/follow hooks, outside the state lock.
fn read_case(case: i64) -> [i64; 4] {
    let sid = "s-snapshot-read-model";
    let world = Arc::new(World::default());
    let mut h = hooks(&world, parking_body(&world));
    let kept = Arc::new(Mutex::new(Kept::default()));
    kept.lock().unwrap().snapshot = (!matches!(case, 5 | 6 | 18)).then(|| snap(sid));
    let revoked = Arc::new(AtomicBool::new(case == 15));
    let reads = Arc::new(AtomicUsize::new(0));
    let follows = Arc::new(AtomicUsize::new(0));
    let returned_snapshot = Snapshot {
        pid: 9191,
        ..snap(sid)
    };
    let from_read = returned_snapshot.clone();
    let state = Arc::clone(&kept);
    let cancels = Arc::clone(&revoked);
    let calls = Arc::clone(&reads);
    h.acts.snapshot = Arc::new(move |_| {
        calls.fetch_add(1, Ordering::SeqCst);
        let mut k = state.lock().unwrap();
        match case {
            2 | 5 | 11 => {
                k.snapshot = Some(Snapshot {
                    pid: 9292,
                    ..snap(sid)
                })
            }
            3 | 12 => k.account = AccountKept::default(),
            4 => k.snapshot = None,
            7 => k.snapshot.as_mut().unwrap().session = Some("newer-conversation".into()),
            16 => cancels.store(true, Ordering::SeqCst),
            _ => {}
        }
        (case <= 7 || case == 15 || case == 16).then(|| from_read.clone())
    });
    let state = Arc::clone(&kept);
    let cancels = Arc::clone(&revoked);
    let calls = Arc::clone(&follows);
    h.acts.follow = Arc::new(move |_, observed| {
        assert_eq!(observed, &snap(sid), "follow the original captured process");
        calls.fetch_add(1, Ordering::SeqCst);
        let mut k = state.lock().unwrap();
        match case {
            13 | 21 => {
                k.snapshot = Some(Snapshot {
                    pid: 9393,
                    ..snap(sid)
                })
            }
            14 | 20 => k.account = AccountKept::default(),
            17 => cancels.store(true, Ordering::SeqCst),
            _ => {}
        }
        observed.session = Some("followed-conversation".into());
        match case {
            9 | 21 => Foreground::Other,
            10 => Foreground::Shell,
            _ => Foreground::Agent,
        }
    });
    let returned = if matches!(case, 19 | 20) {
        follow_snapshot_guarded(sid, &kept, &h, Some(&revoked))
    } else {
        refresh_snapshot_guarded(sid, &kept, &h, Some(&revoked))
    };
    let k = kept.lock().unwrap();
    let published = if matches!(case, 9 | 21) {
        k.snapshot.is_none()
    } else {
        k.snapshot.as_ref().is_some_and(|current| {
            current == &returned_snapshot
                || current.session.as_deref() == Some("followed-conversation")
        })
    };
    [
        i64::try_from(reads.load(Ordering::SeqCst)).unwrap(),
        i64::try_from(follows.load(Ordering::SeqCst)).unwrap(),
        i64::from(published),
        i64::from(returned),
    ]
}

pub(crate) fn read_transition(
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
        "Observe" => {
            let values = read_case(state["case"]);
            next.insert("phase", 2);
            for (name, value) in ["reads", "follows", "published", "returned"]
                .into_iter()
                .zip(values)
            {
                next.insert(name, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn snapshot_reads_publish_only_into_the_context_that_started_them() {
    let model = aterm_models_harness::harness_snapshot_read_model();
    assert_eq!(model.mutant_dials(), (1..=22).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) >= 21);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = read_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (case, field, value, law) in [
        (3, "published", 1, "Publication"),
        (11, "follows", 1, "FallbackAdmission"),
        (15, "reads", 1, "ReadAdmission"),
        (8, "returned", 1, "ReadResult"),
    ] {
        let mut state = model.init_state();
        state.insert("phase", 1);
        state.insert("case", case);
        let mut bad = read_transition(&state, "Observe");
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad));
    }
}
