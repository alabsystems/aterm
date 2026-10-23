// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A code check macOS never answers is a named refusal through [`aterm_uds::deadline::TRUST`], never a hang.

#![cfg(target_vendor = "apple")]

use std::io;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use aterm_keeper::client::{KeeperClient, refused_identity};
use aterm_keeper::identity::{CheckError, Identity, IdentityPolicy};
use aterm_uds::CtlStream;
use aterm_uds::deadline::{Refused, TRUST};

const HANG: Duration = Duration::from_secs(60);

#[test]
fn a_silent_trust_service_refuses_every_code_check_at_once_and_its_answer_lifts_it() {
    let id = Identity::for_self(IdentityPolicy::Designated).expect("own requirement");

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

    let exe = std::env::current_exe().expect("test binary");
    let copy = id
        .admits_code_at(&exe)
        .expect_err("a silent trustd admitted the copy");
    assert!(
        copy.starts_with("macOS's code-signing service (trustd) is not answering"),
        "{copy}"
    );

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
