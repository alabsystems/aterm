// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// Reads once at launch, re-reads only while `NotGranted` (an `Unknown` reading, while Full
/// Disk Access is not held, reads nothing more), and hands over once per approved revision.
#[test]
fn the_watch_reads_at_launch_and_while_the_owner_is_wanted() {
    let t0 = Instant::now();
    let mut watch = DevToolsWatch::default();
    assert!(watch.wants_read(t0), "the launch read");
    assert!(!watch.wants_read(t0), "one launch read, not one per focus");
    assert_eq!(watch.observe(DevToolGrant::Unknown, false), Step::Nothing);
    assert!(
        !watch.wants_read(t0 + REREAD_EVERY * 9),
        "no Full Disk Access: the list cannot be read, so it is not read again"
    );
    assert_eq!(
        watch.observe(DevToolGrant::NotGranted, false),
        Step::NotOn { hand_over: true }
    );
    let t1 = t0 + REREAD_EVERY;
    assert!(watch.wants_read(t1), "the owner coming back reads again");
    assert!(
        !watch.wants_read(t1 + REREAD_EVERY / 2),
        "not on every focus"
    );
    assert!(watch.wants_read(t1 + REREAD_EVERY));
    assert_eq!(
        watch.observe(DevToolGrant::NotGranted, false),
        Step::NotOn { hand_over: false },
        "a second reading before the marker is written opens nothing"
    );
    assert_eq!(
        watch.observe(DevToolGrant::NotGranted, true),
        Step::NotOn { hand_over: false },
        "opened once for this revision"
    );
    assert_eq!(watch.observe(DevToolGrant::Granted, true), Step::Clear);
    assert!(
        !watch.wants_read(t1 + REREAD_EVERY * 9),
        "on the list: focus reads nothing"
    );
    watch.forget_hand_over();
    assert_eq!(
        watch.observe(DevToolGrant::NotGranted, false),
        Step::NotOn { hand_over: true }
    );
    assert_eq!(
        watch.observe(DevToolGrant::NotGranted, false),
        Step::NotOn { hand_over: false }
    );
}

/// Full Disk Access granted after an `Unknown` launch reading owes one read at once, with no
/// focus gain and no verdict refresh, until a reading is observed, and its `NotGranted` asks
/// for the page; a second grant owes none. Once it is held an `Unknown` reading is read again
/// at the cadence. Negative control: with no grant noted, the `Unknown` reading reads nothing
/// more.
#[test]
fn a_full_disk_access_grant_reads_the_list_once_at_once() {
    let t0 = Instant::now();
    let t1 = t0 + Duration::from_secs(1);
    let mut watch = DevToolsWatch::default();
    assert!(watch.wants_read(t0), "the launch read");
    assert_eq!(watch.observe(DevToolGrant::Unknown, false), Step::Nothing);
    assert!(
        !watch.due(t1) && !watch.wants_read(t0 + REREAD_EVERY * 9),
        "the control: no grant, no read"
    );

    let mut watch = DevToolsWatch::default();
    assert!(watch.wants_read(t0), "the launch read");
    assert_eq!(watch.observe(DevToolGrant::Unknown, false), Step::Nothing);
    watch.note_fda_granted();
    assert!(watch.due(t1), "owed at once");
    assert!(watch.wants_read(t1), "the grant's read");
    assert!(
        watch.wants_read(t1),
        "no reading came back (the probe could not read yet): still owed"
    );
    assert_eq!(
        watch.observe(DevToolGrant::NotGranted, false),
        Step::NotOn { hand_over: true },
        "not on the list: the page is asked for"
    );
    assert!(
        !watch.take_return_prime(false, DevToolGrant::NotGranted),
        "nothing was handed over: no verdict refresh"
    );
    watch.note_fda_granted();
    assert!(!watch.wants_read(t1), "one read: a second grant owes none");

    let mut watch = DevToolsWatch::default();
    assert!(watch.wants_read(t0));
    let _ = watch.observe(DevToolGrant::Unknown, false);
    watch.note_fda_granted();
    assert!(watch.wants_read(t1));
    assert_eq!(watch.observe(DevToolGrant::Unknown, false), Step::Nothing);
    assert!(!watch.wants_read(t1 + REREAD_EVERY / 2), "the cadence");
    assert!(
        watch.wants_read(t1 + REREAD_EVERY),
        "held, an unread list is read again"
    );
    assert_eq!(watch.observe(DevToolGrant::Granted, false), Step::Clear);
    assert!(!watch.take_refresh(), "no switch-on was seen");
    assert!(!watch.wants_read(t1 + REREAD_EVERY * 9), "on the list");
}

