// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{after_healthy_check, cadence, github};
use std::time::Duration;

#[test]
fn a_healthy_check_that_found_a_head_in_flight_takes_its_quick_rung() {
    let _guard = super::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let base = Duration::from_secs(cadence::INTERVAL_SECS);
    let mut schedule = cadence::Cadence::new(base);
    let spent = cadence::IN_FLIGHT_CHECKS;
    for (latched, wait) in [
        (Some(("v0.92.0".to_string(), 2)), Duration::from_secs(60)),
        (
            Some(("v0.92.0".to_string(), spent)),
            Duration::from_secs(60),
        ),
        (Some(("v0.92.0".to_string(), spent + 1)), base),
        (None, base),
    ] {
        github::set_head_in_flight(latched.clone());
        after_healthy_check(&mut schedule);
        assert_eq!(schedule.nominal(), wait, "{latched:?}");
    }
}
