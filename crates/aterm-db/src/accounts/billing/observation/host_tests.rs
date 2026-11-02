// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::accounts::broker::{
    Token,
    host::{self, Host, Limits},
    transport::{self, AdapterReply, Call, Status},
};
use aterm_uds::{CtlStream, secret::Secret};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};

fn client(
    dir: &Path,
    token: &Token,
    id: u8,
) -> thread::JoinHandle<std::io::Result<transport::Reply>> {
    let dir = dir.to_owned();
    let token = Secret::new(token.bytes());
    thread::spawn(move || {
        transport::call_until(
            &dir,
            Call {
                token: token.expose(),
                logical: [id; 16],
                adapter: "example-v1",
                payload: b"question",
            },
            deadline(),
        )
    })
}

fn poll_until(host: &mut Host, ready: impl Fn(&Host) -> bool) {
    let until = deadline();
    while !ready(host) {
        assert!(Instant::now() < until, "host hang detector");
        host.poll().unwrap();
        thread::yield_now();
    }
}

pub(crate) fn host_transition(
    before: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    transition_with(before, action, true)
}

fn transition_with(
    before: &aterm_spec::interp::State,
    action: &str,
    check_stop: bool,
) -> aterm_spec::interp::State {
    let (f, broker, token, grant) = super::broker_tests::setup();
    let dir = transport::tests::socket_dir();
    aterm_uds::private_file::create_private_dir(dir.path()).unwrap();
    let entered = Arc::new(AtomicUsize::new(0));
    let count = entered.clone();
    let (notice, heard) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let wait = Mutex::new(wait);
    let stop_admitted = action == "StopAdmitted";
    let panic_first = action == "PanicReap";
    let mut host = Host::bind(
        dir.path(),
        broker,
        Limits::new(2, Duration::from_secs(60)).unwrap(),
        move |request, credential, until| {
            assert_eq!(request.payload(), b"question");
            assert_eq!(credential, b"synthetic-private-credential");
            assert!(until <= deadline());
            let n = count.fetch_add(1, Ordering::SeqCst);
            if stop_admitted {
                notice.send(()).unwrap();
                wait.lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(60))
                    .unwrap();
            }
            assert!(!(panic_first && n == 0), "synthetic adapter panic");
            Some(AdapterReply::new(b"answer", None).unwrap())
        },
    )
    .unwrap();
    let mut peak = 0;
    let mut delivered = 0;
    match action {
        "Saturated" => {
            let socket = dir.path().join("broker.sock");
            let _first = aterm_uds::connect::until(&socket, deadline()).unwrap();
            let _second = aterm_uds::connect::until(&socket, deadline()).unwrap();
            let _third = aterm_uds::connect::until(&socket, deadline()).unwrap();
            peak = host.poll().unwrap().workers;
            assert_eq!(host.poll().unwrap().workers, 2);
        }
        "StopWaiting" => {
            // A complete declared frame with NO peer EOF must not turn into a
            // charge when the host's local shutdown unblocks the EOF reader.
            let _partial = transport::tests::open_request(dir.path(), &token);
            peak = host.poll().unwrap().workers;
            assert_eq!(peak, 1);
            host.drain_until(deadline()).unwrap();
        }
        "StopAdmitted" => {
            let call = client(dir.path(), &token, 91);
            poll_until(&mut host, |h| h.snapshot().workers == 1);
            peak = 1;
            heard.recv_timeout(Duration::from_secs(60)).unwrap();
            assert_eq!(host.stop().workers, 1);
            assert_eq!(
                host.drain_until(Instant::now()).unwrap_err().kind(),
                std::io::ErrorKind::TimedOut
            );
            release.send(()).unwrap();
            host.drain_until(deadline()).unwrap();
            if let Ok(reply) = call.join().unwrap() {
                assert_eq!(reply.status, Status::Unknown);
                assert!(reply.entered);
                assert_eq!(reply.body.expose(), b"answer");
            }
        }
        "StopAtGate" => {
            let (arrived, seen) = mpsc::channel();
            let (allow, paused) = mpsc::channel();
            let paused = Arc::new(Mutex::new(paused));
            let call = client(dir.path(), &token, 91);
            let until = deadline();
            while host.snapshot().workers == 0 {
                assert!(Instant::now() < until);
                let paused = paused.clone();
                let arrived = arrived.clone();
                host::poll_before_entry(&mut host, check_stop, move || {
                    arrived.send(()).unwrap();
                    paused
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(60))
                        .unwrap();
                })
                .unwrap();
                thread::yield_now();
            }
            peak = 1;
            seen.recv_timeout(Duration::from_secs(60)).unwrap();
            host.stop();
            allow.send(()).unwrap();
            host.drain_until(deadline()).unwrap();
            if let Ok(reply) = call.join().unwrap() {
                assert_eq!(reply.status, Status::Unknown);
                assert_eq!(reply.entered, !check_stop);
                assert_eq!(
                    reply.body.expose(),
                    if check_stop {
                        b"".as_slice()
                    } else {
                        b"answer"
                    }
                );
            }
        }
        "PanicReap" => {
            let first = client(dir.path(), &token, 91);
            poll_until(&mut host, |h| h.snapshot().finished == 1);
            assert!(first.join().unwrap().is_err());
            assert_eq!(host.snapshot().failed, 1);
            let second = client(dir.path(), &token, 92);
            poll_until(&mut host, |h| h.snapshot().finished == 2);
            assert!(second.join().unwrap().unwrap().entered);
            peak = 1;
            host.drain_until(deadline()).unwrap();
        }
        "TokenChild" => {
            let token = host
                .issue_until(grant.id, Duration::from_secs(120), deadline())
                .unwrap();
            let delivery = token.delivery_until(deadline()).unwrap();
            let input: std::os::fd::OwnedFd = delivery.into();
            let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "accounts::billing::observation::tests::host_tests::broker_host_token_child",
                    "--nocapture",
                ])
                .env("ATERM_BROKER_TEST_CHILD_DIR", dir.path())
                .stdin(input)
                .stdout(aterm_uds::spawnfd::Stdio::null())
                .spawn()
                .unwrap();
            let until = deadline();
            loop {
                peak = peak.max(host.poll().unwrap().workers);
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success());
                    delivered = 1;
                    break;
                }
                assert!(Instant::now() < until, "token child hang detector");
                thread::yield_now();
            }
            assert!(host.revoke(token.bytes()).unwrap());
            assert!(!host.revoke(token.bytes()).unwrap());
            host.drain_until(deadline()).unwrap();
        }
        _ => panic!("unknown host scenario"),
    }
    let snapshot = host.snapshot();
    if action != "Saturated" {
        assert!(!snapshot.accepting);
        assert!(
            host.issue_until(grant.id, Duration::from_secs(1), deadline())
                .is_err()
        );
        assert_eq!(host.poll().unwrap().workers, 0);
    }
    let debit = f
        .source(31, 0)
        .prepare_until(deadline())
        .unwrap()
        .resources()
        .snapshot()
        .unwrap()
        .used
        .values()
        .sum::<u64>();
    let scenario = [
        "Saturated",
        "StopWaiting",
        "StopAdmitted",
        "StopAtGate",
        "PanicReap",
        "TokenChild",
    ]
    .iter()
    .position(|name| *name == action)
    .unwrap()
        + 1;
    let mut next = before.clone();
    next.insert("scenario", scenario as i64);
    next.insert("peak", peak as i64);
    next.insert("workers", snapshot.workers as i64);
    next.insert("open", i64::from(snapshot.accepting));
    next.insert("entered", entered.load(Ordering::SeqCst) as i64);
    next.insert("debit", debit as i64);
    next.insert("delivered", delivered);
    host.drain_until(deadline()).unwrap();
    next
}

