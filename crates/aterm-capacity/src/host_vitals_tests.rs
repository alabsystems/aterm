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

const GIB: u64 = 1 << 30;

/// One look at a 48 GiB, 14-core Mac: every reading present, each class distinct from every
/// other so a figure read from the wrong one shows.
fn full_look() -> Look {
    Look {
        cores: Some(14),
        load: Some([2.5, 3.0, 4.004]),
        memory: Some(aterm_sysprobe::MemoryDetail {
            total: 48 * GIB,
            available: 7 * GIB,
            free: GIB,
            speculative: Some(GIB / 4),
            active: Some(20 * GIB),
            inactive: Some(10 * GIB),
            wired: 4 * GIB,
            compressed: Some(3 * GIB),
            stored: Some(9 * GIB),
            purgeable: Some(GIB / 2),
            file_backed: 5 * GIB,
            app: 18 * GIB,
            shared: Some(GIB),
            swap_used: GIB / 2,
            swap_total: 2 * GIB,
            pressure: Some(aterm_sysprobe::MemoryLevel::Warn),
            exact: true,
            ..aterm_sysprobe::MemoryDetail::default()
        }),
        gpu: Some(aterm_sysprobe::GpuReading {
            device_pct: Some(37),
            cores: Some(40),
            name: Some(" Apple M5 Max ".into()),
            ..aterm_sysprobe::GpuReading::default()
        }),
        volume: Some(aterm_sysprobe::volumes::Reading {
            mount: "/System/Volumes/Data".into(),
            key: aterm_sysprobe::volumes::VolumeKey::Container("disk3".into()),
            total: 1000 * GIB,
            free: 200 * GIB,
            files_free: None,
            dev: 1,
            read_only: false,
        }),
    }
}

#[test]
fn a_look_maps_every_reading_to_the_figure_it_owns() {
    let v = HostVitals::of(&full_look(), Some(4 * 86_400 + 11 * 3_600));
    assert_eq!(
        v,
        HostVitals {
            cpu_cores: Some(14),
            load_avg_100: Some([250, 300, 400]),
            gpu_name: Some("Apple M5 Max".into()),
            gpu_cores: Some(40),
            gpu_busy_pct: Some(37),
            mem_total_bytes: Some(48 * GIB),
            mem_available_bytes: Some(7 * GIB),
            mem_used_bytes: Some(26 * GIB),
            mem_wired_bytes: Some(4 * GIB),
            mem_compressed_bytes: Some(3 * GIB),
            swap_used_bytes: Some(GIB / 2),
            mem_pressure: Some("warn".into()),
            disk_total_bytes: Some(1000 * GIB),
            disk_free_bytes: Some(200 * GIB),
            uptime_secs: Some(4 * 86_400 + 11 * 3_600),
            rtt_us: None,
        }
    );
    assert_eq!(v.disk_used(), Some(800 * GIB));
    let wire = aterm_json::to_string(&v).unwrap();
    assert_eq!(aterm_json::from_str::<HostVitals>(&wire).unwrap(), v);
}

#[test]
fn a_reading_that_is_absent_leaves_its_figures_absent() {
    let bare = Look {
        cores: Some(14),
        ..Look::default()
    };
    assert_eq!(
        HostVitals::of(&bare, None),
        HostVitals {
            cpu_cores: Some(14),
            ..HostVitals::default()
        }
    );
    let unread_cores = Look {
        cores: Some(0),
        ..full_look()
    };
    assert_eq!(
        HostVitals::of(&unread_cores, None).cpu_cores,
        None,
        "a layout that reads no core says nothing, not one core"
    );
    assert_eq!(
        HostVitals::of(&full_look(), None).uptime_secs,
        None,
        "an uptime nobody read is absent, whatever else was read"
    );
}

/// LAW: available memory is the memory classes' own figure (`MemoryDetail::available`, which
/// `sys mem` and admission read), never the free, inactive and purgeable pages' sum; memory
/// used is `MemoryDetail::used`, app (shared included), wired and compressed, at most the
/// total, never the active queue. CONTROL: on this reading each rejected formula gives
/// another figure, so a mapping that used it fails.
#[test]
fn available_and_used_memory_each_have_one_definition() {
    let r = full_look();
    let v = HostVitals::of(&r, None);
    let m = r.memory.unwrap();
    let overstated = m.free + m.inactive.unwrap() + m.purgeable.unwrap();
    assert_eq!(v.mem_available_bytes, Some(m.available));
    assert_ne!(
        v.mem_available_bytes,
        Some(overstated),
        "CONTROL: free + inactive + purgeable overstates"
    );
    assert_eq!(
        v.mem_used_bytes,
        Some(m.app + m.shared.unwrap() + m.wired + 3 * GIB)
    );
    assert_ne!(
        v.mem_used_bytes,
        Some(m.active.unwrap() + m.wired + 3 * GIB),
        "CONTROL: the active queue is not memory used"
    );
    let mut over = r.clone();
    if let Some(memory) = over.memory.as_mut() {
        memory.total = 20 * GIB;
    }
    assert_eq!(HostVitals::of(&over, None).mem_used_bytes, Some(20 * GIB));
}

/// A look now reads this machine in process: its logical cores, and the volume holding the
/// home it is given, never smaller than what is free on it. CONTROL: asked for no home, it
/// reads no volume. A look without the GPU reads the same machine and volume and leaves the
/// GPU unread.
#[test]
fn a_look_now_reads_this_machine_and_the_homes_volume() {
    let home = aterm_tempfile::tempdir().unwrap();
    let look = Look::now(Some(home.path()));
    assert!(look.cores.is_some_and(|n| n > 0), "{look:?}");
    let volume = look.volume.expect("the home's volume");
    assert!(
        volume.total > 0 && volume.total >= volume.free,
        "{volume:?}"
    );
    assert_eq!(Look::now(None).volume, None);
    let bare = Look::without_gpu(Some(home.path()));
    assert_eq!(bare.gpu, None);
    assert_eq!(bare.cores, look.cores);
    assert_eq!(
        bare.volume.map(|v| (v.key, v.total)),
        Some((volume.key, volume.total))
    );
}
