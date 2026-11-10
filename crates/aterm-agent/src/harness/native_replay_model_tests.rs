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

/// The seed with its saved prepare made a cold restart, which a host may answer `lapsed`; the
/// renewal's private restart stays [`seed`].
fn cold_seed() -> St {
    let mut st = seed();
    let Some(Request::Prepare(mut prepare)) = Request::parse(&st.native_prepare) else {
        panic!("the original preparation")
    };
    let NativePlan::Restart { cold, .. } = &mut prepare.plan else {
        panic!("a restart plan")
    };
    *cold = Some(Box::new(native_act::ColdIntent {
        login: None,
        home: None,
        auth_observed: true,
        route: super::super::super::accounts::AuthRoute::Stored,
        conversation: None,
        profile: None,
        mode: super::super::super::accounts::Mode::Wall,
        billing: native_act::ColdBilling::Keep,
    }));
    assert_eq!(prepare.constructor(), Constructor::ColdRestart);
    st.native_prepare = Request::Prepare(prepare).encode().unwrap();
    st
}

/// The host's answer to each dispatched replay of the exact saved prepare: none (a lost reply),
/// or, once the model's host holds it, its proof that it never admitted the id, or that no exec
/// grant was or can be issued under it.
fn answer(state: &State, expected: Request) -> impl FnMut(&str) -> Option<String> + Send + 'static {
    let unadmitted = state["unadmitted"] == 1;
    let lapsed = state["lapsed"] == 1;
    move |line| {
        assert_eq!(request(line), expected, "replay keeps the exact operation");
        let Request::Prepare(prepare) = &expected else {
            return None;
        };
        if unadmitted {
            native_act::unadmitted_reply(&prepare.id)
        } else if lapsed {
            native_act::lapsed_reply(&prepare.id)
        } else {
            None
        }
    }
}

/// The saved record a model state names: the cold seed, its wire dropped once `cleared`.
fn retained(state: &State) -> (St, Request) {
    let mut st = cold_seed();
    let expected = Request::parse(&st.native_prepare).unwrap();
    if state["cleared"] == 1 {
        st.native_prepare.clear();
        st.native_rollback = false;
    }
    st.native_retired_unissued = state["held"] == 1;
    (st, expected)
}

/// Execute one real replay with a lost or unadmitted response and optional directory failure.
/// Bypassing recovery is the negative control for the old dispatch ordering.
fn attempt(state: &State, bypass: bool) -> (usize, bool, bool) {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut opts = opts(root.path());
    opts.dry_run = state["dry"] == 1;
    let (mut st, expected) = retained(state);
    let original = st.clone();
    let (mut client, worker) = endpoint(answer(state, expected));
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
            } else if state["cleared"] == 1 {
                Err("native-plan")
            } else if fault.is_some() {
                Err("native-unsaved")
            } else if state["unadmitted"] == 1 {
                Ok(Recovery::Unadmitted)
            } else if state["lapsed"] == 1 {
                Ok(Recovery::Lapsed)
            } else {
                Err("native-unknown")
            }
        );
    }
    drop(fault);
    drop(client);
    let cleared = dropped(&original, &st);
    (worker.join().unwrap().len(), st == original, cleared)
}

/// Whether this attempt dropped the saved wire.
fn dropped(original: &St, st: &St) -> bool {
    !original.native_prepare.is_empty() && st.native_prepare.is_empty()
}

/// Negative control: the replay's reply is lost, and its wire is dropped anyway.
fn clear_on_lost_reply(state: &State) -> (usize, bool, bool) {
    let root = aterm_tempfile::tempdir().unwrap();
    let opts = opts(root.path());
    let (mut st, expected) = retained(state);
    let original = st.clone();
    let (mut client, worker) = endpoint(answer(state, expected));
    assert_eq!(
        recover(&opts, "conversation", &mut client, SID, &mut st),
        Err("native-unknown")
    );
    st.native_prepare.clear();
    drop(client);
    let cleared = dropped(&original, &st);
    (worker.join().unwrap().len(), st == original, cleared)
}

fn observed(state: &State, dispatched: usize, intact: bool, cleared: bool) -> State {
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
    if cleared {
        next.insert("cleared", 1);
        if dispatched == 0 || (state["unadmitted"] == 0 && state["lapsed"] == 0) {
            next.insert("lost_cleared", 1);
        }
    } else if !intact {
        next.insert("changed", 1);
    }
    next
}

