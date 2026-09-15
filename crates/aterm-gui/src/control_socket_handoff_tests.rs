// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The real bind/token writer, coupled to the same gate the Commit waiter opens.

use super::*;
use aterm_spec::derive::{
    Model, native_update_control_socket_handoff_model, native_update_handoff_bind_retry_model,
};
use std::collections::BTreeMap;
use std::sync::mpsc;
use std::time::Duration;

fn fixture() -> (aterm_tempfile::TempDir, control_auth::SocketPlan) {
    // Keep sun_path short even when the platform temporary root is long.
    let dir = aterm_tempfile::TempDir::new_in("/tmp").unwrap();
    let sock = dir.path().join("control.sock");
    let plan = control_auth::SocketPlan {
        sock_path: sock.to_str().unwrap().to_string(),
        token_path: control_auth::token_path_for_socket(sock.to_str().unwrap()),
        latest_link: None,
    };
    (dir, plan)
}

fn step(model: &Model, state: &mut BTreeMap<&'static str, i64>, action: &'static str) {
    let successors = model.successors(action, state);
    assert_eq!(successors.len(), 1, "{action} is not admitted at {state:?}");
    *state = successors[0].clone();
}

#[test]
fn incoming_control_preparation_requires_both_real_service_lanes() {
    let model = aterm_spec::derive::native_update_control_preparation_model();
    for rpc in [0, 1, CONTROL_WORKERS] {
        for subscriptions in [0, 1, CONTROL_SUBSCRIPTION_WORKERS] {
            let preparation = ControlPreparation::default();
            let mut state = model.init_state();
            assert_eq!(preparation.state(), ControlPreparationState::Pending);
            assert!(!model.action_enabled("Proof", &state));
            if rpc > 0 {
                step(&model, &mut state, "ReserveRpc");
            }
            if subscriptions > 0 {
                step(&model, &mut state, "ReserveSubscription");
            }
            let ready = control_lanes_prepared(rpc, subscriptions);
            assert_eq!(ready, model.action_enabled("Prepare", &state));
            preparation.finish(ready);
            step(&model, &mut state, if ready { "Prepare" } else { "Fail" });
            assert_eq!(
                preparation.state() == ControlPreparationState::Ready,
                model.action_enabled("Proof", &state),
            );
            if !ready {
                // Old readiness ignored whether either pool could allocate a
                // worker; this exact resource-failure control must block proof.
                assert_eq!(preparation.state(), ControlPreparationState::Failed);
                preparation.finish(true);
                assert_eq!(preparation.state(), ControlPreparationState::Failed);
            } else {
                step(&model, &mut state, "Proof");
                preparation.finish(false);
                assert_eq!(preparation.state(), ControlPreparationState::Ready);
            }
        }
    }
    assert!(matches!(
        crate::update_handoff_wake_class(&Wake::ControlPrepared),
        crate::UpdateHandoffEventClass::Exempt
    ));
    let buggy = aterm_spec::interp::with_consts(&model, &[("Buggy", 1)]);
    assert!(buggy.action_enabled("Prepare", &buggy.init_state()));
    assert!(!control_lanes_prepared(0, 0));
}

