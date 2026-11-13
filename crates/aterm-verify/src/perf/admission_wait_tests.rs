// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn accepted_attempts_or_missing_counters_cannot_become_measurements() {
    let valid = json!({
        "arm":"old", "candidates":32, "workers":2, "passes":4, "attempts":256,
        "admitted":0, "all_queued":true, "holder_unchanged":true,
        "read_calls":512, "process_sweeps":256, "process_cpu_ns":1000,
        "lineage_reads":256, "wait_mark_reads":256,
        "unavailable_observations":0,
        "logical_write_bytes":100, "storage_read_bytes":0, "storage_write_bytes":0,
        "worker_cpu_ns":[400,500]
    });
    validate(&valid, Arm::Old).unwrap();
    for (key, value) in [
        ("admitted", json!(1)),
        ("all_queued", json!(false)),
        ("attempts", json!(255)),
        ("logical_write_bytes", Value::Null),
        ("unavailable_observations", json!(1)),
        ("worker_cpu_ns", json!([400])),
    ] {
        let mut invalid = valid.clone();
        invalid.as_object_mut().unwrap().insert(key.into(), value);
        assert!(validate(&invalid, Arm::Old).is_err());
    }
    assert!(validate(&valid, Arm::Probe).is_err());
}
