// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::upgrade_attended_turn_end_model, interp, verify};

/// A PERSON'S GRACE OVER A READY ANSWER ENDS IN THE UPGRADE'S RESTART (the
/// 2026-09-27 20:27 point, s-d3346: `held-back:attended` owning nothing, and
/// `keep going` typed at the grace's lapse over the READY). Proved at `Buggy =
/// 0`, caught at `Buggy = 1` on the live path; the backstop's count caught on
/// its own dials — `Every = 1` (every wait in a row, the review of 2026-09-28)
/// and `Stretch = 1` (not started over at the READY's turn) — each by the
/// bounded check and on its own path; and the backstop for a stamp that fails
/// closed is real: past `Bound` looks at the point the loop may continue, and
/// does so without the invariant reading it as the bug.
#[test]
fn a_person_s_grace_over_a_ready_ends_in_the_restart() {
    let model = upgrade_attended_turn_end_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the attended turn-end machine must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "upgrade attended turn end");

    // The live path, fixed: the READY comes, the look owns the point, the
    // grace lapses, and the loop cannot continue — the host's next look
    // restarts.
    let mut s = model.init_state();
    for action in ["ReadyArrives", "HostLooks", "HostLooks", "GraceLapses"] {
        assert!(model.fire(action, &mut s), "{action} at {s:?}");
    }
    assert!(!model.action_enabled("LoopContinues", &s), "{s:?}");
    assert!(model.fire("HostActs", &mut s));
    assert_eq!((s["point"], s["over"]), (2, 0), "{s:?}");

    // NEGATIVE CONTROL: the host before the fix — the loop takes the lapse.
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

    // The review of 2026-09-28: the notice's answers without READY before it,
    // more of them than the backstop's looks. Fixed, the READY's first look
    // owns the point all the same.
    let before: Vec<&str> = times(bound + 1, "HostWaitsBefore")
        .chain(["ReadyArrives", "HostLooks", "GraceLapses"])
        .collect();
    let mut w = model.init_state();
    for action in &before {
        assert!(model.fire(action, &mut w), "{action} at {w:?}");
    }
    assert_eq!(w["owns"], 1, "{w:?}");
    assert!(!model.action_enabled("LoopContinues", &w), "{w:?}");
    // NEGATIVE CONTROL: the backstop reading every wait in a row owns
    // nothing there, and the loop continues over the READY.
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

    // A person who held the re-ask past the backstop before the READY: its
    // turn starts the looks over, and the READY's point is the upgrade's.
    let held: Vec<&str> = times(bound + 1, "HostHoldsBefore")
        .chain(["ReadyArrives", "HostLooks", "GraceLapses"])
        .collect();
    let mut h = model.init_state();
    for action in &held {
        assert!(model.fire(action, &mut h), "{action} at {h:?}");
    }
    assert!(!model.action_enabled("LoopContinues", &h), "{h:?}");
    // NEGATIVE CONTROL: a count the READY's turn leaves standing.
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

    // The backstop: a person who stays past `Bound` looks at the READY's
    // point gives it back.
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