#[test]
fn broker_host_lifecycle_conforms_and_stop_gate_is_necessary() {
    let model = aterm_models_db::broker_host_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let initial = model.init_state();
    aterm_spec::fs_lane::each(&model.actions, |action| {
        let next = super::host_transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&next),
            "{}: {next:?}",
            action.name
        );
        next
    });
    let unsafe_next = transition_with(&initial, "StopAtGate", false);
    assert!(!model.check_invariant("StopBeforeEntry", &unsafe_next));
}

#[test]
fn broker_host_token_child() {
    let Some(dir) = std::env::var_os("ATERM_BROKER_TEST_CHILD_DIR") else {
        return;
    };
    use std::os::fd::AsFd as _;
    let input = std::io::stdin().as_fd().try_clone_to_owned().unwrap();
    let token = transport::receive_token_until(CtlStream::from(input), deadline()).unwrap();
    let reply = transport::call_until(
        Path::new(&dir),
        Call {
            token: token.expose(),
            logical: [93; 16],
            adapter: "example-v1",
            payload: b"question",
        },
        deadline(),
    )
    .unwrap();
    assert!(reply.entered);
    assert_eq!(reply.status, Status::Unknown);
    assert_eq!(reply.body.expose(), b"answer");
}

#[test]
fn broker_host_denied_gate_reports_no_provider_entry_and_retains_attempt() {
    let (f, broker, token, _) = super::broker_tests::setup();
    let (served, reply) = transport::tests::exchange_with_gate(
        &broker,
        &token,
        "Fresh",
        false,
        |_| false,
        |_, _, _| panic!("closed gate entered provider"),
    );
    served.unwrap();
    let reply = reply.unwrap();
    assert_eq!(reply.status, Status::Unknown);
    assert!(!reply.entered);
    assert!(reply.body.is_empty());
    let retry = broker
        .dispatch_until(
            "example-v1",
            token.bytes(),
            [81; 16],
            b"private request",
            deadline(),
            |_, _| panic!("closed gate retried provider"),
        )
        .unwrap();
    assert!(!retry.entered);
    assert_eq!(
        retry.accounting.attempt.observation,
        crate::resources::ObservationState::Unknown
    );
    assert_eq!(
        f.source(31, 0)
            .prepare_until(deadline())
            .unwrap()
            .resources()
            .snapshot()
            .unwrap()
            .used
            .values()
            .sum::<u64>(),
        1
    );
}

