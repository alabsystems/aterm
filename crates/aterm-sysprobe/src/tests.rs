// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
use super::*;
use crate::machine;

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

/// A `children` text names each pid its space closes, so a text cut at the
/// cap drops its torn last pid instead of naming a shorter one.
#[test]
fn a_children_text_names_only_the_pids_its_spaces_close() {
    assert_eq!(linux::children("812 9001 7 "), vec![812, 9001, 7]);
    assert_eq!(linux::children("812 90"), vec![812], "the torn last pid");
    assert_eq!(linux::children(""), Vec::<u32>::new());
    assert_eq!(linux::children("812 x 7 "), vec![812, 7], "a word no pid");
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
    #[cfg(unix)]
    {
        let zero = read_bounded(Path::new("/dev/zero")).expect("/dev/zero opens");
        assert_eq!(zero.len(), READ_CAP);
    }
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
    let torn = &VMSTAT[..VMSTAT.find("pswpout 34").unwrap() + "pswpout 34".len()];
    assert_eq!(linux::swap_pages(torn), None);
}

#[test]
fn kernel_pressure_levels_map_to_memory_levels() {
    assert_eq!(pressure_of(1), Some(MemoryLevel::Normal));
    assert_eq!(pressure_of(2), Some(MemoryLevel::Warn));
    assert_eq!(pressure_of(4), Some(MemoryLevel::Critical));
    assert_eq!(pressure_of(3), None);
}

