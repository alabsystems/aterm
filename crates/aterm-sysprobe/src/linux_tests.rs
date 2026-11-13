// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Linux arm's readers on fixture trees.

use std::path::Path;

use crate::jobs::RunState;
use crate::linux::{self, Root, ThrottleReader, TripKind, Zone};
use crate::{Cap, CapSource, MemoryLevel, ThrottleLevel, Why};

type Row<'a> = (u32, u32, &'a str, Option<u64>, Option<u64>, Option<u64>);

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

/// The machine's level asks for both PSI's stall and the machine's shortage, held to the live
/// reading that found the gap: one 64 MiB `MemoryHigh` scope on a 16 GiB guest read
/// `some avg10=74.01`, `full avg10=73.81` with `MemAvailable` at 8,626,676 KiB of 16,341,464 KiB,
/// and PSI alone called the machine critical. NEGATIVE CONTROLS: one KiB under a tenth available
/// is the machine's shortage again (PSI decides), and an unread or zero total leaves PSI alone
/// deciding.
#[test]
fn the_machines_level_asks_for_both_the_stall_and_the_shortage() {
    let stalled = linux::Psi {
        some_avg10: 74.01,
        full_avg10: 73.81,
    };
    assert_eq!(linux::psi_level(&stalled), MemoryLevel::Critical);
    assert_eq!(
        linux::machine_pressure(&stalled, Some((16_341_464, 8_626_676))),
        MemoryLevel::Normal,
        "the live guest: a cgroup's limit, 53 % available"
    );
    assert_eq!(
        linux::machine_pressure(&stalled, Some((10_000_000, 1_000_000))),
        MemoryLevel::Normal,
        "exactly a tenth is room"
    );
    assert_eq!(
        linux::machine_pressure(&stalled, Some((10_000_000, 999_999))),
        MemoryLevel::Critical
    );
    let warn = linux::Psi {
        some_avg10: 12.5,
        full_avg10: 2.0,
    };
    assert_eq!(
        linux::machine_pressure(&warn, Some((10_000_000, 500_000))),
        MemoryLevel::Warn
    );
    assert_eq!(
        linux::machine_pressure(&stalled, None),
        MemoryLevel::Critical
    );
    assert_eq!(
        linux::machine_pressure(&stalled, Some((0, 0))),
        MemoryLevel::Critical
    );
    assert_eq!(
        linux::machine_pressure(&stalled, Some((u64::MAX, u64::MAX))),
        MemoryLevel::Normal,
        "no overflow"
    );
    let live = tree(&[
        (
            "proc/pressure/memory",
            "some avg10=74.01 avg60=40.00 avg300=9.00 total=1\nfull avg10=73.81 avg60=39.00 avg300=8.00 total=1\n",
        ),
        (
            "proc/meminfo",
            "MemTotal:       16341464 kB\nMemFree:         8000000 kB\nMemAvailable:    8626676 kB\n",
        ),
    ]);
    let r = Root::new(live.path());
    assert_eq!(r.pressure(), Ok(MemoryLevel::Normal));
    write(
        live.path(),
        "proc/meminfo",
        "MemTotal:       16341464 kB\nMemAvailable:     800000 kB\n",
    );
    assert_eq!(r.pressure(), Ok(MemoryLevel::Critical), "4.9 % available");
    std::fs::remove_file(live.path().join("proc/meminfo")).expect("remove");
    assert_eq!(r.pressure(), Ok(MemoryLevel::Critical), "PSI alone");
    assert_eq!(
        Root::new(tree(&[]).path()).pressure(),
        Err(Why::NotEnabled("psi"))
    );
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
    assert_eq!(
        m.pressure,
        Some(MemoryLevel::Normal),
        "a stall with 62.5 % available is a cgroup's, not the machine's"
    );
    assert_eq!(m.available, 20_480_000 * 1024, "MemAvailable");
    assert_eq!((m.compressed, m.stored), (None, None));
    assert_eq!(m.shared, Some(1_024_000 * 1024));
    let short = MEMINFO_ZSWAP.replace("MemAvailable:   20480000 kB", "MemAvailable:    2048000 kB");
    write(root.path(), "proc/meminfo", &short);
    let m = r.memory_detail().expect("a reading");
    assert_eq!(m.pressure, Some(MemoryLevel::Warn), "6.25 % available");
    assert_eq!(r.pressure(), Ok(MemoryLevel::Warn));
    write(root.path(), "proc/meminfo", MEMINFO_ZSWAP);
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

const VMSTAT_ZSWAP: &str = "\
nr_free_pages 3000000
pgpgin 629812
pswpin 12
pswpout 30
pgmajfault 3271
workingset_refault_file 41446
zswpin 5
zswpout 9
";

/// `/proc/vmstat` read as macOS's VM counters: swap pages scaled by the page size, zswap's loads
/// and stores as the compressor's, major faults as page-ins. NEGATIVE CONTROLS: a kernel without
/// zswap reads 0 compressions, a text without the fault counter is no reading, `page_bytes == 0`
/// is no reading, and a `vmstat` past [`linux::VMSTAT_CAP`] is no reading rather than the counters
/// before its cut.
#[test]
fn vmstat_gives_the_vm_counters() {
    let c = linux::vm_counters(VMSTAT_ZSWAP, 16_384).expect("counters");
    assert_eq!(
        (c.swapin_bytes, c.swapout_bytes, c.page_bytes),
        (12 * 16_384, 30 * 16_384, 16_384)
    );
    assert_eq!((c.decompressions, c.compressions, c.pageins), (5, 9, 3271));
    let no_zswap = VMSTAT_ZSWAP.replace("zswpin 5\nzswpout 9\n", "");
    let c = linux::vm_counters(&no_zswap, 4096).expect("counters");
    assert_eq!((c.decompressions, c.compressions), (0, 0));
    assert_eq!(
        linux::vm_counters(&VMSTAT_ZSWAP.replace("pgmajfault 3271\n", ""), 4096),
        None
    );
    assert_eq!(linux::vm_counters(VMSTAT_ZSWAP, 0), None);

    let root = tree(&[
        ("proc/meminfo", MEMINFO_ZSWAP),
        ("proc/vmstat", VMSTAT_ZSWAP),
    ]);
    let r = Root::new(root.path());
    assert_eq!(r.page_bytes(), 4096, "no auxv in a fixture: the default");
    let m = r.memory_detail().expect("a reading");
    assert_eq!(
        m.counters
            .map(|c| (c.swapin_bytes, c.pageins, c.page_bytes)),
        Some((12 * 4096, 3271, 4096))
    );
    let long: String = (0..2000).map(|i| format!("nr_pad_{i:05} 0\n")).collect();
    write(root.path(), "proc/vmstat", &format!("{long}{VMSTAT_ZSWAP}"));
    assert_eq!(r.vm_counters(), None, "cut at {} bytes", linux::VMSTAT_CAP);
    assert_eq!(r.memory_detail().map(|m| m.counters), Some(None));
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

/// An NVML arm that reads the card busy at `busy`, with no name and no memory fact.
const fn nvml_busy(busy: Result<u8, Why>) -> linux::NvidiaCard {
    linux::NvidiaCard {
        busy,
        power_mw: Err(Why::NotRead),
        name: None,
        unified: false,
    }
}

fn nvml_42(slot: &str) -> linux::NvidiaCard {
    assert_eq!(slot, "000f:01:00.0", "the card's PCI_SLOT_NAME");
    nvml_busy(Ok(42))
}

fn nvml_never(slot: &str) -> linux::NvidiaCard {
    panic!("NVML asked for {slot}, a card whose runtime PM a read could move");
}

#[test]
fn an_nvidia_card_reads_through_the_roots_nvml_arm() {
    let gpu = |root: &aterm_tempfile::TempDir, arm: linux::NvidiaRead| {
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
    assert_eq!(gpu(&spark, |_| nvml_busy(Ok(250))), nvidia(Some(100), None));
    let failures: [(linux::NvidiaRead, Why); 3] = [
        (|_| nvml_busy(Err(Why::Unsupported)), Why::Unsupported),
        (
            |_| nvml_busy(Err(Why::NeedsPrivilege("nvml"))),
            Why::NeedsPrivilege("nvml"),
        ),
        (|_| nvml_busy(Err(Why::NotRead)), Why::NotRead),
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
    assert_eq!(
        linux::no_nvml("000f:01:00.0"),
        linux::NvidiaCard::unread(Why::NotRead)
    );
}

/// A3 L4 (D7): an NVIDIA card NVML reads takes NVML's own name for it (`NVIDIA GB10`), not its
/// DRM driver's, and carries the reader's memory fact, whether NVML read its busy figure or not.
/// NEGATIVE CONTROL: an arm that names nothing, a card NVML is not asked about (its runtime PM
/// could move) and a fixture root's inert arm all leave the driver's name, `nvidia`, and no fact.
#[test]
fn the_nvml_gpu_is_named_by_nvml() {
    fn gb10(slot: &str) -> linux::NvidiaCard {
        assert_eq!(slot, "000f:01:00.0", "the card's PCI_SLOT_NAME");
        linux::NvidiaCard {
            busy: Ok(7),
            power_mw: Ok(31_500),
            name: Some("NVIDIA GB10".into()),
            unified: true,
        }
    }
    fn gb10_unsupported(_: &str) -> linux::NvidiaCard {
        linux::NvidiaCard {
            busy: Err(Why::Unsupported),
            ..gb10("000f:01:00.0")
        }
    }
    let gpu = |root: &aterm_tempfile::TempDir, arm: linux::NvidiaRead| {
        let g = Root::new(root.path())
            .with_nvidia(arm)
            .gpu()
            .expect("a GPU");
        (g.name, g.unified, g.device_pct, g.unread)
    };
    let spark = nvidia_card("auto\n", "1\n");
    assert_eq!(
        gpu(&spark, gb10),
        (Some("NVIDIA GB10".to_owned()), true, Some(7), None)
    );
    assert_eq!(
        gpu(&spark, gb10_unsupported),
        (
            Some("NVIDIA GB10".to_owned()),
            true,
            None,
            Some(Why::Unsupported)
        )
    );
    let driver = |pct, unread| (Some("nvidia".to_owned()), false, pct, unread);
    assert_eq!(gpu(&spark, nvml_42), driver(Some(42), None));
    assert_eq!(
        gpu(&spark, linux::no_nvml),
        driver(None, Some(Why::NotRead))
    );
    let laptop = nvidia_card("auto\n", "0\n");
    assert_eq!(gpu(&laptop, nvml_never), driver(None, Some(Why::NotRead)));
}

/// A4 §4 (N2, heat): an NVIDIA card's board power rides its reading as NVML gave it, or the
/// reason it did not; a card NVML is not asked about (its runtime PM unsettled, or asleep) carries
/// its busy figure's reason. CONTROL: a card no power reader reads (amdgpu here) carries none,
/// never a zero.
#[test]
fn an_nvidia_cards_power_is_nvmls() {
    fn gb10(_: &str) -> linux::NvidiaCard {
        linux::NvidiaCard {
            busy: Ok(7),
            power_mw: Ok(31_500),
            name: Some("NVIDIA GB10".into()),
            unified: true,
        }
    }
    let power = |root: &aterm_tempfile::TempDir, arm: linux::NvidiaRead| {
        Root::new(root.path())
            .with_nvidia(arm)
            .gpu()
            .expect("a GPU")
            .power_mw
    };
    let spark = nvidia_card("auto\n", "1\n");
    assert_eq!(power(&spark, gb10), Some(Ok(31_500)));
    assert_eq!(power(&spark, linux::no_nvml), Some(Err(Why::NotRead)));
    let laptop = nvidia_card("auto\n", "0\n");
    assert_eq!(power(&laptop, nvml_never), Some(Err(Why::NotRead)));
    // Asleep, NVML is not asked either, and the power says why: asleep, as the busy figure does.
    let asleep = tree(&[
        (
            "sys/class/drm/card1/device/uevent",
            "DRIVER=nvidia\nPCI_SLOT_NAME=000f:01:00.0\n",
        ),
        (
            "sys/class/drm/card1/device/power/runtime_status",
            "suspended\n",
        ),
    ]);
    assert_eq!(power(&asleep, nvml_never), Some(Err(Why::Asleep)));
    let amd = tree(&[
        ("sys/class/drm/card0/device/uevent", "DRIVER=amdgpu\n"),
        (
            "sys/class/drm/card0/device/power/runtime_status",
            "active\n",
        ),
        ("sys/class/drm/card0/device/power/control", "on\n"),
        ("sys/class/drm/card0/device/gpu_busy_percent", "37\n"),
    ]);
    let g = Root::new(amd.path()).gpu().expect("a GPU");
    assert_eq!((g.device_pct, g.power_mw), (Some(37), None), "control");
}

#[test]
fn a_status_file_gives_anonymous_memory_resident_and_swapped() {
    let status = "Name:\tclaude\nState:\tS (sleeping)\nPPid:\t4100\nVmRSS:\t  412000 kB\n\
                  RssAnon:\t  300000 kB\nRssFile:\t  110000 kB\nRssShmem:\t    2000 kB\n\
                  VmSwap:\t   20000 kB\n";
    assert_eq!(linux::status_footprint(status), Some(320_000 * 1024));
    assert_eq!(
        linux::status_memory(status),
        Some((320_000 * 1024, 300_000 * 1024)),
        "resident: the anonymous pages in memory, not the swapped ones"
    );
    assert_eq!(
        linux::status_footprint("Name:\tx\nRssAnon:\t 4 kB\n"),
        Some(4096),
        "no VmSwap reads 0 swapped"
    );
    assert_eq!(linux::status_footprint("Name:\tkthreadd\nPPid:\t0\n"), None);
}

/// ONE PROCESS'S FOOTPRINT (`procs::footprint` on Linux, `Root::footprint`) is the sweep's figure:
/// its anonymous pages in memory and swapped (`RssAnon + VmSwap`), read whole however long its
/// `status` is up to the cap. NEGATIVE CONTROL: the file-backed pages it maps are not its own, so
/// `VmRSS + VmSwap` (the explicit-pid reader it replaced) is not what it reads; a `status` past
/// the cap is `InvalidData`, and a kernel thread or a gone pid `NotFound`.
#[test]
fn one_processs_footprint_is_the_sweeps_anonymous_pages_and_swap() {
    let status = "Name:\tclaude\nVmRSS:\t  412000 kB\nRssAnon:\t  300000 kB\n\
                  RssFile:\t  110000 kB\nVmSwap:\t   20000 kB\n";
    let long = format!(
        "Name:\tlong\n{}RssAnon:\t 8 kB\nVmSwap:\t 4 kB\n",
        "x\n".repeat(4096)
    );
    let past = format!("Name:\tpast\n{}", "x".repeat(linux::STATUS_CAP));
    let root = tree(&[
        ("proc/42/status", status),
        ("proc/43/status", &long),
        ("proc/44/status", &past),
        ("proc/2/status", "Name:\tkthreadd\nPPid:\t0\n"),
    ]);
    let r = Root::new(root.path());
    assert_eq!(r.footprint(42).ok(), Some(320_000 * 1024));
    assert_ne!(
        r.footprint(42).ok(),
        Some(432_000 * 1024),
        "not VmRSS + VmSwap"
    );
    assert_eq!(
        r.footprint(43).ok(),
        Some(12 * 1024),
        "read whole past 4 KiB"
    );
    let kind = |pid| r.footprint(pid).map_err(|e| e.kind()).err();
    assert_eq!(kind(44), Some(std::io::ErrorKind::InvalidData));
    assert_eq!(
        kind(2),
        Some(std::io::ErrorKind::NotFound),
        "a kernel thread"
    );
    assert_eq!(kind(99), Some(std::io::ErrorKind::NotFound), "gone");
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
            "Name:\tsystemd\nUid:\t0\t0\t0\t0\nRssAnon:\t    8000 kB\nVmSwap:\t       0 kB\n",
        ),
        (
            "proc/2/stat",
            "2 (kthreadd) S 0 0 0 0 -1 2129984 0 0 0 0 0 7 0 0 20 0 1 0 5 0 0\n",
        ),
        (
            "proc/4242/stat",
            "4242 (tmux: server (1)) S 1 4242 4242 0 -1 4194560 1209 0 0 0 37 12 4 6 20 0 1 0 12345 0 0\n",
        ),
        (
            "proc/4242/status",
            "Name:\ttmux: server\nUid:\t1000\t1000\t1000\t1000\nRssAnon:\t    2048 kB\nVmSwap:\t    1024 kB\n",
        ),
        (
            "proc/4242/io",
            "rchar: 9000\nwchar: 5000\nsyscr: 30\nsyscw: 20\nread_bytes: 8192\nwrite_bytes: 4096\ncancelled_write_bytes: 0\n",
        ),
        // tmux's two threads waited 0.2 s and 0.05 s for a core; its own
        // `schedstat` is the leader's alone and is not read.
        ("proc/4242/schedstat", "400000000 200000000 9\n"),
        ("proc/4242/task/4242/schedstat", "400000000 200000000 9\n"),
        ("proc/4242/task/4243/schedstat", "90000000 50000000 2\n"),
        (
            "proc/4300/stat",
            "4300 (zsh) R 4242 4300 4242 34816 4300 4194304 100 0 0 0 5 5 0 0 20 0 1 0 900 0 0\n",
        ),
        (
            "proc/4300/status",
            "Name:\tzsh\nUid:\t1000\t0\t0\t0\nRssAnon:\t    1000 kB\n",
        ),
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
        .map(|p| {
            (
                p.pid,
                p.ppid,
                p.name.as_str(),
                p.cpu_ns,
                p.child_cpu_ns,
                p.footprint,
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                1,
                0,
                "systemd",
                Some(5_000_000_000),
                Some(0),
                Some(8000 * 1024)
            ),
            (2, 0, "kthreadd", Some(70_000_000), Some(0), Some(0)),
            (
                4242,
                1,
                "tmux: server (1)",
                Some(490_000_000),
                Some(100_000_000),
                Some(3072 * 1024)
            ),
            (
                4300,
                4242,
                "zsh",
                Some(100_000_000),
                Some(0),
                Some(1000 * 1024)
            ),
            (4400, 1, "old", Some(50_000_000), Some(0), Some(250 * 4096)),
        ]
    );
    assert_eq!(
        sweep
            .procs
            .iter()
            .map(|p| (p.pid, p.resident))
            .collect::<Vec<_>>(),
        [
            (1, Some(8000 * 1024)),
            (2, None),
            (4242, Some(2048 * 1024)),
            (4300, Some(1000 * 1024)),
            (4400, None),
        ],
        "RssAnon, the swapped pages left out; none read by statm"
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
    assert_eq!(
        sweep
            .procs
            .iter()
            .map(|p| (p.pid, p.uid))
            .collect::<Vec<_>>(),
        [
            (1, Some(0)),
            (2, Some(0)),
            (4242, Some(1000)),
            (4300, Some(1000)),
            (4400, None),
        ],
        "each uid is status's real Uid (0 for a kernel thread); CONTROL: 4400 has no Uid line and is None even on a Unix file system"
    );
    // `stat`'s thread count, the unit the per-user limit counts here.
    assert!(
        sweep.procs.iter().all(|p| p.threads == Some(1)),
        "{sweep:?}"
    );
    // Storage bytes come from `io`; a process without one reads none, never zero.
    let disk: Vec<(u32, Option<u64>, Option<u64>)> = sweep
        .procs
        .iter()
        .map(|p| (p.pid, p.disk_read, p.disk_written))
        .collect();
    assert_eq!(
        disk,
        [
            (1, None, None),
            (2, None, None),
            (4242, Some(8192), Some(4096)),
            (4300, None, None),
            (4400, None, None),
        ]
    );
    // The runnable time is the CPU time plus every thread's wait; one without `task/*/schedstat` reads none.
    let runnable: Vec<(u32, Option<u64>)> =
        sweep.procs.iter().map(|p| (p.pid, p.runnable_ns)).collect();
    assert_eq!(
        runnable,
        [
            (1, None),
            (2, None),
            (4242, Some(490_000_000 + 250_000_000)),
            (4300, None),
            (4400, None),
        ]
    );
    // Per-user gating: uid 1000 reads 4242's `io` and `runnable_ns`; uid 1001 skips another user's.
    let own = Root::new(root.path())
        .with_euid(Some(1000))
        .processes(100, 4096)
        .expect("a sweep");
    let p4242 = own.procs.iter().find(|p| p.pid == 4242).expect("4242");
    assert_eq!(
        (p4242.disk_read, p4242.disk_written, p4242.runnable_ns),
        (Some(8192), Some(4096), Some(490_000_000 + 250_000_000))
    );
    let other = Root::new(root.path())
        .with_euid(Some(1001))
        .processes(100, 4096)
        .expect("a sweep");
    let p4242_other = other.procs.iter().find(|p| p.pid == 4242).expect("4242");
    assert_eq!(
        (
            p4242_other.disk_read,
            p4242_other.disk_written,
            p4242_other.runnable_ns
        ),
        (None, None, None),
        "NEGATIVE CONTROL: another user's process is not asked"
    );
    assert!(!sweep.truncated);
    assert_eq!(Root::new(tree(&[]).path()).processes(100, 4096), None);
}

/// A `comm` cut at its 15 bytes is read whole from the process's `exe` link (an unlinked image's
/// ` (deleted)` left off) or its `argv[0]`, where either begins with it; a shorter one is whole as
/// it stands; and one neither says whole ends in the mark: a kernel thread's, and one whose
/// arguments were rewritten into a title, which names another file. NEGATIVE CONTROL: the same
/// cut name with no `exe` link or argument words is marked.
#[test]
fn the_sweep_reads_a_cut_comm_whole_or_marks_it() {
    let stat = |pid: u32, comm: &str, flags: u32| {
        format!("{pid} ({comm}) S 1 {pid} {pid} 0 -1 {flags} 0 0 0 0 1 1 0 0 20 0 1 0 50 0 0\n")
    };
    let user = 4_194_304;
    let rows = [
        (
            10,
            "kube-controller",
            user,
            Some("/usr/bin/kube-controller-manager\0--leader\0"),
        ),
        (11, "zsh", user, Some("-zsh\0")),
        (
            12,
            "abcdefghijklmno",
            user,
            Some("abcdefghijklmnop: worker /etc/abcdefghijklmnop.conf\0"),
        ),
        (13, "systemd-journal", user, None),
        (14, "kworker/0:1H-kb", 2_129_984, Some("")),
    ];
    let mut files: Vec<(String, String)> = Vec::new();
    for (pid, comm, flags, cmdline) in rows {
        files.push((format!("proc/{pid}/stat"), stat(pid, comm, flags)));
        files.push((
            format!("proc/{pid}/status"),
            "RssAnon:\t    100 kB\n".to_owned(),
        ));
        if let Some(cmdline) = cmdline {
            files.push((format!("proc/{pid}/cmdline"), cmdline.to_owned()));
        }
    }
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let root = tree(&borrowed);
    let names = |root: &Path| {
        let mut sweep = Root::new(root).processes(100, 4096).expect("a sweep");
        sweep.procs.sort_by_key(|p| p.pid);
        sweep
            .procs
            .into_iter()
            .map(|p| (p.pid, p.name))
            .collect::<Vec<_>>()
    };
    // Only a Unix file system holds the link.
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        "/opt/bin/systemd-journald-extra (deleted)",
        root.path().join("proc/13/exe"),
    )
    .expect("an exe link");
    let journal = if cfg!(unix) {
        "systemd-journald-extra"
    } else {
        "systemd-journal\u{2026}"
    };
    assert_eq!(
        names(root.path()),
        [
            (10, "kube-controller-manager".to_owned()),
            (11, "zsh".to_owned()),
            (12, "abcdefghijklmno\u{2026}".to_owned()),
            (13, journal.to_owned()),
            (14, "kworker/0:1H-kb\u{2026}".to_owned()),
        ]
    );
    let bare = tree(
        &borrowed
            .iter()
            .copied()
            .filter(|(rel, _)| !rel.ends_with("cmdline"))
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        names(bare.path())[0],
        (10, "kube-controller\u{2026}".to_owned()),
        "CONTROL"
    );
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
    assert_eq!(linux::parse_uptime_ns("+12 7\n"), None);
    assert_eq!(linux::parse_uptime_ns("12. 7\n"), None);
    assert_eq!(linux::parse_uptime_ns(".5 7\n"), None);
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

/// An agent's process group on a fixture `/proc` (the memory ceiling's measure,
/// [`crate::group_memory`]): a Claude Code (group leader, 600 MiB anonymous plus 40 MiB swapped)
/// and a helper it forked into its group (100 MiB) are summed; a Bash tool's shell that leads a
/// group of its own, and the 2 GiB build under it, are not the agent's; a zombie member (no
/// `RssAnon`) is skipped, not counted as zero. NEGATIVE CONTROLS: a kernel thread (group 0), a pid
/// that is gone, another user's group, and a group whose one member's `status` is unreadable read
/// `None`.
#[test]
fn a_groups_memory_sums_its_own_members_on_a_fixture_proc() {
    let stat = |pid: u32, name: &str, ppid: u32, pgrp: u32| {
        format!(
            "{pid} ({name}) S {ppid} {pgrp} {pgrp} 34816 {pgrp} 4194560 0 0 0 0 1 1 0 0 20 0 1 0 5 0 0\n"
        )
    };
    let status = |uid: u32, anon_kib: u64, swap_kib: u64| {
        format!(
            "Name:\tx\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\nRssAnon:\t{anon_kib} kB\nVmSwap:\t{swap_kib} kB\n"
        )
    };
    let rows = [
        ("proc/7000/stat", stat(7000, "claude", 4300, 7000)),
        ("proc/7000/status", status(1000, 600 * 1024, 40 * 1024)),
        ("proc/7001/stat", stat(7001, "claude helper", 7000, 7000)),
        ("proc/7001/status", status(1000, 100 * 1024, 0)),
        ("proc/7002/stat", stat(7002, "defunct", 7000, 7000)),
        (
            "proc/7002/status",
            "Name:\tdefunct\nUid:\t1000\t1000\t1000\t1000\nState:\tZ (zombie)\n".to_owned(),
        ),
        ("proc/7100/stat", stat(7100, "zsh", 7000, 7100)),
        ("proc/7100/status", status(1000, 5 * 1024, 0)),
        ("proc/7101/stat", stat(7101, "cargo", 7100, 7100)),
        ("proc/7101/status", status(1000, 2048 * 1024, 0)),
        (
            "proc/2/stat",
            "2 (kthreadd) S 0 0 0 0 -1 2129984 0 0 0 0 0 7 0 0 20 0 1 0 5 0 0\n".to_owned(),
        ),
        ("proc/2/status", "Name:\tkthreadd\nPPid:\t0\n".to_owned()),
        ("proc/8000/stat", stat(8000, "lone", 1, 8000)),
    ];
    let files: Vec<(&str, &str)> = rows.iter().map(|(p, t)| (*p, t.as_str())).collect();
    let root = tree(&files);
    let proc = Root::new(root.path()).with_euid(Some(1000));
    let agent = crate::GroupMemory {
        pgid: 7000,
        footprint_mib: 600 + 40 + 100,
        measured: 2,
    };
    assert_eq!(proc.group_memory(7000), Some(agent), "asked by its leader");
    assert_eq!(proc.group_memory(7001), Some(agent), "asked by a member");
    assert_eq!(
        proc.group_memory(7101),
        Some(crate::GroupMemory {
            pgid: 7100,
            footprint_mib: 5 + 2048,
            measured: 2,
        }),
        "the tool's own group"
    );
    assert_eq!(
        Root::new(root.path())
            .with_euid(Some(1001))
            .group_memory(7000),
        None,
        "NEGATIVE CONTROL: another user's group"
    );
    assert_eq!(proc.group_memory(2), None, "a kernel thread's group 0");
    assert_eq!(proc.group_memory(9999), None, "gone");
    assert_eq!(proc.group_memory(8000), None, "nothing measured");
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

/// THE NETWORK SCOPE COUNTS ONLY TCP SOCKETS (A3 D13): a descriptor counts when it links to a
/// socket the newest TCP dump names; a tty, a pipe, an event queue and a Unix socket do not.
/// NEGATIVE CONTROL: the same fd directory with no socket of it in the dump counts 0, where
/// counting every `socket:[` link counted 2.
#[cfg(unix)]
#[test]
fn the_network_scope_counts_only_tcp_sockets() {
    use std::os::unix::fs::symlink;
    let root = tree(&[]);
    let fd_dir = root.path().join("proc/4242/fd");
    std::fs::create_dir_all(&fd_dir).expect("fd dir");
    symlink("/dev/pts/3", fd_dir.join("0")).expect("tty symlink");
    symlink("pipe:[12345]", fd_dir.join("1")).expect("pipe symlink");
    symlink("socket:[12345]", fd_dir.join("3")).expect("a TCP socket");
    symlink("socket:[67890]", fd_dir.join("4")).expect("a Unix socket");
    symlink("anon_inode:[eventpoll]", fd_dir.join("5")).expect("epoll");
    let tcp = std::collections::HashMap::from([(
        12345,
        crate::jobs::linux::TcpRow {
            inode: 12345,
            state: crate::jobs::TcpState::Established,
            local_port: 1,
            remote: None,
            rx_queue: 0,
        },
    )]);
    let root = Root::new(root.path());
    assert_eq!(root.open_socket_count(4242, &tcp), Some(1));
    assert_eq!(root.open_socket_count(9999, &tcp), None);
    assert_eq!(
        root.open_socket_count(4242, &std::collections::HashMap::new()),
        Some(0)
    );
}

/// THE SOCKET SCOPES COUNT THIS NAMESPACE'S SOCKETS ALONE: their counts are shares of this
/// namespace's TCP total, so a process in another network namespace counts none of its sockets,
/// even once the jobs pass has read that namespace's table, which names them. NEGATIVE CONTROL:
/// the jobs pass's lookup does name the sandbox's listener.
#[cfg(unix)]
#[test]
fn the_socket_scopes_count_this_namespaces_sockets_alone() {
    use std::os::unix::fs::symlink;
    let dir = tree(&[]);
    let at = |p: &str| dir.path().join(p);
    let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 0100007F:0C38 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 55555 1 0\n";
    for pid in ["self", "4343", "4444"] {
        std::fs::create_dir_all(at(&format!("proc/{pid}/ns"))).expect("ns dir");
    }
    symlink("net:[4026531840]", at("proc/self/ns/net")).expect("own namespace");
    symlink("net:[4026532999]", at("proc/4343/ns/net")).expect("a sandbox's namespace");
    symlink("net:[4026531840]", at("proc/4444/ns/net")).expect("this namespace");
    for pid in ["4343", "4444"] {
        std::fs::create_dir_all(at(&format!("proc/{pid}/fd"))).expect("fd dir");
        std::fs::create_dir_all(at(&format!("proc/{pid}/net"))).expect("net dir");
        std::fs::write(at(&format!("proc/{pid}/net/tcp")), table).expect("tcp table");
        symlink("socket:[55555]", at(&format!("proc/{pid}/fd/3"))).expect("a listener");
        symlink("socket:[67890]", at(&format!("proc/{pid}/fd/4"))).expect("a Unix socket");
    }
    let _ = linux::tcp::namespace_sockets(&at("proc/4343"));
    linux::tcp::read_namespaces_now();
    assert!(
        linux::tcp::namespace_sockets(&at("proc/4343")).is_some_and(|t| t.contains_key(&55555)),
        "the jobs pass's lookup names it"
    );
    let root = Root::new(dir.path());
    let none = std::collections::HashMap::new();
    assert_eq!(root.open_socket_count(4343, &none), Some(0));
    assert_eq!(
        root.open_socket_count(4444, &none),
        Some(0),
        "this namespace: the dump's word"
    );
}

/// The sequence the socket-dump fixtures answer.
const DUMP_SEQ: u32 = 7;

/// One netlink message as the kernel writes it, from literal numbers (never the parser's
/// constants, so a wrong constant there fails here): its header (length, `kind`, `flags`, `seq`,
/// port 0), then `payload`, padded to 4 bytes.
fn nlmsg(kind: u16, flags: u16, seq: u32, payload: &[u8]) -> Vec<u8> {
    let len = u32::try_from(16 + payload.len()).expect("a fixture fits a u32");
    let mut m = Vec::new();
    m.extend(len.to_ne_bytes());
    m.extend(kind.to_ne_bytes());
    m.extend(flags.to_ne_bytes());
    m.extend(seq.to_ne_bytes());
    m.extend(0_u32.to_ne_bytes());
    m.extend(payload);
    m.resize(m.len().next_multiple_of(4), 0);
    m
}

/// One attribute: its length (header and value, unpadded), `kind`, `value`, padded to 4 bytes.
fn nlattr(kind: u16, value: &[u8]) -> Vec<u8> {
    let len = u16::try_from(4 + value.len()).expect("a fixture fits a u16");
    let mut a = Vec::new();
    a.extend(len.to_ne_bytes());
    a.extend(kind.to_ne_bytes());
    a.extend(value);
    a.resize(a.len().next_multiple_of(4), 0);
    a
}

/// The name attribute (type 0) of a socket bound at `path`: the kernel's copy of `sun_path`,
/// ending in its NUL.
fn name_attr(path: &[u8]) -> Vec<u8> {
    nlattr(0, &[path, b"\0"].concat())
}

/// The shutdown attribute (type 6, one byte) the kernel adds to every record.
fn shutdown_attr() -> Vec<u8> {
    nlattr(6, &[0])
}

/// A `unix_diag_msg` record of `family`: type 1 (stream), `state`, `inode`, a cookie, then
/// `attrs`, in a message of type 20 (`SOCK_DIAG_BY_FAMILY`) with flag 0x2 (`NLM_F_MULTI`).
fn diag_record(family: u8, state: u8, inode: u32, attrs: &[Vec<u8>]) -> Vec<u8> {
    let mut r = vec![family, 1, state, 0];
    r.extend(inode.to_ne_bytes());
    r.extend([9, 0, 0, 0, 0, 0, 0, 0]);
    r.extend(attrs.concat());
    nlmsg(20, 0x2, DUMP_SEQ, &r)
}

/// A listener (state 10) of family 1 bound at `path`.
fn listener_at(inode: u32, path: &[u8]) -> Vec<u8> {
    diag_record(1, 10, inode, &[name_attr(path), shutdown_attr()])
}

/// The bound file attribute (type 1): the file's inode, then the kernel's device number.
fn vfs_attr(ino: u32, dev: u32) -> Vec<u8> {
    nlattr(1, &[ino.to_ne_bytes(), dev.to_ne_bytes()].concat())
}

/// What the dump decodes a listener bound at `path`, its record naming no file, to.
fn listener(inode: u64, path: &[u8]) -> linux::UnixDiagListener {
    linux::UnixDiagListener {
        inode,
        path: path.to_vec(),
        file: None,
    }
}

/// The dump's end: type 3 with flag 0x2 and the dump's `int` result.
fn dump_done(result: i32) -> Vec<u8> {
    nlmsg(3, 0x2, DUMP_SEQ, &result.to_ne_bytes())
}

/// A dump names each listener bound at a path by its inode and its path's bytes, with the file
/// its bind made where the record names one: a stream at a listener's path (state 1), an abstract
/// listener and a record with no name are left out, a path with a space or with no UTF-8 is kept
/// as its bytes, and a dump runs across datagrams to its end. NEGATIVE CONTROL: read a record of
/// any state, and the stream at the listener's path joins.
#[test]
fn a_socket_dump_names_each_listener_by_its_bound_path() {
    let first = [
        listener_at(41001, b"/run/user/1000/b.sock"),
        diag_record(
            1,
            10,
            41009,
            &[
                name_attr(b"/run/user/1000/b.sock.812.0"),
                vfs_attr(0x1234, (8 << 20) | 0x123),
                shutdown_attr(),
            ],
        ),
        diag_record(
            1,
            10,
            41003,
            &[nlattr(0, b"\0/tmp/.X11-unix/X0"), shutdown_attr()],
        ),
        diag_record(
            1,
            1,
            41002,
            &[name_attr(b"/run/user/1000/b.sock"), shutdown_attr()],
        ),
        listener_at(41005, b"/tmp/a b.sock"),
        listener_at(41007, b"/tmp/\xff.sock"),
    ]
    .concat();
    let second = [diag_record(1, 10, 41008, &[shutdown_attr()]), dump_done(0)].concat();
    let mut into = Vec::new();
    assert_eq!(
        linux::unix_diag_listeners(&first, DUMP_SEQ, &mut into),
        Ok(linux::Dump::More)
    );
    assert_eq!(
        linux::unix_diag_listeners(&second, DUMP_SEQ, &mut into),
        Ok(linux::Dump::Done)
    );
    let sibling = linux::UnixDiagListener {
        file: Some(linux::BoundFile { ino: 0x1234 }),
        ..listener(41009, b"/run/user/1000/b.sock.812.0")
    };
    assert_eq!(
        into,
        [
            listener(41001, b"/run/user/1000/b.sock"),
            sibling,
            listener(41005, b"/tmp/a b.sock"),
            listener(41007, b"/tmp/\xff.sock"),
        ],
        "listeners bound at a path, a stream at that path left out"
    );
}

/// An inode number as `stat(2)` reports one is a bound file's by the low 32 bits the record
/// keeps.
#[test]
fn a_bound_file_is_its_inode_by_the_low_32_bits() {
    let file = linux::BoundFile { ino: 0x1234 };
    assert!(file.is_inode(0x1234));
    assert!(
        file.is_inode(0x7_0000_1234),
        "the record keeps the low 32 bits"
    );
    assert!(!file.is_inode(0x1235));
}

/// A socket name holding a newline and a whole forged `/proc/net/unix` row is one name: the
/// record and each attribute carry their length, so the row names no listener of its own, and an
/// attribute before the name (the shutdown byte, padded to 4) moves nothing. NEGATIVE CONTROL:
/// step over each attribute by its length unpadded, and the name's header is misread.
#[test]
fn a_socket_name_holding_a_newline_is_one_name() {
    let forged: &[u8] = b"/tmp/x\n0: 0 0 10000 1 1 41001 /run/user/1000/b.sock";
    let datagram = diag_record(1, 10, 41010, &[shutdown_attr(), name_attr(forged)]);
    let mut into = Vec::new();
    assert_eq!(
        linux::unix_diag_listeners(&datagram, DUMP_SEQ, &mut into),
        Ok(linux::Dump::More)
    );
    assert_eq!(into, [listener(41010, forged)]);
    assert!(
        !into.iter().any(|l| l.path == b"/run/user/1000/b.sock"),
        "{into:?}"
    );
}

/// A datagram the kernel does not write, or one that says the dump failed or was interrupted,
/// is refused whole and appends nothing, even after a record it read: a dump read in part
/// proves no absence. NEGATIVE CONTROL: accept a message of another sequence, and another
/// request's record reads as this dump's.
#[test]
fn a_dump_read_in_part_is_refused() {
    use linux::DumpFault::{Interrupted, Refused, Torn};
    let good = listener_at(41001, b"/run/b.sock");
    let with_len = |mut m: Vec<u8>, len: u32| {
        m[0..4].copy_from_slice(&len.to_ne_bytes());
        m
    };
    let len_of = |m: &[u8]| u32::try_from(m.len()).expect("a fixture fits a u32");
    let short_msg = nlmsg(20, 0x2, DUMP_SEQ, &[1, 1, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let mut attr_past = diag_record(1, 10, 41001, &[]);
    attr_past.extend(40_u16.to_ne_bytes());
    attr_past.extend(0_u16.to_ne_bytes());
    attr_past.extend(b"/run");
    let attr_past = with_len(attr_past.clone(), len_of(&attr_past));
    let error = |errno: i32| {
        nlmsg(
            2,
            0,
            DUMP_SEQ,
            &[&errno.to_ne_bytes()[..], &good[..16]].concat(),
        )
    };
    let mut interrupted = listener_at(41004, b"/run/c.sock");
    interrupted[6..8].copy_from_slice(&(0x2_u16 | 0x10).to_ne_bytes());
    let cases: Vec<(&str, Vec<u8>, Result<linux::Dump, linux::DumpFault>)> = vec![
        (
            "a length under a header",
            with_len(good.clone(), 12),
            Err(Torn("")),
        ),
        (
            "a length past the datagram",
            with_len(good.clone(), len_of(&good) + 4),
            Err(Torn("")),
        ),
        (
            "another request's record",
            [good.clone(), nlmsg(20, 0x2, DUMP_SEQ + 1, &good[16..])].concat(),
            Err(Torn("")),
        ),
        (
            "a record of another family",
            diag_record(2, 10, 41001, &[name_attr(b"/run/b.sock")]),
            Err(Torn("")),
        ),
        ("a socket record cut short", short_msg, Err(Torn(""))),
        ("an attribute past its record", attr_past, Err(Torn(""))),
        (
            "a bound file attribute of another size",
            diag_record(
                1,
                10,
                41001,
                &[name_attr(b"/run/b.sock"), nlattr(1, &[0; 4])],
            ),
            Err(Torn("")),
        ),
        (
            "an error",
            [good.clone(), error(-1)].concat(),
            Err(Refused(1)),
        ),
        ("an acknowledgement", error(0), Err(Torn(""))),
        (
            "a dump that ended in an error",
            [good.clone(), dump_done(-105)].concat(),
            Err(Refused(105)),
        ),
        (
            "an interrupted dump",
            [good.clone(), interrupted].concat(),
            Err(Interrupted),
        ),
        (
            "an overrun",
            [good.clone(), nlmsg(4, 0, DUMP_SEQ, &[])].concat(),
            Err(Torn("")),
        ),
        (
            "a message of another type",
            nlmsg(16, 0, DUMP_SEQ, &[0; 16]),
            Err(Torn("")),
        ),
        ("an empty datagram", Vec::new(), Err(Torn(""))),
    ];
    for (what, datagram, want) in cases {
        let mut into = Vec::new();
        let got = linux::unix_diag_listeners(&datagram, DUMP_SEQ, &mut into);
        match want {
            Err(Torn(_)) => assert!(matches!(got, Err(Torn(_))), "{what}: {got:?}"),
            want => assert_eq!(got, want, "{what}"),
        }
        assert!(into.is_empty(), "{what}: nothing appended, {into:?}");
    }
    let mut into = Vec::new();
    let noop = nlmsg(1, 0, DUMP_SEQ, &[]);
    assert_eq!(
        linux::unix_diag_listeners(&[noop, good].concat(), DUMP_SEQ, &mut into),
        Ok(linux::Dump::More),
        "a no-op is skipped"
    );
    assert_eq!(into, [listener(41001, b"/run/b.sock")]);
}

/// The request asks the kernel for every `AF_UNIX` socket in state `TCP_LISTEN` with its name and
/// bound file, as a dump, under its sequence. NEGATIVE CONTROL: ask for another state, and the
/// bytes differ.
#[test]
fn the_dump_request_asks_for_every_listener_by_name() {
    let want: Vec<u8> = [
        &40_u32.to_ne_bytes()[..],
        &20_u16.to_ne_bytes(),
        &0x301_u16.to_ne_bytes(),
        &7_u32.to_ne_bytes(),
        &0_u32.to_ne_bytes(),
        &[1, 0],
        &0_u16.to_ne_bytes(),
        &0x400_u32.to_ne_bytes(),
        &0_u32.to_ne_bytes(),
        &3_u32.to_ne_bytes(),
        &0xffff_ffff_u32.to_ne_bytes(),
        &0xffff_ffff_u32.to_ne_bytes(),
    ]
    .concat();
    assert_eq!(linux::unix_diag_request(7).to_vec(), want);
}

#[test]
fn speed_bps_psi_and_zones_reject_plus_prefixed_tokens() {
    assert_eq!(linux::speed_bps("+1000\n"), None);
    assert_eq!(linux::psi("some avg10=+1.25\nfull avg10=0.00\n"), None);
    let root = tree(&[
        ("sys/class/thermal/thermal_zone0/temp", "+45000\n"),
        ("sys/class/thermal/thermal_zone0/trip_point_0_type", "hot\n"),
        (
            "sys/class/thermal/thermal_zone0/trip_point_0_temp",
            "80000\n",
        ),
    ]);
    assert!(Root::new(root.path()).zones().is_empty());
}
