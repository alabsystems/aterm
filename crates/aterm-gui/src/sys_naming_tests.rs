// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_canceled_or_expired_request_never_enters_the_engine() {
    let live = Arc::new(Mutex::new(true));
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let mut request = Request::new(
        Operation::Capture {
            session: 7,
            target: NameTarget::Job("b1".into()),
        },
        Arc::clone(&live),
        tx,
    );
    *live.lock().unwrap() = false;
    assert_eq!(
        request.run_active(|| -> Result<(), NamingError> { panic!("canceled operation ran") }),
        Err(Error::Canceled)
    );
    *live.lock().unwrap() = true;
    request.deadline = Instant::now();
    assert_eq!(
        request.run_active(|| -> Result<(), NamingError> { panic!("expired operation ran") }),
        Err(Error::Canceled)
    );
    request.respond(Err(Error::Unavailable));
    assert!(matches!(rx.recv().unwrap(), Err(Error::Canceled)));
}

#[test]
fn identity_requires_exact_waiter_birth_and_an_exec_version() {
    let mut waiter = NamingWaiter {
        pid: 71,
        start: 123_456,
        exec_version: Some(8),
        exe: "worker".into(),
    };
    let identity = expected(&waiter).unwrap();
    assert_eq!(
        (identity.pid, identity.birth.0, identity.idversion),
        (71, 123_456, Some(8))
    );
    waiter.exec_version = None;
    assert_eq!(expected(&waiter), Err(Error::Engine(NamingError::Unread)));
    waiter.exec_version = Some(u64::MAX);
    assert_eq!(expected(&waiter), Err(Error::Engine(NamingError::Unread)));
}

#[test]
fn scope_refusal_precedes_any_probe_or_sampler_request() {
    let shared = Shared::new();
    assert_eq!(
        command(&shared, 1, NameTarget::Job("b1".into()), false, false),
        "ERR scope\n"
    );
}

#[test]
fn shutdown_cancels_a_lease_without_releasing_a_newer_operation() {
    let shared = Shared::new();
    assert!(matches!(shared.enter_naming(), Err(Error::Unavailable)));
    let (tx, _rx) = std::sync::mpsc::sync_channel(2);
    shared.install(tx.clone());
    let old = shared.enter_naming().unwrap();
    assert!(matches!(shared.enter_naming(), Err(Error::Busy)));
    shared.uninstall();
    assert!(old.canceled());
    assert!(matches!(shared.enter_naming(), Err(Error::Unavailable)));
    shared.install(tx);
    let current = shared.enter_naming().unwrap();
    drop(old);
    assert!(!current.canceled());
    assert!(matches!(shared.enter_naming(), Err(Error::Busy)));
    drop(current);
    assert!(shared.enter_naming().is_ok());
}

#[test]
fn a_full_probe_queue_never_coalesces_a_naming_request() {
    let shared = Shared::new();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    shared.install(tx.clone());
    tx.try_send(crate::vitals_host::Msg::Pull).unwrap();
    let lease = shared.enter_naming().unwrap();
    let result = shared.request_name(
        &lease.live,
        Operation::Capture {
            session: 7,
            target: NameTarget::Job("b1".into()),
        },
    );
    assert!(matches!(result, Err(Error::Busy)));
    assert!(matches!(
        rx.try_recv().unwrap(),
        crate::vitals_host::Msg::Pull
    ));
    assert!(matches!(
        rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
}

#[cfg(unix)]
#[test]
fn a_closed_caller_cannot_apply_after_its_worker_and_descriptor_are_gone() {
    use std::os::fd::AsRawFd as _;
    let (client, server) = aterm_uds::CtlStream::pair().unwrap();
    let live = Arc::new(Mutex::new(true));
    let (reply, _answer) = std::sync::mpsc::sync_channel(1);
    let serving = aterm_control::hangup::ServingFd::enter(server.as_raw_fd());
    let request = Request::new(
        Operation::Capture {
            session: 7,
            target: NameTarget::Job("b1".into()),
        },
        live,
        reply,
    );
    assert!(request.is_active());
    drop(serving);
    drop(server);
    assert!(
        request.is_active(),
        "the request owns its connection duplicate"
    );
    drop(client);
    // A sibling test's fork may retain the peer until exec closes its copy.
    // Wait for the required hangup, then exercise cancellation exactly once.
    aterm_spec::wait::until_within(BOUND, "the last caller descriptor closes", || {
        request.caller_gone().then_some(())
    });
    // The probe thread has no control-worker TLS and the original descriptor is closed.
    let result = std::thread::spawn(move || {
        request.run_active(|| -> Result<(), NamingError> { panic!("closed caller entered engine") })
    })
    .join()
    .unwrap();
    assert_eq!(result, Err(Error::Canceled));
}

#[test]
fn naming_json_keeps_typed_unknown_holder_without_raw_evidence() {
    let reply = success("b1", TierB::Lock, Duration::from_millis(1234), true);
    let (_, body) = reply.split_once('\n').unwrap();
    let body: aterm_json::Value = aterm_json::from_str(body.trim()).unwrap();
    assert_eq!(body["kind"].as_str(), Some("sys.name"));
    assert_eq!(body["named"].as_str(), Some("lock"));
    assert_eq!(body["job"].as_str(), Some("b1"));
    assert_eq!(body["took_ms"].as_u64(), Some(1234));
    assert_eq!(body["holder"]["why"].as_str(), Some("not_sampled"));
    assert_eq!(body.as_object().unwrap().len(), 6);
    let timer = success("b1", TierB::Timer, Duration::ZERO, true);
    let body: aterm_json::Value =
        aterm_json::from_str(timer.split_once('\n').unwrap().1.trim()).unwrap();
    assert!(body.get("holder").is_none());
}
