// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A terminal start ([`super`]): its child leads a session on the slave, with the forked tab
//! shell's signals; each step that stops it is named with its errno, in a shared child and a
//! copy; the report's end is moved off a pass's number; a child that never execs is killed at
//! the bound; and another holder of the report's end delays nothing.

use super::{Limit, Start, Started, Step, start};
use crate::spawnfd::PassAt;
use crate::spawnfd::clone_spawn::{
    Seam, last_pid, sharing_or_could_not_run, with_child, with_report,
};
use crate::spawnfd::tests::native_shared_child_applicable;
use aterm_spec::wait::MUST_HAPPEN;
use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd};
use std::time::{Duration, Instant};

/// A fresh pty: its master and its slave, each close-on-exec, and the slave's name.
fn pty() -> (OwnedFd, OwnedFd, String) {
    // SAFETY: flags only; the result is a fresh descriptor or -1.
    let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
    assert!(master >= 0, "posix_openpt: {}", io::Error::last_os_error());
    // SAFETY: just opened here, owned by nothing else.
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    let mut name = [0 as libc::c_char; 128];
    // SAFETY: the master just opened, and a live buffer of the length passed.
    unsafe {
        assert_eq!(libc::grantpt(master.as_raw_fd()), 0);
        assert_eq!(libc::unlockpt(master.as_raw_fd()), 0);
        assert_eq!(
            libc::ptsname_r(master.as_raw_fd(), name.as_mut_ptr(), name.len()),
            0
        );
    }
    // SAFETY: `ptsname_r` NUL-terminated it.
    let name = unsafe { CStr::from_ptr(name.as_ptr()) };
    // SAFETY: a NUL-terminated path; the result is a fresh descriptor or -1.
    let slave = unsafe {
        libc::open(
            name.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        )
    };
    assert!(slave >= 0, "open {name:?}: {}", io::Error::last_os_error());
    // SAFETY: just opened here, owned by nothing else.
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    (master, slave, name.to_string_lossy().into_owned())
}

fn strings(words: &[&str]) -> Vec<CString> {
    words
        .iter()
        .map(|word| CString::new(*word).expect("no NUL"))
        .collect()
}

/// One case's start.
type Try<'a> = Box<dyn Fn() -> Started + 'a>;

/// A start of `program` with `argv` on `slave`, bounded by the hang detector.
fn run(program: &CStr, argv: &[&str], slave: BorrowedFd<'_>) -> Started {
    run_with(program, argv, slave, None, &[], None)
}

fn run_with(
    program: &CStr,
    argv: &[&str],
    slave: BorrowedFd<'_>,
    cwd: Option<&CStr>,
    limits: &[Limit],
    pass: Option<PassAt>,
) -> Started {
    start(
        &Start {
            program,
            argv: &strings(argv),
            envp: &strings(&["PATH=/usr/bin:/bin"]),
            cwd,
            limits,
            pass,
        },
        slave,
        MUST_HAPPEN,
    )
    .expect("a start")
}

/// Kill and reap a running child.
fn end(pid: i32) {
    // SAFETY: our own child, not yet reaped.
    unsafe { libc::kill(pid, libc::SIGKILL) };
    let mut status = 0;
    // SAFETY: our own child; `status` is a live local.
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
}

