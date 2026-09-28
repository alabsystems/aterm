// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tier-0 of `HarnessLoginWall` (`aterm_spec::derive::harness_login_wall_model`):
//! proved at `Buggy = 0` and caught at `Buggy = 1` on EACH invariant, with no
//! dead action at the committed configuration; ONE KNOB PER DEFECT, each
//! caught on its own by the invariant it breaks; the incident of 2026-09-27
//! walked as it ran (the caught negative control) and as it goes now. Tier-1 is
//! aterm-agent's `conformance_login_wall`, over the real readers, reducer and
//! turn-end decider on every reachable state.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use aterm_spec::derive::{Model, harness_login_wall_model};
use aterm_spec::{interp, verify};

type S = BTreeMap<&'static str, i64>;

fn only(m: &Model, invariant: &str) -> Model {
    let mut m = m.clone();
    m.invariants.retain(|i| i.name == invariant);
    m
}

fn run(m: &Model, actions: &[&str]) -> S {
    let mut s = m.init_state();
    for action in actions {
        assert!(m.fire(action, &mut s), "{action} at {s:?}");
    }
    s
}

/// Every state `m` reaches, invariants unchecked.
fn reachable(m: &Model) -> Vec<S> {
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([m.init_state()]);
    let mut out = Vec::new();
    while let Some(s) = queue.pop_front() {
        if !seen.insert(s.clone()) {
            continue;
        }
        for a in &m.actions {
            let mut next = s.clone();
            if m.fire(a.name, &mut next) {
                queue.push_back(next);
            }
        }
        out.push(s);
    }
    out
}

const INVARIANTS: [&str; 4] = [
    "NoNoticeAtTheWall",
    "NoGiveUpUnread",
    "NoFutileContinue",
    "TheOwnerIsToldFirst",
];

/// The defects the fix repairs, one knob each, and the invariant that
/// catches each alone.
const KNOBS: [(&str, &str); 6] = [
    ("NoGate", "NoNoticeAtTheWall"),
    ("NoRefund", "NoGiveUpUnread"),
    ("ClockAtWall", "NoGiveUpUnread"),
    ("NoSee", "NoFutileContinue"),
    ("NoSee", "TheOwnerIsToldFirst"),
    ("NoHold", "NoFutileContinue"),
];

#[test]
fn the_login_wall_model_proves_and_catches() {
    let model = harness_login_wall_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the model must stay enrolled in the spec-link registry"
    );
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
/// breaks all four invariants.
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
    for invariant in INVARIANTS {
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
    let mut s = run(&m, &["Expire", "WallHit", "TypeLogin"]);
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
    let mut s = run(&m, &["Expire", "Announce"]);
    assert_eq!((s["asks"], s["unread"], s["got"]), (1, 1, 0));
    assert!(!m.action_enabled("Elapse", &s), "no clock at the wall");
    assert!(m.fire("LoginBack", &mut s));
    assert!(m.fire("Announce", &mut s));
    assert_eq!((s["asks"], s["unread"], s["got"]), (1, 0, 1));

    // The inherited give-up: taken back from the first ask, and given up
    // again only after the bound, every notice received.
    let mut s = run(&m, &["Expire", "WallHit", "Inherit"]);
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
