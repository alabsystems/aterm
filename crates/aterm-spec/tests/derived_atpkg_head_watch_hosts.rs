// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::atpkg_head_watch_hosts_model, interp, verify};

/// Who watches the vendor heads (gap #28) proves over the whole bounded space — a window
/// and a session never both, one session at most, no seat outliving its holder — and
/// catches the three designs the rendezvous replaced.
#[test]
fn a_window_or_one_session_watches_never_both() {
    let model = atpkg_head_watch_hosts_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the host rendezvous must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "atpkg head watch hosts");

    // The healthy path: a session watches, a window opening mid-round is refused, takes
    // the watch after the round, and the session stands by until the window closes.
    let mut s = model.init_state();
    for action in ["S1Starts", "S1Admits"] {
        assert!(model.fire(action, &mut s), "{action}");
    }
    assert_eq!(s["s1"], 3, "the session rounds: {s:?}");
    for action in ["WindowOpens", "WindowAdmits"] {
        assert!(model.fire(action, &mut s), "{action}");
    }
    assert_eq!(s["w"], 1, "refused mid-round: {s:?}");
    for action in ["S1EndsRound", "WindowAdmits", "S1Admits"] {
        assert!(model.fire(action, &mut s), "{action}");
    }
    assert_eq!(
        (s["w"], s["s1"]),
        (2, 2),
        "the window watches, the session stands by"
    );
    for action in ["WindowCloses", "S1Admits"] {
        assert!(model.fire(action, &mut s), "{action}");
    }
    assert_eq!(s["s1"], 3, "the session watches again");
    // A second session stands by at the seat, and takes it when the first exits.
    for action in ["S1EndsRound", "S2Starts", "S2Admits"] {
        assert!(model.fire(action, &mut s), "{action}");
    }
    assert_eq!(s["s2"], 1);
    for action in ["S1Exits", "S2Admits"] {
        assert!(model.fire(action, &mut s), "{action}");
    }
    assert_eq!(s["s2"], 3);

    // NEGATIVE CONTROLS, one per claim, under the replaced designs.
    let buggy = interp::with_buggy(&model, 1);
    let mut probed = buggy.init_state();
    for action in [
        "S1Starts",
        "S1Admits",
        "S1EndsRound",
        "WindowOpens",
        "WindowAdmits",
        "S1RoundsOnStaleProbe",
    ] {
        assert!(buggy.fire(action, &mut probed), "{action}");
    }
    assert!(!buggy.check_invariant("NeverBothKinds", &probed));
    let mut seatless = buggy.init_state();
    for action in [
        "S1Starts",
        "S1Admits",
        "S1EndsRound",
        "S2Starts",
        "S2RoundsWithoutSeat",
    ] {
        assert!(buggy.fire(action, &mut seatless), "{action}");
    }
    assert!(!buggy.check_invariant("OneSessionWatcher", &seatless));
    let mut kept = buggy.init_state();
    for action in ["S1Starts", "S1Admits", "S1ExitsKeepingSeat"] {
        assert!(buggy.fire(action, &mut kept), "{action}");
    }
    assert!(!buggy.check_invariant("NoOrphanedSeat", &kept));
}
