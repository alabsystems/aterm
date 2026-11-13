// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test support (Unix; test and debug builds, never release ones): whether the kernel reports a child of this process stopped, read once or polled under the caller's bound, and a spinning child stopped for a CPU reader to read.

use std::time::{Duration, Instant};

use crate::spawnfd::{Child, Command};

/// What the kernel reported instead of a stop of the child [`stop_state`] or [`await_stopped`] read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReport {
    /// The child ended: `code` is `CLD_EXITED`, `CLD_KILLED` or `CLD_DUMPED`, and `status` its exit status or the signal that ended it. It can never stop now.
    Exited {
        /// The report's `si_code`.
        code: i32,
        /// The report's `si_status`.
        status: i32,
    },
    /// The child was stopped and has been continued since (`CLD_CONTINUED`).
    Continued,
    /// The kernel reports no stop, end or continue: the child runs on (a stop the kernel discarded mid-exec), or another wait without `WNOWAIT` took its stop report. `state` is XNU's `p_stat` on Apple (`SSTOP` 4: still stopped, its report taken; `SRUN` 2: not stopped), and none where it is not read or where the read fails.
    Neither {
        /// `pbsi_status` from `proc_pidinfo` on Apple.
        state: Option<u32>,
    },
}

/// One read of whether the kernel reports `pid`, an unreaped child of this process, stopped: `Ok` when it does, else what it reports instead. The read is one `waitid(WEXITED | WSTOPPED | WCONTINUED)`, which reports an end, and of a child that has not ended a stop before a continue. One read with `WEXITED` in it, never a stop read first: Linux refuses a read without `WEXITED` of a child that has ended (`ECHILD`, as for no child at all; XNU reports nothing), so a stop read would fail on a child that ended before it or between two reads. The read adds `WNOHANG | WNOWAIT`, so it neither blocks nor consumes a report: the child stays waitable, and the caller's reap still collects its status and rusage.
///
/// # Errors
/// What the kernel reported instead of a stop: [`StopReport`].
///
/// # Panics
/// `pid` is not positive, or `waitid` refuses it: it is not an unreaped child of this process.
pub fn stop_state(pid: i32) -> Result<(), StopReport> {
    let id = libc::id_t::try_from(pid).expect("a positive pid");
    let report = reported(id, libc::WEXITED | libc::WSTOPPED | libc::WCONTINUED);
    match report.si_code {
        libc::CLD_STOPPED => Ok(()),
        libc::CLD_CONTINUED => Err(StopReport::Continued),
        code @ (libc::CLD_EXITED | libc::CLD_KILLED | libc::CLD_DUMPED) => {
            // SAFETY: a `waitid` report of an ended child fills the `SIGCHLD` arm, `si_status` included.
            let status = unsafe { report.si_status() };
            Err(StopReport::Exited { code, status })
        }
        _ => Err(StopReport::Neither { state: state(pid) }),
    }
}

