// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for Windows decoders on buffers laid out at SDK offsets.

use std::collections::HashMap;

use crate::jobs::RunState;
use crate::windows::{
    self, CpuSet, MemoryStatusEx, PerformanceInformation64, SessionClass, console_host_pid,
    cpu_sets, drive_is_local, drive_root, layout_from_cpu_sets, os_build, os_words, page_files,
    processes, processor_times, root_wide, session_class, tree, tree_adopting, tree_memory,
    tree_usage,
};
use crate::{
    Commit, CoreTicks, GroupMemory, MAX_GROUP_PIDS, MemberUsage, MemoryLevel, ProcessReading,
};

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
    assert_eq!(m.available, status.avail_phys, "Windows' ullAvailPhys");
    assert_ne!(m.available, m.free, "not the free pages alone");
    assert_eq!(m.used(), m.app + m.wired);
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
            (0, Some(0), RunState::Unknown, None, None),
            "no Unix job facts on Windows; session 0 owns it; no thread records, no state: {p:?}"
        );
    }
}

/// Each process's birth is its creation `FILETIME` in unix ms. NEGATIVE CONTROL: a creation
/// time before the unix epoch (or none) reads no birth.
#[test]
fn the_process_sweep_reads_each_birth_from_its_creation_time() {
    const UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;
    let p = |pid, created| Proc {
        pid,
        ppid: 4,
        created,
        user: 0,
        kernel: 0,
        private: 0,
        name: Some("rustc.exe"),
    };
    let buf = process_buffer(&[
        p(5000, UNIX_EPOCH_TICKS + 17_900_000_000_000_000),
        p(5001, 1000),
        p(5002, 0),
    ]);
    let births: Vec<(u32, Option<i64>)> = processes(&buf, BASE)
        .procs
        .iter()
        .map(|p| (p.pid, p.birth_ms))
        .collect();
    assert_eq!(
        births,
        [(5000, Some(1_790_000_000_000)), (5001, None), (5002, None)]
    );
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

/// One process entry as the kernel lays it out: the 256-byte record, its thread records
/// (`(state, wait reason)` each), then its image name; `next` is the whole entry's length, 0 for
/// the last.
struct Entry<'a> {
    pid: u64,
    ppid: u64,
    created: i64,
    session: u32,
    working_set: u64,
    private: u64,
    peak: u64,
    read: i64,
    written: i64,
    threads: &'a [(u32, u32)],
}

fn entries(list: &[Entry<'_>]) -> Vec<u8> {
    let mut buf = Vec::new();
    for (i, e) in list.iter().enumerate() {
        let at = buf.len();
        let len = 256 + e.threads.len() * 80;
        buf.resize(at + len, 0);
        let next = if i + 1 == list.len() { 0 } else { len };
        put_u32(
            &mut buf,
            at,
            u32::try_from(next).expect("an entry's length"),
        );
        put_u32(
            &mut buf,
            at + 4,
            u32::try_from(e.threads.len()).expect("a thread count"),
        );
        put_i64(&mut buf, at + 32, e.created);
        put_u64(&mut buf, at + 80, e.pid);
        put_u64(&mut buf, at + 88, e.ppid);
        put_u32(&mut buf, at + 100, e.session);
        put_u64(&mut buf, at + 136, 0xBAD0_0000);
        put_u64(&mut buf, at + 144, e.working_set);
        put_u64(&mut buf, at + 184, 0xBAD0_0001);
        put_u64(&mut buf, at + 192, e.peak);
        put_u64(&mut buf, at + 200, e.private);
        for (slot, v) in [11, 12, 13, e.read, e.written, 14].into_iter().enumerate() {
            put_i64(&mut buf, at + 208 + slot * 8, v);
        }
        for (t, (state, reason)) in e.threads.iter().enumerate() {
            let th = at + 256 + t * 80;
            put_u64(&mut buf, th + 40, e.pid);
            put_u32(&mut buf, th + 68, *state);
            put_u32(&mut buf, th + 72, *reason);
        }
    }
    buf
}

const READY: (u32, u32) = (1, 0);
const RUNNING: (u32, u32) = (2, 0);
const WAITING: (u32, u32) = (5, 6);
const SUSPENDED: (u32, u32) = (5, 5);

fn entry(pid: u64, ppid: u64, created: i64) -> Entry<'static> {
    Entry {
        pid,
        ppid,
        created,
        session: 1,
        working_set: 0,
        private: 0,
        peak: 0,
        read: 0,
        written: 0,
        threads: &[WAITING],
    }
}

