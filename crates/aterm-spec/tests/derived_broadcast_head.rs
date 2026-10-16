// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::broadcast_head_subscription_model, interp, verify};

#[test]
fn a_head_subscriber_never_takes_a_record_already_on_the_broker() {
    let model = broadcast_head_subscription_model();
    verify::prove_and_catch_scalar(&model, "broadcast head subscription");

    // The dangerous schedule: the broker has the record, but the lower-priority say reader is
    // still behind when the topic event is handled.
    let mut correct = model.init_state();
    for action in ["PublishBacklog", "AddHead", "TakeBacklog"] {
        assert!(model.fire(action, &mut correct), "{action}: {correct:?}");
    }
    assert_eq!(correct["cursor"], 1);
    assert_eq!(correct["backlog_delivered"], 0);

    // `Buggy = 1` takes `observed_head=0`: the same schedule delivers the pre-subscription
    // record.
    let old = interp::with_buggy(&model, 1);
    let mut stale = old.init_state();
    for action in ["PublishBacklog", "AddHead", "TakeBacklog"] {
        assert!(old.fire(action, &mut stale), "{action}: {stale:?}");
    }
    assert_eq!(stale["cursor"], 0);
    assert_eq!(stale["backlog_delivered"], 1);
    assert!(!old.check_invariant("HeadSkipsEarlierRecord", &stale));
}
