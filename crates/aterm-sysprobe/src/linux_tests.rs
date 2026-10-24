// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Linux arm's readers on fixture trees.

use std::path::Path;

use crate::jobs::RunState;
use crate::linux::{self, Root, ThrottleReader, TripKind, Zone};
use crate::{Cap, CapSource, MemoryLevel, ThrottleLevel, Why};

type Row<'a> = (u32, u32, &'a str, Option<u64>, Option<u64>);

fn tree(files: &[(&str, &str)]) -> aterm_tempfile::TempDir {
    let root = aterm_tempfile::tempdir().expect("scratch root");
    for (rel, text) in files {
        write(root.path(), rel, text);
    }
    root
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("dirs");
    std::fs::write(path, text).expect("a fixture file");
}

fn names(layout: &crate::CpuLayout) -> Vec<(&str, Vec<u16>)> {
    layout
        .clusters
        .iter()
        .map(|c| (c.name.as_str(), c.logical.clone()))
        .collect()
}

#[test]
fn a_hybrid_intel_laptop_is_split_by_its_pmus() {
    let root = tree(&[
        ("sys/devices/system/cpu/present", "0-15\n"),
        ("sys/devices/system/cpu/online", "0-15\n"),
        ("sys/devices/cpu_core/cpus", "0-7\n"),
        ("sys/devices/cpu_core/type", "4\n"),
        ("sys/devices/cpu_atom/cpus", "8-15\n"),
        ("sys/devices/cpu/type", "4\n"),
    ]);
    let layout = Root::new(root.path()).cpu_layout();
    assert_eq!(
        names(&layout),
        [
            ("Performance", (0..8).collect()),
            ("Efficiency", (8..16).collect())
        ]
    );
}

#[test]
fn a_hybrid_laptop_with_an_offline_core_keeps_its_split() {
    let root = tree(&[
        ("sys/devices/system/cpu/present", "0-15\n"),
        ("sys/devices/system/cpu/online", "0-2,4-14\n"),
        ("sys/devices/cpu_core/cpus", "0-2,4-7\n"),
        ("sys/devices/cpu_atom/cpus", "8-14\n"),
    ]);
    let layout = Root::new(root.path()).cpu_layout();
    assert_eq!(
        names(&layout),
        [
            ("Performance", (0..8).collect()),
            ("Efficiency", (8..16).collect())
        ]
    );
    let present: Vec<u16> = (0..16).collect();
    let online: Vec<u16> = (0..15).collect();
    let pmus = [
        ("cpu_core".to_owned(), vec![0, 1, 2, 4, 5, 6, 7]),
        ("cpu_atom".to_owned(), (8..15).collect()),
    ];
    let one = linux::layout_from(&present, Some(&online), &pmus, &[]);
    assert_eq!(names(&one), [("CPU", present.clone())]);
}

#[test]
fn an_arm_board_is_split_by_cpu_capacity() {
    let mut files = vec![(
        "sys/devices/system/cpu/present".to_owned(),
        "0-7\n".to_owned(),
    )];
    for (id, cap) in [414, 414, 414, 414, 1024, 1024, 1024, 1024]
        .iter()
        .enumerate()
    {
        files.push((
            format!("sys/devices/system/cpu/cpu{id}/cpu_capacity"),
            format!("{cap}\n"),
        ));
    }
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let root = tree(&refs);
    assert_eq!(
        names(&Root::new(root.path()).cpu_layout()),
        [
            ("Performance", vec![4, 5, 6, 7]),
            ("Efficiency", vec![0, 1, 2, 3])
        ]
    );

    let three: Vec<(u16, u32)> = vec![
        (0, 160),
        (1, 160),
        (2, 160),
        (3, 160),
        (4, 498),
        (5, 498),
        (6, 1024),
        (7, 1024),
    ];
    let layout = linux::layout_from(&(0..8).collect::<Vec<_>>(), None, &[], &three);
    assert_eq!(
        names(&layout),
        [
            ("Performance", vec![6, 7]),
            ("Mid", vec![4, 5]),
            ("Efficiency", vec![0, 1, 2, 3])
        ]
    );
}

#[test]
fn a_gb10_is_two_kinds_however_its_capacities_spread() {
    let core = |id: u16| -> (u32, &str, &str) {
        match id {
            0..=4 => (718, "0x00000000410fd871", "2808000"),
            10..=14 => (731, "0x00000000410fd871", "2808000"),
            5..=9 => (997, "0x00000000410fd851", "3900000"),
            19 => (1024, "0x00000000410fd851", "3900000"),
            _ => (1017, "0x00000000410fd851", "3900000"),
        }
    };
    let mut files = vec![
        (
            "sys/devices/system/cpu/present".to_owned(),
            "0-19\n".to_owned(),
        ),
        (
            "sys/devices/armv8_pmuv3_0/cpus".to_owned(),
            "0-4,10-14\n".to_owned(),
        ),
        (
            "sys/devices/armv8_pmuv3_1/cpus".to_owned(),
            "5-9,15-19\n".to_owned(),
        ),
    ];
    for id in 0..20 {
        let (cap, midr, top) = core(id);
        let cpu = format!("sys/devices/system/cpu/cpu{id}");
        files.push((format!("{cpu}/cpu_capacity"), format!("{cap}\n")));
        files.push((
            format!("{cpu}/regs/identification/midr_el1"),
            format!("{midr}\n"),
        ));
        files.push((
            format!("{cpu}/cpufreq/cpuinfo_max_freq"),
            format!("{top}\n"),
        ));
    }
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let root = tree(&refs);
    let x925: Vec<u16> = (5..10).chain(15..20).collect();
    let a725: Vec<u16> = (0..5).chain(10..15).collect();
    assert_eq!(
        names(&Root::new(root.path()).cpu_layout()),
        [("Performance", x925), ("Efficiency", a725)]
    );
}

