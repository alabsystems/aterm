// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tier-0 of `HarnessUpgradeNeverStrands`
//! (`aterm_spec::derive::harness_upgrade_never_strands_model`): proved at
//! `Buggy = 0` and caught at `Buggy = 1` on EACH invariant, with no dead action
//! at the committed configuration; ONE KNOB PER DEFECT, each caught on its own
//! by the invariant it breaks; the last word (`Look`) checked, state by state,
//! to be exactly the reducer's own guards negated; the incident of 2026-09-25/26
//! walked as the schedule the model was written for. Tier-1 is aterm-agent's
//! `harness::upgrade_drive` tests (`upgrade_stall_tests.rs`), over the real
//! reducer, gates, record transitions and the host's reading of each word.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use aterm_spec::derive::{Model, harness_upgrade_never_strands_model};
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

/// The defects the fix and its reviews found, one knob each, and the
/// invariant that catches each alone.
const KNOBS: [(&str, &str); 9] = [
    ("NoF1", "NoNoticeWhileLimited"),
    ("NoF2", "NeverStranded"),
    ("NoOwe", "NeverStranded"),
    ("NoType", "NeverStranded"),
    ("KeepReady", "NeverStranded"),
    ("LastWhileOwed", "NeverStranded"),
    ("StaleDirection", "NeverStranded"),
    ("UnansweredDirection", "NeverStranded"),
    ("ReadyOverDirection", "NoRestartOverDirection"),
];

#[test]
fn the_upgrade_never_strands_the_agent_it_asked() {
    let model = harness_upgrade_never_strands_model();
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
        "harness upgrade never strands: no notice while limited, and every agent asked is \
         restarted or released",
    );
    // Each invariant is load-bearing on its own: the pre-fix reducer breaks both.
    for invariant in ["NoNoticeWhileLimited", "NeverStranded"] {
        let alone = only(&model, invariant);
        assert!(
            interp::bmc(&interp::with_buggy(&alone, 0)).is_ok(),
            "{invariant}"
        );
        assert!(
            interp::bmc(&interp::with_buggy(&alone, 1)).is_err(),
            "{invariant} catches the pre-fix reducer"
        );
    }
}

/// ONE KNOB PER DEFECT, EACH CAUGHT ON ITS OWN (the review of 2026-09-26: with
/// the hand-written last word, F2 reverted, the release never typed, and both
/// together all HELD — 90, 72 and 72 states — so `NeverStranded` proved nothing
/// about either). Each knob alone breaks the invariant it names, and the
/// committed configuration breaks none.
#[test]
fn each_defect_is_caught_on_its_own() {
    let model = harness_upgrade_never_strands_model();
    for (knob, invariant) in KNOBS {
        let alone = only(&model, invariant);
        assert!(
            interp::bmc(&interp::with_consts(&alone, &[(knob, 0)])).is_ok(),
            "{knob} off"
        );
        let caught = interp::bmc(&interp::with_consts(&alone, &[(knob, 1)]));
        assert!(caught.is_err(), "{knob} is caught by {invariant}");
    }
    // The review's own probes, as knobs: F2 reverted, and the release never
    // typed, together — caught.
    let both = interp::with_consts(&model, &[("NoF2", 1), ("NoType", 1)]);
    assert!(interp::bmc(&only(&both, "NeverStranded")).is_err());
    // A release dropped over an agent holding for the restart is the direction
    // knobs' own defect, and theirs alone.
    for (knob, _) in KNOBS {
        let dropped = interp::bmc(&interp::with_consts(
            &only(&model, "NoDropOverAHold"),
            &[(knob, 1)],
        ));
        assert_eq!(
            dropped.is_err(),
            knob.ends_with("Direction") && knob != "ReadyOverDirection",
            "{knob}"
        );
    }
}

/// THE LAST WORD IS THE REDUCER'S OWN GUARDS NEGATED: on every reachable state
/// of the upgrade's last phases (gave up, stopped) at a point the agent can
/// read, `Look` is enabled exactly where none of `Restart`, `Release` and
/// `DropRelease` is —
/// at the committed configuration and under every knob but `LastWhileOwed`,
/// whose defect IS a last word said over a release (there it fires where
/// `Release` also does, at a stop). No `Buggy` disjunct redefines it.
#[test]
fn the_last_word_is_derived_from_the_reducers_guards() {
    let model = harness_upgrade_never_strands_model();
    let configs: Vec<(&str, Model)> = std::iter::once(("committed", model.clone()))
        .chain(std::iter::once(("Buggy", interp::with_buggy(&model, 1))))
        .chain(
            KNOBS
                .iter()
                .map(|(knob, _)| (*knob, interp::with_consts(&model, &[(knob, 1)]))),
        )
        .collect();
    for (name, m) in &configs {
        let mut looked = 0;
        for s in reachable(m) {
            if !(s["phase"] == 2 || s["phase"] == 4) || s["limited"] != 0 || s["stuck"] != 0 {
                assert!(!m.action_enabled("Look", &s), "{name}: {s:?}");
                continue;
            }
            let last = !m.action_enabled("Restart", &s)
                && !m.action_enabled("Release", &s)
                && !m.action_enabled("DropRelease", &s);
            let look = m.action_enabled("Look", &s);
            if *name == "LastWhileOwed" {
                assert_eq!(
                    look,
                    last || (s["phase"] == 4 && !m.action_enabled("Restart", &s)),
                    "{name}: {s:?}"
                );
            } else {
                assert_eq!(look, last, "{name}: {s:?}");
            }
            looked += usize::from(look);
        }
        assert!(looked > 0, "{name}: the last word is said somewhere");
    }
}

