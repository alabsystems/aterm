// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::atpkg_index_pending_park_model, interp, verify};

#[test]
fn one_index_probe_can_overlap_a_vendor_hint_without_losing_ready_priority() {
    let model = atpkg_index_pending_park_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, "atpkg index pending park");
    assert_eq!(
        verify::audit_dead_negative_controls(&model, &[]),
        Ok(0),
        "a rejected extra-start attempt is live at the committed dial"
    );

    let mut vendor_first = model.init_state();
    for action in [
        "Start",
        "VendorReady",
        "ChooseVendor",
        "FinishPublished",
        "NextPark",
        "Harvest",
        "ChooseIndex",
    ] {
        assert!(
            model.fire(action, &mut vendor_first),
            "{action}: {vendor_first:?}"
        );
    }
    assert_eq!(vendor_first["chosen"], 3);
    assert!(model.fire("Rearm", &mut vendor_first));
    assert!(model.fire("NextPark", &mut vendor_first));
    assert!(model.fire("Start", &mut vendor_first));
    assert_eq!(vendor_first["phase"], 1, "the next minute may probe again");

    let mut index_first = model.init_state();
    for action in [
        "Start",
        "FinishPublished",
        "Harvest",
        "VendorReady",
        "ChooseIndex",
    ] {
        assert!(
            model.fire(action, &mut index_first),
            "{action}: {index_first:?}"
        );
    }
    assert!(!model.action_enabled("ChooseVendor", &index_first));
    assert!(model.fire("NextPark", &mut index_first));
    assert_eq!(
        index_first["vendor"], 1,
        "ready vendor survives index priority"
    );
    assert!(model.fire("ChooseVendor", &mut index_first));
    assert!(model.fire("NextPark", &mut index_first));
    assert_eq!(index_first["vendor"], 0, "delivered vendor is retired");

    let mut near_first = model.init_state();
    for action in ["Start", "NearPublished", "ChooseIndex"] {
        assert!(
            model.fire(action, &mut near_first),
            "{action}: {near_first:?}"
        );
    }
    assert_eq!(near_first["phase"], 1, "the worker remains owned");
    assert!(model.check_invariant("NoStaleNearChosen", &near_first));

    // The first near hint is covered by the pass; a higher near HEAD from the
    // same owned worker is fresh even though its original generation is old.
    let mut newer_near = model.init_state();
    for action in [
        "Start",
        "NearPublished",
        "ChooseIndex",
        "NextPark",
        "FullPass",
    ] {
        assert!(
            model.fire(action, &mut newer_near),
            "{action}: {newer_near:?}"
        );
    }
    assert!(!model.action_enabled("ChooseIndex", &newer_near));
    for action in ["NearAfterPass", "ChooseIndex"] {
        assert!(
            model.fire(action, &mut newer_near),
            "{action}: {newer_near:?}"
        );
    }
    assert_eq!(newer_near["chosen"], 3);
    assert!(model.check_invariant("NoStaleNearChosen", &newer_near));

    // The worker may finish before the lane harvests it. Joining its stale
    // final answer still leaves the later near hint available exactly once.
    let mut finished_near = model.init_state();
    for action in [
        "Start",
        "NearPublished",
        "ChooseIndex",
        "NextPark",
        "FullPass",
        "FinishPublished",
        "NearAfterPass",
        "Harvest",
        "ChooseIndex",
    ] {
        assert!(
            model.fire(action, &mut finished_near),
            "{action}: {finished_near:?}"
        );
    }
    assert_eq!(finished_near["chosen"], 3);
    assert!(model.check_invariant("NoStaleAnswerApplied", &finished_near));

    let mut full_first = model.init_state();
    for action in [
        "VendorReady",
        "FullDue",
        "ChooseFull",
        "NextPark",
        "ChooseVendor",
    ] {
        assert!(
            model.fire(action, &mut full_first),
            "{action}: {full_first:?}"
        );
    }
    assert_eq!(full_first["chosen"], 4);

    let mut rejected_extra = model.init_state();
    assert!(model.fire("Start", &mut rejected_extra));
    assert!(model.fire("SpawnExtra", &mut rejected_extra));
    assert_eq!(
        rejected_extra["phase"], 1,
        "the owned worker remains the only one"
    );
    assert_eq!(rejected_extra["extra_spawn_attempts"], 1);
    assert!(!model.action_enabled("SpawnExtra", &rejected_extra));
    assert!(model.check_invariant("OneOwnedIndexWorker", &rejected_extra));

    let buggy = interp::with_buggy(&model, 1);
    let mut extra = buggy.init_state();
    assert!(buggy.fire("Start", &mut extra));
    assert!(buggy.fire("SpawnExtra", &mut extra));
    assert_eq!(extra["extra_spawn_attempts"], 1);
    assert!(!buggy.check_invariant("OneOwnedIndexWorker", &extra));

    let mut wrong_priority = buggy.init_state();
    for action in [
        "Start",
        "FinishPublished",
        "Harvest",
        "VendorReady",
        "ChooseVendor",
    ] {
        assert!(buggy.fire(action, &mut wrong_priority), "{action}");
    }
    assert!(!buggy.check_invariant("ReadyIndexBeforeVendor", &wrong_priority));

    let mut wrong_stale_near = buggy.init_state();
    for action in ["Start", "NearPublished", "FullPass", "ChooseIndex"] {
        assert!(
            buggy.fire(action, &mut wrong_stale_near),
            "{action}: {wrong_stale_near:?}"
        );
    }
    assert!(!buggy.check_invariant("NoStaleNearChosen", &wrong_stale_near));

    let mut lost = buggy.init_state();
    for action in ["VendorReady", "FullDue", "ChooseFull", "NextPark"] {
        assert!(buggy.fire(action, &mut lost), "{action}: {lost:?}");
    }
    assert!(!buggy.check_invariant("ReadyVendorSurvivesPreemption", &lost));

    let mut stale_missing = model.init_state();
    for action in ["Start", "FullPass", "FinishMissing", "Harvest"] {
        assert!(model.fire(action, &mut stale_missing), "{action}");
    }
    assert_eq!(stale_missing["unlanded"], 1);
    assert_eq!(stale_missing["published"], 0);
    assert!(model.check_invariant("NoStaleAnswerApplied", &stale_missing));
    let mut stale_published = model.init_state();
    for action in ["Start", "FinishPublished", "FullPass", "Harvest"] {
        assert!(model.fire(action, &mut stale_published), "{action}");
    }
    assert_eq!(stale_published["unlanded"], 1);
    assert_eq!(stale_published["published"], 0);

    let mut wrong_stale = buggy.init_state();
    for action in ["Start", "FullPass", "FinishMissing", "Harvest"] {
        assert!(buggy.fire(action, &mut wrong_stale), "{action}");
    }
    assert_eq!(wrong_stale["unlanded"], 0);
    assert!(!buggy.check_invariant("NoStaleAnswerApplied", &wrong_stale));

    let mut no_inline = model.init_state();
    assert!(model.fire("Start", &mut no_inline));
    assert!(model.fire("VendorSpawnFailed", &mut no_inline));
    assert_eq!(no_inline["vendor_retry"], 1);
    assert_eq!(no_inline["inline_vendor_get"], 0);
    let mut wrong_inline = buggy.init_state();
    assert!(buggy.fire("Start", &mut wrong_inline));
    assert!(buggy.fire("VendorSpawnFailed", &mut wrong_inline));
    assert!(!buggy.check_invariant("NoInlineVendorGetOnHelperFailure", &wrong_inline));
}
