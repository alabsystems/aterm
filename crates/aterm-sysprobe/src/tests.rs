// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

// ---------------------------------------------------------------------------
// Fixtures: real /proc texts (a 4-core Linux 6.8 box), trimmed.
// ---------------------------------------------------------------------------

const MEMINFO: &str = "\
MemTotal:       16318756 kB
MemFree:          812344 kB
MemAvailable:    4079689 kB
Buffers:          204800 kB
Cached:          3310000 kB
SwapTotal:       2097148 kB
SwapFree:        1048576 kB
";

const VMSTAT: &str = "\
nr_free_pages 203086
nr_zone_inactive_anon 100
pgpgin 5123456
pgpgout 6123456
pswpin 1200
pswpout 3400
pgalloc_dma 0
";

// ---------------------------------------------------------------------------
// Linux parsers.
// ---------------------------------------------------------------------------

#[test]
fn meminfo_gives_the_total_and_the_available() {
    let m = linux::meminfo(MEMINFO).unwrap();
    assert_eq!(m.total_kib, 16_318_756);
    assert_eq!(m.available_kib, 4_079_689);
    assert_eq!(linux::meminfo("MemTotal: 10 kB\n"), None);
}

#[test]
fn vmstat_sums_swap_in_and_out() {
    assert_eq!(linux::swap_pages(VMSTAT), Some(4600));
    assert_eq!(linux::swap_pages("pswpin 3\n"), None);
}

#[cfg(unix)]
#[test]
fn the_auxiliary_vector_gives_the_page_size() {
    const W: usize = std::mem::size_of::<usize>();
    let pairs: [(usize, usize); 4] = [(33, 0xdead), (6, 16_384), (17, 250), (0, 0)];
    let bytes: Vec<u8> = pairs
        .iter()
        .flat_map(|(k, v)| k.to_ne_bytes().into_iter().chain(v.to_ne_bytes()))
        .collect();
    assert_eq!(linux::auxv_value(&bytes, 6), Some(16_384));
    assert_eq!(linux::auxv_value(&bytes, 99), None);
    assert_eq!(linux::auxv_value(&bytes[..W], 33), None, "a cut pair");
}

