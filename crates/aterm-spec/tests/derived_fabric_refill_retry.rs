// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::fabric_refill_retry_model, interp, verify};

#[test]
fn failed_fetch_keeps_old_mail_owed_before_a_group_subscription() {
    let model = fabric_refill_retry_model();
    verify::prove_and_catch_scalar(&model, "Fabric refill debt and delivery order");

    let mut s = model.init_state();
    for action in [
        "Discover",
        "Attach",
        "FetchFails",
        "Attach",
        "RefillCompletes",
        "Subscribe",
        "DeliverNew",
    ] {
        assert!(model.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!(
        (s["owed"], s["old_delivered"], s["new_delivered"]),
        (0, 1, 1)
    );

    let buggy = interp::with_buggy(&model, 1);
    let mut lost = buggy.init_state();
    for action in ["Discover", "Attach", "FetchFails"] {
        assert!(buggy.fire(action, &mut lost));
    }
    assert!(!buggy.check_invariant("RefillDebtSurvivesFailure", &lost));
    assert!(buggy.fire("Attach", &mut lost));
    assert!(buggy.fire("Subscribe", &mut lost));
    assert!(!buggy.check_invariant("OldBeforeLive", &lost));
    assert!(buggy.fire("DeliverNew", &mut lost));
    assert!(!buggy.check_invariant("OldBeforeNew", &lost));

    // The group reader may precede a newly opened tab: a fetch fault after discovery keeps the
    // already-queued new record behind the old one.
    let mut live = model.init_state();
    for action in ["Attach", "Subscribe", "Discover", "FetchFails"] {
        assert!(model.fire(action, &mut live), "{action}: {live:?}");
    }
    assert!(!model.fire("DeliverNew", &mut live));
    for action in ["Attach", "RefillCompletes", "DeliverNew"] {
        assert!(model.fire(action, &mut live), "{action}: {live:?}");
    }
    assert!(model.check_invariant("OldBeforeNew", &live));

    let mut overtaken = buggy.init_state();
    for action in [
        "Attach",
        "Subscribe",
        "Discover",
        "FetchFails",
        "DeliverNew",
    ] {
        assert!(
            buggy.fire(action, &mut overtaken),
            "{action}: {overtaken:?}"
        );
    }
    assert!(!buggy.check_invariant("OldBeforeNew", &overtaken));
}