#[test]
fn a_design_is_its_part_at_its_clock() {
    let a78 = 0x411f_d410_u64;
    let a55 = 0x412f_d050_u64;
    let caps = [(0, 1024), (1, 890), (2, 880), (3, 400)];
    let designs = [
        (0, (a78 & linux::MIDR_DESIGN, Some(3_000_000))),
        (1, (a78 & linux::MIDR_DESIGN, Some(2_600_000))),
        (2, ((a78 | 0x1) & linux::MIDR_DESIGN, Some(2_600_000))),
        (3, (a55 & linux::MIDR_DESIGN, Some(2_000_000))),
    ];
    assert_eq!(
        linux::capacities_by_design(&caps, &designs),
        [(0, 1024), (1, 890), (2, 890), (3, 400)]
    );
    let spread = [(0, 1024), (1, 1017), (2, 997), (3, 731)];
    let one = (0x410f_d851 & linux::MIDR_DESIGN, Some(3_900_000));
    let gb10 = [
        (0, one),
        (1, one),
        (2, one),
        (3, (0x410f_d871 & linux::MIDR_DESIGN, Some(2_808_000))),
    ];
    assert_eq!(
        linux::capacities_by_design(&spread, &gb10),
        [(0, 1024), (1, 1024), (2, 1024), (3, 731)]
    );
    assert_eq!(linux::capacities_by_design(&spread, &gb10[..3]), spread);
    let present: Vec<u16> = (0..4).collect();
    assert_eq!(
        linux::layout_from(&present, None, &[], &spread)
            .clusters
            .len(),
        4
    );
}

#[test]
fn alike_cores_and_inconsistent_sources_are_one_cpu_cluster() {
    let present: Vec<u16> = (0..4).collect();
    let one = |l: crate::CpuLayout| names(&l) == [("CPU", present.clone())];
    assert!(one(linux::layout_from(&present, None, &[], &[])));
    let equal: Vec<(u16, u32)> = present.iter().map(|&i| (i, 1024)).collect();
    assert!(one(linux::layout_from(&present, None, &[], &equal)));
    let missing = [(0, 1024), (1, 1024), (2, 400)];
    assert!(one(linux::layout_from(&present, None, &[], &missing)));
    let pmu = |n: &str, ids: &[u16]| (n.to_owned(), ids.to_vec());
    let overlap = [pmu("cpu_core", &[0, 1, 2]), pmu("cpu_atom", &[2, 3])];
    assert!(one(linux::layout_from(&present, None, &overlap, &[])));
    let short = [pmu("cpu_core", &[0, 1]), pmu("cpu_atom", &[2])];
    assert!(one(linux::layout_from(&present, None, &short, &[])));
    assert!(one(linux::layout_from(
        &present,
        None,
        &[pmu("cpu_core", &[0, 1, 2, 3])],
        &[]
    )));
    let ranked = [(0, 1024), (1, 1024), (2, 400), (3, 400)];
    assert_eq!(
        names(&linux::layout_from(&present, None, &overlap, &ranked)),
        [("Performance", vec![0, 1]), ("Efficiency", vec![2, 3])]
    );
    let empty = tree(&[]);
    let layout = Root::new(empty.path()).cpu_layout();
    assert_eq!(layout.clusters.len(), 1);
    assert_eq!(layout.clusters[0].name, "CPU");
}

#[test]
fn offline_cores_are_withdrawn() {
    let root = tree(&[
        ("sys/devices/system/cpu/present", "0-7\n"),
        ("sys/devices/system/cpu/online", "0-5\n"),
    ]);
    assert_eq!(Root::new(root.path()).withdrawn(), Some(vec![6, 7]));
    let none = tree(&[("sys/devices/system/cpu/present", "0-7\n")]);
    assert_eq!(
        Root::new(none.path()).withdrawn(),
        None,
        "online unreadable"
    );
}

const PSI_QUIET: &str = "\
some avg10=0.00 avg60=0.12 avg300=0.04 total=1234567
full avg10=0.00 avg60=0.03 avg300=0.01 total=345678
";

#[test]
fn psi_gives_its_averages_and_the_pressure_level() {
    let p = linux::psi(PSI_QUIET).expect("a PSI file");
    assert_eq!((p.some_avg10, p.full_avg10), (0.0, 0.0));
    assert_eq!(linux::psi_level(&p), MemoryLevel::Normal);
    let level = |some: f32, full: f32| {
        linux::psi_level(&linux::Psi {
            some_avg10: some,
            full_avg10: full,
        })
    };
    assert_eq!(level(6.99, 0.0), MemoryLevel::Normal);
    assert_eq!(level(7.0, 0.5), MemoryLevel::Warn);
    assert_eq!(level(80.0, 59.9), MemoryLevel::Warn);
    assert_eq!(level(90.0, 60.0), MemoryLevel::Critical);
    assert_eq!(
        linux::psi("some avg10=1.00 avg60=0 avg300=0 total=0\n"),
        None
    );
    assert_eq!(
        linux::psi("some avg10=x avg60=0 avg300=0 total=0\nfull avg10=0 total=0\n"),
        None
    );
    let empty = tree(&[]);
    assert_eq!(Root::new(empty.path()).psi(), Err(Why::NotEnabled("psi")));
}

const MEMINFO_ZSWAP: &str = "\
MemTotal:       32768000 kB
MemFree:         2048000 kB
MemAvailable:   20480000 kB
Buffers:          512000 kB
Cached:         12288000 kB
Active:          8000000 kB
Inactive:        6000000 kB
SwapTotal:       8388604 kB
SwapFree:        8388604 kB
Zswap:                 0 kB
Zswapped:              0 kB
AnonPages:       9000000 kB
Shmem:           1024000 kB
Slab:             900000 kB
KernelStack:       30000 kB
PageTables:       100000 kB
CommitLimit:    24772604 kB
Committed_AS:   18000000 kB
";

