// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::upgrade_settle_turn_end_model, interp, verify};

/// The point the upgrade acts at next is owned through its settle: a READY after notices typed
/// at breaks owns its first settle look, so the loop's `keep going` cannot void it. Proved at
/// `Buggy = 0`, caught at `Buggy = 1`; each dial is caught alone by the bounded check and on its
/// own path: `Carry = 1` (a notice at a break leaves the count) and `Every = 1` (every wait
/// counts). Past the settle's `Bound` looks the loop may continue without the invariant reading
/// it as the bug.
#[test]
fn a_ready_after_notices_at_breaks_owns_its_settle() {
    let model = upgrade_settle_turn_end_model();
    verify::prove_and_catch_scalar(&model, "upgrade settle turn end");

    let walk = |m: &aterm_spec::derive::Model, path: &[&str]| {
        let mut s = m.init_state();
        for action in path {
            assert!(m.fire(action, &mut s), "{action} at {s:?}");
        }
        s
    };
    // A person's looks, notices at breaks, the READY, its first settle look.
    let incident = [
        "OtherWait",
        "OtherWait",
        "NoticeAtBreak",
        "NoticeAtBreak",
        "ReadyArrives",
        "HostSettles",
    ];
    let s = walk(&model, &incident);
    assert_eq!(s["owns"], 1, "the READY's first look owns the point: {s:?}");
    assert!(!model.action_enabled("LoopContinues", &s), "{s:?}");

    // Negative control: the loop takes the READY.
    let buggy = interp::with_buggy(&model, 1);
    let mut b = walk(&buggy, &incident);
    assert_eq!(b["owns"], 0, "{b:?}");
    assert!(buggy.fire("LoopContinues", &mut b));
    assert!(
        !buggy.check_invariant("NeverContinueOverASettlingReady", &b),
        "{b:?}"
    );

    // Each dial alone, on its own path.
    for (dial, path) in [
        (
            "Carry",
            &[
                "SettleBefore",
                "SettleBefore",
                "NoticeAtBreak",
                "ReadyArrives",
                "HostSettles",
            ][..],
        ),
        (
            "Every",
            &[
                "NoticeAtIdle",
                "OtherWait",
                "OtherWait",
                "ReadyArrives",
                "HostSettles",
            ][..],
        ),
    ] {
        let fixed = walk(&model, path);
        assert_eq!(fixed["owns"], 1, "{dial}: fixed owns: {fixed:?}");
        let wrong = interp::with_consts(&model, &[(dial, 1)]);
        let mut w = walk(&wrong, path);
        assert_eq!(w["owns"], 0, "{dial}: {w:?}");
        assert!(wrong.fire("LoopContinues", &mut w), "{dial}");
        assert!(
            !wrong.check_invariant("NeverContinueOverASettlingReady", &w),
            "{dial}: {w:?}"
        );
        assert!(
            interp::bmc(&wrong).is_err(),
            "{dial}: caught by the bounded check"
        );
    }

    // The bound: past the settle's looks the point is the loop's again.
    let bound = model.consts.iter().find(|c| c.0 == "Bound").unwrap().1;
    let mut long = walk(&model, &["NoticeAtIdle", "ReadyArrives"]);
    for _ in 0..=bound {
        assert!(model.fire("HostSettles", &mut long), "{long:?}");
    }
    assert_eq!(long["owns"], 0, "past the bound: {long:?}");
    assert!(model.fire("LoopContinues", &mut long));
    assert_eq!(
        long["over"], 0,
        "the bound is no continuation over the READY"
    );
}

