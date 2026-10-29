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
mod tests {
    use std::os::unix::process::ExitStatusExt as _;
    use std::panic::AssertUnwindSafe;
    use std::time::{Duration, Instant};

    use super::{
        SPIN, StopReport, await_stopped, reported, spin, spin_stopped, spinner, stop_state,
    };
    use crate::spawnfd::Child;

    /// A test's one-minute hang detector (aterm-spec's `MUST_HAPPEN`, which this crate cannot name).
    const HANG: Duration = Duration::from_secs(60);

    /// A short bound for a child the test never stops, so its report is the same at any bound.
    const SHORT: Duration = Duration::from_millis(20);

    /// A `/bin/sleep` child, killed and reaped when the test ends, a failed one included.
    struct Sleeper(Child);

    impl Sleeper {
        fn spawn() -> Self {
            Self(
                crate::spawnfd::command("/bin/sleep")
                    .arg("120")
                    .spawn()
                    .expect("spawn a sleep"),
            )
        }

        fn pid(&self) -> i32 {
            i32::try_from(self.0.id()).expect("a pid")
        }

        fn signal(&self, signal: i32) {
            // SAFETY: `kill` takes no pointers; the pid is this test's unreaped child.
            let sent = unsafe { libc::kill(self.pid(), signal) };
            assert_eq!(sent, 0, "signal {signal} to our own child");
        }
    }

    impl Drop for Sleeper {
        fn drop(&mut self) {
            if self.0.kill().is_ok() {
                let _ = self.0.wait();
            }
        }
    }

    /// A stopped child reads stopped, again on a second read (`WNOWAIT` took no report), and is still this process's to kill and reap.
    #[test]
    fn a_stopped_child_reads_stopped_and_stays_waitable() {
        let mut child = Sleeper::spawn();
        child.signal(libc::SIGSTOP);
        assert_eq!(await_stopped(child.pid(), HANG), Ok(()));
        assert_eq!(stop_state(child.pid()), Ok(()), "a read takes no report");
        child.0.kill().expect("kill our own stopped child");
        let status = child.0.wait().expect("reap our own child");
        assert_eq!(status.signal(), Some(libc::SIGKILL), "{status:?}");
    }

    /// NEGATIVE CONTROL: a child nothing stops reads neither stopped, ended nor continued once the bound the test passes has passed.
    #[test]
    fn a_child_never_stopped_reads_neither_within_its_bound() {
        let child = Sleeper::spawn();
        let report = await_stopped(child.pid(), SHORT);
        #[cfg(target_vendor = "apple")]
        assert!(
            matches!(report, Err(StopReport::Neither { state: Some(state) })
                if state != crate::process::BsdShortInfo::SSTOP),
            "XNU reads a live child's state, and not stopped: {report:?}"
        );
        #[cfg(not(target_vendor = "apple"))]
        assert_eq!(report, Err(StopReport::Neither { state: None }));
    }

