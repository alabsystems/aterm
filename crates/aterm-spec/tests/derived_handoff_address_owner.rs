// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::handoff_address_owner_model, interp, verify};

/// WHO ANSWERS AN ADOPTED `@<sid>` WHILE A SEAMLESS UPDATE DECIDES (the
/// 2026-09-28 update 0.95 → 0.97: the predecessor refused its own supervisor
/// `ERR ambiguous session id … also served by pid <successor>` in the ~200 ms its
/// per-process successor had published before the Commit). Proved at `Buggy = 0`,
/// caught at `Buggy = 1` (the refusal before the fix) — and the stranger half of
/// the invariant caught by its own dial, `Blanket = 1`, so no half of it is a
/// conjunct nothing can falsify.
#[test]
fn the_predecessor_serves_its_own_candidate_and_refuses_a_stranger() {
    let model = handoff_address_owner_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the handoff address machine must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "handoff-address-owner");

    // THE LIVE PATH: the candidate publishes early, and this process's own
    // supervisor asks for its session — served.
    let mut s = model.init_state();
    for action in ["Launch", "SuccessorPublishes", "Request"] {
        assert!(model.fire(action, &mut s), "{action} at {s:?}");
    }
    assert_eq!((s["asked"], s["refused"]), (2, 0), "{s:?}");
    // …and at the Commit nothing reaches this process any more.
    assert!(model.fire("Commit", &mut s));
    assert!(!model.action_enabled("Request", &s), "{s:?}");

    // A REJECTED attempt: reaped, let go; its dead candidate's entry is no holder.
    let mut r = model.init_state();
    for action in ["Launch", "SuccessorPublishes", "Reject", "Request"] {
        assert!(model.fire(action, &mut r), "{action} at {r:?}");
    }
    assert_eq!((r["registered"], r["refused"]), (0, 0), "{r:?}");

    // A STRANGER publishing the same id mid-update is refused.
    let mut d = model.init_state();
    for action in ["Launch", "StrangerPublishes", "Request"] {
        assert!(model.fire(action, &mut d), "{action} at {d:?}");
    }
    assert_eq!((d["asked"], d["refused"]), (3, 1), "{d:?}");

    // NEGATIVE CONTROL 1 — the rule before the fix refuses the live path.
    let buggy = interp::with_buggy(&model, 1);
    let mut b = buggy.init_state();
    for action in ["Launch", "SuccessorPublishes", "Request"] {
        assert!(buggy.fire(action, &mut b), "{action} at {b:?}");
    }
    assert!(
        !buggy.check_invariant("RefusesExactlyAStranger", &b),
        "the pre-fix refusal of the predecessor's own session: {b:?}"
    );

    // NEGATIVE CONTROL 2 — an exemption keyed on "an attempt decides", not on
    // WHICH pid, serves a stranger; found by the whole-space check too.
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
        interp::bmc(&blanket).is_err(),
        "the blanket exemption must be caught by the bounded check"
    );
}