/// A notice its own fence refused owns its point, bounded: `Bound` refused looks own the point,
/// the next owns nothing, and no turn of the agent's renews it (the `Renew` dial, a count per
/// point, owns the next point again). The count is the refused looks' own and the owner's word
/// starts it over (`Shared` and `Stale` caught on their paths). Proved at `Buggy = 0`, caught at
/// `Buggy = 1` (a refused notice owns nothing).
#[test]
fn a_refused_notice_owns_its_point_for_its_looks_and_no_turn_renews_them() {
    use aterm_spec::derive::upgrade_refused_turn_end_model;
    let model = upgrade_refused_turn_end_model();
    verify::prove_and_catch_scalar(&model, "upgrade refused notice turn end");
    let bound = model.consts.iter().find(|c| c.0 == "Bound").unwrap().1;

    // The refused look owns the point and the loop cannot take it; the notice is typed at the
    // next look.
    let mut s = model.init_state();
    assert!(model.fire("Refused", &mut s));
    assert_eq!(s["owns"], 1, "{s:?}");
    assert!(!model.action_enabled("LoopContinues", &s), "{s:?}");
    assert!(model.fire("Types", &mut s));
    assert_eq!(
        (s["due"], s["count"]),
        (0, 0),
        "an act starts the count over"
    );

    // Negative control: the loop takes it.
    let buggy = interp::with_buggy(&model, 1);
    let mut b = buggy.init_state();
    assert!(buggy.fire("Refused", &mut b));
    assert!(buggy.fire("LoopContinues", &mut b));
    assert!(
        !buggy.check_invariant("NeverContinueOverARefusedNoticeInItsLooks", &b),
        "{b:?}"
    );

    // The bound: `Bound` refused looks own it, the next does not, and the loop may continue
    // without the invariant reading it as the bug.
    let mut long = model.init_state();
    for n in 0..bound {
        assert!(model.fire("Refused", &mut long), "{n}");
        assert_eq!(long["owns"], 1, "{n}: {long:?}");
    }
    assert!(model.fire("Refused", &mut long));
    assert_eq!(long["owns"], 0, "past the bound: {long:?}");
    assert!(model.fire("LoopContinues", &mut long));
    assert_eq!(long["over"], 0, "{long:?}");
    // A turn of the agent's renews nothing: the next point's refused look owns nothing.
    assert!(model.fire("PointComes", &mut long));
    assert!(model.fire("Refused", &mut long));
    assert_eq!(long["owns"], 0, "no turn renews the bound: {long:?}");
    // Negative control: a count per point owns every point anew.
    let renew = interp::with_consts(&model, &[("Renew", 1)]);
    let mut r = renew.init_state();
    for _ in 0..=bound {
        assert!(renew.fire("Refused", &mut r));
    }
    assert!(renew.fire("LoopContinues", &mut r));
    assert!(renew.fire("PointComes", &mut r));
    assert!(renew.fire("Refused", &mut r));
    assert_eq!(
        r["owns"], 1,
        "the Renew dial owns the next point again: {r:?}"
    );

    // The refused looks are a count of their own, and the owner's word starts it over: settles
    // of the notice's gate before the refusals leave it alone, and the owner's word after the
    // bound owns the next refused look again. Negative controls, each on its path and by the
    // bounded check: one shared count (`Shared`), a word that leaves it standing (`Stale`).
    let walk = |m: &aterm_spec::derive::Model, path: &[&str]| {
        let mut s = m.init_state();
        for action in path {
            assert!(m.fire(action, &mut s), "{action} at {s:?}");
        }
        s
    };
    let settled: Vec<&str> = std::iter::repeat_n("SettleBefore", 3)
        .chain(["Refused"])
        .collect();
    let mut worded: Vec<&str> = std::iter::repeat_n("Refused", 4).collect();
    worded.extend(["OwnerWord", "Refused"]);
    for (dial, path) in [("Shared", &settled), ("Stale", &worded)] {
        let fixed = walk(&model, path);
        assert_eq!(
            fixed["owns"], 1,
            "{dial}: the fixed host owns it: {fixed:?}"
        );
        assert!(!model.action_enabled("LoopContinues", &fixed), "{dial}");
        let wrong = interp::with_consts(&model, &[(dial, 1)]);
        let mut w = walk(&wrong, path);
        assert_eq!(w["owns"], 0, "{dial}: {w:?}");
        assert!(wrong.fire("LoopContinues", &mut w), "{dial}");
        assert!(
            !wrong.check_invariant("NeverContinueOverARefusedNoticeInItsLooks", &w),
            "{dial}: {w:?}"
        );
        assert!(
            interp::bmc(&wrong).is_err(),
            "{dial}: caught by the bounded check"
        );
    }
}

