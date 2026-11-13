// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Budget, CouldNotRun, SpinScene, await_pet_ready, sole_session};

const READY: &str = "resolved=rainbow-kitty focused=true motion_stage=full \
                    pet_active=true pet_action=sleep pet_deep=true resting=false";

#[test]
fn readiness_wait_observes_deep_sleep_instead_of_assuming_it() {
    for (from, to) in [
        ("resolved=rainbow-kitty", "resolved=comet"),
        ("focused=true", "focused=false"),
        ("motion_stage=full", "motion_stage=reduced"),
        ("pet_active=true", "pet_active=false"),
        ("pet_action=sleep", "pet_action=loaf"),
        ("pet_deep=true", "pet_deep=false"),
        ("pet_deep=true", ""),
        ("resting=false", "resting=true"),
    ] {
        let mut calls = 0;
        await_pet_ready(Budget::from_now(60), SpinScene::PetAsleep, || {
            calls += 1;
            Ok(if calls == 1 {
                READY.replace(from, to)
            } else {
                READY.to_owned()
            })
        })
        .unwrap();
        assert_eq!(calls, 2, "the first reading is not ready: {from} -> {to}");
    }
}

#[test]
fn resting_readiness_needs_rest_and_does_not_retry_a_failed_read() {
    let failure = CouldNotRun::Refused("planted lost reader".to_owned());
    let mut calls = 0;
    let result = await_pet_ready(Budget::from_now(60), SpinScene::Resting, || {
        calls += 1;
        match calls {
            1 => Ok(READY.to_owned()),
            2 => Err(failure.clone()),
            _ => panic!("a failed read must not be re-rolled"),
        }
    });
    assert_eq!(result, Err(failure));
    assert_eq!(calls, 2, "an awake window cannot satisfy the resting row");

    await_pet_ready(Budget::from_now(60), SpinScene::Resting, || {
        Ok(READY.replace("resting=false", "resting=true"))
    })
    .unwrap();
}

#[test]
fn an_expired_readiness_wait_never_samples_an_unready_pet() {
    assert!(
        await_pet_ready(Budget::from_now(0), SpinScene::PetAsleep, || {
            Ok(READY.replace("pet_deep=true", "pet_deep=false"))
        })
        .is_err()
    );
}

#[test]
fn native_raise_selects_only_the_isolated_session() {
    assert_eq!(
        sole_session("123 0 s-a12b - alive test active=1"),
        Some("s-a12b")
    );
    assert_eq!(sole_session("OK 1\nsession s-a12b alive\n"), Some("s-a12b"));
    assert_eq!(sole_session("123 0 s-a12b alive\n123 1 s-c34d alive"), None);
    assert_eq!(sole_session("ERR unavailable"), None);
}