#[test]
fn memory_reads_psi_and_drops_a_disabled_zswap() {
    let root = tree(&[
        ("proc/meminfo", MEMINFO_ZSWAP),
        (
            "proc/pressure/memory",
            "some avg10=12.50 avg60=4.00 avg300=1.00 total=99\nfull avg10=2.00 avg60=1.00 avg300=0.10 total=9\n",
        ),
        ("sys/module/zswap/parameters/enabled", "N\n"),
    ]);
    let r = Root::new(root.path());
    let m = r.memory_detail().expect("a reading");
    assert_eq!(m.pressure, Some(MemoryLevel::Warn));
    assert_eq!((m.compressed, m.stored), (None, None));
    assert_eq!(m.shared, Some(1_024_000 * 1024));
    write(root.path(), "sys/module/zswap/parameters/enabled", "Y\n");
    let m = r.memory_detail().expect("a reading");
    assert_eq!(
        (m.compressed, m.stored),
        (Some(0), Some(0)),
        "on, and empty"
    );
    let no_psi = tree(&[("proc/meminfo", MEMINFO_ZSWAP)]);
    let m = Root::new(no_psi.path()).memory_detail().expect("a reading");
    assert_eq!(m.pressure, None, "no PSI is no level, never normal");
    assert_eq!(Root::new(tree(&[]).path()).memory_detail(), None);
}

#[test]
fn cgroup_files_parse_into_limits() {
    assert_eq!(linux::cpu_max_millicores("max 100000\n"), Some(None));
    assert_eq!(
        linux::cpu_max_millicores("200000 100000\n"),
        Some(Some(2000))
    );
    assert_eq!(linux::cpu_max_millicores("50000 100000"), Some(Some(500)));
    assert_eq!(linux::cpu_max_millicores("100 0"), None);
    assert_eq!(linux::cpu_max_millicores("max"), None);
    assert_eq!(linux::cpu_max_millicores("x 100000"), None);
    assert_eq!(linux::memory_max("max\n"), Some(None));
    assert_eq!(linux::memory_max("8589934592\n"), Some(Some(8 << 30)));
    assert_eq!(linux::memory_max("lots"), None);
    assert_eq!(
        linux::cgroup_v2_path("0::/user.slice/user-1000.slice/session-2.scope\n"),
        Some("/user.slice/user-1000.slice/session-2.scope")
    );
    assert_eq!(linux::cgroup_v2_path("12:cpu,cpuacct:/user.slice\n"), None);
    assert_eq!(linux::cgroup_v2_path("0::/a/../b\n"), None);
    assert_eq!(linux::cgroup_v2_path("0::relative\n"), None);
}

#[test]
fn the_capacity_is_the_tightest_cgroup_limit_on_the_path() {
    let root = tree(&[
        (
            "proc/self/cgroup",
            "0::/user.slice/user-1000.slice/app.slice/aterm.scope\n",
        ),
        ("sys/fs/cgroup/cgroup.controllers", "cpu memory pids\n"),
        ("sys/fs/cgroup/user.slice/cpu.max", "max 100000\n"),
        ("sys/fs/cgroup/user.slice/memory.max", "4294967296\n"),
        (
            "sys/fs/cgroup/user.slice/user-1000.slice/cpu.max",
            "max 100000\n",
        ),
        (
            "sys/fs/cgroup/user.slice/user-1000.slice/app.slice/cpu.max",
            "200000 100000\n",
        ),
        (
            "sys/fs/cgroup/user.slice/user-1000.slice/app.slice/aterm.scope/memory.max",
            "8589934592\n",
        ),
    ]);
    let c = Root::new(root.path()).capacity();
    assert_eq!(c.cpu_millicores, Some(2000));
    assert_eq!(c.mem_bytes, Some(4 << 30));
    assert_eq!(c.source, CapSource::CgroupLimit);

    let container = tree(&[
        ("proc/self/cgroup", "0::/\n"),
        ("sys/fs/cgroup/cgroup.controllers", "cpu memory\n"),
        ("sys/fs/cgroup/cpu.max", "150000 100000\n"),
        ("sys/fs/cgroup/memory.max", "max\n"),
    ]);
    let c = Root::new(container.path()).capacity();
    assert_eq!((c.cpu_millicores, c.mem_bytes), (Some(1500), None));
    assert_eq!(c.source, CapSource::CgroupLimit);

    let v1 = tree(&[
        (
            "proc/self/cgroup",
            "12:cpu,cpuacct:/user.slice\n1:name=systemd:/\n",
        ),
        ("sys/fs/cgroup/cpu.max", "100000 100000\n"),
    ]);
    assert_eq!(Root::new(v1.path()).capacity(), crate::Capacity::default());
    let unmounted = tree(&[
        ("proc/self/cgroup", "0::/\n"),
        ("sys/fs/cgroup/cpu.max", "100000 100000\n"),
    ]);
    assert_eq!(
        Root::new(unmounted.path()).capacity().source,
        CapSource::Host
    );
    let unlimited = tree(&[
        ("proc/self/cgroup", "0::/user.slice\n"),
        ("sys/fs/cgroup/cgroup.controllers", "cpu\n"),
        ("sys/fs/cgroup/user.slice/cpu.max", "max 100000\n"),
    ]);
    assert_eq!(
        Root::new(unlimited.path()).capacity().source,
        CapSource::Host
    );
}

fn zone(temp_c: i64, trips: &[(TripKind, i64)]) -> Zone {
    Zone {
        temp_mc: temp_c * 1000,
        trips: trips.iter().map(|&(k, c)| (k, c * 1000)).collect(),
    }
}

