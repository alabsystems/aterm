// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A code check macOS never answers is a named refusal, never a hang.
//!
//! When `trustd` stops answering, the keeper's code checks would block forever.
//! Both run through the process's one `trustd` gate, `aterm_uds::deadline::TRUST`,
//! which refuses every call at once while an earlier one is overdue. This binary
//! — a process of its own, so nothing else shares its gate — makes the gate's
//! state what a wedged `trustd` makes it (work that never returns, past its
//! suspect age), and drives the shipped checks through it: whatever the real
//! `trustd` is doing, each must come back refused, naming the service. Then the
//! stand-in "answers", and the same checks pass again with no restart. The
//! checks are of this test binary's own ad-hoc code, which needs no `trustd`
//! once the gate lets the call through.

#![cfg(target_vendor = "apple")]

use std::io;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use aterm_keeper::client::{KeeperClient, refused_identity};
use aterm_keeper::identity::{CheckError, Identity, IdentityPolicy};
use aterm_uds::CtlStream;
use aterm_uds::deadline::{Refused, TRUST};

/// A wait for something that must happen: a hang detector, not a budget.
const HANG: Duration = Duration::from_secs(60);

#[test]
fn a_silent_trust_service_refuses_every_code_check_at_once_and_its_answer_lifts_it() {
    let id = Identity::for_self(IdentityPolicy::Designated).expect("own requirement");

    // The stand-in for a `trustd` that does not reply: work that blocks until
    // released, abandoned at its deadline and overdue from then on.
    let (release, held) = mpsc::channel::<()>();
    let stuck = TRUST.run(
        Duration::from_millis(1),
        Duration::from_millis(50),
        move || {
            let _ = held.recv();
        },
    );
    assert_eq!(stuck, Err(Refused::Unanswered));
    std::thread::sleep(Duration::from_millis(5));

    // A peer check: Unanswered (a refusal, never a pass, typed apart from a
    // peer that could not be looked up), naming the service, and at once
    // rather than after its own deadline.
    let (a, _b) = CtlStream::pair().expect("pair");
    let asked = Instant::now();
    match id.check(&a, HANG) {
        Err(CheckError::Unanswered(why)) => assert!(
            why.starts_with("macOS's code-signing service (trustd) is not answering"),
            "{why}"
        ),
        other => panic!("a silent trustd was not a refusal: {other:?}"),
    }
    assert!(
        asked.elapsed() < HANG,
        "the check waited instead of refusing"
    );

    // The window's dial: an error that is NOT read as another build's keeper.
    let dir = std::env::temp_dir().join(format!("aktrust-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let sock = dir.join("k.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
    let dial = KeeperClient::connect(&sock, &id, HANG).expect_err("a silent trustd admitted");
    assert!(!refused_identity(&dial), "read as another build: {dial}");
    assert_eq!(dial.kind(), io::ErrorKind::Other, "{dial}");
    assert!(
        dial.to_string().contains("(trustd) is not answering"),
        "{dial}"
    );

    // The keeper's check of its own copy: refused, naming the service.
    let exe = std::env::current_exe().expect("test binary");
    let copy = id
        .admits_code_at(&exe)
        .expect_err("a silent trustd admitted the copy");
    assert!(
        copy.starts_with("macOS's code-signing service (trustd) is not answering"),
        "{copy}"
    );

    // The service answers: the stuck call returns, and the same checks pass
    // again — no restart.
    release.send(()).expect("the stuck call is still waiting");
    let since = Instant::now();
    while TRUST.running() != 0 {
        assert!(since.elapsed() < HANG, "the released call never returned");
        std::thread::sleep(Duration::from_millis(10));
    }
    id.check(&a, HANG)
        .expect("this build's own code, once trustd answers");
    id.admits_code_at(&exe)
        .expect("this binary meets its own requirement");
    let _ = std::fs::remove_dir_all(&dir);
}
