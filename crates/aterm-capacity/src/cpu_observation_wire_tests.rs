// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn unavailable_observations_never_advertise_free_slots() {
    for observation in [
        CpuObservation::Unsupported,
        CpuObservation::Absent,
        CpuObservation::Initializing,
        CpuObservation::Busy,
        CpuObservation::Unreadable("permission denied".into()),
    ] {
        let json = observation.json();
        for field in ["total", "held", "available", "holders"] {
            assert!(json.get(field).is_none(), "{json}");
        }
    }
}

#[test]
fn held_lock_without_owner_record_still_consumes_capacity() {
    let json = CpuObservation::Ready {
        total: 2,
        external: None,
        holders: vec![CpuHolder {
            slot: 1,
            reported_pid: None,
            reported_birth: None,
        }],
    }
    .json();
    assert_eq!(json["held"], Value::from(1u64));
    assert_eq!(json["available"], Value::from(1u64));
    assert_eq!(json["holders"][0]["reported_pid"], Value::Null);
    assert_eq!(json["holders"][0]["reported_birth"], Value::Null);
}