#[test]
fn the_throttle_level_reads_trip_points_and_counters() {
    use TripKind::{Critical, Hot, Passive};
    let level = linux::throttle_level;
    let trips = [(Passive, 95), (Hot, 100), (Critical, 105)];
    assert_eq!(level(&[zone(60, &trips)], None), Ok(ThrottleLevel::Nominal));
    assert_eq!(level(&[zone(96, &trips)], None), Ok(ThrottleLevel::Light));
    assert_eq!(
        level(&[zone(96, &trips)], Some(true)),
        Ok(ThrottleLevel::Heavy)
    );
    assert_eq!(
        level(&[zone(101, &trips)], Some(false)),
        Ok(ThrottleLevel::Heavy)
    );
    assert_eq!(level(&[zone(106, &trips)], None), Ok(ThrottleLevel::Severe));
    assert_eq!(
        level(&[zone(40, &[])], Some(true)),
        Ok(ThrottleLevel::Light)
    );
    assert_eq!(
        level(&[zone(40, &[])], Some(false)),
        Ok(ThrottleLevel::Nominal)
    );
    assert_eq!(level(&[zone(99, &[])], None), Err(Why::Absent));
    assert_eq!(level(&[], None), Err(Why::Absent));
    assert_eq!(linux::trip_kind("passive\n"), Some(Passive));
    assert_eq!(linux::trip_kind("active"), None, "fans are not throttling");
}

#[test]
fn the_throttle_reader_sees_zones_and_a_rise_in_the_counters() {
    let root = tree(&[
        ("sys/devices/system/cpu/present", "0-1\n"),
        ("sys/class/thermal/thermal_zone0/type", "acpitz\n"),
        ("sys/class/thermal/thermal_zone0/temp", "47000\n"),
        (
            "sys/class/thermal/thermal_zone0/trip_point_0_type",
            "critical\n",
        ),
        (
            "sys/class/thermal/thermal_zone0/trip_point_0_temp",
            "119000\n",
        ),
        (
            "sys/class/thermal/thermal_zone0/trip_point_1_type",
            "active\n",
        ),
        (
            "sys/class/thermal/thermal_zone0/trip_point_1_temp",
            "60000\n",
        ),
        (
            "sys/class/thermal/thermal_zone0/trip_point_2_type",
            "passive\n",
        ),
        ("sys/class/thermal/thermal_zone0/trip_point_2_temp", "0\n"),
        ("sys/class/thermal/thermal_zone1/type", "x86_pkg_temp\n"),
        ("sys/class/thermal/thermal_zone1/temp", "52000\n"),
        (
            "sys/class/thermal/thermal_zone1/trip_point_0_type",
            "passive\n",
        ),
        (
            "sys/class/thermal/thermal_zone1/trip_point_0_temp",
            "95000\n",
        ),
        ("sys/class/thermal/cooling_device0/type", "Processor\n"),
        (
            "sys/devices/system/cpu/cpu0/thermal_throttle/core_throttle_count",
            "10\n",
        ),
        (
            "sys/devices/system/cpu/cpu0/thermal_throttle/package_throttle_count",
            "3\n",
        ),
        (
            "sys/devices/system/cpu/cpu1/thermal_throttle/core_throttle_count",
            "8\n",
        ),
    ]);
    let r = Root::new(root.path());
    assert_eq!(
        r.zones(),
        [
            zone(47, &[(TripKind::Critical, 119)]),
            zone(52, &[(TripKind::Passive, 95)])
        ]
    );
    assert_eq!(r.throttle_count(), Some(21));
    let mut reader = ThrottleReader::new(r);
    let level = |reader: &mut ThrottleReader| reader.sample().map(|t| (t.level, t.exact));
    assert_eq!(
        level(&mut reader),
        Ok((ThrottleLevel::Nominal, false)),
        "first"
    );
    write(
        root.path(),
        "sys/devices/system/cpu/cpu1/thermal_throttle/core_throttle_count",
        "9\n",
    );
    assert_eq!(
        level(&mut reader),
        Ok((ThrottleLevel::Light, false)),
        "a rise"
    );
    assert_eq!(
        level(&mut reader),
        Ok((ThrottleLevel::Nominal, false)),
        "still"
    );
    let vm = tree(&[("sys/devices/system/cpu/present", "0-1\n")]);
    assert_eq!(
        ThrottleReader::new(Root::new(vm.path())).sample(),
        Err(Why::Absent)
    );
}