    /// NEGATIVE CONTROL: a child stopped, then continued, reads continued, not stopped.
    #[test]
    fn a_continued_child_reads_continued() {
        let child = Sleeper::spawn();
        child.signal(libc::SIGSTOP);
        assert_eq!(await_stopped(child.pid(), HANG), Ok(()));
        child.signal(libc::SIGCONT);
        let by = Instant::now() + HANG;
        loop {
            let report = stop_state(child.pid());
            if report == Err(StopReport::Continued) {
                break;
            }
            assert!(
                Instant::now() < by,
                "the continued report arrives within {HANG:?} (last read {report:?})"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        // The event is established; SHORT bounds only the timeout's last-report assertion.
        assert_eq!(
            await_stopped(child.pid(), SHORT),
            Err(StopReport::Continued)
        );
    }

    /// A child killed before any stop reads ended, with the signal that ended it, once it has ended and before its reap (the read Linux refuses without `WEXITED`), and stays waitable.
    #[test]
    fn a_killed_child_reads_ended() {
        let mut child = Sleeper::spawn();
        child.signal(libc::SIGKILL);
        let id = libc::id_t::try_from(child.pid()).expect("a positive pid");
        let by = Instant::now() + HANG;
        while reported(id, libc::WEXITED).si_code == 0 {
            assert!(
                Instant::now() < by,
                "the kill ends our own child within {HANG:?}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            await_stopped(child.pid(), HANG),
            Err(StopReport::Exited {
                code: libc::CLD_KILLED,
                status: libc::SIGKILL
            })
        );
        let status = child.0.wait().expect("reap our own child");
        assert_eq!(status.signal(), Some(libc::SIGKILL), "{status:?}");
    }

    /// A stopped child whose stop report another wait took reads neither; on Apple its state still reads `SSTOP`, which tells it apart from a child that runs.
    #[test]
    fn a_stop_whose_report_was_taken_reads_neither() {
        let child = Sleeper::spawn();
        child.signal(libc::SIGSTOP);
        assert_eq!(await_stopped(child.pid(), HANG), Ok(()));
        let mut status = 0;
        // SAFETY: `status` is a live out-param; the pid is this test's child, and `WUNTRACED` without `WNOWAIT` takes its stop report and reaps nothing.
        let taken =
            unsafe { libc::waitpid(child.pid(), &mut status, libc::WUNTRACED | libc::WNOHANG) };
        assert_eq!(taken, child.pid(), "the stop report is ours to take");
        assert!(libc::WIFSTOPPED(status), "{status:#x}");
        #[cfg(target_vendor = "apple")]
        let state = Some(crate::process::BsdShortInfo::SSTOP);
        #[cfg(not(target_vendor = "apple"))]
        let state = None;
        assert_eq!(stop_state(child.pid()), Err(StopReport::Neither { state }));
    }

    /// The CPU the spinner tests wait for before the stop: both callers' 50 ms.
    const SPUN_NS: u64 = 50_000_000;

    /// `pid`'s user plus system CPU time in nanoseconds, its own and not its children's: `proc_pid_rusage`'s Mach ticks scaled by the kernel's timebase ([`aterm_sysprobe::procs::kernel_timebase`]: `hw.tbfrequency` reduced, or this process's `mach_timebase_info` when that sysctl answers nothing). Not this process's timebase first: under Rosetta 2 it reads 1/1 while the kernel still counts 24 MHz ticks, so a reader scaled by it reads 3/125 of the CPU (docs/measured/rosetta-mac-2026-10-20.md §3 (b)).
    #[cfg(target_os = "macos")]
    fn cpu_ns(pid: u32) -> Option<u64> {
        let pid = i32::try_from(pid).ok()?;
        let (numer, denom) = aterm_sysprobe::procs::kernel_timebase()?;
        // SAFETY: `rusage_info_v4` is plain integers and bytes; all-zero is valid.
        let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a writable `rusage_info_v4`, the layout this flavor fills, and outlives the call.
        let rc = unsafe {
            libc::proc_pid_rusage(
                pid,
                libc::RUSAGE_INFO_V4,
                (&raw mut info).cast::<libc::rusage_info_t>(),
            )
        };
        if rc != 0 {
            return None;
        }
        let ticks = info.ri_user_time.checked_add(info.ri_system_time)?;
        Some(aterm_sysprobe::procs::ticks_to_ns(ticks, numer, denom))
    }

    /// See the macOS twin: `/proc/<pid>/stat`'s `utime` and `stime`, in clock ticks.
    #[cfg(not(target_os = "macos"))]
    fn cpu_ns(pid: u32) -> Option<u64> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // After the command name, the state is the first field and `utime` the twelfth.
        let mut fields = stat.get(stat.rfind(')')? + 1..)?.split_ascii_whitespace();
        let utime: u64 = fields.nth(11)?.parse().ok()?;
        let stime: u64 = fields.next()?.parse().ok()?;
        // SAFETY: `sysconf` takes an integer and touches nothing of ours.
        let hz = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).ok()?;
        (utime + stime).checked_mul(1_000_000_000_u64.checked_div(hz)?)
    }

    /// Whether `read` is within 2x of `kernel`, either way.
    fn within_2x(read: u64, kernel: u64) -> bool {
        read <= kernel.saturating_mul(2) && read.saturating_mul(2) >= kernel
    }

    /// Fails unless `pid` names no child of this process: the reap, or the guard, collected the spinner.
    fn assert_reaped(pid: u32) {
        let pid = i32::try_from(pid).expect("a pid");
        let mut status = 0;
        // SAFETY: `status` is a live out-param; `WNOHANG` blocks on nothing.
        let got = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        // Read at once, and named only for a failed call: one that read a pid or 0 sets no errno, so an earlier call's is still there.
        let error = std::io::Error::last_os_error();
        assert_eq!(
            got, -1,
            "the spinner (pid {pid}) is no child of ours now: waitpid read {got} (status {status:#x})"
        );
        assert_eq!(
            error.raw_os_error(),
            Some(libc::ECHILD),
            "the spinner (pid {pid}) is no child of ours now: waitpid failed with {error}"
        );
    }

    /// The first 4 KiB of `text`, for a failure message.
    fn head(text: &str) -> &str {
        let mut end = text.len().min(4096);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[..end]
    }

    /// The spinner's CPU reads at least the threshold while the kernel reports it stopped, within 2x of what the reap's rusage counts for it, and the reap collects it.
    #[test]
    fn a_spun_child_reads_its_cpu_while_stopped_as_the_reap_counts_it() {
        let ((pid, read), kernel) = spin_stopped(
            SPUN_NS,
            cpu_ns,
            |pid| {
                let stopped = stop_state(i32::try_from(pid).expect("a pid"));
                assert_eq!(stopped, Ok(()), "read while stopped");
                (pid, cpu_ns(pid))
            },
            HANG,
        );
        let read = read.expect("the reader reads its stopped child");
        assert!(read >= SPUN_NS, "read {read} ns");
        assert!(
            within_2x(read, kernel),
            "read {read} ns; the reap's rusage for the same stopped child is {kernel} ns"
        );
        assert_reaped(pid);
    }

    /// NEGATIVE CONTROL: a reader that never reads the threshold fails by name, with the threshold and its last reading, once the bound the test passes has passed, and the guard kills and reaps the spinner.
    #[test]
    fn a_reader_short_of_its_threshold_fails_by_name_with_its_reading() {
        let asked = std::cell::Cell::new(None);
        let failed = std::panic::catch_unwind(AssertUnwindSafe(|| {
            spin_stopped(
                SPUN_NS,
                |pid| {
                    asked.set(Some(pid));
                    Some(7)
                },
                |_| (),
                SHORT,
            )
        }));
        let payload = failed.expect_err("a reader stuck at 7 ns fails");
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied());
        assert_eq!(
            message,
            Some(
                format!(
                    "the reader reads {SPUN_NS} ns of the spinner's CPU within {SHORT:?} \
                     (last read Some(7))"
                )
                .as_str()
            )
        );
        assert_reaped(asked.get().expect("the reader was asked"));
    }

