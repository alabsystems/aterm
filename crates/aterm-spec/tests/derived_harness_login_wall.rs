// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tier-0 of `HarnessLoginWall` (`aterm_spec::derive::harness_login_wall_model`):
//! proved at `Buggy = 0` and caught at `Buggy = 1` on EACH invariant, with no
//! dead action at the committed configuration; ONE KNOB PER DEFECT, each
//! caught on its own by the invariant it breaks; the incident of 2026-09-27
//! walked as it ran (the caught negative control) and as it goes now. Tier-1 is
//! aterm-agent's `conformance_login_wall`, over the real readers, reducer and
//! turn-end decider on every reachable state.

use crate::walk::{only, reachable, walk};
use aterm_spec::derive::harness_login_wall_model;
use aterm_spec::{interp, verify};

/// The incident's invariants: the four its walk breaks.
const INCIDENT: [&str; 4] = [
    "NoNoticeAtTheWall",
    "NoGiveUpUnread",
    "NoFutileContinue",
    "TheOwnerIsToldFirst",
];

/// Every invariant: the incident's, and the renewable login's (S7).
const INVARIANTS: [&str; 7] = [
    "NoNoticeAtTheWall",
    "NoGiveUpUnread",
    "NoFutileContinue",
    "TheOwnerIsToldFirst",
    "NoLoginTypedOnARoster",
    "NoSessionLeftOnADeadLogin",
    "NoCheckUnderAnIdleLogin",
];

/// The defects the fix repairs, one knob each, and the invariant that
/// catches each alone.
const KNOBS: [(&str, &str); 9] = [
    ("NoGate", "NoNoticeAtTheWall"),
    ("NoRefund", "NoGiveUpUnread"),
    ("ClockAtWall", "NoGiveUpUnread"),
    ("NoSee", "NoFutileContinue"),
    ("NoSee", "TheOwnerIsToldFirst"),
    ("NoHold", "NoFutileContinue"),
    ("TypeOnRoster", "NoLoginTypedOnARoster"),
    ("NoFollow", "NoSessionLeftOnADeadLogin"),
    ("CheckIdle", "NoCheckUnderAnIdleLogin"),
];

#[test]
fn the_login_wall_model_proves_and_catches() {
    let model = harness_login_wall_model();
    let fired = interp::fired_actions(&model);
    assert_eq!(
        fired.len(),
        model.actions.len(),
        "every action fires at the committed configuration: {fired:?}"
    );
    verify::prove_and_catch_scalar(
        &model,
        "harness login wall: nothing typed into the wall, no give-up spent on a notice it \
         answered, no continuation into a login seen gone, the owner told first",
    );
    // Each invariant is load-bearing on its own: the incident's code breaks
    // every one.
    for invariant in INVARIANTS {
        let alone = only(&model, invariant);
        assert!(
            interp::bmc(&interp::with_buggy(&alone, 0)).is_ok(),
            "{invariant}"
        );
        assert!(
            interp::bmc(&interp::with_buggy(&alone, 1)).is_err(),
            "{invariant} catches the incident's code"
        );
    }
}

/// ONE KNOB PER DEFECT, EACH CAUGHT ON ITS OWN: each knob alone breaks the
/// invariant it names, and the committed configuration breaks none.
#[test]
fn each_defect_is_caught_on_its_own() {
    let model = harness_login_wall_model();
    for (knob, invariant) in KNOBS {
        let alone = only(&model, invariant);
        assert!(
            interp::bmc(&interp::with_consts(&alone, &[(knob, 0)])).is_ok(),
            "{knob} off"
        );
        let caught = interp::bmc(&interp::with_consts(&alone, &[(knob, 1)]));
        assert!(caught.is_err(), "{knob} is caught by {invariant}");
    }
}

