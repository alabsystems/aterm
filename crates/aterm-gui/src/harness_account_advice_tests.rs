// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn ready(sid: &str) -> (Arc<Accts>, WorkerIdle) {
    let a = Accts::new();
    a.spawn(sid, ACCT_TALK_A, "main");
    a.wall(ACCT_TALK_A, ACCT_NOW - 10);
    let idle = a.worker(sid, &on());
    assert_eq!(moves(&idle.account(usage_wall())), Some("work-2"));
    (a, idle)
}

#[test]
fn a_mismatched_restart_preserves_the_current_advice() {
    let (a, idle) = ready("s-advice-mismatch");
    let before = idle.kept.lock().unwrap().account.advised.clone();
    assert_eq!(
        idle.restart(&move_to("pool-a", AccountAt::Wall)).as_deref(),
        Some("wait:account-changed")
    );
    assert_eq!(idle.kept.lock().unwrap().account.advised, before);
    assert_eq!(a.asked("restart:"), 0);
    assert_eq!(
        idle.restart(&move_to("work-2", AccountAt::Wall)).as_deref(),
        Some("adopted")
    );
    assert_eq!(a.asked("restart:"), 1);
}

#[test]
fn a_busy_landing_gate_preserves_advice_for_retry() {
    let (a, idle) = ready("s-advice-busy");
    let before = idle.kept.lock().unwrap().account.advised.clone();
    let gate = Arc::clone(&idle.kept.lock().unwrap().account.landing);
    let held = gate.lock().unwrap();
    assert_eq!(
        idle.restart(&move_to("work-2", AccountAt::Wall)).as_deref(),
        Some("wait:account-unverified")
    );
    assert_eq!(idle.kept.lock().unwrap().account.advised, before);
    assert_eq!(a.asked("restart:"), 0);
    drop(held);
    assert_eq!(
        idle.restart(&move_to("work-2", AccountAt::Wall)).as_deref(),
        Some("adopted")
    );
    assert_eq!(a.asked("restart:"), 1);
}

#[test]
fn an_old_calculation_cannot_clear_a_new_generations_advice() {
    let (a, mut idle) = ready("s-advice-old-publish");
    let original = Arc::clone(&idle.hooks.acts.account_env);
    let kept = Arc::clone(&idle.kept);
    let replaced = AtomicBool::new(false);
    idle.hooks.acts.account_env = Arc::new(move |snap| {
        let env = original(snap);
        if !replaced.swap(true, Ordering::SeqCst) {
            kept.lock().unwrap().account.landing = Arc::default();
            let request = AdviceTurn::begin(&kept).capture(&kept).unwrap();
            let _ = request.publish(
                &kept,
                Some(AccountAdvice::Move {
                    to: "pool-a".into(),
                }),
                AccountPoint::TurnEnd,
            );
        }
        env
    });
    assert_eq!(idle.account(usage_wall()), None);
    let k = idle.kept.lock().unwrap();
    let advised = k
        .account
        .advised
        .as_ref()
        .expect("the new generation's advice");
    assert_eq!(advised.to, "pool-a");
    assert_eq!(advised.point, AccountPoint::TurnEnd);
    assert_eq!(a.asked("restart:"), 0);
}

fn move_advice(to: &str) -> Option<AccountAdvice> {
    Some(AccountAdvice::Move { to: to.into() })
}

fn publish_label(kept: &Mutex<Kept>, to: Option<&str>) -> Option<AccountAdvice> {
    AdviceTurn::begin(kept).capture(kept).unwrap().publish(
        kept,
        to.and_then(move_advice),
        usage_wall(),
    )
}

fn advice_label(advice: Option<AccountAdvice>) -> i64 {
    match advice {
        None => 0,
        Some(AccountAdvice::Move { to }) if to == "first" => 1,
        Some(AccountAdvice::Move { to }) if to == "second" => 2,
        _ => unreachable!(),
    }
}

fn queue_state(job: &WorkerJob, returned: i64, taken: bool, cancelled: bool) -> [i64; 5] {
    let k = job.kept.lock().unwrap();
    let (queued, usable) = k.account.advised.as_ref().map_or((0, false), |a| {
        (
            if a.to == "first" {
                1
            } else {
                assert_eq!(a.to, "second");
                2
            },
            a.request.matches_context(&k),
        )
    });
    [
        returned,
        queued,
        i64::from(taken),
        i64::from(usable),
        i64::from(cancelled),
    ]
}

fn pending_move(snap: &Snapshot) -> Flying {
    Flying {
        origin: AccountContext {
            roster: None,
            state: "/example/state".into(),
            home: "/example".into(),
        },
        to: "first".into(),
        at: AccountAt::Wall,
        login: snap.login.clone(),
        pid: snap.pid,
        start: snap.start.clone(),
        rejected: None,
    }
}

