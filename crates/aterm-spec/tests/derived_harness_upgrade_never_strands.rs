// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tier-0 of `HarnessUpgradeNeverStrands`: proved at `Buggy = 0`, caught at `Buggy = 1` on each
//! invariant, and one knob per defect caught alone by the invariant it breaks. The last word
//! (`Look`) is checked state by state to be the reducer's own guards negated, and no stop is for
//! good: every stopped round whose rest has run out has a way on. Tier-1 is aterm-agent's
//! `harness::upgrade_drive` tests (`upgrade_stall_tests.rs`), over the real reducer, gates,
//! record transitions and the host's reading of each word.

use crate::walk::{only, reachable, walk};
use aterm_spec::derive::{Model, harness_upgrade_never_strands_model};
use aterm_spec::{interp, verify};

/// One knob per defect, and the invariant that catches each alone.
const KNOBS: [(&str, &str); 13] = [
    ("Terminal", "NeverStalls"),
    ("NoF1", "NoNoticeWhileLimited"),
    ("NoF2", "NeverStranded"),
    ("NoOwe", "NeverStranded"),
    ("NoType", "NeverStranded"),
    ("KeepReady", "NeverStranded"),
    ("LastWhileOwed", "NeverStranded"),
    ("StaleDirection", "NeverStranded"),
    ("UnansweredDirection", "NeverStranded"),
    ("ReadyOverDirection", "NoRestartOverDirection"),
    ("IdleOnlyRelease", "NeverStranded"),
    ("HeldAsked", "NeverStranded"),
    ("HeldRelease", "NeverStranded"),
];

