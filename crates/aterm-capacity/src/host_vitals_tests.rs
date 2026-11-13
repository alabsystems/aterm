// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn vitals_omit_unread_values_on_the_wire_and_keep_a_measured_zero() {
    let wire = r#"{"cpu_cores":18,"mem_total_bytes":17179869184,"mem_used_bytes":0,"disk_total_bytes":1024000,"disk_free_bytes":204800}"#;
    let v: HostVitals = aterm_json::from_str(wire).unwrap();
    assert_eq!(v.cpu_cores, Some(18));
    assert_eq!(v.mem_total_bytes, Some(16 << 30));
    assert_eq!(v.mem_used_bytes, Some(0));
    assert_eq!(v.disk_total_bytes, Some(1_024_000));
    assert_eq!(v.disk_free_bytes, Some(204_800));
    assert_eq!(v.gpu_cores, None);
    let json = aterm_json::to_value(&v).unwrap();
    assert_eq!(json.get("mem_used_bytes").and_then(|v| v.as_u64()), Some(0));
    assert!(json.get("gpu_cores").is_none());
    assert_eq!(
        aterm_json::from_str::<HostVitals>("{}").unwrap(),
        HostVitals::default()
    );
    assert_eq!(
        aterm_json::from_str::<HostVitals>(r#"{"mem_used_bytes":null}"#)
            .unwrap()
            .mem_used_bytes,
        None
    );
    let absent = aterm_json::to_value(HostVitals::default()).unwrap();
    for field in [
        "cpu_cores",
        "mem_total_bytes",
        "mem_available_bytes",
        "mem_used_bytes",
        "disk_total_bytes",
        "disk_free_bytes",
    ] {
        assert!(absent.get(field).is_none(), "{field}: {absent}");
    }
}

/// LAW: the disk's used bytes are its size less what is free (`f_bavail`), so the blocks a
/// volume reserves from this account count as used: what `sys disk` reads as used on the
/// same volume ([`aterm_vitals::model::VolumeSample::used`]). CONTROL: `df`'s Used column
/// (size less `f_bfree`) on the same volume is less, so a used figure that left the reserve
/// out fails.
#[test]
fn disk_used_is_the_size_less_what_this_account_may_write() {
    let total = 1_000_000_u64;
    let f_bavail = 200_000_u64;
    let f_bfree = 250_000_u64;
    let v = HostVitals {
        disk_total_bytes: Some(total),
        disk_free_bytes: Some(f_bavail),
        ..HostVitals::default()
    };
    assert_eq!(v.disk_used(), Some(total - f_bavail));
    let sys_disk = aterm_vitals::model::VolumeSample {
        total,
        free: f_bavail,
        reserved: f_bfree - f_bavail,
        mount: "/".into(),
    };
    assert_eq!(v.disk_used(), Some(sys_disk.used()), "as sys disk reads it");
    assert_ne!(
        v.disk_used(),
        Some(total - f_bfree),
        "CONTROL: df's Used counts reserved blocks as free"
    );
    assert!(
        v.format_disk().contains("(80% \u{b7} "),
        "{}",
        v.format_disk()
    );
    let past = HostVitals {
        disk_free_bytes: Some(total + 1),
        ..v.clone()
    };
    assert_eq!(past.disk_used(), Some(0), "never below nothing");
    let free_only = HostVitals {
        disk_free_bytes: Some(f_bavail),
        ..HostVitals::default()
    };
    assert_eq!(free_only.disk_used(), None);
}

#[test]
fn vitals_format_partial_and_zero_readings_without_inventing_capacity() {
    let sparse = HostVitals {
        cpu_cores: Some(18),
        load_avg_100: Some([250, 300, 400]),
        mem_total_bytes: Some(16 << 30),
        disk_total_bytes: Some(1 << 40),
        gpu_cores: Some(40),
        ..Default::default()
    };
    assert_eq!(sparse.format_cpu(), "18c \u{b7} load 2.50/3.00/4.00");
    assert!(
        sparse
            .format_memory()
            .contains("unknown used/16.0 GiB total")
    );
    assert!(!sparse.format_memory().contains('%'));
    assert!(sparse.format_disk().contains("unknown used/1.00 TiB total"));
    assert!(sparse.format_disk().contains("unknown free"));
    assert!(!sparse.format_disk().contains('%'));
    assert_eq!(sparse.format_gpu(), "40 cores");
    let zero = HostVitals {
        gpu_busy_pct: Some(0),
        mem_used_bytes: Some(0),
        mem_available_bytes: Some(0),
        disk_free_bytes: Some(1 << 40),
        ..sparse.clone()
    };
    assert!(zero.format_gpu().contains("0%"));
    assert!(
        zero.format_memory()
            .contains("0 MiB used/16.0 GiB total (0%")
    );
    assert!(zero.format_memory().contains("0 MiB available"));
    assert!(zero.format_disk().contains("0 MiB used/1.00 TiB total (0%"));
    assert!(zero.format_disk().contains("1.00 TiB free"));
    let usage_only = HostVitals {
        mem_used_bytes: Some(1 << 30),
        disk_free_bytes: Some(3 << 30),
        ..Default::default()
    };
    let line = usage_only.format_summary_line();
    assert!(line.contains("1.0 GiB used/unknown total"), "{line}");
    assert!(line.contains("unknown used/unknown total"), "{line}");
    assert!(line.contains("3.0 GiB free"), "{line}");
    assert!(!line.contains('%'), "{line}");
    assert_eq!(HostVitals::default().format_cpu(), "unprobed");
    assert_eq!(
        HostVitals {
            cpu_cores: Some(18),
            ..Default::default()
        }
        .format_cpu(),
        "18c"
    );
    assert_eq!(
        HostVitals {
            load_avg_100: Some([100, 0, 0]),
            ..Default::default()
        }
        .format_cpu(),
        "cores unknown \u{b7} load 1.00/0.00/0.00"
    );
    let large_rtt = HostVitals {
        rtt_us: Some(5_000_000_000),
        ..Default::default()
    };
    assert_eq!(large_rtt.format_summary_line(), "RTT 5000000.0 ms");
    assert_eq!(HostVitals::default().summary(), None);
    assert_eq!(
        HostVitals::default().format_summary_line(),
        "telemetry unprobed"
    );
}

#[test]
fn a_share_needs_a_measured_numerator_and_a_positive_capacity() {
    assert_eq!(HostVitals::used_percent(None, Some(100)), None);
    assert_eq!(HostVitals::used_percent(Some(0), None), None);
    assert_eq!(HostVitals::used_percent(Some(0), Some(0)), None);
    assert_eq!(HostVitals::used_percent(Some(0), Some(100)), Some(0));
    assert_eq!(
        HostVitals::used_percent(Some(u64::MAX / 2), Some(u64::MAX)),
        Some(49)
    );
    assert_eq!(
        HostVitals::used_percent(Some(u64::MAX), Some(u64::MAX)),
        Some(100)
    );
    assert_eq!(HostVitals::used_percent(Some(u64::MAX), Some(1)), Some(100));
    assert_eq!(HostVitals::observed_bytes(None), "unknown");
    assert_eq!(HostVitals::observed_bytes(Some(0)), "0 MiB");
}