#[test]
fn the_gpu_reads_drm_sysfs_without_waking_a_sleeping_card() {
    let root = tree(&[
        (
            "sys/class/drm/card0/device/uevent",
            "DRIVER=i915\nPCI_ID=8086:46A6\n",
        ),
        (
            "sys/class/drm/card0/device/power/runtime_status",
            "active\n",
        ),
        ("sys/class/drm/card0/device/boot_vga", "1\n"),
        (
            "sys/class/drm/card1/device/uevent",
            "DRIVER=amdgpu\nPCI_ID=1002:73FF\n",
        ),
        (
            "sys/class/drm/card1/device/power/runtime_status",
            "active\n",
        ),
        ("sys/class/drm/card1/device/power/control", "auto\n"),
        ("sys/class/drm/card1/device/boot_vga", "0\n"),
        ("sys/class/drm/card1/device/gpu_busy_percent", "37\n"),
        ("sys/class/drm/card1-DP-1/status", "connected\n"),
        ("sys/class/drm/renderD128/dev", "226:128\n"),
        ("sys/class/drm/version", "drm 1.1.0 20060810\n"),
    ]);
    let read = || Root::new(root.path()).gpu().expect("a GPU");
    let g = read();
    assert_eq!(
        (g.device_pct, g.name.as_deref(), g.unread),
        (None, Some("i915"), Some(Why::NeedsPrivilege("cap_perfmon")))
    );
    for (file, text) in [
        ("power/control", "on\n"),
        ("power/runtime_status", "unsupported\n"),
        ("boot_vga", "1\n"),
    ] {
        write(
            root.path(),
            "sys/class/drm/card1/device/power/control",
            "auto\n",
        );
        write(
            root.path(),
            "sys/class/drm/card1/device/power/runtime_status",
            "active\n",
        );
        write(root.path(), "sys/class/drm/card1/device/boot_vga", "0\n");
        write(
            root.path(),
            &format!("sys/class/drm/card1/device/{file}"),
            text,
        );
        let g = read();
        assert_eq!(
            (g.device_pct, g.name.as_deref(), g.unread),
            (Some(37), Some("amdgpu"), None),
            "{file} {text:?}"
        );
    }
    assert!(linux::pm_settled("active", "on", "0"));
    assert!(!linux::pm_settled("active", "auto", "0"));

    write(
        root.path(),
        "sys/class/drm/card1/device/power/runtime_status",
        "suspended\n",
    );
    write(
        root.path(),
        "sys/class/drm/card1/device/gpu_busy_percent",
        "99\n",
    );
    let g = read();
    assert_eq!(
        (g.device_pct, g.name.as_deref(), g.unread),
        (None, Some("i915"), Some(Why::NeedsPrivilege("cap_perfmon")))
    );
    assert_eq!(
        crate::caps::gpu_cap(Some(&g)),
        Cap::No(Why::NeedsPrivilege("cap_perfmon"))
    );
    write(
        root.path(),
        "sys/class/drm/card0/device/power/runtime_status",
        "suspended\n",
    );
    let g = read();
    assert_eq!((g.device_pct, g.name.as_deref()), (None, Some("i915")));
    assert_eq!(g.unread, Some(Why::Asleep), "known idle, never woken");
    assert_eq!(crate::caps::gpu_cap(Some(&g)), Cap::Yes);
    let lone = tree(&[
        ("sys/class/drm/card0/device/uevent", "DRIVER=amdgpu\n"),
        (
            "sys/class/drm/card0/device/power/runtime_status",
            "suspended\n",
        ),
        ("sys/class/drm/card0/device/gpu_busy_percent", "99\n"),
    ]);
    let g = Root::new(lone.path()).gpu().expect("a GPU");
    assert_eq!((g.device_pct, g.unread), (None, Some(Why::Asleep)));

    let only = |driver: &str| {
        let uevent = format!("DRIVER={driver}\n");
        let t = tree(&[("sys/class/drm/card0/device/uevent", uevent.as_str())]);
        Root::new(t.path()).gpu().and_then(|g| g.unread)
    };
    assert_eq!(only("i915"), Some(Why::NeedsPrivilege("cap_perfmon")));
    assert_eq!(only("xe"), Some(Why::NeedsPrivilege("cap_perfmon")));
    assert_eq!(only("nvidia"), Some(Why::NotRead));
    assert_eq!(only("virtio_gpu"), Some(Why::Unsupported));
    assert_eq!(only("amdgpu"), Some(Why::NotRead), "no PM files: not read");
    assert_eq!(only("simpledrm"), None, "a firmware framebuffer is no GPU");
    assert_eq!(
        only("simple-framebuffer"),
        None,
        "simpledrm, by its platform driver's name"
    );
    let fb = tree(&[("sys/class/drm/card0/device/uevent", "DRIVER=simpledrm\n")]);
    assert_eq!(Root::new(fb.path()).gpu(), None);
    assert_eq!(Root::new(tree(&[]).path()).gpu(), None);
    assert_eq!(crate::caps::gpu_cap(None), Cap::No(Why::Absent));
}

fn nvidia_card(control: &str, boot_vga: &str) -> aterm_tempfile::TempDir {
    tree(&[
        (
            "sys/class/drm/card0/device/uevent",
            "DRIVER=simple-framebuffer\nMODALIAS=platform:simple-framebuffer\n",
        ),
        (
            "sys/class/drm/card1/device/uevent",
            "DRIVER=nvidia\nPCI_CLASS=30000\nPCI_ID=10DE:2E12\n\
             PCI_SUBSYS_ID=10DE:0000\nPCI_SLOT_NAME=000f:01:00.0\n",
        ),
        (
            "sys/class/drm/card1/device/power/runtime_status",
            "active\n",
        ),
        ("sys/class/drm/card1/device/power/control", control),
        ("sys/class/drm/card1/device/boot_vga", boot_vga),
    ])
}

fn nvml_42(slot: &str) -> Result<u8, Why> {
    assert_eq!(slot, "000f:01:00.0", "the card's PCI_SLOT_NAME");
    Ok(42)
}

fn nvml_never(slot: &str) -> Result<u8, Why> {
    panic!("NVML asked for {slot}, a card whose runtime PM a read could move");
}

