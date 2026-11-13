// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::agent::{Layout, run};
use crate::port::{Fake, Ran};
use std::path::Path;

/// `ioreg -n Root -d1 -a` in the macOS 27.0 VM with the admin (uid 501) logged in and locked.
const IOREG_LOCKED: &[u8] = include_bytes!("../../fixtures/ioreg/vm-27.0-root-locked.plist");

/// The same, another boot, the session logged in and unlocked.
const IOREG_UNLOCKED: &[u8] = include_bytes!("../../fixtures/ioreg/vm-27.0-root-unlocked.plist");

fn admin_fake() -> Fake {
    let mut fake = Fake::root();
    fake.euid = 501;
    fake
}

fn ioreg_key() -> String {
    ioreg_root_cmd().display()
}

#[test]
fn the_lock_reads_back_from_the_sessions_own_console_entry() {
    let on = |locked| {
        Ok(Some(ConsoleSession {
            on_console: true,
            locked,
        }))
    };
    assert_eq!(console_session(IOREG_LOCKED, 501), on(true));
    assert_eq!(console_session(IOREG_UNLOCKED, 501), on(false));
    assert_eq!(console_session(IOREG_LOCKED, 502), Ok(None));
    assert!(console_session(b"not a plist", 501).is_err());
}

#[test]
fn lock_screen_is_an_agent_verb_that_locks_through_the_port_and_reads_it_back() {
    let fake = admin_fake();
    fake.on(
        &ioreg_key(),
        Ran::ok(&String::from_utf8_lossy(IOREG_LOCKED)),
    );
    let mut out = Vec::new();
    let code = run(
        &fake,
        &Layout::installed(),
        Path::new("/x"),
        &["lock-screen"],
        &mut std::io::empty(),
        &mut out,
    );
    assert_eq!(String::from_utf8_lossy(&out), "locked\n");
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(fake.calls(), ["lock-screen".to_owned(), ioreg_key()]);
}

#[test]
fn a_refused_lock_is_not_locked_and_reads_nothing() {
    let mut fake = admin_fake();
    fake.lock_refused = Some("SACLockScreenImmediate answered 22".into());
    let mut out = Vec::new();
    let code = run(
        &fake,
        &Layout::installed(),
        Path::new("/x"),
        &["lock-screen"],
        &mut std::io::empty(),
        &mut out,
    );
    assert_eq!(
        String::from_utf8_lossy(&out),
        "not locked: SACLockScreenImmediate answered 22\n"
    );
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(fake.calls(), ["lock-screen"]);
}

#[test]
fn a_scheduled_console_read_can_follow_an_unlocked_reading() {
    let fake = admin_fake();
    fake.on(
        &ioreg_key(),
        Ran::ok(&String::from_utf8_lossy(IOREG_UNLOCKED)),
    );
    fake.on(
        &ioreg_key(),
        Ran::ok(&String::from_utf8_lossy(IOREG_LOCKED)),
    );
    let mut out = Vec::new();
    let code = lock_screen_with_start(&fake, &mut out, LOCK_HANG, || {
        Instant::now() - Duration::from_secs(11)
    });
    assert_eq!(code, ExitCode::SUCCESS, "{}", String::from_utf8_lossy(&out));
    assert_eq!(out, b"locked\n");
    assert_eq!(fake.calls().len(), 3);

    let fake = admin_fake();
    fake.on(
        &ioreg_key(),
        Ran::ok(&String::from_utf8_lossy(IOREG_UNLOCKED)),
    );
    let mut out = Vec::new();
    assert_eq!(
        lock_screen_with_start(&fake, &mut out, LOCK_HANG, || {
            Instant::now() - Duration::from_secs(61)
        }),
        ExitCode::FAILURE,
    );
    assert!(String::from_utf8_lossy(&out).starts_with("not locked:"));
    assert_eq!(fake.calls().len(), 2);
}

#[test]
fn a_lock_taken_but_never_read_back_is_not_locked() {
    let fake = admin_fake();
    fake.on(
        &ioreg_key(),
        Ran::ok(&String::from_utf8_lossy(IOREG_UNLOCKED)),
    );
    let mut out = Vec::new();
    let code = lock_screen(&fake, &mut out, Duration::ZERO);
    let out = String::from_utf8_lossy(&out);
    assert!(
        out.starts_with("not locked: loginwindow took the request"),
        "{out}"
    );
    assert_eq!(code, ExitCode::FAILURE);
    let fake = admin_fake();
    fake.on(
        &ioreg_key(),
        Ran::ok(&String::from_utf8_lossy(IOREG_UNLOCKED)),
    );
    fake.on(
        &ioreg_key(),
        Ran::ok(&String::from_utf8_lossy(IOREG_LOCKED)),
    );
    let mut out = Vec::new();
    assert_eq!(
        lock_screen(&fake, &mut out, Duration::from_secs(60)),
        ExitCode::SUCCESS
    );
    assert_eq!(fake.calls().len(), 3, "{:?}", fake.calls());
}

/// A diagnostic tail is not a complete console-user record, even if its
/// remaining bytes form a plist which says locked.
#[test]
fn a_lock_report_requires_a_complete_console_reading() {
    let fake = admin_fake();
    fake.on(
        &ioreg_key(),
        Ran::ok(&String::from_utf8_lossy(IOREG_LOCKED)),
    );
    let mut out = Vec::new();
    assert_eq!(
        lock_screen(&fake, &mut out, Duration::ZERO),
        ExitCode::SUCCESS
    );
    assert_eq!(out, b"locked\n");
    assert_eq!(fake.calls(), ["lock-screen".to_owned(), ioreg_key()]);

    let fake = admin_fake();
    fake.on(
        &ioreg_key(),
        Ran::ok(&String::from_utf8_lossy(IOREG_LOCKED)),
    );
    fake.refuse_complete_stdout(&ioreg_key(), "stdout did not reach EOF");
    let mut out = Vec::new();
    let code = lock_screen(&fake, &mut out, Duration::ZERO);
    let out = String::from_utf8_lossy(&out);
    assert_eq!(fake.calls(), ["lock-screen".to_owned(), ioreg_key()]);
    assert!(
        code == ExitCode::FAILURE && out.contains("stdout did not reach EOF"),
        "MACHINES_LOCK_ACCEPTED_INCOMPLETE_CONSOLE_RECORD: {code:?} {out}"
    );
}