/// What the sweep keeps of one row beside its CPU time: pid, owner, working set, commit, peak
/// commit, bytes read and bytes written through any handle, and the disk bytes read and
/// written.
type Kept = (
    u32,
    Option<u32>,
    Option<u64>,
    Option<u64>,
    Option<u64>,
    Option<u64>,
    Option<u64>,
    (Option<u64>, Option<u64>),
);

/// The sweep keeps what a process record holds beside its CPU time and commit: its session as
/// its owner, its working set, its peak commit, and the bytes it read and wrote (the transfer
/// counts, never the operation counts beside them) as bytes moved through any handle: a
/// program printing 9 GB to its terminal has written no disk, so the disk figures stay unread
/// (before 2026-11-11 they held the transfer counts, and the Disk lens named a chatty tab a
/// writer). NEGATIVE CONTROL: the peak working set and
/// the page-file usage, the fields next to the two memory figures, hold marker values no row
/// may show, and a negative transfer count (never the kernel's) reads 0, not a huge figure.
#[test]
fn the_process_sweep_keeps_session_memory_and_io() {
    let buf = entries(&[
        Entry {
            session: 0,
            working_set: 8 << 20,
            private: 1 << 20,
            peak: 2 << 20,
            ..entry(4, 0, 10)
        },
        Entry {
            session: 3,
            working_set: 300 << 20,
            private: 200 << 20,
            peak: 450 << 20,
            read: 7_000,
            written: 9_000_000_000,
            ..entry(5000, 4, 2000)
        },
        Entry {
            read: -1,
            written: -9,
            ..entry(5004, 5000, 3000)
        },
    ]);
    let sweep = processes(&buf, BASE);
    let got: Vec<Kept> = sweep
        .procs
        .iter()
        .map(|p| {
            (
                p.pid,
                p.uid,
                p.resident,
                p.footprint,
                p.peak_footprint,
                p.io_read,
                p.io_written,
                (p.disk_read, p.disk_written),
            )
        })
        .collect();
    let mib = |n: u64| Some(n << 20);
    assert_eq!(
        got,
        [
            (
                4,
                Some(0),
                mib(8),
                mib(1),
                mib(2),
                Some(0),
                Some(0),
                (None, None)
            ),
            (
                5000,
                Some(3),
                mib(300),
                mib(200),
                mib(450),
                Some(7_000),
                Some(9_000_000_000),
                (None, None)
            ),
            (
                5004,
                Some(1),
                Some(0),
                Some(0),
                Some(0),
                Some(0),
                Some(0),
                (None, None)
            ),
        ]
    );
    for p in &sweep.procs {
        assert_eq!(
            (p.sid, p.pgid),
            (0, 0),
            "a Windows session is no Unix session id: {p:?}"
        );
    }
}