#[test]
fn an_nvidia_card_reads_through_the_roots_nvml_arm() {
    let gpu = |root: &aterm_tempfile::TempDir, arm: linux::NvidiaBusy| {
        let g = Root::new(root.path())
            .with_nvidia(arm)
            .gpu()
            .expect("a GPU");
        (g.device_pct, g.name, g.unread)
    };
    let nvidia = |pct, unread| (pct, Some("nvidia".to_owned()), unread);
    let spark = nvidia_card("auto\n", "1\n");
    assert_eq!(gpu(&spark, nvml_42), nvidia(Some(42), None));
    let g = Root::new(spark.path())
        .with_nvidia(nvml_42)
        .gpu()
        .expect("a GPU");
    assert_eq!(crate::caps::gpu_cap(Some(&g)), Cap::Yes);
    assert_eq!(gpu(&spark, |_| Ok(250)), nvidia(Some(100), None));
    let failures: [(linux::NvidiaBusy, Why); 3] = [
        (|_| Err(Why::Unsupported), Why::Unsupported),
        (
            |_| Err(Why::NeedsPrivilege("nvml")),
            Why::NeedsPrivilege("nvml"),
        ),
        (|_| Err(Why::NotRead), Why::NotRead),
    ];
    for (arm, why) in failures {
        assert_eq!(gpu(&spark, arm), nvidia(None, Some(why)));
        let g = Root::new(spark.path())
            .with_nvidia(arm)
            .gpu()
            .expect("a GPU");
        assert_eq!(crate::caps::gpu_cap(Some(&g)), Cap::No(why));
    }

    let laptop = nvidia_card("auto\n", "0\n");
    assert_eq!(gpu(&laptop, nvml_never), nvidia(None, Some(Why::NotRead)));
    write(
        laptop.path(),
        "sys/class/drm/card1/device/power/control",
        "on\n",
    );
    assert_eq!(gpu(&laptop, nvml_42), nvidia(Some(42), None));
    write(
        laptop.path(),
        "sys/class/drm/card1/device/power/control",
        "auto\n",
    );
    write(
        laptop.path(),
        "sys/class/drm/card1/device/power/runtime_status",
        "unsupported\n",
    );
    assert_eq!(gpu(&laptop, nvml_42), nvidia(Some(42), None));
    for (file, text) in [
        ("power/runtime_status", "suspended\n"),
        ("boot_vga", "1\n"),
        ("power/control", "on\n"),
    ] {
        write(
            laptop.path(),
            &format!("sys/class/drm/card1/device/{file}"),
            text,
        );
        assert_eq!(
            gpu(&laptop, nvml_never),
            nvidia(None, Some(Why::Asleep)),
            "{file} {text:?}"
        );
    }

    let hybrid = nvidia_card("auto\n", "0\n");
    write(
        hybrid.path(),
        "sys/class/drm/card0/device/uevent",
        "DRIVER=i915\n",
    );
    write(
        hybrid.path(),
        "sys/class/drm/card0/device/power/runtime_status",
        "active\n",
    );
    write(hybrid.path(), "sys/class/drm/card0/device/boot_vga", "1\n");
    assert_eq!(
        gpu(&hybrid, nvml_never),
        (
            None,
            Some("i915".to_owned()),
            Some(Why::NeedsPrivilege("cap_perfmon"))
        )
    );
    write(
        hybrid.path(),
        "sys/class/drm/card1/device/power/control",
        "on\n",
    );
    assert_eq!(gpu(&hybrid, nvml_42), nvidia(Some(42), None));

    let platform = tree(&[
        ("sys/class/drm/card0/device/uevent", "DRIVER=nvidia\n"),
        (
            "sys/class/drm/card0/device/power/runtime_status",
            "active\n",
        ),
        ("sys/class/drm/card0/device/power/control", "on\n"),
    ]);
    assert_eq!(gpu(&platform, nvml_never), nvidia(None, Some(Why::NotRead)));

    let g = Root::new(spark.path()).gpu().expect("a GPU");
    assert_eq!(
        (g.device_pct, g.unread),
        (None, Some(Why::NotRead)),
        "a fixture root never reaches NVML"
    );
    assert_eq!(linux::no_nvml("000f:01:00.0"), Err(Why::NotRead));
}

#[test]
fn a_status_file_gives_anonymous_memory_resident_and_swapped() {
    let status = "Name:\tclaude\nState:\tS (sleeping)\nPPid:\t4100\nVmRSS:\t  412000 kB\n\
                  RssAnon:\t  300000 kB\nRssFile:\t  110000 kB\nRssShmem:\t    2000 kB\n\
                  VmSwap:\t   20000 kB\n";
    assert_eq!(linux::status_footprint(status), Some(320_000 * 1024));
    assert_eq!(
        linux::status_footprint("Name:\tx\nRssAnon:\t 4 kB\n"),
        Some(4096),
        "no VmSwap reads 0 swapped"
    );
    assert_eq!(linux::status_footprint("Name:\tkthreadd\nPPid:\t0\n"), None);
}

#[test]
fn the_sweep_reads_a_fixture_proc() {
    let root = tree(&[
        (
            "proc/1/stat",
            "1 (systemd) S 0 1 1 0 -1 4194560 50000 0 0 0 300 200 0 0 20 0 1 0 5 0 0\n",
        ),
        (
            "proc/1/status",
            "Name:\tsystemd\nRssAnon:\t    8000 kB\nVmSwap:\t       0 kB\n",
        ),
        (
            "proc/2/stat",
            "2 (kthreadd) S 0 0 0 0 -1 2129984 0 0 0 0 0 7 0 0 20 0 1 0 5 0 0\n",
        ),
        (
            "proc/4242/stat",
            "4242 (tmux: server (1)) S 1 4242 4242 0 -1 4194560 1209 0 0 0 37 12 0 0 20 0 1 0 12345 0 0\n",
        ),
        (
            "proc/4242/status",
            "Name:\ttmux: server\nRssAnon:\t    2048 kB\nVmSwap:\t    1024 kB\n",
        ),
        (
            "proc/4300/stat",
            "4300 (zsh) R 4242 4300 4242 34816 4300 4194304 100 0 0 0 5 5 0 0 20 0 1 0 900 0 0\n",
        ),
        ("proc/4300/status", "Name:\tzsh\nRssAnon:\t    1000 kB\n"),
        (
            "proc/4400/stat",
            "4400 (old) S 1 4400 4400 0 -1 4194304 0 0 0 0 2 3 0 0 20 0 1 0 50 0 0\n",
        ),
        ("proc/4400/status", "Name:\told\nPPid:\t1\n"),
        ("proc/4400/statm", "900 250 0 0 0 0 0\n"),
        ("proc/5000/stat", "5000 (cut"),
        ("proc/self/stat", "1 (systemd) S 0 1 1\n"),
        ("proc/meminfo", MEMINFO_ZSWAP),
    ]);
    let mut sweep = Root::new(root.path())
        .processes(100, 4096)
        .expect("a sweep");
    sweep.procs.sort_by_key(|p| p.pid);
    let got: Vec<Row<'_>> = sweep
        .procs
        .iter()
        .map(|p| (p.pid, p.ppid, p.name.as_str(), p.cpu_ns, p.footprint))
        .collect();
    assert_eq!(
        got,
        [
            (1, 0, "systemd", Some(5_000_000_000), Some(8000 * 1024)),
            (2, 0, "kthreadd", Some(70_000_000), Some(0)),
            (
                4242,
                1,
                "tmux: server (1)",
                Some(490_000_000),
                Some(3072 * 1024)
            ),
            (4300, 4242, "zsh", Some(100_000_000), Some(1000 * 1024)),
            (4400, 1, "old", Some(50_000_000), Some(250 * 4096)),
        ]
    );
    let unit: Vec<(u32, RunState, bool, bool)> = sweep
        .procs
        .iter()
        .map(|p| {
            (
                p.pgid,
                p.state,
                p.session_leader.expect("read"),
                p.has_tty.expect("read"),
            )
        })
        .collect();
    assert_eq!(
        unit,
        [
            (1, RunState::Sleeping, true, false),
            (0, RunState::Sleeping, false, false),
            (4242, RunState::Sleeping, true, false),
            (4300, RunState::Running, false, true),
            (4400, RunState::Sleeping, true, false),
        ]
    );
    // The owner is the entry's uid, which only a Unix file system keeps.
    assert_eq!(
        sweep.procs.iter().all(|p| p.uid.is_some()),
        cfg!(unix),
        "{sweep:?}"
    );
    assert!(!sweep.truncated);
    assert_eq!(Root::new(tree(&[]).path()).processes(100, 4096), None);
}

