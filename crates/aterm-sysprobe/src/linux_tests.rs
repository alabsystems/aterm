// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Linux arm's readers on fixture trees: each test writes the files a
//! kernel would show under `/proc` and `/sys` (in the kernel's own formats:
//! an Alder Lake laptop's hybrid PMUs, an RK3588 board's and a phone SoC's
//! `cpu_capacity`, a systemd session's and a container's cgroup v2 files,
//! an x86 laptop's thermal zones and throttle counters, an amdgpu and an
//! i915 card) into a scratch root and reads it with the code a Linux build
//! runs on `/`.

use std::path::Path;

use crate::jobs::RunState;
use crate::linux::{self, Root, ThrottleReader, TripKind, Zone};
use crate::{Cap, CapSource, MemoryLevel, ThrottleLevel, Why};

/// One process as the sweep tests compare it: pid, parent, name, CPU
/// time and footprint.
type Row<'a> = (u32, u32, &'a str, Option<u64>, Option<u64>);

/// A scratch root holding `files` (`(path under the root, text)`).
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

/// An Alder Lake laptop (i7-1260P: 4 P-cores with SMT, 8 E-cores): the
/// kernel's hybrid PMUs split the present cores, P first. The plain `cpu`
/// PMU of a non-hybrid part and the `cpu_core` PMU's other files are not
/// read as levels.
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

/// The same laptop with a core taken offline (`echo 0 > cpuN/online`): the
/// PMUs list only online cores, so they partition the online ones, and each
/// offline core keeps its place beside its nearest online neighbour, a P
/// core among the P cores and an E core among the E cores. NEGATIVE
/// CONTROL: an online core no PMU lists still falls through to one cluster.
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

/// ARM boards name no level; `cpu_capacity` ranks them. An RK3588 (four
/// Cortex-A55 at 414, four A76 at 1024) reads two levels, fastest first; a
/// phone SoC's three (160, 498, 1024) reads Performance, Mid, Efficiency.
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

/// A machine whose cores are alike is one `"CPU"` cluster. NEGATIVE
/// CONTROLS: equal capacities; a capacity missing for one core; PMUs that
/// overlap or leave a present core out; a single PMU. Each falls through to
/// the next rule, never to a half-placed layout.
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
    // Overlapping PMUs fall through to the capacities, which still rank.
    let ranked = [(0, 1024), (1, 1024), (2, 400), (3, 400)];
    assert_eq!(
        names(&linux::layout_from(&present, None, &overlap, &ranked)),
        [("Performance", vec![0, 1]), ("Efficiency", vec![2, 3])]
    );
    // A root with no sysfs reads `available_parallelism` cores as one.
    let empty = tree(&[]);
    let layout = Root::new(empty.path()).cpu_layout();
    assert_eq!(layout.clusters.len(), 1);
    assert_eq!(layout.clusters[0].name, "CPU");
}

/// Present but offline cores are the ones the OS withdrew.
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

/// PSI's ten-second averages and the level they map to: warn at lmkd's 7 %
/// partial stall, critical at systemd-oomd's 60 % full stall. NEGATIVE
/// CONTROLS: a file without its `full` line (the `cpu` file before 5.13), a
/// malformed average, and a root without the file (`not_enabled:psi`).
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

/// The root's memory reading adds the PSI level, and a zswap that is off
/// reads no compressed memory (its pool's keys say 0, which is not a
/// measurement of a compressor that is not running).
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

/// cgroup v2's files. NEGATIVE CONTROLS: malformed texts and a zero period.
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

/// The capacity is the tightest limit on the cgroup's path: a 2-core quota
/// on its slice and a 4 GiB memory limit two levels up. Inside a container
/// the namespace's root carries the limit. NEGATIVE CONTROLS: cgroup v1, a
/// v2 path without the unified hierarchy mounted, and no limit anywhere are
/// the host's.
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

/// The heuristic level from trip points and the throttle counters.
/// NEGATIVE CONTROL: no trip point and no counter is `absent`, never `none`.
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

/// The reader on a laptop's tree: zones with trips (an active trip and a
/// disabled 0 °C trip left out), and the counters, whose rise between two
/// readings is a throttle. NEGATIVE CONTROL: a virtual machine's empty tree
/// is `absent`.
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

/// The GPU from DRM sysfs, never waking a card or holding one awake. An
/// awake amdgpu card answers its busy percent only where the read cannot
/// move its runtime PM (forbidden `on`, runtime PM `unsupported`, or the
/// boot display), and is then picked over an i915 iGPU that cannot answer;
/// a hybrid laptop's awake dGPU (`auto`, not the boot display) is not read.
/// A suspended card is asleep and its busy file is not believed, and it
/// stands for the GPU only when every card sleeps: beside an awake iGPU
/// nobody can read, the GPU is unknown, never 0 %. i915 needs
/// `CAP_PERFMON`, NVIDIA's driver NVML, and a virtual GPU has no figure. A
/// firmware framebuffer, a connector and a render node are no GPU.
/// NEGATIVE CONTROL: no card at all is `None`.
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
    // A hybrid laptop's awake dGPU under runtime PM is not read: the iGPU,
    // which cannot answer, is the reading, with its reason.
    let g = read();
    assert_eq!(
        (g.device_pct, g.name.as_deref(), g.unread),
        (None, Some("i915"), Some(Why::NeedsPrivilege("cap_perfmon")))
    );
    // Where the read cannot move its runtime PM, the dGPU answers and is
    // picked over the iGPU.
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

    // The dGPU sleeps beside an awake iGPU nobody can read: unknown, not 0.
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
    // Every card asleep: known idle, and the busy file is not believed.
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
    let fb = tree(&[("sys/class/drm/card0/device/uevent", "DRIVER=simpledrm\n")]);
    assert_eq!(Root::new(fb.path()).gpu(), None);
    assert_eq!(Root::new(tree(&[]).path()).gpu(), None);
    assert_eq!(crate::caps::gpu_cap(None), Cap::No(Why::Absent));
}

/// `status`'s anonymous memory, resident and swapped. NEGATIVE CONTROL: a
/// kernel thread's status has no `RssAnon`.
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

/// One sweep of a fixture `/proc`: parents, names (a `comm` with spaces and
/// parentheses), CPU time at the clock rate, memory from `status` with
/// `statm` for a kernel thread, and what the stuck-process engine forms
/// job units from (state, group, session leadership, a terminal, the
/// owner). A name that is not a pid and a process whose `stat` is cut are
/// left out.
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
        ("proc/2/status", "Name:\tkthreadd\nPPid:\t0\n"),
        ("proc/2/statm", "0 0 0 0 0 0 0\n"),
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
        ]
    );
    assert!(sweep.procs.iter().all(|p| p.uid.is_some()), "{sweep:?}");
    assert!(!sweep.truncated);
    assert_eq!(Root::new(tree(&[]).path()).processes(100, 4096), None);
}

/// A user in thousands of supplementary groups has a `status` whose
/// `Groups:` line pushes the memory lines past one stack buffer: the
/// footprint still counts the swapped pages after it. A status past
/// [`linux::STATUS_CAP`] is no footprint, never a short one, and `statm`'s
/// resident size (another measure) does not stand in. NEGATIVE CONTROL: the
/// same process with a short status reads the same footprint.
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

/// The capability report measured on two trees: a laptop with everything,
/// and a stripped virtual machine where each row says why not.
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
        // An APU: its GPU drives the display, so reading it wakes nothing.
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
