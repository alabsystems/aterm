// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{BootClock, procfs};

#[test]
fn a_filetime_is_a_birth_counted_from_the_unix_epoch() {
    use super::{ProcessBirth, birth_of_filetime};
    const EPOCH: u64 = 116_444_736_000_000_000;
    let at = |seconds, microseconds| {
        Some(ProcessBirth {
            seconds,
            microseconds,
        })
    };
    assert_eq!(birth_of_filetime(EPOCH), at(0, 0));
    assert_eq!(birth_of_filetime(EPOCH + 9), at(0, 0));
    assert_eq!(
        birth_of_filetime(EPOCH + 17_900_000_000_000_000 + 1_234_567),
        at(1_790_000_000, 123_456)
    );
    assert_eq!(birth_of_filetime(EPOCH - 1), None);
}

/// What Linux says of a process Rosetta translates, as measured in the lane's guest
/// (`docs/measured/linux-rosetta-exe-2026-10-30.md`): `auxv` names the descriptor
/// `binfmt_misc` handed the translator (`AT_EXECFD`, 3, after `AT_PLATFORM`), whose `fdinfo`
/// carries the exec open's mark and the program's inode, and `maps` maps the program
/// executable, a newline in its path as `maps` prints one (`\012`). NEGATIVE CONTROLS: a
/// native process's `auxv`, and an `AT_EXECFD` past `AT_NULL`, name no descriptor;
/// `/dev/null` put on it is no exec open; an `fdinfo` with no `ino:` (before Linux 5.14) or
/// a signed one names no inode; a file mapped only readable (libc's row, and the program's
/// once its `r-xp` row is gone) is not mapped executable, and neither is a path that only
/// begins like the program's or runs past it, nor no path.
#[test]
fn a_translated_program_reads_off_the_descriptor_the_kernel_handed_its_translator() {
    let auxv = |pairs: &[(u64, u64)]| -> Vec<u8> {
        pairs
            .iter()
            .flat_map(|&(kind, value)| [kind.to_ne_bytes(), value.to_ne_bytes()])
            .flatten()
            .collect()
    };
    let translated = [
        (33, 275_810_927_144_960),
        (6, 4096),
        (3, 140_737_488_355_392),
        (9, 140_737_488_515_072),
        (31, 281_474_101_522_403),
        (15, 281_474_101_517_848),
        (2, 3),
        (27, 28),
        (28, 32),
        (0, 0),
    ];
    assert_eq!(procfs::exec_fd(&auxv(&translated)), Some(3));
    let native = [
        (6, 4096),
        (3, 215_624_163_786_816),
        (9, 215_624_163_789_760),
        (0, 0),
    ];
    assert_eq!(procfs::exec_fd(&auxv(&native)), None, "a native process");
    assert_eq!(
        procfs::exec_fd(&auxv(&[(6, 4096), (0, 0), (2, 3)])),
        None,
        "past AT_NULL"
    );

    assert!(procfs::exec_opened(
        "pos:\t64\nflags:\t0400040\nmnt_id:\t32\nino:\t797962\n"
    ));
    assert!(
        !procfs::exec_opened("pos:\t0\nflags:\t0400000\nmnt_id:\t25\nino:\t5\n"),
        "/dev/null put on the descriptor"
    );
    assert!(!procfs::exec_opened("pos:\t0\nmnt_id:\t25\n"), "no flags");
    assert!(
        !procfs::exec_opened("flags:\t+0400040\n"),
        "a sign is no octal digit"
    );
    assert_eq!(
        procfs::open_inode("pos:\t64\nflags:\t0400040\nmnt_id:\t32\nino:\t797962\n"),
        Some(797_962)
    );
    assert_eq!(
        procfs::open_inode("pos:\t64\nflags:\t0400040\nmnt_id:\t32\n"),
        None,
        "before 5.14"
    );
    assert_eq!(procfs::open_inode("ino:\t+797962\n"), None, "a sign");

    const MAPS: &[u8] = b"\
555555554000-555555555000 r--p 00000000 fd:01 797962                     /tmp/rp.OR2FwC/probe
555555555000-555555556000 r-xp 00001000 fd:01 797962                     /tmp/rp.OR2FwC/probe
555555558000-555555559000 rw-p 00003000 fd:01 797962                     /tmp/rp.OR2FwC/probe
55555555a000-55555555b000 r-xp 00001000 fd:01 262170                     /var/cache/rosettad/5e6b.aotcache
55555555b000-555555563000 r-xp 00000000 00:00 0
7fffff326000-7fffff34e000 r--p 00000000 fd:01 525283                     /usr/lib/x86_64-linux-gnu/libc.so.6
";
    assert!(procfs::maps_executable(MAPS, b"/tmp/rp.OR2FwC/probe"));
    assert!(!procfs::maps_executable(
        MAPS,
        b"/usr/lib/x86_64-linux-gnu/libc.so.6"
    ));
    assert!(
        !procfs::maps_executable(MAPS, b"/tmp/rp.OR2FwC/prob"),
        "a prefix"
    );
    assert!(
        !procfs::maps_executable(MAPS, b"/tmp/rp.OR2FwC/probe2"),
        "a longer name"
    );
    assert!(
        !procfs::maps_executable(MAPS, b""),
        "no path, though an anonymous row is executable"
    );
    let odd = b"555555555000-555555556000 r-xp 00001000 fd:01 797963   /tmp/a\\012b/probe\n";
    assert!(
        procfs::maps_executable(&odd[..], b"/tmp/a\nb/probe"),
        "maps prints a newline as \\012"
    );
    let read_only: Vec<u8> = MAPS
        .split(|&b| b == b'\n')
        .filter(|row| !row.windows(4).any(|w| w == b"r-xp"))
        .flat_map(|row| [row, b"\n"].concat())
        .collect();
    assert!(!procfs::maps_executable(
        read_only.as_slice(),
        b"/tmp/rp.OR2FwC/probe"
    ));
}

