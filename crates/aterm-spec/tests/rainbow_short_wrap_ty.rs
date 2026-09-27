// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0: a content follow licenses only its own delayed short park.

use aterm_spec::{derive::rainbow_short_wrap_park_model, verify};

#[test]
fn short_wrap_park_proves_exact_follow_and_catches_both_old_failures() {
    let model = rainbow_short_wrap_park_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|candidate| candidate.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, model.name);

    let held = model.successors("Hold", &model.init_state())[0].clone();
    let other = model.successors("FollowOther", &held)[0].clone();
    assert!(model.action_enabled("CancelSameEnd", &other));
    let unproved = model.successors("FlushLicensed", &other)[0].clone();
    assert_eq!(unproved.get("armed"), Some(&0));

    let exact = model.successors("FollowExact", &held)[0].clone();
    assert!(!model.action_enabled("CancelSameEnd", &exact));
    let denied = model.successors("FlushDenied", &exact)[0].clone();
    assert_eq!(denied.get("armed"), Some(&0));
    let queued = model.successors("FlushLicensed", &exact)[0].clone();
    assert_eq!(queued.get("armed"), Some(&1));
    let replayed = model.successors("Replay", &queued)[0].clone();
    assert_eq!(replayed.get("relayed"), Some(&1));
    assert_eq!(replayed.get("armed"), Some(&0));

    // The old same-end branch cancelled the one-cell wrap. The other old
    // failure let an ordinary one-cell park relay without a content follow.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let held = buggy.successors("Hold", &buggy.init_state())[0].clone();
    let exact = buggy.successors("FollowExact", &held)[0].clone();
    let cancelled = buggy.successors("CancelSameEnd", &exact)[0].clone();
    assert!(!buggy.check_invariant("ProvedParkCannotCancel", &cancelled));
    let unproved = buggy.successors("FlushLicensed", &held)[0].clone();
    assert!(!buggy.check_invariant("OnlyExactFollowArms", &unproved));
    let denied = buggy.successors("FlushDenied", &exact)[0].clone();
    assert!(!buggy.check_invariant("OnlyLicensedFlushArms", &denied));
}