/// A held read is made once though the launch read was spent; `due` spends nothing. Negative
/// control: the same focus gain with nothing held reads nothing.
#[test]
fn a_held_read_is_made_once_though_the_launch_read_was_spent() {
    let t0 = Instant::now();
    let mut watch = DevToolsWatch::default();
    assert!(watch.wants_read(t0), "the launch read");
    assert!(!watch.due(t0) && !watch.wants_read(t0), "the control");
    watch.hold_read();
    assert!(watch.read_held() && watch.due(t0) && watch.due(t0));
    assert!(watch.wants_read(t0), "the held read");
    assert!(!watch.read_held());
    assert!(!watch.wants_read(t0), "once");
}

/// Returning from a Settings hand-over bypasses the cooldown for a single read.
#[test]
fn the_return_from_a_hand_over_reads_however_soon_it_comes() {
    let t0 = Instant::now();
    let soon = Duration::from_secs(20);
    assert!(soon < REREAD_EVERY);
    let mut watch = DevToolsWatch::default();
    assert!(watch.wants_read(t0));
    assert_eq!(
        watch.observe(DevToolGrant::NotGranted, false),
        Step::NotOn { hand_over: true }
    );
    assert!(!watch.wants_read(t0 + soon), "no hand-over: the gate holds");
    let mut watch = DevToolsWatch::default();
    assert!(watch.wants_read(t0));
    assert_eq!(
        watch.observe(DevToolGrant::NotGranted, false),
        Step::NotOn { hand_over: true }
    );
    watch.note_handed_over();
    assert!(watch.wants_read(t0 + soon), "back from System Settings");
    assert!(
        !watch.wants_read(t0 + soon + Duration::from_secs(1)),
        "one read: the gate is back"
    );
    assert!(watch.wants_read(t0 + soon + REREAD_EVERY));
    for grant in [DevToolGrant::Unknown, DevToolGrant::Granted] {
        let mut watch = DevToolsWatch::default();
        assert!(watch.wants_read(t0));
        let _ = watch.observe(grant, false);
        assert!(!watch.wants_read(t0 + REREAD_EVERY * 9), "{grant:?}");
        watch.note_handed_over();
        assert!(watch.wants_read(t0 + REREAD_EVERY * 9), "{grant:?}");
        assert!(!watch.wants_read(t0 + REREAD_EVERY * 20), "{grant:?}");
    }
}

/// Requests a verdict refresh once per `NotGranted`-to-`Granted` transition or manual confirm.
#[test]
fn macos_is_asked_about_aterm_again_once_per_switch_on() {
    let mut watch = DevToolsWatch::default();
    let _ = watch.observe(DevToolGrant::Granted, false);
    assert!(!watch.take_refresh(), "granted at launch: never refused");

    let mut watch = DevToolsWatch::default();
    let _ = watch.observe(DevToolGrant::NotGranted, true);
    assert!(!watch.take_refresh());
    let _ = watch.observe(DevToolGrant::Unknown, true);
    assert!(!watch.take_refresh(), "unknown moves nothing");
    let _ = watch.observe(DevToolGrant::Granted, true);
    assert!(watch.take_refresh(), "switched on");
    assert!(!watch.take_refresh(), "taken");
    let _ = watch.observe(DevToolGrant::Granted, true);
    assert!(!watch.take_refresh(), "still on: no second ask");
    let _ = watch.observe(DevToolGrant::NotGranted, true);
    let _ = watch.observe(DevToolGrant::Granted, true);
    assert!(watch.take_refresh(), "off and on again: asked again");

    let mut watch = DevToolsWatch::default();
    let _ = watch.observe(DevToolGrant::NotGranted, true);
    assert!(watch.note_switched_on(), "the owner's press");
    assert!(
        watch.note_switched_on(),
        "every press: an early one met the switch still off"
    );
    let _ = watch.observe(DevToolGrant::Granted, true);
    assert!(
        !watch.take_refresh(),
        "the reading the press brought about asks no more"
    );
}

/// Returning from a hand-over primes once when the list cannot be read directly.
#[test]
fn the_owners_return_asks_once_when_the_list_cannot_be_read() {
    let t0 = Instant::now();
    let mut watch = DevToolsWatch::default();
    assert!(watch.wants_read(t0), "the launch read");
    let _ = watch.observe(DevToolGrant::NotGranted, true);
    assert!(
        !watch.take_return_prime(false, DevToolGrant::NotGranted),
        "the launch read is no return"
    );
    watch.note_handed_over();
    assert!(watch.wants_read(t0), "the owner's return reads");
    let _ = watch.observe(DevToolGrant::NotGranted, true);
    assert!(watch.take_return_prime(false, DevToolGrant::NotGranted));
    assert!(
        !watch.take_return_prime(false, DevToolGrant::NotGranted),
        "one per return"
    );
    let _ = watch.observe(DevToolGrant::Granted, true);
    assert!(
        !watch.take_refresh(),
        "the reading it brings about asks no more"
    );
    for (list_read, grant) in [
        (true, DevToolGrant::NotGranted),
        (false, DevToolGrant::Granted),
    ] {
        watch.note_handed_over();
        assert!(watch.wants_read(t0));
        assert!(
            !watch.take_return_prime(list_read, grant),
            "{list_read} {grant:?}"
        );
    }
}