const TRICKY: &str = "7 (a) b (c) S 1 1 1 0 -1 4194368 63 0 0 0 0 0 0 0 20 0 1 0 51832760 \
     7032832 463 18446744073709551615 205458244435968 205458248030472 281474793899808 0 0 0 \
     0 128 0 1 0 0 17 0 0 0 0 0 0 205458248044264 205458248115556 205458772447232 \
     281474793901609 281474793901860 281474793901860 281474793902058 0\n";
const ZOMBIE: &str = "7 (a) b (c) Z 1 1 1 0 -1 4227148 140 0 0 0 0 0 0 0 20 0 1 0 51832760 0 \
     0 18446744073709551615 0 0 0 0 0 0 0 128 0 1 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n";
const CAT: &str = "13 (cat) R 1 1 1 0 -1 4194304 43 0 0 0 0 0 0 0 20 0 1 0 51831871 2424832 \
     192 18446744073709551615 218051104473088 218051104506384 281474237451360 0 0 0 0 0 0 0 \
     0 0 17 1 0 0 0 0 0 218051104603072 218051104604896 218051165171712 281474237452043 \
     281474237452063 281474237452063 281474237452267 0\n";
const STATUS_501: &str = "Name:\tcat\nState:\tR (running)\nTgid:\t13\nPid:\t13\n\
     Uid:\t501\t502\t501\t502\nGid:\t20\t20\t20\t20\n";

/// The ids `ps -o ppid=,pgid=,tpgid=` prints, counted from the command name's last `)` (a
/// name may hold `)` and spaces), `-1` with no terminal; a boot second and a start in ticks
/// make `ps -o lstart`'s whole second, cut short. Negative control: counting from the first
/// `)` reads `TRICKY`'s state as `b` and its parent as `(c)`, which is no number.
#[cfg(any(unix, windows))]
#[test]
fn a_linux_process_s_ids_and_wall_start_are_read_as_ps_prints_them() {
    assert_eq!(procfs::stat_ids(TRICKY, 7), Some((1, 1, -1)));
    assert_eq!(procfs::stat_ids(CAT, 13), Some((1, 1, -1)));
    assert_eq!(
        procfs::stat_ids("40 (bash) S 39 40 40 34816 41 4194560 1", 40),
        Some((39, 40, 41)),
        "on a terminal, another group in front"
    );
    assert_eq!(procfs::stat_ids(CAT, 14), None, "another pid's line");
    assert_eq!(procfs::stat_ids("13 (cat) R 1 1", 13), None, "cut short");
    assert_eq!(
        procfs::btime("cpu  1 2 3\nintr 9\nbtime 1794200000\nprocesses 7\n"),
        Some(1_794_200_000)
    );
    assert_eq!(procfs::btime("cpu  1 2 3\n"), None, "no boot line");
    assert_eq!(procfs::btime("btime -3\n"), None, "no whole second");
    assert_eq!(
        procfs::start_s(1_794_200_000, 51_831_871, 100),
        1_794_718_318
    );
    assert_eq!(
        procfs::start_s(1_794_200_000, 199, 100),
        1_794_200_001,
        "cut short"
    );
    assert_eq!(
        procfs::start_s(5, 7, 0),
        12,
        "no tick rate read as one a second"
    );
}

