// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_uds::deadline::Gate;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn native_gate_waits_for_a_scheduled_evaluation() {
    static GATE: Gate = Gate::new("certificate-wait-regression");
    let ran = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&ran);
    let result = run_evaluation_after(&GATE, Duration::from_secs(31), move || {
        count.fetch_add(1, Ordering::Relaxed);
        7
    });
    assert!(matches!(result, Ok(7)), "{result:?}");
    assert_eq!(ran.load(Ordering::Relaxed), 1);
    assert_eq!(GATE.running(), 0);

    let count = Arc::clone(&ran);
    let expired = run_evaluation_after(&GATE, Duration::from_secs(61), move || {
        count.fetch_add(1, Ordering::Relaxed);
        9
    });
    assert!(matches!(expired, Err(Refused::Unanswered)), "{expired:?}");
    assert_eq!(ran.load(Ordering::Relaxed), 1, "expiry starts no helper");
    assert_eq!(GATE.running(), 0);
}
