// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::machine;

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

/// A `stat` line's group is its fifth field, counted from the command
/// name's LAST `)`, so a name holding spaces and parentheses cannot move it.
/// NEGATIVE CONTROLS: a line cut before the group, and one with no name.
#[test]
fn a_stat_line_gives_its_group_past_any_command_name() {
    assert_eq!(
        linux::stat_pgrp("4242 (zsh) S 4200 4242 4242 34816 4242"),
        Some(4242)
    );
    assert_eq!(
        linux::stat_pgrp("77 (my (odd) name) R 1 76 76 0 -1"),
        Some(76),
        "a name with spaces and parentheses"
    );
    assert_eq!(
        linux::stat_pgrp("77 (sh) S 1"),
        None,
        "cut before the group"
    );
    assert_eq!(linux::stat_pgrp("77 sh S 1 76"), None, "no name");
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
    // incident read one to EOF and took the machine down). Unix's: Windows
    // has no endless device a path opens.
    #[cfg(unix)]
    {
        let zero = read_bounded(Path::new("/dev/zero")).expect("/dev/zero opens");
        assert_eq!(zero.len(), READ_CAP);
    }
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

/// The production census adapter requires a positive, complete leader-only
/// answer. Empty, failed, misaligned, full and overlong replies cannot prove
/// that no other process belongs to the group.
#[cfg(target_os = "macos")]
#[test]
fn only_a_complete_group_census_admits_its_leader() {
    use crate::macos::complete_lone_group;
    let leader = [41, 0, 0, 0];
    assert!(complete_lone_group(41, 4, &leader));
    assert!(
        complete_lone_group(41, 8, &leader),
        "zero padding is not a member"
    );
    for bytes in [-1, 0, 1, 3, 5, 16, 20] {
        assert!(
            !complete_lone_group(41, bytes, &leader),
            "byte count {bytes}"
        );
    }
    assert!(!complete_lone_group(0, 4, &leader));
    assert!(!complete_lone_group(41, 4, &[0, 0, 0, 0]));
    assert!(!complete_lone_group(41, 4, &[42, 0, 0, 0]));
    assert!(!complete_lone_group(41, 8, &[41, 42, 0, 0]));
    assert!(!complete_lone_group(41, 8, &[41, -1, 0, 0]));
    assert!(!complete_lone_group(41, 8, &[41, 41, 0, 0]));
    assert!(!complete_lone_group(41, 4, &[]));
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

// ---------------------------------------------------------------------------
// Volumes: the pure lookups and the mountinfo parser, then one live read.
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn mount(on: &str, fstype: &str, local: bool) -> volumes::Mount {
    volumes::Mount {
        on: std::path::PathBuf::from(on),
        from: format!("/dev/{}", on.len()),
        fstype: fstype.to_owned(),
        local,
        automounted: fstype == "autofs",
        read_only: false,
        container: None,
    }
}

#[cfg(unix)]
fn mac_table() -> volumes::Table {
    volumes::Table {
        mounts: vec![
            mount("/", "apfs", true),
            mount("/System/Volumes/Data", "apfs", true),
            mount("/System/Volumes/Data/home", "autofs", false),
            mount("/Volumes/Recovery", "apfs", true),
        ],
        complete: true,
        firmlinks: volumes::parse_firmlinks(
            "/Users\tUsers\n/private\tprivate\n/Volumes\tVolumes\n",
        ),
    }
}

/// A firmlinked path is read in its data-volume spelling: `~` is on the data
/// volume, not on the sealed root, and a path under `/System/Volumes/Data`
/// stays there. The longest prefix wins, component by component (`/Volumes/
/// Recoveryx` is not on `/Volumes/Recovery`). NEGATIVE CONTROL: with no
/// firmlinks the same `~` reads as the root's.
// Unix-only: the table holds `/` mount points, no path Windows reads as absolute.
#[cfg(unix)]
#[test]
fn a_path_finds_its_mount_through_the_firmlinks() {
    let t = mac_table();
    let on = |p: &str| volumes::mount_of(std::path::Path::new(p), &t).map(|m| m.on.clone());
    let data = Some(std::path::PathBuf::from("/System/Volumes/Data"));
    assert_eq!(on("/Users//jsmith/aterm"), data);
    assert_eq!(on("/private/tmp/x"), data);
    assert_eq!(on("/System/Volumes/Data/Users//x"), data);
    assert_eq!(on("/home/x"), on("/"), "/home is not a firmlink here");
    assert_eq!(
        on("/System/Volumes/Data/home/x"),
        Some(std::path::PathBuf::from("/System/Volumes/Data/home"))
    );
    assert_eq!(
        on("/Volumes/Recovery/x"),
        Some(std::path::PathBuf::from("/Volumes/Recovery"))
    );
    assert_eq!(on("/Volumes/Recoveryx"), data, "a component, not a prefix");
    assert_eq!(on("relative/path"), None);
    assert_eq!(on("/Users/../etc"), None, "`..` is never placed");
    let bare = volumes::Table {
        firmlinks: Vec::new(),
        ..mac_table()
    };
    assert_eq!(
        volumes::mount_of(std::path::Path::new("/Users//jsmith"), &bare).map(|m| m.on.clone()),
        Some(std::path::PathBuf::from("/"))
    );
}

/// `mounts_under` finds a mount at or under a directory in either spelling,
/// and nothing above or beside it; a relative directory is answered with
/// every mount (nothing ruled out on a path nobody can place).
// Unix-only: the table holds `/` mount points, no path Windows reads as absolute.
#[cfg(unix)]
#[test]
fn mounts_under_sees_a_mount_inside_a_directory_in_either_spelling() {
    let mut t = mac_table();
    t.mounts.push(mount(
        "/System/Volumes/Data/Users//a/repo/target/mnt",
        "apfs",
        true,
    ));
    let under = |p: &str| volumes::mounts_under(std::path::Path::new(p), &t).count();
    assert_eq!(under("/Users//a/repo/target"), 1);
    assert_eq!(under("/System/Volumes/Data/Users//a/repo/target"), 1);
    assert_eq!(under("/Users//a/repo/target/mnt"), 1, "at it");
    assert_eq!(under("/Users//a/other"), 0);
    assert_eq!(under("/Users//a/repo/targetx"), 0);
    assert_eq!(under("rel"), t.mounts.len());
}

/// The mountinfo parser: `\040` decoded, the optional fields skipped up to
/// `-`, `ro` read, an autofs mount automounted and a network one not local,
/// and a torn last line dropped — which, like a read cut at the cap, makes
/// the table incomplete. NEGATIVE CONTROL: the same text whole is complete.
// Unix-only: the table holds `/` mount points, no path Windows reads as absolute.
#[cfg(unix)]
#[test]
fn the_mountinfo_parser_reads_escapes_and_marks_a_cut_table() {
    let text = "\
22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw
23 22 0:21 / /mnt/my\\040disk ro,nosuid shared:7 master:2 - xfs /dev/sdb1 ro
24 22 0:44 / /net rw - autofs systemd-1 rw
25 22 0:50 / /srv/nfs rw - nfs4 host:/x rw
";
    let t = volumes::parse_mountinfo(text, false);
    assert!(t.complete);
    let ons: Vec<&str> = t.mounts.iter().map(|m| m.on.to_str().unwrap()).collect();
    assert_eq!(ons, ["/", "/mnt/my disk", "/net", "/srv/nfs"]);
    assert!(t.mounts[0].local && !t.mounts[0].read_only);
    assert!(t.mounts[1].read_only && t.mounts[1].fstype == "xfs");
    assert!(t.mounts[2].automounted && !t.mounts[2].local);
    assert!(!t.mounts[3].local, "nfs is remote");
    assert_eq!(
        volumes::read(&t.mounts[2]),
        Err(volumes::Unread::Automounted)
    );
    assert_eq!(volumes::read(&t.mounts[3]), Err(volumes::Unread::Remote));
    let torn = &text[..text.len() - 10];
    let t = volumes::parse_mountinfo(torn, false);
    assert!(!t.complete, "a torn last line");
    assert_eq!(t.mounts.len(), 3);
    assert!(
        !volumes::parse_mountinfo(text, true).complete,
        "cut at the cap"
    );
    let many: String = (0..=volumes::MAX_MOUNTS)
        .map(|i| format!("{i} 1 0:1 / /m{i} rw - ext4 /dev/x rw\n"))
        .collect();
    let t = volumes::parse_mountinfo(&many, false);
    assert!(!t.complete && t.mounts.len() == volumes::MAX_MOUNTS);
}

#[test]
fn an_apfs_source_names_its_container() {
    for (from, want) in [
        ("/dev/disk3s5", Some("disk3")),
        ("/dev/disk3s1s1", Some("disk3")),
        ("com.apple.os.update-ABC@/dev/disk12s1", Some("disk12")),
        ("map auto_home", None),
        ("/dev/disk3", None),
        ("devfs", None),
    ] {
        assert_eq!(volumes::apfs_container(from).as_deref(), want, "{from}");
    }
}

// Unix-only: the table holds `/` mount points, no path Windows reads as absolute.
#[cfg(unix)]
#[test]
fn the_firmlinks_file_is_read_line_by_line() {
    let f = volumes::parse_firmlinks("/Users\tUsers\nbad line\nrel\tx\n/opt\topt\n");
    assert_eq!(
        f,
        [
            (
                std::path::PathBuf::from("/Users"),
                std::path::PathBuf::from("/System/Volumes/Data/Users")
            ),
            (
                std::path::PathBuf::from("/opt"),
                std::path::PathBuf::from("/System/Volumes/Data/opt")
            ),
        ]
    );
}

/// The live read, on this test's own temp directory's volume: a syscall, not
/// WindowServer. The table lists the mount; its size is at least its free
/// figure, both above zero; free inodes are counted.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_volume_reads_its_size_free_space_and_inodes() {
    let t = volumes::table();
    assert!(!t.mounts.is_empty(), "{t:?}");
    let dir = std::fs::canonicalize(std::env::temp_dir()).expect("temp dir");
    let r = volumes::read_path(&dir, &t).expect("a local volume");
    assert!(r.total >= r.free && r.free > 0, "{r:?}");
    assert!(r.files_free.is_some(), "{r:?}");
    assert!(t.points().iter().any(|p| p == &r.mount));
}

// ---------------------------------------------------------------------------
// The machine now: per-core ticks, the cluster layout, memory by page class,
// the GPU and a volume. The pure pieces first, then one live read of each.
// ---------------------------------------------------------------------------

/// Busy is user + system + nice, total adds idle, and neither wraps.
#[test]
fn a_cores_busy_and_total_ticks_add_up_and_saturate() {
    let t = CoreTicks {
        user: 10,
        system: 5,
        idle: 80,
        nice: 2,
    };
    assert_eq!((t.busy(), t.total()), (17, 97));
    assert_eq!(
        (CoreTicks::default().busy(), CoreTicks::default().total()),
        (0, 0)
    );
    let full = CoreTicks {
        user: u64::MAX,
        system: 1,
        idle: 1,
        nice: 0,
    };
    assert_eq!((full.busy(), full.total()), (u64::MAX, u64::MAX));
}

/// `/proc/stat` of a 4-core Linux 6.8 box, trimmed: the aggregate line, the
/// four cores (ten columns), then the kernel's next lines.
const PROC_STAT: &str = "\
cpu  4705 150 1120 1644379 1200 0 52 0 0 0
cpu0 1393 30 290 410203 300 0 40 0 0 0
cpu1 1101 40 280 411101 280 0 4 0 0 0
cpu2 1205 50 270 411538 310 0 4 7 0 0
cpu3 1006 30 280 411537 310 0 4 0 0 0
intr 1462898 0 9 0 0 0 0 3 0 1 0 0 0 0 0 0
ctxt 3022891
btime 1729339200
";

/// Each `cpuN` line is core N: system takes irq + softirq, idle takes
/// iowait, steal and guest are left out. A core the kernel left out (offline)
/// reads zero in its place; four columns (an old kernel) are enough.
/// NEGATIVE CONTROLS: three columns, a word for a number, an id past
/// `MAX_CPUS`, and a text with no core line read as nothing.
#[test]
fn proc_stat_gives_every_cores_ticks_by_id() {
    let cores = linux::proc_stat_cores(PROC_STAT, false).expect("four cores");
    assert_eq!(cores.len(), 4);
    assert_eq!(
        cores[0],
        CoreTicks {
            user: 1393,
            nice: 30,
            system: 290 + 40,
            idle: 410_203 + 300,
        }
    );
    assert_eq!(cores[2].system, 270 + 4, "steal (7) is not system time");
    let gap = linux::proc_stat_cores("cpu  1 1 1 1\ncpu0 1 2 3 4\ncpu2 5 6 7 8\n", false).unwrap();
    assert_eq!(gap.len(), 3);
    assert_eq!(gap[1], CoreTicks::default(), "an offline core reads zero");
    assert_eq!(
        gap[2],
        CoreTicks {
            user: 5,
            nice: 6,
            system: 7,
            idle: 8
        }
    );
    assert_eq!(
        linux::proc_stat_cores("cpu0 1 2 3\n", false),
        None,
        "three columns"
    );
    assert_eq!(linux::proc_stat_cores("cpu0 1 2 x 4\n", false), None);
    let far = format!("cpu{} 1 2 3 4\n", linux::MAX_CPUS);
    assert_eq!(
        linux::proc_stat_cores(&far, false),
        None,
        "an id past the cap"
    );
    assert_eq!(linux::proc_stat_cores("intr 1 2\n", false), None, "no core");
}

/// A read cut at its cap is whole only when the core lines are known to
/// have ended: a whole line that is not a core's was read, or the torn tail
/// begins one (`intr`, whose line is the long one). A cut inside the core
/// lines, or exactly after one, might hide more cores: nothing.
#[test]
fn a_cut_proc_stat_is_read_only_past_its_core_lines() {
    let intr = PROC_STAT.find("intr").unwrap();
    let in_intr = &PROC_STAT[..intr + 9];
    assert_eq!(
        linux::proc_stat_cores(in_intr, true).map(|c| c.len()),
        Some(4)
    );
    let in_cpu3 = &PROC_STAT[..PROC_STAT.find("cpu3").unwrap() + 8];
    assert_eq!(linux::proc_stat_cores(in_cpu3, true), None, "cut in a core");
    assert_eq!(
        linux::proc_stat_cores(&PROC_STAT[..intr], true),
        None,
        "cut at a line end"
    );
    let in_label = &PROC_STAT[..intr + 2];
    assert_eq!(
        linux::proc_stat_cores(in_label, true).map(|c| c.len()),
        Some(4),
        "`in` is no core"
    );
    let maybe_cpu = &PROC_STAT[..PROC_STAT.find("cpu3").unwrap() + 2];
    assert_eq!(
        linux::proc_stat_cores(maybe_cpu, true),
        None,
        "`cp` may be a core"
    );
    assert_eq!(
        linux::proc_stat_cores(&PROC_STAT[..intr], false).map(|c| c.len()),
        Some(4),
        "the whole file"
    );
}

/// The Linux reader on a fixture root: `<root>/proc/stat` read into its own
/// buffer and padded to the present count. A file whose core lines run past
/// `STAT_CAP` is refused, not read short. NEGATIVE CONTROL: a root with no
/// `proc/stat`.
#[test]
fn the_stat_reader_reads_a_fixture_root_and_refuses_a_cut_core_list() {
    let root = aterm_tempfile::tempdir().expect("scratch root");
    std::fs::create_dir_all(root.path().join("proc")).unwrap();
    std::fs::write(root.path().join("proc/stat"), PROC_STAT).unwrap();
    let mut reader = linux::StatReader::new(root.path(), 6);
    let cores = reader.sample().expect("the fixture");
    assert_eq!(cores.len(), 6, "padded to the present count");
    assert_eq!(cores[3].user, 1006);
    assert_eq!(cores[5], CoreTicks::default());
    assert_eq!(reader.sample(), Some(cores), "the buffer is reused");

    let many: String = (0..2000)
        .map(|i| format!("cpu{i} 1000000 2000000 3000000 4000000 5 6 7 8 9 10\n"))
        .collect();
    assert!(many.len() > linux::STAT_CAP);
    std::fs::write(root.path().join("proc/stat"), &many).unwrap();
    assert_eq!(reader.sample(), None, "core lines past the cap");

    let empty = aterm_tempfile::tempdir().expect("scratch root");
    assert_eq!(linux::StatReader::new(empty.path(), 4).sample(), None);
}

/// `read_into` reads at most its buffer and says whether there was more.
#[test]
fn a_buffered_read_stops_at_its_buffer_and_says_whether_it_was_cut() {
    let dir = aterm_tempfile::tempdir().expect("scratch dir");
    let path = dir.path().join("f");
    let mut buf = [0u8; 8];
    std::fs::write(&path, b"0123456789").unwrap();
    assert_eq!(read_into(&path, &mut buf), Ok((8, true)));
    assert_eq!(&buf, b"01234567");
    std::fs::write(&path, b"01234567").unwrap();
    assert_eq!(read_into(&path, &mut buf), Ok((8, false)), "exactly full");
    std::fs::write(&path, b"012").unwrap();
    assert_eq!(read_into(&path, &mut buf), Ok((3, false)));
    #[cfg(unix)]
    assert_eq!(read_into(Path::new("/dev/zero"), &mut buf), Ok((8, true)));
    assert!(read_into(&dir.path().join("absent"), &mut buf).is_err());
    std::fs::write(&path, b"MemTotal: 1 kB\n").unwrap();
    assert_eq!(
        with_bounded(&path, str::len),
        Some(15),
        "the stack-buffer reader"
    );
    assert_eq!(with_bounded(&dir.path().join("absent"), str::len), None);
    assert_eq!(utf8_prefix(b"ab\xffcd"), "ab", "text ends where UTF-8 does");
}

/// A kernel CPU list: ranges and singles, ascending and distinct.
/// NEGATIVE CONTROLS: empty, a word, a range that runs backwards, an id past
/// the cap.
#[test]
fn a_cpu_list_reads_its_ranges() {
    assert_eq!(
        linux::cpulist("0-3,5,8-9\n"),
        Some(vec![0, 1, 2, 3, 5, 8, 9])
    );
    assert_eq!(linux::cpulist("0"), Some(vec![0]));
    assert_eq!(linux::cpulist("2-3,0-2"), Some(vec![0, 1, 2, 3]));
    for bad in ["", "\n", "x", "3-1", "0-", "0-5000"] {
        assert_eq!(linux::cpulist(bad), None, "{bad:?}");
    }
}

/// `/proc/meminfo` by page class: what maps is filled (wired is the four
/// never-paged counters, file-backed the two file LRUs, app the anonymous
/// pages, compressed and stored zswap's pool and contents), the rest zero.
/// NEGATIVE CONTROL: no `MemFree` is no reading; a missing optional key is 0.
#[test]
fn meminfo_gives_the_page_classes_that_map() {
    const FULL: &str = "\
MemTotal:       16318756 kB
MemFree:          812344 kB
MemAvailable:    4079689 kB
Cached:          3310000 kB
SwapCached:        10000 kB
Active:          9000000 kB
Inactive:        5000000 kB
Active(anon):    7000000 kB
Inactive(anon):  1500000 kB
Active(file):    2000000 kB
Inactive(file):  3500000 kB
Unevictable:        4000 kB
Mlocked:            4000 kB
SwapTotal:       2097148 kB
SwapFree:        1048576 kB
Zswap:            100000 kB
Zswapped:         300000 kB
AnonPages:       8400000 kB
KernelStack:       20000 kB
PageTables:        60000 kB
SUnreclaim:       150000 kB
";
    let k = |kib: u64| kib * 1024;
    let m = linux::memory_detail(FULL).expect("a meminfo");
    assert_eq!((m.total, m.free), (k(16_318_756), k(812_344)));
    assert_eq!((m.active, m.inactive), (k(9_000_000), k(5_000_000)));
    assert_eq!(m.wired, k(4_000 + 150_000 + 20_000 + 60_000));
    assert_eq!((m.compressed, m.stored), (k(100_000), k(300_000)));
    assert_eq!(m.file_backed, k(2_000_000 + 3_500_000));
    assert_eq!(m.app, k(8_400_000));
    assert_eq!(
        (m.swap_used, m.swap_total),
        (k(2_097_148 - 1_048_576), k(2_097_148))
    );
    assert_eq!((m.speculative, m.purgeable, m.pressure), (0, 0, None));
    let old = linux::memory_detail(MEMINFO).expect("the short fixture");
    assert_eq!((old.compressed, old.app), (0, 0), "missing keys read 0");
    assert_eq!(linux::memory_detail("MemTotal: 10 kB\n"), None);
}

/// One `hw.perflevelN` level.
fn level(name: &str, logical: u16, l2_mib: u64) -> machine::Level {
    machine::Level {
        name: name.to_owned(),
        logical,
        l2: Some(l2_mib << 20),
    }
}

/// One device-tree CPU node.
fn node(id: u16, kind: u8, l2_mib: u64) -> machine::DtCpu {
    machine::DtCpu {
        id,
        kind: Some(kind),
        l2: Some(l2_mib << 20),
    }
}

/// The owner's M5 as its sysctls and device tree read on 2026-10-19:
/// perflevel0 "Super" (4 cores, 16 MiB L2), perflevel1 "Efficiency" (6
/// cores, 6 MiB L2); cpu0–5 `E`, cpu6–9 `P`.
fn m5() -> (Vec<machine::Level>, Vec<machine::DtCpu>) {
    let levels = vec![level("Super", 4, 16), level("Efficiency", 6, 6)];
    let cpus = (0..10)
        .map(|i| {
            if i < 6 {
                node(i, b'E', 6)
            } else {
                node(i, b'P', 16)
            }
        })
        .collect();
    (levels, cpus)
}

/// Every logical id in `0..n` is in exactly one cluster.
fn partitions(layout: &CpuLayout, n: u16) -> bool {
    let mut ids: Vec<u16> = layout
        .clusters
        .iter()
        .flat_map(|c| c.logical.clone())
        .collect();
    ids.sort_unstable();
    ids == (0..n).collect::<Vec<_>>()
}

/// The clusters come from the levels, fastest first, and their cores from
/// the device tree: the M5's Super cores are 6–9 and its Efficiency cores
/// 0–5. Where the L2 sizes cannot tell the levels apart, `cluster-type`
/// does. NEGATIVE CONTROL: a device tree that puts the fast cores FIRST is
/// believed over the convention, so the map is read, not assumed.
#[test]
fn the_cluster_layout_is_read_from_the_device_tree() {
    let (levels, cpus) = m5();
    let layout = machine::layout_of(&levels, &cpus, 10);
    let want = |s: &[u16], e: &[u16]| CpuLayout {
        clusters: vec![
            Cluster {
                name: "Super".to_owned(),
                logical: s.to_vec(),
            },
            Cluster {
                name: "Efficiency".to_owned(),
                logical: e.to_vec(),
            },
        ],
    };
    assert_eq!(layout, want(&[6, 7, 8, 9], &[0, 1, 2, 3, 4, 5]));
    assert!(partitions(&layout, 10));
    assert_eq!(layout.logical_cores(), 10);

    let same_l2: Vec<machine::DtCpu> = cpus
        .iter()
        .map(|c| machine::DtCpu {
            l2: Some(1 << 20),
            ..*c
        })
        .collect();
    let same_levels = vec![level("Super", 4, 1), level("Efficiency", 6, 1)];
    assert_eq!(
        machine::layout_of(&same_levels, &same_l2, 10),
        layout,
        "by cluster-type"
    );

    let fast_first: Vec<machine::DtCpu> = (0..10)
        .map(|i| {
            if i < 4 {
                node(i, b'P', 16)
            } else {
                node(i, b'E', 6)
            }
        })
        .collect();
    assert_eq!(
        machine::layout_of(&levels, &fast_first, 10),
        want(&[0, 1, 2, 3], &[4, 5, 6, 7, 8, 9])
    );
}

/// Where the device tree cannot be read or does not add up, the convention
/// places the slowest level at the lowest ids; with no levels (or levels
/// that do not sum to the core count) there is one `"CPU"` cluster. Every
/// answer places every logical id exactly once.
#[test]
fn a_device_tree_that_does_not_add_up_falls_back_to_the_convention() {
    let (levels, cpus) = m5();
    let read = machine::layout_of(&levels, &cpus, 10);
    assert_eq!(machine::layout_of(&levels, &[], 10), read, "no device tree");
    let mut dup = cpus.clone();
    dup[9].id = 0;
    assert_eq!(
        machine::dt_assignment(&levels, &dup, 10),
        None,
        "an id twice"
    );
    assert_eq!(machine::layout_of(&levels, &dup, 10), read);
    let mut lopsided = cpus.clone();
    lopsided[0] = node(0, b'P', 16);
    assert_eq!(
        machine::dt_assignment(&levels, &lopsided, 10),
        None,
        "5 Super nodes for 4"
    );
    let mut unknown = cpus.clone();
    unknown[3] = machine::DtCpu {
        id: 3,
        kind: None,
        l2: Some(3 << 20),
    };
    assert_eq!(
        machine::dt_assignment(&levels, &unknown, 10),
        None,
        "a node of no level"
    );
    let mut far = cpus.clone();
    far[9].id = 10;
    assert_eq!(
        machine::dt_assignment(&levels, &far, 10),
        None,
        "an id past the count"
    );

    let three = vec![level("A", 2, 8), level("B", 2, 4), level("C", 4, 2)];
    let conv = machine::layout_of(&three, &[], 8);
    let ids: Vec<Vec<u16>> = conv.clusters.iter().map(|c| c.logical.clone()).collect();
    assert_eq!(ids, [vec![6, 7], vec![4, 5], vec![0, 1, 2, 3]]);

    for (levels, n) in [(Vec::new(), 8), (vec![level("A", 3, 1)], 8)] {
        let single = machine::layout_of(&levels, &[], n);
        assert_eq!(single, CpuLayout::single(n));
        assert_eq!(single.clusters[0].name, "CPU");
        assert!(partitions(&single, n));
    }
    let one = machine::layout_of(&[level("Performance", 8, 4)], &[], 8);
    assert_eq!(one.clusters.len(), 1);
    assert_eq!(one.clusters[0].name, "Performance");
    let empty_level = vec![level("Super", 4, 16), level("Efficiency", 0, 6)];
    let four = machine::layout_of(&empty_level, &[], 4);
    assert_eq!(four.clusters.len(), 1, "a level with no cores is left out");
}

/// Memory by page class from one read: free is the free count less the
/// speculative pages it holds, app is internal less purgeable (Activity
/// Monitor's App Memory), stored is not clamped to physical memory, and the
/// differences stop at zero. The figures are the M5's 14:00 sample of
/// 2026-10-16 (design §9.1), in 16 KiB pages.
#[test]
fn memory_by_page_class_is_counted_from_one_read() {
    let page = 16_384;
    let p = machine::VmPages {
        free: 20_000,
        speculative: 3_000,
        active: 600_000,
        inactive: 580_000,
        wired: 173_670,
        compressor: 138_281,
        stored: 317_850,
        purgeable: 10_000,
        external: 783_000,
        internal: 930_000,
    };
    let m = machine::memory_detail_of(
        &p,
        page,
        32 << 30,
        Some((1 << 30, 2 << 30)),
        Some(MemoryLevel::Normal),
    );
    assert_eq!(m.free, 17_000 * page);
    assert_eq!(m.speculative, 3_000 * page);
    assert_eq!(m.app, 920_000 * page);
    assert_eq!(m.stored, 317_850 * page);
    assert_eq!(
        (m.wired, m.compressed, m.file_backed),
        (173_670 * page, 138_281 * page, 783_000 * page)
    );
    assert_eq!(
        (m.swap_used, m.swap_total, m.pressure),
        (1 << 30, 2 << 30, Some(MemoryLevel::Normal))
    );
    let odd = machine::VmPages {
        free: 1,
        speculative: 5,
        internal: 1,
        purgeable: 9,
        stored: u64::MAX,
        ..machine::VmPages::default()
    };
    let m = machine::memory_detail_of(&odd, page, 1 << 30, None, None);
    assert_eq!((m.free, m.app), (0, 0), "differences stop at zero");
    assert_eq!(
        m.stored,
        u64::MAX,
        "products saturate, stored is not clamped"
    );
    assert_eq!((m.swap_used, m.swap_total), (0, 0), "no swap reading");
}

/// A driver's utilization is a whole percent: past 100 reads 100, negative
/// reads unknown.
#[test]
fn a_utilization_figure_is_held_to_a_percent() {
    for (v, want) in [
        (0, Some(0)),
        (25, Some(25)),
        (100, Some(100)),
        (250, Some(100)),
        (i64::MAX, Some(100)),
        (-1, None),
        (i64::MIN, None),
    ] {
        assert_eq!(machine::percent_of(v), want, "{v}");
    }
}

/// A volume's figures are its block counts times its block size, free held
/// to the size and available to free, every product saturating.
#[test]
fn a_volumes_figures_are_held_in_order() {
    let r = machine::volume_of(1000, 400, 300, 4096, "/".into(), "apfs".into());
    assert_eq!(
        (r.total, r.free, r.available),
        (4_096_000, 1_638_400, 1_228_800)
    );
    let odd = machine::volume_of(10, 20, 30, 1, "/x".into(), "x".into());
    assert_eq!((odd.total, odd.free, odd.available), (10, 10, 10));
    let huge = machine::volume_of(u64::MAX, u64::MAX, 1, 2, "/".into(), "x".into());
    assert_eq!(
        (huge.total, huge.free, huge.available),
        (u64::MAX, u64::MAX, 2)
    );
}

/// A path becomes a C string in a caller's buffer, never past it.
/// NEGATIVE CONTROLS: an interior NUL, and a path with no room for its
/// terminator.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_path_is_a_c_string_in_its_buffer() {
    let mut buf = [0u8; 8];
    let c = machine::c_path(Path::new("/a/b"), &mut buf).expect("fits");
    assert_eq!(c.to_bytes(), b"/a/b");
    assert_eq!(machine::c_path(Path::new("/a\0b"), &mut buf), None);
    assert_eq!(
        machine::c_path(Path::new("/1234567"), &mut buf),
        None,
        "no room for NUL"
    );
    assert!(machine::c_path(Path::new("/123456"), &mut buf).is_some());
}