/// Each process's birth is its boot-clock start against `/proc/uptime` and the wall clock read
/// once a sweep. NEGATIVE CONTROL: a tree with no `/proc/uptime` reads no birth.
#[test]
fn the_sweep_reads_each_processs_birth_on_the_boot_clock() {
    const WALL_MS: i64 = 1_790_000_000_000;
    let stat = |pid: u32, start_ticks: u64| {
        format!(
            "{pid} (rustc) R 1 {pid} 1 0 -1 4194304 0 0 0 0 5 5 0 0 20 0 1 0 {start_ticks} 0 0\n"
        )
    };
    let (zsh, rustc) = (stat(4300, 900), stat(4301, 360_900));
    let files = [
        ("proc/4300/stat", zsh.as_str()),
        ("proc/4300/status", "Name:\tzsh\nRssAnon:\t    1000 kB\n"),
        ("proc/4301/stat", rustc.as_str()),
        ("proc/4301/status", "Name:\trustc\nRssAnon:\t    1000 kB\n"),
    ];
    let births = |root: &aterm_tempfile::TempDir| {
        let mut sweep = Root::new(root.path())
            .with_wall_ms(|| Some(WALL_MS))
            .processes(100, 4096)
            .expect("a sweep");
        sweep.procs.sort_by_key(|p| p.pid);
        sweep
            .procs
            .iter()
            .map(|p| (p.pid, p.birth_ms))
            .collect::<Vec<_>>()
    };
    let mut with = files.to_vec();
    with.push(("proc/uptime", "3609.50 7200.00\n"));
    assert_eq!(
        births(&tree(&with)),
        [
            (4300, Some(WALL_MS - 3_600_500)),
            (4301, Some(WALL_MS - 500)),
        ],
        "started 9 s and 3609 s after boot, read 3609.5 s after it"
    );
    assert_eq!(
        births(&tree(&files)),
        [(4300, None), (4301, None)],
        "no boot clock, no birth"
    );
}

#[test]
fn uptime_reads_to_the_nanosecond() {
    assert_eq!(
        linux::parse_uptime_ns("3609.50 7200.00\n"),
        Some(3_609_500_000_000)
    );
    assert_eq!(linux::parse_uptime_ns("12 7\n"), Some(12_000_000_000));
    assert_eq!(linux::parse_uptime_ns("0.000000001"), Some(1));
    assert_eq!(
        linux::parse_uptime_ns("1.0000000001"),
        None,
        "past the nanosecond"
    );
    assert_eq!(linux::parse_uptime_ns("1.-5"), None);
    assert_eq!(linux::parse_uptime_ns(""), None);
    assert_eq!(linux::parse_uptime_ns("18446744074 0"), None, "past u64 ns");
}

#[test]
fn a_long_groups_line_never_cuts_the_footprint() {
    let status = |groups: usize| {
        let ids: Vec<String> = (0..groups).map(|g| (100_000 + g).to_string()).collect();
        format!(
            "Name:\tbuild\nPPid:\t1\nGroups:\t{}\nRssAnon:\t    2048 kB\nVmSwap:\t    1024 kB\n",
            ids.join(" ")
        )
    };
    let stat = "77 (build) S 1 77 77 0 -1 4194560 0 0 0 0 1 1 0 0 20 0 1 0 5 0 0\n";
    let long = status(6000);
    assert!(long.len() > crate::READ_CAP && long.len() < linux::STATUS_CAP);
    let huge = status(linux::STATUS_CAP / 7 + 1);
    assert!(huge.len() > linux::STATUS_CAP);
    let short = status(4);
    for (text, want) in [
        (&short, Some(3072 * 1024)),
        (&long, Some(3072 * 1024)),
        (&huge, None),
    ] {
        let root = tree(&[
            ("proc/77/stat", stat),
            ("proc/77/status", text.as_str()),
            ("proc/77/statm", "9000 500 0 0 0 0 0\n"),
        ]);
        let sweep = Root::new(root.path())
            .processes(100, 4096)
            .expect("a sweep");
        assert_eq!(sweep.procs.len(), 1);
        assert_eq!(
            sweep.procs[0].footprint,
            want,
            "status of {} bytes",
            text.len()
        );
    }
}