/// A DIRECTION DROPS A RELEASE ONLY ONCE THE AGENT HAS TAKEN IT UP (the
/// second review of 2026-09-26). The review's sequence — asked to the bound,
/// given up, a peer's message, the agent's READY, that READY voided — owes a
/// release that is TYPED; under `StaleDirection` (the message still counted,
/// as it was) it is dropped, and the upgrade's last word finds the agent
/// holding. A direction the agent took up drops it, the agent at work. One it
/// has not answered yet drops nothing (`UnansweredDirection` drops it, and the
/// READY the agent then gives is to markers nothing keeps). A READY someone
/// spoke after is no consent (`ReadyOverDirection` restarts over them). And
/// at EVERY reachable state of the committed model the drop's guard — the
/// driver's own rule — finds the agent at work: `holding == 0` is proved of
/// it, never assumed in it.
#[test]
fn a_release_is_dropped_only_for_an_agent_that_took_other_work_up() {
    let m = harness_upgrade_never_strands_model();
    let asked = ["Announce", "Elapse", "Announce", "Elapse", "GiveUp"];
    let then = |more: &[&'static str]| -> Vec<&'static str> { [&asked[..], more].concat() };
    let review = then(&["Direct", "AgentReady", "Void"]);
    let mut s = run(&m, &review);
    assert_eq!((s["holding"], s["owed"], s["directed"]), (1, 1, 0));
    assert!(!m.action_enabled("DropRelease", &s), "{s:?}");
    assert!(m.fire("Release", &mut s));
    assert_eq!(s["holding"], 0, "released");
    let stale = interp::with_consts(&m, &[("StaleDirection", 1)]);
    let mut s = run(&stale, &review);
    assert!(stale.action_enabled("DropRelease", &s) && !stale.action_enabled("Release", &s));
    for action in ["DropRelease", "Look"] {
        assert!(stale.fire(action, &mut s), "{action}: {s:?}");
    }
    assert!(!stale.check_invariant("NeverStranded", &s), "{s:?}");
    assert!(!stale.check_invariant("NoDropOverAHold", &s), "{s:?}");

    // Taken up: dropped, the agent at work; the last word finds it so.
    let mut s = run(&m, &then(&["Direct", "AgentGoesOn"]));
    for action in ["DropRelease", "Look"] {
        assert!(m.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!((s["holding"], s["stuck"], s["dropheld"]), (0, 0, 0));

    // Not answered yet: released; `UnansweredDirection` drops it instead.
    let s = run(&m, &then(&["Direct"]));
    assert!(!m.action_enabled("DropRelease", &s) && m.action_enabled("Release", &s));
    let unanswered = interp::with_consts(&m, &[("UnansweredDirection", 1)]);
    let mut s = run(&unanswered, &then(&["Direct", "DropRelease"]));
    assert!(
        !unanswered.action_enabled("AgentReady", &s),
        "a READY now answers nothing kept: {s:?}"
    );
    assert!(unanswered.fire("Look", &mut s));
    assert!(!unanswered.check_invariant("NeverStranded", &s), "{s:?}");

    // A READY someone spoke after is no consent to a restart.
    let s = run(&m, &["Announce", "AgentReady", "Direct"]);
    assert!(!m.action_enabled("Restart", &s), "{s:?}");
    let over = interp::with_consts(&m, &[("ReadyOverDirection", 1)]);
    let mut s = run(&over, &["Announce", "AgentReady", "Direct"]);
    assert!(over.fire("Restart", &mut s));
    assert!(!over.check_invariant("NoRestartOverDirection", &s), "{s:?}");

    // The drop's guard requires `holding == 0` at every reachable state.
    let mut drops = 0;
    for s in reachable(&m) {
        if m.action_enabled("DropRelease", &s) {
            assert_eq!(s["holding"], 0, "{s:?}");
            drops += 1;
        }
    }
    assert!(drops > 0, "the drop is taken somewhere");
}

/// THE INCIDENT, fixed: the limit hits before the notice; nothing is typed
/// and no clock runs until it resets; then one notice, READY, the restart.
/// A notice whose wind-down hits the limit gets its whole window after the
/// reset. And an agent asked to its bound is released, not left holding.
#[test]
fn a_limited_session_is_asked_after_its_reset_and_a_tired_one_is_released() {
    let m = harness_upgrade_never_strands_model();
    let mut limited = run(&m, &["LimitHits"]);
    assert!(
        !m.action_enabled("Announce", &limited),
        "never while limited"
    );
    assert!(m.fire("LimitResets", &mut limited));
    for action in ["Announce", "AgentReady", "Restart"] {
        assert!(m.fire(action, &mut limited), "{action}: {limited:?}");
    }
    assert_eq!((limited["phase"], limited["holding"]), (3, 0));

    // Asked, and the limit hits during the wind-down: the clock is held, and
    // a window that ran out before the limit starts again after it.
    let mut asked = run(&m, &["Announce", "LimitHits"]);
    assert!(!m.action_enabled("Elapse", &asked), "the clock is held");
    assert!(!m.action_enabled("GiveUp", &asked));
    assert!(m.fire("LimitResets", &mut asked));
    assert!(m.action_enabled("Elapse", &asked));
    let mut due = run(&m, &["Announce", "Elapse", "LimitHits", "LimitResets"]);
    assert_eq!(due["window"], 0, "the whole window after the reset");
    assert!(!m.action_enabled("Announce", &due));
    assert!(m.fire("Elapse", &mut due));

    // Asked to the bound and never answered: released, and a READY that comes
    // before the release restarts it — or, held past the drain, is void and
    // the agent still released.
    let tired = run(&m, &["Announce", "Elapse", "Announce", "Elapse", "GiveUp"]);
    assert_eq!((tired["phase"], tired["owed"]), (2, 1));
    assert!(!m.action_enabled("Look", &tired), "a release is owed");
    let mut released = tired.clone();
    assert!(m.fire("Release", &mut released));
    assert_eq!(released["holding"], 0);
    assert!(m.action_enabled("Look", &released), "then the last word");
    let mut late = tired.clone();
    assert!(m.fire("AgentReady", &mut late));
    assert!(
        !m.action_enabled("Release", &late),
        "the restart goes first"
    );
    let mut voided = late.clone();
    assert!(m.fire("Restart", &mut late));
    assert!(m.fire("Void", &mut voided));
    assert!(m.fire("Release", &mut voided), "{voided:?}");
    assert_eq!(voided["holding"], 0);

    // A restart refused after READY: the round's markers go with it, and the
    // release is typed (the review's first blocker; `KeepReady` is the
    // defect, caught above).
    let mut refused = run(&m, &["Announce", "AgentReady", "Abandon"]);
    assert_eq!(
        (refused["ready"], refused["live"], refused["owed"]),
        (0, 0, 1)
    );
    assert!(!m.action_enabled("Look", &refused));
    assert!(m.fire("Release", &mut refused));
    assert_eq!(refused["holding"], 0);
}

/// THE INCIDENT AS IT RAN (`Buggy = 1`, the reducer before the fix): notices
/// typed into the limited session, the clock and the asks spent there, a give
/// up with nothing owed, and the READY after the reset heard by nobody — the
/// last word said over an agent holding. The last word is the same derived
/// one: it fires because the pre-fix reducer has no restart for a gave-up
/// READY, not because the observer changed. The same schedule's first notice
/// is refused at `Buggy = 0`.
#[test]
fn the_incident_as_it_ran_is_the_caught_negative_control() {
    let m = harness_upgrade_never_strands_model();
    let buggy = interp::with_buggy(&m, 1);
    let ran = [
        "LimitHits",
        "Announce",
        "Elapse",
        "Announce",
        "Elapse",
        "GiveUp",
        "LimitResets",
        "AgentReady",
        "Look",
    ];
    let mut s = buggy.init_state();
    for (i, action) in ran.iter().enumerate() {
        if *action == "Look" {
            assert!(!buggy.action_enabled("Restart", &s), "{s:?}");
            assert!(!buggy.action_enabled("Release", &s), "{s:?}");
            assert!(m.action_enabled("Restart", &s), "the fix restarts: {s:?}");
        }
        assert!(buggy.fire(action, &mut s), "{action}: {s:?}");
        if i == 1 {
            assert!(!buggy.check_invariant("NoNoticeWhileLimited", &s));
        }
    }
    assert!(!buggy.check_invariant("NeverStranded", &s), "{s:?}");
    let fixed = run(&m, &["LimitHits"]);
    assert!(!m.action_enabled("Announce", &fixed));
}
