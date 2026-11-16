// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

use aterm_sysprobe::notify::Posts;

#[test]
fn a_post_reaches_notifyd() {
    assert!(aterm_sysprobe::notify::post(
        c"com.aterm.aterm.acaffeine.test.unobserved"
    ));
}

/// The mug's files under a scratch state home `tmp/<name>`.
fn scratch(tmp: &aterm_tempfile::TempDir, name: &str) -> Paths {
    Paths {
        state_dir: tmp.path().join(name),
        home: tmp.path().to_path_buf(),
        logs_dir: tmp.path().join("logs"),
        local_volume: |_| true,
    }
}

/// Wait until `ear` heard a post: notifyd delivers it on its own time, so a one-minute hang
/// detector.
fn heard(ear: &mut Posts, what: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while ear.posted() != Ok(true) {
        assert!(std::time::Instant::now() < deadline, "never heard {what}");
        aterm_uds::deadline::poll_yield();
    }
}

/// A change of one state home's mug files is heard where a mug of that home registers
/// ([`observe_changed`]'s name) and by none of another home's, so a writer under a scratch home
/// wakes no mug of the login's. Each post waits until its own home's ear heard it, so notifyd
/// had handled it when the other ear is asked. The scratch homes' names are this test's alone.
/// NEGATIVE CONTROL: `changed` named `/` whatever the mug's home: "assertion `left == right`
/// failed: a change of another home's files is not heard", left `Ok(true)`, right `Ok(false)`.
#[test]
fn a_change_is_heard_by_its_own_home_alone() {
    let tmp = aterm_tempfile::Builder::new()
        .prefix("aterm-acaffeine-notify-")
        .tempdir()
        .expect("a scratch directory");
    let (mine, other) = (scratch(&tmp, "mine"), scratch(&tmp, "other"));
    let mut ear = Posts::new(&changed(&mine)).expect("registers");
    let mut other_ear = Posts::new(&changed(&other)).expect("registers");

    post_changed(&other);
    heard(&mut other_ear, "the other home's change");
    assert_eq!(
        ear.posted(),
        Ok(false),
        "a change of another home's files is not heard"
    );
    post_changed(&mine);
    heard(&mut ear, "this home's change");
    assert_eq!(
        other_ear.posted(),
        Ok(false),
        "nor is this home's change heard by the other"
    );
}

#[test]
fn the_change_is_observed_only_on_the_main_thread() {
    assert!(aterm_objc::MainThread::new().is_none());
    let tmp = aterm_tempfile::Builder::new()
        .prefix("aterm-acaffeine-notify-")
        .tempdir()
        .expect("a scratch directory");
    assert!(observe_changed(&scratch(&tmp, "state"), || {}).is_none());
}
