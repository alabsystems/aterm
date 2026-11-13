// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A sweep whose table was wanted and not read leaves every process's `reaps_unwaited`
/// unread and reads no process's own record, which would wait on what the table waited on;
/// a table read fills it in where its row names the swept parent; and a sweep of no more
/// than `UNWAITED_AT_ONCE` reads each one's record. NEGATIVE CONTROL: before 2026-11-06 an
/// unread table fell back to each one's record (`one` called 13 times here).
#[cfg(target_os = "macos")]
#[test]
fn an_unread_table_leaves_reaps_unwaited_unread_and_reads_no_record() {
    let sweep = |n: u32| -> Vec<ProcessReading> {
        (1..=n)
            .map(|pid| ProcessReading {
                pid,
                ppid: 1,
                ..ProcessReading::default()
            })
            .collect()
    };
    let ones = std::cell::Cell::new(0);
    let one = |_: u32| {
        ones.set(ones.get() + 1);
        Some((1, true))
    };
    let mut procs = sweep(13);
    imp::read_unwaited_by(&mut procs, || None, one);
    assert_eq!(ones.get(), 0, "no process's own record is read");
    assert!(procs.iter().all(|p| p.reaps_unwaited.is_none()));

    let mut procs = sweep(13);
    imp::read_unwaited_by(&mut procs, || Some(vec![(1, 1, true), (2, 9, true)]), one);
    assert_eq!(ones.get(), 0);
    assert_eq!(procs[0].reaps_unwaited, Some(true));
    assert_eq!(
        procs[1].reaps_unwaited, None,
        "its row names another parent"
    );
    assert_eq!(procs[2].reaps_unwaited, None, "no row");

    let mut procs = sweep(3);
    imp::read_unwaited_by(&mut procs, || panic!("a small sweep reads no table"), one);
    assert_eq!(ones.get(), 3);
    assert!(procs.iter().all(|p| p.reaps_unwaited == Some(true)));
}

/// A name at its reader's cut is the first longer name that begins with it and is below its
/// own cut; one below the cut is whole and pulls no longer reader; a longer reader's name that
/// does not begin with the cut one (another program's) is passed over; and one no reader
/// holds whole ends in the mark, after the longest cut name read, whole characters only.
/// NEGATIVE CONTROL: the cut name the Memory lens once showed as whole, with no longer reader,
/// is marked.
#[test]
fn a_name_at_its_readers_cut_is_read_whole_or_marked() {
    type Longer = Option<(Vec<u8>, Option<usize>)>;
    let named = |n: &str, cut: Option<usize>| -> Longer { Some((n.as_bytes().to_vec(), cut)) };
    let never = || -> Longer { panic!("a whole name pulls no longer reader") };
    let webkit = "com.apple.WebKit.WebContent";
    assert_eq!(
        whole_name(&webkit.as_bytes()[..15], 15, [named(webkit, Some(31))]),
        webkit
    );
    assert_eq!(
        whole_name(b"WindowServer", 15, std::iter::once_with(never)),
        "WindowServer"
    );
    let fifteen = "SetStoreUpdate1";
    assert_eq!(
        whole_name(fifteen.as_bytes(), 15, [named(fifteen, None)]),
        fifteen,
        "a reader that cuts nothing says a name at the cut is whole"
    );
    let long = "aterm_sysprobe-0123456789abcdef0123";
    let image = named(long, None);
    assert_eq!(
        whole_name(
            &long.as_bytes()[..15],
            15,
            [None, named(&long[..31], Some(31)), image.clone()]
        ),
        long,
        "past a refused reader and a cut one, the image's basename"
    );
    assert_eq!(
        whole_name(
            &long.as_bytes()[..15],
            15,
            [named(&long[..31], Some(31)), None]
        ),
        format!("{}\u{2026}", &long[..31]),
        "the longest cut name read, marked"
    );
    assert_eq!(
        whole_name(b"averyverylongsy", 15, [named("sleep", None)]),
        "averyverylongsy\u{2026}",
        "another program's name is passed over"
    );
    let cyrillic = "Приложение";
    let at_cut = &cyrillic.as_bytes()[..15];
    assert_eq!(whole_name(at_cut, 15, [named(cyrillic, None)]), cyrillic);
    assert_eq!(
        whole_name(at_cut, 15, std::iter::empty()),
        "Приложе\u{2026}",
        "a character the cut split is dropped before the mark"
    );
    assert_eq!(
        whole_name(&webkit.as_bytes()[..15], 15, std::iter::empty()),
        "com.apple.WebKi\u{2026}",
        "CONTROL"
    );
}

#[test]
fn priority_minus_one_is_a_value_only_without_errno() {
    assert_eq!(priority_result(-1, 0), Ok(-1));
    assert_eq!(priority_result(0, 0), Ok(0));
    assert_eq!(priority_result(-1, 1), Err(1));
    assert_eq!(priority_result(-1, 3), Err(3));
}

