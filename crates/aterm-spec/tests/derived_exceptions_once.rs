// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of `ExceptionsOnce`: the owner's exceptions digest raises a cause once, and the raise
//! is queued before any surface shows it. Each defect has its own switch (`Buggy = 1` sets all):
//! * `Reopen`: a dismissed cause is raised again while it still stands;
//! * `Early`: the chip shows an entry before its raise line is queued, so an edge-triggered
//!   loss dies with the process that saw it;
//! * `Volatile`: the dismissal lives in memory only, so a restart reopens it;
//! * `StaleSuccessor`: an update's successor never folds the ledger's tail in at Commit;
//! * `NoFlush`: the parent execs the successor without flushing its queue.
//!
//! Tier-1 is the real `MessageCenter`: `aterm-messages/src/family_tests.rs` (the update overlap)
//! and the restart tests of `need_tests.rs`.

use aterm_spec::derive::exceptions_once_model;
use aterm_spec::{interp, verify};

#[test]
fn exceptions_once_proves_and_catches() {
    let model = exceptions_once_model();
    let fired = interp::fired_actions(&model);
    assert_eq!(
        fired.len(),
        model.actions.len(),
        "every action fires at the committed configuration: {fired:?}"
    );
    verify::prove_and_catch_scalar(
        &model,
        "Exceptions inbox: a cause is raised once, and never before its line is queued",
    );
}

#[test]
fn exceptions_once_every_defect_is_caught_on_its_own() {
    for (switch, invariant) in [
        ("Reopen", "DismissedStaysQuiet"),
        ("Early", "ShownImpliesQueued"),
        ("Volatile", "DismissedStaysQuiet"),
        ("StaleSuccessor", "DismissedStaysQuiet"),
        ("NoFlush", "DismissedStaysQuiet"),
    ] {
        let m = interp::with_consts(&exceptions_once_model(), &[(switch, 1)]);
        let Err((state, broken)) = interp::bmc(&m) else {
            panic!("{switch} must break an invariant");
        };
        assert_eq!(broken, invariant, "{switch}: {state:?}");
    }
    // …and all of them together (Buggy = 1) break both.
    let all = interp::with_buggy(&exceptions_once_model(), 1);
    assert!(interp::bmc(&all).is_err());
}

/// Raise, dismiss, the file flushed at the restart, the same cause asserted again: quiet.
#[test]
fn exceptions_once_walks_the_full_disk_access_history() {
    let m = exceptions_once_model();
    let mut st = m.init_state();
    for action in ["Assert", "Log", "Show", "Dismiss", "Restart"] {
        assert!(m.fire(action, &mut st), "{action}: {st:?}");
    }
    assert_eq!((st["st"], st["disk"], st["shown"]), (2, 2, 0));
    assert!(
        !m.action_enabled("Assert", &st),
        "the same cause after a restart is not raised again"
    );
    assert!(m.fire("Drift", &mut st));
    assert!(m.action_enabled("Assert", &st), "a new cause is news");

    // `Volatile`: the dismissal never reaches the file.
    let bug = interp::with_consts(&m, &[("Volatile", 1)]);
    let mut st = bug.init_state();
    for action in ["Assert", "Log", "Show", "Dismiss", "Restart"] {
        assert!(bug.fire(action, &mut st), "{action}: {st:?}");
    }
    assert_eq!(st["st"], 1, "the dismissed question is open again");
    assert!(bug.fire("Show", &mut st));
    assert!(!bug.check_invariant("DismissedStaysQuiet", &st));
}

/// A dismissal made during the update: the successor loads while the cause is open, the owner
/// dismisses in the parent, the file catches up, Commit, and the same cause asserted again is
/// quiet because the successor folded the file's tail in.
#[test]
fn exceptions_once_walks_a_dismissal_made_during_the_update() {
    let m = exceptions_once_model();
    let mut st = m.init_state();
    for action in [
        "Assert", "Log", "Handoff", "Show", "Dismiss", "LogFiled", "Commit",
    ] {
        assert!(m.fire(action, &mut st), "{action}: {st:?}");
    }
    assert_eq!(
        (st["st"], st["disk"], st["succ"], st["shown"]),
        (2, 2, 0, 0)
    );
    assert_eq!(st["reshown"], 0);
    assert!(
        !m.action_enabled("Assert", &st),
        "the cause the owner just dismissed is not raised again by the successor"
    );

    // A successor that trusts its snapshot shows the question again.
    for (switch, why) in [
        ("StaleSuccessor", "the successor never folded the tail in"),
        ("NoFlush", "the parent never flushed its queue"),
    ] {
        let bug = interp::with_consts(&m, &[(switch, 1)]);
        let mut st = bug.init_state();
        for action in [
            "Assert", "Log", "Handoff", "Show", "Dismiss", "LogFiled", "Commit",
        ] {
            if switch == "NoFlush" && action == "LogFiled" {
                continue; // the filing was still queued when the parent exec'd
            }
            assert!(bug.fire(action, &mut st), "{switch} {action}: {st:?}");
        }
        assert_eq!(st["st"], 1, "{why}: the dismissed question is open again");
        assert!(bug.fire("Show", &mut st), "{switch}");
        assert!(!bug.check_invariant("DismissedStaysQuiet", &st), "{switch}");
    }
}
