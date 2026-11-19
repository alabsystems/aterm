// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A plain sweep that reads each process's scheduling too.
#[cfg(target_os = "macos")]
const SCHEDULING: SweepOpts = SweepOpts {
    coalitions: false,
    versions: false,
    scheduling: true,
};

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
    let mine = processes_with(SCHEDULING)
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

/// The sweep's CPU time is the kernel's, in nanoseconds, natively or under Rosetta (and on
/// Linux, where `stat`'s clock ticks are scaled by `_SC_CLK_TCK`): a stopped spinning child
/// reads within 2x of what `wait4` reports for it.
#[cfg(any(target_os = "macos", target_os = "linux"))]
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
         {kernel} ns ({})",
        super::stopped_spinner::scale()
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

/// A sweep asked for coalitions reads each process's, and gives its pid version, as the one-process readers do, another user's too (launchd's, root's); a plain sweep gives neither (it reads no coalition, and the versions only to bracket each row's reads).
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

/// A sweep asked for versions gives the pid version of each process whose start it cannot
/// read (launchd's, root's, to a user that is not root) and of no other (this process's,
/// whose start it reads), and no coalition; it reads every process's, to bracket the row's other
/// reads. NEGATIVE CONTROL: a plain sweep gives no version.
#[cfg(target_os = "macos")]
#[test]
fn a_sweep_asked_for_versions_gives_them_where_it_reads_no_start() {
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

/// Deschedule only the owned stand-in immediately after its background request.
#[cfg(target_os = "macos")]
const EXEC_STOP_AFTER_BG: &str = "ATERM_SYSPROBE_STOP_AFTER_BG";

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
    if stage == "foreground" {
        request_background(0, false).expect("drop its own background");
    }
    println!("\n{stage}");
    std::io::stdout().flush().expect("the stage's proof");
    // Publish readiness before requesting background: a backgrounded process's CPU and
    // page-ins can wait behind foreground work. The parent observes policy independently.
    if stage == "background" {
        request_background(0, true).expect("request its own background");
        if std::env::var_os(EXEC_STOP_AFTER_BG).is_some() {
            // SAFETY: SIGSTOP stops this child until its parent kills and reaps it.
            assert_eq!(unsafe { libc::kill(libc::getpid(), libc::SIGSTOP) }, 0);
        }
    }
    let mut byte = [0; 1];
    match std::io::stdin().read(&mut byte) {
        Ok(1) if stage == "before" => {
            let mut command =
                aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"));
            command
                .args([
                    "--exact",
                    EXEC_STAND_IN,
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(EXEC_STAGE, "after");
            let _closed = crate::exec_gate::close(HANG)
                .expect("the stand-in refuses exec while its process-table reads remain active");
            let failed = command.exec();
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
    let pid = child.id();
    let saw = move |word: &str| {
        let saw = aterm_spec::wait::until_while_working(
            pid,
            &format!("the stand-in wrote {word:?}"),
            || loop {
                match lines.try_recv() {
                    Ok(line) if line == word => return Some(true),
                    Ok(_) => {}
                    Err(std::sync::mpsc::TryRecvError::Empty) => return None,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => return Some(false),
                }
            },
        );
        assert!(saw, "the stand-in closed its proof pipe before {word:?}");
    };
    (child, saw)
}

/// Request (`on`) or drop Darwin background for `pid` (`0`: this process) through
/// `setpriority`, as Darwin's public resource.h names selector 4 `PRIO_DARWIN_PROCESS` and
/// `0x1000` `PRIO_DARWIN_BG`. A process's request for itself is its internal bit; another
/// process's request for it, its external one (XNU's `do_background_proc`).
#[cfg(target_os = "macos")]
fn request_background(pid: libc::id_t, on: bool) -> std::io::Result<()> {
    // SAFETY: constant selectors; setpriority only writes the named process's policy.
    let rc = unsafe { libc::setpriority(4, pid, if on { 0x1000 } else { 0 }) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// TARGET BITS DISTINGUISH CHILDREN (macOS, live) even when the observer's request stays the
/// same: a child's request for itself reads as its internal bit, and this process's request
/// for a child as that child's external bit. Nothing waits on a child once it is in the
/// background, where a loaded Mac can keep it off every core and its page-ins behind every
/// other I/O: the background child writes its line before its request, and the test waits
/// for the request to read with its hang detector. NEGATIVE CONTROL: the foreground child,
/// which drops any background it inherited, reads neither bit until this process asks.
#[cfg(target_os = "macos")]
#[test]
fn scheduling_reads_each_childs_requested_background_bits() {
    let bits = |pid: u32| {
        processes_with(SCHEDULING).and_then(|s| {
            s.procs
                .into_iter()
                .find(|p| p.pid == pid)
                .and_then(|p| p.scheduling)
                .map(|s| (s.background_internal, s.background_external))
        })
    };
    let (mut foreground, saw) = stand_in(EXEC_STAND_IN, (EXEC_STAGE, "foreground".as_ref()));
    saw("foreground");
    let (mut background, saw) = stand_in(EXEC_STAND_IN, (EXEC_STAGE, "background".as_ref()));
    saw("background");
    let own = aterm_spec::wait::within_while_working(
        background.id(),
        HANG,
        aterm_spec::wait::WORKING_GUARD,
        std::time::Duration::from_millis(20),
        || bits(background.id()).filter(|&(internal, _)| internal),
    );
    let neither = bits(foreground.id());
    let request = request_background(foreground.id(), true);
    let asked = bits(foreground.id());
    // Reap before assertions so a failed observation leaves no stand-in behind.
    for child in [&mut foreground, &mut background] {
        drop(child.stdin.take());
        aterm_uds::deadline::kill_within(child, HANG);
    }
    assert_eq!(
        own,
        Ok((true, false)),
        "hang detector: the background child's own request never read as its internal bit"
    );
    assert_eq!(neither, Some((false, false)), "the foreground child");
    request.expect("this process's request for the foreground child");
    assert_eq!(
        asked,
        Some((false, true)),
        "this process's request for the foreground child"
    );
}

/// A child denied any execution after requesting background has already flushed its proof.
/// The kernel stop fixes the scheduling boundary without loading the shared Mac: moving the
/// proof below the background request leaves no proof in the pipe at this same boundary.
#[cfg(target_os = "macos")]
#[test]
fn background_proof_precedes_a_descheduled_child() {
    use aterm_uds::spawnfd::Stdio;
    use std::io::Read as _;
    use std::os::fd::AsFd as _;

    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
        .args([
            "--exact",
            EXEC_STAND_IN,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(EXEC_STAGE, "background")
        .env(EXEC_STOP_AFTER_BG, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("the owned stand-in starts");
    let pid = child.id();
    let stopped = aterm_spec::wait::within_while_working(
        pid,
        HANG,
        aterm_spec::wait::WORKING_GUARD,
        std::time::Duration::from_millis(10),
        || {
            let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
            // SAFETY: pid names this test's child and info is a valid out-parameter.
            // WNOWAIT leaves an exited child unreaped too, keeping its pid ours until
            // the kill and reap below even when the requested stop fails.
            let rc = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid,
                    info.as_mut_ptr(),
                    libc::WSTOPPED | libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if rc != 0 {
                let error = std::io::Error::last_os_error();
                return (error.kind() != std::io::ErrorKind::Interrupted)
                    .then_some(Err(error.raw_os_error()));
            }
            // SAFETY: siginfo_t is plain C data, valid zeroed and as the kernel wrote it.
            let info = unsafe { info.assume_init() };
            (info.si_signo == libc::SIGCHLD).then(|| {
                // SAFETY: waitid reported this child's initialized SIGCHLD information.
                Ok((info.si_code, unsafe { info.si_status() }))
            })
        },
    );
    let scheduling = processes_with(SCHEDULING).and_then(|s| {
        s.procs
            .into_iter()
            .find(|p| p.pid == child.id())
            .and_then(|p| p.scheduling)
    });
    let mut bytes = Vec::new();
    let read = {
        let out = std::fs::File::from(std::os::fd::OwnedFd::from(
            child.stdout.take().expect("its proof pipe"),
        ));
        aterm_uds::ready::with_nonblocking(out.as_fd(), || {
            (&out).take(4096).read_to_end(&mut bytes)
        })
    };
    // Reap even on a failed stop, sweep or read before asserting any observation.
    aterm_uds::deadline::kill_within(&mut child, HANG);
    assert!(
        stopped.is_ok_and(|event| event
            .is_ok_and(|(code, status)| code == libc::CLD_STOPPED && status == libc::SIGSTOP)),
        "the child's kernel stop: {stopped:?}"
    );
    assert!(
        scheduling.is_some_and(|s| s.background_internal),
        "the child's actual background request: {scheduling:?}"
    );
    assert!(
        read.is_ok()
            || read
                .as_ref()
                .is_err_and(|e| e.kind() == std::io::ErrorKind::WouldBlock),
        "the nonblocking proof read: {read:?}"
    );
    assert!(
        String::from_utf8_lossy(&bytes)
            .lines()
            .any(|line| line == "background"),
        "proof must precede descheduling: {:?}",
        String::from_utf8_lossy(&bytes)
    );
}

/// AN EXEC MOVES THE PID VERSION, SO A KEPT COALITION IS READ AGAIN (macOS, live): a child
/// that execs keeps its pid, its kernel start and its coalition (its new task inherits the
/// old one's) while its pid version moves, and the version is what [`processes_keeping`]
/// keeps a coalition under, and a name at the cut. The child's name, this test binary's
/// `aterm_sysprobe-<16 hex>`, runs to the short name's cut and to `p_name`'s, so only its image
/// says it whole: on the real kernel the keeping sweeps name it as a fresh sweep does, before the
/// exec and after. That half only confirms the live readers: the exec runs this same binary
/// again, so the name does not move with the version, and a name kept by pid alone would pass
/// it too; [`a_keeping_sweep_reads_each_generation_once`] decides the keep.
/// NEGATIVE CONTROL: across two sweeps with no exec between, the version stays and the
/// coalition is kept.
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
    let fresh_name = || {
        processes_with(SweepOpts {
            coalitions: true,
            ..SweepOpts::default()
        })
        .expect("a sweep")
        .procs
        .into_iter()
        .find(|p| p.pid == pid)
        .expect("the stand-in swept")
        .name
    };
    let exe = std::env::current_exe().expect("this test binary's path");
    let file = exe.file_name().and_then(|n| n.to_str()).expect("its name");
    assert!(file.len() >= 31, "at both cuts: {file}");
    assert_eq!(
        [
            first.name.as_str(),
            again.name.as_str(),
            fresh_name().as_str()
        ],
        [file; 3],
        "kept, then fresh"
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
    assert_eq!(
        [after.name.as_str(), fresh_name().as_str()],
        [file; 2],
        "after the exec"
    );
    drop(stdin);
    aterm_uds::deadline::kill_within(&mut child, HANG);
}

/// A scripted process table the macOS sweep reads through [`imp::Reads`]: each process's
/// answers, steps it takes (an exec, a new process at its pid, an exit, a new effective uid, a
/// new group or session) between sweeps or, scripted, as the sweep makes any one of its reads,
/// and a count of every read the sweep makes. Each answer names the generation (the pid and its
/// pid version) that gave it: a usage's footprint ([`world::footprint_of`]), a coalition's id
/// ([`world::coalition_of`]), a short name's and the longer names' program.
#[cfg(target_os = "macos")]
mod world {
    use std::cell::RefCell;
    use std::collections::{BTreeMap, VecDeque};
    use std::ffi::c_int;

    use crate::jobs::darwin::Kinfo;
    use crate::procs::imp::{Reads, ShortInfo};
    use crate::rusage::{RusageV6, Usage};

    /// This world's user, the sweep's own.
    pub(super) const OWN: u32 = 501;
    /// Another user (root), whose usage and `p_name` the kernel refuses.
    pub(super) const OTHER: u32 = 0;

    /// A step one process takes, between two sweeps or as the sweep makes a read
    /// ([`World::race`]). Each holds to xnu's rules: a step the kernel refuses panics.
    #[derive(Clone, Copy, Debug)]
    pub(super) enum Step {
        /// It execs this program: the same process, a new pid version.
        Exec(&'static str),
        /// It exits and is reaped, and a new process running this program as this user starts
        /// at its pid, a session's leader: a new start and a new pid version.
        Reuse(&'static str, u32),
        /// It exits, unreaped.
        Die,
        /// It is gone.
        Reap,
        /// Its effective uid becomes this one (`setuid`); its pid version stays.
        SetUid(u32),
        /// `setpgid(pid, pgid)`: refused to a session's leader, and into a group of another
        /// session or none (xnu's `setpgid`).
        SetPgid(c_int),
        /// `setsid()`: refused to a group's leader and where a group of its pid's id is left
        /// (xnu's `setsid_internal`); else its session and group become its pid.
        SetSid,
    }

    /// One process: the file it last exec'd, which each name reader cuts at its own length.
    #[derive(Clone, Debug)]
    struct Proc {
        version: i32,
        program: String,
        uid: u32,
        start: u64,
        zombie: bool,
        pgid: c_int,
        sid: c_int,
    }

    /// One generation as the world made it: a pid, a pid version, the program it ran and the
    /// process's start.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(super) struct Made {
        pub(super) pid: c_int,
        pub(super) version: i32,
        pub(super) program: String,
        pub(super) start: u64,
    }

    #[derive(Clone, Debug, Default)]
    struct Inner {
        procs: BTreeMap<c_int, Proc>,
        versions: i32,
        /// The processes each pid has had.
        spawned: BTreeMap<c_int, u64>,
        /// Every generation made.
        made: Vec<Made>,
        /// Each step scripted to land as the sweep makes a read: that read's kind and pid, how
        /// many such reads pass first, and the step, taken before that read answers.
        races: Vec<(&'static str, c_int, u32, Step)>,
        /// The answers a pid's rusage reads give, in turn, in place of its own.
        rusage: BTreeMap<c_int, VecDeque<Result<u64, i32>>>,
        /// The answers a pid's version reads give, in turn, in place of its own.
        version: BTreeMap<c_int, VecDeque<Option<i32>>>,
        reads: BTreeMap<(&'static str, c_int), u32>,
    }

    /// The world. A clone is the same world at the same instant, scripts included.
    #[derive(Clone, Debug, Default)]
    pub(super) struct World(RefCell<Inner>);

    /// The footprint a usage read of pid version `version` answers: each generation's own.
    pub(super) fn footprint_of(version: i32) -> u64 {
        u64::from(version.unsigned_abs()) << 20
    }

    /// The coalition id a coalition read of pid version `version` answers: each generation's own.
    pub(super) fn coalition_of(version: i32) -> u64 {
        7_000 + u64::from(version.unsigned_abs())
    }

    impl World {
        fn next_version(inner: &mut Inner) -> i32 {
            inner.versions += 1;
            inner.versions
        }

        /// A new process at `pid` running `program` as `uid`, its own session's and group's
        /// leader.
        pub(super) fn spawn(&self, pid: c_int, program: &str, uid: u32) {
            self.spawn_in(pid, program, uid, pid, pid);
        }

        /// A new process at `pid` running `program` as `uid` in group `pgid` of session `sid`,
        /// as a fork of a process there makes it.
        pub(super) fn spawn_in(
            &self,
            pid: c_int,
            program: &str,
            uid: u32,
            pgid: c_int,
            sid: c_int,
        ) {
            let inner = &mut *self.0.borrow_mut();
            let version = Self::next_version(inner);
            let nth = inner.spawned.entry(pid).or_default();
            let start = 1_000 + u64::from(pid.unsigned_abs()) + 100_000 * *nth;
            *nth += 1;
            inner.procs.insert(
                pid,
                Proc {
                    version,
                    program: program.into(),
                    uid,
                    start,
                    zombie: false,
                    pgid,
                    sid,
                },
            );
            inner.made.push(Made {
                pid,
                version,
                program: program.into(),
                start,
            });
        }

        /// `pid` takes `step`.
        pub(super) fn step(&self, pid: c_int, step: Step) {
            match step {
                Step::Exec(program) => {
                    let inner = &mut *self.0.borrow_mut();
                    let version = Self::next_version(inner);
                    let p = inner.procs.get_mut(&pid).expect("a live pid");
                    assert!(!p.zombie, "a zombie runs nothing");
                    p.version = version;
                    p.program = program.into();
                    let start = p.start;
                    inner.made.push(Made {
                        pid,
                        version,
                        program: program.into(),
                        start,
                    });
                }
                Step::Reuse(program, uid) => {
                    self.0.borrow_mut().procs.remove(&pid).expect("a pid");
                    self.spawn(pid, program, uid);
                }
                Step::Die => {
                    self.0
                        .borrow_mut()
                        .procs
                        .get_mut(&pid)
                        .expect("a live pid")
                        .zombie = true;
                }
                Step::Reap => {
                    self.0.borrow_mut().procs.remove(&pid).expect("a pid");
                }
                Step::SetUid(uid) => {
                    self.0.borrow_mut().procs.get_mut(&pid).expect("a pid").uid = uid;
                }
                Step::SetPgid(pgid) => {
                    let inner = &mut *self.0.borrow_mut();
                    let p = &inner.procs[&pid];
                    assert_ne!(p.sid, pid, "setpgid is refused to a session's leader");
                    assert!(
                        pgid == pid
                            || inner
                                .procs
                                .values()
                                .any(|q| q.pgid == pgid && q.sid == p.sid),
                        "setpgid into no group of its session"
                    );
                    inner.procs.get_mut(&pid).expect("a pid").pgid = pgid;
                }
                Step::SetSid => {
                    let inner = &mut *self.0.borrow_mut();
                    assert!(
                        !inner.procs.values().any(|q| q.pgid == pid),
                        "setsid is refused to a group's leader, and where its pid's group is left"
                    );
                    let p = inner.procs.get_mut(&pid).expect("a pid");
                    p.pgid = pid;
                    p.sid = pid;
                }
            }
        }

        /// `pid` execs `program`: the same process, a new pid version.
        pub(super) fn exec(&self, pid: c_int, program: &'static str) {
            self.step(pid, Step::Exec(program));
        }

        /// `pid` exits, unreaped.
        pub(super) fn die(&self, pid: c_int) {
            self.step(pid, Step::Die);
        }

        /// `pid` is gone.
        pub(super) fn reap(&self, pid: c_int) {
            self.step(pid, Step::Reap);
        }

        /// `pid` takes `step` as the sweep makes its read of kind `kind` (a [`Reads`] method's
        /// name) after `pass` such reads of it, before that read answers.
        pub(super) fn race(&self, kind: &'static str, pid: c_int, pass: u32, step: Step) {
            self.0.borrow_mut().races.push((kind, pid, pass, step));
        }

        /// Drops the races no read reached, and counts them.
        pub(super) fn drop_races(&self) -> usize {
            std::mem::take(&mut self.0.borrow_mut().races).len()
        }

        /// `pid`'s next rusage reads answer `answers` (a start, or an errno), in turn.
        pub(super) fn script_rusage(&self, pid: c_int, answers: &[Result<u64, i32>]) {
            self.0
                .borrow_mut()
                .rusage
                .insert(pid, answers.iter().copied().collect());
        }

        /// `pid`'s next version reads answer `answers`, in turn.
        pub(super) fn script_version(&self, pid: c_int, answers: &[Option<i32>]) {
            self.0
                .borrow_mut()
                .version
                .insert(pid, answers.iter().copied().collect());
        }

        /// `pid`'s current pid version.
        pub(super) fn version_of(&self, pid: c_int) -> i32 {
            self.0.borrow().procs[&pid].version
        }

        /// `pid`'s session id now, as `getsid` answers it.
        pub(super) fn sid_of(&self, pid: u32) -> u32 {
            let pid = c_int::try_from(pid).expect("a pid");
            self.0.borrow().procs[&pid].sid.unsigned_abs()
        }

        /// Every generation the world made.
        pub(super) fn made(&self) -> Vec<Made> {
            self.0.borrow().made.clone()
        }

        /// Every read since the last take, by kind and pid.
        pub(super) fn take_reads(&self) -> BTreeMap<(&'static str, c_int), u32> {
            std::mem::take(&mut self.0.borrow_mut().reads)
        }

        /// A read of kind `kind` of `pid`: the race scripted there taken first, then counted.
        fn read(&self, kind: &'static str, pid: c_int) -> std::cell::RefMut<'_, Inner> {
            let due = {
                let inner = &mut *self.0.borrow_mut();
                match inner
                    .races
                    .iter()
                    .position(|(k, p, _, _)| *k == kind && *p == pid)
                {
                    Some(at) if inner.races[at].2 == 0 => Some(inner.races.remove(at).3),
                    Some(at) => {
                        inner.races[at].2 -= 1;
                        None
                    }
                    None => None,
                }
            };
            if let Some(step) = due {
                self.step(pid, step);
            }
            let mut inner = self.0.borrow_mut();
            *inner.reads.entry((kind, pid)).or_default() += 1;
            inner
        }
    }

    fn usage(start: u64, version: i32) -> Usage {
        let mut ri = RusageV6::default();
        ri.ri_proc_start_abstime = start;
        ri.ri_user_time = 7_000;
        ri.ri_phys_footprint = footprint_of(version);
        Usage { ri, v6: true }
    }

    fn cut(program: &str, at: usize) -> &[u8] {
        &program.as_bytes()[..program.len().min(at)]
    }

    impl Reads for World {
        fn pids(&self) -> Option<Vec<c_int>> {
            let inner = self.read("pids", 0);
            let (zombies, live): (Vec<_>, Vec<_>) = inner.procs.iter().partition(|(_, p)| p.zombie);
            Some(
                live.into_iter()
                    .rev()
                    .chain(zombies.into_iter().rev())
                    .map(|(pid, _)| *pid)
                    .collect(),
            )
        }

        fn rusage(&self, pid: c_int) -> Result<Usage, i32> {
            let mut inner = self.read("rusage", pid);
            let version = inner.procs.get(&pid).map_or(0, |p| p.version);
            if let Some(answer) = inner.rusage.get_mut(&pid).and_then(VecDeque::pop_front) {
                return answer.map(|start| usage(start, version));
            }
            match inner.procs.get(&pid) {
                None => Err(libc::ESRCH),
                Some(p) if p.uid != OWN && !p.zombie => Err(libc::EPERM),
                Some(p) => Ok(usage(p.start, p.version)),
            }
        }

        fn short_info(&self, pid: c_int) -> Option<ShortInfo> {
            let inner = self.read("short_info", pid);
            let p = inner.procs.get(&pid).filter(|p| !p.zombie)?;
            let mut info = ShortInfo::default();
            info.pid = pid.unsigned_abs();
            info.ppid = 1;
            info.pgid = p.pgid.unsigned_abs();
            info.status = 2;
            info.uid = p.uid;
            let comm = cut(&p.program, 15);
            info.comm[..comm.len()].copy_from_slice(comm);
            Some(info)
        }

        fn version(&self, pid: c_int) -> Option<i32> {
            let mut inner = self.read("version", pid);
            if let Some(answer) = inner.version.get_mut(&pid).and_then(VecDeque::pop_front) {
                return answer;
            }
            inner
                .procs
                .get(&pid)
                .filter(|p| !p.zombie)
                .map(|p| p.version)
        }

        fn coalition(&self, pid: u32) -> Option<u64> {
            let inner = self.read("coalition", c_int::try_from(pid).ok()?);
            inner
                .procs
                .get(&c_int::try_from(pid).ok()?)
                .filter(|p| !p.zombie)
                .map(|p| coalition_of(p.version))
        }

        fn priority(&self, pid: c_int, _which: c_int) -> Result<i32, i32> {
            let _ = self.read("priority", pid);
            Ok(0)
        }

        fn sid(&self, pid: c_int) -> c_int {
            let inner = self.read("sid", pid);
            inner
                .procs
                .get(&pid)
                .filter(|p| !p.zombie)
                .map_or(-1, |p| p.sid)
        }

        fn p_name(&self, pid: c_int) -> Option<Vec<u8>> {
            let inner = self.read("p_name", pid);
            let p = inner.procs.get(&pid).filter(|p| p.uid == OWN)?;
            Some(cut(&p.program, 31).to_vec())
        }

        fn image_name(&self, pid: c_int) -> Option<Vec<u8>> {
            let inner = self.read("image_name", pid);
            let p = inner.procs.get(&pid).filter(|p| !p.zombie)?;
            Some(p.program.as_bytes().to_vec())
        }

        fn kinfo(&self, pid: c_int) -> Option<Kinfo> {
            let inner = self.read("kinfo", pid);
            let p = inner.procs.get(&pid)?;
            Some(Kinfo {
                pid: pid.unsigned_abs(),
                ppid: 1,
                pgid: p.pgid.unsigned_abs(),
                stat: if p.zombie { 5 } else { 2 },
                uid: p.uid,
                start_us: 0,
                tty: None,
                tpgid: None,
                session_leader: p.sid == pid,
                exec: true,
                reaps_unwaited: false,
                comm: String::from_utf8_lossy(cut(&p.program, 16)).into_owned(),
            })
        }

        fn unwaited(&self) -> Option<Vec<(u32, u32, bool)>> {
            let inner = self.read("unwaited", 0);
            Some(
                inner
                    .procs
                    .keys()
                    .map(|pid| (pid.unsigned_abs(), 1, false))
                    .collect(),
            )
        }

        fn own_uid(&self) -> u32 {
            let _ = self.read("own_uid", 0);
            OWN
        }
    }
}

/// A sweep of `world` as a whole reading with every reader asks it (coalitions, versions and
/// scheduling), keeping in `ids`, its rows as they compare: with `read_at`, the clock's reading,
/// left out.
#[cfg(target_os = "macos")]
fn swept(world: &world::World, ids: &mut crate::coalitions::CoalitionIds) -> Vec<ProcessReading> {
    let opts = SweepOpts {
        coalitions: true,
        versions: true,
        scheduling: true,
    };
    imp::sweep(world, opts, ids)
        .expect("a sweep")
        .procs
        .into_iter()
        .map(|p| ProcessReading { read_at: None, ..p })
        .collect()
}

/// The sweep's generation laws, judged sweep by sweep over [`swept`]'s rows and the reads it
/// made, with each pid's refusal the last sweep kept a row of.
#[cfg(target_os = "macos")]
#[derive(Default)]
struct Ledger {
    /// Each pid whose last kept row was refused another user's usage: its pid version and uid.
    refused: std::collections::BTreeMap<u32, (i32, u32)>,
}

#[cfg(target_os = "macos")]
impl Ledger {
    /// LAW (i): every live row the sweep kept holds the usage, short name, coalition, version and
    /// whole name of one generation the world made at its pid. LAW (ii): a live row whose usage
    /// was read holds the one usage read made of its pid in the sweep; a refused one made one
    /// read, or none where the pid's last kept row was refused under the same pid version and
    /// uid (the uid another user's: the sweep's own is read again).
    fn judge(
        &mut self,
        world: &world::World,
        rows: &[ProcessReading],
        reads: &std::collections::BTreeMap<(&'static str, std::ffi::c_int), u32>,
    ) -> Result<(), String> {
        let made = world.made();
        let mut refused = std::collections::BTreeMap::new();
        for row in rows
            .iter()
            .filter(|r| r.state != crate::jobs::RunState::Zombie)
        {
            let of_one = made.iter().any(|g| {
                g.pid.unsigned_abs() == row.pid
                    && row.name == g.program
                    && row.start.is_none_or(|s| s == g.start)
                    && row
                        .footprint
                        .is_none_or(|f| f == world::footprint_of(g.version))
                    && row
                        .coalition
                        .is_none_or(|c| c == world::coalition_of(g.version))
                    && row.version.is_none_or(|v| v == g.version)
            });
            if !of_one {
                return Err(format!("LAW (i): of no one generation: {row:?}"));
            }
            let pid = std::ffi::c_int::try_from(row.pid).map_err(|e| e.to_string())?;
            let usage_reads = reads.get(&("rusage", pid)).copied().unwrap_or(0);
            let Some(version) = row.version else {
                continue;
            };
            let refusal = row.scheduling.and_then(|s| s.qos_cpu_ns) == Some(Err(libc::EPERM));
            let uid = row.uid.unwrap_or(world::OWN);
            let want = if !refusal {
                1
            } else if self.refused.get(&row.pid) == Some(&(version, uid)) {
                0
            } else {
                1
            };
            if usage_reads != want {
                return Err(format!(
                    "LAW (ii): {usage_reads} usage reads, not {want}: {row:?}"
                ));
            }
            if refusal && uid != world::OWN {
                refused.insert(row.pid, (version, uid));
            }
        }
        self.refused = refused;
        Ok(())
    }
}

/// LAW (iv): every live row's session id is the one `getsid` answers of its pid now.
#[cfg(target_os = "macos")]
fn sessions_hold(world: &world::World, rows: &[ProcessReading]) -> Result<(), String> {
    match rows
        .iter()
        .filter(|r| r.state != crate::jobs::RunState::Zombie)
        .find(|r| r.sid != world.sid_of(r.pid))
    {
        Some(row) => Err(format!(
            "LAW (iv): sid {} where getsid answers {}: {row:?}",
            row.sid,
            world.sid_of(row.pid)
        )),
        None => Ok(()),
    }
}

/// A world of every kind of row the laws judge: a session's leader, a job's leader (a group's
/// leader in another's session) with a member at the short name's cut, another user's process
/// with a long name, and a zombie.
#[cfg(target_os = "macos")]
fn law_world() -> world::World {
    use world::{OTHER, OWN, World};
    let world = World::default();
    world.spawn(100, "zsh", OWN);
    // A test binary's name: 31 bytes, at `p_name`'s cut as at the short name's.
    world.spawn_in(200, "aterm_sysprobe-0123456789abcdef", OWN, 200, 100);
    world.spawn(300, "com.apple.WebKit.WebContent", OTHER);
    world.spawn_in(400, "SetStoreUpdate1", OWN, 200, 100);
    world.spawn(500, "exited", OWN);
    world.die(500);
    world
}

/// EVERY KEPT ROW IS READ OF ONE GENERATION (macOS, a scripted kernel world): whichever of a
/// row's reads a step races (its first and last version reads, its usage, short info,
/// coalition, either scheduling read, session and longer names; an exec, a new process at the
/// pid, an exit, a reap, a new effective uid, a new group, a new session), on first sight or
/// with the generation's facts kept from the sweep before: LAW (i), every live row the sweep keeps
/// holds facts of one generation; LAW (ii), each kept live row with a readable usage was read
/// once, and a refused one at most once a generation; a row a version-moving step landed inside
/// (after its first version read) is not kept live; LAW (v), a process that exits (unreaped) as
/// any of its row's reads is made has a zombie row, whose start, where it names one, is its
/// generation's, and which names one where its usage was readable (this user's); and the next sweep
/// with no race answers what a sweep reading every fact fresh answers (LAW (iii)) with each
/// session id `getsid`'s (LAW (iv)). The world's exec sets the names with the version at one
/// instant, as xnu's does not (`imp::sweep`'s bracket): of names, LAW (i) holds in such a world.
/// NEGATIVE CONTROLS (each run on a mutant `imp::sweep`): keeping a row whose second version read
/// answered another version fails LAW (i); dropping a row whose last version read answered none
/// fails LAW (v); a second usage read before the last version read fails LAW (ii), as do a
/// refusal kept across an exec, a refusal answered whatever uid the short info names, and, on the
/// sweep after this user's uid raced to root's process, a refusal kept where its uid is the
/// sweep's own; a session kept whatever the group fails LAW (iv) once a group's member leads a
/// session of its own.
#[cfg(target_os = "macos")]
#[test]
fn every_kept_row_is_read_of_one_generation() {
    use world::{OTHER, OWN, Step};
    let kinds: [(&str, u32); 10] = [
        ("version", 0),
        ("rusage", 0),
        ("short_info", 0),
        ("coalition", 0),
        ("priority", 0),
        ("priority", 1),
        ("sid", 0),
        ("p_name", 0),
        ("image_name", 0),
        ("version", 1),
    ];
    let steps: [(i32, &[Step]); 3] = [
        (
            200,
            &[
                Step::Exec("aterm_sysprobe-fedcba9876543210"),
                Step::Die,
                Step::Reap,
                Step::SetUid(OTHER),
                Step::SetPgid(100),
            ],
        ),
        (
            300,
            &[
                Step::Exec("com.apple.Safari.SafeBrowsing"),
                Step::Reuse("com.apple.WebKit.Networking", OWN),
                Step::Reuse("com.apple.WebKit.GPUProcess", OTHER),
                Step::Die,
                Step::Reap,
                Step::SetUid(OWN),
            ],
        ),
        (
            400,
            &[
                Step::Exec("softwareupdated"),
                Step::Reuse("SetStoreUpdate2", OWN),
                Step::Die,
                Step::Reap,
                Step::SetUid(OTHER),
                Step::SetPgid(400),
                Step::SetSid,
            ],
        ),
    ];
    let mut fired = std::collections::BTreeMap::<(&str, u32), u32>::new();
    let mut cases = 0;
    for (target, target_steps) in steps {
        for &step in target_steps {
            for (kind, pass) in kinds {
                for kept_before in [false, true] {
                    let what =
                        format!("{step:?} of {target} at {kind} #{pass}, kept {kept_before}");
                    let world = law_world();
                    let mut ids = crate::coalitions::CoalitionIds::default();
                    let mut ledger = Ledger::default();
                    if kept_before {
                        let rows = swept(&world, &mut ids);
                        ledger
                            .judge(&world, &rows, &world.take_reads())
                            .unwrap_or_else(|e| panic!("{what}, the sweep before: {e}"));
                    }
                    world.race(kind, target, pass, step);
                    let rows = swept(&world, &mut ids);
                    let reads = world.take_reads();
                    ledger
                        .judge(&world, &rows, &reads)
                        .unwrap_or_else(|e| panic!("{what}: {e}"));
                    if world.drop_races() == 0 {
                        *fired.entry((kind, pass)).or_default() += 1;
                        let moves = matches!(
                            step,
                            Step::Exec(_) | Step::Reuse(..) | Step::Die | Step::Reap
                        );
                        if moves && (kind, pass) != ("version", 0) {
                            assert!(
                                rows.iter().all(|r| r.pid != target.unsigned_abs()
                                    || r.state == crate::jobs::RunState::Zombie),
                                "{what}: a row its version moved across is not kept live"
                            );
                        }
                        if matches!(step, Step::Die) {
                            let start = world
                                .made()
                                .iter()
                                .rev()
                                .find(|g| g.pid == target)
                                .map(|g| g.start);
                            let zombie = rows.iter().find(|r| {
                                r.pid == target.unsigned_abs()
                                    && r.state == crate::jobs::RunState::Zombie
                            });
                            // Its usage, read by the zombie pass under the start its live read
                            // named; another user's (300's), refused while it ran, names none.
                            let read = zombie.is_some_and(|z| match z.start {
                                Some(s) => Some(s) == start,
                                None => target == 300,
                            });
                            assert!(
                                read,
                                "{what}: LAW (v), an exit inside the row is read as a zombie: {zombie:?}"
                            );
                        }
                    }
                    let fresh = swept(
                        &world.clone(),
                        &mut crate::coalitions::CoalitionIds::default(),
                    );
                    let rows = swept(&world, &mut ids);
                    let reads = world.take_reads();
                    ledger
                        .judge(&world, &rows, &reads)
                        .unwrap_or_else(|e| panic!("{what}, the sweep after: {e}"));
                    sessions_hold(&world, &rows)
                        .unwrap_or_else(|e| panic!("{what}, the sweep after: {e}"));
                    assert_eq!(rows, fresh, "{what}, the sweep after: LAW (iii)");
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 18 * 10 * 2);
    for kind in kinds {
        assert!(
            fired.get(&kind).is_some_and(|n| *n > 0),
            "a race at every read: none at {kind:?} ({fired:?})"
        );
    }
}

/// KEPT GENERATION FACTS ANSWER AS FRESH READS (macOS, a scripted kernel world), from one sweep
/// to the next as processes exec, change their effective uid, leave their group and lead a
/// session, and end: LAWS (i) and (ii) as [`every_kept_row_is_read_of_one_generation`] holds
/// them, LAW (iii), every row a sweep reading every fact fresh answers, and LAW (iv), each
/// session id `getsid`'s. A session's leader and a group's member are asked their session only
/// on first sight of a generation, a group's leader that leads no session at every sweep; a
/// refused usage is read once a generation and uid. NEGATIVE CONTROLS (each run on a mutant
/// `imp::sweep`): a session kept whatever the group asks neither job's leader again; a refusal
/// kept across an exec (its pid version) fails LAW (ii); a refusal answered whatever uid the short
/// info names fails LAW (iii) where root's process takes this user's uid.
#[cfg(target_os = "macos")]
#[test]
fn kept_generation_facts_answer_as_fresh_reads() {
    use world::{OTHER, OWN, Step, World};
    let world = World::default();
    world.spawn(100, "zsh", OWN);
    world.spawn_in(200, "cargo", OWN, 200, 100);
    world.spawn_in(201, "rustc", OWN, 200, 100);
    world.spawn(300, "com.apple.WebKit.WebContent", OTHER);
    world.spawn_in(600, "make", OWN, 600, 100);
    let mut ids = crate::coalitions::CoalitionIds::default();
    let mut ledger = Ledger::default();
    let mut step = |what: &str| {
        let fresh = swept(
            &world.clone(),
            &mut crate::coalitions::CoalitionIds::default(),
        );
        let rows = swept(&world, &mut ids);
        let reads = world.take_reads();
        assert_eq!(rows, fresh, "{what}: LAW (iii)");
        ledger
            .judge(&world, &rows, &reads)
            .unwrap_or_else(|e| panic!("{what}: {e}"));
        sessions_hold(&world, &rows).unwrap_or_else(|e| panic!("{what}: {e}"));
        move |kind: &'static str, pid: std::ffi::c_int| {
            reads.get(&(kind, pid)).copied().unwrap_or(0)
        }
    };
    let sids = |n: &dyn Fn(&'static str, std::ffi::c_int) -> u32| {
        [100, 200, 201, 300, 600].map(|pid| n("sid", pid))
    };

    let n = step("first sight");
    assert_eq!(sids(&n), [1; 5], "first sight: every session read");
    assert_eq!(n("rusage", 300), 1, "and the refused usage");
    let n = step("no step");
    assert_eq!(
        sids(&n),
        [0, 1, 0, 0, 1],
        "only the jobs' leaders, which lead no session, are asked again"
    );
    assert_eq!(
        n("rusage", 300),
        0,
        "a refusal kept for its generation and uid"
    );

    world.step(300, Step::Exec("com.apple.Safari.SafeBrowsing"));
    let n = step("another user's process execs");
    assert_eq!(
        (n("rusage", 300), n("sid", 300)),
        (1, 1),
        "a new generation: read again"
    );
    world.step(300, Step::SetUid(OWN));
    let n = step("root's process takes this user's uid");
    assert_eq!(n("rusage", 300), 1, "a refusal is not kept across its uid");
    world.step(300, Step::SetUid(OTHER));
    let n = step("and gives it up");
    assert_eq!(n("rusage", 300), 1);
    let n = step("refused again");
    assert_eq!(n("rusage", 300), 0);

    world.step(600, Step::SetPgid(100));
    world.step(600, Step::SetSid);
    let n = step("a job's leader leaves its group and leads a session");
    assert_eq!(
        n("sid", 600),
        1,
        "a group's leader with another's session is asked"
    );
    let n = step("the session's leader now");
    assert_eq!(n("sid", 600), 0, "a session's leader is not asked again");

    world.step(201, Step::SetPgid(201));
    let n = step("a job's member makes a group of its own");
    assert_eq!(
        n("sid", 201),
        1,
        "now a group's leader in another's session"
    );
    world.step(201, Step::Reuse("cc", OWN));
    let _ = step("a new process at the member's pid");
    world.die(200);
    let _ = step("the job's leader exits");
    world.reap(200);
    let _ = step("and is reaped");
}

/// THE SWEEP READS EACH GENERATION ONCE (macOS, a scripted kernel world). A sweep that keeps its
/// ids from one sweep to the next answers, at each step, every row a fresh sweep of the same world
/// answers, names included (`read_at`, the clock, aside). A name at the short name's cut is read
/// whole (`p_name`, then the image's basename) once per pid version and short name: on first
/// sight, again after an exec that keeps the short name and after one that moves it, and again
/// after an exec that raced the sweep's reads, whose row that sweep dropped; a name below the cut
/// is never read; one that left the cut or whose pid is gone is not kept. A readable usage is read
/// once a sweep, one the kernel refuses (another user's process) once a generation, and a
/// zombie's twice (in the live pass, then in the zombie pass). NEGATIVE CONTROLS: a name kept by
/// pid alone answers the pre-exec name, so the rows part from the fresh sweep's; a refusal kept
/// across an exec reads the new generation's usage no time.
#[cfg(target_os = "macos")]
#[test]
fn a_keeping_sweep_reads_each_generation_once() {
    use world::{OTHER, OWN, Step, World};
    let world = World::default();
    world.spawn(100, "zsh", OWN);
    // A test binary's name: 31 bytes, at `p_name`'s cut as at the short name's.
    world.spawn(200, "aterm_sysprobe-0123456789abcdef", OWN);
    world.spawn(300, "com.apple.WebKit.WebContent", OTHER);
    world.spawn(400, "SetStoreUpdate1", OWN);
    world.spawn(500, "exited", OWN);
    world.die(500);
    let mut ids = crate::coalitions::CoalitionIds::default();
    let step = |ids: &mut crate::coalitions::CoalitionIds, what: &str, refused_reads: u32| {
        let fresh = swept(
            &world.clone(),
            &mut crate::coalitions::CoalitionIds::default(),
        );
        let kept = swept(&world, ids);
        let reads = world.take_reads();
        assert_eq!(kept, fresh, "{what}: the rows a fresh sweep reads");
        let n = |kind: &'static str, pid: i32| reads.get(&(kind, pid)).copied().unwrap_or(0);
        assert_eq!(
            n("rusage", 300),
            refused_reads,
            "{what}: the refused usage's reads"
        );
        for row in kept.iter().filter(|p| p.uid == Some(OWN)) {
            let pid = i32::try_from(row.pid).expect("a pid");
            let want = if row.state == crate::jobs::RunState::Zombie {
                2
            } else {
                1
            };
            assert_eq!(n("rusage", pid), want, "{what}: a readable usage's reads");
        }
        let named = |pid: i32| (n("p_name", pid), n("image_name", pid));
        (kept, [100, 200, 300, 400].map(named))
    };
    let name = |rows: &[ProcessReading], pid: u32| {
        rows.iter()
            .find(|p| p.pid == pid)
            .map(|p| p.name.clone())
            .expect("swept")
    };

    let (rows, reads) = step(&mut ids, "first sight", 1);
    assert_eq!(reads, [(0, 0), (1, 1), (1, 1), (1, 0)], "first sight");
    assert_eq!(name(&rows, 200), "aterm_sysprobe-0123456789abcdef");
    assert_eq!(name(&rows, 300), "com.apple.WebKit.WebContent");
    let refused = rows.iter().find(|p| p.pid == 300).expect("swept");
    assert_eq!(
        (refused.start, refused.cpu_ns, refused.footprint),
        (None, None, None),
        "a refused usage: no counters {refused:?}"
    );
    assert_eq!(
        refused.scheduling.and_then(|s| s.qos_cpu_ns),
        Some(Err(libc::EPERM)),
        "and its refusal"
    );
    assert_eq!(
        ids.kept_name(200),
        Some((world.version_of(200), "aterm_sysprobe-0123456789abcdef"))
    );
    assert_eq!(ids.kept_name(100), None, "a name below the cut is not kept");

    let (_, reads) = step(&mut ids, "no exec", 0);
    assert_eq!(reads, [(0, 0); 4], "every generation kept");

    world.exec(200, "aterm_sysprobe-fedcba9876543210");
    let (rows, reads) = step(&mut ids, "an exec that keeps the short name", 0);
    assert_eq!(reads, [(0, 0), (1, 1), (0, 0), (0, 0)]);
    assert_eq!(name(&rows, 200), "aterm_sysprobe-fedcba9876543210");

    world.exec(300, "com.apple.Safari.SafeBrowsing");
    let (rows, reads) = step(&mut ids, "an exec that moves the short name", 1);
    assert_eq!(reads, [(0, 0), (0, 0), (1, 1), (0, 0)]);
    assert_eq!(name(&rows, 300), "com.apple.Safari.SafeBrowsing");

    world.race("short_info", 400, 0, Step::Exec("softwareupdated"));
    let (rows, reads) = step(
        &mut ids,
        "an exec between the usage's read and the short name's",
        0,
    );
    assert_eq!(reads, [(0, 0), (0, 0), (0, 0), (1, 0)]);
    assert!(
        rows.iter().all(|p| p.pid != 400),
        "the row the exec moved the version across is dropped"
    );
    assert_eq!(ids.kept_name(400), None, "and nothing read for it kept");
    let (rows, reads) = step(&mut ids, "the new program's row", 0);
    assert_eq!(reads, [(0, 0), (0, 0), (0, 0), (1, 0)]);
    assert_eq!(name(&rows, 400), "softwareupdated");

    world.exec(200, "zsh");
    world.reap(400);
    let (rows, reads) = step(&mut ids, "a name below the cut, and a pid gone", 0);
    assert_eq!(reads, [(0, 0); 4]);
    assert_eq!(name(&rows, 200), "zsh");
    assert_eq!(ids.kept_name(200), None, "a name that left the cut");
    assert_eq!(ids.kept_name(400), None, "a gone pid's");
    assert_eq!(
        ids.kept_name(300),
        Some((world.version_of(300), "com.apple.Safari.SafeBrowsing"))
    );
}

/// A ROW WHOSE FIRST VERSION READ ANSWERS NONE IS BRACKETED BY ITS START (macOS, a scripted
/// kernel world): its usage is read again after its other reads; a row whose second usage read
/// names another start (a new process at the pid) or is refused is dropped from the sweep, and one
/// whose two reads name one start is kept with the second's counters and no version. A first read
/// the kernel refused is the only one made: the row is kept with no counters and that refusal even
/// where a second read would have answered (a process that became this user's between the reads)
/// or found the process gone (`ESRCH`, a reap between them). A row whose first version read
/// answers reads its usage once, under that version. NEGATIVE CONTROL: without the second read
/// the row of another start is kept.
#[cfg(target_os = "macos")]
#[test]
fn a_row_whose_first_version_is_unread_is_bracketed_by_its_start() {
    use world::{OWN, World};
    let world = World::default();
    for pid in [200, 201, 202, 203, 204, 205] {
        world.spawn(pid, "worker", OWN);
    }
    for pid in [200, 201, 202, 203, 204] {
        world.script_version(pid, &[None]);
    }
    world.script_rusage(201, &[Ok(1_201), Ok(9_999)]);
    world.script_rusage(202, &[Ok(1_202), Err(libc::EPERM)]);
    world.script_rusage(203, &[Err(libc::EPERM), Ok(1_203)]);
    world.script_rusage(204, &[Err(libc::EPERM), Err(libc::ESRCH)]);
    let rows = swept(&world, &mut crate::coalitions::CoalitionIds::default());
    let reads = world.take_reads();
    let refusal = |pid: u32| {
        rows.iter()
            .find(|p| p.pid == pid)
            .and_then(|p| p.scheduling)
            .and_then(|s| s.qos_cpu_ns)
    };
    assert_eq!(
        [refusal(203), refusal(204)],
        [Some(Err(libc::EPERM)); 2],
        "the first read's refusal, not a second read's answer"
    );
    assert_eq!(
        rows.iter()
            .map(|p| (p.pid, p.start, p.version))
            .collect::<Vec<_>>(),
        [
            (205, Some(1_205), Some(world.version_of(205))),
            (204, None, None),
            (203, None, None),
            (200, Some(1_200), None)
        ],
        "the kept rows, newest first"
    );
    let n = |kind: &'static str| {
        [200, 201, 202, 203, 204, 205].map(|pid| reads.get(&(kind, pid)).copied().unwrap_or(0))
    };
    assert_eq!(n("rusage"), [2, 2, 2, 1, 1, 1]);
    assert_eq!(n("version"), [1, 1, 1, 1, 1, 2]);
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
    if cfg!(any(target_os = "macos", target_os = "linux")) {
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
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        assert!(mine.runnable_ns.is_some_and(|r| r > 0), "{mine:?}");
        if cfg!(target_os = "linux") {
            // Linux constructs this value from CPU time plus scheduler wait time.
            // macOS reads a separate scheduler timer and CPU accounting; their
            // independently updated counters need not satisfy this ordering.
            assert!(
                mine.runnable_ns
                    .is_some_and(|r| mine.cpu_ns.is_some_and(|c| r >= c)),
                "{mine:?}"
            );
        }
    }
    if cfg!(target_os = "macos") {
        assert!(mine.peak_footprint.is_some_and(|p| p > 0), "{mine:?}");
        assert!(mine.disk_written.is_some(), "{mine:?}");
    }
    if cfg!(target_os = "linux") {
        // The test harness runs tests on threads of its own.
        assert!(mine.threads.is_some_and(|t| t >= 2), "{mine:?}");
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

/// A SWEEP READS SCHEDULING ONLY WHEN ASKED (C4; macOS, a scripted kernel world): a sweep whose
/// options ask none makes no `getpriority` and leaves every row's scheduling `None`, its usage
/// reads the same as when asked; one that asks makes two for each live process (its nice and
/// its Darwin role) and gives each live row its scheduling, and a zombie none. NEGATIVE
/// CONTROL: a sweep that reads scheduling whatever its options makes six unasked, which the
/// first assertion refuses; the asking sweep over the same world counts them.
#[cfg(target_os = "macos")]
#[test]
fn a_sweep_reads_scheduling_only_when_asked() {
    use world::{OWN, World};
    let world = World::default();
    for pid in [200, 201, 202] {
        world.spawn(pid, "worker", OWN);
    }
    world.spawn(203, "gone", OWN);
    world.die(203);
    let sweep = |scheduling| {
        let opts = SweepOpts {
            coalitions: true,
            versions: true,
            scheduling,
        };
        let rows = imp::sweep(
            &world,
            opts,
            &mut crate::coalitions::CoalitionIds::default(),
        )
        .expect("a sweep")
        .procs;
        let reads = world.take_reads();
        let count = |kind: &str| {
            reads
                .iter()
                .filter(|((k, _), _)| *k == kind)
                .map(|(_, n)| *n)
                .sum::<u32>()
        };
        let mut read = rows
            .iter()
            .filter(|p| p.scheduling.is_some())
            .map(|p| p.pid)
            .collect::<Vec<_>>();
        read.sort_unstable();
        (count("priority"), count("rusage"), read)
    };
    let (priorities, usage, read) = sweep(false);
    assert_eq!(priorities, 0, "no getpriority unasked");
    assert_eq!(read, [0_u32; 0], "and no row's scheduling");
    let (asked, asked_usage, asked_read) = sweep(true);
    assert_eq!(asked, 2 * 3, "CONTROL: two a live process when asked");
    assert_eq!(asked_read, [200, 201, 202], "every live row's, no zombie's");
    assert_eq!(
        usage, asked_usage,
        "the usage reads are the same either way"
    );
}