/// Whether `pid`, a child a start killed or saw stop, is gone (reaped by the start or the reaper
/// it was handed to) within the hang detector.
fn gone(pid: i32) -> bool {
    let deadline = Instant::now() + MUST_HAPPEN;
    // SAFETY: a probe of `pid` with no signal.
    while unsafe { libc::kill(pid, 0) } == 0 {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

/// The fields of `/proc/<pid>/stat` after its command's name: state first.
fn stat_fields(pid: i32) -> Vec<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("stat");
    let after = &stat[stat.rfind(')').expect("a command name") + 2..];
    after.split(' ').map(str::to_owned).collect()
}

/// The child leads a session of its own on the slave, which is its controlling terminal and its
/// 0, 1 and 2, as `login_tty` leaves a forked shell.
#[test]
fn a_program_leads_a_session_on_its_terminal_with_the_slave_as_its_stdio() {
    for copy in [false, true] {
        let (_master, slave, name) = pty();
        let started = with_child(copy, Seam::None, || {
            run(c"/bin/sleep", &["sleep", "60"], slave.as_fd())
        });
        let Started::Running(pid) = started else {
            panic!("copy={copy}: a start on a live pty runs: {started:?}");
        };
        let fields = stat_fields(pid);
        let (group, session, tty) = (&fields[2], &fields[3], &fields[4]);
        // SAFETY: `stat` is plain data the call fills.
        let rdev = unsafe {
            let mut st: libc::stat = std::mem::zeroed();
            assert_eq!(libc::fstat(slave.as_raw_fd(), &mut st), 0);
            st.st_rdev
        };
        let tty: u64 = tty.parse().expect("tty_nr");
        let (major, minor) = ((tty >> 8) & 0xfff, (tty & 0xff) | ((tty >> 12) & 0xfff00));
        let seen = (
            group.parse::<i32>().ok(),
            session.parse::<i32>().ok(),
            (major, minor),
            (0..3)
                .map(|fd| {
                    std::fs::read_link(format!("/proc/{pid}/fd/{fd}"))
                        .map(|path| path.to_string_lossy().into_owned())
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>(),
        );
        end(pid);
        assert_eq!(
            seen,
            (
                Some(pid),
                Some(pid),
                (u64::from(libc::major(rdev)), u64::from(libc::minor(rdev))),
                vec![name.clone(), name.clone(), name.clone()]
            ),
            "copy={copy}: the child's group and session are its own, its controlling terminal \
             is the slave, and so are its 0, 1 and 2"
        );
    }
}

/// The forked shell's signals: every signal below 32 back to its default though this process
/// ignores it, and one above 31 this process ignores still ignored.
#[test]
fn signals_below_32_go_back_to_their_default_and_an_ignored_one_above_stays_ignored() {
    const LOW: libc::c_int = libc::SIGURG;
    const HIGH: libc::c_int = 40;
    let ignore = |signal: libc::c_int, how: libc::sighandler_t| {
        // SAFETY: an action set for a signal whose default (SIGURG: ignored) or absence of a
        // sender (signal 40) no other test relies on, restored below.
        unsafe { libc::signal(signal, how) }
    };
    let (low_was, high_was) = (ignore(LOW, libc::SIG_IGN), ignore(HIGH, libc::SIG_IGN));
    let (_master, slave, _) = pty();
    let started = run(c"/bin/sleep", &["sleep", "60"], slave.as_fd());
    ignore(LOW, low_was);
    ignore(HIGH, high_was);
    let Started::Running(pid) = started else {
        panic!("a start on a live pty runs: {started:?}");
    };
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).expect("status");
    end(pid);
    let ignored = status
        .lines()
        .find_map(|line| line.strip_prefix("SigIgn:"))
        .and_then(|mask| u64::from_str_radix(mask.trim(), 16).ok())
        .expect("SigIgn");
    let bit = |signal: libc::c_int| ignored & (1 << (signal - 1)) != 0;
    assert_eq!(
        (bit(LOW), bit(HIGH)),
        (false, true),
        "SIGURG is back to its default, signal 40 still ignored (SigIgn {ignored:#x})"
    );
}

/// Each step that stops a child is named with the errno it failed with, in a child that shares
/// this process's memory and in a copy.
#[test]
fn each_step_that_stops_a_child_is_named_with_its_errno() {
    let mut hard = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: a live out-param.
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut hard) },
        0
    );
    let null = std::fs::File::open("/dev/null").expect("/dev/null");
    for copy in [false, true] {
        let (_master, slave, _) = pty();
        let (not_a_tty, _write) = crate::pipe().expect("a pipe");
        let cases: [(&str, Seam, Try<'_>, Step, i32); 7] = [
            (
                "a pipe for a terminal",
                Seam::None,
                Box::new(|| run(c"/bin/true", &["true"], not_a_tty.as_fd())),
                Step::Terminal,
                libc::ENOTTY,
            ),
            (
                "a terminal gone once taken",
                Seam::DeadAfterTerminal,
                Box::new(|| run(c"/bin/true", &["true"], slave.as_fd())),
                Step::Live,
                libc::EBADF,
            ),
            (
                "a soft limit above its hard one",
                Seam::None,
                Box::new(|| {
                    let limit = Limit {
                        #[allow(
                            clippy::unnecessary_cast,
                            reason = "`c_uint` or `c_int` by target"
                        )]
                        resource: libc::RLIMIT_NOFILE as u32,
                        soft: hard.rlim_max.saturating_add(1),
                        hard: hard.rlim_max,
                    };
                    run_with(c"/bin/true", &["true"], slave.as_fd(), None, &[limit], None)
                }),
                Step::Limits,
                libc::EINVAL,
            ),
            (
                "a missing working directory",
                Seam::None,
                Box::new(|| {
                    let cwd = Some(c"/nonexistent/aterm-terminal-test");
                    run_with(c"/bin/true", &["true"], slave.as_fd(), cwd, &[], None)
                }),
                Step::Folder,
                libc::ENOENT,
            ),
            (
                "a pass past the descriptor limit",
                Seam::None,
                Box::new(|| {
                    let pass = PassAt::new(null.as_fd(), libc::c_int::MAX).expect("a pass");
                    run_with(
                        c"/bin/true",
                        &["true"],
                        slave.as_fd(),
                        None,
                        &[],
                        Some(pass),
                    )
                }),
                Step::Pass,
                libc::EBADF,
            ),
            (
                "a missing program",
                Seam::None,
                Box::new(|| run(c"/nonexistent/aterm-terminal-test", &["x"], slave.as_fd())),
                Step::Exec,
                libc::ENOENT,
            ),
            (
                "a bare name, which is no search",
                Seam::None,
                Box::new(|| run_with(c"true", &["true"], slave.as_fd(), Some(c"/"), &[], None)),
                Step::Exec,
                libc::ENOENT,
            ),
        ];
        for (what, seam, case, step, errno) in &cases {
            let started = with_child(copy, *seam, case);
            assert!(
                matches!(
                    &started,
                    Started::Stopped { step: s, error } if s == step
                        && error.raw_os_error() == Some(*errno)
                ),
                "copy={copy}, {what}: stopped at {step:?} with errno {errno}, not {started:?}"
            );
            // The next case's child takes the same slave, which this one's session must let go.
            assert!(
                gone(last_pid()),
                "copy={copy}, {what}: the stopped child is reaped"
            );
        }
    }
}

