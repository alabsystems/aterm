// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::atpkg_tag_record_model, interp, verify};

/// Tier-0: the record contract proves at `Buggy=0` and is caught at `Buggy=1` — the
/// never-cleared record of before 2026-09-23 — on both invariants, each by its own path.
#[test]
fn the_tag_record_proves_and_catches_the_never_cleared_record() {
    let model = atpkg_tag_record_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the tag record must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "atpkg tag record");

    // Healthy: an install that could not clear, then a heal that could — no record left,
    // and doctor silent.
    let mut healthy = model.init_state();
    for action in ["StageLeftTagged", "Doctor", "HealClears", "Doctor"] {
        assert!(model.fire(action, &mut healthy), "{action}: {healthy:?}");
    }
    assert_eq!(healthy.get("record"), Some(&0));
    assert_eq!(healthy.get("said"), Some(&1));

    let buggy = interp::with_buggy(&model, 1);
    // The record the heal never cleared.
    let mut stale = buggy.init_state();
    for action in ["StageLeftTagged", "HealClears"] {
        assert!(buggy.fire(action, &mut stale), "{action}: {stale:?}");
    }
    assert!(!buggy.check_invariant("RecordOnlyBesideATag", &stale));
    // …which doctor then reported from the record rather than the files.
    assert!(buggy.fire("Doctor", &mut stale));
    assert!(!buggy.check_invariant("DoctorSaysWhatIsOnDisk", &stale));

    // The record beside a superseded build nothing reaches.
    let mut orphan = buggy.init_state();
    for action in ["StageLeftTagged", "Supersede", "HealPassesBy"] {
        assert!(buggy.fire(action, &mut orphan), "{action}: {orphan:?}");
    }
    assert!(!buggy.check_invariant("RecordOnlyBesideATag", &orphan));
}