/// A process's state is read from its threads: running with any thread on a core or ready for
/// one, stopped with every thread suspended, sleeping otherwise. NEGATIVE CONTROLS: one waiting
/// thread among suspended ones is not a stopped process; a record that says more threads than
/// its entry holds reads no state, never the next process's bytes as threads.
#[test]
fn the_process_sweep_reads_each_state_from_its_threads() {
    let with = |pid: u64, threads: &'static [(u32, u32)]| Entry {
        threads,
        ..entry(pid, 4, 100)
    };
    let mut buf = entries(&[
        with(10, &[WAITING, RUNNING]),
        with(11, &[WAITING, READY, SUSPENDED]),
        with(12, &[SUSPENDED, SUSPENDED]),
        with(13, &[SUSPENDED, (5, 12)]),
        with(14, &[SUSPENDED, WAITING]),
        with(15, &[WAITING, (5, 0)]),
        with(16, &[(3, 0)]),
        with(17, &[(7, 0)]),
        with(18, &[]),
        with(19, &[SUSPENDED]),
        with(20, &[SUSPENDED]),
    ]);
    let states = |buf: &[u8]| -> Vec<(u32, RunState)> {
        processes(buf, BASE)
            .procs
            .iter()
            .map(|p| (p.pid, p.state))
            .collect()
    };
    assert_eq!(
        states(&buf),
        [
            (10, RunState::Running),
            (11, RunState::Running),
            (12, RunState::Stopped),
            (13, RunState::Stopped),
            (14, RunState::Sleeping),
            (15, RunState::Sleeping),
            (16, RunState::Running),
            (17, RunState::Running),
            (18, RunState::Unknown),
            (19, RunState::Stopped),
            (20, RunState::Stopped),
        ]
    );
    // Pid 19's record now claims two threads where its entry holds one: the second would be
    // pid 20's record. Pid 20, the last entry, claims two where the buffer ends after one.
    let at_19 = buf.len() - 2 * (256 + 80);
    put_u32(&mut buf, at_19 + 4, 2);
    let at_20 = buf.len() - (256 + 80);
    put_u32(&mut buf, at_20 + 4, 2);
    let cut = states(&buf);
    assert_eq!(cut[9], (19, RunState::Unknown));
    assert_eq!(cut[10], (20, RunState::Unknown));
    assert_eq!(windows::run_state(&[0u8; 79]), RunState::Unknown);
}

#[test]
fn a_session_is_the_services_this_persons_or_anothers() {
    assert_eq!(session_class(0, 1), SessionClass::Services);
    assert_eq!(session_class(1, 1), SessionClass::Own);
    assert_eq!(session_class(2, 1), SessionClass::Other);
    assert_eq!(
        session_class(0, 0),
        SessionClass::Services,
        "a reader that is itself a service"
    );
}

fn row(pid: u32, ppid: u32, footprint_mib: u64) -> ProcessReading {
    ProcessReading {
        pid,
        ppid,
        start: Some(u64::from(pid) * 10),
        cpu_ns: Some(u64::from(pid)),
        footprint: Some(footprint_mib << 20),
        peak_footprint: Some((footprint_mib + 1) << 20),
        io_read: Some(3),
        io_written: Some(4),
        ..ProcessReading::default()
    }
}

fn pids(members: &[&ProcessReading]) -> Vec<u32> {
    members.iter().map(|p| p.pid).collect()
}

/// A tree is its root and every live descendant, breadth first. NEGATIVE CONTROLS: a sibling
/// tree, a process whose parent pid is its own, a parent cycle and a root that is not running
/// add nothing; pid 0, which every orphan names, roots no tree.
#[test]
fn a_tree_is_the_root_and_its_live_descendants() {
    let procs = [
        row(4, 0, 1),
        row(100, 4, 10),
        row(110, 100, 20),
        row(111, 110, 30),
        row(120, 100, 40),
        row(200, 4, 500),
        row(210, 200, 600),
        row(300, 300, 7),
        row(400, 410, 8),
        row(410, 400, 9),
        row(500, 0, 11),
    ];
    assert_eq!(pids(&tree(&procs, 100)), [100, 110, 120, 111]);
    assert_eq!(pids(&tree(&procs, 111)), [111]);
    assert_eq!(pids(&tree(&procs, 300)), [300]);
    assert_eq!(pids(&tree(&procs, 400)), [400, 410]);
    assert!(tree(&procs, 999).is_empty());
    assert!(tree(&procs, 0).is_empty());
    assert_eq!(
        tree_memory(&procs, 100, |_| None),
        Some(GroupMemory {
            pgid: 100,
            footprint_mib: 100,
            measured: 4
        })
    );
    assert_eq!(tree_memory(&procs, 999, |_| None), None);
    assert_eq!(tree_usage(&procs, 999, |_| None), None);
    assert_eq!(
        tree_usage(&procs, 110, |_| None),
        Some(vec![
            MemberUsage {
                pid: 110,
                start: 1100,
                cpu_ns: 110,
                runnable_ns: 0,
                io_bytes: 7,
                footprint: 20 << 20,
                peak_footprint: 21 << 20,
            },
            MemberUsage {
                pid: 111,
                start: 1110,
                cpu_ns: 111,
                runnable_ns: 0,
                io_bytes: 7,
                footprint: 30 << 20,
                peak_footprint: 31 << 20,
            },
        ])
    );
}

