// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn words(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

#[test]
fn transport_requires_canonical_parent_and_callbacks_cannot_select_other_verbs() {
    let args = words(&["42", "repo", "refs", "a/b"]);
    assert!(
        matches!(invocation(&args), Some(Invocation::Transport(42, tail)) if tail == &args[1..])
    );
    for op in ["get", "store", "erase", "capability"] {
        let args = words(&["credential", "auth", "git-credential", op]);
        assert!(
            matches!(invocation(&args), Some(Invocation::Credential(tail)) if tail == &args[1..])
        );
    }
    for args in [
        words(&[]),
        words(&["42"]),
        words(&["0", "help"]),
        words(&["1", "help"]),
        words(&["-2", "help"]),
        words(&["+2", "help"]),
        words(&["02", "help"]),
        words(&["4294967296", "help"]),
        words(&["credential", "help"]),
        words(&["credential", "auth", "git-credential", "get", "extra"]),
        words(&["credential", "api", "repos/a/b", "GET"]),
    ] {
        assert!(invocation(&args).is_none(), "{args:?}");
    }
}

#[test]
fn owned_transport_reserves_stdin_for_file_backed_native_push_only() {
    let args = words(&[
        "owned", "42", "repo", "push", "a/b", "/source", "--input", "/batch",
    ]);
    assert!(matches!(invocation(&args), Some(Invocation::Owned(42, tail)) if tail == &args[2..]));
    for args in [
        words(&[
            "owned", "42", "repo", "push", "a/b", "/source", "--input", "-",
        ]),
        words(&["owned", "42", "repo", "refs", "a/b"]),
        words(&["owned", "42", "credential", "auth", "git-credential", "get"]),
        words(&[
            "owned", "042", "repo", "push", "a/b", "/source", "--input", "/batch",
        ]),
        words(&[
            "owned", "42", "repo", "push", "a/b", "/source", "--input", "/batch", "extra",
        ]),
    ] {
        assert!(invocation(&args).is_none(), "{args:?}");
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn mutation_cancellation_requires_full_close_and_joins_on_return_or_unwind() {
    use std::sync::{atomic::Ordering, mpsc};
    use std::time::Duration;
    let hang = Duration::from_secs(60);
    let (peer, stream) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
    let (ready, started) = mpsc::sync_channel(1);
    let (proceed, resume) = mpsc::sync_channel(1);
    std::thread::scope(|scope| {
        let child = scope.spawn(move || {
            cancellation(stream, |stop| {
                ready.send(stop.clone()).unwrap();
                resume.recv_timeout(hang).unwrap();
                assert!(
                    !stop.load(Ordering::Acquire),
                    "a half-close is no cancellation"
                );
                ready.send(stop.clone()).unwrap();
                aterm_spec::wait::until_within(hang, "mutation peer close", || {
                    stop.load(Ordering::Acquire).then_some(())
                });
            })
            .unwrap();
        });
        started.recv_timeout(hang).unwrap();
        peer.shutdown(std::net::Shutdown::Write).unwrap();
        proceed.send(()).unwrap();
        let stop = started.recv_timeout(hang).unwrap();
        assert!(!stop.load(Ordering::Acquire));
        drop(peer);
        child.join().unwrap();
        assert!(stop.load(Ordering::Acquire));
    });
    let (_peer, stream) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
    assert_eq!(
        cancellation(stream, |stop| {
            assert!(!stop.load(Ordering::Acquire));
            7
        })
        .unwrap(),
        7
    );
    let (_peer, stream) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
    let (done, completion) = mpsc::sync_channel(1);
    let unwinding = std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cancellation(stream, |_| panic!("controlled mutation owner unwind"))
        }));
        done.send(result.is_err()).unwrap();
    });
    assert!(completion.recv_timeout(hang).unwrap());
    unwinding.join().unwrap();
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn mutation_cancellation_stops_when_its_bell_is_lost_and_refuses_unmonitored_work() {
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    let bell = aterm_uds::hangup::Bell::new();
    let (_peer, stream) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
    cancellation_with(&bell, stream, |stop| {
        bell.end_thread_for_test();
        aterm_spec::wait::until_within(Duration::from_secs(60), "mutation watcher failure", || {
            stop.load(Ordering::Acquire).then_some(())
        });
    })
    .unwrap();
    let (_peer, stream) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
    assert!(cancellation_with(&bell, stream, |_| panic!("unmonitored mutation started")).is_err());
}