#[test]
fn fixed_socket_bind_follows_real_commit_gate_and_parent_exit() {
    let (_dir, plan) = fixture();
    let (parent, parent_token) =
        bind_control_listener(&plan, None, || false, |_| false, None).unwrap();
    let token_before = std::fs::read(&plan.token_path).unwrap();
    let identity = crate::control_socket_identity::SocketIdentity::capture(
        &plan,
        &parent,
        parent_token.as_str(),
    )
    .unwrap();
    let gate = crate::spawn::DeferredReaderGate::closed();
    let parent_live = Arc::new(AtomicBool::new(true));
    let observed_parent = Arc::new(AtomicBool::new(false));
    let (began_tx, began_rx) = mpsc::sync_channel(1);
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let child = {
        let plan = plan.clone();
        let gate = gate.clone();
        let live = parent_live.clone();
        let observed = observed_parent.clone();
        std::thread::spawn(move || {
            began_tx.send(()).unwrap();
            let result = bind_control_listener(
                &plan,
                Some(&gate),
                || {
                    observed.store(true, Ordering::Release);
                    live.load(Ordering::Acquire)
                },
                |pid| pid == std::process::id(),
                Some(&identity),
            );
            done_tx.send(result).unwrap();
        })
    };
    began_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let model = native_update_control_socket_handoff_model();
    let mut state = model.init_state();
    assert!(model.successors("Bind", &state).is_empty());
    assert!(done_rx.recv_timeout(Duration::from_millis(30)).is_err());
    assert!(
        !observed_parent.load(Ordering::Acquire),
        "the worker crossed the Commit gate"
    );
    assert_eq!(std::fs::read(&plan.token_path).unwrap(), token_before);

    gate.release();
    step(&model, &mut state, "Commit");
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while !observed_parent.load(Ordering::Acquire) {
        assert!(
            std::time::Instant::now() < deadline,
            "worker did not reach parent witness"
        );
        std::thread::yield_now();
    }
    assert!(model.successors("Bind", &state).is_empty());
    assert!(done_rx.recv_timeout(Duration::from_millis(30)).is_err());
    assert_eq!(std::fs::read(&plan.token_path).unwrap(), token_before);

    // Darwin can publish the death witness before closing the listener.
    parent_live.store(false, Ordering::Release);
    step(&model, &mut state, "Exit");
    assert!(done_rx.recv_timeout(Duration::from_millis(30)).is_err());
    assert_eq!(std::fs::read(&plan.token_path).unwrap(), token_before);
    drop(parent);
    let (successor, successor_token) = done_rx
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .unwrap();
    child.join().unwrap();
    step(&model, &mut state, "Bind");
    assert_eq!(state["bound"], 1);
    assert_ne!(
        parent_token, successor_token,
        "normal per-launch auth rotation survives"
    );
    assert_eq!(
        control_auth::read_token(&plan.sock_path).as_deref(),
        Some(successor_token.as_str())
    );
    let client = CtlStream::connect(&plan.sock_path).unwrap();
    let (accepted, _) = successor.accept().unwrap();
    control_auth::peer_check(&accepted).unwrap();
    drop((client, accepted, successor));
    control_auth::cleanup_socket(&plan);
}

#[test]
fn failed_candidate_and_unrelated_live_owner_keep_socket_and_token() {
    let (_dir, plan) = fixture();
    let (parent, token) = bind_control_listener(&plan, None, || false, |_| false, None).unwrap();
    let identity =
        crate::control_socket_identity::SocketIdentity::capture(&plan, &parent, token.as_str())
            .unwrap();
    let before = std::fs::read(&plan.token_path).unwrap();
    // Historical behavior: starting the successor before Commit loses its
    // control listener. Keep this negative control against the genuine binder.
    assert!(bind_control_listener(&plan, None, || false, |_| false, None).is_none());
    assert_eq!(std::fs::read(&plan.token_path).unwrap(), before);

    // Even a released gate and an exited predecessor do not grant permission
    // to displace a DIFFERENT listener on the fixed path.
    let gate = crate::spawn::DeferredReaderGate::closed();
    gate.release();
    let model = native_update_control_socket_handoff_model();
    let mut state = model.init_state();
    step(&model, &mut state, "Commit");
    step(&model, &mut state, "Exit");
    step(&model, &mut state, "ForeignBind");
    assert!(model.successors("Bind", &state).is_empty());
    assert!(
        bind_control_listener(&plan, Some(&gate), || false, |_| false, Some(&identity)).is_none()
    );
    assert_eq!(std::fs::read(&plan.token_path).unwrap(), before);
    assert!(control_auth::socket_is_live(&plan.sock_path));
    drop(parent);
    control_auth::cleanup_socket(&plan);

    // A crashed predecessor without Commit is not takeover authority either.
    let model = native_update_control_socket_handoff_model();
    let mut state = model.init_state();
    step(&model, &mut state, "Exit");
    step(&model, &mut state, "Reject");
    assert!(model.successors("Bind", &state).is_empty());
}

