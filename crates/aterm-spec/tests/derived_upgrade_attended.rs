// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::upgrade_attended_turn_end_model, interp, verify};

/// A person's grace over a ready answer ends in the upgrade's restart, not `keep going` typed at
/// the grace's lapse. Proved at `Buggy = 0`, caught at `Buggy = 1` on the live path; the
/// backstop's count is caught on its own dials, `Every = 1` (every wait in a row) and
/// `Stretch = 1` (not started over at the READY's turn); and the backstop for a stamp that fails
/// closed is real: past `Bound` looks at the point, the loop may continue without the invariant
/// reading it as the bug.
#[test]
fn a_person_s_grace_over_a_ready_ends_in_the_restart() {
    let model = upgrade_attended_turn_end_model();
    verify::prove_and_catch_scalar(&model, "upgrade attended turn end");

    // The live path: the READY comes, the look owns the point, the grace lapses, the loop cannot
    // continue, and the host's next look restarts.
    let mut s = model.init_state();
    for action in ["ReadyArrives", "HostLooks", "HostLooks", "GraceLapses"] {
        assert!(model.fire(action, &mut s), "{action} at {s:?}");
    }
    assert!(!model.action_enabled("LoopContinues", &s), "{s:?}");
    assert!(model.fire("HostActs", &mut s));
    assert_eq!((s["point"], s["over"]), (2, 0), "{s:?}");

    // Negative control: the loop takes the lapse.
    let buggy = interp::with_buggy(&model, 1);
    let mut b = buggy.init_state();
    for action in [
        "ReadyArrives",
        "HostLooks",
        "HostLooks",
        "GraceLapses",
        "LoopContinues",
    ] {
        assert!(buggy.fire(action, &mut b), "{action} at {b:?}");
    }
    assert!(
        !buggy.check_invariant("NeverContinueOverAReadyHeldByAPerson", &b),
        "{b:?}"
    );

    let bound = model.consts.iter().find(|c| c.0 == "Bound").unwrap().1;
    let times =
        |n: i64, action: &'static str| std::iter::repeat_n(action, usize::try_from(n).unwrap());

    // More answers without a READY than the backstop's looks: the READY's first look still owns
    // the point.
    let before: Vec<&str> = times(bound + 1, "HostWaitsBefore")
        .chain(["ReadyArrives", "HostLooks", "GraceLapses"])
        .collect();
    let mut w = model.init_state();
    for action in &before {
        assert!(model.fire(action, &mut w), "{action} at {w:?}");
    }
    assert_eq!(w["owns"], 1, "{w:?}");
    assert!(!model.action_enabled("LoopContinues", &w), "{w:?}");
    // Negative control: a backstop counting every wait in a row owns nothing there, and the loop
    // continues over the READY.
    let every = interp::with_consts(&model, &[("Every", 1)]);
    let mut e = every.init_state();
    for action in before.iter().copied().chain(["LoopContinues"]) {
        assert!(every.fire(action, &mut e), "{action} at {e:?}");
    }
    assert!(
        !every.check_invariant("NeverContinueOverAReadyHeldByAPerson", &e),
        "{e:?}"
    );
    assert!(interp::bmc(&every).is_err(), "caught by the bounded check");

    // A person who held the re-ask past the backstop before the READY: its turn starts the looks
    // over, and the READY's point is the upgrade's.
    let held: Vec<&str> = times(bound + 1, "HostHoldsBefore")
        .chain(["ReadyArrives", "HostLooks", "GraceLapses"])
        .collect();
    let mut h = model.init_state();
    for action in &held {
        assert!(model.fire(action, &mut h), "{action} at {h:?}");
    }
    assert!(!model.action_enabled("LoopContinues", &h), "{h:?}");
    // Negative control: a count the READY's turn leaves standing.
    let stretch = interp::with_consts(&model, &[("Stretch", 1)]);
    let mut t = stretch.init_state();
    for action in held.iter().copied().chain(["LoopContinues"]) {
        assert!(stretch.fire(action, &mut t), "{action} at {t:?}");
    }
    assert!(
        !stretch.check_invariant("NeverContinueOverAReadyHeldByAPerson", &t),
        "{t:?}"
    );
    assert!(
        interp::bmc(&stretch).is_err(),
        "caught by the bounded check"
    );

    // The backstop: a person who stays past `Bound` looks at the READY's point gives it back.
    let mut long = model.init_state();
    assert!(model.fire("ReadyArrives", &mut long));
    for _ in 0..bound + 2 {
        assert!(model.fire("HostLooks", &mut long), "{long:?}");
    }
    assert_eq!(long["owns"], 0, "past the bound: {long:?}");
    assert!(model.fire("GraceLapses", &mut long));
    assert!(model.fire("LoopContinues", &mut long), "{long:?}");
    assert_eq!(
        long["over"], 0,
        "the backstop is no continuation over the READY"
    );

    // A person who touches the tab again after the lapse holds it again.
    let mut again = model.init_state();
    for action in ["ReadyArrives", "HostLooks", "GraceLapses", "PersonTouches"] {
        assert!(model.fire(action, &mut again), "{action} at {again:?}");
    }
    assert!(!model.action_enabled("LoopContinues", &again));
    assert!(!model.action_enabled("HostActs", &again));
}
