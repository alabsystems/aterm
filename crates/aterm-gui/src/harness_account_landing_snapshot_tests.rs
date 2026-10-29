// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn mode(value: i64) -> Landing {
    match value {
        0 => Landing::Immediate,
        1 => Landing::Carried,
        2 => Landing::Asked,
        _ => unreachable!(),
    }
}

fn context(a: &Accts, idle: &WorkerIdle, observed: &Snapshot, value: i64) {
    match value {
        0 | 1 => {
            let mut snap = observed.clone();
            if value == 1 {
                snap.version = Some("enriched".into());
                snap.cwd = "new-directory".into();
            }
            a.tabs.lock().unwrap().insert(idle.sid.clone(), snap);
            assert!(refresh_snapshot(&idle.sid, &idle.kept, &idle.hooks));
        }
        2 => {
            a.spawn(&idle.sid, ACCT_TALK_B, "main");
            assert!(refresh_snapshot(&idle.sid, &idle.kept, &idle.hooks));
        }
        3 => with_kept_of(&idle.kept, |k| k.snapshot = None),
        _ => unreachable!(),
    }
}

fn images(a: &Accts) -> [Option<Vec<u8>>; 4] {
    let store = a.store();
    [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
    ]
    .map(|path| std::fs::read(path).ok())
}