#[test]
fn the_capability_report_says_what_each_tree_can_give() {
    let full = tree(&[
        ("proc/stat", "cpu  1 2 3 4\ncpu0 1 2 3 4\nintr 0\n"),
        ("proc/meminfo", MEMINFO_ZSWAP),
        ("proc/pressure/memory", PSI_QUIET),
        (
            "proc/1/stat",
            "1 (systemd) S 0 1 1 0 -1 0 0 0 0 0 1 1 0 0 20 0 1 0 5 0 0\n",
        ),
        ("proc/self/cgroup", "0::/\n"),
        ("sys/fs/cgroup/cgroup.controllers", "cpu memory\n"),
        ("sys/fs/cgroup/cpu.max", "400000 100000\n"),
        ("sys/devices/system/cpu/present", "0\n"),
        ("sys/devices/system/cpu/online", "0\n"),
        ("sys/module/zswap/parameters/enabled", "Y\n"),
        ("sys/class/thermal/thermal_zone0/temp", "40000\n"),
        (
            "sys/class/thermal/thermal_zone0/trip_point_0_type",
            "critical\n",
        ),
        (
            "sys/class/thermal/thermal_zone0/trip_point_0_temp",
            "105000\n",
        ),
        ("sys/class/drm/card0/device/uevent", "DRIVER=amdgpu\n"),
        ("sys/class/drm/card0/device/boot_vga", "1\n"),
        ("sys/class/drm/card0/device/gpu_busy_percent", "3\n"),
    ]);
    let rows = Root::new(full.path()).caps().rows();
    let word = |rows: &[(&str, String)], name: &str| {
        rows.iter()
            .find(|(n, _)| *n == name)
            .map(|(_, w)| w.clone())
            .unwrap_or_default()
    };
    for (name, want) in [
        ("os", "linux"),
        ("per_core", "yes"),
        ("core_kinds", "yes"),
        ("cores_withdrawn", "yes"),
        ("memory_classes", "estimate"),
        ("pressure", "yes"),
        ("commit", "estimate"),
        ("compressor", "yes"),
        ("gpu_device", "yes"),
        ("gpu_per_proc", "unavailable(not_read)"),
        ("proc_cpu_others", "yes"),
        ("throttle", "estimate"),
        ("coalitions", "unavailable(unsupported)"),
        ("capacity", "cgroup_limit"),
    ] {
        assert_eq!(word(&rows, name), want, "{name} of {rows:?}");
    }
    let vm = tree(&[
        ("proc/stat", "cpu  1 2 3 4\ncpu0 1 2 3 4\nintr 0\n"),
        ("proc/meminfo", "MemTotal: 1000 kB\nMemFree: 500 kB\n"),
        ("sys/module/zswap/parameters/enabled", "N\n"),
        ("sys/class/drm/card0/device/uevent", "DRIVER=virtio_gpu\n"),
    ]);
    let rows = Root::new(vm.path()).caps().rows();
    for (name, want) in [
        ("pressure", "unavailable(not_enabled:psi)"),
        ("commit", "unavailable(unsupported)"),
        ("compressor", "unavailable(not_enabled:zswap)"),
        ("gpu_device", "unavailable(unsupported)"),
        ("proc_cpu_others", "estimate"),
        ("throttle", "unavailable(absent)"),
        ("capacity", "host"),
    ] {
        assert_eq!(word(&rows, name), want, "{name} of {rows:?}");
    }
}

#[test]
fn machine_words_are_the_first_cpu_model_and_the_os_pretty_name() {
    let cpuinfo = "processor\t: 0\nvendor_id\t: GenuineIntel\nmodel name\t: Intel(R) Xeon(R) CPU\n";
    let os = "NAME=\"Ubuntu\"\nPRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\n";
    let root = tree(&[("proc/cpuinfo", cpuinfo), ("etc/os-release", os)]);
    let words = Root::new(root.path()).machine_words();
    assert_eq!(words.cpu.as_deref(), Some("Intel(R) Xeon(R) CPU"));
    assert_eq!(words.os.as_deref(), Some("Ubuntu 24.04.1 LTS"));
    assert_eq!(
        linux::cpu_model("Model\t\t: Raspberry Pi 5\n").as_deref(),
        Some("Raspberry Pi 5")
    );
    assert_eq!(linux::cpu_model("processor: 0\n"), None);
    assert_eq!(linux::os_pretty_name("PRETTY_NAME=\"\"\n"), None);
    let empty = tree(&[]);
    assert_eq!(
        Root::new(empty.path()).machine_words(),
        crate::MachineWords::default()
    );
}

/// A clipped protocol table cannot supply a machine total, even when all rows
/// before its cap parse. Exact-cap EOF is complete; one further valid row is not.
#[test]
fn clipped_tcp_tables_cannot_supply_a_complete_count() {
    const HEADER: &str =
        "sl local_address rem_address st tx_queue:rx_queue tr retrnsmt uid timeout inode\n";
    const V4: &str =
        "0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000 1000 0 42\n";
    const V6: &str = "0: 00000000000000000000000001000000:1F90 00000000000000000000000000000000:0000 01 00000000:00000000 00:00000000 00000000 1000 0 43\n";
    let v4 = format!("{HEADER}{V4}");
    let v6 = format!("{HEADER}{V6}");
    let root = tree(&[("proc/net/tcp", &v4), ("proc/net/tcp6", &v6)]);
    let mut scratch = Vec::new();
    let expected = crate::TcpCounts {
        established: 1,
        listen: 1,
        ..crate::TcpCounts::default()
    };
    assert_eq!(
        linux::tcp_counts_under_into(root.path(), &mut scratch),
        Some(expected)
    );

    // Use the actual reader's buffer size, filling it with complete valid rows
    // and then blank lines so a row never straddles the cap in this control.
    let cap = scratch.len();
    let capped = |row: &str| {
        let n = (cap - HEADER.len()) / row.len();
        let mut text = format!("{HEADER}{}", row.repeat(n));
        text.extend(std::iter::repeat_n('\n', cap - text.len()));
        (text, u32::try_from(n).expect("bounded fixture count"))
    };
    let (mut full_v4, listens) = capped(V4);
    write(root.path(), "proc/net/tcp", &full_v4);
    assert_eq!(
        linux::tcp_counts_under_into(root.path(), &mut scratch),
        Some(crate::TcpCounts {
            listen: listens,
            ..expected
        }),
        "EOF exactly at the cap still counts every complete row"
    );
    full_v4.push_str(V4);
    write(root.path(), "proc/net/tcp", &full_v4);
    assert_eq!(
        linux::tcp_counts_under_into(root.path(), &mut scratch),
        None,
        "an unread valid IPv4 row must not become an exact total"
    );

    write(root.path(), "proc/net/tcp", &v4);
    let (mut full_v6, _) = capped(V6);
    full_v6.push_str(V6);
    write(root.path(), "proc/net/tcp6", &full_v6);
    assert_eq!(
        linux::tcp_counts_under_into(root.path(), &mut scratch),
        None,
        "a complete IPv4 table cannot conceal clipped IPv6"
    );
}