/// This process's own reading has its scheduling, its CPU classes scaled by the timebase,
/// and the pages it holds in physical memory now (`ri_resident_size`), which a live process
/// running this test has. NEGATIVE CONTROL: a reading that left `resident` unread is `None`.
#[cfg(target_os = "macos")]
#[test]
fn own_scheduling_is_read_and_cpu_classes_are_scaled() {
    let mine = processes()
        .unwrap()
        .procs
        .into_iter()
        .find(|p| p.pid == std::process::id())
        .unwrap();
    assert!(
        mine.resident.is_some_and(|r| r > 0),
        "a live process has pages in memory: {:?}",
        mine.resident
    );
    let s = mine
        .scheduling
        .expect("a live Darwin process has scheduling readings");
    assert!(s.nice.is_ok(), "{s:?}");
    assert!(s.role.is_ok(), "{s:?}");
    let ns = s
        .qos_cpu_ns
        .expect("kernel timebase")
        .expect("own rusage classes");
    assert!(ns.iter().any(|n| *n > 0));
    // The kernel charges the QoS classes on its own clock, apart from user plus system
    // time: a binary started outside cargo books its first millisecond to `default` and
    // its classes sum a few percent over its CPU time. Both go through the same timebase,
    // so a dropped or doubled scaling (125/3 on Apple silicon) shows as a factor of ~40.
    let (classes, cpu) = (ns.iter().copied().sum::<u64>(), mine.cpu_ns.unwrap());
    assert!(
        classes <= cpu * 4 && cpu <= classes * 4,
        "classes {classes} ns, cpu {cpu} ns"
    );
}

#[test]
fn statm_gives_the_resident_pages() {
    assert_eq!(parse_statm_resident("2048 512 100 1 0 300 0"), Some(512));
    assert_eq!(parse_statm_resident("2048"), None);
}

#[test]
fn mach_ticks_scale_by_the_timebase() {
    assert_eq!(ticks_to_ns(3, 125, 3), 125);
    assert_eq!(ticks_to_ns(7, 1, 1), 7);
    assert_eq!(ticks_to_ns(7, 1, 0), 7);
    assert_eq!(ticks_to_ns(u64::MAX, 125, 3), u64::MAX);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn sweep_start_identity_matches_the_control_peers_kernel_record() {
    let pid = std::process::id();
    let expected = aterm_uds::process::read_process_start(i32::try_from(pid).unwrap())
        .expect("own process has a kernel start record");
    let sweep = processes().expect("native process sweep");
    let me = sweep
        .procs
        .iter()
        .find(|process| process.pid == pid)
        .unwrap();
    assert_eq!(me.start, Some(expected.boot.0));
}

#[test]
fn a_boot_clock_start_is_its_wall_birth() {
    const NOW_MS: i64 = 1_790_000_000_000;
    let boot = 3_600 * 1_000_000_000;
    assert_eq!(
        birth_of_boot_start(boot - 47 * 60 * 1_000_000_000, boot, NOW_MS),
        Some(NOW_MS - 47 * 60_000)
    );
    assert_eq!(
        birth_of_boot_start(boot + 5_000_000, boot, NOW_MS),
        Some(NOW_MS),
        "born while the sweep ran"
    );
    assert_eq!(
        birth_of_boot_start(0, u64::MAX, NOW_MS),
        None,
        "before the epoch"
    );
    assert_eq!(
        birth_of_boot_start(0, boot, i64::MIN),
        None,
        "past the range"
    );
}

#[test]
fn a_filetime_is_its_unix_birth() {
    assert_eq!(birth_of_filetime(116_444_736_000_000_000), Some(0));
    assert_eq!(
        birth_of_filetime(116_444_736_000_000_000 + 17_900_000_000_000_000 + 12_345),
        Some(1_790_000_000_001)
    );
    assert_eq!(birth_of_filetime(116_444_735_999_999_999), None);
    assert_eq!(birth_of_filetime(u64::MAX), Some(1_833_029_933_770_955));
}

/// Live on Linux: this process's swept birth is `ps`'s reckoning of it (`/proc/stat`'s
/// `btime` plus its start in clock ticks), within `btime`'s whole second.
#[cfg(target_os = "linux")]
#[cfg_attr(
    target_pointer_width = "64",
    expect(
        clippy::useless_conversion,
        reason = "sysconf's c_long is i64 on LP64 and i32 on 32-bit"
    )
)]
#[test]
fn the_linux_sweep_reads_this_processs_birth_as_ps_reckons_it() {
    let me = std::process::id();
    let swept = processes()
        .expect("a sweep")
        .procs
        .into_iter()
        .find(|p| p.pid == me)
        .and_then(|p| p.birth_ms)
        .expect("this process's birth");
    let stat = std::fs::read_to_string("/proc/stat").expect("/proc/stat");
    let btime: i64 = stat
        .lines()
        .find_map(|l| l.strip_prefix("btime "))
        .and_then(|b| b.trim().parse().ok())
        .expect("btime");
    let own = std::fs::read_to_string(format!("/proc/{me}/stat")).expect("its stat");
    let start = crate::jobs::linux::parse_stat(&own)
        .expect("its start")
        .start;
    // SAFETY: `sysconf` reads a constant.
    let hz = i64::from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).max(1);
    let ps_ms = btime * 1000 + i64::try_from(start).expect("ticks") * 1000 / hz;
    // `btime` is a whole second and `/proc/uptime` a centisecond, each cut short, so the
    // two reckonings part by up to 1.01 s plus the milliseconds each division drops.
    assert!(
        (swept - ps_ms).abs() <= 1_100,
        "swept {swept}, ps reckons {ps_ms}"
    );
}

/// The macOS sweep reads no wall birth: its start is Mach absolute time, which stops while
/// the Mac sleeps, so an age from it would read hours young after a night's sleep.
#[cfg(target_os = "macos")]
#[test]
fn the_macos_sweep_leaves_the_birth_to_the_kernel_record() {
    let sweep = processes().expect("a sweep");
    assert!(sweep.procs.iter().all(|p| p.birth_ms.is_none()));
}