/// The end of the agent's own work is the upgrade's point: when work the host saw ends, the host
/// asks and the loop's `keep going` cannot take the idle point. Proved at `Buggy = 0`, caught at
/// `Buggy = 1`; `Nag = 1` (an ask with no work seen since the notice) and `Hold = 1` (the restart
/// gate's wait on a status lagging the idle screen, held past the re-ask's window) are each
/// caught alone by the bounded check and on its own path.
#[test]
fn the_end_of_the_agents_own_work_is_the_upgrades_point() {
    use aterm_spec::derive::harness_upgrade_work_end_model;
    let model = harness_upgrade_work_end_model();
    verify::prove_and_catch_scalar(&model, "upgrade: the end of the agent's work");
    let walk = |m: &aterm_spec::derive::Model, path: &[&str]| {
        let mut s = m.init_state();
        for action in path {
            assert!(m.fire(action, &mut s), "{action} at {s:?}");
        }
        s
    };
    // Asked, work runs, gave up, work ends: the host asks (a rest re-armed), and the loop cannot
    // take the point.
    let s = walk(&model, &["WorkRuns", "GivesUp", "WorkEnds", "HostAsks"]);
    assert_eq!((s["phase"], s["asks"], s["owns"]), (1, 1, 1), "{s:?}");
    assert!(!model.action_enabled("LoopContinues", &s), "{s:?}");
    // Negative control: a host that waits there lets the loop take it.
    let buggy = interp::with_buggy(&model, 1);
    let mut b = walk(&buggy, &["WorkRuns", "GivesUp", "WorkEnds", "HostWaits"]);
    assert!(buggy.fire("LoopContinues", &mut b));
    assert!(
        !buggy.check_invariant("TheEndOfItsWorkIsTheUpgrades", &b),
        "{b:?}"
    );
    // No work seen: the quiet word, as ever, and the loop's point.
    let s = walk(&model, &["HostWaits", "LoopContinues"]);
    assert_eq!(s["handed"], 0, "{s:?}");
    assert!(!model.action_enabled("HostAsks", &model.init_state()));
    // Negative control: an ask with no work seen.
    let nag = interp::with_consts(&model, &[("Nag", 1)]);
    let n = walk(&nag, &["HostAsks"]);
    assert!(!nag.check_invariant("AnAskFollowsWorkItSaw", &n), "{n:?}");
    assert!(interp::bmc(&nag).is_err(), "caught by the bounded check");

    // A status that lags the idle screen: the restart's gate waits there and holds the point
    // inside the re-ask's window; past it the re-ask's line goes through the lag, and with the
    // asks spent the round gives up, never waiting again.
    let lag = ["WorkRuns", "WorkEnds", "StatusLags"];
    let s = walk(&model, &lag);
    assert!(!model.action_enabled("HostAsks", &s), "{s:?}");
    let held = walk(&model, &[&lag[..], &["HostHolds"]].concat());
    assert_eq!(held["owns"], 1, "the gate's wait owns the point: {held:?}");
    let past = walk(&model, &[&lag[..], &["HostHolds", "WindowPasses"]].concat());
    assert!(!model.action_enabled("HostHolds", &past), "{past:?}");
    let asked = walk(
        &model,
        &[&lag[..], &["HostHolds", "WindowPasses", "HostReasks"]].concat(),
    );
    assert_eq!((asked["asks"], asked["worked"]), (2, 0), "{asked:?}");
    let spent = walk(
        &model,
        &[
            &lag[..],
            &["WindowPasses", "HostReasks", "WindowPasses", "HostReasks"],
            &["WindowPasses", "HostGivesUp"],
        ]
        .concat(),
    );
    assert_eq!(spent["phase"], 2, "given up: {spent:?}");
    // Negative control: the wait held past the window, where no re-ask comes.
    let hold = interp::with_consts(&model, &[("Hold", 1)]);
    let late = walk(&hold, &[&lag[..], &["WindowPasses"]].concat());
    assert!(!hold.action_enabled("HostReasks", &late), "{late:?}");
    assert!(!hold.action_enabled("HostGivesUp", &late), "{late:?}");
    let h = walk(&hold, &[&lag[..], &["WindowPasses", "HostHolds"]].concat());
    assert!(
        !hold.check_invariant("NoWaitOutlivesTheReaskWindow", &h),
        "{h:?}"
    );
    assert!(interp::bmc(&hold).is_err(), "caught by the bounded check");
}
