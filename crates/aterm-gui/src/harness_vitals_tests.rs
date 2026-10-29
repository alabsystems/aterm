// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Partial machine telemetry stays partial in the computer monitor.

use super::*;

fn hello(text: &str) -> aterm_json::Value {
    aterm_json::from_str(text).expect("a machine's telemetry")
}

#[test]
fn cached_vitals_keep_known_capacity_without_inventing_usage() {
    let reading = hello(
        r#"{
            "cpu_cores":18,
            "gpu_name":"Test GPU","gpu_cores":40,
            "mem_total_bytes":17179869184,
            "disk_total_bytes":1099511627776
        }"#,
    );
    let host = "vitals-test-capacity";
    let text = format_hello_vitals_json(host, &reading);
    assert!(text.contains("CPU unavailable (18c)"), "{text}");
    assert!(text.contains("GPU (Test GPU 40c)"), "{text}");
    assert!(text.contains("RAM unknown/16.0 GiB"), "{text}");
    assert!(
        text.contains("Disk unknown/1.00 TiB (unknown free)"),
        "{text}"
    );
    assert!(!text.contains('%'), "no measured share: {text}");
    assert!(sparkline(&format!("{host}/cpu"), None).is_empty());

    let reading = hello(
        r#"{
            "cpu_busy_pct":25,
            "mem_used_bytes":8589934592,
            "disk_used_bytes":549755813888,
            "disk_avail_bytes":274877906944
        }"#,
    );
    let text = format_hello_vitals_json("vitals-test-usage-only", &reading);
    assert!(
        text.contains("CPU 25%") && text.contains("cores unknown"),
        "{text}"
    );
    assert!(text.contains("RAM 8.0 GiB/unknown"), "{text}");
    assert!(
        text.contains("Disk 512 GiB/unknown (256 GiB free)"),
        "{text}"
    );
    assert_eq!(
        text.matches('%').count(),
        1,
        "only CPU has its share: {text}"
    );

    let unavailable =
        hello(r#"{"mem_used_bytes":null,"disk_used_bytes":"unread","disk_avail_bytes":null}"#);
    assert!(format_hello_vitals_json("vitals-test-unread", &unavailable).is_empty());

    for (reading, expected) in [
        (r#"{"gpu_busy_pct":0}"#, "GPU 0%"),
        (r#"{"gpu_cores":40}"#, "GPU (40c)"),
    ] {
        let text = format_hello_vitals_json("vitals-test-partial-gpu", &hello(reading));
        assert!(text.contains(expected), "{text}");
    }
}

#[test]
fn cached_vitals_preserve_measured_zero_and_complete_readings() {
    let zero = hello(
        r#"{
            "cpu_cores":18,"cpu_busy_pct":0,
            "gpu_name":"Test GPU","gpu_busy_pct":0,
            "mem_total_bytes":17179869184,"mem_used_bytes":0,
            "disk_total_bytes":1099511627776,"disk_used_bytes":0,"disk_avail_bytes":0
        }"#,
    );
    let host = "vitals-test-zero";
    let text = format_hello_vitals_json(host, &zero);
    assert!(text.contains("CPU 0%"), "{text}");
    assert!(text.contains("GPU 0%"), "{text}");
    assert!(text.contains("RAM 0 MiB/16.0 GiB (0%"), "{text}");
    assert!(
        text.contains("Disk 0 MiB/1.00 TiB (0% \u{b7} 0 MiB free)"),
        "{text}"
    );
    assert!(
        !text.contains("unknown") && !text.contains("unavailable"),
        "{text}"
    );
    assert_eq!(sparkline(&format!("{host}/cpu"), None).chars().count(), 1);

    let full = hello(
        r#"{
            "cpu_cores":18,"cpu_busy_pct":50,
            "gpu_name":"Test GPU","gpu_busy_pct":50,
            "mem_total_bytes":17179869184,"mem_used_bytes":8589934592,
            "disk_total_bytes":1099511627776,"disk_used_bytes":549755813888,
            "disk_avail_bytes":274877906944
        }"#,
    );
    let text = format_hello_vitals_json("vitals-test-full", &full);
    assert!(text.contains("CPU 50%") && text.contains("(18c)"), "{text}");
    assert!(text.contains("GPU 50%"), "{text}");
    assert!(text.contains("RAM 8.0 GiB/16.0 GiB (50%"), "{text}");
    assert!(
        text.contains("Disk 512 GiB/1.00 TiB (50% \u{b7} 256 GiB free)"),
        "{text}"
    );

    use aterm_vitals::TrendView;
    use aterm_vitals::model::{GpuSample, GpuView, Snapshot};
    let mut snapshot = Snapshot {
        gpu: Some(GpuView {
            load: TrendView::flat(0.85),
            sample: GpuSample {
                device_pct: Some(0),
                cores: Some(40),
                name: Some("Test GPU".into()),
                ..GpuSample::default()
            },
            ..GpuView::default()
        }),
        ..Snapshot::default()
    };
    let text = format_local_snapshot_vitals(&snapshot);
    assert!(
        text.contains("GPU 0%") && text.contains("Test GPU 40c"),
        "{text}"
    );
    snapshot.facts.gpu = Some(GpuSample {
        cores: Some(40),
        name: Some("Test GPU".into()),
        ..GpuSample::default()
    });
    for has_view in [true, false] {
        if !has_view {
            snapshot.gpu = None;
        }
        let text = format_local_snapshot_vitals(&snapshot);
        assert_eq!(
            text, "GPU (Test GPU 40c)",
            "unread use keeps known hardware"
        );
    }
}