#[test]
fn a_tick_frequency_reduces_to_its_timebase() {
    assert_eq!(timebase_of_frequency(24_000_000), Some((125, 3)));
    assert_eq!(timebase_of_frequency(1_000_000_000), Some((1, 1)));
    assert_eq!(timebase_of_frequency(19_200_000), Some((625, 12)));
    assert_eq!(timebase_of_frequency(0), None);
    assert_eq!(timebase_of_frequency(u64::MAX), None, "terms past u32");
}

/// The timebase is the tick rate's whenever `hw.tbfrequency` answers a frequency, and this process's `mach_timebase_info` only when it does not. The inputs are Rosetta 2's, as measured on this Mac (docs/measured/rosetta-mac-2026-10-20.md §1): `hw.tbfrequency` says 24,000,000 and `mach_timebase_info` 1/1, while the kernel's CPU times are in 24 MHz ticks, so a choice of mach's 1/1 reads them 3/125 of the truth (§3 (b)). Natively the two readings agree, so a live reading cannot tell the order apart; these inputs do, on any host.
#[test]
fn the_tick_rate_chooses_the_timebase_before_mach_timebase_info() {
    let rosetta = || Some((1, 1));
    assert_eq!(timebase_from(Some(24_000_000), rosetta), Some((125, 3)));
    assert_eq!(timebase_from(None, rosetta), Some((1, 1)), "no answer");
    assert_eq!(
        timebase_from(Some(0), rosetta),
        Some((1, 1)),
        "no frequency"
    );
}

/// The sysctl [`kernel_timebase`] reads answers a frequency on macOS (natively and under Rosetta 2, docs/measured/rosetta-mac-2026-10-20.md §1). A misspelt name would answer nothing, and the timebase would fall back to `mach_timebase_info` unseen: right natively, 125/3 too small under Rosetta.
#[cfg(target_os = "macos")]
#[test]
fn the_tick_rate_sysctl_answers_a_frequency() {
    let hz = crate::macos::sysctl_integer_result(TICK_FREQUENCY)
        .unwrap_or_else(|errno| panic!("{TICK_FREQUENCY:?} answers (errno {errno})"));
    assert!(
        u64::try_from(hz)
            .ok()
            .and_then(timebase_of_frequency)
            .is_some(),
        "{TICK_FREQUENCY:?} answers a frequency: {hz}"
    );
}

/// The sweep's CPU time is the kernel's, in nanoseconds, natively or under Rosetta: a stopped
/// spinning child reads within 2x of what `wait4` reports for it.
#[cfg(target_os = "macos")]
#[test]
fn the_sweep_reads_a_stopped_childs_cpu_time_as_wait4_counts_it() {
    use super::stopped_spinner::{read, within_2x};
    let swept = |pid: u32| {
        processes()
            .and_then(|s| s.procs.into_iter().find(|p| p.pid == pid))
            .and_then(|p| p.cpu_ns)
    };
    let (swept_ns, kernel) = read(50_000_000, swept, swept);
    let swept_ns = swept_ns.expect("the sweep reads its own child");
    assert!(
        within_2x(swept_ns, kernel),
        "the sweep read {swept_ns} ns; the reap's rusage for the same stopped child is \
         {kernel} ns (timebase {:?})",
        kernel_timebase()
    );
}

/// The sweep says which kind of core a busy child ran on where the host has two
/// (`hw.nperflevels` above 1: Apple silicon's performance and efficiency cores), within what
/// it counts in all: its time on performance cores is at most its CPU time, and its
/// instructions there at most all it retired, of which there are some. A host with one kind
/// (an Intel Mac) reads no split.
#[cfg(target_os = "macos")]
#[test]
fn the_sweep_splits_a_busy_childs_cpu_by_kind_of_core() {
    use super::stopped_spinner::read;
    let row = |pid: u32| processes().and_then(|s| s.procs.into_iter().find(|p| p.pid == pid));
    let (swept, _) = read(200_000_000, |pid| row(pid)?.cpu_ns, row);
    let swept = swept.expect("the sweep reads its own child");
    let cpu = swept.cpu_ns.expect("its CPU time");
    let levels = crate::macos::sysctl_int(c"hw.nperflevels").unwrap_or(0);
    match swept.cores {
        Some(c) => {
            assert!(levels > 1, "{levels} kinds of core, split: {swept:?}");
            assert!(c.p_cpu_ns <= cpu, "{swept:?}");
            assert!(
                c.instructions > 0 && c.p_instructions <= c.instructions,
                "{swept:?}"
            );
        }
        None => assert!(levels <= 1, "{levels} kinds of core, no split: {swept:?}"),
    }
}

/// A sweep asked for coalitions reads each process's, and its pid version, as the one-process readers do, another user's too (launchd's, root's); a plain sweep reads neither.
#[cfg(target_os = "macos")]
#[test]
fn a_sweep_reads_coalitions_only_when_asked() {
    let sweep = processes_with(SweepOpts {
        coalitions: true,
        ..SweepOpts::default()
    })
    .expect("a sweep");
    for pid in [std::process::id(), 1] {
        let row = sweep.procs.iter().find(|p| p.pid == pid).expect("swept");
        let own = crate::coalitions::coalition_of(pid).expect("its coalition");
        assert_eq!(row.coalition, Some(own), "{pid}");
        let version = pid_version(pid).expect("its pid version");
        assert_eq!(row.version, Some(version), "{pid}");
        // The same field as aterm-uds's flavor-17 reader reads it (no SDK header declares it).
        let uds = aterm_uds::process::uniq_identifier(i32::try_from(pid).expect("a pid"))
            .map(|i| i.p_idversion);
        assert_eq!(uds, Some(version), "{pid}");
    }
    assert!(
        processes()
            .expect("a sweep")
            .procs
            .iter()
            .all(|p| p.coalition.is_none() && p.version.is_none())
    );
}