/// `stat` field 22 counted from the command name's last `)`, the effective uid as the second
/// `Uid:` id, a tick as its first instant; a process whose main thread alone exited has its
/// start, and every failure case answers `None`. Negative control: judge the first read's
/// state alone, and the process that died between the reads has a start.
#[cfg(any(unix, windows))]
#[test]
fn a_linux_start_is_read_off_proc_as_linux_writes_it() {
    assert_eq!(procfs::stat_start(TRICKY, 7), Some(('S', 51_832_760)));
    assert_eq!(procfs::stat_start(ZOMBIE, 7), Some(('Z', 51_832_760)));
    assert_eq!(procfs::stat_start(CAT, 13), Some(('R', 51_831_871)));
    assert_eq!(procfs::stat_start(CAT, 14), None, "another pid's line");
    assert_eq!(
        procfs::stat_start("13 (cat) R 1 1 1 0 -1 4194304 43 0 0", 13),
        None,
        "cut short"
    );
    assert_eq!(procfs::effective_uid(STATUS_501), Some(502));

    let at = |ticks: u64| Some(BootClock(ticks * 10_000_000));
    assert_eq!(
        procfs::start(CAT, STATUS_501, CAT, 13, 502, 100),
        at(51_831_871),
        "518318.71 s after boot"
    );
    let tricky_status = STATUS_501.replace("Tgid:\t13\nPid:\t13", "Tgid:\t7\nPid:\t7");
    assert_eq!(
        procfs::start(TRICKY, &tricky_status, TRICKY, 7, 502, 100),
        at(51_832_760)
    );
    let thread_status = STATUS_501.replace("Tgid:\t13\nPid:\t13", "Tgid:\t6\nPid:\t7");
    assert_eq!(
        procfs::start(TRICKY, &thread_status, TRICKY, 7, 502, 100),
        None,
        "a thread's id"
    );
    let main_thread_gone = ZOMBIE.replacen(" 20 0 1 0 ", " 20 0 3 0 ", 1);
    assert_eq!(
        procfs::start(
            &main_thread_gone,
            &tricky_status,
            &main_thread_gone,
            7,
            502,
            100
        ),
        at(51_832_760),
        "a process whose main thread alone exited (`Zl`) runs"
    );
    for (case, stat, again, euid, per_second) in [
        ("a zombie", ZOMBIE, ZOMBIE, 502, 100),
        ("dead", &*TRICKY.replace(") S ", ") X "), TRICKY, 502, 100),
        ("died between the reads", TRICKY, ZOMBIE, 502, 100),
        ("another effective uid", TRICKY, TRICKY, 501, 100),
        (
            "reused between the reads",
            TRICKY,
            &*CAT.replacen("13 ", "7 ", 1),
            502,
            100,
        ),
        ("no tick rate", TRICKY, TRICKY, 502, 0),
    ] {
        assert_eq!(
            procfs::start(stat, &tricky_status, again, 7, euid, per_second),
            None,
            "{case}"
        );
    }
    assert_eq!(
        procfs::tick_start(51_831_871, 100),
        Some(BootClock(518_318_710_000_000))
    );
    assert_eq!(procfs::tick_start(7, 3), Some(BootClock(2_333_333_333)));
}

/// A start Linux records in ticks precedes the real one by less than [`procfs::tick_span`]:
/// every tick's first instant ([`procfs::tick_start`]) lies within one span of the next, at
/// rates that divide a second (100, 250, 1000) and ones that do not (3, 1024, 7 million),
/// and no rate is no span. NEGATIVE CONTROL (scratch): a span rounded down fails at 3 a
/// second, whose tick 2 spans 333 333 334 ns, one past it.
#[test]
fn a_linux_start_tick_spans_every_tick_rounded_up() {
    assert_eq!(procfs::tick_span(100), Some(10_000_000));
    assert_eq!(procfs::tick_span(0), None);
    for per_second in [3, 100, 250, 1000, 1024, 7_000_000] {
        let span = procfs::tick_span(per_second).expect("a span");
        for ticks in [0, 1, 2, 3, 1023, 51_831_871] {
            let first = procfs::tick_start(ticks, per_second).expect("this tick").0;
            let next = procfs::tick_start(ticks + 1, per_second)
                .expect("the next")
                .0;
            assert!(
                next - first <= span,
                "{per_second}/s: tick {ticks} spans {} > {span}",
                next - first
            );
        }
    }
}

/// Whether a process has exec'd since its fork, off `/proc/<pid>/stat` as Linux writes it:
/// the flags, field 9 of the line (the 7th after the command name's last `)`), without
/// `PF_FORKNOEXEC` (0x40). TRICKY's 4194368 is `PF_RANDOMIZE | PF_FORKNOEXEC`, a fork that
/// has not exec'd; CAT's 4194304 has exec'd; another pid's line or one cut short reads
/// nothing. NEGATIVE CONTROL (scratch): test `PF_RANDOMIZE` (0x400000) instead, and CAT reads
/// as not exec'd.
#[cfg(any(unix, windows))]
#[test]
fn a_linux_exec_since_the_fork_is_read_off_the_stat_flags() {
    assert_eq!(procfs::execd_row(CAT, 13), Some(true), "exec'd");
    assert_eq!(procfs::execd_row(TRICKY, 7), Some(false), "forked, no exec");
    assert_eq!(
        procfs::execd_row(&CAT.replacen(" 4194304 ", " 4194368 ", 1), 13),
        Some(false),
        "the same line forked"
    );
    assert_eq!(procfs::execd_row(CAT, 14), None, "another pid's line");
    assert_eq!(
        procfs::execd_row("13 (cat) R 1 1 1 0 -1 4194304 43 0 0", 13),
        None,
        "cut short"
    );
}