/// THE INCIDENT AS IT RAN (`Buggy = 1`, 0.93.0 and main before the fix), with
/// the asks scaled to the model's bound: the login goes; a background
/// completion's turn is answered by the wall; the supervisor continues into
/// it and tells nobody; the upgrade types a notice into it, lets its window
/// run out at the wall, types another, and gives up having reached the model
/// with neither; the owner's `/login`, their `continue`, the agent's READY,
/// the restart. Every step is `Buggy`'s, the committed model refuses its
/// first act at the wall (the supervisor's `Continue`), and the end state
/// breaks all four of the incident's invariants.
#[test]
fn the_incident_as_it_ran_is_the_caught_negative_control() {
    let m = harness_login_wall_model();
    let buggy = interp::with_buggy(&m, 1);
    let ran = [
        "Expire",
        "WallHit",
        "Continue",
        "Announce",
        "Elapse",
        "Announce",
        "Elapse",
        "GiveUp",
        "LoginBack",
        "Continue",
        "AgentReady",
        "Restart",
    ];
    let mut s = buggy.init_state();
    let mut refused = None;
    for action in ran {
        let prev = s.clone();
        assert!(buggy.fire(action, &mut s), "{action} at {prev:?}");
        if refused.is_none() && interp::admits(&m, &prev, &s).is_none() {
            refused = Some(action);
        }
    }
    assert_eq!(refused, Some("Continue"), "the first act at the wall");
    assert_eq!((s["phase"], s["got"]), (3, 0), "restarted, no notice read");
    for invariant in INCIDENT {
        assert!(!buggy.check_invariant(invariant, &s), "{invariant}: {s:?}");
    }
}