/// A sweep asked for versions reads the pid version of each process whose start it cannot
/// read (launchd's, root's, to a user that is not root) and of no other (this process's,
/// whose start it reads), and no coalition. NEGATIVE CONTROL: a plain sweep reads no version.
#[cfg(target_os = "macos")]
#[test]
fn a_sweep_asked_for_versions_reads_them_where_it_reads_no_start() {
    let sweep = processes_with(SweepOpts {
        versions: true,
        ..SweepOpts::default()
    })
    .expect("a sweep");
    let row = |pid: u32| sweep.procs.iter().find(|p| p.pid == pid).expect("swept");
    let me = row(std::process::id());
    assert!(me.start.is_some(), "this process's start: {me:?}");
    assert_eq!(me.version, None, "read where the start is not");
    let launchd = row(1);
    let version = pid_version(1).expect("launchd's pid version");
    assert_eq!(
        launchd.version,
        launchd.start.is_none().then_some(version),
        "{launchd:?}"
    );
    assert!(sweep.procs.iter().all(|p| p.coalition.is_none()));
    assert!(
        processes()
            .expect("a sweep")
            .procs
            .iter()
            .all(|p| p.version.is_none()),
        "NEGATIVE CONTROL: a plain sweep"
    );
}

/// The exec stand-in's stage: `before` execs this test binary again as `after` on a byte.
#[cfg(target_os = "macos")]
const EXEC_STAGE: &str = "ATERM_SYSPROBE_EXEC_STAGE";

/// [`exec_stand_in`], as `--exact` names it.
#[cfg(target_os = "macos")]
const EXEC_STAND_IN: &str = "procs::tests::exec_stand_in";

/// The hang detector for what must happen (a child's line).
#[cfg(target_os = "macos")]
const HANG: std::time::Duration = std::time::Duration::from_secs(60);