/// The report's write end never sits at a pass's number, which the pass's `dup3` would close:
/// put there first, it moves, and a failed exec is still told as one, while the pass reaches the
/// program.
#[test]
fn the_report_end_moves_off_a_pass_number_so_a_failed_exec_is_told() {
    const AT: libc::c_int = 900;
    let null = std::fs::File::open("/dev/null").expect("/dev/null");
    let pass = || PassAt::new(null.as_fd(), AT).expect("a pass");
    let (master, slave, _) = pty();
    let failed = with_report(Some(AT), false, || {
        run_with(
            c"/nonexistent/aterm-terminal-test",
            &["x"],
            slave.as_fd(),
            None,
            &[],
            Some(pass()),
        )
    });
    assert!(
        matches!(&failed, Started::Stopped { step: Step::Exec, error }
            if error.raw_os_error() == Some(libc::ENOENT)),
        "a failed exec with the report's end first at the pass's number is told: {failed:?}"
    );
    let script = format!("stat -L -c %F /proc/self/fd/{AT}");
    let ran = with_report(Some(AT), false, || {
        run_with(
            c"/bin/sh",
            &["sh", "-c", &script],
            slave.as_fd(),
            None,
            &[],
            Some(pass()),
        )
    });
    let Started::Running(pid) = ran else {
        panic!("a start with a pass runs: {ran:?}");
    };
    drop(slave);
    let mut said = Vec::new();
    let mut chunk = [0_u8; 256];
    loop {
        // SAFETY: a live, writable buffer; the master returns EIO once no slave is open.
        let n = unsafe { libc::read(master.as_raw_fd(), chunk.as_mut_ptr().cast(), chunk.len()) };
        let Ok(n @ 1..) = usize::try_from(n) else {
            break;
        };
        said.extend_from_slice(&chunk[..n]);
    }
    let mut status = 0;
    // SAFETY: our own child; `status` is a live local.
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    assert_eq!(
        String::from_utf8_lossy(&said).trim(),
        "character special file",
        "the program holds /dev/null at {AT}"
    );
}

/// A child that never reaches its exec is killed at the bound and reaped.
#[test]
fn a_child_that_never_execs_is_killed_at_the_bound() {
    let bound = Duration::from_millis(300);
    for copy in [false, true] {
        let (_master, slave, _) = pty();
        let began = Instant::now();
        let started = with_child(copy, Seam::Stall, || {
            start(
                &Start {
                    program: c"/bin/true",
                    argv: &strings(&["true"]),
                    envp: &[],
                    cwd: None,
                    limits: &[],
                    pass: None,
                },
                slave.as_fd(),
                bound,
            )
            .expect("a start")
        });
        let took = began.elapsed();
        let pid = last_pid();
        let reaped = gone(pid);
        assert!(
            matches!(started, Started::Hung) && took >= bound && reaped,
            "copy={copy}: a stalled child is killed at the bound and reaped ({started:?}, \
             {took:?}, pid {pid} reaped: {reaped})"
        );
    }
}

/// Another holder of the report's write end (as a child another thread made meanwhile holds it
/// until its own exec) delays nothing: the start ends at the child's exec.
#[test]
fn another_holder_of_the_report_end_delays_no_start() {
    const NAME: &str = "spawnfd::terminal::tests::another_holder_of_the_report_end_delays_no_start";
    if !native_shared_child_applicable(NAME) {
        return;
    }
    // A copy's fence is its report's end, which the other holder keeps open.
    sharing_or_could_not_run();
    let (_master, slave, _) = pty();
    let started = with_report(None, true, || {
        run(c"/bin/sleep", &["sleep", "60"], slave.as_fd())
    });
    let Started::Running(pid) = started else {
        panic!("a start whose report end another holds runs: {started:?}");
    };
    end(pid);
}
