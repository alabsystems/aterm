// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for the package lane's read-only store-lock release wake.

use crate::{
    BumpWatch, CONTENTION_WEDGE_PARK, LiveSwitch, LoopGate, PARK_SLICE, ParkEnd, ParkProbes,
    PassWhy, park_slice, sleep_interval_watching_bump, store_release_park_end,
};
use aterm_spec::derive::{Model, atpkg_contention_release_park_model};
use aterm_spec::interp::State;
use aterm_spec::verify::validate_transition_tiered;
use std::time::{Duration, SystemTime};

fn assert_decision(model: &Model, before: &State, actual: &Option<ParkEnd>, label: &str) {
    let decision = match actual {
        None => 0,
        Some(ParkEnd::Bumped) => 1,
        Some(ParkEnd::VendorMoved(_)) => 2,
        Some(ParkEnd::StoreUnlocked) => 3,
        other => panic!("{label}: unexpected contention outcome {other:?}"),
    };
    let mut after = before.clone();
    after.insert("checked", 1);
    after.insert("decision", decision);
    assert!(model.action_enabled("Check", before), "{label}: guard");
    let (conforms, evidence) =
        validate_transition_tiered(model, &[], before, &after, Some("Check"), label);
    assert!(conforms, "{label}: {evidence}");
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, &after),
            "{label}: {}",
            invariant.name
        );
    }
}

/// The real atpkg flock and GUI park choice refine every relevant model input.
/// Held, released, disabled, bump, and vendor rows include wake and no-wake
/// outcomes. Two fabricated wrong results reject both the early-pass and
/// extra-hour failure modes.
#[test]
fn real_store_release_wakes_contention_without_stealing_priority() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let layout = atpkg::store::Layout {
        prefix: dir.path().join("pkg"),
    };
    let model = atpkg_contention_release_park_model();
    let holder = atpkg::lock::try_lock_store(&layout).unwrap();

    let held = model.init_state();
    let still_held = store_release_park_end(&layout, true, None);
    assert_eq!(still_held, None, "a live writer cannot wake a pass");
    assert_decision(&model, &held, &still_held, "held writer");

    let mut bump = model.init_state();
    assert!(model.fire("OfferBump", &mut bump));
    let bumped = store_release_park_end(&layout, true, Some(ParkEnd::Bumped));
    assert_eq!(bumped, Some(ParkEnd::Bumped));
    assert_decision(&model, &bump, &bumped, "bump before release check");

    let mut vendor = model.init_state();
    assert!(model.fire("OfferVendor", &mut vendor));
    let moved = store_release_park_end(&layout, true, Some(ParkEnd::VendorMoved(vec!["claude"])));
    assert_eq!(moved, Some(ParkEnd::VendorMoved(vec!["claude"])));
    assert_decision(&model, &vendor, &moved, "vendor before release check");

    drop(holder);
    let mut released = model.init_state();
    assert!(model.fire("Release", &mut released));
    let wake = store_release_park_end(&layout, true, None);
    assert_eq!(wake, Some(ParkEnd::StoreUnlocked));
    assert_decision(&model, &released, &wake, "first slice after release");
    assert_eq!(
        wake.unwrap().pass_why(PassWhy::Published(45)),
        PassWhy::Published(45),
        "the same target reaches the existing durable dedup gate"
    );

    let mut disabled = released.clone();
    assert!(model.fire("Disable", &mut disabled));
    let no_probe = store_release_park_end(&layout, false, None);
    assert_eq!(no_probe, None, "healthy parks do not ask the store lock");
    assert_decision(&model, &disabled, &no_probe, "probe disabled");

    let mut early = held.clone();
    early.insert("checked", 1);
    early.insert("decision", 3);
    assert!(
        !model.successors("Check", &held).contains(&early),
        "negative control: retry while still held is rejected"
    );
    let mut late = released.clone();
    late.insert("checked", 1);
    late.insert("decision", 0);
    assert!(
        !model.successors("Check", &released).contains(&late),
        "negative control: an extra hour after release is rejected"
    );
    let now = SystemTime::now();
    assert_eq!(PARK_SLICE, Duration::from_secs(5));
    assert_eq!(
        park_slice(now + CONTENTION_WEDGE_PARK, now, CONTENTION_WEDGE_PARK),
        Some(PARK_SLICE),
        "the real wedge park checks release in five-second slices"
    );

    // The actual sleep loop reaches the local probe even when no vendor watch
    // exists. A short deadline keeps this integration check fast; PARK_SLICE
    // above pins the production latency bound.
    let short = Duration::from_millis(20);
    let holder = atpkg::lock::try_lock_store(&layout).unwrap();
    let while_held = sleep_interval_watching_bump(
        Some(&layout),
        &mut (SystemTime::now() + short),
        short,
        &mut BumpWatch::default(),
        ParkProbes::STORE_RELEASE,
        None,
        &mut LiveSwitch::new(None, LoopGate::On),
    );
    assert_eq!(
        while_held,
        ParkEnd::Elapsed,
        "held until the short deadline"
    );
    drop(holder);
    let after_release = sleep_interval_watching_bump(
        Some(&layout),
        &mut (SystemTime::now() + short),
        short,
        &mut BumpWatch::default(),
        ParkProbes::STORE_RELEASE,
        None,
        &mut LiveSwitch::new(None, LoopGate::On),
    );
    assert_eq!(
        after_release,
        ParkEnd::StoreUnlocked,
        "the production loop does not skip a release check without vendor heads"
    );
}