/// Retire a record that kept only its wire: the replay recovers its binding, or the host's
/// proof that it never admitted one or that no exec grant was or can be issued, which retires it.
fn bare_retirement_attempt(state: &State) -> (usize, bool, bool) {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut opts = opts(root.path());
    opts.dry_run = state["dry"] == 1;
    let (mut st, expected) = retained(state);
    let original = st.clone();
    let (mut client, worker) = endpoint(answer(state, expected));
    let fault = (state["barrier_failed"] == 1)
        .then(|| aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported));
    let retired = retire(&opts, "conversation", &mut client, SID, &mut st);
    assert_eq!(
        retired,
        state["held"] == 0
            && (state["cleared"] == 1
                || (!opts.dry_run
                    && fault.is_none()
                    && (state["unadmitted"] == 1 || state["lapsed"] == 1)))
    );
    drop(fault);
    drop(client);
    let cleared = dropped(&original, &st);
    (worker.join().unwrap().len(), st == original, cleared)
}

fn retirement_attempt(state: &State, bypass: bool) -> (usize, bool, bool) {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut opts = opts(root.path());
    let mut st = cold_seed();
    let Request::Prepare(plan) = Request::parse(&st.native_prepare).unwrap() else {
        panic!("the original preparation")
    };
    st.native = Some(Binding {
        id: plan.id.clone(),
        capability: binding().capability,
    });
    if state["cleared"] == 1 {
        st.native = None;
        st.native_prepare.clear();
        st.native_rollback = false;
    }
    upgrade_drive::save_checked(&opts, "conversation", &st).unwrap();
    let mut st = upgrade_drive::load(&opts, "conversation").unwrap();
    st.native_retired_unissued = state["held"] == 1;
    opts.dry_run = state["dry"] == 1;
    let original = st.clone();
    // A bound retirement replays its exact kept prepare first: a lapsed answer retires it, and a
    // lost reply ends the attempt.
    let lapsed = state["lapsed"] == 1;
    let id = plan.id.clone();
    let expected = Request::Prepare(plan);
    let request_copy = expected.clone();
    let (mut client, worker) = endpoint(move |line| {
        assert_eq!(
            request(line),
            request_copy,
            "retirement keeps its operation"
        );
        lapsed.then(|| native_act::lapsed_reply(&id).unwrap())
    });
    let fault = (state["barrier_failed"] == 1)
        .then(|| aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported));
    if bypass {
        assert!(upgrade_drive::save_checked(&opts, "conversation", &st).is_err());
        assert!(native_act::exchange(&mut client, SID, &expected).is_err());
    } else {
        assert_eq!(
            retire(&opts, "conversation", &mut client, SID, &mut st),
            state["held"] == 0
                && (state["cleared"] == 1 || (!opts.dry_run && fault.is_none() && lapsed))
        );
    }
    drop(fault);
    drop(client);
    let cleared = dropped(&original, &st);
    (worker.join().unwrap().len(), st == original, cleared)
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

fn environment(state: &State, action: &str) -> State {
    let mut next = state.clone();
    let (field, value) = match action {
        "FailBarrier" => ("barrier_failed", 1),
        "RepairBarrier" => ("barrier_failed", 0),
        "DryRun" => ("dry", 1),
        "LiveRun" => ("dry", 0),
        "Hold" => ("held", 1),
        "Unadmit" => ("unadmitted", 1),
        "Lapse" => ("lapsed", 1),
        _ => unreachable!("unmapped replay environment input"),
    };
    next.insert(field, value);
    next
}

pub(super) fn transition(state: &State, action: &str) -> State {
    if action == "Attempt" {
        let (dispatched, intact, cleared) = attempt(state, false);
        observed(state, dispatched, intact, cleared)
    } else {
        environment(state, action)
    }
}

pub(super) fn retirement_transition(state: &State, action: &str) -> State {
    if action == "Attempt" {
        let (dispatched, intact, cleared) = retirement_attempt(state, false);
        observed(state, dispatched, intact, cleared)
    } else {
        environment(state, action)
    }
}

pub(super) fn bare_retirement_transition(state: &State, action: &str) -> State {
    if action == "Attempt" {
        let (dispatched, intact, cleared) = bare_retirement_attempt(state);
        observed(state, dispatched, intact, cleared)
    } else {
        environment(state, action)
    }
}