/// The sweep's creation-time check reaches the tree: a process whose parent pid now names a
/// process created after it was handed a reused pid, and is not that process's child.
/// NEGATIVE CONTROL: the same rows with the reused pid's process older than the row are one
/// tree.
#[test]
fn a_reused_parent_pid_adopts_nothing() {
    let sweep = |agent_created| {
        processes(
            &entries(&[
                Entry {
                    private: 100 << 20,
                    ..entry(700, 4, agent_created)
                },
                Entry {
                    private: 50 << 20,
                    ..entry(710, 700, 5_000)
                },
                Entry {
                    private: 900 << 20,
                    ..entry(720, 700, 2_000)
                },
            ]),
            BASE,
        )
    };
    let reused = sweep(3_000);
    assert_eq!(pids(&tree(&reused.procs, 700)), [700, 710]);
    assert_eq!(
        tree_memory(&reused.procs, 700, |_| None).map(|m| (m.footprint_mib, m.measured)),
        Some((150, 2)),
        "pid 720 was created before the process now at pid 700"
    );
    let older = sweep(1_000);
    assert_eq!(pids(&tree(&older.procs, 700)), [700, 710, 720]);
    assert_eq!(
        tree_memory(&older.procs, 700, |_| None).map(|m| m.footprint_mib),
        Some(1050)
    );
}

#[test]
fn a_tree_stops_at_the_group_cap() {
    let mut procs = vec![row(1000, 4, 1)];
    procs.extend((0..400).map(|i| row(2000 + i, 1000, 1)));
    let members = tree(&procs, 1000);
    assert_eq!(members.len(), MAX_GROUP_PIDS);
    assert_eq!(members[0].pid, 1000);
    assert_eq!(
        tree_memory(&procs, 1000, |_| None).map(|m| m.measured),
        Some(u32::try_from(MAX_GROUP_PIDS).expect("a small cap"))
    );
}

/// A console host's pid is the answer less its two flag bits. NEGATIVE CONTROLS: the flags
/// alone (a process with no console) and an answer past a pid's range name no host.
#[test]
fn a_console_hosts_pid_is_the_answer_less_its_flags() {
    assert_eq!(console_host_pid(11_273), Some(11_272));
    assert_eq!(console_host_pid(7_604), Some(7_604));
    assert_eq!(console_host_pid(7_606), Some(7_604));
    assert_eq!(console_host_pid(0), None);
    assert_eq!(console_host_pid(1), None);
    assert_eq!(console_host_pid(3), None);
    assert_eq!(console_host_pid(1 << 40), None);
}

/// The rows of an agent in a tab, as Git Bash leaves them: the tab's console host (10) and its
/// shell (20), the agent (100, created at 1000) with a command's shell (110) that made its own
/// console (host 112, its child), and what that shell's scripts left behind once the processes
/// that started them exited: 130 and 140 (parents 9001 and 9002, gone) with 131 under 130.
/// Beside them: 150, orphaned in the tab's own console after the agent started; 160, orphaned
/// there before it; 170, orphaned in another tab's console (host 12); 180, orphaned with no
/// console; and 190 with 191, another tab's live tree.
fn orphaned_rows() -> (Vec<ProcessReading>, HashMap<u32, u32>) {
    let at = |pid: u32, ppid: u32, start: u64| ProcessReading {
        start: Some(start),
        ..row(pid, ppid, 1)
    };
    let procs = vec![
        at(10, 4, 100),
        at(12, 4, 110),
        at(20, 10, 200),
        at(100, 20, 1000),
        at(110, 100, 1100),
        at(112, 110, 1101),
        at(130, 9001, 1200),
        at(131, 130, 1210),
        at(140, 9002, 1300),
        at(150, 9003, 1400),
        at(160, 9004, 900),
        at(170, 9005, 1500),
        at(180, 9006, 1600),
        at(190, 12, 1700),
        at(191, 190, 1710),
    ];
    let consoles = HashMap::from([
        (20, 10),
        (100, 10),
        (130, 112),
        (131, 112),
        (140, 112),
        (150, 10),
        (160, 10),
        (170, 12),
        (190, 12),
        (191, 12),
    ]);
    (procs, consoles)
}