/// Memory in use matches Activity Monitor's `Memory Used`: `(internal - purgeable) + wired + compressed`.
#[test]
fn memory_in_use_is_counted_from_the_vm_pages() {
    let page = 16_384;
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

/// A process's working directory and executable are read from the OS: this
/// test's own are what the process says they are.
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

/// `group_memory` sums member footprints within the target process group,
/// excluding children placed in their own group (`process_group(0)`).
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
        let _ = child.kill();
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
/// answer.
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

/// `stdin_sharers` returns group members whose fd 0 shares the leader's
/// input device vnode (including independent opens of the same device),
/// excluding `/dev/null`, pipes, separate process groups, and non-leaders.
#[cfg(target_os = "macos")]
#[test]
fn a_leaders_input_is_shared_only_by_members_on_that_very_device() {
    use aterm_uds::spawnfd::{Child, Stdio, command};
    /// A `sleep` killed (`SIGKILL`) and reaped by its own pid when the test
    /// lets go of it, on a panic too: it is never left running, and its pid,
    /// unreaped until then, cannot have been recycled.
    struct Reaped(Child);
    impl Drop for Reaped {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let zero = || std::fs::File::open("/dev/zero").expect("/dev/zero");
    let sleep = |group: i32, input: Stdio| {
        Reaped(
            command("/bin/sleep")
                .arg("30")
                .process_group(group)
                .stdin(input)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("sleep starts"),
        )
    };
    let leader = sleep(0, zero().into());
    let pgid = leader.0.id();
    let group = i32::try_from(pgid).expect("a pid");
    let handed = sleep(group, zero().into());
    let _on_null = sleep(group, Stdio::null());
    let _on_pipe = sleep(group, Stdio::piped());
    let apart = sleep(0, zero().into());
    let piped_leader = sleep(0, Stdio::piped());

    assert_eq!(crate::stdin_sharers(pgid), Some(vec![handed.0.id()]));
    assert_eq!(
        crate::stdin_sharers(handed.0.id()),
        None,
        "a member leads no group"
    );
    assert_eq!(
        crate::stdin_sharers(apart.0.id()),
        Some(Vec::new()),
        "alone in its group"
    );
    assert_eq!(
        crate::stdin_sharers(piped_leader.0.id()),
        None,
        "a leader reading a pipe"
    );
    assert_eq!(crate::stdin_sharers(0), None);
    assert_eq!(crate::stdin_sharers(u32::MAX), None);
}

/// The machine's facts read on this host: its physical memory and the
/// kernel's pressure level, each one `sysctl`.
#[cfg(target_os = "macos")]
#[test]
fn the_machines_memory_and_pressure_are_read_on_this_host() {
    let mib = crate::physical_memory_mib().expect("hw.memsize");
    assert!(mib >= 4 * 1024, "{mib} MiB");
    assert!(crate::memory_pressure().is_some());
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

/// `SwapMeter` computes MB/s over readings separated by at least
/// `SWAP_SPAN_MIN`, reuses the prior rate for sub-span reads, resets across
/// gaps exceeding `SWAP_SPAN_MAX` or `None` readings, and returns `None` on
/// the first sample.
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

#[cfg(unix)]
fn mount(on: &str, fstype: &str, local: bool) -> volumes::Mount {
    volumes::Mount {
        on: std::path::PathBuf::from(on),
        from: format!("/dev/{}", on.len()),
        fstype: fstype.to_owned(),
        local,
        automounted: fstype == "autofs",
        removable: false,
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
/// stays there.
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
/// udisks' media folders removable, and a torn last line dropped — which,
/// like a read cut at the cap, makes the table incomplete.
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
    assert!(
        t.mounts.iter().all(|m| !m.removable),
        "no mount above sits where udisks mounts removable media"
    );
    let media = volumes::parse_mountinfo(
        "30 22 8:33 / /run/media/a/STICK rw - vfat /dev/sdc1 rw\n\
         31 22 8:49 / /media/CARD rw - exfat /dev/sdd1 rw\n\
         32 22 8:65 / /mediax rw - ext4 /dev/sde1 rw\n",
        false,
    );
    let removable: Vec<bool> = media.mounts.iter().map(|m| m.removable).collect();
    assert_eq!(removable, [true, true, false]);
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
/// WindowServer.
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
/// iowait, steal and guest are left out.
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
/// begins one (`intr`, whose line is the long one).
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
/// buffer and padded to the present count.
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

/// `/proc/meminfo` page classes: anonymous app pages, `Shmem` shared memory,
/// wired kernel slab/stacks/pagetables, `Cached - Shmem + Buffers` cache,
/// zswap compressed/stored, and `Committed_AS`/`CommitLimit`.
#[test]
fn meminfo_gives_the_classes_of_section_9_6() {
    const FULL: &str = "\
MemTotal:       16318756 kB
MemFree:          812344 kB
MemAvailable:    4079689 kB
Buffers:          204800 kB
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
Shmem:            500000 kB
Slab:             400000 kB
SReclaimable:     250000 kB
SUnreclaim:       150000 kB
KernelStack:       20000 kB
PageTables:        60000 kB
CommitLimit:    10256524 kB
Committed_AS:   19000000 kB
";
    let k = |kib: u64| kib * 1024;
    let m = linux::memory_detail(FULL).expect("a meminfo");
    assert_eq!((m.total, m.free), (k(16_318_756), k(812_344)));
    assert_eq!(m.app, k(8_400_000));
    assert_eq!(m.shared, Some(k(500_000)));
    assert_eq!(m.wired, k(150_000 + 20_000 + 60_000));
    assert_eq!(m.file_backed, k(3_310_000 - 500_000 + 204_800 + 250_000));
    assert_eq!(
        (m.active, m.inactive),
        (Some(k(9_000_000)), Some(k(5_000_000)))
    );
    assert_eq!(
        (m.compressed, m.stored),
        (Some(k(100_000)), Some(k(300_000)))
    );
    assert_eq!(
        (m.swap_used, m.swap_total),
        (k(2_097_148 - 1_048_576), k(2_097_148))
    );
    assert_eq!(
        m.commit,
        Some(Commit {
            charge: k(19_000_000),
            limit: k(10_256_524),
            exact: false
        }),
        "overcommit: the charge passes an advisory limit"
    );
    assert_eq!(m.commit.and_then(|c| c.permille()), Some(1852));
    assert_eq!((m.speculative, m.purgeable, m.pressure), (None, None, None));
    assert!(!m.exact, "Linux leaves a remainder no class names");

    let old = linux::memory_detail(MEMINFO).expect("the short fixture");
    assert_eq!((old.compressed, old.shared, old.commit), (None, None, None));
    assert_eq!(old.app, 0, "a summed key that is missing adds nothing");
    assert_eq!(
        old.file_backed,
        k(3_310_000 + 204_800),
        "no Shmem: nothing taken out"
    );
    let halves = "MemTotal: 100 kB\nMemFree: 10 kB\nSReclaimable: 3 kB\nSUnreclaim: 4 kB\n";
    assert_eq!(
        linux::memory_detail(halves).map(|m| (m.wired, m.file_backed)),
        Some((k(4), k(3)))
    );
    let whole = "MemTotal: 100 kB\nMemFree: 10 kB\nSlab: 9 kB\n";
    assert_eq!(
        linux::memory_detail(whole).map(|m| (m.wired, m.file_backed)),
        Some((k(9), 0))
    );
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

/// M5 fixture: perflevel0 "Super" (4 cores, 16 MiB L2, `P`), perflevel1 "Efficiency" (6 cores, 6 MiB L2, `E`).
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
/// 0–5.
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
/// that do not sum to the core count) there is one `"CPU"` cluster.
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

/// `memory_detail_of` computes free (`free - speculative`), app (`internal - purgeable`),
/// wired, compressed, and unclamped swap stored from 16 KiB page counts.
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
        swapins: 7,
        swapouts: 11,
        decompressions: 1_234,
        compressions: 2_345,
        pageins: 99,
    };
    let m = machine::memory_detail_of(
        &p,
        page,
        32 << 30,
        Some((1 << 30, 2 << 30)),
        Some(MemoryLevel::Normal),
    );
    assert_eq!(m.free, 17_000 * page);
    assert_eq!(m.speculative, Some(3_000 * page));
    assert_eq!(m.app, 920_000 * page);
    assert_eq!(m.stored, Some(317_850 * page));
    assert_eq!(
        (m.wired, m.compressed, m.file_backed),
        (173_670 * page, Some(138_281 * page), 783_000 * page)
    );
    assert!(m.exact, "macOS's classes partition physical memory");
    assert_eq!(
        (m.shared, m.commit),
        (None, None),
        "no such classes on macOS"
    );
    assert_eq!(
        (m.swap_used, m.swap_total, m.pressure),
        (1 << 30, 2 << 30, Some(MemoryLevel::Normal))
    );
    assert_eq!(
        m.counters,
        Some(crate::VmCounters {
            swapin_bytes: 7 * page,
            swapout_bytes: 11 * page,
            decompressions: 1_234,
            compressions: 2_345,
            pageins: 99,
        }),
        "the counters come from the same read, swap in bytes"
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
        Some(u64::MAX),
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
/// system, idle, nice), read unsigned.
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
/// `hw.logicalcpu` counts, and a second read runs no counter backwards.
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
    let class = |c: Option<u64>| c.expect("every class reads on macOS");
    for class in [
        m.free,
        class(m.speculative),
        class(m.active),
        class(m.inactive),
        m.wired,
        class(m.compressed),
        class(m.purgeable),
        m.file_backed,
        m.app,
    ] {
        assert!(class <= total, "{class} of {m:?}");
    }
    let sum = [
        m.wired,
        class(m.active),
        class(m.inactive),
        class(m.speculative),
        m.free,
        class(m.compressed),
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

/// NVML's return codes as the vitals say them: not supported is
/// `unsupported`, no permission names NVML's gate, and every other failure
/// (uninitialised, not found, GPU lost, a library behind its kernel module)
/// is `not_read`.
#[cfg(target_os = "linux")]
#[test]
fn an_nvml_failure_says_why() {
    assert_eq!(crate::nvml::why(3), Why::Unsupported);
    assert_eq!(crate::nvml::why(4), Why::NeedsPrivilege("nvml"));
    assert_eq!(
        Why::NeedsPrivilege("nvml").to_string(),
        "needs_privilege:nvml"
    );
    for code in [1, 2, 6, 9, 15, 18, 999] {
        assert_eq!(crate::nvml::why(code), Why::NotRead, "{code}");
    }
}

/// Live: this host's NVIDIA card, where it has one and the driver's
/// `libnvidia-ml.so.1`, reads a whole percent from the real NVML by its PCI
/// slot, and, where its runtime PM cannot move, through the system root's
/// GPU reader too.
#[cfg(target_os = "linux")]
#[test]
fn nvml_reads_the_nvidia_card_on_this_host() {
    let drm = Path::new("/sys/class/drm");
    let card = std::fs::read_dir(drm).ok().and_then(|cards| {
        cards.flatten().find_map(|e| {
            let dev = e.path().join("device");
            let uevent = std::fs::read_to_string(dev.join("uevent")).ok()?;
            uevent.lines().any(|l| l == "DRIVER=nvidia").then_some(())?;
            let slot = uevent
                .lines()
                .find_map(|l| l.strip_prefix("PCI_SLOT_NAME="))?;
            Some((dev, slot.to_owned()))
        })
    });
    let Some((dev, slot)) = card else {
        eprintln!("SKIP: no NVIDIA DRM card with a PCI slot");
        return;
    };
    if aterm_dl::Library::open(crate::nvml::SONAMES).is_err() {
        eprintln!("SKIP: no libnvidia-ml.so.1");
        return;
    }
    let word = |file: &str| {
        std::fs::read_to_string(dev.join(file))
            .map(|t| t.trim().to_owned())
            .unwrap_or_default()
    };
    let status = word("power/runtime_status");
    let settled = linux::pm_settled(&status, &word("power/control"), &word("boot_vga"));
    if status == "suspended" || !settled {
        eprintln!("SKIP: {slot} is under runtime PM ({status}); NVML would wake it");
        return;
    }
    let first = Instant::now();
    let pct = crate::nvml::device_pct(&slot).expect("NVML's figure");
    let first = first.elapsed();
    let again = Instant::now();
    let pct2 = crate::nvml::device_pct(&slot).expect("NVML's figure, again");
    let again = again.elapsed();
    assert!(pct <= 100 && pct2 <= 100, "{pct} {pct2}");
    eprintln!("nvml {slot}: {pct}% in {first:?}, then {pct2}% in {again:?}");
    let g = linux::Root::system().gpu().expect("a GPU");
    eprintln!("Root::system().gpu(): {g:?}");
    assert!(g.device_pct.is_some_and(|p| p <= 100), "{g:?}");
    assert_eq!(g.unread, None, "{g:?}");
}

/// Live: the root volume's figures are in order and name their mount and
/// type.
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

/// Compile-time check that [`QuietErrors`] implements neither `Send` nor
/// `Sync` (since the thread error mode it sets and restores is thread-local).
#[test]
fn a_quiet_guard_cannot_leave_its_thread() {
    /// Answered through `()` by every type, and through a marker by a type
    /// that is `Send` or `Sync`.
    trait OnItsThread<A> {}
    impl<T> OnItsThread<()> for T {}
    /// The answer of a `Send` type.
    struct IsSend;
    impl<T: Send> OnItsThread<IsSend> for T {}
    /// The answer of a `Sync` type.
    struct IsSync;
    impl<T: Sync> OnItsThread<IsSync> for T {}
    fn on_its_thread<T: OnItsThread<A>, A>() {}
    on_its_thread::<QuietErrors, _>();
}

/// A link that is up and running, named `name`, with `physical` as given.
fn link(name: &str, physical: Option<bool>) -> LinkReading {
    LinkReading {
        name: name.to_owned(),
        up: true,
        physical,
        ..LinkReading::default()
    }
}

/// The machine's own links: on macOS (no device fact) the up, running
/// `en*` and `pdp_ip*` links; on Linux (a device fact) the up, running
/// links a device backs.
#[test]
fn only_the_machines_own_links_count_toward_its_traffic() {
    for name in ["en0", "en7", "pdp_ip0", "pdp_ip3"] {
        assert!(counts_toward_machine(&link(name, None)), "{name}");
    }
    for name in [
        "lo0", "utun4", "ipsec0", "ppp0", "gif0", "stf0", "bridge0", "awdl0", "llw0", "nan0",
        "ap1", "anpi0", "vmenet0", "fw0", "XHC20", "",
    ] {
        assert!(!counts_toward_machine(&link(name, None)), "{name}");
    }
    for name in ["eth0", "enp3s0", "wlan0", "wlp2s0", "wwan0"] {
        assert!(counts_toward_machine(&link(name, Some(true))), "{name}");
        assert!(!counts_toward_machine(&link(name, Some(false))), "{name}");
    }
    for name in ["docker0", "veth12ab", "br-1f2e", "tun0", "wg0", "lo"] {
        assert!(!counts_toward_machine(&link(name, Some(false))), "{name}");
    }
    assert!(
        !counts_toward_machine(&link("bridge0", Some(true))),
        "a prefix outranks a device"
    );
    let mut loopback = link("en9", None);
    loopback.loopback = true;
    assert!(!counts_toward_machine(&loopback));
    let mut down = link("en0", None);
    down.up = false;
    assert!(!counts_toward_machine(&down));
}

/// A link's kind by name, where the OS does not class it.
#[test]
fn a_links_name_gives_its_kind_where_the_os_classes_none() {
    for (name, kind) in [
        ("pdp_ip0", LinkKind::Cellular),
        ("wwan0", LinkKind::Cellular),
        ("rmnet_data0", LinkKind::Cellular),
        ("wlan0", LinkKind::Wifi),
        ("wlp2s0", LinkKind::Wifi),
        ("en0", LinkKind::Wired),
        ("enp3s0", LinkKind::Wired),
        ("eth1", LinkKind::Wired),
        ("utun0", LinkKind::Other),
        ("lo0", LinkKind::Other),
        ("", LinkKind::Other),
    ] {
        assert_eq!(machine::kind_by_name(name), kind, "{name}");
    }
    assert_eq!(LinkKind::Wifi.as_str(), "wifi");
    assert_eq!(LinkKind::default(), LinkKind::Other);
}

const NET_DEV: &str = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 4045398   40310    0    0    0     0          0         0  4045398   40310    0    0    0     0       0          0
  eth0: 98765432101 1234567 3 4 0 0 0 99 12345678901 765432 5 6 0 0 0 0
wlan0:7 8 9 10 0 0 0 0 11 12 13 14 0 0 0 0
 bad0: 1 2 3
 bad1: 1 2 3 4 5 6 7 8 9 10 11 x 13 14 15 16
  ../x: 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16
  torn: 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16";

/// `/proc/net/dev`: each interface's received and sent counters, the header
/// lines skipped, a name with no space after its colon read.
#[test]
fn proc_net_dev_gives_each_interfaces_counters() {
    let rows = linux::net_dev(NET_DEV);
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["lo", "eth0", "wlan0"]);
    let eth = &rows[1];
    assert_eq!(
        eth.rx,
        LinkCounters {
            bytes: 98_765_432_101,
            packets: 1_234_567,
            errors: 3,
            drops: 4
        }
    );
    assert_eq!(
        eth.tx,
        LinkCounters {
            bytes: 12_345_678_901,
            packets: 765_432,
            errors: 5,
            drops: 6
        }
    );
    assert_eq!((rows[2].rx.bytes, rows[2].tx.drops), (7, 14));
    assert!(linux::net_dev("").is_empty());
}

/// sysfs's link facts: hex flags, up only with `IFF_UP` and an `operstate`
/// of `up` or `unknown`, a speed in Mb/s as bits per second, and the kind
/// from `wireless`, the name and `device`.
#[test]
fn sysfs_link_facts_read_flags_state_speed_and_kind() {
    assert_eq!(linux::hex_flags("0x1003\n"), Some(0x1003));
    assert_eq!(linux::hex_flags("9"), Some(9));
    assert_eq!(linux::hex_flags("0xZZ"), None);
    assert!(linux::link_up(0x1003, "up\n"));
    assert!(linux::link_up(0x9, "unknown\n"), "the loopback");
    assert!(!linux::link_up(0x1002, "up"), "administratively down");
    assert!(!linux::link_up(0x1003, "down"));
    assert!(!linux::link_up(0x1003, "dormant"));
    assert_eq!(linux::speed_bps("1000\n"), Some(1_000_000_000));
    assert_eq!(linux::speed_bps("25000"), Some(25_000_000_000));
    for bad in ["-1", "0", "", "fast"] {
        assert_eq!(linux::speed_bps(bad), None, "{bad:?}");
    }
    assert_eq!(linux::sys_kind("wlan0", true, true), LinkKind::Wifi);
    assert_eq!(linux::sys_kind("wwan0", false, true), LinkKind::Cellular);
    assert_eq!(linux::sys_kind("enp3s0", false, true), LinkKind::Wired);
    assert_eq!(linux::sys_kind("docker0", false, false), LinkKind::Other);
    assert_eq!(linux::sys_kind("ap0", true, true), LinkKind::Other);
    assert_eq!(linux::sys_kind("bridge0", false, true), LinkKind::Other);
}

/// The Linux link reader on a fixture root: each `/proc/net/dev` row joined
/// to its sysfs facts, a link with no sysfs directory read as not up with
/// no device.
#[test]
fn the_link_reader_reads_a_fixture_root() {
    let root = aterm_tempfile::tempdir().expect("scratch root");
    let r = root.path();
    std::fs::create_dir_all(r.join("proc/net")).unwrap();
    std::fs::write(
        r.join("proc/net/dev"),
        "Inter-|\n face |\n\
         \x20   lo: 10 1 0 0 0 0 0 0 10 1 0 0 0 0 0 0\n\
         \x20 eth0: 500 5 1 2 0 0 0 0 300 3 0 1 0 0 0 0\n\
         \x20wlan0: 70 7 0 0 0 0 0 0 80 8 0 0 0 0 0 0\n\
         docker0: 9 9 0 0 0 0 0 0 9 9 0 0 0 0 0 0\n\
         \x20 gone: 1 1 0 0 0 0 0 0 1 1 0 0 0 0 0 0\n",
    )
    .unwrap();
    let sys = |name: &str, files: &[(&str, &str)], dirs: &[&str]| {
        let dir = r.join("sys/class/net").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for (file, text) in files {
            std::fs::write(dir.join(file), text).unwrap();
        }
        for sub in dirs {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
    };
    sys(
        "lo",
        &[
            ("ifindex", "1\n"),
            ("flags", "0x9\n"),
            ("operstate", "unknown\n"),
        ],
        &[],
    );
    sys(
        "eth0",
        &[
            ("ifindex", "2\n"),
            ("flags", "0x1003\n"),
            ("operstate", "up\n"),
            ("speed", "1000\n"),
        ],
        &["device"],
    );
    sys(
        "wlan0",
        &[
            ("ifindex", "3\n"),
            ("flags", "0x1003\n"),
            ("operstate", "up\n"),
            ("speed", "-1\n"),
        ],
        &["device", "wireless"],
    );
    sys(
        "docker0",
        &[
            ("ifindex", "4\n"),
            ("flags", "0x1003\n"),
            ("operstate", "up\n"),
        ],
        &[],
    );
    let links = linux::links_under_into(r, &mut Vec::new()).expect("the fixture");
    let names: Vec<&str> = links.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["lo", "eth0", "wlan0", "docker0", "gone"]);

    let lo = &links[0];
    assert!(lo.up && lo.loopback && lo.physical == Some(false), "{lo:?}");
    let eth = &links[1];
    assert_eq!((eth.index, eth.kind), (2, LinkKind::Wired), "{eth:?}");
    assert_eq!(eth.rate_bps, Some(1_000_000_000));
    assert_eq!((eth.rx.bytes, eth.rx.errors, eth.tx.drops), (500, 1, 1));
    let wlan = &links[2];
    assert_eq!(
        (wlan.kind, wlan.rate_bps),
        (LinkKind::Wifi, None),
        "{wlan:?}"
    );
    let docker = &links[3];
    assert_eq!(
        (docker.kind, docker.physical),
        (LinkKind::Other, Some(false))
    );
    let gone = &links[4];
    assert!(
        !gone.up && gone.index == 0 && gone.physical == Some(false),
        "{gone:?}"
    );

    let counted: Vec<&str> = links
        .iter()
        .filter(|l| counts_toward_machine(l))
        .map(|l| l.name.as_str())
        .collect();
    assert_eq!(counted, ["eth0", "wlan0"]);

    let empty = aterm_tempfile::tempdir().expect("scratch root");
    assert_eq!(linux::links_under_into(empty.path(), &mut Vec::new()), None);
}

/// The link reader's scratch: made [`linux::NET_DEV_CAP`] long where it is
/// shorter and read again in place by the next reading, whose answer is
/// what it read now, not what an earlier, longer file left in it; a longer
/// scratch is kept whole.
#[test]
fn the_link_readers_scratch_is_read_again_in_place() {
    let fixture = |dev: &str| {
        let root = aterm_tempfile::tempdir().expect("scratch root");
        std::fs::create_dir_all(root.path().join("proc/net")).unwrap();
        std::fs::write(root.path().join("proc/net/dev"), dev).unwrap();
        root
    };
    let head = "Inter-|\n face |\n";
    let two = fixture(&format!(
        "{head}  eth0: 500 5 1 2 0 0 0 0 300 3 0 1 0 0 0 0\n\
         \x20wlan0: 70 7 0 0 0 0 0 0 80 8 0 0 0 0 0 0\n"
    ));
    let one = fixture(&format!("{head}  eth1: 9 9 0 0 0 0 0 0 9 9 0 0 0 0 0 0\n"));
    let names = |links: &[LinkReading]| links.iter().map(|l| l.name.clone()).collect::<Vec<_>>();

    let mut scratch = Vec::new();
    let first = linux::links_under_into(two.path(), &mut scratch).expect("two links");
    assert_eq!(names(&first), ["eth0", "wlan0"]);
    assert_eq!(
        Some(&first),
        linux::links_under_into(two.path(), &mut Vec::new()).as_ref()
    );
    assert_eq!(scratch.len(), linux::NET_DEV_CAP);

    let at = scratch.as_ptr();
    let second = linux::links_under_into(one.path(), &mut scratch).expect("one link");
    assert_eq!(
        names(&second),
        ["eth1"],
        "nothing of the longer file is read"
    );
    assert_eq!(
        Some(&second),
        linux::links_under_into(one.path(), &mut Vec::new()).as_ref()
    );
    assert_eq!(
        (scratch.as_ptr(), scratch.len()),
        (at, linux::NET_DEV_CAP),
        "read again in place"
    );

    let mut long = vec![0u8; 2 * linux::NET_DEV_CAP];
    let third = linux::links_under_into(two.path(), &mut long).expect("two links");
    assert_eq!(third, first);
    assert_eq!(
        long.len(),
        2 * linux::NET_DEV_CAP,
        "a longer scratch is kept"
    );

    let empty = aterm_tempfile::tempdir().expect("scratch root");
    assert_eq!(linux::links_under_into(empty.path(), &mut scratch), None);
}

/// `/proc/loadavg`'s total: the count after the `/` of its fourth field,
/// from a whole first line.
#[test]
fn the_load_average_file_gives_every_thread_on_the_machine() {
    assert_eq!(
        linux::loadavg_tasks("0.20 0.18 0.12 1/80 11206\n"),
        Some(80)
    );
    assert_eq!(
        linux::loadavg_tasks("30.12 31.40 29.98 12/3412 98765\n"),
        Some(3412)
    );
    assert_eq!(linux::loadavg_tasks("0.20 0.18 0.12 1/8"), None, "torn");
    assert_eq!(linux::loadavg_tasks("0.20 0.18 0.12 1/0 5\n"), None);
    assert_eq!(linux::loadavg_tasks("0.20 0.18 0.12 80 5\n"), None);
    assert_eq!(linux::loadavg_tasks("0.20 0.18 0.12 x/80 5\n"), None);
    assert_eq!(linux::loadavg_tasks("0.20 0.18 0.12 1/-80 5\n"), None);
    assert_eq!(linux::loadavg_tasks("0.20 0.18\n"), None);
    assert_eq!(linux::loadavg_tasks(""), None);

    let root = aterm_tempfile::tempdir().expect("scratch root");
    std::fs::create_dir_all(root.path().join("proc")).unwrap();
    std::fs::write(
        root.path().join("proc/loadavg"),
        "1.50 1.20 0.90 3/897 4242\n",
    )
    .unwrap();
    assert_eq!(linux::Root::new(root.path()).tasks(), Some(897));
    let empty = aterm_tempfile::tempdir().expect("scratch root");
    assert_eq!(linux::Root::new(empty.path()).tasks(), None);
}

/// The macOS pid listing: offered the size ask's room and a quarter more,
/// never past [`machine::MAX_PIDS`], and counted only when it listed some
/// pids and did not fill its room; one uid's listing of none counts zero
/// where `errno` stayed clear.
#[test]
fn a_pid_listing_counts_only_what_it_listed_within_its_room() {
    assert_eq!(machine::pid_room(920), Some(1150));
    assert_eq!(machine::pid_room(1_000_000), Some(machine::MAX_PIDS));
    assert_eq!(machine::pid_room(0), None);
    assert_eq!(machine::pid_room(-1), None);
    assert_eq!(machine::listed_count(897, 1150), Some(897));
    assert_eq!(machine::listed_count(1149, 1150), Some(1149));
    assert_eq!(machine::listed_count(1150, 1150), None, "a full room");
    assert_eq!(machine::listed_count(0, 1150), None, "zero is a failure");
    assert_eq!(machine::listed_count(-1, 1150), None);
    assert_eq!(machine::owned_count(0, 0, 1150), Some(0), "owns none");
    assert_eq!(machine::owned_count(0, 22, 1150), None, "a failure");
    assert_eq!(machine::owned_count(12, 0, 1150), Some(12));
    assert_eq!(machine::owned_count(1150, 0, 1150), None, "a full room");
}

/// Two ceilings: the smaller of the known; an unlimited soft limit is no
/// ceiling.
#[test]
fn the_smaller_known_ceiling_wins_and_infinity_is_none() {
    assert_eq!(machine::smaller(Some(10_666), Some(4_000)), Some(4_000));
    assert_eq!(machine::smaller(Some(10_666), None), Some(10_666));
    assert_eq!(machine::smaller(None, Some(7)), Some(7));
    assert_eq!(machine::smaller(None, None), None);
    assert_eq!(machine::rlimit_ceiling(10_666, u64::MAX), Some(10_666));
    assert_eq!(machine::rlimit_ceiling(u64::MAX, u64::MAX), None);
}

/// Live: the process table's ceilings on this host. The machine's is known
/// and positive; one user's, where known, is no larger (macOS clamps
/// `kern.maxprocperuid` to `kern.maxproc`).
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_process_tables_ceilings_read_on_this_host() {
    let l = crate::process_limits();
    let system = l.system.expect("the machine's ceiling");
    assert!(system > 0, "{l:?}");
    if cfg!(target_os = "macos") {
        let per_user = l.per_user.expect("one user's ceiling");
        assert!(per_user <= system, "{l:?}");
    }
}

/// Live: three load averages, each finite and non-negative.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_load_average_reads_on_this_host() {
    let load = crate::load_average().expect("three load averages");
    assert!(load.iter().all(|l| l.is_finite() && *l >= 0.0), "{load:?}");
}

/// Live: this Mac's interfaces. Each has a name and a distinct index; the
/// loopback `lo0` is up, a loopback, and not the machine's traffic; every
/// link the machine filter refuses by name (a tunnel, a bridge, a
/// peer-to-peer or coprocessor link) reads other, whatever its functional
/// type.
#[cfg(target_os = "macos")]
#[test]
fn the_network_links_read_a_structurally_valid_snapshot_on_this_host() {
    let mut scratch = Vec::new();
    let first = crate::net_links_into(&mut scratch).expect("the interface list");
    assert!(!first.is_empty());
    let mut indexes: Vec<u32> = first.iter().map(|l| l.index).collect();
    indexes.sort_unstable();
    indexes.dedup();
    assert_eq!(indexes.len(), first.len(), "{first:?}");
    assert!(
        first.iter().all(|l| !l.name.is_empty() && l.index > 0),
        "{first:?}"
    );
    let lo = first.iter().find(|l| l.name == "lo0").expect("lo0");
    assert!(lo.up && lo.loopback && !counts_toward_machine(lo), "{lo:?}");
    for l in first
        .iter()
        .filter(|l| machine::never_the_machines(&l.name))
    {
        assert_eq!(l.kind, LinkKind::Other, "{l:?}");
    }
}

/// The capability words agents read (`aterm ctl sys caps`): one word per
/// answer, and the reason when unavailable.
#[test]
fn capability_words_are_one_word_each() {
    use crate::{Cap, CapSource, Caps, Why};
    for (cap, word) in [
        (Cap::Yes, "yes"),
        (Cap::Estimate, "estimate"),
        (Cap::No(Why::Unsupported), "unavailable(unsupported)"),
        (Cap::No(Why::NotRead), "unavailable(not_read)"),
        (
            Cap::No(Why::NotEnabled("psi")),
            "unavailable(not_enabled:psi)",
        ),
        (
            Cap::No(Why::NeedsPrivilege("root")),
            "unavailable(needs_privilege:root)",
        ),
        (Cap::No(Why::Asleep), "unavailable(asleep)"),
        (Cap::No(Why::Absent), "unavailable(absent)"),
    ] {
        assert_eq!(cap.to_string(), word);
        assert!(!word.contains(' '), "{word}");
    }
    assert_eq!(CapSource::default(), CapSource::Host);
    assert_eq!(CapSource::CgroupLimit.word(), "cgroup_limit");
    let rows = Caps::unsupported("plan9").rows();
    assert_eq!(rows[0], ("os", "plan9".to_owned()));
    assert_eq!(rows.last(), Some(&("capacity", "host".to_owned())));
    for (name, word) in &rows[1..rows.len() - 1] {
        assert_eq!(word, "unavailable(unsupported)", "{name}");
    }
}

/// This host's own report names its OS and answers every row.
#[test]
fn this_hosts_capability_report_names_its_os() {
    let rows = crate::caps().rows();
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(windows) {
        "windows"
    } else {
        "other"
    };
    assert_eq!(rows[0], ("os", os.to_owned()));
    for (name, word) in &rows {
        assert!(!word.is_empty() && !word.contains(' '), "{name}={word}");
    }
}

/// Level names by count, and clusters by rank, fastest first.
#[test]
fn levels_are_named_by_count_and_ranked_fastest_first() {
    assert!(crate::level_names(0).is_empty());
    assert_eq!(crate::level_names(1), ["CPU"]);
    assert_eq!(crate::level_names(2), ["Performance", "Efficiency"]);
    assert_eq!(crate::level_names(3), ["Performance", "Mid", "Efficiency"]);
    assert_eq!(
        crate::level_names(5),
        ["Performance", "Mid 1", "Mid 2", "Mid 3", "Efficiency"]
    );
    let layout =
        crate::layout_by_rank(&[(3, 0), (0, 2), (2, 0), (1, 2), (1, 2)]).expect("a layout");
    let got: Vec<(&str, &[u16])> = layout
        .clusters
        .iter()
        .map(|c| (c.name.as_str(), c.logical.as_slice()))
        .collect();
    assert_eq!(
        got,
        [("Performance", &[0, 1][..]), ("Efficiency", &[2, 3][..])]
    );
    assert_eq!(crate::layout_by_rank(&[]), None);
    let one = crate::layout_by_rank(&[(0, 5), (1, 5)]).expect("a layout");
    assert_eq!(one.clusters.len(), 1);
    assert_eq!(one.clusters[0].name, "CPU");
}
