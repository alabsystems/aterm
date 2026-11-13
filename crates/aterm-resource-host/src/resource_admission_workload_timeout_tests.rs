// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn complete_maximum_repetition_budget_remains_finite_without_a_day_cap() {
    let mut request = aterm_types::resource_admission::Submission {
        binding: Binding {
            session: "s-acde".into(),
            attempt: [1; 16],
        },
        peer_pid: std::process::id(),
        peer_birth: 1,
        argv: vec![
            b"/sdk/bin/targo".to_vec(),
            wire::WORKLOAD_STARTUP_ARGUMENT.as_bytes().to_vec(),
        ],
        cwd: b"/work".to_vec(),
        env: Vec::new(),
        cpu_units: 1,
        memory_bytes: 1,
        label: "finite workload profile".into(),
        timeout_ms: Some(261_900_000),
    };
    // 67 real phase budgets at3600s,67 bounded300s handoffs,300s
    // startup and300s settlement. This value is containment DATA only.
    assert!(closed_request(&request));
    for timeout in [
        None,
        Some(0),
        Some(host::MAX_ROOT_TIMEOUT_MS + 1),
        Some(u64::MAX),
    ] {
        request.timeout_ms = timeout;
        assert!(!closed_request(&request));
    }
    request.timeout_ms = Some(1);
    assert!(closed_request(&request));
}