fn queue_case(case: i64) -> [i64; 5] {
    let job = left_job("s-advice-model", Program::Claude, &on());
    job.kept.lock().unwrap().snapshot = Some(snap(&job.sid));
    let turn = AdviceTurn::begin(&job.kept);
    if case == 14 {
        assert_eq!(advice_label(publish_label(&job.kept, Some("second"))), 2);
        assert!(
            turn.capture(&job.kept).is_none(),
            "an older preflight cannot overtake"
        );
        return queue_state(&job, 0, false, false);
    }
    if case == 15 {
        job.kept
            .lock()
            .unwrap()
            .snapshot
            .as_mut()
            .unwrap()
            .argv
            .push("--preflight-context".into());
    }
    if case == 16 {
        job.kept.lock().unwrap().snapshot = None;
    }
    let request = turn.capture(&job.kept).unwrap();
    if matches!(case, 2..=5) {
        if case == 5 {
            job.kept.lock().unwrap().account.landing = Arc::default();
        }
        let _ = publish_label(&job.kept, (case != 4).then_some("second"));
        let returned = request.publish(
            &job.kept,
            if case == 3 {
                None
            } else {
                move_advice("first")
            },
            usage_wall(),
        );
        return queue_state(&job, advice_label(returned), false, false);
    }
    if case == 6 {
        job.kept
            .lock()
            .unwrap()
            .snapshot
            .as_mut()
            .unwrap()
            .argv
            .push("--other-auth".into());
    }
    if case == 7 {
        job.kept.lock().unwrap().snapshot.as_mut().unwrap().session = Some("new-record".into());
    }
    if case == 21 {
        let mut k = job.kept.lock().unwrap();
        k.account.flying = Some(pending_move(k.snapshot.as_ref().unwrap()));
    }
    let mut returned = advice_label(request.publish(&job.kept, move_advice("first"), usage_wall()));
    if case == 17 {
        returned = advice_label(publish_label(&job.kept, None));
    }
    let target = if case == 8 { "second" } else { "first" };
    let peek = with_kept(&job, |k| advised_for(k, target, case != 9));
    if case == 10 {
        let _ = publish_label(&job.kept, Some("second"));
    }
    if case == 11 {
        job.kept.lock().unwrap().snapshot = None;
    }
    if case == 12 {
        job.kept.lock().unwrap().snapshot.as_mut().unwrap().login =
            Some("/example/new-login".into());
    }
    if case == 18 {
        job.kept.lock().unwrap().account.landing = Arc::default();
    }
    if case == 19 {
        job.kept.lock().unwrap().snapshot.as_mut().unwrap().cwd = "/example/new-directory".into();
    }
    let pending = (case == 20).then(|| AdviceTurn::begin(&job.kept));
    let taken = if matches!(case, 8..=13 | 18..=20) {
        peek.is_some_and(|a| {
            let gate = Arc::clone(&a.request.turn.generation);
            let _held = gate.lock().unwrap();
            take_advised(&job.kept, &a).is_some()
        })
    } else {
        false
    };
    let cancelled = pending.is_some_and(|turn| turn.capture(&job.kept).is_none());
    queue_state(&job, returned, taken, cancelled)
}

#[test]
fn a_pending_move_still_allows_wait_advice() {
    let job = left_job("s-advice-wait", Program::Claude, &on());
    let snap = snap(&job.sid);
    {
        let mut k = job.kept.lock().unwrap();
        k.account.flying = Some(pending_move(&snap));
        k.snapshot = Some(snap);
    }
    let wait = AccountAdvice::Wait {
        until: Instant::now(),
    };
    let request = AdviceTurn::begin(&job.kept).capture(&job.kept).unwrap();
    assert_eq!(
        request.publish(&job.kept, Some(wait.clone()), usage_wall()),
        Some(wait)
    );
    assert!(job.kept.lock().unwrap().account.advised.is_none());
}

pub(crate) fn queue_transition(
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
            for (key, value) in ["returned", "queued", "taken", "usable", "cancelled"]
                .into_iter()
                .zip(queue_case(state["case"]))
            {
                next.insert(key, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn advice_queue_conforms_across_publication_and_claim_races() {
    let model = aterm_models_harness::account_advice_queue_model();
    assert_eq!(model.mutant_dials(), (1..=17).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 20);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = queue_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (case, key, value, law) in [
        (2, "queued", 1, "QueueIdentity"),
        (6, "returned", 1, "CurrentPublication"),
        (8, "taken", 1, "CurrentClaim"),
        (18, "usable", 1, "QueueContext"),
        (20, "cancelled", 0, "ClaimInvalidatesProducer"),
    ] {
        let mut state = model.init_state();
        state.insert("phase", 1);
        state.insert("case", case);
        let mut bad = queue_transition(&state, "Apply");
        bad.insert(key, value);
        assert!(!model.check_invariant(law, &bad));
    }
}