/// Polls [`stop_state`] every 2 ms until the kernel reports `pid` stopped. A poll, not a blocking `WUNTRACED` wait: a stop the kernel discarded leaves a child that runs on, which that wait would wait on forever (macOS discards a `SIGSTOP` that lands while the child is inside an exec). `within` is the caller's hang detector (`aterm_spec::wait::MUST_HAPPEN` in a test; this crate cannot depend on aterm-spec).
///
/// # Errors
/// What the kernel last reported: once `within` has passed, or at once when the child has ended ([`StopReport::Exited`]), since an ended child cannot stop.
///
/// # Panics
/// As [`stop_state`], and when `within` reaches past what `Instant` can hold.
pub fn await_stopped(pid: i32, within: Duration) -> Result<(), StopReport> {
    let by = Instant::now() + within;
    loop {
        let report = match stop_state(pid) {
            Ok(()) => return Ok(()),
            Err(report) => report,
        };
        if matches!(report, StopReport::Exited { .. }) || Instant::now() >= by {
            return Err(report);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The loop [`spin_stopped`]'s bash runs: builtins only, so it never forks and all its CPU is its own.
const SPIN: &str = "while :; do :; done";

/// The child [`spin_stopped`] spins: `/bin/bash -c` [`SPIN`], with an empty environment.
///
/// Bash itself, not `/bin/sh`: `/bin/sh` is a trampoline that execs bash after `spawn` returns, and macOS discards a `SIGSTOP` that lands mid-exec (3d899ede48). Bash runs the builtin loop in-process, so its image is final when `spawn` returns. An absolute path running builtins needs nothing from the environment, and bash takes code from it: the file `BASH_ENV` names, the options `SHELLOPTS` lists (under `noexec` bash reads the loop, never runs it and exits 0; under `xtrace` it writes each `:` to stderr) and each function a `BASH_FUNC_<name>%%` exports (`BASH_FUNC_:%%` replaces the loop's `:`). Any of them could end the loop, exec over it, or spend CPU in children that the reap's rusage counts and the pid's own counters do not.
fn spinner() -> Command {
    let mut spinner = crate::spawnfd::command("/bin/bash");
    spinner.args(["-c", SPIN]).env_clear();
    spinner
}

/// Spins a `/bin/bash` child of this process ([`spinner`]) until `cpu_ns(pid)` reads at least `min_ns`, stops it, hands its pid to `read` once the kernel reports it stopped, then kills it and reaps it. Answers `read`'s value and the CPU time, in nanoseconds, that the reap's `wait4` rusage counts for the child: a stopped child spends no more CPU, so `read` and the reap count the same CPU at any load. `within` is the caller's hang detector, for the reader and again for the stop (`aterm_spec::wait::MUST_HAPPEN` in a test; this crate cannot depend on aterm-spec).
///
/// # Panics
/// The spinner cannot be spawned, stopped, killed or reaped; it ends before `cpu_ns` reads `min_ns`, or `cpu_ns` does not read `min_ns` within `within` (each message names `min_ns` and the last reading); it does not stop within `within` ([`await_stopped`]'s report); something other than the kill ends it; its CPU time does not fit `u64` nanoseconds or reads zero; or `within` reaches past what `Instant` can hold. The spinner is killed and reaped on every panic, one in `cpu_ns` or `read` included.
pub fn spin_stopped<T>(
    min_ns: u64,
    cpu_ns: impl FnMut(u32) -> Option<u64>,
    read: impl FnOnce(u32) -> T,
    within: Duration,
) -> (T, u64) {
    spin(spinner(), min_ns, cpu_ns, read, within)
}

/// [`spin_stopped`] of the child `spinner` starts.
fn spin<T>(
    mut spinner: Command,
    min_ns: u64,
    mut cpu_ns: impl FnMut(u32) -> Option<u64>,
    read: impl FnOnce(u32) -> T,
    within: Duration,
) -> (T, u64) {
    use std::os::unix::process::ExitStatusExt as _;
    let mut child = Spinner(spinner.spawn().expect("spawn a spinning /bin/bash"));
    let id = child.0.id();
    let pid = i32::try_from(id).expect("a pid");
    let ready_by = Instant::now() + within;
    loop {
        let reading = cpu_ns(id);
        if reading.is_some_and(|ns| ns >= min_ns) {
            break;
        }
        if matches!(child.0.exited(), Ok(true)) {
            let status = child.0.wait().expect("reap the ended spinner");
            panic!(
                "the spinner ended ({status}) before the reader read {min_ns} ns of its CPU \
                 (last read {reading:?})"
            );
        }
        assert!(
            Instant::now() < ready_by,
            "the reader reads {min_ns} ns of the spinner's CPU within {within:?} \
             (last read {reading:?})"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // SAFETY: `kill` takes no pointers; `pid` is this process's unreaped child.
    let stop = unsafe { libc::kill(pid, libc::SIGSTOP) };
    assert_eq!(
        stop,
        0,
        "stop the spinner: {}",
        std::io::Error::last_os_error()
    );
    if let Err(report) = await_stopped(pid, within) {
        panic!("the spinner stops within {within:?}: {report:?}");
    }
    let answer = read(id);
    child.0.kill().expect("kill the stopped spinner");
    let usage = child.0.wait_with_rusage().expect("reap the spinner");
    assert_eq!(
        usage.status.signal(),
        Some(libc::SIGKILL),
        "the kill ended the spinner: {:?}",
        usage.status
    );
    let cpu = u64::try_from(usage.cpu.as_nanos()).expect("CPU time within u64 nanoseconds");
    assert!(cpu > 0, "the reap counts the spinner's CPU");
    (answer, cpu)
}

/// The spinner [`spin`] owns: killed and reaped on drop, so a panic leaves nothing spinning. Once a reap has collected it, [`Child::kill`] signals nothing and [`Child::wait`] answers the status it kept.
struct Spinner(Child);

impl Drop for Spinner {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `waitid(P_PID, id, options | WNOHANG | WNOWAIT)`'s report: zeroed when the kernel has none to give.
fn reported(id: libc::id_t, options: libc::c_int) -> libc::siginfo_t {
    // SAFETY: an all-zero `siginfo_t` is a valid value of a C struct of integers and pointers.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a live out-param of the declared layout; `WNOWAIT` reaps nothing and `WNOHANG` blocks on nothing.
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            id,
            &mut info,
            options | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    assert_eq!(
        rc,
        0,
        "waitid on an unreaped child of this process: {}",
        std::io::Error::last_os_error()
    );
    info
}

/// XNU's `p_stat` of `pid` ([`StopReport::Neither`]).
fn state(pid: i32) -> Option<u32> {
    #[cfg(target_vendor = "apple")]
    {
        crate::process::read_short_info(pid).map(|info| info.pbsi_status)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let _ = pid;
        None
    }
}

#[cfg(test)]
#[path = "stopped_tests.rs"]
mod tests;
