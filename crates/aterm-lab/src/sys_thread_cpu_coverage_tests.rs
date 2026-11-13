// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn idle_cpu_native_snapshot_preserves_failed_tids_and_readable_counters() {
    let reading = thread_cpu_from_ids(vec![3, 1, 2], |tid| {
        (tid != 2).then(|| ThreadCpu {
            tid,
            name: "fixture".into(),
            ns: tid * 10,
        })
    });
    assert!(!reading.complete());
    assert!(reading.list_complete);
    assert_eq!(reading.missing_tids, [2]);
    assert_eq!(
        reading
            .threads
            .iter()
            .map(|t| (t.tid, t.ns))
            .collect::<Vec<_>>(),
        [(1, 10), (3, 30)]
    );
}

#[test]
fn idle_cpu_native_empty_or_full_table_is_not_complete() {
    for ids in [Vec::new(), (1..=512).collect()] {
        let reading = thread_cpu_from_ids(ids, |tid| {
            Some(ThreadCpu {
                tid,
                name: String::new(),
                ns: 0,
            })
        });
        assert!(
            !reading.complete(),
            "a zero or potentially truncated table is not a complete observation"
        );
    }
    let reading = thread_cpu_from_ids(vec![1], |tid| {
        Some(ThreadCpu {
            tid,
            name: "main".into(),
            ns: 0,
        })
    });
    assert!(
        reading.complete(),
        "a genuine complete zero counter is known zero"
    );
}
