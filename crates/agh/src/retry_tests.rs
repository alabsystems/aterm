// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;

#[test]
fn a_custom_policy_accounts_for_retry_after_in_every_slot() {
    let policy = RetryPolicy {
        attempts: 3,
        pauses: [Duration::from_secs(1), Duration::from_secs(2)],
        max_secondary_wait: Duration::from_secs(5),
    };
    assert_eq!(policy.bound(), "3 attempts / 10s total retry-pause bound");
    let cause = Cause::RateLimited {
        retry_after: Duration::from_secs(5),
    };
    let mut budget = policy.budget();
    assert!(matches!(
        budget.after(1, cause),
        RetryDecision::Retry(retry) if retry.pause == Duration::from_secs(5)
    ));
    assert!(matches!(
        budget.after(2, cause),
        RetryDecision::Retry(retry) if retry.pause == Duration::from_secs(5)
    ));
    assert!(matches!(
        budget.after(3, cause),
        RetryDecision::Exhausted { .. }
    ));
}