/// `host_processor_info`'s array is runs of four 32-bit counters (user,
/// system, idle, nice), read unsigned. NEGATIVE CONTROL: an array shorter
/// than its count.
#[cfg(target_os = "macos")]
#[test]
fn the_processor_array_reads_four_counters_per_core() {
    let words = [10, 5, 80, 2, 11, 6, 81, -1];
    let cores = macos::cores_of(&words, 2).expect("two cores");
    assert_eq!(
        cores[0],
        CoreTicks {
            user: 10,
            system: 5,
            idle: 80,
            nice: 2
        }
    );
    assert_eq!(cores[1].nice, u64::from(u32::MAX), "unsigned counters");
    assert_eq!(macos::cores_of(&words, 3), None);
    assert_eq!(macos::cores_of(&words, 0), Some(Vec::new()));
}

/// The device tree's encodings: little-endian data words and a
/// `cluster-type` letter.
#[cfg(target_os = "macos")]
#[test]
fn device_tree_data_reads_little_endian() {
    assert_eq!(macos::le_u64(&[0x00, 0x00, 0x60, 0x00]), Some(6 << 20));
    assert_eq!(macos::le_u64(&[0x00, 0x00, 0x00, 0x01]), Some(16 << 20));
    assert_eq!(macos::le_u64(&[]), None);
    assert_eq!(macos::le_u64(&[0; 9]), None);
    assert_eq!(macos::cluster_kind(b"E\0"), Some(b'E'));
    assert_eq!(macos::cluster_kind(b"P"), Some(b'P'));
    assert_eq!(macos::cluster_kind(b"\0"), None);
    assert_eq!(macos::cluster_kind(b""), None);
}