/// The kernel's own record: a forked child parked before any exec reads `Some(false)`, and a
/// spawned child (an exec) and this test process read `Some(true)`, on macOS (the BSD info's
/// `PROC_FLAG_EXEC`) and Linux (`/proc/<pid>/stat`). NEGATIVE CONTROL (scratch): a reader
/// that answers `Some(true)` for every live process fails "a fork that has not exec'd".
#[cfg(unix)]
#[test]
fn a_forked_child_reads_as_not_execd_until_it_execs() {
    use std::os::fd::AsRawFd as _;
    let own = i32::try_from(std::process::id()).unwrap();
    let wired = cfg!(any(target_vendor = "apple", target_os = "linux"));
    if !wired {
        assert_eq!(super::read_process_execd(own), None);
        return;
    }
    assert_eq!(super::read_process_execd(own), Some(true), "this process");
    let mut spawned = crate::spawnfd::command("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("spawn /bin/sleep");
    let spawned_pid = i32::try_from(spawned.id()).unwrap();
    let spawned_execd = super::read_process_execd(spawned_pid);
    let _ = spawned.kill();
    let _ = spawned.wait();
    assert_eq!(spawned_execd, Some(true), "a spawned child");
    let (go_r, go_w) = crate::pipe().expect("pipe");
    let (go_fd, go_w_fd) = (go_r.as_raw_fd(), go_w.as_raw_fd());
    // The child waits on the read end, which is kept out of every fork it is not handed to.
    let handing = crate::spawn_lock::handing(&[go_fd]);
    // SAFETY: the child calls only async-signal-safe functions (`close`, `read`, `_exit`)
    // on descriptors opened before `fork`, then exits without unwinding.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork: {}", std::io::Error::last_os_error());
    if pid == 0 {
        let mut b = 0u8;
        // SAFETY: as above.
        unsafe {
            libc::close(go_w_fd);
            loop {
                let n = libc::read(go_fd, (&raw mut b).cast(), 1);
                if n >= 0
                    || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
                {
                    break;
                }
            }
            libc::_exit(0);
        }
    }
    drop(handing);
    drop(go_r);
    let forked = super::read_process_execd(pid);
    drop(go_w);
    let mut status = 0;
    // SAFETY: `pid` is this test's own child, reaped once here.
    let _ = unsafe { libc::waitpid(pid, &raw mut status, 0) };
    assert_eq!(forked, Some(false), "a fork that has not exec'd");
}

/// A status whose thread group is `pid`, with effective uid 502.
#[cfg(any(unix, windows))]
fn status_of(pid: i32) -> String {
    STATUS_501.replace("Tgid:\t13\nPid:\t13", &format!("Tgid:\t{pid}\nPid:\t{pid}"))
}

/// A lineage off `/proc` as Linux writes it: the start tick as both birth and unique id, no
/// pid version, and the group and terminal of the second read (a terminal the kernel prints
/// negative is its `int` with the sign bit set); a process whose main thread alone exited
/// reads, and every case in the table reads nothing. Negative controls: drop the second
/// read's start or state check, and the reused pid or the process that died between the
/// reads reads live; map the terminal to `None`, or read one that does not convert as none
/// (`parse_stat`), and a member with one would be proven free of it; judge a row's state
/// alone, whatever its threads, and the running process reads nothing.
#[cfg(any(unix, windows))]
#[test]
fn a_linux_lineage_is_read_off_proc_as_linux_writes_it() {
    use super::{Birth, Identity, Lineage};
    let status = status_of(7);
    assert_eq!(
        procfs::lineage(TRICKY, &status, TRICKY, 7, 502),
        Some(Lineage {
            identity: Identity {
                pid: 7,
                birth: Birth(51_832_760),
                idversion: None,
            },
            uniqueid: 51_832_760,
            parent: 1,
            pgid: 1,
            launcher_version: None,
            tty: None,
        })
    );
    let grouped = TRICKY.replacen(") S 1 1 1 0 -1 ", ") S 1 500 500 34817 -1 ", 1);
    let read = procfs::lineage(&grouped, &status, &grouped, 7, 502).expect("a member");
    assert_eq!((read.pgid, read.tty), (500, Some(34817)), "{read:?}");
    let far = TRICKY.replacen(") S 1 1 1 0 -1 ", ") S 1 500 500 -2147448832 -1 ", 1);
    let read = procfs::lineage(&far, &status, &far, 7, 502).expect("a member");
    assert_eq!(
        read.tty,
        Some(0x8000_8800),
        "pts 524288, past 2^19: {read:?}"
    );
    let main_thread_gone = ZOMBIE.replacen(" 20 0 1 0 ", " 20 0 3 0 ", 1);
    assert_eq!(
        procfs::lineage(&main_thread_gone, &status, &main_thread_gone, 7, 502).map(|l| l.identity),
        Some(Identity {
            pid: 7,
            birth: Birth(51_832_760),
            idversion: None,
        }),
        "a process whose main thread alone exited (`Zl`) runs"
    );
    let unread_tty = TRICKY.replacen(") S 1 1 1 0 -1 ", ") S 1 1 1 x -1 ", 1);
    let cut = &TRICKY[..40];
    for (case, stat, again, status, euid) in [
        ("a zombie", ZOMBIE, ZOMBIE, status.clone(), 502),
        (
            "dead",
            &*TRICKY.replace(") S ", ") X "),
            TRICKY,
            status.clone(),
            502,
        ),
        (
            "died between the reads",
            TRICKY,
            ZOMBIE,
            status.clone(),
            502,
        ),
        ("another effective uid", TRICKY, TRICKY, status.clone(), 501),
        (
            "reused between the reads",
            TRICKY,
            &*CAT.replacen("13 ", "7 ", 1),
            status.clone(),
            502,
        ),
        ("a thread's id", TRICKY, TRICKY, status_of(6), 502),
        (
            "a terminal that does not read",
            &*unread_tty,
            &*unread_tty,
            status.clone(),
            502,
        ),
        ("cut short", cut, cut, status.clone(), 502),
        ("another pid's line", CAT, CAT, status.clone(), 502),
    ] {
        assert_eq!(
            procfs::lineage(stat, &status, again, 7, euid),
            None,
            "{case}"
        );
    }
}

/// A group is listed from every `/proc/<pid>/stat`: its live members by `pgrp` (a command
/// name of `x) (y` and a `Z` row with three threads among them), never its zombie leader or a
/// dead row; a gone row is skipped, and an unreadable or garbled row, more members than the
/// bound, or a `/proc` that does not list every process leave it incomplete. Negative
/// controls: stop skipping zombies, and the exited leader is listed; skip every `Z` row, and
/// the running process is left out; read an unreadable row as gone, or start the listing
/// complete whatever `/proc` shows, and a listing with a hole reads complete.
#[cfg(any(unix, windows))]
#[test]
fn a_linux_group_is_listed_from_proc_without_its_zombies() {
    use procfs::StatRead;
    let row = |pid: i32, state: char, pgrp: u32| {
        let stat = format!(
            "{pid} (sh) {state} 1 {pgrp} {pgrp} 0 -1 4194368 63 0 0 0 0 0 0 0 20 0 1 0                  51832760 7032832 463 18446744073709551615 0 0 0 0 0 0 0 128 0 1 0 0 17 0 0 0
"
        );
        (pid, StatRead::Text(stat))
    };
    let odd = TRICKY.replacen("7 (a) b (c) S 1 1 1 ", "502 (x) (y) S 1 500 500 ", 1);
    let main_thread_gone = match row(506, 'Z', 500) {
        (pid, StatRead::Text(stat)) => (
            pid,
            StatRead::Text(stat.replacen(" 20 0 1 0 ", " 20 0 3 0 ", 1)),
        ),
        other => other,
    };
    let rows = || {
        vec![
            row(500, 'Z', 500),
            row(501, 'S', 500),
            (502, StatRead::Text(odd.clone())),
            row(503, 'S', 600),
            (504, StatRead::Gone),
            row(505, 'X', 500),
            main_thread_gone.clone(),
        ]
    };
    let listed = procfs::group_of(rows(), 500, true);
    assert_eq!(listed.pids, [501, 502, 506], "{listed:?}");
    assert!(listed.complete, "{listed:?}");
    let hidden = procfs::group_of(rows(), 500, false);
    assert_eq!(hidden.pids, [501, 502, 506], "{hidden:?}");
    assert!(
        !hidden.complete,
        "a /proc that does not list every process: {hidden:?}"
    );
    let mut holed = rows();
    holed.push((506, StatRead::Unreadable));
    assert!(
        !procfs::group_of(holed, 500, true).complete,
        "a row that did not read"
    );
    let mut garbled = rows();
    garbled.push((507, StatRead::Text("507 (sh) S 1 500".to_owned())));
    assert!(
        !procfs::group_of(garbled, 500, true).complete,
        "a row that did not parse"
    );
    let many = (1000..1257).map(|pid| row(pid, 'S', 500));
    let full = procfs::group_of(many, 500, true);
    assert_eq!(full.pids.len(), super::MAX_GROUP_PIDS);
    assert!(!full.complete, "past the bound");
    assert_eq!(procfs::parent("PPid:\t+12\n"), None);
    assert_eq!(
        procfs::effective_uid("Uid:\t1000\t+1000\t1000\t1000\n"),
        None
    );
    assert_eq!(procfs::thread_group("Tgid:\t+500\n"), None);
}

/// A member that forks and exits while the walk reads `/proc` leaves its child unlisted, so
/// the listing follows every id allocated behind it: a relayed member is listed, and
/// allocations it cannot follow leave the listing incomplete.
/// Negative control: return [`procfs::group_of`] over the walk alone from
/// [`procfs::chased_group_of`], and the relayed member is missed in a complete listing.
#[test]
fn a_linux_group_listing_follows_the_ids_allocated_behind_its_walk() {
    use procfs::StatRead;
    let row = |pid: i32, state: char, pgrp: u32| {
        StatRead::Text(format!(
            "{pid} (sh) {state} 1 {pgrp} {pgrp} 0 -1 4194368 63 0 0 0 0 0 0 0 20 0 1 0 \
             51832760 7032832 463 18446744073709551615 0 0 0 0 0 0 0 128 0 1 0 0 17 0 0 0\n"
        ))
    };
    // The walk read the relaying member 501 a zombie; it forked 103 first, which forked 105,
    // and 104 is another process's thread.
    let walked = || vec![(100, row(100, 'S', 600)), (501, row(501, 'Z', 500))];
    let chased = |id: i32| match id {
        103 => row(103, 'Z', 500),
        104 => StatRead::Gone,
        105 => row(105, 'S', 500),
        _ => StatRead::Gone,
    };
    let reads = |seq: Vec<Option<u32>>| {
        let mut seq = seq.into_iter();
        move || seq.next().flatten()
    };
    let listed = procfs::chased_group_of(
        walked(),
        Some(102),
        reads(vec![Some(104), Some(105), Some(105)]),
        || None,
        chased,
        500,
        true,
    );
    assert_eq!(listed.pids, [105], "{listed:?}");
    assert!(listed.complete, "{listed:?}");
    let mut last = 102_u32;
    let moving = procfs::chased_group_of(
        walked(),
        Some(102),
        move || {
            last += 1;
            Some(last)
        },
        || None,
        chased,
        500,
        true,
    );
    assert!(
        !moving.complete,
        "ids still allocated after every round: {moving:?}"
    );
    let unread = procfs::chased_group_of(walked(), None, || Some(102), || None, chased, 500, true);
    assert!(!unread.complete, "no last id before the walk: {unread:?}");
    let wrapped = procfs::chased_group_of(
        walked(),
        Some(4_194_302),
        reads(vec![Some(301)]),
        || None,
        chased,
        500,
        true,
    );
    assert!(!wrapped.complete, "a wrap with no pid_max: {wrapped:?}");
    assert_eq!(
        procfs::allocated_after(4_194_301, 2, Some(4_194_304), 8),
        Some(vec![4_194_302, 4_194_303, 1, 2])
    );
    assert_eq!(procfs::allocated_after(7, 7, None, 8), Some(Vec::new()));
    assert_eq!(procfs::allocated_after(7, 9, None, 8), Some(vec![8, 9]));
    assert_eq!(
        procfs::allocated_after(7, 20, None, 8),
        None,
        "past the limit"
    );
}

/// An id the walk read as a bystander's can be freed and handed to a member's fork before the
/// walk is judged (the kernel hands out the lowest free id at its cursor, so after a wrap the
/// next fork takes an id just freed above it): the chase reads every id allocated behind the
/// walk again, the walk's row of it replaced, and lists the member. Negative control: skip an
/// id the walk listed (the chase's earlier rule), and the listing is complete with the member
/// missed.
#[test]
fn a_listing_reads_again_an_id_its_walk_listed_that_was_allocated_behind_it() {
    use procfs::StatRead;
    let row = |pid: i32, state: char, pgrp: u32| {
        StatRead::Text(format!(
            "{pid} (sh) {state} 1 {pgrp} {pgrp} 0 -1 4194368 63 0 0 0 0 0 0 0 20 0 1 0 \
             51832760 7032832 463 18446744073709551615 0 0 0 0 0 0 0 128 0 1 0 0 17 0 0 0\n"
        ))
    };
    // The walk read 1001 a bystander's, then 9000, the member that forked 1001 once the
    // bystander was reaped, a zombie.
    let walked = vec![(1001, row(1001, 'S', 600)), (9000, row(9000, 'Z', 500))];
    let mut reads = vec![Some(1001), Some(1001)].into_iter();
    let listed = procfs::chased_group_of(
        walked,
        Some(1000),
        move || reads.next().flatten(),
        || None,
        |id| match id {
            1001 => row(1001, 'S', 500),
            _ => StatRead::Gone,
        },
        500,
        true,
    );
    assert_eq!(listed.pids, [1001], "{listed:?}");
    assert!(listed.complete, "{listed:?}");
}

/// An id a listing follows past its walk is read as a process only when it leads its thread
/// group: this process's id reads, and a thread of it reads as gone, its process listed on a
/// row of its own. Negative control: drop the `Tgid` match from [`process_row`], and the
/// thread reads as a process.
#[cfg(target_os = "linux")]
#[test]
fn an_id_followed_past_the_walk_is_read_as_a_process_only_when_it_leads_its_group() {
    let me = i32::try_from(std::process::id()).expect("a pid");
    assert!(
        matches!(super::process_row(me), procfs::StatRead::Text(_)),
        "this process reads"
    );
    let (tid_tx, tid_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::spawn(move || {
        // SAFETY: `gettid` takes nothing and touches no memory.
        let tid = unsafe { libc::syscall(libc::SYS_gettid) };
        tid_tx
            .send(i32::try_from(tid).expect("a tid"))
            .expect("send the tid");
        done_rx.recv().ok();
    });
    let tid = tid_rx.recv().expect("the thread's id");
    assert_ne!(tid, me);
    assert!(
        matches!(super::process_row(tid), procfs::StatRead::Gone),
        "a thread is no process row"
    );
    done_tx.send(()).expect("release the thread");
    thread.join().expect("the thread ends");
}

/// `/proc` lists every process only under a `proc` mount at `/proc` with no `hidepid`, or
/// `hidepid=0` or `off`; a `proc` mount elsewhere and other file systems under `/proc` do
/// not count. Negative control: read `invisible` as showing every process, and a `/proc` that
/// hides this uid's non-dumpable processes reads as listing them.
#[test]
fn a_proc_mounted_to_hide_processes_shows_no_listing_whole() {
    let line = |at: &str, fs: &str, options: &str| {
        format!(
            "22 1 0:21 / {at} rw,nosuid,nodev,noexec,relatime shared:12 - {fs} {fs} {options}\n"
        )
    };
    let table = |options: &str| {
        [
            line("/", "ext4", "rw"),
            line("/proc", "proc", options),
            line("/proc/sys/fs/binfmt_misc", "binfmt_misc", "rw,relatime"),
            line("/proc/kcore", "tmpfs", "rw,size=0k"),
            line("/mnt/proc", "proc", "rw,hidepid=invisible"),
        ]
        .concat()
    };
    for options in ["rw", "rw,hidepid=off", "rw,hidepid=0"] {
        assert!(procfs::shows_every_process(&table(options)), "{options}");
    }
    for options in [
        "rw,hidepid=invisible",
        "rw,hidepid=2",
        "rw,hidepid=ptraceable",
        "rw,hidepid=4",
        "rw,hidepid=noaccess",
        "rw,hidepid=1",
        "rw,gid=27,hidepid=invisible,subset=pid",
    ] {
        assert!(!procfs::shows_every_process(&table(options)), "{options}");
    }
    let stacked = [table("rw"), line("/proc", "proc", "rw,hidepid=invisible")].concat();
    assert!(
        !procfs::shows_every_process(&stacked),
        "a hiding mount over it"
    );
    assert!(
        !procfs::shows_every_process(&line("/", "ext4", "rw")),
        "no /proc mount"
    );
    assert!(!procfs::shows_every_process(""), "no mount table");
}

/// A listing's row is gone only when the kernel says its process is (`ENOENT`, `ESRCH`):
/// `EPERM`, a `hidepid=1` mount's answer for another user's process, and every other error
/// leave a hole. Negative control: read a refused permission as gone, and the listing with a
/// hidden row reads whole.
#[cfg(unix)]
#[test]
fn a_row_the_kernel_will_not_show_leaves_the_listing_partial() {
    use std::io::{Error, ErrorKind};
    let member = "501 (sh) S 1 500 500 0 -1 4194368 63 0 0 0 0 0 0 0 20 0 1 0 51832760 0 0\n";
    assert_eq!(
        super::stat_read(Ok(member.to_owned())),
        procfs::StatRead::Text(member.to_owned())
    );
    for (case, error, whole) in [
        ("gone", Error::from(ErrorKind::NotFound), true),
        (
            "exited by its read",
            Error::from_raw_os_error(libc::ESRCH),
            true,
        ),
        (
            "hidepid=1's refusal",
            Error::from_raw_os_error(libc::EPERM),
            false,
        ),
        (
            "a refused open",
            Error::from_raw_os_error(libc::EACCES),
            false,
        ),
    ] {
        let listed = procfs::group_of(
            [
                (501, super::stat_read(Ok(member.to_owned()))),
                (9, super::stat_read(Err(error))),
            ],
            500,
            true,
        );
        assert_eq!(listed.pids, [501], "{case}");
        assert_eq!(listed.complete, whole, "{case}");
    }
}

/// A `/proc` file whose command name holds no UTF-8 still reads
/// ([`super::read_proc_text`]): its lineage, start, group row and exited row are judged.
/// Negative control: read the files with `read_to_string`, and the lineage reads nothing and
/// the row is unreadable.
#[cfg(unix)]
#[test]
fn a_proc_file_whose_command_name_is_not_utf8_still_reads() {
    use super::{read_proc_text, stat_row};
    let dir = aterm_tempfile::TempDir::new().expect("tempdir");
    let cut_name = |text: &str| {
        let mut bytes = text.as_bytes().to_vec();
        let at = bytes
            .windows(3)
            .position(|w| w == b"(a)")
            .expect("the name")
            + 1;
        bytes.splice(at..=at, [0xe3, 0x83]);
        bytes
    };
    let stat = dir.path().join("stat");
    let status = dir.path().join("status");
    std::fs::write(&stat, cut_name(TRICKY)).expect("stat");
    let mut raw_status = status_of(7).into_bytes();
    raw_status.splice(6..9, [0xe3, 0x83]);
    std::fs::write(&status, raw_status).expect("status");
    assert!(
        std::fs::read_to_string(&stat).is_err() && std::fs::read_to_string(&status).is_err(),
        "the fixture's names hold no UTF-8"
    );
    let read = |path: &std::path::Path| read_proc_text(path).expect("a file that reads");
    let lineage = procfs::lineage(&read(&stat), &read(&status), &read(&stat), 7, 502);
    assert_eq!(lineage.map(|l| l.uniqueid), Some(51_832_760), "{lineage:?}");
    assert!(
        procfs::start(&read(&stat), &read(&status), &read(&stat), 7, 502, 100).is_some(),
        "its start"
    );
    assert!(
        matches!(stat_row(&stat), procfs::StatRead::Text(_)),
        "a group row that reads"
    );
    let zombie = dir.path().join("zombie");
    std::fs::write(&zombie, cut_name(ZOMBIE)).expect("zombie");
    assert!(procfs::exited_row(&read(&zombie), 7), "an exited row");
    assert!(!procfs::exited_row(&read(&stat), 7), "a live row");
    assert_eq!(
        stat_row(&dir.path().join("gone")),
        procfs::StatRead::Gone,
        "a file gone by its read"
    );
}

#[cfg(unix)]
#[test]
fn a_child_started_after_a_boot_clock_reading_reads_later() {
    use std::time::{Duration, Instant};
    let own = i32::try_from(std::process::id()).unwrap();
    let Some(moment) = super::boot_clock_now() else {
        if super::translation() == super::Translation::Translated {
            a_child_started_after_a_wall_reading_reads_later();
            return;
        }
        let wired = cfg!(any(target_vendor = "apple", target_os = "linux"));
        assert!(!wired, "macOS and Linux have a boot clock");
        assert_eq!(super::read_process_start(own), None);
        return;
    };
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let step = {
        let (_, per_second) = super::linux_reader().expect("linux clock ticks");
        (1_000_000_000u64 / per_second.max(1))
            .saturating_mul(2)
            .max(1)
    };
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let step = 1u64;
    let deadline = Instant::now() + Duration::from_secs(60);
    while super::boot_clock_now().expect("boot clock").0 < moment.0.saturating_add(step) {
        assert!(
            Instant::now() < deadline,
            "boot clock did not advance past {step}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut child = crate::spawnfd::command("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("spawn /bin/sleep");
    let pid = i32::try_from(child.id()).unwrap();
    let started = super::read_process_start(pid);
    let after = super::boot_clock_now().expect("the boot clock again");
    let wall = super::read_process_birth(pid);
    let _ = child.kill();
    let _ = child.wait();
    let started = started.expect("a live child's start");
    assert!(started.boot > moment, "{started:?} after {moment:?}");
    assert!(started.boot <= after, "{started:?} by {after:?}");
    assert_eq!(started.wall, wall, "the wall record beside it");
    assert_eq!(started.wall.is_some(), cfg!(target_vendor = "apple"));
    let mine = super::read_process_start(own).expect("this process's start");
    assert!(mine.boot < moment, "{mine:?} before {moment:?}");
    assert_eq!(super::read_process_start(pid), None, "an ended process");
    assert_eq!(super::read_process_start(1), None, "never pid 1");
}

#[cfg(unix)]
fn a_child_started_after_a_wall_reading_reads_later() {
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    let moment = SystemTime::now();
    let deadline = Instant::now() + Duration::from_secs(60);
    while SystemTime::now().duration_since(moment).unwrap_or_default() < Duration::from_millis(2) {
        assert!(Instant::now() < deadline, "wall clock did not advance");
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut child = crate::spawnfd::command("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("spawn /bin/sleep");
    let started = super::read_process_start(i32::try_from(child.id()).unwrap());
    let _ = child.kill();
    let _ = child.wait();
    let birth = started
        .and_then(|s| s.wall)
        .expect("a translated reader still reads the start record");
    let wall =
        UNIX_EPOCH + Duration::from_secs(birth.seconds) + Duration::from_micros(birth.microseconds);
    assert!(wall > moment, "{wall:?} after {moment:?}");
}

#[test]
fn a_translated_process_reads_no_boot_clock() {
    let native_ticks = 16_941_646_465_128;
    assert_eq!(
        super::apple_boot_clock(super::Translation::Native, || native_ticks),
        Some(BootClock(native_ticks))
    );
    assert_eq!(
        super::apple_boot_clock(super::Translation::Unknown, || native_ticks),
        Some(BootClock(native_ticks))
    );
    assert_eq!(
        super::apple_boot_clock(super::Translation::Translated, || {
            panic!("a translated process's reading is never taken")
        }),
        None
    );
    if cfg!(target_vendor = "apple") {
        assert_eq!(
            super::boot_clock_now().is_none(),
            super::translation() == super::Translation::Translated
        );
    } else {
        assert_eq!(super::translation(), super::Translation::Native);
    }
}