#[test]
fn prefilled_foreign_backlog_cannot_replace_the_parents_endpoint() {
    use std::os::fd::AsRawFd;
    let (_dir, plan) = fixture();
    let (parent, token) = bind_control_listener(&plan, None, || false, |_| false, None).unwrap();
    let identity =
        crate::control_socket_identity::SocketIdentity::capture(&plan, &parent, token.as_str())
            .unwrap();
    drop(parent);
    control_auth::cleanup_socket(&plan);
    let (foreign, foreign_token) =
        bind_control_listener(&plan, None, || false, |_| false, None).unwrap();
    assert_eq!(unsafe { libc::listen(foreign.as_raw_fd(), 1) }, 0);
    let _connections: Vec<_> = (0..128)
        .filter_map(|_| control_auth::connect_socket_nonblocking(&plan.sock_path).ok())
        .collect();
    let before = std::fs::read(&plan.token_path).unwrap();
    let gate = crate::spawn::DeferredReaderGate::closed();
    gate.release();
    // Even an errno that looks stale grants nothing over a replaced inode/token.
    assert!(
        bind_control_listener(&plan, Some(&gate), || false, |_| false, Some(&identity)).is_none()
    );
    assert_eq!(std::fs::read(&plan.token_path).unwrap(), before);
    assert_eq!(
        control_auth::read_token(&plan.sock_path).as_deref(),
        Some(foreign_token.as_str())
    );
    let model = native_update_control_socket_handoff_model();
    let mut state = model.init_state();
    for action in ["Commit", "Exit", "ForeignBind"] {
        step(&model, &mut state, action);
    }
    assert!(model.successors("Bind", &state).is_empty());
    drop(foreign);
    control_auth::cleanup_socket(&plan);
}

#[test]
fn a_failed_handoff_bind_never_retries_over_a_foreign_full_backlog() {
    use std::os::fd::AsRawFd;
    let (_dir, plan) = fixture();
    let (foreign, token) = bind_control_listener(&plan, None, || false, |_| false, None).unwrap();
    let identity =
        crate::control_socket_identity::SocketIdentity::capture(&plan, &foreign, token.as_str())
            .unwrap();
    // SAFETY: this fixture owns the listener whose accept queue it fills.
    assert_eq!(unsafe { libc::listen(foreign.as_raw_fd(), 1) }, 0);
    let connections: Vec<_> = (0..128)
        .filter_map(|_| control_auth::connect_socket_nonblocking(&plan.sock_path).ok())
        .collect();
    assert!(!connections.is_empty());
    let before = std::fs::read(&plan.token_path).unwrap();
    let (send, received) = mpsc::sync_channel(1);
    let worker = {
        let plan = plan.clone();
        std::thread::spawn(move || {
            // The foreign bind raced after the caller's final ownership check
            // and initial unlink. Drive the real remaining bind/retry path.
            let result = bind_prepared_control_listener(&plan, true);
            let _ = send.send(result);
        })
    };
    let result = received.recv_timeout(Duration::from_secs(2));
    let untouched = identity.matches_current(&plan)
        && std::fs::read(&plan.token_path).is_ok_and(|after| after == before);
    // Release even a regressed blocking probe before asserting, so a failing
    // test cannot leave an indefinitely parked worker in the suite.
    drop((connections, foreign));
    worker.join().unwrap();
    assert!(
        result.is_ok(),
        "handoff bind entered a blocking retry probe"
    );
    assert!(
        result.unwrap().is_none(),
        "handoff stole the foreign endpoint"
    );
    assert!(
        untouched,
        "failed handoff changed the foreign socket or token"
    );
    let model = native_update_control_socket_handoff_model();
    let mut state = model.init_state();
    for action in ["Commit", "Exit", "ForeignBind"] {
        step(&model, &mut state, action);
    }
    assert!(model.successors("Bind", &state).is_empty());
    control_auth::cleanup_socket(&plan);
}

