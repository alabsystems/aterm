// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn fulfillment_requires_a_complete_recent_matching_generation_and_boundary() {
    let scope = RefreshScope::new([1; 32]);
    let observed = RefreshObservation {
        scope: Some(scope),
        version: RefreshVersion {
            input: 2,
            attempt: 3,
            fulfilled: 3,
        },
        recheck_ms: Some(1),
        ..RefreshObservation::default()
    };
    let initial = RefreshRequest::ensure(Some(scope), RefreshVersion::default());
    assert!(observed.fulfills(initial));
    assert!(
        !observed.fulfills(RefreshRequest {
            scope: None,
            ..initial
        }),
        "an unbound request cannot inherit an unrelated receipt"
    );
    assert!(
        !RefreshObservation {
            recheck_ms: Some(0),
            ..observed
        }
        .fulfills(initial)
    );
    assert!(
        !RefreshObservation {
            pending: Some(initial),
            ..observed
        }
        .fulfills(initial)
    );
    assert!(!observed.fulfills(RefreshRequest {
        scope: Some(RefreshScope::new([2; 32])),
        ..initial
    }));
    let changed = RefreshRequest::inputs(
        scope,
        RefreshVersion {
            input: 2,
            ..RefreshVersion::default()
        },
    );
    assert!(
        !observed.fulfills(changed),
        "same bytes do not erase an unacknowledged transition"
    );
    let older = RefreshRequest::inputs(
        scope,
        RefreshVersion {
            input: 1,
            ..RefreshVersion::default()
        },
    );
    assert!(
        !observed.fulfills(older.merge(changed)),
        "an older pending request cannot erase the later ABA boundary"
    );
    assert!(
        RefreshObservation {
            version: RefreshVersion {
                input: 3,
                ..observed.version
            },
            ..observed
        }
        .fulfills(changed)
    );
    assert!(!observed.fulfills(RefreshRequest::after(Some(scope), observed.version)));
    assert!(observed.fulfills(RefreshRequest::after(
        Some(scope),
        RefreshVersion {
            attempt: 2,
            ..observed.version
        }
    )));
}

#[test]
fn shared_retry_covers_only_its_retained_source_transition_and_forced_boundary() {
    let scope = RefreshScope::new([1; 32]);
    let pending = RefreshRequest {
        scope: Some(scope),
        base_input: 2,
        changed: true,
        after_attempt: Some(4),
    };
    let observed = RefreshObservation {
        pending: Some(pending),
        retry_ms: Some(5_000),
        ..RefreshObservation::default()
    };
    assert_eq!(observed.retry_for(pending), Some(5_000));
    assert_eq!(
        observed.retry_for(RefreshRequest {
            base_input: 1,
            ..pending
        }),
        Some(5_000)
    );
    assert_eq!(
        observed.retry_for(RefreshRequest {
            base_input: 3,
            ..pending
        }),
        None
    );
    assert_eq!(
        observed.retry_for(RefreshRequest {
            after_attempt: Some(5),
            ..pending
        }),
        None
    );
    assert_eq!(
        observed.retry_for(RefreshRequest {
            scope: Some(RefreshScope::new([2; 32])),
            ..pending
        }),
        None
    );
}
