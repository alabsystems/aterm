// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::{Fake, Ran};

/// `ioreg -n Root -d1 -a` in the macOS 27.0 VM with the admin (uid 501) logged in and locked.
const LOCKED: &[u8] = include_bytes!("../../fixtures/ioreg/vm-27.0-root-locked.plist");

/// The same, another boot, the session logged in and unlocked.
const UNLOCKED: &[u8] = include_bytes!("../../fixtures/ioreg/vm-27.0-root-unlocked.plist");

const SECOND: u64 = 1_000_000_000;

#[test]
fn only_an_unlocked_console_session_with_the_keys_at_rest_is_open() {
    assert_eq!(
        reach_of(UNLOCKED, Some(60 * SECOND), 501),
        SessionReach::Open
    );
    assert_eq!(
        reach_of(UNLOCKED, Some(2 * SECOND), 501),
        SessionReach::KeysInUse
    );
    assert_eq!(reach_of(UNLOCKED, None, 501), SessionReach::Unread);
    // Locked, however long the keys rested.
    assert_eq!(
        reach_of(LOCKED, Some(3_600 * SECOND), 501),
        SessionReach::Locked
    );
    // No session of this account on the console.
    assert_eq!(
        reach_of(UNLOCKED, Some(60 * SECOND), 502),
        SessionReach::SwitchedAway
    );
    assert_eq!(
        reach_of(b"not a plist", Some(60 * SECOND), 501),
        SessionReach::Unread
    );
}

#[test]
fn the_live_reading_runs_two_ioreg_reads_through_the_port() {
    let sys = Fake::root();
    sys.on(
        &ioreg_root_cmd().display(),
        Ran::ok(&String::from_utf8_lossy(UNLOCKED)),
    );
    sys.on(
        &crate::facts::idle_cmd().display(),
        Ran::ok("    \"HIDIdleTime\" = 61000000000\n"),
    );
    assert_eq!(read(&sys, 501), SessionReach::Open);
    let unread = Fake::root();
    unread.on(&ioreg_root_cmd().display(), Ran::exit(1, "ioreg failed"));
    assert_eq!(read(&unread, 501), SessionReach::Unread);
}
