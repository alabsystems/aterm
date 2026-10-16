// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::handoff_address_owner_model, interp, verify};

/// Who answers an adopted `@<sid>` while a seamless update decides: a successor that publishes
/// before the Commit makes the predecessor refuse its own supervisor (`ERR ambiguous session
/// id … also served by pid <successor>`). Proved at `Buggy = 0` and caught at `Buggy = 1`; the
/// order's half is caught by its own dial, `Early = 1`, with the exemption then proved alone,
/// and the exemption's stranger half is caught by `Blanket = 1`.
#[test]
fn the_predecessor_serves_its_own_candidate_and_refuses_a_stranger() {
    let model = handoff_address_owner_model();
    verify::prove_and_catch_scalar(&model, "handoff-address-owner");

    // The candidate is launched and its entry waits for the Commit: this process's own
    // supervisor meets its own entry, served.
    let mut s = model.init_state();
    assert!(model.fire("Launch", &mut s));
    assert!(
        !model.action_enabled("SuccessorPublishes", &s),
        "no entry names the candidate while this process owns the session: {s:?}"
    );
    assert!(model.fire("Request", &mut s));
    assert_eq!((s["asked"], s["refused"]), (1, 0), "{s:?}");
    // …the Commit, and only then the candidate's entry; nothing reaches this process any more.
    assert!(model.fire("Commit", &mut s));
    assert!(
        model.fire("SuccessorPublishes", &mut s),
        "published at the Commit"
    );
    assert!(!model.action_enabled("Request", &s), "{s:?}");

    // A rejected attempt is reaped and let go; it never named the candidate.
    let mut r = model.init_state();
    for action in ["Launch", "Reject", "Request"] {
        assert!(model.fire(action, &mut r), "{action} at {r:?}");
    }
    assert_eq!(
        (r["entry"], r["registered"], r["refused"]),
        (0, 0, 0),
        "{r:?}"
    );
    assert!(!model.action_enabled("SuccessorPublishes", &r), "{r:?}");

    // A stranger publishing the same id mid-update is refused.
    let mut d = model.init_state();
    for action in ["Launch", "StrangerPublishes", "Request"] {
        assert!(model.fire(action, &mut d), "{action} at {d:?}");
    }
    assert_eq!((d["asked"], d["refused"]), (3, 1), "{d:?}");

    // `Buggy = 1`: the candidate is named while this process owns the session, and its own
    // session is refused.
    let buggy = interp::with_buggy(&model, 1);
    let mut b = buggy.init_state();
    for action in ["Launch", "SuccessorPublishes"] {
        assert!(buggy.fire(action, &mut b), "{action} at {b:?}");
    }
    assert!(
        !buggy.check_invariant("CandidateNamedOnlyAfterCommit", &b),
        "the boot-time publication names the candidate early: {b:?}"
    );
    assert!(buggy.fire("Request", &mut b));
    assert!(
        !buggy.check_invariant("RefusesExactlyAStranger", &b),
        "the pre-fix refusal of the predecessor's own session: {b:?}"
    );

    // `Early = 1`, the boot-time publication with the exemption alone: the order's invariant
    // is caught…
    let early = interp::with_consts(&model, &[("Early", 1)]);
    assert!(
        matches!(
            interp::bmc(&early),
            Err((_, "CandidateNamedOnlyAfterCommit"))
        ),
        "the boot-time publication must be caught by the bounded check"
    );
    // …and the exemption, checked on its own, still serves this process's own supervisor on
    // that path and refuses only a stranger.
    let mut exemption_alone = early.clone();
    exemption_alone
        .invariants
        .retain(|inv| inv.name != "CandidateNamedOnlyAfterCommit");
    assert!(
        interp::bmc(&exemption_alone).is_ok(),
        "the exemption alone keeps RefusesExactlyAStranger"
    );
    let mut e = early.init_state();
    for action in ["Launch", "SuccessorPublishes", "Request"] {
        assert!(early.fire(action, &mut e), "{action} at {e:?}");
    }
    assert_eq!((e["asked"], e["refused"]), (2, 0), "{e:?}");

    // `Blanket = 1`: an exemption keyed on "an attempt decides", not on which pid, serves a
    // stranger; the whole-space check finds it too.
    let blanket = interp::with_consts(&model, &[("Blanket", 1)]);
    let mut x = blanket.init_state();
    for action in ["Launch", "StrangerPublishes", "Request"] {
        assert!(blanket.fire(action, &mut x), "{action} at {x:?}");
    }
    assert!(
        !blanket.check_invariant("RefusesExactlyAStranger", &x),
        "{x:?}"
    );
    assert!(
        matches!(interp::bmc(&blanket), Err((_, "RefusesExactlyAStranger"))),
        "the blanket exemption must be caught by the bounded check"
    );
}
