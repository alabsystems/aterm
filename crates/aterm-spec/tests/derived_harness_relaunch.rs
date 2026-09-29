// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{
    derive::{
        harness_codex_exit_witness_model, harness_exit_record_model, harness_relaunch_on_exit_model,
    },
    interp, verify,
};

#[test]
fn a_person_wins_and_no_exit_is_dropped_silently() {
    let model = harness_relaunch_on_exit_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the relaunch machine must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "harness relaunch on exit");

    // A crash is relaunched; one that keeps missing is said to a person
    // after `Badge` misses in a row, and still tried; a landing clears it.
    let badge = model.consts.iter().find(|c| c.0 == "Badge").unwrap().1;
    let mut s = model.init_state();
    assert!(model.fire("Crash", &mut s));
    for n in 0..badge {
        assert_eq!(s["badged"], 0, "after {n} misses: {s:?}");
        assert!(model.fire("Miss", &mut s));
    }
    assert_eq!((s["badged"], s["pending"]), (1, 1), "{s:?}");
    assert!(model.fire("Land", &mut s));
    assert_eq!((s["badged"], s["running"]), (0, 1), "{s:?}");

    // The back-off grows to its last step and stays there…
    let steps = model.consts.iter().find(|c| c.0 == "Steps").unwrap().1;
    for _ in 0..steps + 2 {
        assert!(model.fire("Crash", &mut s) && model.fire("Land", &mut s));
    }
    assert_eq!(s["step"], steps, "{s:?}");
    // …until a healthy run starts it over.
    assert!(model.fire("Healthy", &mut s));
    assert_eq!(s["step"], 0);

    // A person's exit leaves the tab to them: nothing is pending, and only
    // they start the next agent.
    let mut p = model.init_state();
    assert!(model.fire("PersonExit", &mut p));
    assert_eq!((p["pending"], p["asked"]), (0, 1), "{p:?}");
    assert!(!model.action_enabled("Land", &p));
    assert!(model.fire("PersonRuns", &mut p));

    // The launch's own end leaves the tab and says nothing; a limit says so,
    // whether it meets the exit or a relaunch still due.
    let mut e = model.init_state();
    assert!(model.fire("Crash", &mut e) && model.fire("Ended", &mut e));
    assert_eq!((e["pending"], e["asked"], e["badged"]), (0, 1, 0), "{e:?}");
    let mut l = model.init_state();
    assert!(model.fire("Limited", &mut l));
    assert_eq!(
        (l["running"], l["pending"], l["badged"]),
        (0, 0, 1),
        "{l:?}"
    );
    let mut w = model.init_state();
    assert!(model.fire("Crash", &mut w) && model.fire("Limited", &mut w));
    assert_eq!((w["pending"], w["badged"]), (0, 1), "{w:?}");

    // The buggy host: a relaunch after a person's exit, one dropped, and a
    // limit kept quiet.
    let buggy = interp::with_buggy(&model, 1);
    let mut b = buggy.init_state();
    assert!(buggy.fire("PersonExit", &mut b));
    assert!(!buggy.check_invariant("PersonWins", &b));
    let mut d = buggy.init_state();
    assert!(buggy.fire("Crash", &mut d) && buggy.fire("Cannot", &mut d));
    assert!(!buggy.check_invariant("NeverSilent", &d));
    let mut q = buggy.init_state();
    assert!(buggy.fire("Limited", &mut q));
    assert!(!buggy.check_invariant("NeverSilent", &q));

    // The harness's OWN restart ended the agent (S0 of the in-flight review,
    // 2026-09-27): due whoever is at the tab, and never left — no person
    // coming back, no limit and no "end of the launch" takes it off. It
    // lands, is tried again, or is said.
    let mut own = model.init_state();
    assert!(model.fire("Restarted", &mut own));
    assert_eq!(
        (own["pending"], own["asked"], own["restarted"]),
        (1, 0, 1),
        "{own:?}"
    );
    for off in ["PersonReturns", "Limited", "Ended"] {
        assert!(!model.action_enabled(off, &own), "{off} at {own:?}");
    }
    let mut said = own.clone();
    assert!(model.fire("Miss", &mut said) && model.fire("Cannot", &mut said));
    assert_eq!((said["pending"], said["badged"]), (0, 1), "{said:?}");
    assert!(model.fire("Land", &mut own));
    assert_eq!((own["running"], own["restarted"]), (1, 0), "{own:?}");
    // The host of c4e24cd3e: a person's keystroke or a hold read before the
    // restart's record left it to them, and a restart its back-off let
    // expire fell through to the graceful exit its own SIGTERM made.
    for off in ["PersonReturns", "Ended"] {
        let mut b = buggy.init_state();
        assert!(buggy.fire("Restarted", &mut b) && buggy.fire(off, &mut b));
        assert!(!buggy.check_invariant("RestartCarried", &b), "{off}: {b:?}");
    }
}

