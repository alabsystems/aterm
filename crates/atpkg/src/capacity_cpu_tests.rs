// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_capacity::CpuHolder;

use super::*;

#[test]
fn rendering_preserves_cpu_lease_counts_and_reported_identities() {
    let report = Report(CpuObservation::Ready {
        total: 2,
        external: None,
        holders: vec![CpuHolder {
            slot: 1,
            reported_pid: Some(42),
            reported_birth: Some(7),
        }],
    });
    assert_eq!(
        report.lines(),
        [
            "cpu-leases 1 held, 1 available of 2",
            "  slot 1 reported_pid=42 reported_birth=7",
        ]
    );
    assert_eq!(report.json().get("held"), Some(&Value::from(1u64)));
    assert_eq!(report.json().get("available"), Some(&Value::from(1u64)));
    assert_eq!(report.json().get("total"), Some(&Value::from(2u64)));
    let unknown = Report(CpuObservation::Ready {
        total: 1,
        external: None,
        holders: vec![CpuHolder {
            slot: 0,
            reported_pid: None,
            reported_birth: None,
        }],
    });
    assert_eq!(
        unknown.lines()[1],
        "  slot 0 reported_pid=unknown reported_birth=unknown"
    );
    assert_eq!(unknown.json().get("held"), Some(&Value::from(1u64)));
}

#[test]
fn unavailable_states_never_render_empty_capacity() {
    for (observation, state, line, failed) in [
        (
            CpuObservation::Unsupported,
            "unsupported",
            "cpu-leases unsupported on this platform",
            false,
        ),
        (
            CpuObservation::Absent,
            "absent",
            "cpu-leases pool absent",
            false,
        ),
        (
            CpuObservation::Initializing,
            "initializing",
            "cpu-leases initializing; occupancy unavailable",
            false,
        ),
        (
            CpuObservation::Unreadable("invalid".to_owned()),
            "unreadable",
            "cpu-leases unreadable: invalid",
            true,
        ),
    ] {
        let report = Report(observation);
        assert_eq!(report.lines(), [line]);
        assert_eq!(report.json().get("state"), Some(&Value::from(state)));
        for key in ["held", "available", "total"] {
            assert!(report.json().get(key).is_none());
        }
        assert_eq!(report.failed(), failed);
    }
}
