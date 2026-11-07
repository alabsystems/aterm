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

fn retirement_attempt(state: &State, bypass: bool) -> (usize, bool) {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut opts = opts(root.path());
    let mut st = seed();
    let Request::Prepare(plan) = Request::parse(&st.native_prepare).unwrap() else {
        panic!("the original preparation")
    };
    st.native = Some(Binding {
        id: plan.id.clone(),
        capability: binding().capability,
    });
    upgrade_drive::save_checked(&opts, "conversation", &st).unwrap();
    let mut st = upgrade_drive::load(&opts, "conversation").unwrap();
    st.native_retired_unissued = state["held"] == 1;
    opts.dry_run = state["dry"] == 1;
    let original = st.clone();
    let expected = Request::Reconcile(st.native.clone().unwrap());
    let request_copy = expected.clone();
    let (mut client, worker) = endpoint(move |line| {
        assert_eq!(request(line), request_copy, "retirement keeps its binding");
        None
    });
    let fault = (state["barrier_failed"] == 1)
        .then(|| aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported));
    if bypass {
        assert!(upgrade_drive::save_checked(&opts, "conversation", &st).is_err());
        assert!(native_act::exchange(&mut client, SID, &expected).is_err());
    } else {
        assert!(!retire(&opts, "conversation", &mut client, SID, &mut st));
    }
    drop(fault);
    drop(client);
    (worker.join().unwrap().len(), st == original)
}

fn renewal_attempt(state: &State, bypass: bool) -> (usize, bool) {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut opts = opts(root.path());
    let mut st = seed();
    let Request::Prepare(prepare) = Request::parse(&st.native_prepare).unwrap() else {
        panic!("the original preparation")
    };
    st.native = Some(Binding {
        id: prepare.id.clone(),
        capability: binding().capability,
    });
    let key = format!("renewal-{}", prepare.id);
    upgrade_drive::save_checked(&opts, &key, &st).unwrap();
    let mut st = upgrade_drive::load_record(&opts, &key).unwrap().unwrap();
    st.native_retired_unissued = state["held"] == 1;
    upgrade_drive::save_checked(&opts, &key, &st).unwrap();
    opts.dry_run = state["dry"] == 1;
    let original = st.clone();
    let expected = Request::Signal(st.native.clone().unwrap());
    let request_copy = expected.clone();
    let (mut client, worker) = endpoint(move |line| {
        assert_eq!(request(line), request_copy, "renewal keeps its binding");
        None
    });
    let fault = (state["barrier_failed"] == 1)
        .then(|| aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported));
    if bypass {
        assert!(upgrade_drive::save_checked(&opts, &key, &st).is_err());
        assert!(native_act::exchange(&mut client, SID, &expected).is_err());
        drop(client);
    } else {
        assert!(
            super::super::super::codex_renewal::drive_connection(
                &opts, SID, &prepare, st, client, true
            )
            .is_none()
        );
    }
    drop(fault);
    let mut saved = upgrade_drive::load_record(&opts, &key).unwrap().unwrap();
    saved.progress_at = original.progress_at;
    saved.progress_key.clone_from(&original.progress_key);
    (worker.join().unwrap().len(), saved == original)
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

pub(super) fn retirement_transition(state: &State, action: &str) -> State {
    if action == "Attempt" {
        let (dispatched, intact) = retirement_attempt(state, false);
        observed(state, dispatched, intact)
    } else {
        transition(state, action)
    }
}

pub(super) fn renewal_transition(state: &State, action: &str) -> State {
    if action == "Attempt" {
        let (dispatched, intact) = renewal_attempt(state, false);
        observed(state, dispatched, intact)
    } else {
        transition(state, action)
    }
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
                let retired = retirement_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&retired),
                    "retirement {}: {state:?} -> {retired:?}",
                    action.name
                );
                let renewed = renewal_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&renewed),
                    "renewal {}: {state:?} -> {renewed:?}",
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

#[test]
fn retirement_publication_detects_dispatch_after_a_failed_save() {
    let model = aterm_models_harness::native_replay_publication_model();
    let mut before = model.init_state();
    assert!(model.fire("FailBarrier", &mut before));
    let (dispatched, intact) = retirement_attempt(&before, true);
    assert_eq!(dispatched, 1, "the negative control must reach the wire");
    let bad = observed(&before, dispatched, intact);
    assert!(!model.check_invariant("PublicationBeforeDispatch", &bad));
    assert!(!model.successors("Attempt", &before).contains(&bad));
}

#[test]
fn renewal_publication_detects_dispatch_after_a_failed_save() {
    let model = aterm_models_harness::native_replay_publication_model();
    let mut before = model.init_state();
    assert!(model.fire("FailBarrier", &mut before));
    let (dispatched, intact) = renewal_attempt(&before, true);
    assert_eq!(dispatched, 1, "the negative control must reach the wire");
    let bad = observed(&before, dispatched, intact);
    assert!(!model.check_invariant("PublicationBeforeDispatch", &bad));
    assert!(!model.successors("Attempt", &before).contains(&bad));
}