/// THE INCIDENT NOW: the wall shows, `/login` is typed once and the owner
/// told; nothing else is typed — no continuation, no notice, and no clock
/// runs — through the dialog and its dismissal; once the person's login is
/// back the worker is continued, the notice typed and read, READY, the
/// restart. A notice typed as the login silently went is typed again as the
/// same ask once it is back, and the give-up a build before the fix left
/// (`Inherit`) is taken back — asked from the first ask the model has not
/// had, and given up again only after `MaxAsks` it received.
#[test]
fn the_incident_now_is_waited_out_and_nothing_is_spent() {
    let m = harness_login_wall_model();
    let mut s = walk(&m, &["Expire", "WallHit", "TypeLogin"]);
    assert_eq!((s["track"], s["told"]), (1, 1));
    for quiet in ["Continue", "Announce", "Elapse", "TypeLogin"] {
        assert!(!m.action_enabled(quiet, &s), "{quiet} at the wall: {s:?}");
    }
    assert!(m.fire("Dismiss", &mut s));
    for quiet in ["Continue", "Announce", "TypeLogin"] {
        assert!(!m.action_enabled(quiet, &s), "{quiet} after the dismissal");
    }
    for action in ["LoginBack", "Continue", "Announce", "AgentReady", "Restart"] {
        assert!(m.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!((s["phase"], s["got"], s["asks"]), (3, 1, 1));

    // A notice the wall answered: the same ask again once the login is back.
    let mut s = walk(&m, &["Expire", "Announce"]);
    assert_eq!((s["asks"], s["unread"], s["got"]), (1, 1, 0));
    assert!(!m.action_enabled("Elapse", &s), "no clock at the wall");
    assert!(m.fire("LoginBack", &mut s));
    assert!(m.fire("Announce", &mut s));
    assert_eq!((s["asks"], s["unread"], s["got"]), (1, 0, 1));

    // The inherited give-up: taken back from the first ask, and given up
    // again only after the bound, every notice received.
    let mut s = walk(&m, &["Expire", "WallHit", "Inherit"]);
    assert_eq!((s["phase"], s["asks"], s["got"]), (2, 2, 0));
    assert!(!m.action_enabled("Announce", &s), "not at the wall");
    for action in [
        "LoginBack",
        "Announce",
        "Elapse",
        "Announce",
        "Elapse",
        "GiveUp",
    ] {
        assert!(m.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!((s["phase"], s["got"], s["spent"]), (2, 2, 0));

    // At every reachable state, whatever the upgrade typed was typed off the
    // wall, and a give-up follows the bound's notices received.
    let mut gave_up = 0;
    for s in reachable(&m) {
        if s["phase"] == 2 && s["inherited"] == 0 {
            assert_eq!(s["got"], 2, "{s:?}");
            gave_up += 1;
        }
    }
    assert!(gave_up > 0, "the give-up is reached");
}

/// THE RENEWABLE LOGIN (S7), as it goes now: the login made renewable, gone,
/// its wall — no `/login` and no continuation, the owner asked to renew it;
/// a person's `/status` hides the row and the wait holds; the renewal, after
/// which the ask stops and the session is relaunched under the renewed login
/// and continued. A session renewed before its wall follows the renewed
/// login at an idle point. NEGATIVE CONTROLS, each the build before S7 on
/// its own knob: `/login` typed on the roster; the session asked again (or
/// continued) on the dead login with the renewed one ready; and the renewal
/// that checks the idle login, which kills it.
#[test]
fn a_renewable_login_is_waited_at_asked_renewed_and_followed() {
    let m = harness_login_wall_model();
    let mut s = walk(&m, &["Enrol", "Expire", "WallHit"]);
    for quiet in ["TypeLogin", "Continue", "Relaunch", "Follow"] {
        assert!(!m.action_enabled(quiet, &s), "{quiet} at the wall: {s:?}");
    }
    assert!(m.fire("AskRenew", &mut s));
    assert_eq!((s["asked"], s["told"]), (1, 1));
    assert!(m.fire("Hide", &mut s));
    for quiet in ["TypeLogin", "Continue"] {
        assert!(
            !m.action_enabled(quiet, &s),
            "{quiet} with the row hidden: {s:?}"
        );
    }
    assert!(m.action_enabled("AskRenew", &s), "the ask stands");
    assert!(m.fire("Renew", &mut s));
    assert_eq!(
        (s["checked"], s["login"]),
        (0, 0),
        "the renewal touched no live login"
    );
    assert!(!m.action_enabled("AskRenew", &s), "renewed: asked no more");
    assert!(m.fire("Relaunch", &mut s));
    assert_eq!(
        (s["login"], s["wall"], s["stood"], s["renewed"]),
        (1, 0, 0, 0)
    );
    assert!(m.fire("Continue", &mut s), "its continuation");
    for invariant in INVARIANTS {
        assert!(m.check_invariant(invariant, &s), "{invariant}: {s:?}");
    }
    // Renewed ahead of the wall: followed at an idle point.
    let mut s = walk(&m, &["Enrol", "Renew"]);
    assert!(!m.action_enabled("Continue", &s), "it follows instead");
    assert!(m.fire("Follow", &mut s));
    assert_eq!((s["login"], s["renewed"]), (1, 0));

    // NEGATIVE CONTROLS, one knob each.
    let typed = interp::with_consts(&m, &[("TypeOnRoster", 1)]);
    let mut s = walk(&typed, &["Enrol", "Expire", "WallHit"]);
    assert!(typed.fire("TypeLogin", &mut s));
    assert!(!typed.check_invariant("NoLoginTypedOnARoster", &s));
    let stuck = interp::with_consts(&m, &[("NoFollow", 1)]);
    let mut s = walk(&stuck, &["Enrol", "Expire", "WallHit", "Renew"]);
    assert!(!stuck.action_enabled("Relaunch", &s));
    assert!(stuck.fire("AskRenew", &mut s));
    assert!(!stuck.check_invariant("NoSessionLeftOnADeadLogin", &s));
    let check = interp::with_consts(&m, &[("CheckIdle", 1)]);
    let mut s = walk(&check, &["Enrol"]);
    assert!(check.fire("Renew", &mut s));
    assert_eq!(s["login"], 0, "the idle login's refresh token spent");
    assert!(!check.check_invariant("NoCheckUnderAnIdleLogin", &s));
}
