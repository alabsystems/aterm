// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn every_error_code_classifies_as_declared() {
    assert_eq!(classify(STATUS_COMPLETED, None), CbOutcome::Completed);
    assert_eq!(
        classify(STATUS_COMPLETED, Some(ERROR_PAGE_FAULT)),
        CbOutcome::Completed
    );

    for status in [
        STATUS_NOT_ENQUEUED,
        STATUS_ENQUEUED,
        STATUS_COMMITTED,
        STATUS_SCHEDULED,
    ] {
        assert_eq!(classify(status, None), CbOutcome::Unfinished { status });
    }

    for (code, name) in [
        (ERROR_TIMEOUT, "MTLCommandBufferErrorTimeout"),
        (ERROR_OUT_OF_MEMORY, "MTLCommandBufferErrorOutOfMemory"),
    ] {
        assert_eq!(
            classify(STATUS_ERROR, Some(code)),
            CbOutcome::Retryable { code, name },
            "code {code} must be RETRYABLE"
        );
    }

    for (code, name) in [
        (ERROR_INTERNAL, "MTLCommandBufferErrorInternal"),
        (ERROR_PAGE_FAULT, "MTLCommandBufferErrorPageFault"),
        (ERROR_ACCESS_REVOKED, "MTLCommandBufferErrorAccessRevoked"),
        (ERROR_NOT_PERMITTED, "MTLCommandBufferErrorNotPermitted"),
        (
            ERROR_INVALID_RESOURCE,
            "MTLCommandBufferErrorInvalidResource",
        ),
        (ERROR_MEMORYLESS, "MTLCommandBufferErrorMemoryless"),
        (ERROR_DEVICE_REMOVED, "MTLCommandBufferErrorDeviceRemoved"),
        (ERROR_STACK_OVERFLOW, "MTLCommandBufferErrorStackOverflow"),
    ] {
        assert_eq!(
            classify(STATUS_ERROR, Some(code)),
            CbOutcome::Lost {
                code: Some(code),
                name
            },
            "code {code} must be LOST"
        );
    }

    assert!(matches!(
        classify(STATUS_ERROR, Some(999)),
        CbOutcome::Lost {
            code: Some(999),
            ..
        }
    ));
    assert!(matches!(
        classify(STATUS_ERROR, None),
        CbOutcome::Lost { code: None, .. }
    ));
    assert!(matches!(
        classify(6, None),
        CbOutcome::Lost { code: None, .. }
    ));
}

#[test]
fn the_latch_flips_only_on_lost_and_stays_flipped() {
    let latch = LossLatch::new();
    assert!(!latch.is_lost(), "a fresh latch is not lost");

    latch.record(&classify(STATUS_COMPLETED, None));
    latch.record(&classify(STATUS_SCHEDULED, None));
    latch.record(&classify(STATUS_ERROR, Some(ERROR_TIMEOUT)));
    latch.record(&classify(STATUS_ERROR, Some(ERROR_OUT_OF_MEMORY)));
    assert!(
        !latch.is_lost(),
        "Completed/Unfinished/Retryable must never latch"
    );
    assert_eq!(latch.reason(), None);

    latch.record(&classify(STATUS_ERROR, Some(ERROR_PAGE_FAULT)));
    assert!(latch.is_lost(), "the first Lost must latch");
    let first = latch
        .reason()
        .expect("a latched latch has a reason")
        .to_owned();
    assert!(!first.is_empty(), "a latched reason must not be empty");

    latch.record(&classify(STATUS_COMPLETED, None));
    assert!(latch.is_lost(), "Completed after a loss must not unlatch");
    latch.record(&classify(STATUS_ERROR, Some(ERROR_DEVICE_REMOVED)));
    assert!(latch.is_lost());
    assert_eq!(
        latch.reason(),
        Some(first.as_str()),
        "the FIRST loss's reason survives later ones"
    );
}
