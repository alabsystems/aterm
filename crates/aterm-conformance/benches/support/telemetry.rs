// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance's event encoding and CPU reader, separate from the dependency-free
//! runner also mounted by other crates' acceptance benches.

pub fn cpu() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        aterm_sysprobe::jobs::cpu_with_reaped_ns(std::process::id())
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Buffered with the row's result; timestamps name execution, not flush times.
/// CPU includes the runner and reaped descendants. Concurrent count rows share
/// the runner, so their intervals overlap and must not be summed.
pub fn events(
    matrix: &str,
    row: &str,
    started: u64,
    ended: u64,
    seconds: f64,
    cpu_s: Option<f64>,
) -> [String; 2] {
    [
        ("measure-row-started", started, 0.0, None),
        ("measure-row-finished", ended, seconds, cpu_s),
    ]
    .map(|(event, at, seconds, cpu_s)| {
        format!(
            "verify-event: {}",
            aterm_json::json!({
                "schema": 1, "event": event, "matrix": matrix, "row": row,
                "at_unix_ms": at, "seconds": seconds, "cpu_s": cpu_s,
                "cpu_scope": "runner-process-and-reaped-descendants",
                "cpu_intervals_overlap_for_concurrent_rows": true,
            })
        )
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn timing_retains_row_identity_and_execution_boundaries() {
        let events =
            super::events("paint", "quoted \"row\"", 123, 456, 0.25, Some(0.125)).map(|line| {
                aterm_json::from_str::<aterm_json::Value>(
                    line.strip_prefix("verify-event: ").unwrap(),
                )
                .unwrap()
            });
        assert_eq!(events[0]["event"].as_str(), Some("measure-row-started"));
        assert_eq!(events[0]["at_unix_ms"].as_u64(), Some(123));
        assert_eq!(events[0]["seconds"].as_f64(), Some(0.0));
        assert!(events[0]["cpu_s"].is_null());
        assert_eq!(events[1]["event"].as_str(), Some("measure-row-finished"));
        assert_eq!(events[1]["at_unix_ms"].as_u64(), Some(456));
        assert_eq!(events[1]["seconds"].as_f64(), Some(0.25));
        assert_eq!(events[1]["cpu_s"].as_f64(), Some(0.125));
        for event in events {
            assert_eq!(event["matrix"].as_str(), Some("paint"));
            assert_eq!(event["row"].as_str(), Some("quoted \"row\""));
        }
        let missing = super::events("spin", "missing", 123, 456, 0.25, None);
        let missing: aterm_json::Value =
            aterm_json::from_str(missing[1].strip_prefix("verify-event: ").unwrap()).unwrap();
        assert!(missing["cpu_s"].is_null());
    }
}