    /// The variable [`a_spinner_in_a_hostile_environment_stand_in`] reads: which spinner it runs.
    const STAND_IN: &str = "ATERM_UDS_SPIN_STAND_IN";

    /// The variable [`stand_in`] names the test binary's pid in: the stand-in's parent.
    const PARENT: &str = "ATERM_UDS_SPIN_PARENT";

    /// The line the stand-in writes once its spin is done.
    const SPUN: &str = "the stand-in spun";

    /// `cpu_ns`, for a stand-in: at its next reading once the test binary that ran the stand-in (the pid [`PARENT`] names) is no longer its parent, it panics, so [`spin`]'s guard kills and reaps the spinner and the stand-in ends. The stand-in's group is not the test binary's ([`stand_in`]), so a signal to that group does not reach it, and without this an orphaned stand-in whose read hangs would keep its spinner spinning until its own `HANG` ends the read. It checks only when the reader reads, while the spinner spins. A parent that dies once the spinner is stopped orphans the stand-in's group with a stopped member in it (the new parent, launchd or init, is outside the group's session), so the kernel sends every member `SIGHUP` and then `SIGCONT`, and both end at once. Where `SIGHUP` is ignored (a `nohup`'d runner), the `SIGCONT` resumes the spinner, and the stand-in ends it by its kill once it has seen the stop, or else by its own `HANG` bound on the stop: that bound is the upper bound. The parent's pid is passed in rather than read with `getppid` at the start, so a parent that died before the stand-in started counts too; and the reading is compared against that pid rather than tested for 1, so an adoption by a subreaper (Linux), whose pid is not 1, counts too.
    ///
    /// # Panics
    /// [`PARENT`] names no pid (when this is made), or the pid it names is no longer the stand-in's parent (when it reads): the parent died, or a debugger attached to the stand-in (an XNU `ptrace` attach reparents it).
    fn while_parented(
        mut cpu_ns: impl FnMut(u32) -> Option<u64>,
    ) -> impl FnMut(u32) -> Option<u64> {
        let parent: i32 = std::env::var(PARENT)
            .ok()
            .and_then(|pid| pid.parse().ok())
            .expect("the stand-in's parent pid");
        move |pid| {
            // SAFETY: `getppid` takes nothing and cannot fail.
            let now = unsafe { libc::getppid() };
            assert_eq!(
                now, parent,
                "the test binary that ran this stand-in (pid {parent}) is no longer its parent: \
                 it died, or a debugger attached"
            );
            cpu_ns(pid)
        }
    }