#[test]
fn every_proc_read_is_bounded_to_4_kib() {
    // A device that never ends costs exactly one buffer (the 2026-07-04
    // incident read one to EOF and took the machine down).
    let zero = read_bounded(Path::new("/dev/zero")).expect("/dev/zero opens");
    assert_eq!(zero.len(), READ_CAP);
    // A long file is cut at the cap; a short one is read whole.
    let dir = std::env::temp_dir().join(format!("aterm-sysprobe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let long = dir.join("long");
    let body: String = (0..1000).map(|i| format!("line{i:05}\n")).collect();
    assert!(body.len() > 2 * READ_CAP);
    std::fs::write(&long, &body).unwrap();
    let got = read_bounded(&long).unwrap();
    assert_eq!(got.len(), READ_CAP);
    assert!(body.starts_with(&got));
    let short = dir.join("short");
    std::fs::write(&short, MEMINFO).unwrap();
    assert_eq!(read_bounded(&short).as_deref(), Some(MEMINFO));
    assert_eq!(read_bounded(&dir.join("absent")), None);
    std::fs::remove_dir_all(&dir).unwrap();
    // A text cut mid-line at the cap loses the torn line, never parses half
    // a number: `pswpout 34` of `pswpout 3400` is not read as 34.
    let torn = &VMSTAT[..VMSTAT.find("pswpout 34").unwrap() + "pswpout 34".len()];
    assert_eq!(linux::swap_pages(torn), None);
}

// ---------------------------------------------------------------------------
// The shared pure pieces.
// ---------------------------------------------------------------------------

#[test]
fn kernel_pressure_levels_map_to_memory_levels() {
    assert_eq!(pressure_of(1), Some(MemoryLevel::Normal));
    assert_eq!(pressure_of(2), Some(MemoryLevel::Warn));
    assert_eq!(pressure_of(4), Some(MemoryLevel::Critical));
    assert_eq!(pressure_of(3), None);
}

/// Memory in use is Activity Monitor's `Memory Used`: app memory (internal
/// less purgeable) + wired + compressed. Measured on a 24 GB m1 at
/// `kern.memorystatus_level` 89, where `1000 − 10·level` reads 2.6 GB in use
/// beside Activity Monitor's ~8 GB.
#[test]
fn memory_in_use_is_counted_from_the_vm_pages() {
    let page = 16_384;
    // 358 721 anonymous (internal) pages, 4 509 purgeable, 118 941 wired,
    // 46 669 occupied by the compressor.
    let used = used_mib_of(358_721, 4_509, 118_941, 46_669, page).unwrap();
    assert_eq!(used, (358_721 - 4_509 + 118_941 + 46_669) * 16 / 1024);
    assert!((7_000..9_000).contains(&used), "{used} MiB");
    assert_eq!(used_mib_of(1, 0, 0, 0, 0), None);
    assert_eq!(
        used_mib_of(1, 5, 0, 0, page),
        Some(0),
        "purgeable past internal"
    );
}

/// Memory AVAILABLE is free + inactive + purgeable pages (what the memory
/// ceiling moves with), never more than the machine where that is known.
/// NEGATIVE CONTROLS: a zero page size is unknown; a sum past the machine
/// (a purgeable page on the inactive queue, counted twice) is capped.
#[test]
fn memory_available_is_free_inactive_and_purgeable_pages() {
    let page = 16_384;
    let got = available_mib_of(100_000, 200_000, 4_000, page, None).unwrap();
    assert_eq!(got, (100_000 + 200_000 + 4_000) * 16 / 1024);
    assert_eq!(available_mib_of(1, 1, 1, 0, None), None);
    assert_eq!(
        available_mib_of(8_000_000, 8_000_000, 0, page, Some(128 * 1024)),
        Some(128 * 1024),
        "capped at the machine"
    );
}

// ---------------------------------------------------------------------------
// Live, on this host.
// ---------------------------------------------------------------------------

/// A process's working directory and executable are read from the OS: this
/// test's own are what the process says they are. NEGATIVE CONTROL: a pid
/// that cannot exist reads as nothing, never as some directory or program.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_processs_working_directory_and_executable_are_its_own() {
    let own = crate::process_cwd(std::process::id()).expect("our own cwd");
    let want = std::env::current_dir().expect("current dir");
    assert_eq!(
        std::fs::canonicalize(&own).ok(),
        std::fs::canonicalize(&want).ok()
    );
    let exe = crate::process_path(std::process::id()).expect("our own executable");
    let want = std::env::current_exe().expect("current exe");
    assert_eq!(
        std::fs::canonicalize(&exe).ok(),
        std::fs::canonicalize(&want).ok()
    );
    for nobody in [0, u32::MAX] {
        assert_eq!(crate::process_cwd(nobody), None);
        assert_eq!(crate::process_path(nobody), None);
    }
}

/// A process group's memory is its members' FOOTPRINT, summed, and only its
/// own members': this test process's group grows by what it touches (512
/// MiB allocated and written, most of it read back past a 384 MiB margin
/// for the other members' churn); a child started in its own group
/// (`process_group(0)`, as Claude Code starts its Bash tool's shells) is its
/// own group, and a child left in ours is measured as one of ours.
/// NEGATIVE CONTROLS: a pid that cannot exist and pid 0 read as nothing.
#[cfg(target_os = "macos")]
#[test]
fn a_groups_memory_is_its_own_members_footprint() {
    use std::os::unix::process::CommandExt as _;
    let me = std::process::id();
    let before = crate::group_memory(me).expect("our own group");
    assert!(
        before.measured >= 1 && before.footprint_mib > 0,
        "{before:?}"
    );
    let mut hog = vec![0u8; 512 << 20];
    for page in hog.chunks_mut(4096) {
        page[0] = 1;
    }
    let after = crate::group_memory(me).expect("our own group");
    std::hint::black_box(&hog);
    assert!(
        after.footprint_mib >= before.footprint_mib + 384,
        "{before:?} -> {after:?}"
    );
    drop(hog);

    let apart = std::process::Command::new("/bin/sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .expect("sleep in a group of its own");
    let beside = std::process::Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("sleep in our group");
    let apart_pid = apart.id();
    let own = crate::group_memory(apart_pid);
    let ours = crate::group_memory(beside.id());
    for mut child in [apart, beside] {
        let _ = child.kill(); // SIGKILL
        let _ = child.wait();
    }
    let own = own.expect("the child's own group");
    assert_eq!((own.pgid, own.measured), (apart_pid, 1), "{own:?}");
    assert_ne!(own.pgid, after.pgid, "a group of its own: {own:?}");
    assert_eq!(ours.map(|g| g.pgid), Some(after.pgid), "{ours:?}");

    assert_eq!(crate::group_memory(0), None);
    assert_eq!(crate::group_memory(u32::MAX), None);
}

/// The machine's facts read on this host: its physical memory and the
/// kernel's pressure level, each one `sysctl`.
#[cfg(target_os = "macos")]
#[test]
fn the_machines_memory_and_pressure_are_read_on_this_host() {
    let mib = crate::physical_memory_mib().expect("hw.memsize");
    assert!(mib >= 4 * 1024, "{mib} MiB");
    assert!(crate::memory_pressure().is_some());
    // What is available and in use, each within the machine.
    let now = crate::machine_memory().expect("HOST_VM_INFO64");
    assert!(now.available_mib <= mib, "{now:?} of {mib} MiB");
    assert!(
        now.used_mib > 0 && now.used_mib <= mib,
        "{now:?} of {mib} MiB"
    );
}

/// The swap counter reads on this host and never runs backwards: two reads
/// of the same `HOST_VM_INFO64` counter, the second at least the first and
/// within what the machine could swap between them.
#[cfg(target_os = "macos")]
#[test]
fn the_swap_counter_reads_on_this_host_and_never_runs_backwards() {
    let first = crate::swapped_kib().expect("HOST_VM_INFO64");
    let second = crate::swapped_kib().expect("HOST_VM_INFO64");
    assert!(second >= first, "{first} KiB then {second} KiB");
    assert!(
        second - first < 1 << 20,
        "a GiB swapped between two reads: {first} -> {second}"
    );
}

/// THE SWAP RATE over the caller's readings: none from one reading; the
/// recorded 2026-09-28 thrash (89 MB/s, `swap 89 MB/s` in the freeze's
/// Messages row) read back from a counter that grew 445 MiB in 5 s; a
/// second reader a millisecond later answers the same rate instead of a
/// span of one page; a counter that stood still reads zero; a gap past
/// [`SWAP_SPAN_MAX`] starts again (no rate), and an unknown counter forgets
/// the base. NEGATIVE CONTROL: a meter taking its rate over the short span
/// would read the millisecond reader's zero, which the assertion rejects.
#[test]
fn the_swap_meter_reads_a_rate_over_a_span_of_its_readings() {
    let t0 = Instant::now();
    let s = std::time::Duration::from_secs;
    let mut m = SwapMeter::new();
    assert_eq!(m.observe(t0, Some(10_000_000)), None, "one reading");
    let thrash = 10_000_000 + 89 * 1024 * 5;
    assert_eq!(m.observe(t0 + s(5), Some(thrash)), Some(89 * 1024));
    let later = t0 + s(5) + std::time::Duration::from_millis(1);
    assert_eq!(
        m.observe(later, Some(thrash)),
        Some(89 * 1024),
        "a reading inside the shortest span answers the last rate"
    );
    assert_eq!(m.observe(t0 + s(10), Some(thrash)), Some(0), "still");
    assert_eq!(m.observe(t0 + s(15), Some(thrash - 1)), Some(0), "back");
    let gap = thrash + (1 << 30);
    assert_eq!(m.observe(t0 + s(200), Some(gap)), None, "a stale base");
    assert_eq!(m.observe(t0 + s(205), Some(gap + 5 * 4096)), Some(4096));
    assert_eq!(m.observe(t0 + s(206), None), None, "unknown");
    assert_eq!(
        m.observe(t0 + s(211), Some(1)),
        None,
        "the base was forgotten"
    );
    assert_eq!(m.observe(t0 + s(216), Some(1 + 5 * 8192)), Some(8192));
}