/// D2 of the 2026-09-26 live test: whether an exit left Claude Code's own
/// record is read as the exit is seen and KEPT, so another Claude Code that
/// removes the dead agent's record during the relaunch's back-off cannot turn
/// a crash into a graceful exit — and the look waits out a graceful exit's
/// own removal, so a clean exit is never read as a crash. NEGATIVE CONTROLS:
/// the host of 57a2b7050 (the record read at the attempt, after the
/// back-off) leaves a crash another Claude swept, and a look at the
/// detection instant relaunches a graceful exit; each is caught.
#[test]
fn a_crash_is_read_at_its_exit_and_kept_through_the_back_off() {
    let model = harness_exit_record_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the exit-record machine must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "harness exit record");

    // A crash, looked at, then swept by another Claude Code during the
    // back-off: relaunched on what the look kept.
    let mut c = model.init_state();
    for a in ["Crash", "Look", "Sweep", "Decide"] {
        assert!(model.fire(a, &mut c), "{a} at {c:?}");
    }
    assert_eq!((c["record"], c["kept"], c["decided"]), (0, 1, 1), "{c:?}");
    // A graceful exit is looked at only once its own removal has landed,
    // and is left.
    let mut g = model.init_state();
    assert!(model.fire("Graceful", &mut g));
    assert!(!model.action_enabled("Look", &g), "not before its removal");
    assert!(model.fire("OwnRemoval", &mut g) && model.fire("Look", &mut g));
    assert!(model.fire("Decide", &mut g));
    assert_eq!((g["kept"], g["decided"]), (0, 2), "{g:?}");
    // No sweep before the look: the quarter second of the settle is the
    // window the design leaves, stated by the guard.
    let mut early = model.init_state();
    assert!(model.fire("Crash", &mut early));
    assert!(!model.action_enabled("Sweep", &early));

    // The buggy hosts: the record read at the attempt leaves a swept crash…
    let buggy = interp::with_buggy(&model, 1);
    let mut b = buggy.init_state();
    for a in ["Crash", "Look", "Sweep", "DecideAtAttempt"] {
        assert!(buggy.fire(a, &mut b), "{a} at {b:?}");
    }
    assert!(!buggy.check_invariant("CrashIsRelaunched", &b), "{b:?}");
    // …and a look at the detection instant relaunches a graceful exit.
    let mut n = buggy.init_state();
    for a in ["Graceful", "LookAtInstant", "OwnRemoval", "Decide"] {
        assert!(buggy.fire(a, &mut n), "{a} at {n:?}");
    }
    assert!(!buggy.check_invariant("GracefulIsLeft", &n), "{n:?}");
    let mut attempt = model.init_state();
    for a in ["Crash", "Look"] {
        assert!(model.fire(a, &mut attempt), "{a} at {attempt:?}");
    }
    for dead in ["DecideAtAttempt", "LookAtInstant"] {
        assert!(
            !model.action_enabled(dead, &attempt),
            "{dead}: never at Buggy=0"
        );
    }
    let mut instant = model.init_state();
    assert!(model.fire("Graceful", &mut instant));
    assert!(!model.action_enabled("LookAtInstant", &instant));
}

/// A CODEX EXIT IS READ ON ITS SHELL'S WORD ALONE (the review of 2026-09-27):
/// someone's signal is never relaunched and a crash is left only when no word
/// came — proven; the first cut's look at the thread lock (which SIGTERM
/// leaves behind as SIGKILL does) relaunches a person's `kill`, and Claude
/// Code's rule inherited (silence a graceful exit) leaves a crash — each
/// caught on a path of its own.
#[test]
fn a_codex_exit_is_decided_on_its_shells_word_alone() {
    let model = harness_codex_exit_witness_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the Codex exit machine must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "harness codex exit witness");
    let buggy = interp::with_buggy(&model, 1);
    for (path, caught) in [
        (&["Signal", "LookAtLock", "Decide"][..], "TheirsIsLeft"),
        (
            &["Crash", "LookAsClaude", "Decide"][..],
            "AToldCrashIsRelaunched",
        ),
    ] {
        let mut st = buggy.init_state();
        for a in path {
            assert!(buggy.fire(a, &mut st), "{a} at {st:?}");
        }
        assert!(!buggy.check_invariant(caught, &st), "{path:?}: {st:?}");
    }
    // The shipped machine: a crash told is relaunched; someone's signal,
    // told, is left; no word at all is left.
    for (path, decided) in [
        (&["Crash", "ShellWord", "Look", "Decide"][..], 1),
        (&["Signal", "ShellWord", "Look", "Decide"][..], 2),
        (&["Crash", "LookGone", "Decide"][..], 2),
    ] {
        let mut st = model.init_state();
        for a in path {
            assert!(model.fire(a, &mut st), "{a} at {st:?}");
        }
        assert_eq!(st["decided"], decided, "{path:?}: {st:?}");
    }
}