#[test]
fn the_upgrade_never_strands_the_agent_it_asked() {
    let model = harness_upgrade_never_strands_model();
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
    // Each invariant is load-bearing alone: `Buggy = 1` breaks every one.
    for invariant in ["NoNoticeWhileLimited", "NeverStranded", "NeverStalls"] {
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

/// Each knob alone breaks the invariant it names, and the committed configuration breaks none.
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
    // `NoF2` and `NoType` together: caught.
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

/// The last word is the reducer's own guards negated: wherever the agent can read in the
/// upgrade's last phases (or in a pending or announced round whose build fails to start), `Look`
/// is enabled exactly where none of `Restart`, `BuildHold`, `Release`, `DropRelease` and `Rearm`
/// is (at a break the drain's `Void` of a gave-up READY stands for `Restart`), and nowhere else.
/// It holds committed, under `Buggy` and under every knob but `LastWhileOwed`, whose defect is a
/// last word said over a release at a stop.
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
            let quiet_phase =
                s["phase"] == 2 || s["phase"] == 4 || (s["fails"] == 1 && s["phase"] <= 1);
            if !quiet_phase || s["limited"] != 0 || s["stuck"] != 0 {
                assert!(!m.action_enabled("Look", &s), "{name}: {s:?}");
                continue;
            }
            let restart = m.action_enabled("Restart", &s)
                || (s["brk"] == 1 && s["phase"] == 2 && m.action_enabled("Void", &s));
            let last = !restart
                && !m.action_enabled("BuildHold", &s)
                && !m.action_enabled("Release", &s)
                && !m.action_enabled("DropRelease", &s)
                && !m.action_enabled("Rearm", &s);
            let look = m.action_enabled("Look", &s);
            if *name == "LastWhileOwed" {
                assert_eq!(
                    look,
                    last || (s["phase"] == 4
                        && !restart
                        && !m.action_enabled("BuildHold", &s)
                        && !m.action_enabled("Rearm", &s)),
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

/// A direction drops a release only once the agent has taken it up: one it took up drops it, the
/// agent at work; one it has not answered drops nothing. `StaleDirection` and
/// `UnansweredDirection` drop it with the agent holding, and `ReadyOverDirection` takes a READY
/// someone spoke after for consent. The drop's guard finds the agent at work at every reachable
/// state: `holding == 0` is proved of it, never assumed in it.
#[test]
fn a_release_is_dropped_only_for_an_agent_that_took_other_work_up() {
    let m = harness_upgrade_never_strands_model();
    let asked = ["Announce", "Elapse", "Announce", "Elapse", "GiveUp"];
    let then = |more: &[&'static str]| -> Vec<&'static str> { [&asked[..], more].concat() };
    let review = then(&["Direct", "AgentReady", "Void"]);
    let mut s = walk(&m, &review);
    assert_eq!((s["holding"], s["owed"], s["directed"]), (1, 1, 0));
    assert!(!m.action_enabled("DropRelease", &s), "{s:?}");
    assert!(m.fire("Release", &mut s));
    assert_eq!(s["holding"], 0, "released");
    let stale = interp::with_consts(&m, &[("StaleDirection", 1)]);
    let mut s = walk(&stale, &review);
    assert!(stale.action_enabled("DropRelease", &s) && !stale.action_enabled("Release", &s));
    for action in ["DropRelease", "Look"] {
        assert!(stale.fire(action, &mut s), "{action}: {s:?}");
    }
    assert!(!stale.check_invariant("NeverStranded", &s), "{s:?}");
    assert!(!stale.check_invariant("NoDropOverAHold", &s), "{s:?}");

    // Taken up: dropped, the agent at work; the last word finds it so.
    let mut s = walk(&m, &then(&["Direct", "AgentGoesOn"]));
    for action in ["DropRelease", "Look"] {
        assert!(m.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!((s["holding"], s["stuck"], s["dropheld"]), (0, 0, 0));

    // Not answered yet: released; `UnansweredDirection` drops it instead.
    let s = walk(&m, &then(&["Direct"]));
    assert!(!m.action_enabled("DropRelease", &s) && m.action_enabled("Release", &s));
    let unanswered = interp::with_consts(&m, &[("UnansweredDirection", 1)]);
    let mut s = walk(&unanswered, &then(&["Direct", "DropRelease"]));
    assert!(
        !unanswered.action_enabled("AgentReady", &s),
        "a READY now answers nothing kept: {s:?}"
    );
    assert!(unanswered.fire("Look", &mut s));
    assert!(!unanswered.check_invariant("NeverStranded", &s), "{s:?}");

    // A READY someone spoke after is no consent to a restart.
    let s = walk(&m, &["Announce", "AgentReady", "Direct"]);
    assert!(!m.action_enabled("Restart", &s), "{s:?}");
    let over = interp::with_consts(&m, &[("ReadyOverDirection", 1)]);
    let mut s = walk(&over, &["Announce", "AgentReady", "Direct"]);
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

/// The limit hits before the notice: nothing is typed and no clock runs until it resets; then
/// one notice, READY, the restart. A notice whose wind-down hits the limit gets its whole
/// window after the reset, and an agent asked to its bound is released, not left holding.
#[test]
fn a_limited_session_is_asked_after_its_reset_and_a_tired_one_is_released() {
    let m = harness_upgrade_never_strands_model();
    let mut limited = walk(&m, &["LimitHits"]);
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
    let mut asked = walk(&m, &["Announce", "LimitHits"]);
    assert!(!m.action_enabled("Elapse", &asked), "the clock is held");
    assert!(!m.action_enabled("GiveUp", &asked));
    assert!(m.fire("LimitResets", &mut asked));
    assert!(m.action_enabled("Elapse", &asked));
    let mut due = walk(&m, &["Announce", "Elapse", "LimitHits", "LimitResets"]);
    assert_eq!(due["window"], 0, "the whole window after the reset");
    assert!(!m.action_enabled("Announce", &due));
    assert!(m.fire("Elapse", &mut due));

    // Asked to the bound and never answered: released; a READY before the release restarts it,
    // and one held past the drain is void, the agent still released.
    let tired = walk(&m, &["Announce", "Elapse", "Announce", "Elapse", "GiveUp"]);
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

    // A restart refused after READY: the round's markers go with it, and the release is typed
    // (`KeepReady` is the defect, caught above).
    let mut refused = walk(&m, &["Announce", "AgentReady", "Abandon"]);
    assert_eq!(
        (refused["ready"], refused["live"], refused["owed"]),
        (0, 0, 1)
    );
    assert!(!m.action_enabled("Look", &refused));
    assert!(m.fire("Release", &mut refused));
    assert_eq!(refused["holding"], 0);
}

/// `Buggy = 1` on the limited session: notices and asks spent into the limit, a give-up with
/// nothing owed, and the READY after the reset heard by nobody, so the (same, derived) last word
/// is said over an agent holding. The schedule's first notice is refused at `Buggy = 0`.
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
    let fixed = walk(&m, &["LimitHits"]);
    assert!(!m.action_enabled("Announce", &fixed));
}

/// A break that never ends (widowed `tail -f` shells under a Claude Code whose status reads
/// `shell`): the agent asked to the bound is released at the break, and a late READY is voided
/// there and released the same way; the restart is never taken at a break. Under
/// `IdleOnlyRelease` the break's own last word finds the agent holding.
#[test]
fn a_break_that_never_ends_still_releases_the_agent_it_asked() {
    let m = harness_upgrade_never_strands_model();
    let tired = [
        "BreakBegins",
        "Announce",
        "Elapse",
        "Announce",
        "Elapse",
        "GiveUp",
    ];
    let s = walk(&m, &tired);
    assert_eq!(
        (s["brk"], s["phase"], s["owed"], s["holding"]),
        (1, 2, 1, 1)
    );
    assert!(!m.action_enabled("Look", &s), "a release is owed: {s:?}");
    let mut released = s.clone();
    assert!(m.fire("Release", &mut released), "{s:?}");
    assert_eq!(released["holding"], 0);
    assert!(m.fire("Look", &mut released));
    assert!(m.check_invariant("NeverStranded", &released));
    // The late READY the shells outlive: no restart at a break; voided,
    // then released.
    let mut late = s.clone();
    assert!(m.fire("AgentReady", &mut late));
    assert!(!m.action_enabled("Restart", &late), "never at a break");
    assert!(
        !m.action_enabled("Look", &late),
        "the void is still to come"
    );
    for action in ["Void", "Release", "Look"] {
        assert!(m.fire(action, &mut late), "{action}: {late:?}");
    }
    assert!(m.check_invariant("NeverStranded", &late), "{late:?}");

    // `IdleOnlyRelease`: the release, and the void, only at an idle point.
    let idle_only = interp::with_consts(&m, &[("IdleOnlyRelease", 1)]);
    let mut s = walk(&idle_only, &tired);
    assert!(!idle_only.action_enabled("Release", &s), "{s:?}");
    assert!(idle_only.fire("Look", &mut s));
    assert!(!idle_only.check_invariant("NeverStranded", &s), "{s:?}");
    let mut late = walk(&idle_only, &tired);
    assert!(idle_only.fire("AgentReady", &mut late));
    assert!(!idle_only.action_enabled("Void", &late), "{late:?}");
    assert!(idle_only.fire("Look", &mut late));
    assert!(
        !idle_only.check_invariant("NeverStranded", &late),
        "{late:?}"
    );
}

/// No reachable state is a permanent wait: a stopped round whose rest has run out has a way on
/// (the new round, the restart a late READY still takes, or the environment's hold on it), a
/// round still resting has its clock, and a re-armed round is asked wherever the session can
/// read. None of it holds at `Terminal = 1` (or `Buggy = 1`).
#[test]
fn no_reachable_state_is_a_permanent_wait() {
    let m = harness_upgrade_never_strands_model();
    let mut rested = 0;
    for s in reachable(&m) {
        if !(s["phase"] == 2 || s["phase"] == 4) {
            continue;
        }
        if s["rest"] == 0 {
            assert!(m.action_enabled("Rests", &s), "{s:?}");
            continue;
        }
        rested += 1;
        let late = s["phase"] == 2 && s["ready"] == 1 && s["live"] == 1;
        if late {
            // The answer is acted on — or the environment holds it: the
            // limit (the restart waits it out), or the drain's void — or, the
            // build failing to start, the round is abandoned.
            assert!(
                m.action_enabled("Restart", &s)
                    || s["limited"] == 1
                    || m.action_enabled("Void", &s)
                    || m.action_enabled("BuildHold", &s),
                "{s:?}"
            );
        } else {
            assert!(m.action_enabled("Rearm", &s), "{s:?}");
            let mut next = s.clone();
            assert!(m.fire("Rearm", &mut next));
            assert_eq!(
                (next["phase"], next["asks"], next["live"], next["rest"]),
                (0, 0, 0, 0),
                "a fresh round: {next:?}"
            );
            if next["limited"] == 0 && next["fails"] == 0 {
                assert!(m.action_enabled("Announce", &next), "asked: {next:?}");
            } else {
                // A limited session is re-armed but never asked, nor one
                // whose build fails to start.
                assert!(!m.action_enabled("Announce", &next), "{next:?}");
            }
        }
        assert!(!m.action_enabled("Look", &s) || late, "{s:?}");
    }
    assert!(rested > 0, "a rested round is reached");
    for (name, dead) in [
        ("Terminal", interp::with_consts(&m, &[("Terminal", 1)])),
        ("Buggy", interp::with_buggy(&m, 1)),
    ] {
        let stuck = reachable(&dead).into_iter().any(|s| {
            (s["phase"] == 2 || s["phase"] == 4)
                && s["rest"] == 1
                && s["limited"] == 0
                && !dead.action_enabled("Rearm", &s)
                && dead.action_enabled("Look", &s)
        });
        assert!(stuck, "{name}: a rested round sits at the quiet word");
    }
}

/// A gave-up round walked to its new round: the late READY voided past the drain, the release,
/// then the rest run out and the new round's restart. Under `Terminal` the walk ends at the
/// quiet word, and `NeverStalls` catches it.
#[test]
fn the_owners_stall_rearms_and_the_terminal_reducer_is_caught() {
    let m = harness_upgrade_never_strands_model();
    let path = [
        "Announce",
        "Elapse",
        "Announce",
        "Elapse",
        "GiveUp",
        "AgentReady",
        "Void",
        "Release",
        "Rests",
    ];
    let mut s = walk(&m, &path);
    assert_eq!(
        (s["phase"], s["rest"], s["owed"], s["holding"]),
        (2, 1, 0, 0)
    );
    assert!(!m.action_enabled("Look", &s), "no quiet word past the rest");
    for action in ["Rearm", "Announce", "AgentReady", "Restart"] {
        assert!(m.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!((s["phase"], s["holding"], s["stalled"]), (3, 0, 0));
    for invariant in ["NeverStalls", "NeverStranded", "NoNoticeWhileLimited"] {
        assert!(m.check_invariant(invariant, &s), "{invariant}");
    }

    let terminal = interp::with_consts(&m, &[("Terminal", 1)]);
    let mut s = walk(&terminal, &path);
    assert!(!terminal.action_enabled("Rearm", &s), "{s:?}");
    assert!(terminal.fire("Look", &mut s), "{s:?}");
    assert!(!terminal.check_invariant("NeverStalls", &s), "{s:?}");
    assert!(
        terminal.check_invariant("NeverStranded", &s),
        "the agent was released: stalled, not stranded"
    );
}

/// A build that fails to start strands no agent it holds: no notice or restart goes onto it, and
/// every agent the round asked is released, a pending round that still owes a release typing it.
/// `HeldAsked` (the hold only a wait where the round would move) and `HeldRelease` each leave a
/// last word over an agent holding.
#[test]
fn a_build_that_fails_to_start_strands_no_agent_it_holds() {
    let m = harness_upgrade_never_strands_model();
    let released = |m: &Model, path: &[&'static str]| {
        let mut s = walk(m, path);
        assert_eq!(s["holding"], 0, "{path:?}: released: {s:?}");
        for invariant in ["NeverStranded", "NeverStalls"] {
            assert!(m.check_invariant(invariant, &s), "{invariant}: {s:?}");
        }
        if m.fire("Look", &mut s) {
            assert!(m.check_invariant("NeverStranded", &s), "{s:?}");
        }
    };
    let announced = ["Announce", "BuildFails"];
    let s = walk(&m, &announced);
    assert!(!m.action_enabled("Announce", &s) && !m.action_enabled("Look", &s));
    released(&m, &["Announce", "BuildFails", "BuildHold", "Release"]);
    let ready = ["Announce", "AgentReady", "BuildFails"];
    let s = walk(&m, &ready);
    assert!(!m.action_enabled("Restart", &s), "no move onto it: {s:?}");
    released(
        &m,
        &[
            "Announce",
            "AgentReady",
            "BuildFails",
            "BuildHold",
            "Release",
        ],
    );
    let late = [
        "Announce",
        "Elapse",
        "Announce",
        "Elapse",
        "GiveUp",
        "AgentReady",
        "BuildFails",
    ];
    let s = walk(&m, &late);
    assert!(!m.action_enabled("Restart", &s) && !m.action_enabled("Release", &s));
    released(&m, &[&late[..], &["BuildHold", "Release"][..]].concat());
    // Given up, the release left owed past the rest, re-armed: a pending
    // round that owes it, and no notice to come.
    let owed = [
        "Announce",
        "Elapse",
        "Announce",
        "Elapse",
        "GiveUp",
        "Rests",
        "Rearm",
        "BuildFails",
    ];
    let s = walk(&m, &owed);
    assert_eq!((s["phase"], s["owed"], s["holding"]), (0, 1, 1), "{s:?}");
    assert!(!m.action_enabled("Announce", &s) && !m.action_enabled("Look", &s));
    released(&m, &[&owed[..], &["Release"][..]].concat());
    // And the build proved to start since: the next round is asked.
    let mut proved = walk(&m, &[&owed[..], &["Release", "BuildStarts"][..]].concat());
    assert!(m.fire("Announce", &mut proved), "{proved:?}");

    // Each strand under its knob.
    let held = interp::with_consts(&m, &[("HeldAsked", 1)]);
    for path in [&announced[..], &ready[..], &late[..]] {
        let mut s = walk(&held, path);
        assert!(
            !held.action_enabled("Restart", &s) && !held.action_enabled("Announce", &s),
            "{path:?}"
        );
        if path == &announced[..] {
            assert!(held.fire("Elapse", &mut s), "the window runs out: {s:?}");
        }
        assert!(held.fire("Look", &mut s), "{path:?}: {s:?}");
        assert!(
            !held.check_invariant("NeverStranded", &s),
            "{path:?}: {s:?}"
        );
    }
    let unreleased = interp::with_consts(&m, &[("HeldRelease", 1)]);
    let mut s = walk(&unreleased, &owed);
    assert!(!unreleased.action_enabled("Release", &s), "{s:?}");
    assert!(unreleased.fire("Look", &mut s), "{s:?}");
    assert!(!unreleased.check_invariant("NeverStranded", &s), "{s:?}");
}