pub(super) fn renewal_transition(state: &State, action: &str) -> State {
    if action == "Attempt" {
        let (dispatched, intact) = renewal_attempt(state, false);
        observed(state, dispatched, intact, false)
    } else {
        environment(state, action)
    }
}

/// The model of a record that holds its admission binding.
fn bound_model() -> aterm_spec::derive::Model {
    aterm_spec::interp::with_consts(
        &aterm_models_harness::native_replay_publication_model(),
        &[("Bound", 1)],
    )
}

/// The model of a bound private restart: the renewal's, which the host never answers `lapsed`.
fn renewal_model() -> aterm_spec::derive::Model {
    aterm_spec::interp::with_consts(
        &aterm_models_harness::native_replay_publication_model(),
        &[("Bound", 1), ("Private", 1)],
    )
}

/// One real lane's projection of a model action.
type Lane = fn(&State, &str) -> State;

fn refines(model: &aterm_spec::derive::Model, lanes: &[(&str, Lane)]) {
    for state in aterm_spec::interp::reachable_states(model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                for (lane, transition) in lanes {
                    let next = transition(&state, action.name);
                    assert!(
                        model.successors(action.name, &state).contains(&next),
                        "{lane} {}: {state:?} -> {next:?}",
                        action.name
                    );
                }
            }
        }
    }
}

#[test]
fn replay_publication_refines_real_persistence_and_transport() {
    let model = aterm_models_harness::native_replay_publication_model();
    let bound = bound_model();
    let renewal = renewal_model();
    aterm_spec::interp::prove_and_catch(&model);
    let faults = aterm_spec::verify::sweep_faults(&[model.clone(), bound.clone(), renewal.clone()]);
    assert!(faults.is_empty(), "{}", faults.join("\n"));
    refines(
        &model,
        &[
            ("replay", transition),
            ("bare retirement", bare_retirement_transition),
        ],
    );
    refines(&bound, &[("retirement", retirement_transition)]);
    refines(&renewal, &[("renewal", renewal_transition)]);
}

#[test]
fn replay_publication_detects_dispatch_after_a_failed_save() {
    let model = aterm_models_harness::native_replay_publication_model();
    let mut before = model.init_state();
    assert!(model.fire("FailBarrier", &mut before));
    let (dispatched, intact, cleared) = attempt(&before, true);
    assert_eq!(dispatched, 1, "the negative control must reach the wire");
    let bad = observed(&before, dispatched, intact, cleared);
    assert!(!model.check_invariant("PublicationBeforeDispatch", &bad));
    assert!(!model.successors("Attempt", &before).contains(&bad));
}

#[test]
fn replay_publication_detects_a_clear_on_a_lost_reply() {
    let model = aterm_models_harness::native_replay_publication_model();
    let before = model.init_state();
    let (dispatched, intact, cleared) = clear_on_lost_reply(&before);
    assert_eq!(dispatched, 1, "the negative control must reach the wire");
    let bad = observed(&before, dispatched, intact, cleared);
    assert!(!model.check_invariant("ClearedOnlyOnPositive", &bad));
    assert!(!model.successors("Attempt", &before).contains(&bad));
}

#[test]
fn retirement_publication_detects_dispatch_after_a_failed_save() {
    let model = bound_model();
    let mut before = model.init_state();
    assert!(model.fire("FailBarrier", &mut before));
    let (dispatched, intact, cleared) = retirement_attempt(&before, true);
    assert_eq!(dispatched, 1, "the negative control must reach the wire");
    let bad = observed(&before, dispatched, intact, cleared);
    assert!(!model.check_invariant("PublicationBeforeDispatch", &bad));
    assert!(!model.successors("Attempt", &before).contains(&bad));
}

#[test]
fn renewal_publication_detects_dispatch_after_a_failed_save() {
    let model = renewal_model();
    let mut before = model.init_state();
    assert!(model.fire("FailBarrier", &mut before));
    let (dispatched, intact) = renewal_attempt(&before, true);
    assert_eq!(dispatched, 1, "the negative control must reach the wire");
    let bad = observed(&before, dispatched, intact, false);
    assert!(!model.check_invariant("PublicationBeforeDispatch", &bad));
    assert!(!model.successors("Attempt", &before).contains(&bad));
}