/// Live: every logical core is read, as many as the layout places and
/// `hw.logicalcpu` counts, and a second read runs no counter backwards. On a
/// Mac with more than one perflevel the layout came from the device tree,
/// not the convention.
#[cfg(target_os = "macos")]
#[test]
fn every_core_is_read_on_this_host_and_its_ticks_only_grow() {
    let logical = macos::sysctl_int(c"hw.logicalcpu").expect("hw.logicalcpu");
    let layout = crate::cpu_layout();
    let mut probe = Probe::new();
    let first = probe.core_ticks().expect("host_processor_info");
    let second = probe.core_ticks().expect("host_processor_info");
    assert_eq!(i64::try_from(first.len()).ok(), Some(logical));
    assert_eq!(first.len(), layout.logical_cores(), "{layout:?}");
    assert!(
        partitions(&layout, u16::try_from(logical).unwrap()),
        "{layout:?}"
    );
    assert!(
        layout
            .clusters
            .iter()
            .all(|c| !c.name.is_empty() && !c.logical.is_empty())
    );
    assert!(first.iter().any(|c| c.total() > 0));
    for (a, b) in first.iter().zip(&second) {
        assert!(
            b.user >= a.user && b.system >= a.system && b.idle >= a.idle && b.nice >= a.nice,
            "{a:?} -> {b:?}"
        );
    }
    let (levels, cpus, n) = macos::layout_inputs();
    if levels.len() > 1 {
        assert!(
            machine::dt_assignment(&levels, &cpus, n).is_some(),
            "the device tree's map did not hold: {levels:?} {cpus:?}"
        );
    }
}