#[test]
fn a_transient_handoff_bind_failure_recovers_without_rewriting_credentials() {
    let (_dir, plan) = fixture();
    let token = control_auth::provision_token(&plan.token_path).unwrap();
    let before = std::fs::read(&plan.token_path).unwrap();
    let mut attempts = 0;
    let mut pauses = Vec::new();
    let listener = bind_handoff_control_listener(
        &plan,
        |path| {
            attempts += 1;
            if attempts == 1 {
                // Historical single-attempt policy stopped here permanently.
                Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
            } else {
                CtlListener::bind(path)
            }
        },
        |duration| pauses.push(duration),
    )
    .expect("a vacant path must recover from the injected transient failure");
    assert_eq!(
        attempts, 2,
        "the failure negative control must be exercised"
    );
    assert_eq!(pauses, [Duration::from_millis(50)]);
    assert_eq!(std::fs::read(&plan.token_path).unwrap(), before);
    let client = CtlStream::connect(&plan.sock_path).unwrap();
    let (accepted, _) = listener.accept().unwrap();
    control_auth::peer_check(&accepted).unwrap();
    assert_eq!(control_auth::read_token(&plan.sock_path).unwrap(), token);

    let model = native_update_handoff_bind_retry_model();
    let mut state = model.init_state();
    for action in ["Fail", "Retry", "Bind"] {
        step(&model, &mut state, action);
    }
    assert_eq!(state["attempts"], attempts);
    assert_eq!(state["bound"], 1);
    drop((client, accepted, listener));
    control_auth::cleanup_socket(&plan);
}

#[test]
fn handoff_bind_retry_exhausts_a_finite_budget_and_matches_the_model_guard() {
    let (_dir, plan) = fixture();
    let model = native_update_handoff_bind_retry_model();
    let mut state = model.init_state();
    let mut attempts = 0;
    let mut pauses = Vec::new();
    let result = bind_handoff_control_listener(
        &plan,
        |_| {
            attempts += 1;
            step(&model, &mut state, "Fail");
            assert_eq!(
                handoff_bind_retry_allowed(attempts, true),
                !model.successors("Retry", &state).is_empty(),
                "shipping retry guard disagrees at failure {attempts}"
            );
            let mut occupied = state.clone();
            step(&model, &mut occupied, "Occupy");
            assert_eq!(
                handoff_bind_retry_allowed(attempts, false),
                !model.successors("Retry", &occupied).is_empty(),
                "a foreign path must reject even while budget remains"
            );
            if handoff_bind_retry_allowed(attempts, true) {
                step(&model, &mut state, "Retry");
            } else {
                step(&model, &mut state, "Stop");
            }
            Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
        },
        |duration| pauses.push(duration),
    );
    assert!(result.is_none());
    assert_eq!(attempts, HANDOFF_BIND_ATTEMPTS);
    assert_eq!(state["attempts"], i64::from(attempts));
    assert_eq!(state["stopped"], 1);
    assert_eq!(
        pauses,
        [Duration::from_millis(50), Duration::from_millis(100)]
    );
    assert!(!std::fs::exists(&plan.sock_path).unwrap());
}

#[test]
fn an_endpoint_arriving_during_handoff_backoff_is_preserved() {
    use std::os::fd::AsRawFd;
    let (_dir, plan) = fixture();
    let mut attempts = 0;
    let mut foreign = None;
    let mut connections = Vec::new();
    let mut identity = None;
    let result = bind_handoff_control_listener(
        &plan,
        |path| {
            attempts += 1;
            if attempts == 1 {
                Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
            } else {
                CtlListener::bind(path)
            }
        },
        |_| {
            assert!(
                foreign.is_none(),
                "an occupied path cannot schedule more backoff"
            );
            let (listener, token) =
                bind_control_listener(&plan, None, || false, |_| false, None).unwrap();
            identity = Some(
                crate::control_socket_identity::SocketIdentity::capture(
                    &plan,
                    &listener,
                    token.as_str(),
                )
                .unwrap(),
            );
            // A real full backlog must not be misread as permission to unlink.
            assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 1) }, 0);
            connections = (0..16)
                .filter_map(|_| control_auth::connect_socket_nonblocking(&plan.sock_path).ok())
                .collect();
            assert!(!connections.is_empty());
            foreign = Some(listener);
        },
    );
    assert!(result.is_none());
    assert_eq!(attempts, 2);
    assert!(identity.unwrap().matches_current(&plan));
    drop((connections, foreign));
    control_auth::cleanup_socket(&plan);
}
