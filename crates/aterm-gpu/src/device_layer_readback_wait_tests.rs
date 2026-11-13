// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::metal::loss::CbOutcome;
use std::time::Duration;

#[test]
fn scheduled_readback_can_finish_after_other_gpu_work() {
    let mut looks = 0;
    let result = wait_readback(aterm_time::Instant::now() - Duration::from_secs(6), || {
        looks += 1;
        if looks == 1 {
            None
        } else {
            Some(CbOutcome::Completed)
        }
    });
    assert_eq!(result, Ok(()));
    assert_eq!(looks, 2);

    let mut looks = 0;
    let expired = wait_readback(aterm_time::Instant::now() - Duration::from_secs(61), || {
        looks += 1;
        None
    });
    assert!(expired.is_err());
    assert_eq!(looks, 1);
}

#[test]
fn only_completed_readback_may_expose_the_buffer() {
    for outcome in [
        CbOutcome::Unfinished { status: 3 },
        CbOutcome::Retryable {
            code: 2,
            name: "fixture retryable",
        },
        CbOutcome::Lost {
            code: Some(4),
            name: "fixture lost",
        },
    ] {
        assert!(wait_readback(aterm_time::Instant::now(), || Some(outcome)).is_err());
    }
}