/// Live: memory by page class on this host. Each physical class is within
/// physical memory, and wired + active + inactive + speculative + free +
/// compressed is within 10% of it (the rest is memory the VM never manages).
#[cfg(target_os = "macos")]
#[test]
fn memory_by_page_class_reads_on_this_host_and_adds_up() {
    let m = crate::memory_detail().expect("HOST_VM_INFO64");
    let total = m.total;
    assert!(total >= 4 << 30, "{m:?}");
    for class in [
        m.free,
        m.speculative,
        m.active,
        m.inactive,
        m.wired,
        m.compressed,
        m.purgeable,
        m.file_backed,
        m.app,
    ] {
        assert!(class <= total, "{class} of {m:?}");
    }
    let sum = [
        m.wired,
        m.active,
        m.inactive,
        m.speculative,
        m.free,
        m.compressed,
    ]
    .iter()
    .fold(0u64, |a, b| a.saturating_add(*b));
    assert!(sum.abs_diff(total) <= total / 10, "{sum} of {total}: {m:?}");
    assert!(m.swap_used <= m.swap_total, "{m:?}");
    assert!(m.pressure.is_some());
}

/// Live: the GPU on this host. An Apple silicon Mac always has its
/// accelerator, with a name and a core count; each percentage is at most
/// 100.
#[cfg(target_os = "macos")]
#[test]
fn the_gpu_reads_on_this_host_in_whole_percent() {
    let g = crate::gpu();
    if cfg!(target_arch = "aarch64") {
        let g = g.as_ref().expect("an Apple silicon GPU");
        assert!(g.name.is_some() && g.cores.is_some_and(|c| c > 0), "{g:?}");
        assert!(g.device_pct.is_some(), "{g:?}");
    }
    if let Some(g) = g {
        for pct in [g.device_pct, g.renderer_pct, g.tiler_pct]
            .into_iter()
            .flatten()
        {
            assert!(pct <= 100, "{g:?}");
        }
    }
}

/// Live: the root volume's figures are in order and name their mount and
/// type. NEGATIVE CONTROLS: a path that does not exist, and one with a NUL.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_root_volume_reads_on_this_host() {
    let r = crate::volume(Path::new("/")).expect("the root volume");
    assert!(
        r.total > 0 && r.total >= r.free && r.free >= r.available,
        "{r:?}"
    );
    assert_eq!(r.mount, Path::new("/"), "{r:?}");
    assert!(!r.fs.is_empty(), "{r:?}");
    assert_eq!(
        crate::volume(Path::new("/no/such/path/aterm-sysprobe")),
        None
    );
    assert_eq!(crate::volume(Path::new("/\0")), None);
}