/// An orphan is counted with the tree whose console it was started in: one attached to a
/// console a member created, and one attached to the root's own console since the root
/// started, each with its descendants. NEGATIVE CONTROLS: the parent chain alone finds none of
/// them (the defect: a script's processes under Git Bash were never measured); an orphan older
/// than the root, one in another tab's console and one with no console are not counted, nor
/// is another tab's live tree; and a console read that answers nothing adopts nothing.
#[test]
fn an_orphan_is_counted_by_the_console_it_shares_with_the_tree() {
    let (procs, consoles) = orphaned_rows();
    let mut asked = Vec::new();
    let members = tree_adopting(&procs, 100, |p| {
        asked.push(p.pid);
        consoles.get(&p.pid).copied()
    });
    assert_eq!(pids(&members), [100, 110, 112, 130, 131, 140, 150]);
    assert_eq!(
        asked,
        [100, 130, 140, 150, 170, 180],
        "the root, then each orphan created since it: never a member, a row with a live \
         parent or an orphan older than the root"
    );
    assert_eq!(pids(&tree(&procs, 100)), [100, 110, 112]);
    assert_eq!(pids(&tree_adopting(&procs, 100, |_| None)), [100, 110, 112]);
    assert_eq!(
        tree_memory(&procs, 100, |p| consoles.get(&p.pid).copied()).map(|m| m.measured),
        Some(7)
    );
    assert_eq!(
        tree_usage(&procs, 100, |p| consoles.get(&p.pid).copied())
            .map(|members| members.iter().map(|m| m.pid).collect::<Vec<_>>()),
        Some(vec![100, 110, 112, 130, 131, 140, 150])
    );
    // The command's shell as the root: its own console's orphans, and none of the tab's.
    assert_eq!(
        pids(&tree_adopting(&procs, 110, |p| consoles
            .get(&p.pid)
            .copied())),
        [110, 112, 130, 131, 140]
    );
    // A root not in the sweep is no tree, whatever the consoles say.
    assert!(tree_adopting(&procs, 999, |p| consoles.get(&p.pid).copied()).is_empty());
}

/// An adopted orphan's own console brings its orphans: the search repeats while it finds a
/// member, whatever order the rows come in. NEGATIVE CONTROL: with the first orphan in another
/// console, neither is counted.
#[test]
fn an_adopted_orphans_console_brings_its_own_orphans() {
    let at = |pid: u32, ppid: u32, start: u64| ProcessReading {
        start: Some(start),
        ..row(pid, ppid, 1)
    };
    // 240 comes before 230 in the sweep and is attached to the console 230 made (host 232).
    let procs = [
        at(10, 4, 100),
        at(200, 10, 1000),
        at(240, 9002, 1300),
        at(230, 9001, 1200),
        at(232, 230, 1201),
    ];
    let chained = HashMap::from([(200, 10), (230, 10), (240, 232)]);
    assert_eq!(
        pids(&tree_adopting(&procs, 200, |p| chained
            .get(&p.pid)
            .copied())),
        [200, 230, 232, 240]
    );
    let elsewhere = HashMap::from([(200, 10), (230, 12), (240, 232)]);
    assert_eq!(
        pids(&tree_adopting(&procs, 200, |p| elsewhere
            .get(&p.pid)
            .copied())),
        [200]
    );
}

/// Adoption keeps the group cap, and a root whose console host is not running lends its
/// console to nothing (a pid no row holds is no console of this sweep).
#[test]
fn adoption_stops_at_the_group_cap_and_needs_a_live_console() {
    let at = |pid: u32, ppid: u32, start: u64| ProcessReading {
        start: Some(start),
        ..row(pid, ppid, 1)
    };
    let mut procs = vec![at(10, 4, 100), at(1000, 10, 1000)];
    procs.extend((0..400).map(|i| at(2000 + i, 50_000 + i, 2000)));
    let members = tree_adopting(&procs, 1000, |_| Some(10));
    assert_eq!(members.len(), MAX_GROUP_PIDS);
    assert_eq!(members[0].pid, 1000);
    assert_eq!(pids(&tree_adopting(&procs, 1000, |_| Some(14))), [1000]);
}

