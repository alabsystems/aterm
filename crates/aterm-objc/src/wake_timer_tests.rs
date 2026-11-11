// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_spent_timer_is_re_armed_for_an_unchanged_request() {
    let t0 = Instant::now();
    let mut plan = WakePlan::new();
    assert_eq!(plan.start(), WakeArm::Now);
    assert_eq!(
        plan.start(),
        WakeArm::Keep,
        "armed and pending: nothing to do"
    );
    plan.fired();
    assert_eq!(
        plan.start(),
        WakeArm::Now,
        "spent: the same request re-arms"
    );
    let later = t0 + Duration::from_secs(5);
    assert_eq!(plan.start_at(t0, Some(later)), WakeArm::At(later));
    assert_eq!(plan.start_at(t0, Some(later)), WakeArm::Keep);
    plan.fired();
    assert_eq!(plan.start_at(t0, Some(later)), WakeArm::At(later));
    assert_eq!(
        plan.start_at(later, Some(later)),
        WakeArm::Now,
        "due is now"
    );
    assert_eq!(plan.start_at(later, None), WakeArm::Disarm);
    assert_eq!(plan.stop(), WakeArm::Keep);
    plan.fired();
    assert!(!plan.spent(), "a disarmed timer has nothing to spend");
}