#[cfg(target_os = "macos")]
#[test]
#[ignore = "the child the exec test starts; alone it returns"]
fn exec_stand_in() {
    use std::io::{Read as _, Write as _};
    let Ok(stage) = std::env::var(EXEC_STAGE) else {
        return;
    };
    if stage == "background" || stage == "foreground" {
        // Darwin's public resource.h names selector 4 PRIO_DARWIN_PROCESS and 0x1000
        // PRIO_DARWIN_BG. The stand-in changes only its own internal request.
        // SAFETY: these are constant selectors and pid zero is this process.
        let rc = unsafe { libc::setpriority(4, 0, if stage == "background" { 0x1000 } else { 0 }) };
        assert_eq!(
            rc,
            0,
            "set own requested-background state: {}",
            std::io::Error::last_os_error()
        );
    }
    println!("\n{stage}");
    std::io::stdout().flush().expect("the stage's proof");
    let mut byte = [0; 1];
    match std::io::stdin().read(&mut byte) {
        Ok(1) if stage == "before" => {
            let failed =
                aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
                    .args([
                        "--exact",
                        EXEC_STAND_IN,
                        "--ignored",
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .env(EXEC_STAGE, "after")
                    .exec();
            panic!("the exec: {failed}");
        }
        _ => aterm_uds::process::kill_self_now(),
    }
}

/// Start this test binary as the stand-in `test` (as `--exact` names it) with `var` set and
/// its stdin and stdout piped; the second half waits for one of its lines (the hang detector).
#[cfg(target_os = "macos")]
fn stand_in(
    test: &str,
    var: (&str, &std::ffi::OsStr),
) -> (aterm_uds::spawnfd::Child, impl Fn(&str)) {
    use aterm_uds::spawnfd::Stdio;
    use std::io::BufRead as _;
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
        .args([
            "--exact",
            test,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(var.0, var.1)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("the stand-in starts");
    let out = child.stdout.take().expect("its stdout");
    let (tx, lines) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let saw = move |word: &str| loop {
        match lines.recv_timeout(HANG) {
            Ok(line) if line == word => break,
            Ok(_) => {}
            Err(e) => panic!("hang detector: the stand-in never wrote {word:?} ({e})"),
        }
    };
    (child, saw)
}

/// Target bits distinguish two children even when the observer's request stays the same.
#[cfg(target_os = "macos")]
#[test]
fn scheduling_reads_each_childs_requested_background_bits() {
    for (stage, expected) in [("foreground", false), ("background", true)] {
        let (mut child, saw) = stand_in(EXEC_STAND_IN, (EXEC_STAGE, stage.as_ref()));
        saw(stage);
        let actual = processes().and_then(|s| {
            s.procs
                .into_iter()
                .find(|p| p.pid == child.id())
                .and_then(|p| p.scheduling)
        });
        // Reap before assertions so a failed observation leaves no stand-in behind.
        drop(child.stdin.take());
        aterm_uds::deadline::kill_within(&mut child, HANG);
        let actual = actual.expect("the stand-in's BSD info was read");
        assert_eq!(actual.background_internal, expected, "{stage}: {actual:?}");
    }
}

/// AN EXEC MOVES THE PID VERSION, SO A KEPT COALITION IS READ AGAIN (macOS, live): a child
/// that execs keeps its pid, its kernel start and its coalition (its new task inherits the
/// old one's) while its pid version moves, and the version is what [`processes_keeping`]
/// keeps a coalition under. NEGATIVE CONTROL: across two sweeps with no exec between, the
/// version stays and the coalition is kept.
#[cfg(target_os = "macos")]
#[test]
fn an_exec_moves_the_pid_version_and_keeps_the_coalition() {
    use std::io::Write as _;
    let (mut child, saw) = stand_in(EXEC_STAND_IN, (EXEC_STAGE, "before".as_ref()));
    saw("before");
    let pid = child.id();
    let mut ids = crate::coalitions::CoalitionIds::default();
    let mut row = || {
        processes_keeping(
            SweepOpts {
                coalitions: true,
                ..SweepOpts::default()
            },
            &mut ids,
        )
        .expect("a sweep")
        .procs
        .into_iter()
        .find(|p| p.pid == pid)
        .expect("the stand-in swept")
    };
    let first = row();
    assert!(first.version.is_some(), "{first:?}");
    assert_eq!(first.coalition, crate::coalitions::coalition_of(pid));
    let again = row();
    assert_eq!(
        (again.version, again.coalition),
        (first.version, first.coalition),
        "no exec between"
    );
    let mut stdin = child.stdin.take().expect("its stdin");
    stdin.write_all(b"x").expect("the byte that execs it");
    saw("after");
    let after = row();
    assert_eq!(
        (after.pid, after.start),
        (first.pid, first.start),
        "the same process"
    );
    assert_ne!(after.version, first.version, "an exec moved the version");
    assert_eq!(
        after.coalition, first.coalition,
        "the new task's, inherited"
    );
    drop(stdin);
    aterm_uds::deadline::kill_within(&mut child, HANG);
}

/// The write stand-in's scratch directory: [`write_stand_in`] writes [`WRITE_BYTES`] there on a byte.
#[cfg(target_os = "macos")]
const WRITE_DIR: &str = "ATERM_SYSPROBE_WRITE_DIR";

/// [`write_stand_in`], as `--exact` names it.
#[cfg(target_os = "macos")]
const WRITE_STAND_IN: &str = "procs::tests::write_stand_in";

/// What [`write_stand_in`] writes and syncs: 8 MiB.
#[cfg(target_os = "macos")]
const WRITE_BYTES: u64 = 8 << 20;

#[cfg(target_os = "macos")]
#[test]
#[ignore = "the child the zombie-write test starts; alone it returns"]
fn write_stand_in() {
    use std::io::{Read as _, Write as _};
    let Some(dir) = std::env::var_os(WRITE_DIR) else {
        return;
    };
    println!("\nready");
    std::io::stdout().flush().expect("the ready proof");
    let mut byte = [0; 1];
    if !matches!(std::io::stdin().read(&mut byte), Ok(1)) {
        aterm_uds::process::kill_self_now();
    }
    let mut file = std::fs::File::create(std::path::Path::new(&dir).join("written"))
        .expect("the scratch file");
    let block: Vec<u8> = (0..1usize << 20).map(|i| (i * 31 % 251) as u8).collect();
    for _ in 0..WRITE_BYTES >> 20 {
        file.write_all(&block).expect("a block");
    }
    aterm_durable::durable(&file).expect("to storage");
    println!("wrote");
    std::io::stdout().flush().expect("the wrote proof");
}

/// A ZOMBIE KEEPS WHAT IT WROTE (macOS, live): a child that writes 8 MiB to storage and exits
/// unreaped reads, as a zombie, at least those bytes under the start it had alive, where the
/// sweep once gave a zombie no counters, so the writes after its last live sweep were lost.
/// NEGATIVE CONTROL: its row read before it wrote reads under 8 MiB.
#[cfg(target_os = "macos")]
#[test]
fn a_zombie_keeps_the_bytes_it_wrote_and_its_start() {
    use std::io::Write as _;
    let dir = aterm_tempfile::tempdir().expect("a scratch directory");
    let (mut child, saw) = stand_in(WRITE_STAND_IN, (WRITE_DIR, dir.path().as_os_str()));
    saw("ready");
    let pid = child.id();
    let row = || {
        processes()
            .expect("a sweep")
            .procs
            .into_iter()
            .find(|p| p.pid == pid)
    };
    let alive = row().expect("the stand-in swept");
    assert!(
        alive.disk_written.is_some_and(|w| w < WRITE_BYTES) && alive.start.is_some(),
        "{alive:?}"
    );
    let mut stdin = child.stdin.take().expect("its stdin");
    stdin
        .write_all(b"x")
        .expect("the byte that starts the write");
    saw("wrote");
    let mut exit = aterm_uds::deadline::ExitWait::new(&child);
    let by = std::time::Instant::now() + HANG;
    while !child.exited().expect("whether it exited") {
        let left = by.saturating_duration_since(std::time::Instant::now());
        assert!(!left.is_zero(), "hang detector: the stand-in never exited");
        exit.wait(left);
    }
    let zombie = row().expect("the zombie swept");
    assert_eq!(zombie.state, crate::jobs::RunState::Zombie, "{zombie:?}");
    assert_eq!(zombie.start, alive.start, "the same process");
    assert!(
        zombie.disk_written.is_some_and(|w| w >= WRITE_BYTES),
        "{zombie:?}"
    );
    child.wait().expect("reaped");
}

/// The spin stand-in's switch: [`spin_stand_in`] spins until it has run [`SPIN_NS`] of CPU;
/// at `nested` it first runs and reaps one more spinner of its own, then says `ready` and
/// spins only on a byte; at `nocld` and `ignore` it is [`nocld_stand_in`] and
/// [`ignore_stand_in`].
#[cfg(target_os = "macos")]
const SPIN_VAR: &str = "ATERM_SYSPROBE_SPIN";

/// [`spin_stand_in`], as `--exact` names it.
#[cfg(target_os = "macos")]
const SPIN_STAND_IN: &str = "procs::tests::spin_stand_in";

/// The CPU [`spin_stand_in`] runs before it exits, by its own account: 200 ms.
#[cfg(target_os = "macos")]
const SPIN_NS: u64 = 200_000_000;

/// This process's own and reaped children's CPU, ns, by the kernel's account.
#[cfg(target_os = "macos")]
fn own_and_reaped_ns(pid: u32) -> (u64, u64) {
    let (numer, denom) = imp::timebase();
    let u = imp::rusage(i32::try_from(pid).expect("a pid"))
        .expect("its usage")
        .ri;
    (
        ticks_to_ns(
            u.ri_user_time.saturating_add(u.ri_system_time),
            numer,
            denom,
        ),
        ticks_to_ns(
            u.ri_child_user_time.saturating_add(u.ri_child_system_time),
            numer,
            denom,
        ),
    )
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "the child the zombie-CPU test starts; alone it returns"]
fn spin_stand_in() {
    use std::io::{Read as _, Write as _};
    let Some(how) = std::env::var_os(SPIN_VAR) else {
        return;
    };
    if how == "nocld" {
        nocld_stand_in();
    }
    if how == "ignore" {
        ignore_stand_in();
    }
    if how == "nested" {
        let status = aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
            .args([
                "--exact",
                SPIN_STAND_IN,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(SPIN_VAR, "1")
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .status()
            .expect("the nested spinner runs");
        assert!(status.success(), "the nested spinner: {status:?}");
        println!("\nready");
        std::io::stdout().flush().expect("the ready proof");
        let mut byte = [0; 1];
        if !matches!(std::io::stdin().read(&mut byte), Ok(1)) {
            aterm_uds::process::kill_self_now();
        }
    }
    let me = std::process::id();
    let mut x = 0u64;
    while own_and_reaped_ns(me).0 < SPIN_NS {
        for i in 0..100_000u64 {
            x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(i));
        }
    }
    std::hint::black_box(x);
    println!("\nspun");
    std::io::stdout().flush().expect("the spun proof");
    std::process::exit(0);
}

/// [`spin_stand_in`] at `nocld`: ignores `SIGCHLD`, runs one spinner the kernel reaps for it,
/// and once that spinner's pid answers no signal (reaped) says `ready` and waits for a byte.
#[cfg(target_os = "macos")]
fn nocld_stand_in() -> ! {
    use std::io::{Read as _, Write as _};
    // SAFETY: sets one signal's disposition; this process waits on no child but by its pid.
    let before = unsafe { libc::signal(libc::SIGCHLD, libc::SIG_IGN) };
    assert_ne!(before, libc::SIG_ERR, "SIGCHLD ignored");
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
        .args([
            "--exact",
            SPIN_STAND_IN,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(SPIN_VAR, "1")
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("the spinner starts");
    let mut out = String::new();
    child
        .stdout
        .take()
        .expect("its stdout")
        .read_to_string(&mut out)
        .expect("its output");
    assert!(out.lines().any(|l| l == "spun"), "the spinner ran: {out:?}");
    let pid = child.id();
    let alive = || {
        // SAFETY: signal 0 only asks whether the pid names a process.
        unsafe { libc::kill(i32::try_from(pid).expect("a pid"), 0) == 0 }
    };
    let mut exit = aterm_uds::deadline::ExitWait::on_pid(pid);
    let by = std::time::Instant::now() + HANG;
    while alive() {
        let left = by.saturating_duration_since(std::time::Instant::now());
        assert!(
            !left.is_zero(),
            "hang detector: the spinner was never reaped"
        );
        exit.wait(left);
    }
    drop(child);
    println!("\nready");
    std::io::stdout().flush().expect("the ready proof");
    let mut byte = [0; 1];
    let _ = std::io::stdin().read(&mut byte);
    std::process::exit(0);
}

/// [`spin_stand_in`] at `ignore`: ignores `SIGCHLD` and, having started and reaped nothing,
/// says `ready` and waits for a byte.
#[cfg(target_os = "macos")]
fn ignore_stand_in() -> ! {
    use std::io::{Read as _, Write as _};
    // SAFETY: sets one signal's disposition; this process starts no child.
    let before = unsafe { libc::signal(libc::SIGCHLD, libc::SIG_IGN) };
    assert_ne!(before, libc::SIG_ERR, "SIGCHLD ignored");
    println!("\nready");
    std::io::stdout().flush().expect("the ready proof");
    let mut byte = [0; 1];
    let _ = std::io::stdin().read(&mut byte);
    std::process::exit(0);
}

/// A PARENT THAT IGNORES `SIGCHLD` READS SO BEFORE ITS FIRST REAP (macOS, live): the kernel
/// may reap its first children for it, doubling their time, and it may go before any reading
/// shows that time, so the sweep reads the flag of a process that has reaped nothing too: a
/// stand-in that ignores `SIGCHLD` and has reaped nothing reads `Some(true)`. NEGATIVE
/// CONTROL: the zombie test's stand-in, which waits for its children, reads `Some(false)`.
#[cfg(target_os = "macos")]
#[test]
fn a_parent_ignoring_sigchld_reads_so_before_its_first_reap() {
    use std::io::Write as _;
    let (mut child, saw) = stand_in(SPIN_STAND_IN, (SPIN_VAR, "ignore".as_ref()));
    let pid = child.id();
    saw("ready");
    let row = processes()
        .expect("a sweep")
        .procs
        .into_iter()
        .find(|p| p.pid == pid)
        .expect("the stand-in swept");
    child
        .stdin
        .take()
        .expect("its stdin")
        .write_all(b"x")
        .expect("the byte that ends it");
    child.wait().expect("reaped");
    assert_eq!(row.child_cpu_ns, Some(0), "it reaped nothing: {row:?}");
    assert_eq!(row.reaps_unwaited, Some(true), "its row: {row:?}");
}

/// A PARENT THAT IGNORES `SIGCHLD` HAS ITS CHILD'S TIME ADDED TO ITS REAPED TIME TWICE, AND
/// THE SWEEP SAYS SO (macOS, live): a stand-in that ignores `SIGCHLD` runs a spinner of 200 ms
/// by its own account, which the kernel reaps for it; its `kinfo_proc` and its sweep row read
/// `reaps_unwaited`, and its reaped-children time is at least twice the spinner's 200 ms.
/// NEGATIVE CONTROL: the zombie test's stand-in, which waits for its spinner, reads
/// `Some(false)`.
#[cfg(target_os = "macos")]
#[test]
fn a_parent_ignoring_sigchld_reads_its_reaped_time_doubled() {
    use std::io::Write as _;
    let (mut child, saw) = stand_in(SPIN_STAND_IN, (SPIN_VAR, "nocld".as_ref()));
    let pid = child.id();
    saw("ready");
    let kinfo = crate::jobs::macos_kinfo(std::ffi::c_int::try_from(pid).expect("a pid"));
    let row = processes()
        .expect("a sweep")
        .procs
        .into_iter()
        .find(|p| p.pid == pid)
        .expect("the stand-in swept");
    let (_, reaped) = own_and_reaped_ns(pid);
    child
        .stdin
        .take()
        .expect("its stdin")
        .write_all(b"x")
        .expect("the byte that ends it");
    child.wait().expect("reaped");
    eprintln!("ignoring SIGCHLD: reaped {reaped} ns for a {SPIN_NS} ns spinner, {row:?}");
    assert!(kinfo.is_some_and(|k| k.reaps_unwaited), "its kinfo_proc");
    assert_eq!(row.reaps_unwaited, Some(true), "its row");
    assert!(reaped >= 2 * SPIN_NS, "doubled: {reaped}");
    assert!(
        row.child_cpu_ns.is_some_and(|ns| ns >= 2 * SPIN_NS),
        "{row:?}"
    );
}

/// AN UNREAPED ZOMBIE ANSWERS ITS FINAL CPU, AND ITS PARENT'S REAPED-CHILDREN TIME TAKES IT AT
/// THE REAP (macOS, live): a child that ran and reaped a 200 ms spinner of its own, then ran
/// 200 ms of CPU by its own account and exited, reads, unreaped, at least that CPU and that
/// reaped time under the start it had alive, from both `proc_pid_rusage` and the sweep's
/// zombie row; once reaped, this process's reaped-children time has grown by at least both
/// (the kernel adds a child's own time and its reaped children's at its reap). NEGATIVE
/// CONTROL: the row read before the spin, which waits for a byte, reads under 200 ms.
#[cfg(target_os = "macos")]
#[test]
fn a_zombie_answers_its_final_cpu_and_the_reap_moves_it_to_its_parent() {
    use std::io::Write as _;
    let (mut child, saw) = stand_in(SPIN_STAND_IN, (SPIN_VAR, "nested".as_ref()));
    let pid = child.id();
    let row = || {
        processes()
            .expect("a sweep")
            .procs
            .into_iter()
            .find(|p| p.pid == pid)
    };
    saw("ready");
    let alive = row().expect("the stand-in swept");
    let mut stdin = child.stdin.take().expect("its stdin");
    stdin
        .write_all(b"x")
        .expect("the byte that starts the spin");
    saw("spun");
    let mut exit = aterm_uds::deadline::ExitWait::new(&child);
    let by = std::time::Instant::now() + HANG;
    while !child.exited().expect("whether it exited") {
        let left = by.saturating_duration_since(std::time::Instant::now());
        assert!(!left.is_zero(), "hang detector: the stand-in never exited");
        exit.wait(left);
    }
    let (zombie_own, zombie_reaped) = own_and_reaped_ns(pid);
    let zombie = row().expect("the zombie swept");
    let me = std::process::id();
    let (_, reaped_before) = own_and_reaped_ns(me);
    child.wait().expect("reaped");
    let (_, reaped_after) = own_and_reaped_ns(me);
    eprintln!(
        "zombie: own {zombie_own} ns, reaped {zombie_reaped} ns; swept {:?} / {:?}; this \
         process's reaped-children time {reaped_before} -> {reaped_after} ns (+{})",
        zombie.cpu_ns,
        zombie.child_cpu_ns,
        reaped_after - reaped_before
    );
    assert!(
        alive.cpu_ns.is_some_and(|ns| ns < SPIN_NS),
        "NEGATIVE CONTROL: before the spin {alive:?}"
    );
    assert_eq!(
        alive.reaps_unwaited,
        Some(false),
        "it waits for its children: {alive:?}"
    );
    assert_eq!(zombie.state, crate::jobs::RunState::Zombie, "{zombie:?}");
    assert_eq!(zombie.start, alive.start, "the same process");
    assert!(zombie_own >= SPIN_NS, "the zombie's own CPU: {zombie_own}");
    assert!(
        zombie_reaped >= SPIN_NS,
        "the zombie's reaped child's CPU: {zombie_reaped}"
    );
    assert_eq!(
        (zombie.cpu_ns, zombie.child_cpu_ns),
        (Some(zombie_own), Some(zombie_reaped)),
        "the sweep's zombie row"
    );
    assert!(
        reaped_after - reaped_before >= zombie_own + zombie_reaped,
        "the reap moved the zombie's final CPU to its parent"
    );
}

/// THE SWEEP READS A PARENT BEFORE ITS CHILD (macOS, live), so a child reaped while it runs
/// is never counted both in its own row and in its parent's reaped-children time: this
/// process's counters are read before its child's, while the rows keep the kernel's order,
/// the child (the newer) first. NEGATIVE CONTROL: reading the list in the kernel's order
/// reads the child first.
#[cfg(target_os = "macos")]
#[test]
fn the_sweep_reads_a_parent_before_its_child_and_keeps_the_kernels_order() {
    let (mut child, saw) = stand_in(EXEC_STAND_IN, (EXEC_STAGE, "parent-first".as_ref()));
    saw("parent-first");
    let sweep = processes().expect("a sweep");
    let at = |pid: u32| {
        sweep
            .procs
            .iter()
            .position(|p| p.pid == pid)
            .map(|k| (k, sweep.procs[k].read_at.expect("its counters' read")))
            .expect("swept")
    };
    let (me, read_me) = at(std::process::id());
    let (kid, read_kid) = at(child.id());
    assert!(read_me < read_kid, "the parent's counters read first");
    assert!(kid < me, "the rows in the kernel's order, newest first");
    aterm_uds::deadline::kill_within(&mut child, HANG);
}

/// THE SWEEP READS A ZOMBIE AFTER ITS PARENT (macOS, live): the kernel lists its zombies after
/// every live process, so read from the list's end a zombie read before its parent, and a
/// parent that reaped it between the two reads held it twice, in the zombie's final row and
/// in its own reaped-children time. The sweep reads the zombies in a second pass, after
/// every live process: this process's counters before its unreaped child's, whose row
/// follows the live ones. NEGATIVE CONTROL: the zombie read in the live pass reads first.
#[cfg(target_os = "macos")]
#[test]
fn the_sweep_reads_a_zombie_after_its_parent() {
    let (mut child, saw) = stand_in(EXEC_STAND_IN, (EXEC_STAGE, "parent-first".as_ref()));
    saw("parent-first");
    drop(child.stdin.take());
    let mut exit = aterm_uds::deadline::ExitWait::new(&child);
    let by = std::time::Instant::now() + HANG;
    while !child.exited().expect("whether it exited") {
        let left = by.saturating_duration_since(std::time::Instant::now());
        assert!(!left.is_zero(), "hang detector: the stand-in never exited");
        exit.wait(left);
    }
    let sweep = processes().expect("a sweep");
    let at = |pid: u32| {
        sweep
            .procs
            .iter()
            .position(|p| p.pid == pid)
            .map(|k| {
                let p = &sweep.procs[k];
                (k, p.read_at.expect("its counters' read"), p.state)
            })
            .expect("swept")
    };
    let (me, read_me, _) = at(std::process::id());
    let (zombie, read_zombie, state) = at(child.id());
    assert_eq!(state, crate::jobs::RunState::Zombie, "unreaped");
    assert!(read_me < read_zombie, "the parent's counters read first");
    assert!(me < zombie, "the zombie's row after the live ones");
    child.wait().expect("reaped");
}

#[cfg(any(
    target_os = "macos",
    target_os = "linux",
    all(windows, target_pointer_width = "64")
))]
#[test]
fn the_sweep_finds_this_process_and_its_parent() {
    let sweep = processes().expect("the platform has a sweep");
    let me = std::process::id();
    let mine = sweep
        .procs
        .iter()
        .find(|p| p.pid == me)
        .expect("this process is in the sweep");
    assert!(mine.ppid > 0, "{mine:?}");
    assert!(mine.cpu_ns.is_some_and(|ns| ns > 0), "{mine:?}");
    assert!(mine.footprint.is_some_and(|b| b > 0), "{mine:?}");
    if cfg!(target_os = "macos") {
        assert!(
            mine.disk_read.is_some() && mine.disk_written.is_some(),
            "{mine:?}"
        );
    }
    assert!(!mine.name.is_empty());
    if cfg!(unix) {
        assert!(mine.uid.is_some(), "{mine:?}");
        // A test binary's name, `aterm_sysprobe-<16 hex>`, runs past every short name's cut
        // (and to `p_name`'s 31 bytes on macOS), so only its image says it whole.
        let exe = std::env::current_exe().expect("this test binary's path");
        let file = exe.file_name().and_then(|n| n.to_str()).expect("its name");
        assert!(file.len() > 16, "{file}");
        assert_eq!(mine.name, file);
    }
    assert!(mine.read_at.is_some(), "{mine:?}");
    if cfg!(target_os = "macos") {
        assert!(mine.runnable_ns.is_some_and(|r| r > 0), "{mine:?}");
        assert!(mine.peak_footprint.is_some_and(|p| p > 0), "{mine:?}");
        assert!(mine.disk_written.is_some(), "{mine:?}");
    }
    assert!(sweep.procs.iter().any(|p| p.pid == mine.ppid));
}

#[test]
fn filetime_birth_and_age_measure_against_the_unix_epoch() {
    let epoch = super::UNIX_EPOCH_TICKS;
    assert_eq!(super::birth_of_filetime(epoch - 1), None);
    assert_eq!(super::filetime_age_ms(epoch - 1, 5_000_000), None);
    assert_eq!(super::birth_of_filetime(epoch + 25_000), Some(2));
    assert_eq!(super::filetime_age_ms(epoch + 25_000, 10_500), Some(8));
    assert_eq!(
        super::filetime_age_ms(epoch + 50_000_000, 1_000),
        Some(0),
        "a creation stamp ahead of the wall-clock read saturates to 0 ms"
    );
}