/// The compression store's working set is the compressed class, taken out of the app class
/// so the memory used does not move. NEGATIVE CONTROLS: a sweep with no store names no class
/// (compression off is not 0 bytes compressed); a program of this person's that merely has
/// the store's name is not it; and a store larger than the app class takes no more than that
/// class holds.
#[test]
fn the_compression_stores_working_set_is_the_compressed_class() {
    let detail = crate::MemoryDetail {
        total: 32 << 30,
        available: 12 << 30,
        wired: 2 << 30,
        app: 18 << 30,
        ..crate::MemoryDetail::default()
    };
    let named = |name: &str, session: u32, resident: Option<u64>| ProcessReading {
        name: name.to_owned(),
        uid: Some(session),
        resident,
        ..row(4044, 4, 1)
    };
    let store = named(windows::MEMORY_COMPRESSION, 0, Some(3 << 30));
    let with = windows::with_compression_store(detail, &[row(4, 0, 1), store.clone()]);
    assert_eq!(with.compressed, Some(3 << 30));
    assert_eq!(with.app, 15 << 30);
    assert_eq!(with.used(), detail.used());
    assert_eq!(
        with,
        crate::MemoryDetail {
            compressed: Some(3 << 30),
            app: 15 << 30,
            ..detail
        },
        "nothing else moves"
    );

    assert_eq!(
        windows::with_compression_store(detail, &[row(4, 0, 1)]),
        detail
    );
    let impostor = named(windows::MEMORY_COMPRESSION, 1, Some(3 << 30));
    assert_eq!(windows::with_compression_store(detail, &[impostor]), detail);
    let unread = named(windows::MEMORY_COMPRESSION, 0, None);
    assert_eq!(windows::with_compression_store(detail, &[unread]), detail);
    let huge = named(windows::MEMORY_COMPRESSION, 0, Some(64 << 30));
    let capped = windows::with_compression_store(detail, &[huge]);
    assert_eq!((capped.compressed, capped.app), (Some(18 << 30), 0));
}

/// The OS words are the product name with the build after it, and from Windows 11's first build
/// a client's frozen `Windows 10` name reads `Windows 11`. NEGATIVE CONTROLS: a Windows 10
/// build keeps its name, a server's name is never rewritten, and neither is a name that only
/// starts with the same letters.
#[test]
fn the_os_words_name_windows_11_by_its_build() {
    let words = |p, b| os_words(Some(p), Some(b));
    assert_eq!(
        words("Windows 10 Home", "26200").as_deref(),
        Some("Windows 11 Home 26200")
    );
    assert_eq!(
        words("Windows 10 Pro", "22000").as_deref(),
        Some("Windows 11 Pro 22000")
    );
    assert_eq!(
        words("Windows 10", "22631").as_deref(),
        Some("Windows 11 22631")
    );
    assert_eq!(
        words("Windows 10 Pro", "19045").as_deref(),
        Some("Windows 10 Pro 19045")
    );
    assert_eq!(
        words("Windows 10 Pro", "21999").as_deref(),
        Some("Windows 10 Pro 21999")
    );
    assert_eq!(
        words("Windows Server 2025 Datacenter", "26100").as_deref(),
        Some("Windows Server 2025 Datacenter 26100")
    );
    assert_eq!(
        words("Windows 100 Home", "26200").as_deref(),
        Some("Windows 100 Home 26200")
    );
    assert_eq!(
        words(" Windows 11 Home ", " 26200 ").as_deref(),
        Some("Windows 11 Home 26200")
    );
    assert_eq!(
        os_words(Some("Windows 10 Home"), None).as_deref(),
        Some("Windows 10 Home")
    );
    assert_eq!(
        words("Windows 10 Home", "not a build").as_deref(),
        Some("Windows 10 Home not a build")
    );
    assert_eq!(os_words(None, Some("26200")), None);
    assert_eq!(os_words(Some("  "), Some("26200")), None);
    let m = crate::MachineWords {
        cpu: None,
        os: words("Windows 10 Home", "26200"),
    };
    assert_eq!(m.os_major(), Some(11));

    assert_eq!(
        os_build(Some("26200"), Some(7171)).as_deref(),
        Some("26200.7171")
    );
    assert_eq!(os_build(Some("26200"), None).as_deref(), Some("26200"));
    assert_eq!(os_build(None, Some(7171)), None);
    assert_eq!(os_build(Some(""), Some(7171)), None);
}
