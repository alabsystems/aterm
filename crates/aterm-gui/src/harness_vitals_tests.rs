// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Partial machine telemetry stays partial in the computer monitor.

use super::*;

fn hello(text: &str) -> HostVitals {
    aterm_json::from_str(text).expect("a machine's telemetry")
}

/// A cached hello is read as the agents' own [`HostVitals`] and shown in their one line
/// ([`HostVitals::summary`]), the line `aterm machines` prints for the same hello; a vitals object
/// that is not one shows no vitals rather than a guess.
#[test]
fn a_cached_hellos_vitals_are_read_as_host_vitals() {
    let row = |vitals: &str| {
        let hello = format!(r#"{{"os_version":"27.0","vitals":{vitals}}}"#);
        let state = aterm_json::from_str::<aterm_json::Value>(&format!(
            "{{\"hello\":{}}}",
            aterm_json::to_string(&hello).unwrap()
        ))
        .unwrap();
        cached_configured_computer(
            "remote".into(),
            "remote".into(),
            "remote.local".into(),
            CachedMachineState::from_json(Some(&state)),
        )
        .0
        .vitals
    };
    let vitals = r#"{"cpu_cores":18,"load_avg_100":[250,300,400],"mem_total_bytes":17179869184,"mem_used_bytes":8589934592,"mem_available_bytes":4294967296,"disk_total_bytes":1024000,"disk_free_bytes":204800,"uptime_secs":385200}"#;
    let now = row(vitals);
    assert_eq!(now, hello(vitals).format_summary_line());
    assert!(
        now.starts_with("CPU 18c \u{b7} load 2.50/3.00/4.00"),
        "{now}"
    );
    assert!(now.contains("4.0 GiB available"), "{now}");
    assert!(now.contains("(80% \u{b7} "), "{now}");
    assert_eq!(row(r#"{"mem_used_bytes":"unread","cpu_cores":18}"#), "");
    assert_eq!(
        row(r#"{"mem_used_bytes":null}"#),
        "",
        "nothing read, nothing shown"
    );
}

/// This Mac's row reads `reading` until the engine has taken its first Snapshot, and the
/// Snapshot's figures after: nothing samples this Mac for the row besides the engine.
#[test]
fn this_mac_reads_reading_until_the_engines_first_snapshot() {
    assert_eq!(local_machine_vitals(None), LOCAL_VITALS_READING);
    let mut snapshot = aterm_vitals::model::Snapshot::default();
    snapshot.facts.gpu = Some(aterm_vitals::model::GpuSample {
        cores: Some(40),
        name: Some("Test GPU".into()),
        ..aterm_vitals::model::GpuSample::default()
    });
    let read = local_machine_vitals(Some(&snapshot));
    assert_eq!(read, "GPU Test GPU \u{b7} 40 cores");

    let mut app = App::headless_for_test();
    app.vitals.snapshot = None;
    let state = app.tab_control_state(crate::autopilot::HarnessPosture::default());
    assert_eq!(state.computers[0].id, "local");
    assert_eq!(state.computers[0].vitals, LOCAL_VITALS_READING);
    app.vitals.snapshot = Some(std::sync::Arc::new(snapshot));
    let state = app.tab_control_state(crate::autopilot::HarnessPosture::default());
    assert_eq!(state.computers[0].vitals, read);
}

/// This Mac's GPU reads its use where the GPU view measured it, and its hardware facts keep its
/// name and cores where its use is unread.
#[test]
fn this_macs_gpu_keeps_known_hardware_when_its_use_is_unread() {
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
        text.starts_with("GPU 0% ") && text.ends_with(" \u{b7} Test GPU \u{b7} 40 cores"),
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
            text, "GPU Test GPU \u{b7} 40 cores",
            "unread use keeps known hardware"
        );
    }
}

#[test]
fn an_unread_cpu_percentage_does_not_become_a_history_sample() {
    let key = "vitals-test-unread/cpu";
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

/// This Mac's memory and home volume read in a hello's words: used of the total with its
/// share, the pressure, and the volume's used bytes as its size less what this user can write.
#[test]
fn this_macs_memory_and_disk_read_as_any_macs() {
    const GIB: u64 = 1 << 30;
    let snapshot = aterm_vitals::model::Snapshot {
        mem: Some(aterm_vitals::model::MemView {
            sample: aterm_vitals::model::MemSample {
                total: 16 * GIB,
                app: 6 * GIB,
                wired: 2 * GIB,
                pressure: aterm_vitals::model::Pressure::Normal,
                ..aterm_vitals::model::MemSample::default()
            },
            ..aterm_vitals::model::MemView::default()
        }),
        volume: Some(aterm_vitals::model::VolumeSample {
            total: 512 << 30,
            free: 64 << 30,
            reserved: 64 << 30,
            mount: "/".into(),
        }),
        ..aterm_vitals::model::Snapshot::default()
    };
    let text = format_local_snapshot_vitals(&snapshot);
    assert!(
        text.contains("RAM 8.0 GiB used/16.0 GiB total (50% "),
        "{text}"
    );
    assert!(
        text.contains(" \u{b7} normal \u{b7} wired 2.0 GiB"),
        "{text}"
    );
    assert!(
        text.contains("Disk 448 GiB used/512 GiB total (87% \u{b7} 64.0 GiB free)"),
        "used is the total less what this user can write, reserved blocks included: {text}"
    );
}
