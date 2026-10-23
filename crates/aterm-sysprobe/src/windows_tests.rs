// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for Windows decoders on buffers laid out at SDK offsets.

use crate::windows::{
    self, CpuSet, MemoryStatusEx, PerformanceInformation64, cpu_sets, drive_is_local, drive_root,
    layout_from_cpu_sets, page_files, processes, processor_times, root_wide,
};
use crate::{Commit, CoreTicks, MemoryLevel};

type Row<'a> = (u32, u32, &'a str, Option<u64>, Option<u64>);

fn put_u16(buf: &mut [u8], at: usize, v: u16) {
    buf[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn put_u32(buf: &mut [u8], at: usize, v: u32) {
    buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn put_u64(buf: &mut [u8], at: usize, v: u64) {
    buf[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
fn put_i64(buf: &mut [u8], at: usize, v: i64) {
    buf[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

#[test]
fn processor_times_split_kernel_time_from_idle() {
    let mut buf = vec![0u8; 48 * 3 + 20];
    for (i, (idle, kernel, user)) in [(700, 1000, 500), (9, 4, 2), (-5, 10, -1)]
        .into_iter()
        .enumerate()
    {
        put_i64(&mut buf, i * 48, idle);
        put_i64(&mut buf, i * 48 + 8, kernel);
        put_i64(&mut buf, i * 48 + 16, user);
        put_i64(&mut buf, i * 48 + 24, 77);
    }
    let ticks = |user, system, idle| CoreTicks {
        user,
        system,
        idle,
        nice: 0,
    };
    assert_eq!(
        processor_times(&buf),
        [ticks(500, 300, 700), ticks(2, 0, 9), ticks(0, 10, 0)]
    );
    assert!(processor_times(&buf[..47]).is_empty());
}

fn cpu_set(group: u16, index: u8, efficiency: u8, parked: bool) -> Vec<u8> {
    let mut rec = vec![0u8; 32];
    put_u32(&mut rec, 0, 32);
    put_u32(&mut rec, 4, 0);
    put_u32(&mut rec, 8, 0x100 + u32::from(index));
    put_u16(&mut rec, 12, group);
    rec[14] = index;
    rec[15] = index / 2;
    rec[18] = efficiency;
    rec[19] = u8::from(parked) | 0b10;
    rec
}

#[test]
fn cpu_sets_of_a_hybrid_laptop_split_by_efficiency_class() {
    let mut buf = Vec::new();
    for i in 0..8 {
        buf.extend(cpu_set(0, i, 1, false));
    }
    let mut other = vec![0u8; 40];
    put_u32(&mut other, 0, 40);
    put_u32(&mut other, 4, 1);
    buf.extend(other);
    for i in 8..16 {
        buf.extend(cpu_set(0, i, 0, i == 13));
    }
    let sets = cpu_sets(&buf);
    assert_eq!(sets.len(), 16);
    assert_eq!(
        sets[13],
        CpuSet {
            group: 0,
            index: 13,
            efficiency: 0,
            parked: true
        }
    );
    let (layout, parked) = layout_from_cpu_sets(&sets, &[16]).expect("a layout");
    let got: Vec<(&str, Vec<u16>)> = layout
        .clusters
        .iter()
        .map(|c| (c.name.as_str(), c.logical.clone()))
        .collect();
    assert_eq!(
        got,
        [
            ("Performance", (0..8).collect()),
            ("Efficiency", (8..16).collect())
        ]
    );
    assert_eq!(parked, [13]);
}

#[test]
fn cpu_sets_across_groups_and_cut_buffers() {
    let sets = [
        CpuSet {
            group: 0,
            index: 0,
            efficiency: 0,
            parked: false,
        },
        CpuSet {
            group: 0,
            index: 1,
            efficiency: 0,
            parked: false,
        },
        CpuSet {
            group: 1,
            index: 0,
            efficiency: 0,
            parked: true,
        },
        CpuSet {
            group: 1,
            index: 2,
            efficiency: 0,
            parked: false,
        },
        CpuSet {
            group: 1,
            index: 9,
            efficiency: 0,
            parked: false,
        },
        CpuSet {
            group: 2,
            index: 0,
            efficiency: 0,
            parked: false,
        },
    ];
    let (layout, parked) = layout_from_cpu_sets(&sets, &[2, 3]).expect("a layout");
    assert_eq!(layout.clusters.len(), 1);
    assert_eq!(layout.clusters[0].name, "CPU");
    assert_eq!(layout.clusters[0].logical, [0, 1, 2, 4]);
    assert_eq!(parked, [2]);
    assert_eq!(layout_from_cpu_sets(&[], &[8]), None);

    let mut short = cpu_set(0, 0, 0, false);
    put_u32(&mut short, 0, 19);
    let mut buf = cpu_set(0, 0, 1, false);
    buf.extend(short);
    buf.extend(cpu_set(0, 1, 1, false));
    assert_eq!(cpu_sets(&buf).len(), 1, "a 19-byte record ends the walk");
    let mut long = cpu_set(0, 0, 1, false);
    put_u32(&mut long, 0, 64);
    assert!(cpu_sets(&long).is_empty());
    assert!(cpu_sets(&[]).is_empty());
}

#[test]
fn page_files_sum_every_page_file() {
    let mut buf = vec![0u8; 48 + 32];
    put_u32(&mut buf, 0, 48);
    put_u32(&mut buf, 4, 1_000);
    put_u32(&mut buf, 8, 100);
    put_u32(&mut buf, 12, 400);
    put_u32(&mut buf, 48 + 4, 500);
    put_u32(&mut buf, 48 + 8, 50);
    assert_eq!(page_files(&buf), Some((150, 1_500)));
    assert_eq!(page_files(&[]), Some((0, 0)), "no page file");
    assert_eq!(page_files(&buf[..60]), None);
    put_u32(&mut buf, 48, 4096);
    assert_eq!(page_files(&buf), None);
}

const GIB: u64 = 1 << 30;
const PAGE: u64 = 4096;

fn machine() -> (MemoryStatusEx, PerformanceInformation64) {
    let status = MemoryStatusEx {
        length: 64,
        total_phys: 16 * GIB,
        avail_phys: 6 * GIB,
        ..MemoryStatusEx::default()
    };
    let perf = PerformanceInformation64 {
        cb: 104,
        commit_total: 20 * GIB / PAGE,
        commit_limit: 24 * GIB / PAGE,
        physical_total: 16 * GIB / PAGE,
        physical_available: 6 * GIB / PAGE,
        system_cache: 4 * GIB / PAGE,
        kernel_paged: 768 << 20 >> 12,
        kernel_nonpaged: 256 << 20 >> 12,
        page_size: PAGE,
        ..PerformanceInformation64::default()
    };
    (status, perf)
}

#[test]
fn memory_detail_maps_the_windows_classes() {
    let (status, perf) = machine();
    let swap = Some((2 * GIB / PAGE / 4, 2 * GIB / PAGE));
    let m = windows::memory_detail_of(&status, &perf, swap, false).expect("a reading");
    assert_eq!(m.total, 16 * GIB);
    assert_eq!(m.file_backed, 4 * GIB);
    assert_eq!(m.free, 2 * GIB);
    assert_eq!(m.wired, GIB);
    assert_eq!(m.app, 9 * GIB);
    assert_eq!(m.app + m.wired + m.file_backed + m.free, m.total);
    assert_eq!((m.swap_used, m.swap_total), (GIB / 2, 2 * GIB));
    assert_eq!(
        m.commit,
        Some(Commit {
            charge: 20 * GIB,
            limit: 24 * GIB,
            exact: true
        })
    );
    assert_eq!(m.pressure, Some(MemoryLevel::Normal), "833 permille");
    assert!(!m.exact);
    for class in [
        m.compressed,
        m.stored,
        m.speculative,
        m.active,
        m.inactive,
        m.purgeable,
        m.shared,
    ] {
        assert_eq!(class, None);
    }
    assert_eq!(
        windows::memory_detail_of(&status, &perf, None, false).map(|m| m.swap_total),
        Some(0)
    );
}

#[test]
fn commit_drives_the_windows_pressure_level() {
    let (status, mut perf) = machine();
    let level = |perf: &PerformanceInformation64, low| {
        windows::memory_detail_of(&status, perf, None, low).and_then(|m| m.pressure)
    };
    assert_eq!(
        level(&perf, true),
        Some(MemoryLevel::Warn),
        "low-memory notification"
    );
    perf.commit_total = 22 * GIB / PAGE;
    assert_eq!(level(&perf, false), Some(MemoryLevel::Warn), "916 permille");
    perf.commit_total = 30 * GIB / PAGE;
    assert_eq!(
        level(&perf, false),
        Some(MemoryLevel::Warn),
        "past the limit"
    );
    perf.commit_limit = 0;
    assert_eq!(level(&perf, true), None);
    let c = |charge, limit| Commit {
        charge,
        limit,
        exact: true,
    };
    assert_eq!(
        windows::commit_level(&c(899, 1000), false),
        Some(MemoryLevel::Normal)
    );
    assert_eq!(
        windows::commit_level(&c(900, 1000), false),
        Some(MemoryLevel::Warn)
    );
    assert_eq!(windows::commit_level(&c(1, 0), true), None);

    let (status, mut perf) = machine();
    perf.system_cache = 64 * GIB / PAGE;
    perf.kernel_paged = 64 * GIB / PAGE;
    let m = windows::memory_detail_of(&status, &perf, None, false).expect("a reading");
    assert_eq!((m.file_backed, m.free), (6 * GIB, 0));
    assert_eq!((m.wired, m.app), (10 * GIB, 0));
    perf.page_size = 0;
    assert_eq!(windows::memory_detail_of(&status, &perf, None, false), None);
    let empty = MemoryStatusEx::default();
    assert_eq!(
        windows::memory_detail_of(&empty, &machine().1, None, false),
        None
    );
}

const BASE: u64 = 0x7FF6_1000_0000;

struct Proc<'a> {
    pid: u64,
    ppid: u64,
    created: i64,
    user: i64,
    kernel: i64,
    private: u64,
    name: Option<&'a str>,
}

fn process_buffer(procs: &[Proc<'_>]) -> Vec<u8> {
    let records = procs.len() * 256;
    let mut buf = vec![0u8; records];
    for (i, p) in procs.iter().enumerate() {
        let at = i * 256;
        let next = if i + 1 == procs.len() { 0 } else { 256 };
        put_u32(&mut buf, at, next);
        put_u32(&mut buf, at + 4, 3);
        put_i64(&mut buf, at + 32, p.created);
        put_i64(&mut buf, at + 40, p.user);
        put_i64(&mut buf, at + 48, p.kernel);
        put_u64(&mut buf, at + 80, p.pid);
        put_u64(&mut buf, at + 88, p.ppid);
        put_u64(&mut buf, at + 200, p.private);
        if let Some(name) = p.name {
            let wide: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let off = buf.len();
            let len = u16::try_from(wide.len()).expect("a short name");
            put_u16(&mut buf, at + 56, len);
            put_u16(&mut buf, at + 58, len + 2);
            put_u64(
                &mut buf,
                at + 64,
                BASE + u64::try_from(off).expect("an offset"),
            );
            buf.extend(wide);
            buf.extend([0, 0]);
        }
    }
    buf
}

#[test]
fn the_process_sweep_decodes_names_times_and_parents() {
    let p = |pid, ppid, created, name| Proc {
        pid,
        ppid,
        created,
        user: 0,
        kernel: 0,
        private: 0,
        name,
    };
    let buf = process_buffer(&[
        Proc {
            kernel: 9_999_999,
            ..p(0, 0, 0, None)
        },
        p(4, 0, 10, Some("System")),
        p(1200, 1100, 1000, Some("explorer.exe")),
        Proc {
            user: 3_000_000,
            kernel: 1_000_000,
            private: 200 << 20,
            ..p(5000, 1200, 2000, Some("Code.EXE"))
        },
        p(6000, 7000, 3000, Some("cmd.exe")),
        p(7000, 4, 9000, Some("conhost.exe")),
        p(8000, 4, 9500, Some(".exe")),
    ]);
    let sweep = processes(&buf, BASE);
    assert!(!sweep.truncated);
    let got: Vec<Row<'_>> = sweep
        .procs
        .iter()
        .map(|p| (p.pid, p.ppid, p.name.as_str(), p.cpu_ns, p.footprint))
        .collect();
    assert_eq!(
        got,
        [
            (4, 0, "System", Some(0), Some(0)),
            (1200, 1100, "explorer", Some(0), Some(0)),
            (5000, 1200, "Code", Some(400_000_000), Some(200 << 20)),
            (6000, 0, "cmd", Some(0), Some(0)),
            (7000, 4, "conhost", Some(0), Some(0)),
            (8000, 4, ".exe", Some(0), Some(0)),
        ]
    );
    for p in &sweep.procs {
        assert_eq!(
            (p.pgid, p.uid, p.state, p.session_leader, p.has_tty),
            (0, None, crate::jobs::RunState::Unknown, None, None),
            "no Unix job facts on Windows: {p:?}"
        );
    }
}

#[test]
fn a_bad_process_buffer_reads_no_garbage() {
    let one = |name| Proc {
        pid: 42,
        ppid: 4,
        created: 1,
        user: 1,
        kernel: 1,
        private: 4096,
        name,
    };
    let buf = process_buffer(&[one(Some("aterm.exe"))]);
    assert_eq!(processes(&buf, BASE).procs[0].name, "aterm");
    assert_eq!(processes(&buf, BASE + 0x1000).procs[0].name, "");
    assert_eq!(processes(&buf, 0).procs[0].name, "");
    assert!(processes(&buf[..255], BASE).procs.is_empty());
    let mut two = process_buffer(&[one(None), one(None)]);
    put_u32(&mut two, 0, 16);
    assert_eq!(processes(&two, BASE).procs.len(), 1);
}

#[test]
fn drives_are_judged_from_the_spelling_and_the_drive_type() {
    for (path, want) in [
        (r"C:\Users\alice\aterm", Some('C')),
        ("c:/Users", Some('C')),
        (r"\\?\D:\very\long\path", Some('D')),
        (r"\\server\share\x", None),
        (r"\\?\UNC\server\share", None),
        (r"\\?\Volume{1b3a2c4d-0000-0000-0000-100000000000}\", None),
        (r"\\.\PhysicalDrive0", None),
        ("C:relative", None),
        (r"relative\path", None),
        ("1:\\", None),
        ("", None),
    ] {
        assert_eq!(drive_root(path), want, "{path}");
    }
    assert!(drive_is_local(windows::DRIVE_FIXED));
    assert!(drive_is_local(windows::DRIVE_RAMDISK));
    for kind in [0, 1, 2, 4, 5] {
        assert!(!drive_is_local(kind), "{kind}");
    }
    assert_eq!(root_wide('D'), [68, 58, 92, 0]);
}