#[test]
fn an_unread_cpu_percentage_does_not_become_a_history_sample() {
    let key = "vitals-test-fallback/cpu";
    let first = format_cpu_vitals_gui(key, Some(37), "18c");
    assert!(first.contains("37%"));
    let before = sparkline(key, None);
    assert_eq!(before.chars().count(), 1);

    let unread = format_cpu_vitals_gui(key, None, "18c");
    assert!(
        unread.contains("unavailable") && unread.contains("18c"),
        "{unread}"
    );
    assert!(!unread.contains('%'));
    assert_eq!(
        sparkline(key, None),
        before,
        "an unread sample adds nothing"
    );

    let zero = format_cpu_vitals_gui(key, Some(0), "18c");
    assert!(zero.contains("0%"));
    assert_eq!(
        sparkline(key, None).chars().count(),
        2,
        "a measured zero adds one"
    );

    use aterm_vitals::TrendView;
    use aterm_vitals::model::{CoreView, CpuView, Snapshot};
    let mut snapshot = Snapshot {
        cpu: Some(CpuView {
            cores: vec![CoreView {
                load: TrendView {
                    raw: 0.75,
                    level: 0.25,
                    ..TrendView::default()
                },
                measured: false,
            }],
            ..CpuView::default()
        }),
        ..Snapshot::default()
    };
    let before = sparkline("local/cpu", None);
    let text = format_local_snapshot_vitals(&snapshot);
    assert!(text.contains("CPU unavailable (1c)"), "{text}");
    assert_eq!(sparkline("local/cpu", None), before);
    snapshot.cpu.as_mut().unwrap().cores[0].measured = true;
    let text = format_local_snapshot_vitals(&snapshot);
    assert!(text.contains("CPU 75%"), "{text}");
}

#[test]
fn resource_shares_require_a_measured_numerator_and_positive_capacity() {
    assert_eq!(used_percent_gui(None, Some(100)), None);
    assert_eq!(used_percent_gui(Some(0), None), None);
    assert_eq!(used_percent_gui(Some(0), Some(0)), None);
    assert_eq!(used_percent_gui(Some(0), Some(100)), Some(0));
    assert_eq!(
        used_percent_gui(Some(u64::MAX / 2), Some(u64::MAX)),
        Some(49)
    );
    assert_eq!(used_percent_gui(Some(u64::MAX), Some(u64::MAX)), Some(100));
    assert_eq!(used_percent_gui(Some(u64::MAX), Some(1)), Some(100));

    let available = format_disk_usage_gui(None, None, Some(0)).expect("free was read");
    assert!(available.contains("unknown/unknown") && available.contains("0 MiB free"));
    assert!(!available.contains('%'));

    let snapshot = aterm_vitals::model::Snapshot {
        volume: Some(aterm_vitals::model::VolumeSample {
            total: 512 << 30,
            free: 128 << 30,
            available: 64 << 30,
            mount: "/".into(),
        }),
        ..aterm_vitals::model::Snapshot::default()
    };
    let text = format_local_snapshot_vitals(&snapshot);
    assert!(
        text.contains("Disk 384 GiB/512 GiB (75% \u{b7} 64.0 GiB free)"),
        "reserved blocks are free but unavailable to writes: {text}"
    );
}