fn run(state: &aterm_spec::interp::State, action: &str) -> [(&'static str, i64); 9] {
    let (a, idle) = super::account_landing::pending(state["kind"] == 1);
    let gate = with_kept_of(&idle.kept, |k| Arc::clone(&k.account.landing));
    let observed = with_kept_of(&idle.kept, |k| k.snapshot.clone().unwrap());
    let mut quiet;
    let recorded;
    let mut published = false;
    {
        let _held = gate.lock().unwrap();
        let pending =
            PendingAccountLanding::observe(&idle.sid, &idle.kept, &idle.hooks, &gate).unwrap();
        context(&a, &idle, &observed, state["before"]);
        let before = images(&a);
        let receipt = pending.record(&idle.sid, &idle.kept, &idle.hooks, &gate);
        recorded = receipt.is_some();
        quiet = images(&a) == before;
        if recorded && action != "Record" {
            context(&a, &idle, &observed, state["after"]);
            let before = images(&a);
            let current = with_kept_of(&idle.kept, |k| k.snapshot.clone());
            let result = receipt.unwrap().publish(
                &idle.sid,
                &idle.kept,
                &idle.hooks,
                mode(state["mode"]),
                &gate,
            );
            assert_eq!(with_kept_of(&idle.kept, |k| k.snapshot.clone()), current);
            published = result != Applied::Unread;
            assert_eq!(published, state["after"] <= 1);
            quiet = images(&a) == before;
        }
    }
    if action == "Poll" || state["polls"] == 1 {
        assert!(!recorded);
        context(&a, &idle, &observed, 0);
        let a_for_hook = Arc::clone(&a);
        let kept = Arc::clone(&idle.kept);
        let hooks = idle.hooks.clone();
        let sid = idle.sid.clone();
        let value = state["before"];
        a.at_seat_read(2, move |_| {
            // The hook runs at the writer after the wrapper's observation.
            match value {
                2 => {
                    a_for_hook.spawn(&sid, ACCT_TALK_B, "main");
                    assert!(refresh_snapshot(&sid, &kept, &hooks));
                }
                3 => with_kept_of(&kept, |k| k.snapshot = None),
                _ => unreachable!(),
            }
        });
        let before = images(&a);
        assert_eq!(
            account_land(&idle.sid, &idle.kept, &idle.hooks, mode(state["mode"])),
            Applied::Unread
        );
        quiet = images(&a) == before;
    }
    let retry = action == "Retry" || (action == "Replay" && state["phase"] == 4);
    if action == "Repair" || retry {
        assert!(!recorded);
        let before = images(&a);
        context(&a, &idle, &observed, 0);
        quiet = images(&a) == before;
    }
    if retry {
        let before = images(&a);
        assert_eq!(
            account_land(&idle.sid, &idle.kept, &idle.hooks, mode(state["mode"])),
            if state["kind"] == 0 {
                Applied::Yes
            } else {
                Applied::No
            }
        );
        quiet = images(&a) == before;
        published = true;
    }
    if action == "Replay" {
        let before = images(&a);
        assert_eq!(
            account_land(&idle.sid, &idle.kept, &idle.hooks, mode(state["mode"])),
            Applied::Unread
        );
        quiet = images(&a) == before;
    }
    let ledger = a.ledger();
    let struck = ledger.iter().filter(|word| **word == "struck").count();
    let history = ledger
        .iter()
        .filter(|word| **word == "struck" || **word == "move")
        .count();
    let kept = idle.kept.lock().unwrap();
    [
        ("recorded", i64::from(recorded || retry)),
        ("history", i64::try_from(history).unwrap()),
        ("pending", i64::from(kept.account.flying.is_some())),
        ("published", i64::from(published)),
        ("moved", i64::from(kept.account.moved.is_some())),
        ("notified", i64::try_from(a.asked("made:")).unwrap()),
        ("struck", i64::try_from(struck).unwrap()),
        ("quiet", i64::from(quiet)),
        ("polls", i64::from(action == "Poll" || state["polls"] == 1)),
    ]
}

/// The model fields `run` reads: action, phase, polls, kind, before, mode and after.
type RunKey = (String, i64, i64, i64, i64, i64, i64);

/// `run`, done once per distinct input: model states that differ only in fields
/// `run` never reads share one real run. `after` is read only when a recorded
/// completion (`before <= 1`) is published by an action other than Record, and
/// `mode` is never read by Record or by a Repair that did not poll.
fn run_once(state: &aterm_spec::interp::State, action: &str) -> [(&'static str, i64); 9] {
    thread_local! {
        static RUNS: std::cell::RefCell<std::collections::HashMap<RunKey, [(&'static str, i64); 9]>> =
            std::cell::RefCell::default();
    }
    let after = if state["before"] <= 1 && action != "Record" {
        state["after"]
    } else {
        0
    };
    let mode = if action == "Record" || (action == "Repair" && state["polls"] == 0) {
        0
    } else {
        state["mode"]
    };
    let key = (
        action.to_owned(),
        state["phase"],
        state["polls"],
        state["kind"],
        state["before"],
        mode,
        after,
    );
    if let Some(seen) = RUNS.with_borrow(|runs| runs.get(&key).copied()) {
        return seen;
    }
    let fresh = run(state, action);
    RUNS.with_borrow_mut(|runs| runs.insert(key, fresh));
    fresh
}

pub(crate) fn snapshot_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Before" | "After" | "Kind" | "Mode" => {
            account_landing_snapshot_environment();
            let key = match action {
                "Before" => "before",
                "After" => "after",
                "Kind" => "kind",
                _ => "mode",
            };
            next.insert(key, state[key] + 1);
        }
        "Record" | "Publish" | "Poll" | "Repair" | "Retry" | "Replay" => {
            if action == "Repair" {
                account_landing_snapshot_environment();
            }
            for (field, value) in run_once(state, action) {
                next.insert(field, value);
            }
            next.insert(
                "phase",
                match action {
                    "Record" | "Poll" => 1,
                    "Publish" => 2,
                    "Repair" => 3,
                    "Retry" => 4,
                    _ => 5,
                },
            );
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn snapshot_completion_conforms_to_record_and_publication_model() {
    let model = aterm_models_harness::account_landing_snapshot_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = snapshot_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    // These corruptions must fail even when the scalar interpreter itself passes.
    for (before, after, field, value, law) in [
        (2, 0, "recorded", 1, "RecordPolicy"),
        (0, 2, "published", 1, "PublishPolicy"),
        (0, 2, "pending", 1, "Retirement"),
        (0, 0, "history", 0, "History"),
        (0, 0, "moved", 0, "Moves"),
        (0, 0, "notified", 0, "Notification"),
    ] {
        let mut state = model.init_state();
        state.insert("mode", 1);
        state.insert("before", before);
        state.insert("after", after);
        let good = snapshot_transition(&state, "Record");
        let mut bad = if before <= 1 {
            snapshot_transition(&good, "Publish")
        } else {
            good
        };
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad), "{law}");
    }
}

#[test]
fn recorded_completion_leaves_a_newer_generation_or_attempt_intact() {
    for new_gate in [false, true] {
        for same_login in [false, true] {
            let (a, idle) = super::account_landing::pending(same_login);
            let gate = with_kept_of(&idle.kept, |k| Arc::clone(&k.account.landing));
            let _held = gate.lock().unwrap();
            let receipt = PendingAccountLanding::observe(&idle.sid, &idle.kept, &idle.hooks, &gate)
                .unwrap()
                .record(&idle.sid, &idle.kept, &idle.hooks, &gate)
                .unwrap();
            with_kept_of(&idle.kept, |k| {
                if new_gate {
                    k.account.landing = Arc::new(Mutex::new(()));
                } else {
                    k.account
                        .flying
                        .as_mut()
                        .unwrap()
                        .start
                        .push_str("-new-attempt");
                }
            });
            let (generation, flight, snapshot) = with_kept_of(&idle.kept, |k| {
                (
                    Arc::clone(&k.account.landing),
                    k.account.flying.clone(),
                    k.snapshot.clone(),
                )
            });
            let before = images(&a);
            assert_eq!(
                receipt.publish(&idle.sid, &idle.kept, &idle.hooks, Landing::Carried, &gate),
                Applied::Unread
            );
            assert_eq!(images(&a), before);
            with_kept_of(&idle.kept, |k| {
                assert!(Arc::ptr_eq(&generation, &k.account.landing));
                assert_eq!(k.account.flying, flight);
                assert_eq!(k.snapshot, snapshot);
                assert!(k.account.moved.is_none());
            });
            assert_eq!(a.asked("made:"), 0);
        }
    }
}
