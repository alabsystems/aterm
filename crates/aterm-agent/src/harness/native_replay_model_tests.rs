// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;

fn seed() -> St {
    static ORIGINAL: std::sync::OnceLock<St> = std::sync::OnceLock::new();
    ORIGINAL
        .get_or_init(|| {
            let root = aterm_tempfile::tempdir().unwrap();
            lost_prepare(&opts(root.path()), &planned())
        })
        .clone()
}

/// Execute one real replay with a lost response and optional directory failure.
/// Bypassing recovery is the negative control for the old dispatch ordering.
fn attempt(state: &State, bypass: bool) -> (usize, bool) {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut opts = opts(root.path());
    opts.dry_run = state["dry"] == 1;
    let mut st = seed();
    st.native_retired_unissued = state["held"] == 1;
    let original = st.clone();
    let expected = Request::parse(&st.native_prepare).unwrap();
    let (mut client, worker) = endpoint(move |line| {
        assert_eq!(request(line), expected, "replay keeps the exact operation");
        None
    });
    let fault = (state["barrier_failed"] == 1)
        .then(|| aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported));
    if bypass {
        assert!(upgrade_drive::save_checked(&opts, "conversation", &st).is_err());
        let parsed = Request::parse(&st.native_prepare).unwrap();
        assert!(native_act::exchange(&mut client, SID, &parsed).is_err());
    } else {
        let result = recover(&opts, "conversation", &mut client, SID, &mut st);
        assert_eq!(
            result,
            if state["held"] == 1 {
                Ok(Recovery::Held)
            } else if opts.dry_run {
                Err("native-dry-run")
            } else if fault.is_some() {
                Err("native-unsaved")
            } else {
                Err("native-unknown")
            }
        );
    }
    drop(fault);
    drop(client);
    (worker.join().unwrap().len(), st == original)
}

fn observed(state: &State, dispatched: usize, intact: bool) -> State {
    let mut next = state.clone();
    next.insert("attempts", state["attempts"] + 1);
    next.insert("sent", state["sent"] + i64::try_from(dispatched).unwrap());
    for (condition, violation) in [
        ("barrier_failed", "unpublished"),
        ("dry", "dry_sent"),
        ("held", "held_sent"),
    ] {
        if dispatched != 0 && state[condition] == 1 {
            next.insert(violation, 1);
        }
    }
    if !intact {
        next.insert("changed", 1);
    }
    next
}

pub(super) fn transition(state: &State, action: &str) -> State {
    if action == "Attempt" {
        let (dispatched, intact) = attempt(state, false);
        return observed(state, dispatched, intact);
    }
    let mut next = state.clone();
    let (field, value) = match action {
        "FailBarrier" => ("barrier_failed", 1),
        "RepairBarrier" => ("barrier_failed", 0),
        "DryRun" => ("dry", 1),
        "LiveRun" => ("dry", 0),
        "Hold" => ("held", 1),
        _ => unreachable!("unmapped replay environment input"),
    };
    next.insert(field, value);
    next
}

#[test]
fn replay_publication_refines_real_persistence_and_transport() {
    let model = aterm_models_harness::native_replay_publication_model();
    aterm_spec::interp::prove_and_catch(&model);
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&model));
    assert!(faults.is_empty(), "{}", faults.join("\n"));
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
}

#[test]
fn replay_publication_detects_dispatch_after_a_failed_save() {
    let model = aterm_models_harness::native_replay_publication_model();
    let mut before = model.init_state();
    assert!(model.fire("FailBarrier", &mut before));
    let (dispatched, intact) = attempt(&before, true);
    assert_eq!(dispatched, 1, "the negative control must reach the wire");
    let bad = observed(&before, dispatched, intact);
    assert!(!model.check_invariant("PublicationBeforeDispatch", &bad));
    assert!(!model.successors("Attempt", &before).contains(&bad));
}