#[test]
fn broker_host_stop_during_preparation_prevents_a_charge() {
    let (f, broker, token, _) = super::broker_tests::setup();
    let dir = transport::tests::socket_dir();
    aterm_uds::private_file::create_private_dir(dir.path()).unwrap();
    let (notice, heard) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let wait = Mutex::new(wait);
    let mut host = Host::bind_prepared(
        dir.path(),
        broker,
        Limits::default(),
        move |_, _, _| {
            notice.send(()).unwrap();
            wait.lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(60))
                .unwrap();
            Ok(())
        },
        |(), _, _, _| panic!("stopped preparation entered the provider"),
    )
    .unwrap();
    let call = client(dir.path(), &token, 97);
    poll_until(&mut host, |h| h.snapshot().workers == 1);
    heard.recv_timeout(Duration::from_secs(60)).unwrap();
    host.stop();
    release.send(()).unwrap();
    host.drain_until(deadline()).unwrap();
    if let Ok(reply) = call.join().unwrap() {
        assert!(!reply.entered);
    }
    assert_eq!(
        f.source(31, 0)
            .prepare_until(deadline())
            .unwrap()
            .resources()
            .snapshot()
            .unwrap()
            .used
            .values()
            .sum::<u64>(),
        0
    );
}

#[test]
fn broker_host_finishes_a_reply_without_a_reaping_poll() {
    let (_fixture, broker, token, _) = super::broker_tests::setup();
    let dir = transport::tests::socket_dir();
    aterm_uds::private_file::create_private_dir(dir.path()).unwrap();
    let mut host = Host::bind(dir.path(), broker, Limits::default(), |_, _, _| {
        Some(AdapterReply::new(b"finished", None).unwrap())
    })
    .unwrap();
    let call = client(dir.path(), &token, 98);
    poll_until(&mut host, |h| h.snapshot().workers == 1);
    // No further poll drops the cancellation clone: EOF must come from the
    // reply writer itself, rather than a future service-loop tick.
    let reply = call.join().unwrap().unwrap();
    assert!(reply.entered);
    assert_eq!(reply.body.expose(), b"finished");
    host.drain_until(deadline()).unwrap();
}
