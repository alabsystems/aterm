// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The real bind/token writer, coupled to the same gate the Commit waiter opens.

use super::*;
use aterm_spec::derive::{
    Model, native_update_control_socket_handoff_model, native_update_handoff_bind_retry_model,
};
use aterm_spec::wait::{MUST_HAPPEN, until_within};
use std::collections::BTreeMap;
use std::sync::atomic::AtomicUsize;
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

/// The instance that bound `plan` leaves: its socket and token go. By path,
/// because the fixture's files are that instance's own; the identity rule an
/// exit really removes by is `owned_endpoint`'s, tested there.
fn leave(plan: &control_auth::SocketPlan) {
    let _ = std::fs::remove_file(&plan.sock_path);
    let _ = std::fs::remove_file(&plan.token_path);
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

/// The successor binds the fixed path only after the Commit, its predecessor's
/// exit AND the close of the predecessor's listener — each judged at a FACT:
/// the worker parked on the closed gate, then asking again about a parent it
/// was told is alive, then probing again a listener it found live. A window
/// ("nothing done within 30 ms") passes vacuously whenever the worker is slow
/// to get there, which a loaded machine makes the common case.
///
/// ALONE IN ITS OWN PROCESS. `drop(parent)` closes the predecessor's listener
/// only when it closes its LAST descriptor, and a child that any sibling test
/// forks holds a copy until it execs (macOS 26.6 honors no `FD_CLOFORK`): the
/// copy kept the listener listening, the worker kept finding its predecessor,
/// and past its 2 s handoff deadline it stood down without binding.
///
/// The handoff wait is a HANG DETECTOR here ([`bind_control_listener_within`]):
/// production's 2 s runs from the release, and every step of this test between
/// the release and `drop(parent)` had to fit inside it — a test thread stalled
/// past it made a correct worker stand down, read as "ended over a live
/// parent". Production's own bound is not this test's subject.
///
/// The listener closes at the WORST instant, every run: the worker's third
/// dial is held between its connect and the ownership question until the
/// listener is gone. The test accepts only the two probes, so that dial is
/// still queued, and closing the listener aborts it: XNU answers its
/// `LOCAL_PEERPID` with the owner before the close and `ENOTCONN` after, as
/// it does for any dial the predecessor's teardown reaches (its exit closes
/// the connections it accepted, too). That is the teardown the worker waits
/// for, which it once read as a foreign owner and stood down (7 runs in 6000
/// at 128 loaded copies, 2026-10-16). The probe after it is refused and the
/// worker binds, so the plain close is walked too.
#[test]
fn fixed_socket_bind_follows_real_commit_gate_and_parent_exit() {
    if !crate::test_alone::alone() {
        return;
    }
    let (_dir, plan) = fixture();
    let (parent, parent_token) = bind_control_listener(&plan, None, || false, |_| false, None)
        .unwrap()
        .into_parts();
    let token_before = std::fs::read(&plan.token_path).unwrap();
    let identity = crate::control_socket_identity::SocketIdentity::capture(
        &plan,
        &parent,
        parent_token.as_str(),
    )
    .unwrap();
    let gate = crate::spawn::DeferredReaderGate::closed();
    let parent_live = Arc::new(AtomicBool::new(true));
    let parent_asked = Arc::new(AtomicUsize::new(0));
    // Dials that reached a listener; the third is held until `closed`, and
    // asked who owns it before the close and after.
    const HELD_DIAL: usize = 3;
    let parent_dialed = Arc::new(AtomicUsize::new(0));
    let (closed_tx, closed_rx) = mpsc::sync_channel::<()>(1);
    let (held_tx, held_rx) = mpsc::sync_channel(1);
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let child = {
        let plan = plan.clone();
        let gate = gate.clone();
        let live = parent_live.clone();
        let asked = parent_asked.clone();
        let dialed = parent_dialed.clone();
        std::thread::spawn(move || {
            let result = bind_control_listener_within(
                &plan,
                Some(&gate),
                || {
                    asked.fetch_add(1, Ordering::AcqRel);
                    live.load(Ordering::Acquire)
                },
                |pid| pid == std::process::id(),
                Some(&identity),
                MUST_HAPPEN,
                |path: &str| {
                    let stream = control_auth::connect_socket_nonblocking(path);
                    let Ok(held) = &stream else { return stream };
                    let peer = || aterm_uds::fdpass::peer_pid(held).map_err(|e| e.kind());
                    // Only this thread counts. The held dial is asked before
                    // the count says it reached the listener, so the listener
                    // is surely live when it answers.
                    let n = dialed.load(Ordering::Acquire) + 1;
                    let before = (n == HELD_DIAL).then(peer);
                    dialed.store(n, Ordering::Release);
                    if let Some(before) = before {
                        // A hang detector: the test closes the listener now.
                        let _ = closed_rx.recv_timeout(MUST_HAPPEN);
                        let _ = held_tx.send((before, peer()));
                    }
                    stream
                },
            );
            done_tx.send(result).unwrap();
        })
    };
    let asked = || parent_asked.load(Ordering::Acquire);
    let dialed = || parent_dialed.load(Ordering::Acquire);
    let model = native_update_control_socket_handoff_model();
    let mut state = model.init_state();

    // Before the Commit: the worker parks on the closed gate. Parked there it
    // can leave only by the release, so what it has not done yet — asked
    // about its parent, finished, touched the token — is judged now.
    until_within(MUST_HAPPEN, "the worker parks on the closed gate", || {
        (gate.parked() == 1 || asked() > 0 || child.is_finished()).then_some(())
    });
    assert_eq!(asked(), 0, "the worker crossed the Commit gate");
    assert!(!child.is_finished(), "the worker ended before the Commit");
    assert!(model.successors("Bind", &state).is_empty());
    assert_eq!(std::fs::read(&plan.token_path).unwrap(), token_before);

    // Committed, its parent alive: asked twice is told twice, so it is
    // holding at the parent witness rather than moving on.
    gate.release();
    step(&model, &mut state, "Commit");
    until_within(
        MUST_HAPPEN,
        "the worker asks again about a live parent",
        || (asked() >= 2 || child.is_finished()).then_some(()),
    );
    assert!(!child.is_finished(), "the worker ended over a live parent");
    assert!(model.successors("Bind", &state).is_empty());
    assert_eq!(std::fs::read(&plan.token_path).unwrap(), token_before);

    // Darwin can publish the death witness before closing the listener. The
    // worker then probes the listener it attested, finds it live, and probes
    // again: its probes are accepted here, two of them, and its third dial
    // reaches the listener too, as the fact. That one is left in the queue.
    parent_live.store(false, Ordering::Release);
    step(&model, &mut state, "Exit");
    parent.set_nonblocking(true).unwrap();
    let mut probes = Vec::new();
    until_within(
        MUST_HAPPEN,
        "the worker probes the live listener twice and dials it again",
        || {
            if probes.len() < HELD_DIAL - 1 {
                match parent.accept() {
                    Ok((probe, _)) => probes.push(probe),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(error) => panic!("accept the worker's probe: {error}"),
                }
            }
            ((probes.len() == HELD_DIAL - 1 && dialed() >= HELD_DIAL) || child.is_finished())
                .then_some(())
        },
    );
    assert!(
        !child.is_finished(),
        "the worker ended while its predecessor's listener was live"
    );
    assert_eq!(std::fs::read(&plan.token_path).unwrap(), token_before);
    // The listener closes inside the held dial, before its ownership question.
    drop((probes, parent));
    closed_tx.send(()).unwrap();
    let (before, after) = held_rx.recv_timeout(MUST_HAPPEN).unwrap();
    assert_eq!(
        before,
        Ok(std::process::id()),
        "the held dial reached the listener"
    );
    if cfg!(target_vendor = "apple") {
        assert_eq!(
            after,
            Err(std::io::ErrorKind::NotConnected),
            "closing the listener aborts the dial still queued on it"
        );
    }
    let (successor, successor_token) = done_rx
        .recv_timeout(MUST_HAPPEN)
        .unwrap()
        .expect("the successor binds once its predecessor's listener closed")
        .into_parts();
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
    leave(&plan);
}

#[test]
fn failed_candidate_and_unrelated_live_owner_keep_socket_and_token() {
    let (_dir, plan) = fixture();
    let (parent, token) = bind_control_listener(&plan, None, || false, |_| false, None)
        .unwrap()
        .into_parts();
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
    leave(&plan);

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
    let (parent, token) = bind_control_listener(&plan, None, || false, |_| false, None)
        .unwrap()
        .into_parts();
    let identity =
        crate::control_socket_identity::SocketIdentity::capture(&plan, &parent, token.as_str())
            .unwrap();
    drop(parent);
    leave(&plan);
    let (foreign, foreign_token) = bind_control_listener(&plan, None, || false, |_| false, None)
        .unwrap()
        .into_parts();
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
    leave(&plan);
}

#[test]
fn a_failed_handoff_bind_never_retries_over_a_foreign_full_backlog() {
    use std::os::fd::AsRawFd;
    let (_dir, plan) = fixture();
    let (foreign, token) = bind_control_listener(&plan, None, || false, |_| false, None)
        .unwrap()
        .into_parts();
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
    // The bind's answer MUST arrive: a hang detector, never a 2 s budget a
    // loaded machine overruns. A regressed probe that blocks on the full
    // backlog is still named — at the minute, as a timeout below.
    let result = received.recv_timeout(MUST_HAPPEN);
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
    leave(&plan);
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
    leave(&plan);
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
            let (listener, token) = bind_control_listener(&plan, None, || false, |_| false, None)
                .unwrap()
                .into_parts();
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
    leave(&plan);
}