    /// Not a check on its own (it returns at once without [`STAND_IN`]): the child [`a_hostile_environment_never_reaches_the_spinner`] runs in the environment it hands it. It spins [`spinner`] (`seam`) or the same loop through a command that inherits this environment (`inherit`), then writes [`SPUN`]. Its reader stops the spin once its parent has died ([`while_parented`]).
    #[test]
    fn a_spinner_in_a_hostile_environment_stand_in() {
        let spinner = match std::env::var(STAND_IN).as_deref() {
            Ok("seam") => spinner(),
            Ok("inherit") => {
                let mut inherits = crate::spawnfd::command("/bin/bash");
                inherits.args(["-c", SPIN]);
                inherits
            }
            _ => return,
        };
        spin(spinner, SPUN_NS, while_parented(cpu_ns), |_| (), HANG);
        println!("{SPUN}");
    }

    /// Runs this test binary as the stand-in `mode` with `key=value` and [`PARENT`] added to its environment, its stdout and stderr in one scratch file. Answers whether it succeeded and what it wrote. The stand-in runs in a process group of its own, which its spinner joins, so a hung stand-in is killed with its whole group, spinner included, without signalling the test binary's own group, which holds the test binary itself; it is then reaped. The cost is that a signal to the test binary's group (a terminal's ^C, a runner's group kill) no longer reaches the stand-in, which is why the stand-in ends itself once its parent has died ([`while_parented`]). A stand-in that has not ended within `3 * HANG` is the hung one: its own spin is bounded by `HANG` for the read and `HANG` again for the stop, and the third covers its start and its end, so a stand-in whose spin hangs fails by its own message first.
    fn stand_in(mode: &str, key: &str, value: &str) -> (bool, String) {
        let scratch = aterm_tempfile::TempDir::new().expect("a scratch folder");
        let path = scratch.path().join("out");
        let out = std::fs::File::create(&path).expect("the stand-in's output file");
        let err = out.try_clone().expect("the same file for its stderr");
        let mut child = crate::spawnfd::command(std::env::current_exe().expect("the test binary"))
            .args([
                "--exact",
                "deadline::stopped::tests::a_spinner_in_a_hostile_environment_stand_in",
                "--nocapture",
                "--quiet",
                "--test-threads=1",
            ])
            .env(STAND_IN, mode)
            .env(key, value)
            .env(PARENT, std::process::id().to_string())
            .process_group(0)
            .stdin(crate::spawnfd::Stdio::null())
            .stdout(out)
            .stderr(err)
            .spawn()
            .expect("re-run this test binary as the stand-in");
        // A hang detector: past the stand-in's own bounds, `HANG` for the read and `HANG` for the stop.
        let bound = 3 * HANG;
        let by = Instant::now() + bound;
        while !child.exited().expect("read whether the stand-in ended") {
            if Instant::now() >= by {
                crate::deadline::children::kill_group(&mut child);
                let _ = child.wait();
                panic!("the {mode} stand-in under {key}={value} ends within {bound:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let status = child.wait().expect("reap the stand-in");
        let wrote = std::fs::read_to_string(&path).expect("read what the stand-in wrote");
        (status.success(), wrote)
    }

    /// The spinner takes nothing from this process's environment. The test binary re-runs itself as the stand-in in three environments that a bash which inherits them takes code from: `SHELLOPTS=noexec` (the loop never runs, and bash exits 0), `SHELLOPTS=xtrace` (each `:` is written to stderr) and an exported function `:` (`BASH_FUNC_:%%`, which here prints and ends the loop). In each, the seam's spinner spins, the stand-in's output lacks that environment's tell, and the spinner ends only by the kill. NEGATIVE CONTROL: the same loop through a command that inherits the environment shows each one's tell.
    #[test]
    fn a_hostile_environment_never_reaches_the_spinner() {
        for (key, value, tell) in [
            ("SHELLOPTS", "noexec", "the spinner ended (exit status: 0)"),
            ("SHELLOPTS", "xtrace", "+ :"),
            ("BASH_FUNC_:%%", "() { echo hijacked; exit 3; }", "hijacked"),
        ] {
            let (ok, wrote) = stand_in("seam", key, value);
            assert!(
                ok && wrote.contains(SPUN) && !wrote.contains(tell),
                "{key}={value} reaches the seam's spinner (succeeded: {ok}): {}",
                head(&wrote)
            );
            let (_, wrote) = stand_in("inherit", key, value);
            assert!(
                wrote.contains(tell),
                "{key}={value} shows no {tell:?} through an inheriting bash: {}",
                head(&wrote)
            );
        }
    }
}
