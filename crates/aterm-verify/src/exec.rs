// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Running the stages' children.
//!
//! A [`Cmd`] is one invocation as a value, so a test can assert the exact argv
//! of a stage (tippy's, say) without a Trust toolchain installed.
//!
//! Output is captured to one file shared by stdout and stderr (via
//! `File::try_clone`, so the streams interleave in the order the child wrote
//! them, like the shell's `2>&1`). Capturing lets independent stages run
//! concurrently while the ladder prints in declared order.
//!
//! Every child runs under a wall-clock ceiling ([`DEFAULT_CHILD_CEILING`]) so a
//! hang ends the run red instead of silent.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Where the child's output goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Capture {
    /// Into the stage report, verbatim, above the ladder line it explains —
    /// where the script's inline `$(…)`-free invocations put it.
    Emit,
    /// `>/dev/null 2>&1`.
    Silent,
    /// `>>file 2>&1`: the smokes keep a child log and print the tail of it only
    /// when something failed.
    Append(PathBuf),
}

/// One child invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cmd {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// Extra environment, applied on top of the inherited one — exactly the
    /// `env VAR=… cmd` prefixes the script used.
    pub envs: Vec<(OsString, OsString)>,
    pub capture: Capture,
    /// Spawn under utility QoS rather than the gate's inherited tier — see
    /// [`Cmd::demoted`] for which children may, and why the rest may not.
    pub demoted: bool,
    /// The directory the child starts in, in place of [`ExecEnv::cwd`] — a
    /// test binary replayed from cargo's own invocation starts where cargo
    /// started it, in its package's root ([`Cmd::replayed`]).
    pub cwd: Option<PathBuf>,
    /// [`Cmd::envs`] is the child's whole environment: nothing is inherited, and
    /// [`ExecEnv`]'s PATH, removals and additions are not applied, because the
    /// environment was recorded from a child that already had them
    /// ([`Cmd::replayed`]), or is a scratch one that must carry nothing of the
    /// gate's ([`crate::first_exec`]).
    pub exact_env: bool,
    /// Text written at the head of the child's captured log before it starts
    /// ([`Capture::Emit`] only), so a ceiling kill's diagnostic, which reads that
    /// log, sees it too: cargo's `Running <target> (<path>)` header for a test
    /// binary the gate runs itself, from which the TIMEOUT block names the
    /// binary and the test still running ([`crate::libtest::note`]).
    pub preamble: Option<String>,
    /// When the gate must end this child early — its ceiling fired, or the
    /// gate itself was interrupted — send its group `SIGTERM` first and give it
    /// this long to exit before the `SIGKILL`. `None` (the default): `SIGKILL`
    /// at once. See [`Cmd::term_grace`] for which children ask for it.
    pub term_grace: Option<Duration>,
}

impl Cmd {
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            envs: Vec::new(),
            capture: Capture::Emit,
            demoted: false,
            cwd: None,
            exact_env: false,
            preamble: None,
            term_grace: None,
        }
    }

    /// A child replayed from a recorded invocation: `argv[0]` with the rest of
    /// `argv`, started in `cwd` with exactly `env` and nothing inherited — the
    /// process cargo itself would have started ([`crate::testrun`]).
    #[must_use]
    pub fn replayed(
        argv: &[OsString],
        cwd: impl Into<PathBuf>,
        env: Vec<(OsString, OsString)>,
    ) -> Self {
        let mut c = Self::new(argv.first().cloned().unwrap_or_default());
        c.args = argv.iter().skip(1).cloned().collect();
        c.envs = env;
        c.exact_env = true;
        c.cwd = Some(cwd.into());
        c
    }

    /// Write `text` at the head of the captured log before the child starts
    /// ([`Cmd::preamble`]).
    #[must_use]
    pub fn preamble(mut self, text: impl Into<String>) -> Self {
        self.preamble = Some(text.into());
        self
    }

    #[must_use]
    pub fn arg(mut self, a: impl Into<OsString>) -> Self {
        self.args.push(a.into());
        self
    }

    #[must_use]
    pub fn args<I, S>(mut self, it: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(it.into_iter().map(Into::into));
        self
    }

    #[must_use]
    pub fn env(mut self, k: impl Into<OsString>, v: impl Into<OsString>) -> Self {
        self.envs.push((k.into(), v.into()));
        self
    }

    #[must_use]
    pub fn capture(mut self, c: Capture) -> Self {
        self.capture = c;
        self
    }

    /// Run this child at utility QoS (`taskpolicy -c utility`, where macOS has
    /// it), so a compile yields the CPU to the programs a person is typing into.
    /// Utility, not background (`-b`): background also throttles disk I/O hard.
    ///
    /// WHY (2026-09-15). Every stage child used to start at the default band,
    /// the band the user's shell and its programs run in: a `trustc` and the
    /// Codex a person was typing into both read `pri 31` in `ps`. While ~45
    /// runnable threads shared 18 cores, aterm's own share of a keystroke stayed
    /// short (`key_write_p99_ms=6.29`) while the child's share, write to first
    /// byte back, stretched to `echo_p95_ms=75.69 echo_p99_ms=150.04
    /// echo_max_ms=367.85`. The program has to be scheduled to read the key,
    /// update and repaint, and the compiles competed with it as equals. Measured:
    /// under `taskpolicy -c utility` a child reads `pri 20`, under the
    /// inherited tier `31`.
    ///
    /// Utility, not background (`-b`, `pri 4`): background also throttles disk
    /// I/O hard, and a cold workspace compile is already the child that sets
    /// [`DEFAULT_CHILD_CEILING`].
    ///
    /// ONLY FOR A CHILD THAT JUST COMPILES. A QoS clamp is inherited by
    /// everything the child starts, and docs/RELEASING.md measured what that
    /// does to a child the gate MEASURES: an aterm launched under UTILITY had
    /// its paint probe's 50 ms timer fire 75 ms late, and the smoke went red 11
    /// times in 30. So the test run, the measuring tests (the paint and spin
    /// guards), the smokes, the drives and every other child that RUNS code keep
    /// the inherited tier. That is why this is opt-in per child, not a default.
    ///
    /// No gate compile is a release's critical path: the release builds the bytes
    /// MEASURE judges itself, once (`aterm-release`'s build step), and hands the
    /// MEASURE run that artifact, so every compile a gate run makes yields to the
    /// desktop.
    #[must_use]
    pub fn demoted(mut self) -> Self {
        self.demoted = true;
        self
    }

    /// End this child with `SIGTERM` and a `grace` before the `SIGKILL`, so a
    /// shell suite's `trap … EXIT` runs. Every stage child leads its own process
    /// group, and both ways the gate ends one early (the ceiling,
    /// [`kill_and_reap`]; an interrupted gate, [`group::kill_on_interrupt`])
    /// signal that group; bash runs its EXIT trap on `SIGTERM` but never on
    /// `SIGKILL`. Suites clean up scratch keychains, private instances' temp
    /// trees and daemons that left the group only in that trap.
    ///
    /// Opt-in, like [`Cmd::demoted`]: a compile has no trap, so the default is
    /// the immediate `SIGKILL`. The grace is a bound, not a wait: the gate moves
    /// on once the group has emptied, and a child still alive at its end is
    /// `SIGKILL`ed. An interrupted gate waits at most [`TERM_GRACE`], whatever a
    /// child asked for.
    #[must_use]
    pub fn term_grace(mut self, grace: Duration) -> Self {
        self.term_grace = Some(grace);
        self
    }

    /// The argv as printable strings — for tests and for ladder labels.
    #[must_use]
    pub fn argv(&self) -> Vec<String> {
        std::iter::once(self.program.to_string_lossy().into_owned())
            .chain(self.args.iter().map(|a| a.to_string_lossy().into_owned()))
            .collect()
    }
}

/// The invariant part of every invocation: cwd is the repo root (several stages
/// pass root-relative manifest paths), PATH is the stage2-first one.
#[derive(Clone, Copy, Debug)]
pub struct ExecEnv<'a> {
    pub cwd: &'a Path,
    pub path: &'a OsStr,
    pub scratch: &'a Path,
    /// The wall-clock ceiling on one child, or `None` to wait forever. Resolved
    /// once from `--stage-timeout` ([`crate::Ctx::exec_env`]) and carried per
    /// invocation, so a stage's decision is a function of a value a test can
    /// construct.
    pub child_ceiling: Option<Duration>,
    /// Variables removed from every child's inherited environment before its own
    /// [`Cmd::envs`] are applied, so a stage that names a variable still wins.
    /// Every run removes [`crate::CHILD_ENV_REMOVED`] (e.g. `CARGO_TARGET_DIR`,
    /// since the lanes are the run's own directories).
    pub remove_env: &'a [&'a str],
    /// Variables added to every child, after [`Self::remove_env`] and before the
    /// stage's own [`Cmd::envs`], so a stage that names a variable still wins.
    /// Facts the whole run must agree on, resolved once on the main thread
    /// ([`crate::CHILD_ENV`], [`crate::Ctx::with_pinned_child_facts`]): the git
    /// stamp, `RUST_TEST_THREADS`, `TRUST_STAGE2_BIN`, `CARGO_INCREMENTAL=0`.
    pub add_env: &'a [(OsString, OsString)],
}

/// The one-minute load average, in-process: `getloadavg(3)` (libSystem, glibc
/// and musl all have it; macOS answers it from the same `vm.loadavg` sysctl the
/// spawn below prints). Read at each stage's start and end for its `time` line,
/// its receipt `load` line and the verdict's under-load label
/// ([`crate::ladder::StageLoad`]); `None` when neither this nor the spawn
/// answers.
///
/// It is a call, not a child, because the ladder asks twice per planned stage,
/// before it decides whether the stage runs at all: a Fast plan is 22 stages,
/// so a ladder that ran nothing still spawned `sysctl` 44 times (measured:
/// 88 spawns for the two lean runs of one `environment_contract` test), each a
/// fork and an exec on a machine whose load is the very thing being read. A
/// reading that is not a finite non-negative number is as good as no answer.
#[must_use]
pub fn load_average() -> Option<f64> {
    #[cfg(unix)]
    {
        if let Some(load) = getloadavg_one() {
            return Some(load);
        }
    }
    load_average_by_file_or_spawn()
}

/// How many cores were busy since the last reading (or over a quarter second,
/// when there is none from the last ten): the test pool's admission reading
/// ([`crate::Ctx::test_load`], [`crate::testrun::Limits::may_start`]).
///
/// WHY NOT THE LOAD AVERAGE (2026-10-05): the one-minute load is an average
/// that trails a burst by a minute. A warm gate's test run held two binaries
/// at a time while the load read 14-18 on 14 cores, though its binaries spent
/// 1,407 CPU-s in 264 s (5.3 cores) beside one other stage: their walls summed
/// to 691 s, so the pool averaged 2.6 of its 8 and the cores it was protecting
/// sat idle after each burst. Busy time over the last second answers what the
/// pool asks — is a core free now — within a sample.
///
/// macOS: `host_statistics(HOST_CPU_LOAD_INFO)`; Linux: `/proc/stat`'s first
/// line (iowait counts as idle). `None` elsewhere, or when the counters cannot
/// be read, and the caller falls back to [`load_average`].
#[must_use]
pub fn busy_cores() -> Option<f64> {
    static LAST: std::sync::Mutex<Option<(Instant, u64, u64)>> = std::sync::Mutex::new(None);
    let mut last = LAST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (then, busy0, total0) = match *last {
        Some(prev) if prev.0.elapsed() < Duration::from_secs(10) => prev,
        _ => {
            let (busy, total) = cpu_ticks()?;
            std::thread::sleep(Duration::from_millis(250));
            (Instant::now(), busy, total)
        }
    };
    let (busy1, total1) = cpu_ticks()?;
    let (busy, total) = (busy1.wrapping_sub(busy0), total1.wrapping_sub(total0));
    if total == 0 {
        // No tick since the last reading. A caller asking again within one is
        // answered nothing, and macOS hands a process that asks many times a
        // second its cached figures (measured 2026-10-05: five readers at once
        // saw one total for half a second while the counters moved every
        // 2.5 ms), so it is never asked again here: the pool reads once a
        // second and falls back to the load average for this sample.
        *last = Some((then, busy0, total0));
        return None;
    }
    *last = Some((Instant::now(), busy1, total1));
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    #[expect(clippy::cast_precision_loss, reason = "tick counts far below 2^52")]
    Some(busy as f64 / total as f64 * cores as f64)
}

/// Cumulative (busy, total) CPU ticks over every core ([`busy_cores`]).
#[cfg(target_os = "macos")]
fn cpu_ticks() -> Option<(u64, u64)> {
    unsafe extern "C" {
        /// `<mach/mach_init.h>`: this task's send right to its host port.
        fn mach_host_self() -> u32;
        /// `<mach/mach_host.h>`; `host_info_t` is `integer_t *`.
        fn host_statistics(host: u32, flavor: i32, info: *mut i32, count: *mut u32) -> i32;
    }
    /// `HOST_CPU_LOAD_INFO` and its `natural_t cpu_ticks[CPU_STATE_MAX]`:
    /// user, system, idle, nice.
    const HOST_CPU_LOAD_INFO: i32 = 3;
    static HOST: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    // SAFETY: no arguments; it returns a port name this task keeps.
    let host = *HOST.get_or_init(|| unsafe { mach_host_self() });
    let mut ticks = [0_i32; 4];
    let mut count: u32 = 4;
    // SAFETY: `ticks` is four writable `integer_t`s and `count` says so; the
    // kernel writes at most `count` of them.
    let kr =
        unsafe { host_statistics(host, HOST_CPU_LOAD_INFO, ticks.as_mut_ptr(), &raw mut count) };
    if kr != 0 || count != 4 {
        return None;
    }
    #[expect(
        clippy::cast_sign_loss,
        reason = "natural_t counters handed back as integer_t"
    )]
    let [user, system, idle, nice] = ticks.map(|t| u64::from(t as u32));
    let busy = user + system + nice;
    Some((busy, busy + idle))
}

/// Cumulative (busy, total) CPU ticks over every core ([`busy_cores`]).
#[cfg(target_os = "linux")]
fn cpu_ticks() -> Option<(u64, u64)> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    let line = stat.lines().next()?.strip_prefix("cpu ")?;
    let v: Vec<u64> = line
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let total: u64 = v.iter().sum();
    let idle = v.get(3)? + v.get(4).copied().unwrap_or(0);
    Some((total - idle, total))
}

/// Cumulative (busy, total) CPU ticks over every core ([`busy_cores`]).
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn cpu_ticks() -> Option<(u64, u64)> {
    None
}

/// `getloadavg(3)` for the one-minute figure; `None` where it cannot say (it
/// returns -1, or fewer samples than asked, when the host has no load average to
/// give) or says something that is not a load.
#[cfg(unix)]
fn getloadavg_one() -> Option<f64> {
    unsafe extern "C" {
        /// `<stdlib.h>`; declared here because this crate has no dependencies
        /// ([`group`] declares its libc symbols the same way). `int` is `i32`
        /// on every target this gate runs on.
        fn getloadavg(loadavg: *mut f64, nelem: i32) -> i32;
    }
    let mut load = [0f64; 1];
    // SAFETY: `load` is a live, writable array of one double, and `getloadavg`
    // writes at most the count it is given.
    let n = unsafe { getloadavg(load.as_mut_ptr(), 1) };
    (n >= 1 && load[0].is_finite() && load[0] >= 0.0).then_some(load[0])
}

/// The older reading, kept for a host whose `getloadavg` fails and for a host
/// without one: `/proc/loadavg` where it exists, else `sysctl -n vm.loadavg`
/// (macOS prints `{ 1.23 2.34 3.45 }`).
fn load_average_by_file_or_spawn() -> Option<f64> {
    let text = std::fs::read_to_string("/proc/loadavg").ok().or_else(|| {
        let out = Command::new("/usr/sbin/sysctl")
            .args(["-n", "vm.loadavg"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    })?;
    parse_load_average(&text)
}

/// The first number in a load-average line, braces and all.
#[must_use]
pub fn parse_load_average(text: &str) -> Option<f64> {
    text.split(|c: char| c.is_whitespace() || c == '{' || c == '}')
        .find(|t| !t.is_empty())
        .and_then(|t| t.parse().ok())
}

/// The host's physical memory in bytes, read once per run for the plan's memory
/// decisions ([`crate::plan::land_builds`], [`crate::testrun::memory_jobs`]), and
/// whether it is Linux's `MemTotal` (a few percent under the installed RAM);
/// `None` when the host will not say.
///
/// The same two sources `tools/trust-gate-all.sh` reads for the trust lane's
/// `HOST:` figure: Linux's `MemTotal` from `/proc/meminfo`, else `sysctl -n
/// hw.memsize` (macOS: the RAM the machine was sold with). Read here and not
/// through `aterm_sysprobe::physical_memory_mib` because this crate takes no
/// dependency on any aterm crate (its `Cargo.toml` says why).
#[must_use]
pub fn host_memory_bytes() -> Option<(u64, bool)> {
    if let Ok(text) = std::fs::read_to_string("/proc/meminfo") {
        return parse_meminfo_total(&text).map(|b| (b, true));
    }
    let out = Command::new("/usr/sbin/sysctl")
        .args(["-n", "hw.memsize"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .ok()
        .filter(|&b: &u64| b > 0)
        .map(|b| (b, false))
}

/// The memory this host can GIVE builds now, in bytes, read once per run beside
/// [`host_memory_bytes`] and held under its `physical` figure: Linux's
/// `MemAvailable`; on macOS the physical memory less what the kernel holds
/// wired, which no build can page out. `None` when the host will not say.
///
/// WHY (measured 2026-10-06, the 8 GiB Mac): the kernel held 228,350 wired
/// 16 KiB pages (3.5 GiB) with nothing building — a degraded state a reboot
/// clears — and three workspace builds sized for 8 GiB thrashed for 3 h 25 min
/// without finishing the test compile ([`crate::plan::land_builds`]).
#[must_use]
pub fn host_usable_bytes(physical: u64) -> Option<u64> {
    if let Ok(text) = std::fs::read_to_string("/proc/meminfo") {
        return parse_meminfo(&text, "MemAvailable:").map(|b| b.min(physical));
    }
    host_wired_bytes().map(|wired| physical.saturating_sub(wired))
}

/// The memory the kernel holds wired, in bytes: `host_statistics64`'s
/// `wire_count` times the host page size — `vm_stat`'s "Pages wired down".
#[cfg(target_os = "macos")]
fn host_wired_bytes() -> Option<u64> {
    unsafe extern "C" {
        /// `<mach/mach_init.h>`: this task's send right to its host port.
        fn mach_host_self() -> u32;
        /// `<mach/mach_host.h>`; `host_info64_t` is `integer_t *`.
        fn host_statistics64(host: u32, flavor: i32, info: *mut i32, count: *mut u32) -> i32;
        /// `<mach/mach_host.h>`; `vm_size_t` is `uintptr_t`.
        fn host_page_size(host: u32, size: *mut usize) -> i32;
    }
    /// `HOST_VM_INFO64` and `HOST_VM_INFO64_COUNT`: `vm_statistics64` is 152
    /// bytes, 38 `integer_t`s, and `wire_count` is its fourth `natural_t`.
    const HOST_VM_INFO64: i32 = 4;
    const COUNT: u32 = 38;
    // SAFETY: no arguments; it returns a port name this task keeps.
    let host = unsafe { mach_host_self() };
    let mut page: usize = 0;
    // SAFETY: `page` is one writable `vm_size_t`.
    if unsafe { host_page_size(host, &raw mut page) } != 0 || page == 0 {
        return None;
    }
    let mut info = [0_i32; COUNT as usize];
    let mut count = COUNT;
    // SAFETY: `info` is `COUNT` writable `integer_t`s and `count` says so; the
    // kernel writes at most `count` of them.
    let kr = unsafe { host_statistics64(host, HOST_VM_INFO64, info.as_mut_ptr(), &raw mut count) };
    if kr != 0 || count < 4 {
        return None;
    }
    #[expect(
        clippy::cast_sign_loss,
        reason = "a natural_t counter handed back as integer_t"
    )]
    let wired = u64::from(info[3] as u32);
    Some(wired.saturating_mul(u64::try_from(page).ok()?))
}

/// The memory the kernel holds wired: unread off macOS (Linux answers
/// `MemAvailable` instead, [`host_usable_bytes`]).
#[cfg(not(target_os = "macos"))]
fn host_wired_bytes() -> Option<u64> {
    None
}

/// `MemTotal:  16303612 kB` from `/proc/meminfo`, in bytes.
#[must_use]
pub fn parse_meminfo_total(text: &str) -> Option<u64> {
    parse_meminfo(text, "MemTotal:")
}

/// One `<key>  <n> kB` row of `/proc/meminfo`, in bytes; `None` when it is
/// absent, unreadable or zero.
#[must_use]
pub fn parse_meminfo(text: &str, key: &str) -> Option<u64> {
    let kib: u64 = text
        .lines()
        .find_map(|l| l.strip_prefix(key))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    (kib > 0).then(|| kib.saturating_mul(1024))
}

/// What a child did.
#[derive(Clone, Debug)]
pub struct Run {
    /// True only on exit status 0 — a signal death is a failure, as in the shell.
    pub ok: bool,
    pub output: String,
    /// The exit code, for the stages whose child says more than pass/fail (the
    /// redraw gate's `2` is "could not run here"). `None` for a signal death or
    /// a child that never spawned.
    pub code: Option<i32>,
    /// Set when the child could not be spawned at all (tool vanished mid-run).
    pub spawn_error: Option<String>,
}

/// The OS's own sentence for ENOSPC, as cargo, rustc and a test harness print
/// it verbatim (`… : No space left on device (os error 28)`).
pub const ENOSPC_SENTENCE: &str = "No space left on device";

impl Run {
    #[must_use]
    pub fn trimmed_output(&self) -> &str {
        self.output.trim_end_matches('\n')
    }

    /// Why this child is not a finding about the tree: `Some(reason)` when it
    /// never spawned ([`Run::spawn_error`]), when its output carries
    /// [`ENOSPC_SENTENCE`] (it ran out of disk), or when every failed test says
    /// the machine refused it ([`crate::libtest::COULD_NOT_RUN_SENTINEL`], e.g.
    /// aterm-link's stray-daemon guard or a starved paint take; one failure
    /// without it keeps the child a finding,
    /// [`crate::libtest::environment_refusals`]). `None` for a child that ran and
    /// answered, pass or fail. [`crate::ladder::Report::fail_child`] asks this
    /// first.
    ///
    /// The disk sentence is matched anywhere in the output, so a failing test
    /// that prints it for its own reasons reads as COULD NOT RUN instead of FAIL:
    /// both red, the cost is a severity, never a green. A passing child is never
    /// asked.
    #[must_use]
    pub fn environment_failure(&self) -> Option<String> {
        if self.ok {
            return None;
        }
        if let Some(e) = &self.spawn_error {
            return Some(e.clone());
        }
        if self.output.contains(ENOSPC_SENTENCE) {
            return Some(format!("the child ran out of disk ({ENOSPC_SENTENCE})"));
        }
        crate::libtest::environment_refusals(&self.output).map(|names| {
            let shown: Vec<&str> = names.iter().take(4).map(String::as_str).collect();
            let more = names.len().saturating_sub(shown.len());
            format!(
                "every failing test refused the machine, not the tree ({}{}) — see `{}` in \
                 their output",
                shown.join(", "),
                if more > 0 {
                    format!(" and {more} more")
                } else {
                    String::new()
                },
                crate::libtest::COULD_NOT_RUN_SENTINEL
            )
        })
    }
}

/// The default wall-clock ceiling on one stage child: 3 hours.
///
/// Without a ceiling a deadlocked stage does not turn the gate red, it makes it
/// silent. The ceiling only tells wedged from slow, so it sits far above the
/// slowest honest child (a cold full-workspace build or `--no-fail-fast` test
/// run on a loaded machine): a false kill turns a true green into a false red
/// and costs a full re-run. It catches a child that never exits; it is not a
/// performance budget and must not be tuned like one.
///
/// The kill reaches the child's whole process group ([`group`]): the `trustc`s
/// a `targo` forked and the aterms a test harness launched go with it. What
/// escapes is a process that left the group on purpose (e.g. one that started
/// its own session), and the diagnostic says so.
pub const DEFAULT_CHILD_CEILING: Duration = Duration::from_secs(3 * 60 * 60);

/// The `SIGTERM` grace the stage suites ask for ([`Cmd::term_grace`]), and the
/// longest an interrupted gate waits for any group it sent `SIGTERM` before it
/// `SIGKILL`s the rest and dies. A bound, not a sleep: the gate stops waiting
/// once every such group has exited, or at a second interrupt.
pub const TERM_GRACE: Duration = Duration::from_secs(10);

/// Anything past a century is not a number of seconds anyone meant, and
/// `Duration::from_secs_f64` panics outright on a value it cannot hold.
const MAX_CEILING_SECS: f64 = 3_153_600_000.0;

/// Parse a `--stage-timeout` value: `Some(Some(d))` moves
/// [`DEFAULT_CHILD_CEILING`] to `d` (seconds, fractional accepted), `Some(None)`
/// removes it (`0`, `off`, `none`, `never`), and `None` is a value this cannot
/// read, which the command line refuses as a usage error. Removing the ceiling
/// must be typed (`--stage-timeout off`), never reached by typing `45m` into a
/// field that wants seconds.
#[must_use]
pub fn parse_ceiling(text: &str) -> Option<Option<Duration>> {
    let text = text.trim();
    if text == "0"
        || text.eq_ignore_ascii_case("off")
        || text.eq_ignore_ascii_case("none")
        || text.eq_ignore_ascii_case("never")
    {
        return Some(None);
    }
    match text.parse::<f64>() {
        // `is_finite` first, so a NaN never reaches the range test (it would
        // land in the refused arm anyway, but only by accident).
        Ok(v) if v.is_finite() && (0.0..=MAX_CEILING_SECS).contains(&v) => {
            // `0.0` (and `0.000`) mean what the bare `0` above means.
            Some((v > 0.0).then(|| Duration::from_secs_f64(v)))
        }
        // Empty, negative, `45m`, `NaN`, a century: not a ceiling.
        _ => None,
    }
}

static SEQ: AtomicU64 = AtomicU64::new(0);

/// macOS's QoS launcher, the same one `snapshot::delete_in_background` uses for
/// trash deletion. Absent elsewhere, where a [`Cmd::demoted`] child runs as is.
pub const TASKPOLICY: &str = "/usr/sbin/taskpolicy";

/// Spawn, wait, and collect. Never panics on a missing tool: an unspawnable
/// child is a failed run whose "output" names the reason, so the caller still
/// reaches its own fail-closed branch.
#[must_use]
pub fn run(cmd: &Cmd, env: ExecEnv<'_>) -> Run {
    run_measured(cmd, env).0
}

/// [`run`], and the CPU the child spent: user plus system time of the child and
/// of every descendant it reaped itself, as `wait4(2)` reports it ([`reap`]).
/// `None` when the child was never reaped normally (unspawnable, killed at the
/// ceiling) or off Unix.
#[must_use]
pub fn run_measured(cmd: &Cmd, env: ExecEnv<'_>) -> (Run, Option<Duration>) {
    let mut c = spawn_command(cmd, env, Path::new(TASKPOLICY));
    c.current_dir(cmd.cwd.as_deref().unwrap_or(env.cwd));
    if cmd.exact_env {
        c.env_clear();
    } else {
        c.env("PATH", env.path);
        for k in env.remove_env {
            c.env_remove(k);
        }
        for (k, v) in env.add_env {
            c.env(k, v);
        }
    }
    for (k, v) in &cmd.envs {
        c.env(k, v);
    }

    match &cmd.capture {
        Capture::Silent => {
            c.stdout(Stdio::null()).stderr(Stdio::null());
            let (r, _, cpu) = finish(c, cmd, env.child_ceiling, None);
            (r, cpu)
        }
        Capture::Append(path) => match open_append(path) {
            Ok(f) => match f.try_clone() {
                Ok(f2) => {
                    c.stdout(f).stderr(f2);
                    let (r, _, cpu) = finish(c, cmd, env.child_ceiling, None);
                    (r, cpu)
                }
                Err(e) => (spawn_failure(cmd, &e.to_string()), None),
            },
            Err(e) => (spawn_failure(cmd, &e.to_string()), None),
        },
        Capture::Emit => {
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            let log = env
                .scratch
                .join(format!("stage.{}.{seq}.log", std::process::id()));
            let mut file = match File::create(&log) {
                Ok(f) => f,
                Err(e) => return (spawn_failure(cmd, &e.to_string()), None),
            };
            if let Some(text) = &cmd.preamble
                && let Err(e) = file.write_all(text.as_bytes())
            {
                return (spawn_failure(cmd, &e.to_string()), None);
            }
            let file2 = match file.try_clone() {
                Ok(f) => f,
                Err(e) => return (spawn_failure(cmd, &e.to_string()), None),
            };
            c.stdout(file).stderr(file2);
            let (mut r, logged, cpu) = finish(c, cmd, env.child_ceiling, Some(&log));
            // `finish` leaves `output` empty except for a ceiling diagnostic, so
            // the child's own bytes go in front of it: a timed-out stage still
            // shows how far it got. These are `finish`'s single read of the log,
            // which the diagnostic's note was computed from too.
            r.output
                .insert_str(0, logged.as_deref().unwrap_or_default());
            std::fs::remove_file(&log).ok();
            (r, cpu)
        }
    }
}

/// The `Command` for `cmd` before its cwd and environment are applied: the
/// program itself or, for a [`Cmd::demoted`] child, the program behind
/// `taskpolicy -c utility`.
///
/// Wrapped only when `taskpolicy` exists and the program resolves to an
/// executable: for a missing program `taskpolicy` exits 66 itself, which would
/// hide the [`Run::spawn_error`]. `taskpolicy` execs the program rather than
/// parenting it, so the pid [`over_ceiling`] kills is the tool's own; the timing
/// rows and the diagnostic name `cmd`'s argv, never the wrapper.
///
/// The child leads its own process group ([`group`]) and reads no stdin: a
/// background-group process that reads the terminal is stopped by `SIGTTIN`,
/// which would be a hang.
fn spawn_command(cmd: &Cmd, env: ExecEnv<'_>, taskpolicy: &Path) -> Command {
    let mut c = if cmd.demoted && crate::is_executable_file(taskpolicy) && resolves(cmd, env) {
        let mut c = Command::new(taskpolicy);
        c.args(["-c", "utility"]).arg(&cmd.program);
        c
    } else {
        Command::new(&cmd.program)
    };
    c.args(&cmd.args).stdin(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        c.process_group(0);
    }
    c
}

/// Whether `execvp` would find `cmd.program` from the child's cwd, on the PATH
/// the child is actually given (its own [`Cmd::envs`] override included).
fn resolves(cmd: &Cmd, env: ExecEnv<'_>) -> bool {
    let program = cmd.program.as_path();
    let cwd = cmd.cwd.as_deref().unwrap_or(env.cwd);
    if program.components().count() > 1 {
        return crate::is_executable_file(&cwd.join(program));
    }
    let path = cmd
        .envs
        .iter()
        .rev()
        .find(|(k, _)| k == "PATH")
        .map_or(env.path, |(_, v)| v.as_os_str());
    std::env::split_paths(path).any(|d| crate::is_executable_file(&cwd.join(d).join(program)))
}

/// Spawn and wait, under `ceiling`. Returns a [`Run`] whose `output` is empty
/// unless the ceiling fired (the callers own the captured bytes and splice the
/// diagnostic onto them).
///
/// `log` is the file the child's stdout and stderr go to, when the caller has
/// one. It is read back once, here, after the child is reaped, and returned
/// beside the `Run`: the ceiling diagnostic uses it to name the test a killed
/// `targo test` child was running ([`crate::libtest`]), and the caller splices
/// the same bytes, so a surviving grandchild appending later cannot make the
/// two disagree. Lossy, so one stray byte costs neither.
///
/// While the child runs, its process group is on [`group`]'s live list, so an
/// interrupted gate takes it down too; while it is being spawned it is counted
/// in flight ([`group::Spawning`]), so an interrupted gate waits for it to get
/// there. The child is reaped through one door ([`group::Live::reap`]), which
/// leaves it unreaped once an interrupted gate may have seen its group: the
/// gate signals a group only while it holds the group's leader.
fn finish(
    mut c: Command,
    cmd: &Cmd,
    ceiling: Option<Duration>,
    log: Option<&Path>,
) -> (Run, Option<String>, Option<Duration>) {
    let read_log = |log: Option<&Path>| {
        log.map(|p| String::from_utf8_lossy(&std::fs::read(p).unwrap_or_default()).into_owned())
    };
    // An interrupted gate is on its way down: a child spawned now would only be
    // killed. The handler raises the flag before it signals anything, so at most
    // a spawn already past this check races it. Between `spawn` and the live
    // list that child exists and the handler cannot name its group, so the
    // spawn is counted in flight from BEFORE the flag is read until the child
    // is on the list (or there is none): the handler waits for the count,
    // then SIGKILLs what reached the list. `group::Spawning` has the argument
    // and what it leaves. Each early return lowers the count before it reads
    // the log, so the handler never waits on a file.
    #[cfg(unix)]
    let spawning = group::Spawning::begin();
    // A stand-in gate's hold on the window, for the tests of it: here, with
    // the spawn counted and the flag not yet read, or below, once the child
    // exists.
    #[cfg(all(test, unix))]
    tests::in_the_window(None);
    #[cfg(unix)]
    if group::stopping() {
        drop(spawning);
        return (
            failed_to_wait("the gate was interrupted before this child started"),
            read_log(log),
            None,
        );
    }
    let child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => {
            #[cfg(unix)]
            drop(spawning);
            return (failed_to_wait(&e.to_string()), read_log(log), None);
        }
    };
    #[cfg(all(test, unix))]
    tests::in_the_window(Some(child.id()));
    let mut stage = Stage {
        #[cfg(unix)]
        live: group::Live::enter(child.id(), cmd.term_grace.is_some()),
        child,
    };
    #[cfg(unix)]
    drop(spawning);
    let Some(limit) = ceiling else {
        return match reap(&mut stage, true) {
            Ok(Some((st, cpu))) => (reaped(Ok(st)), read_log(log), cpu),
            Ok(None) => unreachable!("a blocking reap returns only once the child exited"),
            Err(e) => (failed_to_wait(&e.to_string()), read_log(log), None),
        };
    };

    // Poll, don't block: std has no wait-with-deadline, and handing the `Child`
    // to a helper thread would leave the killer holding a raw pid the waiter may
    // already have reaped (a pid-reuse race). With no `libc` (this crate has no
    // dependencies), a non-blocking reap plus a sleep is the construction.
    //
    // The nap starts at 1 ms and doubles to a 25 ms cap, so sub-second children
    // pay about a millisecond, and a long child costs 40 non-blocking waits a
    // second.
    let started = Instant::now();
    let mut nap = Duration::from_millis(1);
    loop {
        match reap(&mut stage, false) {
            Ok(Some((st, cpu))) => return (reaped(Ok(st)), read_log(log), cpu),
            Ok(None) => {}
            Err(e) => return (failed_to_wait(&e.to_string()), read_log(log), None),
        }
        let elapsed = started.elapsed();
        let Some(left) = limit.checked_sub(elapsed) else {
            // Kill and reap first, then read: the bytes the child wrote before it
            // died are the ones the note and the splice both describe.
            let killed = kill_and_reap(&mut stage, cmd.term_grace);
            let logged = read_log(log);
            return (
                over_ceiling(cmd, elapsed, limit, &killed, logged.as_deref()),
                logged,
                None,
            );
        };
        // Never sleep past the ceiling: the last nap lands exactly on it.
        std::thread::sleep(nap.min(left));
        nap = (nap * 2).min(Duration::from_millis(25));
    }
}

/// A stage child and, on Unix, its group's hold ([`group::Live`]): the group's
/// place on the live list, and the only handle the runner signals that group
/// or reaps that child through.
struct Stage {
    child: std::process::Child,
    #[cfg(unix)]
    live: group::Live,
}

/// Reap the stage's child once it has exited: its status, and the user plus
/// system CPU it and the descendants it reaped spent. `Ok(None)` while it still
/// runs (never when `block`ing).
///
/// On Unix this is `wait4(2)`, std's `wait` with the `rusage` it drops: a test
/// run's wall time on a saturated machine says how long a binary waited, its CPU
/// what it cost everything else (measured 2026-10-05: a gate test run at load
/// 12-20 on 14 cores moved 3 % while summed binary walls moved 7 %). It goes
/// through [`group::Live::reap`], the one door a stage child is reaped
/// through, which answers an error instead of reaping once an interrupted gate
/// may have seen the child's group. Once this reaps, the `Child` is never
/// waited on again: [`kill_and_reap`] runs only on a child that has not exited.
fn reap(
    stage: &mut Stage,
    block: bool,
) -> io::Result<Option<(std::process::ExitStatus, Option<Duration>)>> {
    #[cfg(unix)]
    {
        stage
            .live
            .reap(block)
            .map(|r| r.map(|(st, cpu)| (st, Some(cpu))))
    }
    #[cfg(not(unix))]
    {
        let st = if block {
            stage.child.wait().map(Some)
        } else {
            stage.child.try_wait()
        };
        st.map(|st| st.map(|st| (st, None)))
    }
}

/// `SIGKILL` the child's whole process group and reap the child, reporting only
/// a kill that itself failed. Separate from [`over_ceiling`] so the log is read
/// after the processes are gone.
///
/// Every signal here goes to the group while this thread holds its leader, the
/// child, unreaped, and the reap comes last, through the one door
/// ([`group::Live`] has the rule and why: a group's id is the gate's only
/// while its leader is, and a reaped leader's id was another process's within
/// 3 ms when a probe aimed for it). The kill is repeated until the group
/// has stayed empty a while ([`group::Live::kill`]): one `SIGKILL` misses a
/// process the group was making as it landed. That costs every ceiling kill
/// its quiet rounds, the one that took its whole group at once too: a `sleep`
/// killed at its ceiling was reported 1.4 ms after it before and is 51 ms
/// after it now (the median of 40, measured 2026-10-13). Rounds that ran out
/// with a process still there are reported, since the block below would
/// otherwise say the group is gone.
///
/// With a `grace` ([`Cmd::term_grace`]) the group is sent `SIGTERM` first and
/// given up to `grace` to empty, polled without reaping so the id stays held. A
/// group that emptied in time, and stayed empty for the quiet rounds
/// ([`group::Live::gone`]: one empty reading can be of a group that is still
/// making a process), needs no `SIGKILL`: its EXIT traps ran, and the note
/// says so. One that answered again inside those rounds is `SIGKILL`ed as a
/// group that outlived its grace.
fn kill_and_reap(stage: &mut Stage, grace: Option<Duration>) -> String {
    // Held, for every signal below: the child is reaped by this function's
    // last statement and by nothing before it (the grace's early return
    // reaps as its own last step), so the group's id is the gate's throughout.
    #[cfg(unix)]
    let term = match grace {
        None => String::new(),
        Some(grace) => match stage.live.term() {
            Ok(()) => {
                let deadline = Instant::now() + grace;
                while stage.live.alive() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(25));
                }
                if stage.live.gone() {
                    let _ = reap(stage, true);
                    return format!(
                        "  (SIGTERM first, with a {:.1}s grace: the group exited within it, so \
                         its EXIT traps ran)\n",
                        grace.as_secs_f64()
                    );
                }
                format!(
                    "  (SIGTERM first; the group outlived its {:.1}s grace and was SIGKILLed, \
                     so an EXIT trap still running was cut short)\n",
                    grace.as_secs_f64()
                )
            }
            Err(e) => format!("  (SIGTERM to its group failed: {e}; SIGKILLed at once)\n"),
        },
    };
    #[cfg(not(unix))]
    let term = {
        let _ = grace;
        String::new()
    };
    #[cfg(unix)]
    let kill = match stage.live.kill() {
        Ok(false) => String::new(),
        Ok(true) => format!(
            "  (its process group still held a process after {} more SIGKILLs, a millisecond \
             apart: one may be running on)\n",
            group::KILL_ROUNDS
        ),
        // By pid, and the child is not reaped yet: the pid is the gate's.
        Err(e) => match stage.child.kill() {
            Ok(()) => format!(
                "  (killing its process group failed: {e} — SIGKILL reached the direct child \
                 only)\n"
            ),
            Err(k) => format!("  (SIGKILL itself failed: {e}; {k})\n"),
        },
    };
    #[cfg(not(unix))]
    let kill = match stage.child.kill() {
        Ok(()) => String::new(),
        Err(e) => format!("  (SIGKILL itself failed: {e})\n"),
    };
    // Reap, so no zombie is left for the rest of the run: last, and through
    // the door, which leaves the zombie where an interrupted gate may still
    // signal its group. SIGKILL is not maskable, so this returns once the
    // kernel has torn the process down.
    let _ = reap(stage, true);
    term + &kill
}

/// The ceiling fired: a failure that says so, never a skip or a pass, and never
/// mistakable for a child that merely exited nonzero.
fn over_ceiling(
    cmd: &Cmd,
    elapsed: Duration,
    limit: Duration,
    kill: &str,
    logged: Option<&str>,
) -> Run {
    // Name the test: a killed `targo test` child's log says which binary never
    // printed its `test result:` line and which test libtest reported slow with
    // no verdict after. The bytes are the caller's one read of the log, so the
    // note and the ladder's log agree.
    let note = logged.and_then(crate::libtest::note).unwrap_or_default();
    let secs = elapsed.as_secs_f64();
    let limit_secs = limit.as_secs_f64();
    let argv = cmd.argv().join(" ");
    Run {
        ok: false,
        output: format!(
            "aterm-verify: TIMEOUT — child killed after {secs:.1}s, over the {limit_secs:.1}s \
             wall-clock ceiling\n\
             \x20 child: {argv}\n\
             {note}\
             {kill}\
             \x20 Never a pass and never a skip: the child never exited.\n\
             \x20 Raise the ceiling with --stage-timeout <seconds>, or remove it with \
             --stage-timeout off.\n\
             \x20 The kill reached the child's whole process group (a targo's trustc \
             processes, a harness's own children); only a process that left the group — one \
             that started its own session, as a PTY-hosted shell does — may still be running.\n"
        ),
        // A signal death, which is what this is, and what the shell would have
        // reported too. `spawn_error` stays `None`: the child ran fine, it just
        // never finished.
        code: None,
        spawn_error: None,
    }
}

/// Every stage child leads a process group of its own, so the gate can end
/// everything a stage started: the ceiling kills the whole group
/// ([`kill_and_reap`]), and an interrupted gate kills every group still running
/// ([`group::kill_on_interrupt`]), the group of a child a stage was in the
/// middle of spawning among them ([`group::Spawning`], which also says what
/// that leaves). Survivors (a hung test's `aterm-gui --headless` daemons, say)
/// would otherwise hold sockets and CPU and trip the next run's stray-daemon
/// guard.
///
/// A GROUP IS SIGNALLED ONLY WHILE THE GATE HOLDS ITS LEADER. A group's id is
/// its leader's pid, and that number is the gate's only while the leader is
/// its child, running or exited and not yet reaped: the kernel hands out no
/// id that is in use, and hands a reaped leader's on to the next process to
/// reach it. So the runner sends a group nothing, not even the question
/// "anyone there?", once it has reaped the group's leader, and a leader an
/// interrupted gate may still signal is never reaped at all
/// ([`group::Live`] has the rule, the argument and the measurements).
///
/// One `SIGKILL` does not end a group that is making a process at that
/// instant: on macOS it reaches the members the group has and not the one on
/// its way. So each `SIGKILL` the runner sends a group is sent again, a
/// millisecond apart, until the group has stayed empty for a while or a
/// bound has passed ([`group::KILL_ROUNDS`] has the measurements, the rule,
/// what it costs every kill and what is left).
///
/// The interrupt half is why this needs `unsafe`: a child in its own group no
/// longer receives the terminal's `SIGINT`, so the gate must forward it, and std
/// has no signal handling. The handler does only async-signal-safe work: atomic
/// loads and stores, `killpg`, `nanosleep` (the bounded `SIGTERM` grace,
/// [`Cmd::term_grace`], the bounded wait for a spawn in flight, and the
/// bounded rounds of the repeated kill), then
/// `signal(SIG_DFL)` and `raise`, so the gate still
/// dies of the signal it was sent (of the last, when a second one hurried it).
/// This crate has no dependencies, so its libc symbols are declared here, with
/// `wait4` and `waitid`, which [`reap`] uses and the handler does not.
#[cfg(unix)]
pub mod group {
    use std::ffi::c_long;
    use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

    const SIGHUP: i32 = 1;
    const SIGINT: i32 = 2;
    const SIGKILL: i32 = 9;
    const SIGTERM: i32 = 15;
    /// `SIG_DFL`, as the handler-pointer value `signal(3)` takes.
    const SIG_DFL: usize = 0;

    /// `struct timespec`: `time_t` and `long` are both `long` on every LP64
    /// target this gate runs on (macOS and Linux, arm64 and x86_64).
    #[repr(C)]
    struct Timespec {
        tv_sec: c_long,
        tv_nsec: c_long,
    }

    /// `struct timeval`: `time_t`, then `suseconds_t`, which is an `int` padded
    /// to eight bytes on macOS and a `long` on Linux. Read as an `i32` it is the
    /// value either way on these little-endian targets (it is below 10^6).
    #[repr(C)]
    #[derive(Default)]
    struct Timeval {
        tv_sec: c_long,
        tv_usec: i32,
        _pad: i32,
    }

    /// `struct rusage`: the two times, then fourteen `long` counters this reads
    /// none of; 144 bytes on every LP64 target this gate runs on.
    #[repr(C)]
    #[derive(Default)]
    struct Rusage {
        utime: Timeval,
        stime: Timeval,
        _counters: [c_long; 14],
    }

    /// `siginfo_t`, as far as [`exited`] reads it: `si_signo` comes first on
    /// macOS and on Linux, `si_code` third on macOS (the one place it is
    /// read), and the rest is room for the kernel to write the whole struct
    /// into (104 bytes on macOS, 128 on Linux, aligned to a pointer on
    /// both).
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[repr(C, align(8))]
    struct Siginfo {
        si_signo: i32,
        _si_errno: i32,
        #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
        si_code: i32,
        _rest: [i32; 29],
    }

    unsafe extern "C" {
        fn killpg(pgrp: i32, sig: i32) -> i32;
        fn signal(sig: i32, handler: usize) -> usize;
        fn raise(sig: i32) -> i32;
        fn nanosleep(req: *const Timespec, rem: *mut Timespec) -> i32;
        fn wait4(pid: i32, status: *mut i32, options: i32, rusage: *mut Rusage) -> i32;
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        fn waitid(idtype: i32, id: u32, info: *mut Siginfo, options: i32) -> i32;
    }

    /// The process groups of the stage children running now, `0` for a free
    /// slot. Fixed-size and lock-free because a signal handler reads it. A few
    /// dozen are ever live; a child that finds every slot taken still runs, and
    /// only an interrupt would miss it.
    ///
    /// A negative entry is a group that asked for a `SIGTERM` grace
    /// ([`crate::exec::Cmd::term_grace`]): one atomic carries both facts, so the
    /// handler never reads a group with another child's flag.
    static LIVE: [AtomicI32; 256] = [const { AtomicI32::new(0) }; 256];

    /// The interrupt that began ending the gate, `0` until one arrives. The
    /// handler sets it before it signals anything, so no stage starts a child
    /// the gate is about to kill (`finish` asks). One atomic says both that an
    /// interrupt is not the first and which signal the first was
    /// ([`on_signal`]).
    static STOPPING: AtomicI32 = AtomicI32::new(0);

    /// The latest second interrupt, `0` while there is none. Its frame leaves
    /// the number here for the first one's ([`on_signal`]), which ends its
    /// grace on it and dies of it.
    static AGAIN: AtomicI32 = AtomicI32::new(0);

    /// How many stage threads are inside a spawn now ([`Spawning`]). The
    /// handler reads it, so it is an atomic like the rest.
    static SPAWNING: AtomicUsize = AtomicUsize::new(0);

    /// How many rounds [`on_signal`] waits, at most, for the spawns in
    /// flight to reach the live list. A round is a 1 ms [`nap`], so this is
    /// about a second (1.3 s when it was measured), and it is paid only
    /// while a spawn is in flight: the wait ends on the first round that
    /// finds none.
    ///
    /// Why a second. A spawn is in flight for the length of one
    /// `Command::spawn`: 0.09 ms at the median and 0.25 ms at the most over
    /// 4,000 spawns on an idle 18-core Mac, and 1.3 ms and 84 ms over 6,000
    /// at a load of 42 to 52 (measured 2026-10-13). A second is twelve
    /// times the worst of those. It is not longer because one case pays it
    /// whole and gains nothing ([`Spawning`], what is left), and it is small
    /// beside the ten seconds an interrupted gate already gives a suite
    /// ([`super::TERM_GRACE`]).
    const SPAWN_ROUNDS: u16 = 1_000;

    /// How many more times, at most, a group this runner has sent `SIGKILL`
    /// is sent it again, a millisecond apart ([`rekill`]): by the ceiling
    /// kill, by a bounded git call, and by [`on_signal`] for every group it
    /// killed.
    ///
    /// WHY A SIGKILL IS REPEATED. On macOS a group's `SIGKILL` reaches the
    /// members the group has at that instant and not a process one of them
    /// is making: a signal does not find a child until its `fork` or
    /// `posix_spawn` is nearly done (read in xnu-12377: `killpg` looks each
    /// member up with `proc_find`, which refuses a process still being
    /// made). The parent dies and the child runs on. Measured 2026-10-13 on
    /// macOS 26.6 with no gate in it, one `SIGKILL` at a group whose leader
    /// was inside the call: the child was left in 1,435 of 3,000 groups for
    /// a `fork`, in 498 of 3,000 for a `posix_spawn`, and in 807 of 2,000
    /// groups forking back to back. One more `SIGKILL` reaches it once the
    /// call is over: each of 11,000 such groups was emptied by one.
    ///
    /// WHY NOT "UNTIL THE GROUP IS GONE". `killpg` answers whether it found
    /// a process (a group down to its unreaped leader says `EPERM`, an
    /// empty one `ESRCH`: 200 of 200 each), and it can answer "nobody"
    /// while a child is still on its way. A process stops answering when
    /// the thread that takes the kill begins its exit, and another of its
    /// threads can be inside `posix_spawn` then: for that long neither
    /// parent nor child is found. Killing only while the group answered
    /// left a `sleep` in 19 of 3,000 groups whose second thread was
    /// spawning, and in 16 of 1,500 spawning back to back. `fork` is spared
    /// (its kernel half ends the child of a process that has begun to
    /// exit), and so is every process with one thread, a shell among them,
    /// which answers until its `fork` is over. A `targo` or a test harness
    /// is neither. So the rule is time, [`QUIET_ROUNDS`]: a group is done
    /// with when that many rounds running have found nobody in it.
    ///
    /// WHAT IT BUYS. On stand-in gates whose stage was making processes
    /// back to back as the kill landed (measured the same day, at a load of
    /// 13 to 22 on 18 cores): one kill, as before, left a process of the
    /// stage after 50 and 62 of 300 ceiling kills of a forking shell
    /// (without a grace, and with one it ignored), after 80 and 59 of 300
    /// interrupts of it, and after 45 of 300 ceiling kills and 27 of 300
    /// interrupts of a stage spawning from a second thread. With the
    /// rounds: none of 500 each.
    ///
    /// THE BOUND. A group that keeps answering is killed this many times
    /// and no more: a third of a second, at 1.26 ms a round. One that
    /// emptied stopped answering within 30 ms of its kill at a load of 42
    /// to 64 (within 1.8 ms at a load under 28), and no group in 25,100
    /// kills by this rule took more than 14 rounds over its quiet ones.
    ///
    /// ONLY WHERE IT WAS MEASURED ([`REPEATS`]): macOS. Linux needs none of
    /// it: its `fork` takes the group's lock to add the child, and fails if
    /// a fatal signal is pending for the parent by then, so the kill takes
    /// the child with the parent or there is no child (`copy_process`, read
    /// in v6.12; nothing here was run on Linux). There an unreaped zombie
    /// answers `killpg` too, so these rounds would only ever run out. On
    /// any other Unix nothing was measured and nothing was read: a group
    /// is sent its one `SIGKILL` there, as before.
    ///
    /// WHAT IS LEFT. A process that left the group, as before. A child
    /// that takes longer to appear than the quiet rounds last. And a group
    /// that still holds a process after every round: something in it makes
    /// processes faster than a kill a millisecond ends them, and the
    /// ceiling's report says so.
    pub(super) const KILL_ROUNDS: u16 = 250;

    /// How many rounds in a row must find nobody in any group before
    /// [`rekill`] stops. It is what every `SIGKILL` of this runner now
    /// costs when it took its whole group at once, which is nearly every
    /// time: 50 ms, where an interrupted stand-in gate used to be gone
    /// 0.3 ms after its signal.
    ///
    /// It has to outlast a `posix_spawn` in flight in a process that has
    /// already stopped answering ([`KILL_ROUNDS`]). Measured 2026-10-13,
    /// groups that read as empty and then answered again did so 1.2 ms
    /// after their kill at the latest, at a load of 3 to 28 (544 of them)
    /// and at a load of 42 to 64 under a load generator (696 of them), and
    /// 8.9 ms after it while another job's build was starting up beside
    /// them (355 of them: 2.2 ms at the ninetieth percentile, 5.9 ms at the
    /// ninety-ninth). Forty rounds are over five times the longest of
    /// those.
    ///
    /// THE COST IS TAKEN ON PURPOSE (ruled 2026-10-13). A kill is a rare
    /// event, a stage over its ceiling or an interrupt, and no rule that
    /// costs a clean kill nothing is correct: one empty answer is exactly
    /// the reading that was measured wrong for a process that spawns from a
    /// second thread, which a `targo` and a test harness both are.
    ///
    /// The same quiet is asked of a group that a `SIGTERM` seems to have
    /// emptied, before the ceiling's grace believes it ([`Live::gone`]).
    pub(super) const QUIET_ROUNDS: u16 = 40;

    /// Whether this kernel's group `SIGKILL` is repeated at all, and an
    /// empty reading of a group doubted: on macOS, where both were measured
    /// ([`KILL_ROUNDS`] says why not on Linux, and what any other Unix
    /// gets).
    pub(super) const REPEATS: bool = cfg!(target_os = "macos");

    /// Whether this module can see that a child has exited without reaping
    /// it ([`exited`]), which is what lets an interrupted gate signal a
    /// group without ever signalling an id it no longer holds ([`Live`]):
    /// on macOS and Linux, where `waitid`'s numbers were read. Nowhere else
    /// does the gate catch an interrupt ([`kill_on_interrupt`]).
    pub(super) const HOLDS: bool = cfg!(any(target_os = "macos", target_os = "linux"));

    /// What a stage is told of a child [`Live::reap`] left unreaped, in
    /// place of a status.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    const KEPT: &str = "the gate was interrupted while this child ran";

    /// How many stage groups are on the live list now (a graced group's negative
    /// entry counts): what a test waits on before it interrupts.
    #[cfg(test)]
    pub(crate) fn live_count() -> usize {
        LIVE.iter()
            .filter(|slot| slot.load(Ordering::SeqCst) != 0)
            .count()
    }

    /// How many spawns are in flight now ([`Spawning`]): what a test reads
    /// once its own are done, or from inside one.
    #[cfg(test)]
    pub(crate) fn in_flight() -> usize {
        SPAWNING.load(Ordering::SeqCst)
    }

    /// A spawn in flight, for as long as this value lives. `finish` holds
    /// one from before it asks [`stopping`] until its child's group is on
    /// [`LIVE`], or until it knows there is no child (the gate was stopping,
    /// or the spawn failed). In between a child can exist that the list does
    /// not hold yet. It leads its own group, so neither the handler's sweeps
    /// nor the terminal's Ctrl-C reach it, and until 2026-10-13 it ran on
    /// after an interrupted gate had died: measured then on a stand-in gate
    /// spawning children back to back, 35 of 600 interrupts left one with
    /// one stage thread and 84 of 400 with four. [`on_signal`] now waits
    /// for the count to reach zero and then sweeps the list once more, and
    /// none of 1,000 did either way.
    ///
    /// WHY THAT MISSES NO CHILD. `finish` raises the count and then reads
    /// [`STOPPING`]. The handler claims `STOPPING` and, after its kills,
    /// reads the count and then the list. All of it is `SeqCst`, so those
    /// steps have one order, and `finish`'s read of the flag falls on one
    /// side of the handler's claim:
    ///
    /// - after it: `finish` reads the gate stopping and spawns nothing;
    /// - before it: then the raise came before the claim too, and so before
    ///   the handler's read of the count. The handler reads the count
    ///   raised until `finish` lowers it, and `finish` lowers it only once
    ///   the child's group is on the list (a full list takes no more, which
    ///   [`LIVE`] owns up to). So the sweep that follows a count of zero
    ///   finds that group and `SIGKILL`s it.
    ///
    /// A value is dropped in ordinary code, never in the handler, and on
    /// every way out of `finish`, so no path leaves the count raised.
    ///
    /// WHAT IS LEFT. The wait is bounded ([`SPAWN_ROUNDS`]), and two spawns
    /// get nothing from it:
    ///
    /// - one that takes longer than the wait. Its child is missed, as every
    ///   such child was before;
    /// - one on the very thread the interrupt's handler runs on. That thread
    ///   is stopped underneath the handler, so its count cannot drop: the wait
    ///   runs out, and a child that already existed is missed. Which thread
    ///   gets the handler is the kernel's choice. macOS and Linux both offer a
    ///   signal sent to the process to its main thread first (XNU's
    ///   `get_signalthread`, Linux's `complete_signal`), and the ladder's
    ///   children are started by stage threads, so in a gate this is the main
    ///   thread's own toolchain query and nothing a stage runs. Measured
    ///   2026-10-13 on macOS 26.6: with stage threads spawning, none of 3,800
    ///   interrupts left a child and none waited the second out; with the MAIN
    ///   thread spawning back to back, 21 of 400 still left one (19 of 400
    ///   before), and those gates died 1.3 s after the interrupt. Closing it
    ///   means blocking the three signals on the spawning thread, and std
    ///   hands a child its parent thread's signal mask (measured the same
    ///   day), so every child would have to unblock them itself, in a
    ///   `pre_exec`, which changes how std spawns. This crate keeps the plain
    ///   spawn.
    ///
    /// And one spawn is not counted at all: the gate's git calls to the
    /// remote (`differential::git_bounded`: main's tip, the published notes,
    /// a baseline's push) put their own group on the list, a few a run, from
    /// the main thread. A count would buy them nothing there (the second
    /// case) and cost its second, so an interrupt that lands inside one of
    /// those spawns leaves that git running, as it did before.
    ///
    /// WHAT THE COUNT COSTS, in a spawn this gate does not make today: a
    /// child named without a slash. `run_measured` sets `PATH`, and std
    /// then starts such a child by `fork` and `exec` (read in this
    /// toolchain's std). Until that fork's own `setpgid` it is a copy of the
    /// gate inside the gate's group, with this handler installed and the
    /// count as it stood at the fork, one at least. An interrupt sent to the
    /// group, as a terminal's Ctrl-C is, reaches the copy too. It waits out
    /// [`SPAWN_ROUNDS`] on a count that nothing in it can lower and dies of
    /// the signal without having exec'ed, and until it has, the gate's own
    /// `spawn` has not returned, so the gate waits as long. Measured
    /// 2026-10-13 on a stand-in gate starting `sleep` by that name from
    /// four threads: none of 480 interrupted through their group left a
    /// child (52 of 100 did before the count), and 57 of them took up to
    /// 1.2 s to die where the rest took milliseconds. Every child the
    /// ladder starts is named by a path (read the same day), which std
    /// starts with `posix_spawn` on macOS, so the gate has no such copy.
    pub struct Spawning(());

    impl Spawning {
        /// Count one spawn in flight. Call it before asking [`stopping`]:
        /// the order is the whole argument.
        #[must_use]
        pub fn begin() -> Self {
            SPAWNING.fetch_add(1, Ordering::SeqCst);
            Self(())
        }
    }

    impl Drop for Spawning {
        fn drop(&mut self) {
            SPAWNING.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// A stage child's process group, HELD: its place on the live list for as
    /// long as this value lives, and the only handle through which the runner
    /// signals that group or reaps that child.
    ///
    /// THE RULE. The gate never signals a process-group id unless it holds
    /// that group's leader as its own child, running or exited and not yet
    /// reaped, at that moment. A group's id is its leader's pid. While the
    /// leader is the gate's child the number is taken: macOS hands out no id
    /// that is in use as a pid or as a group's (`forkproc`, read in
    /// xnu-12377; an unreaped leader's id was never handed out over a full
    /// wrap of the pid counter, 99,184 forks, measured 2026-10-13), and
    /// Linux frees a number only when nothing uses it as either (read in
    /// v6.12, not run). Once the leader is reaped the number is anybody's,
    /// the next process's that the kernel's counter brings to it. Measured
    /// the same day, by a probe that aimed for one: a process that was not
    /// the gate's led a new group with a reaped leader's id 2.7 ms after
    /// the reap, and this module as it then was `SIGKILL`ed it, 10.6 s
    /// after the interrupt, with the kill an interrupted gate sends a
    /// graced group once the grace is over. The premise is a child that
    /// stays a zombie until its parent reaps it. The kernel reaps a
    /// process's children for it only once that process has itself set
    /// `SIGCHLD` to be ignored, which nothing in this crate does; an ignore
    /// the gate merely inherited keeps its zombies as the default does
    /// (measured 2026-10-13 on macOS 26.6: a probe that inherited the
    /// ignore across `exec`, either way of handing it over, kept its exited
    /// child a zombie until it reaped it, and a stand-in gate started with
    /// `SIGCHLD` ignored kept its killed leader a zombie and died of its
    /// interrupt as one started without).
    ///
    /// ORDINARY CODE keeps the rule by construction. [`Live::term`],
    /// [`Live::alive`], [`Live::gone`] and [`Live::kill`] are the only ways
    /// to signal a group from outside the handler; each refuses once
    /// [`Live::reap`] has reaped the child, and the thread that owns this
    /// value is the only one that reaps it. (A test build asks the kernel
    /// all the same, before every signal [`send`] sends: a child reaped
    /// behind this value's back, by std's `wait`, fails the test it is in.)
    ///
    /// THE HANDLER ([`on_signal`]) cannot ask anyone. It reads group ids off
    /// [`LIVE`] and goes on signalling them for up to twelve seconds, on a
    /// thread the kernel chose, while the stage threads run. So the rule is
    /// kept from this side, at the one door a stage child is reaped through,
    /// [`Live::reap`]:
    ///
    /// 1. see that the child has exited, without reaping it ([`exited`]);
    /// 2. take the group off the list;
    /// 3. read [`STOPPING`];
    /// 4. reap, if that read found no interrupt. If it found one the child
    ///    is never reaped: it stays a zombie, and its id the gate's, for as
    ///    long as the gate lives, which is until the handler has finished.
    ///    The gate is dying of its signal, and its zombies go with it.
    ///
    /// WHY THAT MISSES NO CASE. The handler claims `STOPPING`, a store,
    /// before it reads the list, and every id it signals is one it read off
    /// the list after that claim: at once, or later from its own record of
    /// such a read. The thread empties the slot (2), a store, and then
    /// reads the flag (3). All of it is `SeqCst`, so those steps have one
    /// order, and the thread's read of the flag falls on one side of the
    /// handler's claim:
    ///
    /// - after it: the thread reads the gate stopping and does not reap.
    ///   Whether or not the handler read this group off the list, its
    ///   leader stays the gate's child for the rest of the gate's life;
    /// - before it: then the slot was emptied before the claim too, and so
    ///   before every read the handler makes of the list. The handler never
    ///   reads this group's id, keeps no record of it and never signals it,
    ///   and the thread may reap. (The slot may be taken again, by another
    ///   child's group, whose own leader is held by this same rule.)
    ///
    /// A look at the flag with the reap after it, and the slot emptied last,
    /// is not enough: the handler can claim, read the list and remember the
    /// group between the look and the reap. A stage thread reaps every 1 to
    /// 25 ms; the handler remembers for seconds.
    ///
    /// Nothing else in the gate reaps a stage child: no call here waits for
    /// "any child", std reaps only a `Child` it is asked to wait for, and
    /// one that went through this door is never asked ([`super::reap`]).
    ///
    /// WHAT IT CHANGES for a stage. A child that exited is asked after twice,
    /// once without reaping and once to reap, where one call did both: a
    /// system call more for each stage child, and nothing that shows.
    /// Measured 2026-10-13 at a load of 9 on 18 cores: either call took
    /// 0.11 microseconds on a running child, and a run of `/usr/bin/true`
    /// took what it had before, 0.85 ms waited for and 1.38 ms polled under
    /// a ceiling, at the median of 500 either way. And the stage of a child
    /// left unreaped is told that the gate was interrupted ([`KEPT`]), an
    /// environment failure like the one a stage gets that asks for a child
    /// once the gate is stopping, where it used to be handed the status of a
    /// child the handler had just killed. The thread goes on from there as
    /// before; it is not parked, so it holds nothing anyone waits for.
    ///
    /// ON LINUX (read in v6.12, not run) a group's unreaped leader answers
    /// `killpg`, signal 0 included, so a group whose leader is kept reads as
    /// alive. An interrupted gate there waits out its whole grace for a
    /// suite that asked for one, unless a second interrupt ends the wait;
    /// before, it stopped waiting once a stage thread had reaped the suite.
    ///
    /// ONLY WHERE `waitid` WAS READ ([`HOLDS`]): macOS and Linux. On any
    /// other Unix a child's exit can be learned only by reaping it
    /// ([`Live::exited`]), with nothing to stop that reaping a child whose
    /// group a handler remembered. So no handler is installed there
    /// ([`kill_on_interrupt`]): an interrupted gate dies of its signal at
    /// once and its stages' groups run on, as they did before the gate
    /// forwarded an interrupt at all. Ordinary code keeps the rule there
    /// as everywhere: nothing is sent a group once its leader is reaped.
    pub struct Live {
        /// The group's place on [`LIVE`], until it has left the list.
        slot: Option<usize>,
        /// The child's pid, which is its group's id, for as long as this
        /// thread may still hold the child unreaped; `0` once it has reaped
        /// it (or never had an id it could signal).
        group: i32,
        /// What reaping the child answered, where asking after its exit is
        /// that reap ([`Live::exited`]), kept for [`Live::reap`] to hand on.
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        reaped: Option<(std::process::ExitStatus, std::time::Duration)>,
    }

    impl Live {
        /// Put `pgid` — the child's pid, which leads its group — on the list;
        /// `graceful` when the child asked for a `SIGTERM` grace. Call it
        /// with the child unreaped, which a child just spawned is.
        #[must_use]
        pub fn enter(pgid: u32, graceful: bool) -> Self {
            // A pid that is no group id this can signal (none is: a pid fits)
            // is never listed and never signalled.
            let g = i32::try_from(pgid).unwrap_or(0);
            let entry = if graceful { -g } else { g };
            Self {
                slot: LIVE.iter().position(|slot| {
                    g > 0
                        && slot
                            .compare_exchange(0, entry, Ordering::SeqCst, Ordering::SeqCst)
                            .is_ok()
                }),
                group: g,
                #[cfg(not(any(target_os = "macos", target_os = "linux")))]
                reaped: None,
            }
        }

        /// Take the group off the list, once.
        fn leave(&mut self) {
            if let Some(i) = self.slot.take() {
                LIVE[i].store(0, Ordering::SeqCst);
            }
        }

        /// The group's id, while this thread has not reaped its leader.
        ///
        /// # Errors
        /// Once it has: the number is no longer the gate's to signal.
        fn held(&self) -> std::io::Result<i32> {
            if self.group > 0 {
                Ok(self.group)
            } else {
                Err(std::io::Error::other(
                    "the child was reaped, so its group's id is no longer the gate's to signal",
                ))
            }
        }

        /// `SIGTERM` every process in the group.
        ///
        /// # Errors
        /// The OS's reason, when no process was signalled; a child already
        /// reaped ([`Live::held`]).
        pub fn term(&self) -> std::io::Result<()> {
            // Held: this thread has not reaped the leader (`held`).
            signal_group(self.held()?, SIGTERM)
        }

        /// Does the group still hold a process a signal would reach?
        /// `killpg(g, 0)`, which delivers nothing. On macOS a group whose
        /// only member is its unreaped, exited leader answers `EPERM`, so
        /// this reads it as empty without reaping the leader, whose id (and
        /// so the group's) stays the gate's. Never, once the child is
        /// reaped: there is no group of the gate's to ask after.
        #[must_use]
        pub fn alive(&self) -> bool {
            // Held: this thread has not reaped the leader (`held`).
            self.held().is_ok_and(|g| signal_group(g, 0).is_ok())
        }

        /// Is the group empty, to stay? One empty reading ([`Live::alive`])
        /// is not that on macOS: a group reads as empty while a process
        /// that has begun to exit is still starting a child from another of
        /// its threads ([`KILL_ROUNDS`]). So the group is asked again, a
        /// millisecond apart, and is gone when [`QUIET_ROUNDS`] rounds
        /// running have found nobody. The first that finds a process
        /// answers no: the caller's `SIGKILL` is for that process. (Where
        /// nothing is repeated, [`REPEATS`], it is the one reading.) It is
        /// what a grace that worked costs now: a child that died of its
        /// `SIGTERM` at once was reported 31 ms after its ceiling before
        /// and is 80 ms after it now (the median of 40, measured
        /// 2026-10-13).
        #[must_use]
        pub fn gone(&self) -> bool {
            let Ok(g) = self.held() else {
                return true;
            };
            // Held, for every round: this thread has not reaped the leader.
            if signal_group(g, 0).is_ok() {
                return false;
            }
            if REPEATS {
                for _ in 0..QUIET_ROUNDS {
                    nap(1);
                    if found(g, 0) {
                        return false;
                    }
                }
            }
            true
        }

        /// `SIGKILL` every process in the group, and again, a millisecond
        /// apart, until the group has stayed empty a while ([`rekill`]).
        /// `Ok(true)` when it had not: the last of those kills still found a
        /// process. The rounds follow whatever the first kill answered: a
        /// leader that began to exit just before it reads as nobody, and can
        /// be starting a child ([`KILL_ROUNDS`]).
        ///
        /// # Errors
        /// The OS's reason, when the first found no process to signal and no
        /// round after it ended with one; a child already reaped
        /// ([`Live::held`]), in which case nothing is sent.
        pub fn kill(&self) -> std::io::Result<bool> {
            // Held, for the first kill and every round after it: the caller
            // reaps through `reap` only, and has not yet (`held`).
            let g = self.held()?;
            let first = signal_group(g, SIGKILL);
            if rekill(&[g]) {
                Ok(true)
            } else {
                first.map(|()| false)
            }
        }

        /// Has the child exited? Asked without reaping it, so its group can
        /// still be signalled whatever the answer (`block`: wait until it
        /// has). For a caller with something to do between the child's exit
        /// and its reap ([`crate::differential`]'s bounded git call), and
        /// the first step of [`Live::reap`].
        ///
        /// # Errors
        /// The OS's, after which the child is taken for reaped: a kernel that
        /// will not say whether it is this process's child (`ECHILD`) is not
        /// asked to signal on the strength of its number. A child already
        /// reaped ([`Live::held`]).
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        pub fn exited(&mut self, block: bool) -> std::io::Result<bool> {
            let seen = exited(self.held()?, block);
            if seen.is_err() {
                self.leave();
                self.group = 0;
            }
            seen
        }

        /// Has the child exited? Off macOS and Linux this module has no way
        /// to ask without reaping ([`HOLDS`]), so this reaps: once it has
        /// answered yes the group's id is the gate's no longer, nothing more
        /// is sent it ([`Live::held`]), and [`Live::reap`] hands on the
        /// status kept here.
        ///
        /// # Errors
        /// The OS's, or a child already reaped and handed on.
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        pub fn exited(&mut self, block: bool) -> std::io::Result<bool> {
            if self.reaped.is_none() {
                let reaped = wait_rusage(self.held()?.unsigned_abs(), block);
                if !matches!(reaped, Ok(None)) {
                    self.leave();
                    self.group = 0;
                }
                self.reaped = reaped?;
            }
            Ok(self.reaped.is_some())
        }

        /// THE ONE DOOR a stage child is reaped through ([`Live`] has the
        /// rule this keeps, and the argument). `Ok(None)` while the child
        /// runs (never when `block`ing), else its status and the user plus
        /// system CPU of it and the descendants it reaped
        /// (`wait_rusage`).
        ///
        /// # Errors
        /// [`KEPT`], when the child has exited and an interrupt has begun
        /// ending the gate: it is left unreaped, for good. The OS's own,
        /// from either wait. A child already reaped ([`Live::held`]).
        pub fn reap(
            &mut self,
            block: bool,
        ) -> std::io::Result<Option<(std::process::ExitStatus, std::time::Duration)>> {
            if !self.exited(block)? {
                return Ok(None);
            }
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            {
                let pid = self.held()?;
                // A test's hold with the exit seen and the group still listed.
                #[cfg(test)]
                super::tests::at_the_door(false);
                self.leave();
                // ...and with the group off the list and the flag not yet read.
                #[cfg(test)]
                super::tests::at_the_door(true);
                if stopping() {
                    return Err(std::io::Error::other(KEPT));
                }
                // Reaped, or lost to an error: either way the number is no
                // longer known to be the gate's, and nothing more is sent it.
                self.group = 0;
                wait_rusage(pid.unsigned_abs(), true)
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            {
                Ok(self.reaped.take())
            }
        }
    }

    impl Drop for Live {
        fn drop(&mut self) {
            self.leave();
        }
    }

    /// Send group `g` signal `sig` (`0` asks and delivers nothing): did it
    /// find a process? The one place this module calls `killpg`, so every
    /// caller owes [`Live`]'s rule at its own call: `g`'s leader is the
    /// gate's child, running or unreaped, right now. Async-signal-safe
    /// ([`on_signal`] calls it): `killpg`, and nothing else outside a test
    /// build.
    fn send(g: i32, sig: i32) -> bool {
        // A test build checks the rule here, before the signal goes out.
        #[cfg(test)]
        super::tests::signalling(g, sig);
        // SAFETY: `killpg` takes two integers and touches no memory of ours;
        // it is async-signal-safe.
        unsafe { killpg(g, sig) == 0 }
    }

    /// [`send`], with the OS's reason when it found no process.
    fn signal_group(g: i32, sig: i32) -> std::io::Result<()> {
        if send(g, sig) {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    /// One more signal for group `g`, a `SIGKILL` or the question (`0`): did
    /// it find a process? The caller holds `g`'s leader ([`send`]).
    fn found(g: i32, sig: i32) -> bool {
        let found = send(g, sig);
        // What a test has the group answer, in place of what it did.
        #[cfg(test)]
        if let Some(scripted) = super::tests::answer_of(g, sig) {
            return scripted;
        }
        found
    }

    /// Send every group in `groups`, each already sent one `SIGKILL`, another
    /// a millisecond later, and another, until [`QUIET_ROUNDS`] rounds in a
    /// row have found no process in any of them or [`KILL_ROUNDS`] rounds
    /// have passed. Answers whether the last round still found one. The
    /// caller holds every group's leader for all of it ([`send`]).
    ///
    /// Async-signal-safe ([`on_signal`] calls it): `killpg` and [`nap`], and
    /// two counters. It ends after its fixed number of rounds whatever those
    /// two calls answer.
    pub(super) fn rekill(groups: &[i32]) -> bool {
        let (mut quiet, mut found_one) = (0_u16, false);
        if !REPEATS || groups.is_empty() {
            return false;
        }
        for _ in 0..KILL_ROUNDS {
            nap(1);
            found_one = false;
            for &g in groups {
                found_one |= found(g, SIGKILL);
            }
            quiet = if found_one { 0 } else { quiet + 1 };
            if quiet == QUIET_ROUNDS {
                break;
            }
        }
        found_one
    }

    /// Add `g` to the `n` groups in `set`, once. A full set takes no more.
    fn remember(set: &mut [i32], n: &mut usize, g: i32) {
        if *n < set.len() && !set[..*n].contains(&g) {
            set[*n] = g;
            *n += 1;
        }
    }

    /// Has child `pid` exited? `waitid(2)` with `WNOWAIT`, which answers
    /// and leaves the child as it was: a zombie still, its pid still the
    /// gate's ([`Live`]). `block` waits until it has; otherwise `WNOHANG`.
    /// Retried on `EINTR`. Not for the handler: `waitid` is not among the
    /// calls POSIX promises a handler, and only stage threads ask.
    ///
    /// The numbers are macOS's (`sys/wait.h`, macOS 26.6) and Linux's
    /// (`include/uapi/linux/wait.h`, v6.12, the same on every
    /// architecture): `P_PID` 1, `WNOHANG` 1 and `WEXITED` 4 on both, and
    /// `WNOWAIT` each its own. The struct is zeroed first, and neither
    /// kernel writes it when `WNOHANG` finds no child to report (XNU's
    /// `waitid_nocancel` leaves it untouched, Linux zeroes it), so a
    /// `si_signo` that is still zero is a child still running.
    ///
    /// A `si_signo` that is not zero (it is `SIGCHLD`) is an exit on
    /// Linux, which reports to a caller that asked for exits nothing else
    /// (`wait_task_stopped`, read in v6.12, not run; a child the caller
    /// traces aside, and the gate traces none). ON macOS IT IS NOT. Asked
    /// for exits alone, macOS 26.6's `waitid` answers for a child that is
    /// only stopped as well, with `si_code` saying so (`CLD_STOPPED`;
    /// measured 2026-10-13, 2 of 2 by hand and then by this module's own
    /// test of it, against the XNU source, which reads as if only a caller
    /// that asked for stopped children were told). A stage's
    /// `ssh` asking for a passphrase is stopped by the terminal, and one
    /// taken for exited would leave the live list with the child still
    /// there. So on macOS the exit is read from `si_code`: `CLD_EXITED`,
    /// `CLD_KILLED` or `CLD_DUMPED` (1, 2, 3 in `sys/signal.h`), which is
    /// every way a process ends.
    ///
    /// That makes a blocking ask return at once, with no exit to show, for
    /// as long as a child is stopped. It is asked again after a nap that
    /// doubles from 1 ms to 25 ms, the poll a child under a ceiling gets
    /// anyway ([`super::finish`]), so a stopped child costs forty calls a
    /// second and never a spin.
    ///
    /// # Errors
    /// The OS's: `ECHILD` for a pid that is not this process's child.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub(super) fn exited(pid: i32, block: bool) -> std::io::Result<bool> {
        const P_PID: i32 = 1;
        const WNOHANG: i32 = 1;
        const WEXITED: i32 = 4;
        #[cfg(target_os = "macos")]
        const WNOWAIT: i32 = 0x20;
        #[cfg(target_os = "linux")]
        const WNOWAIT: i32 = 0x0100_0000;
        const EINTR: i32 = 4;
        let options = WEXITED | WNOWAIT | if block { 0 } else { WNOHANG };
        let mut nap = std::time::Duration::from_millis(1);
        loop {
            let mut info = Siginfo {
                si_signo: 0,
                _si_errno: 0,
                si_code: 0,
                _rest: [0; 29],
            };
            // SAFETY: `info` is live, writable, and at least as large and as
            // aligned as either kernel's `siginfo_t` (`Siginfo`).
            let got = unsafe { waitid(P_PID, pid.unsigned_abs(), &raw mut info, options) };
            if got == 0 {
                // `CLD_EXITED`, `CLD_KILLED`, `CLD_DUMPED`.
                #[cfg(target_os = "macos")]
                let ended = info.si_signo != 0 && (1..=3).contains(&info.si_code);
                #[cfg(target_os = "linux")]
                let ended = info.si_signo != 0;
                if ended || !block {
                    return Ok(ended);
                }
                // Asked to wait, and answered with no exit: a stopped child,
                // on macOS. Wait a little and ask again.
                std::thread::sleep(nap);
                nap = (nap * 2).min(std::time::Duration::from_millis(25));
                continue;
            }
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() != Some(EINTR) {
                return Err(e);
            }
        }
    }

    /// `wait4(2)` on `pid` ([`super::reap`]): `Ok(None)` while it runs
    /// (`WNOHANG` unless `block`), else its status and the user plus system
    /// CPU of it and the descendants it reaped. Retried on `EINTR`.
    ///
    /// Private to this module: it reaps by pid, and a child whose group the
    /// gate signals is reaped through [`Live`] only, which sends that group
    /// nothing once it has.
    fn wait_rusage(
        pid: u32,
        block: bool,
    ) -> std::io::Result<Option<(std::process::ExitStatus, std::time::Duration)>> {
        use std::os::unix::process::ExitStatusExt as _;
        const WNOHANG: i32 = 1;
        const EINTR: i32 = 4;
        let pid = i32::try_from(pid).map_err(|_| std::io::Error::other("pid out of range"))?;
        let since = |t: &Timeval| {
            std::time::Duration::from_secs(u64::try_from(t.tv_sec).unwrap_or(0))
                + std::time::Duration::from_micros(u64::try_from(t.tv_usec).unwrap_or(0))
        };
        loop {
            let mut status = 0_i32;
            let mut ru = Rusage::default();
            // SAFETY: `status` and `ru` are live, writable, and laid out as
            // `wait4` writes them (see `Rusage`).
            let got = unsafe {
                wait4(
                    pid,
                    &raw mut status,
                    if block { 0 } else { WNOHANG },
                    &raw mut ru,
                )
            };
            if got == 0 {
                return Ok(None);
            }
            if got < 0 {
                let e = std::io::Error::last_os_error();
                if e.raw_os_error() == Some(EINTR) {
                    continue;
                }
                return Err(e);
            }
            return Ok(Some((
                std::process::ExitStatus::from_raw(status),
                since(&ru.utime) + since(&ru.stime),
            )));
        }
    }

    /// Has an interrupt begun ending the gate?
    #[must_use]
    pub fn stopping() -> bool {
        STOPPING.load(Ordering::SeqCst) != 0
    }

    /// One round of [`on_signal`]'s wait on the groups it sent `SIGTERM`, in
    /// milliseconds. The wait counts rounds, not time, and a later
    /// interrupt's frame that returns into a round's [`nap`] ends that nap.
    /// So a second interrupt taken on the waiting thread ends the grace there
    /// and then, one taken on another thread within a round, and a repeated
    /// hangup, which ends no grace, costs the grace at most a round.
    pub(crate) const NAP_MS: u16 = 50;

    /// Sleep `ms` milliseconds, async-signal-safely, or until a signal's
    /// handler returns into the sleep. What is left of a nap cut short is
    /// not slept: asking `nanosleep` for it (a `rem`) sends macOS's to the
    /// clock service, a Mach message from handler context whose error path
    /// formats to stderr (read in `libsystem_c`, macOS 26.6, 2026-10-13), and
    /// the handler has made this call with a null `rem` since its grace was
    /// added.
    fn nap(ms: c_long) {
        let t = Timespec {
            tv_sec: 0,
            tv_nsec: ms * 1_000_000,
        };
        // SAFETY: `nanosleep` reads the struct we own and writes nothing when
        // `rem` is null; it is async-signal-safe.
        unsafe {
            nanosleep(&raw const t, std::ptr::null_mut());
        }
    }

    /// The handler [`kill_on_interrupt`] installs for `SIGHUP`, `SIGINT` and
    /// `SIGTERM`.
    ///
    /// ONE FRAME DOES THE WORK. The first interrupt claims [`STOPPING`] and
    /// runs the five phases below to the end, on the thread the kernel
    /// handed it. Every later interrupt, of any of the three, stores its
    /// number in [`AGAIN`] at most and returns. Nested in the first frame or
    /// on another thread, it is the same frame: it touches two atomics and
    /// calls nothing, so `errno` and the code it interrupted are as they
    /// were. That is all the re-entrancy there is.
    ///
    /// A SECOND INTERRUPT IS THE OPERATOR SAYING "NOW". The first frame reads
    /// [`AGAIN`] each time round its wait, so the grace ends within one round
    /// ([`NAP_MS`]) of a second interrupt, at once when its frame nests in
    /// the waiting one, and the `SIGKILL`s follow. The frame that waits is
    /// the frame that sent the `SIGTERM`s, so it still knows every group it
    /// sent one, a group whose leader has since exited and is off [`LIVE`]
    /// among them (that leader is left unreaped, so the id is still the
    /// gate's: EVERY ID IT SIGNALS, below). No group with a member left
    /// outlives the gate, whichever signal ends the wait and whenever.
    ///
    /// A HANGUP TOLD TWICE IS ONE HANGUP. A `SIGHUP` after a first `SIGHUP`
    /// is not a second interrupt and ends no grace. A closing terminal hangs
    /// its foreground job up more than once: the kernel hangs up the
    /// foreground group, and the shell its jobs (measured 2026-10-12, zsh 5.9
    /// on macOS 26.6: two `SIGHUP`s 0.3 to 0.5 ms apart, 6 of 6, and one
    /// under `setopt nohup`). Nobody at a keyboard said "now", and a closing
    /// terminal is what the grace is for: taken for a second interrupt, that
    /// `SIGHUP` ends it within a nap, and a suite's EXIT trap needs more than
    /// one (0.03 to 1.2 s, measured when the grace was added). An operator in
    /// a hurry has `SIGINT` and `SIGTERM`. The repeat still ends the nap it
    /// lands in, which is one round off that grace ([`NAP_MS`]), not its end.
    /// The rounds add up, because the wait counts them: 200 repeats, one to
    /// a round, are a whole grace (measured 2026-10-13: 200 of them 3 ms
    /// apart ended a grace in 0.6 s, 100 of them 64 ms apart in 6.8 s, and 2
    /// took nothing measurable off its 10.7 s). A closing terminal sends
    /// two.
    ///
    /// A CHILD BEING SPAWNED IS WAITED FOR AND KILLED. A stage thread that
    /// read the gate running just before this frame claimed [`STOPPING`] can
    /// have a child the list does not hold yet ([`Spawning`] has the
    /// argument, and what it leaves). So after its `SIGKILL`s the frame
    /// waits while any spawn is in flight, [`SPAWN_ROUNDS`] rounds at most,
    /// and then `SIGKILL`s whatever has reached the list. A second interrupt
    /// does not end this wait, though like any later signal it ends the
    /// round it lands in. The kills the operator asked for are done by
    /// then, the wait is over in the time one spawn takes, and it is all
    /// that stands between "now" and a child left running. A group that
    /// asked for a grace gets none here: its child was born as the gate was
    /// interrupted and has barely started, and phase 3 has always
    /// `SIGKILL`ed a child that started during the grace.
    ///
    /// EVERY GROUP IT KILLED IS KILLED AGAIN. A `SIGKILL` does not reach a
    /// process its group was making at that instant ([`KILL_ROUNDS`]), and
    /// a gate that died straight after its kills left that process running.
    /// The frame keeps every group it sends `SIGKILL`, one whose leader has
    /// since exited and left [`LIVE`] among them, and its last
    /// phase sends each of them `SIGKILL` again, a millisecond apart, until
    /// [`QUIET_ROUNDS`] rounds running have found nobody in any of them
    /// ([`rekill`]). That is what an interrupt costs now when every group
    /// died at once: 50 ms, measured on a stand-in gate with one group,
    /// which used to be gone 0.3 ms after its signal. [`KILL_ROUNDS`]
    /// rounds, a third of a second, is the most it can cost, and a frame
    /// that killed nothing sleeps no round. The record holds 256 groups, as
    /// the live list does, and a full one takes no more. Like the wait
    /// before it, this one does not end on a second interrupt, which ends
    /// the round it lands in: the rounds are counted, so a signal that cuts
    /// one short takes a millisecond off the quiet it asks for. The kills
    /// are `SIGKILL` only: no group is sent a second `SIGTERM` here either.
    ///
    /// EVERY ID IT SIGNALS IS STILL THE GATE'S. A group's id is its
    /// leader's pid, and the kernel hands a reaped leader's on to the next
    /// process to reach it: within 3 ms, for a probe that aimed for one
    /// ([`Live`]).
    /// This frame signals ids for as long as it runs, up to twelve seconds
    /// after it read them, so each must still be the gate's when the signal
    /// goes. It is: every id here was read off [`LIVE`] after this frame
    /// claimed [`STOPPING`], at once or from the frame's own record of such
    /// a read (`termed`, `killed`), and a stage thread never reaps a child
    /// whose group could have been read so ([`Live::reap`], which has the
    /// argument). The leader is running or a zombie, and the gate's child,
    /// until the gate itself is gone. Until 2026-10-13 a stage thread
    /// reaped at once, and phase 3's kill of a graced group, sent when the
    /// grace was over, could land on a group the gate had never started:
    /// measured that day on a stand-in gate, it did.
    ///
    /// NOTHING IS BLOCKED WHILE IT RUNS ([`catch`]: an empty mask and
    /// `SA_NODEFER`), because an interrupt held back is one the wait never
    /// hears of. Until 2026-10-12 `signal(3)` installed this, which holds back
    /// the signal being handled until its handler returns, and a frame that
    /// was not the first did the `SIGKILL`s itself, from [`LIVE`] alone, then
    /// ended the gate. Measured then, with a graced group that kept a
    /// survivor: a second Ctrl-C skipped nothing on a stand-in gate down to
    /// one thread (it died 10.7 s after the first, as it did without one),
    /// and on a gate whose stage threads were alive it ended the gate at once
    /// and left the survivor running, as a `SIGTERM` or `SIGHUP` during the
    /// grace did on either (3 of 3). A closing terminal ended the gate within
    /// 0.01 s (11 closes, 4 of them of the gate itself), in 10 of them before
    /// its suite's EXIT trap, one `touch`, had run.
    ///
    /// Async-signal-safe throughout: atomic loads and stores, `killpg`
    /// ([`send`]), `nanosleep`, `signal` and `raise`, the four calls this
    /// handler has made since its grace was added, in the forms it made
    /// them then (`nanosleep` with no `rem`: [`nap`]); no allocation, no
    /// lock, no formatting. What those calls answer is read, never trusted to
    /// succeed: `killpg` failing (`ESRCH`, `EPERM`) is a group with nobody
    /// left to signal, and a `nanosleep` that a later interrupt cut short, or
    /// that would not sleep, is one round of its wait all the same, so each
    /// of the three waits ends after its fixed number of rounds whatever
    /// they answered.
    extern "C" fn on_signal(sig: i32) {
        if let Err(first) = STOPPING.compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst) {
            if !(sig == SIGHUP && first == SIGHUP) {
                AGAIN.store(sig, Ordering::SeqCst);
            }
            return;
        }
        // Phase 1: SIGKILL every plain group at once; SIGTERM every group that
        // asked for a grace and remember which, so none is sent a second SIGTERM
        // (bash would take it in the middle of its EXIT trap).
        let mut termed = [0_i32; 256];
        let mut n = 0;
        // Every group this frame sends SIGKILL, for phase 5.
        let mut killed = [0_i32; 256];
        let mut k = 0;
        // Held, here and at every signal below: each id is read off the list
        // after the claim above, so its leader is never reaped again
        // (`Live::reap`), and is the gate's child for as long as this runs.
        for slot in &LIVE {
            let v = slot.load(Ordering::SeqCst);
            if v > 0 {
                send(v, SIGKILL);
                remember(&mut killed, &mut k, v);
            } else if v < 0 {
                send(-v, SIGTERM);
                termed[n] = -v;
                n += 1;
            }
        }
        // Phase 2: wait for those groups to empty, at most TERM_GRACE and only
        // until a second interrupt.
        let grace_ms = c_long::try_from(super::TERM_GRACE.as_millis()).unwrap_or(10_000);
        let round = c_long::from(NAP_MS);
        let mut waited: c_long = 0;
        // Held: `termed` is this frame's record of ids read off the list
        // after the claim, so the question is still asked of the gate's own
        // groups however long the wait has run. Signal 0 delivers nothing.
        while n > 0
            && waited < grace_ms
            && AGAIN.load(Ordering::SeqCst) == 0
            && termed[..n].iter().any(|&g| send(g, 0))
        {
            nap(round);
            waited += round;
        }
        // Phase 3: SIGKILL whatever is still there — the graced groups that
        // outlived their grace, and any child that started in the meantime.
        //
        // Held: a group in `termed` may have left the list seconds ago, its
        // leader long exited. That leader is a zombie the gate keeps: its
        // stage thread found the gate stopping and did not reap it.
        for g in &termed[..n] {
            send(*g, SIGKILL);
            remember(&mut killed, &mut k, *g);
        }
        // Held: read off the list now, after the claim.
        for slot in &LIVE {
            let v = slot.load(Ordering::SeqCst);
            if v != 0 {
                send(v.abs(), SIGKILL);
                remember(&mut killed, &mut k, v.abs());
            }
        }
        // Phase 4: the children that were being spawned when this frame
        // claimed STOPPING. Wait until no spawn is in flight, a bounded number
        // of rounds, then SIGKILL whatever has reached the list since
        // (`Spawning` says why that is every such child, and what is left).
        let mut rounds = 0_u16;
        while SPAWNING.load(Ordering::SeqCst) != 0 && rounds < SPAWN_ROUNDS {
            nap(1);
            rounds += 1;
        }
        // Held: read off the list now, after the claim.
        for slot in &LIVE {
            let v = slot.load(Ordering::SeqCst);
            if v != 0 {
                send(v.abs(), SIGKILL);
                remember(&mut killed, &mut k, v.abs());
            }
        }
        // Phase 5: a SIGKILL does not reach a process its group was making
        // at that instant, so every group killed above is killed again, a
        // millisecond apart, until none of them has held a process for a
        // while (`rekill` has the rule, `KILL_ROUNDS` what was measured).
        //
        // Held: `killed` is this frame's record of ids read off the list
        // after the claim, like `termed`.
        rekill(&killed[..k]);
        // Die of the last interrupt: the second when one came (which is what
        // the gate died of before, whenever a second one ended it), else this
        // frame's own. Restoring the default action and raising ends the
        // process inside `raise`. `raise` returns only for a signal this
        // thread blocks; `last` reached the process, perhaps on another
        // thread, and `sig` was handed to this one, so it is the fallback.
        let last = AGAIN.load(Ordering::SeqCst);
        // SAFETY: both are async-signal-safe and take integers only.
        unsafe {
            if last != 0 {
                signal(last, SIG_DFL);
                raise(last);
            }
            signal(sig, SIG_DFL);
            raise(sig);
        }
    }

    /// `SIGINT` back at its default action, for a test's stand-in gate: a process
    /// started as a background job with job control off inherits `SIGINT`
    /// ignored (POSIX), and a stand-in that ignores the interrupt measures
    /// nothing.
    #[cfg(test)]
    pub(crate) fn default_interrupt() {
        // SAFETY: `signal` takes an integer and the `SIG_DFL` value and
        // touches no memory of ours.
        unsafe {
            signal(SIGINT, SIG_DFL);
        }
    }

    /// On `SIGINT`, `SIGTERM` or `SIGHUP`, end every live stage child's group,
    /// then die of the signal: `SIGKILL` at once, except a group whose child
    /// asked for a `SIGTERM` grace ([`crate::exec::Cmd::term_grace`]), which is
    /// sent `SIGTERM` and given up to [`super::TERM_GRACE`] to exit — its EXIT
    /// traps run — before the `SIGKILL`. A second interrupt during that wait
    /// ends it at once ([`on_signal`]). A child that was being spawned as
    /// the interrupt landed is waited for and `SIGKILL`ed too ([`Spawning`]),
    /// and every group it killed is killed again until it has stayed empty
    /// a while ([`KILL_ROUNDS`]).
    /// Idempotent: it installs the same handler.
    ///
    /// Only where a stage child's exit can be seen without reaping it
    /// ([`HOLDS`]: macOS and Linux). The handler signals group ids it read
    /// seconds earlier, which is right only while the gate still holds each
    /// group's leader ([`Live`]), and nowhere else is there a way to keep a
    /// stage thread from reaping one in the meantime. On any other Unix
    /// this installs nothing: an interrupt ends the gate at once and its
    /// stages' groups run on, which a signal sent to a group that is no
    /// longer the gate's would be worse than.
    pub fn kill_on_interrupt() {
        if !HOLDS {
            return;
        }
        for sig in [SIGHUP, SIGINT, SIGTERM] {
            catch(sig);
        }
    }

    /// Install [`on_signal`] for `sig` with `sigaction(2)`: an empty mask and
    /// `SA_NODEFER`, so no signal, `sig` included, is blocked while the
    /// handler runs and a second interrupt always reaches it; and
    /// `SA_RESTART`, which `signal(3)` set too, so a slow system call that a
    /// later interrupt's frame returns into resumes instead of failing with
    /// `EINTR`. The system has no reason to refuse (the signal can be caught
    /// and the struct is ours); if it does, `signal(3)` installs the handler,
    /// under which a first interrupt is still forwarded.
    ///
    /// Only where `struct sigaction` and the two flags' numbers were read,
    /// and are declared in here: macOS, and 64-bit Linux under glibc or musl
    /// on x86_64 and arm64. They are not one layout (in the libc crate
    /// 0.2.190: glibc's mips puts the flags first and its s390 the mask last,
    /// its sparc numbers `SA_RESTART` 2, `uclibc`'s x86_64 puts the flags
    /// second), and a struct laid out wrong does not fail: the system reads a
    /// handler and a mask out of other fields and answers success. Every
    /// other Unix gets the arm below.
    #[cfg(any(
        target_os = "macos",
        all(
            target_os = "linux",
            target_pointer_width = "64",
            any(target_arch = "x86_64", target_arch = "aarch64"),
            any(target_env = "gnu", target_env = "musl")
        )
    ))]
    fn catch(sig: i32) {
        /// `struct sigaction` as `sigaction(2)` takes it: the handler, the
        /// signals blocked while it runs, the flags. macOS keeps the mask in
        /// 32 bits. Linux keeps 1024 and ends in a trampoline slot the C
        /// library fills itself. All-zero is the empty mask on each.
        #[cfg(target_os = "macos")]
        #[repr(C)]
        struct Sigaction {
            handler: usize,
            mask: u32,
            flags: i32,
        }

        #[cfg(target_os = "linux")]
        #[repr(C)]
        struct Sigaction {
            handler: usize,
            mask: [u64; 16],
            flags: i32,
            restorer: usize,
        }

        /// `SA_RESTART | SA_NODEFER`.
        #[cfg(target_os = "macos")]
        const RESTART_NODEFER: i32 = 0x0002 | 0x0010;

        #[cfg(target_os = "linux")]
        const RESTART_NODEFER: i32 = 0x1000_0000 | 0x4000_0000;

        unsafe extern "C" {
            fn sigaction(sig: i32, act: *const Sigaction, old: *mut Sigaction) -> i32;
        }

        let handler = on_signal as extern "C" fn(i32) as usize;
        let act = Sigaction {
            handler,
            mask: Default::default(),
            flags: RESTART_NODEFER,
            #[cfg(target_os = "linux")]
            restorer: 0,
        };
        // SAFETY: `act` is live and laid out as `sigaction` reads it
        // (`Sigaction`), and a null `old` is not written. `on_signal` is an
        // `extern "C" fn(i32)` that lives for the whole program and does only
        // async-signal-safe work.
        unsafe {
            if sigaction(sig, &raw const act, std::ptr::null_mut()) != 0 {
                signal(sig, handler);
            }
        }
    }

    /// On a Unix whose `struct sigaction` is not declared above, `signal(3)`
    /// installs [`on_signal`]. There a second interrupt of the
    /// number being handled is held back on the handling thread, as it was
    /// everywhere before; one of another number, or on another thread, ends
    /// the grace as above.
    #[cfg(not(any(
        target_os = "macos",
        all(
            target_os = "linux",
            target_pointer_width = "64",
            any(target_arch = "x86_64", target_arch = "aarch64"),
            any(target_env = "gnu", target_env = "musl")
        )
    )))]
    fn catch(sig: i32) {
        // SAFETY: `on_signal` is an `extern "C" fn(i32)` that lives for the
        // whole program and does only async-signal-safe work.
        unsafe {
            signal(sig, on_signal as extern "C" fn(i32) as usize);
        }
    }
}

/// A child that reached an exit status of its own.
fn reaped(st: io::Result<std::process::ExitStatus>) -> Run {
    match st {
        Ok(st) => Run {
            ok: st.success(),
            output: String::new(),
            code: st.code(),
            spawn_error: None,
        },
        Err(e) => failed_to_wait(&e.to_string()),
    }
}

/// The child could not be spawned, or could not be waited on — an environment
/// failure, not a finding about the tree, and fail-closed either way.
fn failed_to_wait(why: &str) -> Run {
    Run {
        ok: false,
        output: String::new(),
        code: None,
        spawn_error: Some(why.to_string()),
    }
}

fn spawn_failure(cmd: &Cmd, why: &str) -> Run {
    Run {
        ok: false,
        output: format!("aterm-verify: cannot run {}: {why}", cmd.program.display()),
        code: None,
        spawn_error: Some(why.to_string()),
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    File::options().create(true).append(true).open(path)
}

/// Run a child and return its merged stdout+stderr with trailing newlines
/// stripped (the shell's `got="$(cmd 2>&1)"`), how every control-socket round
/// trip in the smokes reads its reply.
#[must_use]
pub fn capture_reply(cmd: &Cmd, env: ExecEnv<'_>) -> String {
    let r = run(&cmd.clone().capture(Capture::Emit), env);
    r.trimmed_output().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32};

    /// The default the gate itself runs under, so every test that does not say
    /// otherwise exercises the polling wait.
    fn env_in(dir: &Path) -> ExecEnv<'_> {
        ceiled(dir, Some(DEFAULT_CHILD_CEILING))
    }

    /// The ceiling for a hung child whose own output a test then asserts: it has
    /// to outlast `/bin/sh` starting and printing on a loaded machine. The child
    /// sleeps 600 s after, so the ceiling always fires.
    const PRINTS_FIRST: Duration = Duration::from_secs(5);

    fn ceiled(dir: &Path, child_ceiling: Option<Duration>) -> ExecEnv<'_> {
        ExecEnv {
            cwd: dir,
            path: OsStr::new("/usr/bin:/bin"),
            scratch: dir,
            child_ceiling,
            remove_env: &[],
            add_env: &[],
        }
    }

    /// The pool's reading is a count of busy cores, between none and every one,
    /// on both hosts the gate runs on.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn busy_cores_reads_between_none_and_every_core() {
        let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        #[expect(clippy::cast_precision_loss, reason = "a core count")]
        let every = cores as f64 + 1e-9;
        // Read the way the pool reads, a sample apart (another test may have
        // just read, and a reading within a tick of it answers nothing).
        let mut readings = Vec::new();
        for _ in 0..20 {
            if let Some(r) = busy_cores() {
                readings.push(r);
                if readings.len() == 2 {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        assert_eq!(readings.len(), 2, "the counters read: {readings:?}");
        for r in readings {
            assert!((0.0..=every).contains(&r), "{r} of {cores}");
        }
    }

    /// A reaped child reports the CPU it and its own reaped children spent,
    /// through the polling wait and the blocking one alike, with its status
    /// intact; a child killed at the ceiling reports none. A shell loop that
    /// spends CPU in a grandchild is counted, which a sleep is not.
    #[cfg(unix)]
    #[test]
    fn a_reaped_child_reports_its_cpu_and_a_killed_one_none() {
        let tmp = crate::mktemp_dir("atv-cpu").expect("mktemp");
        let burn = Cmd::new("/bin/sh").args([
            "-c",
            "/bin/sh -c 'i=0; while [ $i -lt 100000 ]; do i=$((i+1)); done'; exit 3",
        ]);
        for env in [env_in(&tmp), ceiled(&tmp, None)] {
            let (r, cpu) = run_measured(&burn, env);
            assert_eq!(r.code, Some(3), "{:?}", r.output);
            let cpu = cpu.expect("a reaped child's CPU");
            assert!(
                cpu >= Duration::from_millis(20),
                "the grandchild's loop: {cpu:?}"
            );
        }
        let (r, cpu) = run_measured(&Cmd::new("/bin/sleep").arg("1"), env_in(&tmp));
        assert!(r.ok);
        assert!(
            cpu.expect("reaped") < Duration::from_millis(500),
            "a sleep spends none"
        );
        let hung = Cmd::new("/bin/sleep").arg("600");
        let (r, cpu) = run_measured(&hung, ceiled(&tmp, Some(Duration::from_millis(200))));
        assert!(!r.ok);
        assert_eq!(cpu, None, "killed at the ceiling: no rusage was reaped");
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The gate's own setting reaches the child over whatever the caller's shell
    /// exported, and a stage that names the variable itself still wins (the same
    /// precedence `remove_env` has). `lib.rs` pins that `CARGO_INCREMENTAL=0` is
    /// one of the run's facts.
    #[test]
    fn an_added_variable_reaches_the_child_and_a_stage_naming_it_still_wins() {
        let tmp = crate::mktemp_dir("atv-setenv").expect("mktemp");
        let added = [(OsString::from("CARGO_INCREMENTAL"), OsString::from("0"))];
        let env = ExecEnv {
            add_env: &added,
            ..env_in(&tmp)
        };
        let ask = Cmd::new("/bin/sh").args(["-c", "printf %s \"${CARGO_INCREMENTAL-unset}\""]);
        assert_eq!(run(&ask, env).trimmed_output(), "0");
        let stage_says = ask.clone().env("CARGO_INCREMENTAL", "1");
        assert_eq!(run(&stage_says, env).trimmed_output(), "1");
        let plain = run(&ask, env_in(&tmp));
        assert!(
            plain.trimmed_output() != "0"
                || std::env::var_os("CARGO_INCREMENTAL").is_some_and(|v| v == "0"),
            "with nothing set the child inherits: {:?}",
            plain.output
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A child that never spawned, and one that died of a full disk, are
    /// environment failures; a child that ran and failed is a finding, and a
    /// child that passed is never asked — whatever it printed.
    #[test]
    fn a_spawn_failure_or_an_enospc_is_the_environment_and_a_plain_failure_is_a_finding() {
        let child = |ok: bool, output: &str, spawn_error: Option<&str>| Run {
            ok,
            output: output.to_string(),
            code: if spawn_error.is_some() {
                None
            } else {
                Some(i32::from(!ok))
            },
            spawn_error: spawn_error.map(str::to_string),
        };
        let never = child(
            false,
            "aterm-verify: cannot run /s2/targo: No space left on device (os error 28)",
            Some("No space left on device (os error 28)"),
        );
        assert_eq!(
            never.environment_failure().as_deref(),
            Some("No space left on device (os error 28)")
        );
        let starved = child(
            false,
            "   Compiling aterm-gui v0.89.0\nerror: failed to write /s/target/debug/deps/rustccu4LYX/lib.rmeta: No space left on device (os error 28)\n\nerror: could not compile `aterm-gui` (lib) due to 1 previous error\n",
            None,
        );
        assert_eq!(
            starved.environment_failure().as_deref(),
            Some("the child ran out of disk (No space left on device)")
        );
        let finding = child(false, "error[E0308]: mismatched types\n", None);
        assert_eq!(finding.environment_failure(), None);
        let passed = child(
            true,
            "test enospc_message_is_last_line ... ok (No space left on device)\n",
            None,
        );
        assert_eq!(
            passed.environment_failure(),
            None,
            "a passing child is never asked"
        );
    }

    #[test]
    fn a_command_is_a_value_before_it_is_a_process() {
        let c = Cmd::new("/s2/targo")
            .arg("--unverified")
            .args(["test", "--doc"])
            .args(["-p", "aterm-grid"])
            .env("RUSTDOC", "/s2/trustdoc");
        assert_eq!(
            c.argv(),
            [
                "/s2/targo",
                "--unverified",
                "test",
                "--doc",
                "-p",
                "aterm-grid"
            ]
        );
        assert_eq!(
            c.envs,
            [(OsString::from("RUSTDOC"), OsString::from("/s2/trustdoc"))]
        );
    }

    #[cfg(unix)]
    fn spawned_argv(c: &Command) -> Vec<String> {
        std::iter::once(c.get_program())
            .chain(c.get_args())
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }

    /// Checks the wrapping decision, with a stand-in `taskpolicy` so it runs
    /// the same on every Unix: a demoted child whose two ends are real runs
    /// behind `taskpolicy -c utility`; every other child is the tool alone.
    #[cfg(unix)]
    #[test]
    fn only_a_demoted_child_that_can_run_is_spawned_under_utility_qos() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = crate::mktemp_dir("atv-qos").expect("mktemp");
        let fake = tmp.join("taskpolicy");
        std::fs::write(&fake, "#!/bin/sh\nexit 0\n").expect("write");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let fake_s = fake.to_string_lossy().into_owned();
        let env = env_in(&tmp);
        let tool = Cmd::new("/bin/sh").args(["-c", "exit 0"]);

        assert_eq!(
            spawned_argv(&spawn_command(&tool, env, &fake)),
            ["/bin/sh", "-c", "exit 0"],
            "a child nobody demoted keeps the inherited tier"
        );
        assert_eq!(
            spawned_argv(&spawn_command(&tool.clone().demoted(), env, &fake)),
            [fake_s.as_str(), "-c", "utility", "/bin/sh", "-c", "exit 0"]
        );
        assert_eq!(
            spawned_argv(&spawn_command(
                &tool.clone().demoted(),
                env,
                &tmp.join("absent")
            ))[0],
            "/bin/sh",
            "no taskpolicy on this machine: the tool alone"
        );
        let missing = Cmd::new(tmp.join("no-such-tool")).demoted();
        assert_eq!(
            spawned_argv(&spawn_command(&missing, env, &fake))[0],
            missing.program.to_string_lossy(),
            "a missing tool is spawned bare, so it still fails to SPAWN"
        );
        assert_eq!(
            spawned_argv(&spawn_command(&Cmd::new("sh").demoted(), env, &fake))[0],
            fake_s,
            "a bare name resolves on the child's PATH"
        );
        let hidden = Cmd::new("sh").env("PATH", tmp.as_os_str()).demoted();
        assert_eq!(
            spawned_argv(&spawn_command(&hidden, env, &fake))[0],
            "sh",
            "the child's own PATH override is the one searched"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_demoted_child_that_cannot_run_still_says_it_could_not_run() {
        let tmp = crate::mktemp_dir("atv-qos-missing").expect("mktemp");
        let r = run(&Cmd::new(tmp.join("no-such-tool")).demoted(), env_in(&tmp));
        assert!(!r.ok);
        assert!(r.spawn_error.is_some(), "{r:?}");
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn output_is_captured_with_stderr_merged_in_write_order() {
        let tmp = crate::mktemp_dir("atv-exec").expect("mktemp");
        let cmd = Cmd::new("/bin/sh").args(["-c", "echo out; echo err >&2; echo out2"]);
        let r = run(&cmd, env_in(&tmp));
        assert!(r.ok);
        assert_eq!(r.trimmed_output(), "out\nerr\nout2");
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_nonzero_exit_is_a_failure_and_the_output_survives() {
        let tmp = crate::mktemp_dir("atv-exec2").expect("mktemp");
        let cmd = Cmd::new("/bin/sh").args(["-c", "echo why >&2; exit 3"]);
        let r = run(&cmd, env_in(&tmp));
        assert!(!r.ok);
        assert_eq!(r.trimmed_output(), "why");
        // The code survives too: a child that distinguishes "failed" from "could
        // not run" says so here and nowhere else.
        assert_eq!(r.code, Some(3));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_missing_tool_is_a_failed_run_not_a_panic_and_never_a_pass() {
        let tmp = crate::mktemp_dir("atv-exec3").expect("mktemp");
        let r = run(&Cmd::new(tmp.join("absent-driver")), env_in(&tmp));
        assert!(
            !r.ok,
            "fail-closed: an unspawnable child never reads as green"
        );
        assert!(r.spawn_error.is_some());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn the_child_runs_in_the_repo_root_with_the_gates_path() {
        let tmp = crate::mktemp_dir("atv-exec4").expect("mktemp");
        std::fs::write(tmp.join("marker"), b"x").expect("write");
        let r = run(
            &Cmd::new("/bin/sh").args(["-c", "cat marker; printf %s \"$PATH\""]),
            env_in(&tmp),
        );
        assert_eq!(r.trimmed_output(), "x/usr/bin:/bin");
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The load reader: the first number of either spelling (macOS's braced
    /// `sysctl -n vm.loadavg`, Linux's `/proc/loadavg`), and nothing from a line
    /// that holds no number.
    #[test]
    fn the_load_reader_takes_the_one_minute_figure_of_either_spelling() {
        assert_eq!(parse_load_average("{ 74.82 58.31 32.86 }\n"), Some(74.82));
        assert_eq!(parse_load_average("1.50 0.90 0.40 2/613 4242\n"), Some(1.5));
        assert_eq!(parse_load_average(""), None);
        assert_eq!(parse_load_average("{ }"), None);
        assert_eq!(parse_load_average("{ busy 1.0 }"), None);
    }

    /// Linux's `MemTotal` is read in kB and answered in bytes; a file without
    /// one, or with nothing usable in it, answers nothing.
    #[test]
    fn the_meminfo_reader_takes_mem_total_in_bytes() {
        let text = "MemFree:  100 kB\nMemTotal:       8059140 kB\nSwapTotal: 0 kB\n";
        assert_eq!(parse_meminfo_total(text), Some(8_059_140 * 1024));
        assert_eq!(parse_meminfo_total("MemFree: 100 kB\n"), None);
        assert_eq!(parse_meminfo_total("MemTotal: lots kB\n"), None);
        assert_eq!(parse_meminfo_total("MemTotal: 0 kB\n"), None);
        let both = "MemTotal: 8059140 kB\nMemFree: 100 kB\nMemAvailable: 6000000 kB\n";
        assert_eq!(parse_meminfo(both, "MemAvailable:"), Some(6_000_000 * 1024));
        assert_eq!(parse_meminfo(text, "MemAvailable:"), None);
    }

    /// The usable reading is the host's own and sits under its physical
    /// memory: on macOS, the physical less a nonzero wired figure.
    #[test]
    fn the_usable_memory_reading_is_under_the_physical() {
        let Some((physical, _)) = host_memory_bytes() else {
            return;
        };
        let usable = host_usable_bytes(physical);
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            let usable = usable.expect("a usable figure on macOS and Linux");
            assert!(usable > 0 && usable <= physical, "{usable} of {physical}");
        }
        if cfg!(target_os = "macos") {
            assert!(usable < Some(physical), "the kernel wires some memory");
        }
    }

    /// The in-process reading answers a load, and it is THE load the machine
    /// reports: it sits between two reads of the older reading
    /// (`/proc/loadavg`, else `sysctl -n vm.loadavg`) taken either side of it.
    /// The kernel moves the one-minute figure in one monotone step every five
    /// seconds, so a read made between two others cannot fall outside them
    /// except by the two-decimal rounding the printed spelling carries (the
    /// `0.1` slack): a sample that lands inside the window moves the later read
    /// to the far side of the bracket, not past it. Only a reader descheduled
    /// across two samples escapes it, and the retry (three tries) covers that.
    /// The bound is tight on purpose: a reader that returned the 15-minute
    /// figure, a scaled one or an unwritten buffer is outside it whenever the
    /// machine's averages differ at all.
    /// It calls [`getloadavg_one`] itself, not [`load_average`]: that one falls
    /// back to the older reading when `getloadavg` says nothing, so a reader
    /// that is dead (it answers `None` on every call) would still pass here
    /// while every ladder went back to a spawn per stage.
    #[cfg(unix)]
    #[test]
    fn the_in_process_load_is_a_load_and_agrees_with_the_older_reading() {
        let older =
            || load_average_by_file_or_spawn().expect("the older reading answers on a unix host");
        let mut seen = Vec::new();
        for _ in 0..3 {
            let before = older();
            let now = getloadavg_one().expect("a unix host answers getloadavg");
            let after = older();
            assert!(now.is_finite() && now >= 0.0, "not a load: {now}");
            if now >= before.min(after) - 0.1 && now <= before.max(after) + 0.1 {
                return;
            }
            seen.push((before, now, after));
        }
        panic!("getloadavg never fell between two reads of the older reading: {seen:?}");
    }

    /// A replayed child is the process cargo would have started: its own
    /// directory, exactly the recorded environment (none of the gate's PATH,
    /// removals or additions), and a preamble at the head of its log, above its
    /// own bytes.
    #[test]
    fn a_replayed_child_starts_where_and_as_it_was_recorded() {
        let tmp = crate::mktemp_dir("atv-replay").expect("mktemp");
        let pkg = tmp.join("pkg");
        std::fs::create_dir_all(&pkg).expect("mkdir");
        std::fs::write(pkg.join("marker"), b"in-pkg").expect("write");
        let added = [(OsString::from("GATE_ADDED"), OsString::from("gate"))];
        let env = ExecEnv {
            add_env: &added,
            ..env_in(&tmp)
        };
        let cmd = Cmd::replayed(
            &[
                OsString::from("/bin/sh"),
                OsString::from("-c"),
                OsString::from(
                    "cat marker; printf ' %s %s %s' \"${RECORDED-unset}\" \"${GATE_ADDED-unset}\" \
                     \"${HOME-unset}\"",
                ),
            ],
            &pkg,
            vec![
                (OsString::from("RECORDED"), OsString::from("yes")),
                (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
            ],
        )
        .preamble("     Running tests/x.rs (target/debug/deps/x-1)\n");
        let r = run(&cmd, env);
        assert!(r.ok, "{}", r.output);
        assert_eq!(
            r.trimmed_output(),
            "     Running tests/x.rs (target/debug/deps/x-1)\nin-pkg yes unset unset",
            "the preamble first, then the child — in its own directory, with only what \
             was recorded"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The preamble is what a ceiling kill reads: a test binary the gate runs
    /// itself carries cargo's `Running` header only because the gate wrote it,
    /// and the TIMEOUT block names the binary and its re-run line from it.
    #[test]
    fn a_timed_out_replayed_binary_is_named_from_its_preamble() {
        let tmp = crate::mktemp_dir("atv-replay-hang").expect("mktemp");
        let script = "printf '\\nrunning 1 test\\n\
                      test a::hang has been running for over 60 seconds\\n'; exec sleep 600";
        let cmd = Cmd::replayed(
            &[
                OsString::from("/bin/sh"),
                OsString::from("-c"),
                OsString::from(script),
            ],
            &tmp,
            vec![(OsString::from("PATH"), OsString::from("/usr/bin:/bin"))],
        )
        .preamble("     Running unittests src/lib.rs (target/debug/deps/x-abc)\n");
        let r = run(&cmd, ceiled(&tmp, Some(PRINTS_FIRST)));
        assert!(!r.ok);
        let out = r.trimmed_output();
        assert!(
            out.contains(
                "  test binary: unittests src/lib.rs (target/debug/deps/x-abc) — it never \
                 printed its `test result:` line\n"
            ),
            "{out}"
        );
        assert!(
            out.contains("    re-run alone: target/debug/deps/x-abc --exact a::hang --nocapture\n"),
            "{out}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn silent_and_append_captures_route_output_where_the_script_did() {
        let tmp = crate::mktemp_dir("atv-exec5").expect("mktemp");
        let noisy = Cmd::new("/bin/sh").args(["-c", "echo loud; echo louder >&2"]);

        let r = run(&noisy.clone().capture(Capture::Silent), env_in(&tmp));
        assert!(r.ok && r.output.is_empty());

        let log = tmp.join("gui.log");
        let r = run(
            &noisy.clone().capture(Capture::Append(log.clone())),
            env_in(&tmp),
        );
        assert!(r.ok && r.output.is_empty());
        let r = run(&noisy.capture(Capture::Append(log.clone())), env_in(&tmp));
        assert!(r.ok);
        let logged = std::fs::read_to_string(&log).expect("log");
        assert_eq!(
            logged.matches("loud\n").count(),
            2,
            "appends, never truncates"
        );
        assert_eq!(
            logged.matches("louder\n").count(),
            2,
            "stderr lands in the same log"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    // -----------------------------------------------------------------------
    // The wall-clock ceiling.
    // -----------------------------------------------------------------------

    /// How long a grandchild that must be gone gets to go: a hang detector (a
    /// killed grandchild is reaped by launchd, and a loaded machine takes its
    /// time). Every such grandchild is a `sleep 600`, so this stays far below
    /// its life.
    #[cfg(unix)]
    const GONE: Duration = Duration::from_secs(60);

    /// A wait for something else that must happen — a trapped group exiting
    /// inside its grace, a ceiling ending a child — is a hang detector too,
    /// never a latency budget.
    const HANG_DETECTOR: Duration = Duration::from_secs(60);

    /// How long a negative control's orphan must SURVIVE to count as left
    /// behind. A window that can only go vacuous: kept short.
    #[cfg(unix)]
    const STILL_THERE: Duration = Duration::from_secs(5);

    /// One `ps` column of `pid`, trimmed: empty when no such process answers.
    #[cfg(unix)]
    fn ps(column: &str, pid: &str) -> String {
        std::process::Command::new("ps")
            .args(["-o", column, "-p", pid])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    }

    /// Has the process a shell forked for `program` exec'ed it? `ps`'s `ucomm`
    /// is the name the kernel gives a process from the file it last exec'ed,
    /// and a fork keeps its parent's: the shell's until the fork's `exec`, the
    /// program's after. Not `comm`, which on macOS is `argv[0]` and so
    /// whatever the process was started as: a shell run under the program's
    /// name reads as the program there ([`suite_slow_to_exec`] runs one).
    /// Once `ucomm` says `program`, nothing of the shell runs in the process
    /// again: the handlers the fork inherited went with the image, and a
    /// signal sent from here on meets the program's own disposition.
    #[cfg(unix)]
    fn has_become(pid: &str, program: &str) -> bool {
        ps("ucomm=", pid) == program
    }

    /// Is `pid` gone (or a zombie) within `window`? Polls, because a killed
    /// grandchild is reaped by launchd, not by us.
    #[cfg(unix)]
    fn gone_within(pid: &str, window: Duration) -> bool {
        let deadline = Instant::now() + window;
        loop {
            let stat = ps("stat=", pid);
            if stat.is_empty() || stat.starts_with('Z') {
                return true;
            }
            if Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The pid a stage child's shell wrote for the `sleep` it backgrounded. The
    /// bound is liveness, not latency: a loaded gate can take seconds to start a
    /// shell.
    #[cfg(unix)]
    fn read_pid(file: &Path) -> String {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(pid) = written_pid(file) {
                return pid;
            }
            assert!(Instant::now() < deadline, "no pid in {}", file.display());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The pid in `file` once its writer has finished the line, else `None`.
    #[cfg(unix)]
    fn written_pid(file: &Path) -> Option<String> {
        let t = std::fs::read_to_string(file).ok()?;
        t.ends_with('\n').then(|| t.trim().to_string())
    }

    /// A shell that backgrounds a long `sleep` — the grandchild — writes its
    /// pid to `file`, and waits on it: the shape of a `targo test` that has
    /// launched a daemon, one level down.
    #[cfg(unix)]
    fn with_grandchild(file: &Path) -> Cmd {
        Cmd::new("/bin/sh").args([
            "-c",
            &format!("sleep 600 & echo $! > '{}'; wait", file.display()),
        ])
    }

    /// The ceiling ends the whole group: a grandchild the stage child started is
    /// gone once it fires. Negative control: `Child::kill` on the direct child
    /// leaves the same grandchild running.
    #[cfg(unix)]
    #[test]
    fn a_ceiling_kill_takes_the_childs_whole_process_group() {
        let tmp = crate::mktemp_dir("atv-ceil-group").expect("mktemp");
        // The ceiling clock starts at the spawn, so a ceiling that fires before
        // the shell has backgrounded its `sleep` kills a group with no
        // grandchild in it, and no pid ever comes (2026-10-01: a merge gate at
        // load 110 on 18 cores waited its minute for one behind a 0.5 s
        // ceiling). The group is dead and reaped once `run` returns, so a
        // pidfile with no finished line by then never gets one: that run
        // tested nothing, and the next doubles the ceiling.
        let mut ceiling = Duration::from_millis(500);
        let grandchild = loop {
            let pidfile = tmp.join(format!("grandchild-{}ms.pid", ceiling.as_millis()));
            let r = run(&with_grandchild(&pidfile), ceiled(&tmp, Some(ceiling)));
            assert!(!r.ok && r.output.contains("TIMEOUT"), "{}", r.output);
            if let Some(pid) = written_pid(&pidfile) {
                break pid;
            }
            assert!(
                ceiling < GONE,
                "no shell backgrounded its grandchild within a {ceiling:?} ceiling"
            );
            ceiling *= 2;
        };
        assert!(
            gone_within(&grandchild, GONE),
            "the grandchild {grandchild} outlived the ceiling kill"
        );

        // The negative control: the direct child killed alone.
        let pidfile = tmp.join("control.pid");
        let mut child = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                &format!("sleep 600 & echo $! > '{}'; wait", pidfile.display()),
            ])
            .spawn()
            .expect("spawn");
        let orphan = read_pid(&pidfile);
        child.kill().expect("kill");
        let _ = child.wait();
        let survived = !gone_within(&orphan, STILL_THERE);
        let _ = std::process::Command::new("/bin/kill")
            .args(["-KILL", &orphan])
            .status();
        assert!(
            survived,
            "a direct-child kill left no orphan, so this test cannot tell the two apart"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The marker a stand-in gate writes beside `pidfile` once it has armed.
    #[cfg(unix)]
    fn armed(pidfile: &Path) -> PathBuf {
        pidfile.with_extension("armed")
    }

    /// A stand-in gate's stage: run `child` and write the [`armed`] marker once
    /// the child's group is on the live list, `pidfile` holds its grandchild's
    /// pid, and that grandchild has exec'ed its `sleep` ([`has_become`]). The
    /// pidfile alone would race the registration, and it races the exec.
    ///
    /// WHY THE EXEC (2026-10-12, a merge gate's one red: "the handler stops
    /// waiting once the group has gone: 10.79 s"). The shell writes `$!` once
    /// it has forked, and the fork is still that shell until it execs. A bash
    /// with an EXIT trap catches `SIGTERM` to run it, the fork inherits the
    /// handler, and a group `SIGTERM` that reaches the fork before its `exec`
    /// can be taken there, and is then gone: the `sleep` it becomes never
    /// hears of it, the group does not empty, and the interrupted gate waits
    /// out its whole grace before the `SIGKILL`. Measured with no Rust in it
    /// (bash 3.2.57, [`suite_with_trap`]'s script, `SIGTERM` within 30 ms of
    /// the pidfile's newline): the `sleep` outlived it in 27 of 1,800 trials
    /// at load 40-75 on 18 cores and in none of 600 on an idle machine.
    /// [`suite_slow_to_exec`] holds that window open.
    #[cfg(unix)]
    fn run_armed(child: &Cmd, pidfile: &Path) {
        run_armed_after(child, pidfile, || {});
    }

    /// [`run_armed`], with `first` run once the stage is ready to arm and
    /// before the marker says so: what a stand-in reads of a stage whose
    /// child is running, while nothing has interrupted it yet.
    #[cfg(unix)]
    fn run_armed_after(child: &Cmd, pidfile: &Path, mut first: impl FnMut()) {
        let dir = pidfile.parent().expect("a dir");
        std::thread::scope(|scope| {
            let stage = scope.spawn(|| run(child, ceiled(dir, None)));
            // The wait has its own end: a stage that has finished leaves
            // nothing to arm on, and past the minute its parent waits for the
            // marker ([`interrupt_when_armed`]) nobody is reading it. Without
            // one, a stand-in whose parent had died went on polling `ps` for
            // as long as its stage lived.
            let deadline = Instant::now() + HANG_DETECTOR;
            while !stage.is_finished() && Instant::now() < deadline {
                if group::live_count() > 0
                    && written_pid(pidfile).is_some_and(|pid| has_become(&pid, "sleep"))
                {
                    first();
                    std::fs::write(armed(pidfile), "armed\n").expect("the armed marker");
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = stage.join();
        });
    }

    /// Send process `pid` the signal `name` (`INT`, `TERM`, `HUP`, `KILL`), as
    /// a terminal or a `kill` would.
    #[cfg(unix)]
    fn send(name: &str, pid: &str) {
        std::process::Command::new("/bin/kill")
            .args([&format!("-{name}"), pid])
            .status()
            .expect("/bin/kill runs");
    }

    /// Wait for a stand-in gate to arm (an event, never a guessed delay), and
    /// answer its stage's grandchild. A stand-in that never arms is killed, and
    /// with it the grandchild its pidfile names, which the panic describes: a
    /// stage that had written its pid leaves no 600 s `sleep` behind. One that
    /// had not is out of reach from here (it leads its own group, and only the
    /// stand-in knew which) and lives out whatever it goes on to start.
    #[cfg(unix)]
    fn armed_grandchild(child: &mut std::process::Child, pidfile: &Path) -> String {
        let marker = armed(pidfile);
        let deadline = Instant::now() + HANG_DETECTOR;
        while !marker.exists() {
            if Instant::now() > deadline || matches!(child.try_wait(), Ok(Some(_))) {
                let _ = child.kill();
                let _ = child.wait();
                let stuck = written_pid(pidfile).map_or_else(
                    || "its stage wrote no grandchild pid".to_string(),
                    |pid| {
                        let image = ps("ucomm=", &pid);
                        let _ = std::process::Command::new("/bin/kill")
                            .args(["-KILL", &pid])
                            .status();
                        format!("its pidfile's grandchild {pid} was {image:?} (it arms on `sleep`)")
                    },
                );
                panic!(
                    "the stand-in gate never armed ({}): {stuck}",
                    marker.display()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        read_pid(pidfile)
    }

    /// Send a stand-in gate `SIGINT` once it has armed ([`armed_grandchild`]),
    /// and answer how it exited, its stage's grandchild, and how long it took
    /// to exit after the signal.
    #[cfg(unix)]
    fn interrupt_when_armed(
        mut child: std::process::Child,
        pidfile: &Path,
    ) -> (std::process::ExitStatus, String, Duration) {
        let grandchild = armed_grandchild(&mut child, pidfile);
        let t = Instant::now();
        send("INT", &child.id().to_string());
        let status = child.wait().expect("the stand-in exits");
        (status, grandchild, t.elapsed())
    }

    /// An interrupted gate takes its stage children with it. The test re-executes
    /// itself as a stand-in gate that installs [`group::kill_on_interrupt`] (or,
    /// for the negative control, does not), runs a stage child with a
    /// grandchild, and is sent `SIGINT` as a terminal's Ctrl-C would. With the
    /// handler the grandchild is gone and the stand-in died of `SIGINT`; without
    /// it the child's group never heard the signal and the grandchild remains.
    #[cfg(unix)]
    #[test]
    fn an_interrupted_gate_kills_every_live_stage_group() {
        use std::os::unix::process::ExitStatusExt as _;
        const PIDFILE: &str = "ATV_INTERRUPT_PIDFILE";
        const NO_HANDLER: &str = "ATV_INTERRUPT_NO_HANDLER";
        if let Some(pidfile) = std::env::var_os(PIDFILE) {
            if std::env::var_os(NO_HANDLER).is_none() {
                group::kill_on_interrupt();
            } else {
                // The Ctrl-C this models meets SIGINT at its default action. As
                // a background job with job control off the stand-in would
                // inherit it ignored, wait out its stage's `sleep`, and the
                // negative control would read "died anyway".
                group::default_interrupt();
            }
            let pidfile = PathBuf::from(pidfile);
            run_armed(&with_grandchild(&pidfile), &pidfile);
            return;
        }
        let tmp = crate::mktemp_dir("atv-interrupt").expect("mktemp");
        let me = std::env::current_exe().expect("the test binary");
        let stand_in = |pidfile: &Path, handler: bool| {
            let mut c = std::process::Command::new(&me);
            c.args([
                "--exact",
                "exec::tests::an_interrupted_gate_kills_every_live_stage_group",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(PIDFILE, pidfile)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
            if !handler {
                c.env(NO_HANDLER, "1");
            }
            let child = c.spawn().expect("the stand-in gate starts");
            let (status, grandchild, _) = interrupt_when_armed(child, pidfile);
            (status, grandchild)
        };

        let (status, grandchild) = stand_in(&tmp.join("handled.pid"), true);
        assert_eq!(
            status.signal(),
            Some(2),
            "it still dies of SIGINT: {status:?}"
        );
        assert!(
            gone_within(&grandchild, GONE),
            "the grandchild {grandchild} outlived the interrupted gate"
        );

        let (_, orphan) = stand_in(&tmp.join("unhandled.pid"), false);
        let survived = !gone_within(&orphan, STILL_THERE);
        let _ = std::process::Command::new("/bin/kill")
            .args(["-KILL", &orphan])
            .status();
        assert!(
            survived,
            "without the handler the grandchild died anyway, so this test cannot see it"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The ceiling for [`suite_with_trap`] runs. The ceiling clock starts at the
    /// spawn, so it must outlast bash's startup to its traps, the pidfile and
    /// its grandchild's exec on a loaded machine; firing first would kill a
    /// shell with no trap installed, or lose the graceful suite's `SIGTERM` in
    /// the fork ([`run_armed`] says how) and wait out its grace. 3 s is still a
    /// small fraction of every grace compared.
    #[cfg(unix)]
    const SUITE_CEILING: Duration = Duration::from_secs(3);

    /// A grace far past [`HANG_DETECTOR`]: a gate that SLEEPS its grace instead
    /// of moving on when the group empties then takes ten minutes, so a minute's
    /// hang detector tells the two apart however loaded the machine is.
    #[cfg(unix)]
    const LONG_GRACE: Duration = Duration::from_secs(600);

    /// A shell SUITE's shape: an EXIT trap that leaves `marker`, and a
    /// grandchild `sleep` (its pid in `pidfile`) the shell waits on. `ignore`
    /// makes the shell — and so the `sleep`, which inherits the disposition —
    /// ignore `SIGTERM`, the suite that will not go quietly.
    #[cfg(unix)]
    fn suite_with_trap(marker: &Path, pidfile: &Path, ignore: bool) -> Cmd {
        Cmd::new("/bin/bash").args([
            "-c",
            &format!(
                "{}trap 'touch {}' EXIT; sleep 600 & echo $! > '{}'; wait",
                if ignore { "trap '' TERM; " } else { "" },
                marker.display(),
                pidfile.display()
            ),
        ])
    }

    /// [`suite_with_trap`] with the window [`run_armed`] waits out held open: a
    /// grandchild that is a bash run under the name `sleep`, writes its own
    /// pid, catches `SIGTERM` as the fork of a trapping bash does, and execs
    /// its `sleep 600` a second later, or as soon as a `SIGTERM` ends the
    /// `sleep 1` it waits on. A `SIGTERM` sent on the strength of `pidfile`
    /// alone, or of the name the process goes by, is taken by that handler and
    /// the `sleep 600` outlives it: a stand-in armed either way waits out its
    /// whole 10 s grace (2026-10-12: on the pidfile three runs of three, on
    /// `ps`'s `comm` three of three).
    #[cfg(unix)]
    fn suite_slow_to_exec(marker: &Path, pidfile: &Path) -> Cmd {
        Cmd::new("/bin/bash").args([
            "-c",
            &format!(
                "trap 'touch {}' EXIT; \
                 (exec -a sleep /bin/bash -c \
                 'trap : TERM; echo $$ > \"{}\"; sleep 1; exec sleep 600') & wait",
                marker.display(),
                pidfile.display()
            ),
        ])
    }

    /// [`suite_with_trap`] with a grandchild that ignores `SIGTERM` for good:
    /// the group's `SIGTERM` runs the shell's EXIT trap and ends the shell,
    /// and the `sleep` lives on in a group whose leader is gone. A suite that
    /// leaves a helper behind, which only a `SIGKILL` ends.
    #[cfg(unix)]
    fn suite_with_survivor(marker: &Path, pidfile: &Path) -> Cmd {
        Cmd::new("/bin/bash").args([
            "-c",
            &format!(
                "trap 'touch {}' EXIT; (trap '' TERM; exec sleep 600) & echo $! > '{}'; wait",
                marker.display(),
                pidfile.display()
            ),
        ])
    }

    /// [`suite_with_trap`] with an EXIT trap that takes a while: it runs a
    /// child for 0.3 s before it leaves `marker`. A second `SIGTERM` to the
    /// group inside that time ends the trap's child, and the trap with it in
    /// a bash that takes the signal there, so the marker is never left.
    #[cfg(unix)]
    fn suite_with_slow_trap(marker: &Path, pidfile: &Path) -> Cmd {
        Cmd::new("/bin/bash").args([
            "-c",
            &format!(
                "trap 'sleep 0.3 && touch {}' EXIT; sleep 600 & echo $! > '{}'; wait",
                marker.display(),
                pidfile.display()
            ),
        ])
    }

    /// A graceful child's EXIT trap runs before the ceiling kill: the group is
    /// sent `SIGTERM`, bash runs its trap, the group empties, and the gate moves
    /// on without waiting out the grace; the grandchild is gone too. Negative
    /// control: the same suite with no grace gets `SIGKILL`, and no marker.
    #[cfg(unix)]
    #[test]
    fn a_graceful_child_runs_its_exit_trap_before_the_ceiling_kill() {
        let tmp = crate::mktemp_dir("atv-ceil-grace").expect("mktemp");
        let (marker, pidfile) = (tmp.join("trap-ran"), tmp.join("grandchild.pid"));
        let t = Instant::now();
        let r = run(
            &suite_with_trap(&marker, &pidfile, false).term_grace(LONG_GRACE),
            ceiled(&tmp, Some(SUITE_CEILING)),
        );
        let waited = t.elapsed();
        assert!(!r.ok && r.output.contains("TIMEOUT"), "{}", r.output);
        assert!(
            marker.exists(),
            "SIGTERM first: the EXIT trap ran\n{}",
            r.output
        );
        assert!(
            r.output
                .contains("SIGTERM first, with a 600.0s grace: the group exited within it"),
            "{}",
            r.output
        );
        assert!(
            waited < SUITE_CEILING + HANG_DETECTOR,
            "the grace is a bound, not a sleep: {waited:?} of a {LONG_GRACE:?} grace"
        );
        let grandchild = read_pid(&pidfile);
        assert!(
            gone_within(&grandchild, GONE),
            "{grandchild} outlived the kill"
        );

        // The negative control: no grace, so SIGKILL, so no trap.
        let (marker, pidfile) = (tmp.join("control-trap-ran"), tmp.join("control.pid"));
        let r = run(
            &suite_with_trap(&marker, &pidfile, false),
            ceiled(&tmp, Some(SUITE_CEILING)),
        );
        assert!(!r.ok && r.output.contains("TIMEOUT"), "{}", r.output);
        let grandchild = read_pid(&pidfile);
        assert!(
            gone_within(&grandchild, GONE),
            "{grandchild} outlived the kill"
        );
        assert!(
            !marker.exists(),
            "a SIGKILLed shell ran its trap, so this test cannot tell the two apart"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// …and a child that ignores `SIGTERM` is still killed when its grace runs
    /// out: the grace delays the `SIGKILL`, never replaces it.
    #[cfg(unix)]
    #[test]
    fn a_graceful_child_that_ignores_sigterm_is_killed_when_its_grace_runs_out() {
        let tmp = crate::mktemp_dir("atv-ceil-grace-deaf").expect("mktemp");
        let (marker, pidfile) = (tmp.join("trap-ran"), tmp.join("grandchild.pid"));
        let t = Instant::now();
        let r = run(
            &suite_with_trap(&marker, &pidfile, true).term_grace(Duration::from_millis(700)),
            ceiled(&tmp, Some(SUITE_CEILING)),
        );
        let waited = t.elapsed();
        assert!(!r.ok && r.output.contains("TIMEOUT"), "{}", r.output);
        assert!(
            r.output
                .contains("SIGTERM first; the group outlived its 0.7s grace and was SIGKILLed"),
            "{}",
            r.output
        );
        assert!(
            waited >= SUITE_CEILING + Duration::from_millis(700),
            "the ceiling, then the whole grace: {waited:?}"
        );
        // A hang detector: a grace that never ends in the SIGKILL waits out the
        // deaf group's `sleep 600`.
        assert!(
            waited < SUITE_CEILING + HANG_DETECTOR,
            "the grace ran out and nothing SIGKILLed the group: {waited:?}"
        );
        let grandchild = read_pid(&pidfile);
        assert!(
            gone_within(&grandchild, GONE),
            "{grandchild} outlived the kill"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// An interrupted gate lets a graceful child run its EXIT trap: the stand-in
    /// gate of [`an_interrupted_gate_kills_every_live_stage_group`], running a
    /// suite-shaped child with a grace, gets `SIGINT`, its handler sends the
    /// child's group `SIGTERM`, the trap runs, the grandchild is gone, and the
    /// stand-in still dies of `SIGINT`. Negative control: no grace, no trap.
    ///
    /// Twice with a grace: the suite as it is, and the one whose grandchild is
    /// slow to exec ([`suite_slow_to_exec`]). The second is there for the
    /// stand-in's arming, not for the handler: a gate interrupted before that
    /// grandchild has exec'ed waits out its whole grace, which is the red
    /// [`run_armed`] records and no fault of the handler's.
    #[cfg(unix)]
    #[test]
    fn an_interrupted_gate_lets_a_graceful_child_run_its_exit_trap() {
        use std::os::unix::process::ExitStatusExt as _;
        const DIR: &str = "ATV_GRACE_INTERRUPT_DIR";
        const GRACE: &str = "ATV_GRACE_INTERRUPT_GRACEFUL";
        const SLOW_EXEC: &str = "ATV_GRACE_INTERRUPT_SLOW_EXEC";
        if let Some(dir) = std::env::var_os(DIR) {
            group::kill_on_interrupt();
            let dir = PathBuf::from(dir);
            let (marker, pidfile) = (dir.join("trap-ran"), dir.join("gc.pid"));
            let mut child = if std::env::var_os(SLOW_EXEC).is_some() {
                suite_slow_to_exec(&marker, &pidfile)
            } else {
                suite_with_trap(&marker, &pidfile, false)
            };
            if std::env::var_os(GRACE).is_some() {
                child = child.term_grace(TERM_GRACE);
            }
            run_armed(&child, &pidfile);
            return;
        }
        let tmp = crate::mktemp_dir("atv-grace-interrupt").expect("mktemp");
        let me = std::env::current_exe().expect("the test binary");
        let stand_in = |dir: &Path, shape: &[&str]| {
            std::fs::create_dir_all(dir).expect("mkdir");
            let mut c = std::process::Command::new(&me);
            c.args([
                "--exact",
                "exec::tests::an_interrupted_gate_lets_a_graceful_child_run_its_exit_trap",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(DIR, dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
            for flag in shape {
                c.env(flag, "1");
            }
            let child = c.spawn().expect("the stand-in gate starts");
            let (status, grandchild, took) = interrupt_when_armed(child, &dir.join("gc.pid"));
            (status, grandchild, took, dir.join("trap-ran").exists())
        };

        for (case, shape) in [
            ("graceful", &[GRACE][..]),
            ("slow-exec", &[GRACE, SLOW_EXEC]),
        ] {
            let (status, grandchild, took, trapped) = stand_in(&tmp.join(case), shape);
            assert_eq!(
                status.signal(),
                Some(2),
                "{case}: it still dies of SIGINT: {status:?}"
            );
            assert!(trapped, "{case}: the graceful child's EXIT trap ran");
            assert!(
                gone_within(&grandchild, GONE),
                "{case}: {grandchild} outlived the gate"
            );
            assert!(
                took < TERM_GRACE,
                "{case}: the handler stops waiting once the group has gone: {took:?}"
            );
        }

        let (status, grandchild, _, trapped) = stand_in(&tmp.join("plain"), &[]);
        assert_eq!(status.signal(), Some(2), "{status:?}");
        assert!(
            gone_within(&grandchild, GONE),
            "{grandchild} outlived the gate"
        );
        assert!(
            !trapped,
            "a plain child's trap ran too, so this test cannot tell the two apart"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// An interrupted gate sends a graced group ONE `SIGTERM`: a second would
    /// land in the middle of the EXIT trap the grace is for. The stand-in
    /// gate runs a suite whose trap takes 0.3 s ([`suite_with_slow_trap`])
    /// and is interrupted once: the trap finishes, and the gate goes when
    /// the group has, long before its grace is up. Measured 2026-10-13 on a
    /// handler that sent each graced group `SIGTERM` again, one round into
    /// the wait or on every round of it: no marker, 6 runs of 6 each.
    #[cfg(unix)]
    #[test]
    fn an_interrupted_gate_sends_a_graceful_group_one_sigterm() {
        use std::os::unix::process::ExitStatusExt as _;
        const DIR: &str = "ATV_ONE_SIGTERM_DIR";
        if let Some(dir) = std::env::var_os(DIR) {
            group::kill_on_interrupt();
            let dir = PathBuf::from(dir);
            let (marker, pidfile) = (dir.join("trap-ran"), dir.join("gc.pid"));
            run_armed(
                &suite_with_slow_trap(&marker, &pidfile).term_grace(TERM_GRACE),
                &pidfile,
            );
            return;
        }
        let tmp = crate::mktemp_dir("atv-one-sigterm").expect("mktemp");
        let mut stand_in =
            std::process::Command::new(std::env::current_exe().expect("the test binary"));
        stand_in
            .args([
                "--exact",
                "exec::tests::an_interrupted_gate_sends_a_graceful_group_one_sigterm",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(DIR, &tmp)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let child = stand_in.spawn().expect("the stand-in gate starts");
        let (status, grandchild, took) = interrupt_when_armed(child, &tmp.join("gc.pid"));
        assert_eq!(
            status.signal(),
            Some(2),
            "it still dies of SIGINT: {status:?}"
        );
        assert!(
            tmp.join("trap-ran").exists(),
            "the suite's slow EXIT trap was cut short: one SIGTERM, never a second"
        );
        assert!(
            gone_within(&grandchild, GONE),
            "{grandchild} outlived the gate"
        );
        assert!(
            took < TERM_GRACE,
            "the handler stops waiting once the group has gone: {took:?}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The variable that makes a test process the stand-in gate of the
    /// survivor tests ([`survivor_stand_in`]): the directory its files go in.
    #[cfg(unix)]
    const SURVIVOR_DIR: &str = "ATV_SECOND_INTERRUPT_DIR";

    /// Set beside [`SURVIVOR_DIR`], it keeps the stand-in's test thread alive
    /// once its stage has ended, as a gate's other stages keep theirs: a
    /// later signal then has a thread other than the handler's to land on.
    #[cfg(unix)]
    const SURVIVOR_STAYS: &str = "ATV_SECOND_INTERRUPT_STAYS";

    /// Be the stand-in gate of the survivor tests when [`SURVIVOR_DIR`] says
    /// so, and answer whether it did: install [`group::kill_on_interrupt`], run
    /// [`suite_with_survivor`] with a grace, and once that stage has ended
    /// leave `stage-ended` beside its pidfile. The stage ends when the first
    /// interrupt's `SIGTERM` has ended its shell and `run` has seen that
    /// (and left the shell unreaped: the gate is stopping, `group::Live`).
    /// So the marker says the survivor's group is off the live list, while
    /// the handler, on the main thread, waits out the grace that survivor
    /// holds.
    #[cfg(unix)]
    fn survivor_stand_in() -> bool {
        let Some(dir) = std::env::var_os(SURVIVOR_DIR) else {
            return false;
        };
        group::kill_on_interrupt();
        let dir = PathBuf::from(dir);
        let (marker, pidfile) = (dir.join("trap-ran"), dir.join("gc.pid"));
        run_armed(
            &suite_with_survivor(&marker, &pidfile).term_grace(TERM_GRACE),
            &pidfile,
        );
        std::fs::write(dir.join("stage-ended"), "ended\n").expect("the stage-ended marker");
        if std::env::var_os(SURVIVOR_STAYS).is_some() {
            // The handler ends this process inside its grace. The bound is
            // for a stand-in nobody interrupted.
            std::thread::sleep(TERM_GRACE + HANG_DETECTOR);
        }
        true
    }

    /// What a stand-in gate with a survivor did when it was interrupted.
    #[cfg(unix)]
    struct Interrupted {
        status: std::process::ExitStatus,
        /// From the first signal sent to the stand-in's exit.
        lived: Duration,
        /// From the last signal sent to the stand-in's exit.
        took: Duration,
        /// The suite's EXIT trap ran.
        trapped: bool,
        /// The survivor was gone once the stand-in had died.
        survivor_gone: bool,
    }

    /// Run `test`'s stand-in gate ([`survivor_stand_in`]) in `dir`, one that
    /// `stays` ([`SURVIVOR_STAYS`]) or not, and send it `first` once it has
    /// armed. Then, when `second` names a signal, send that once the
    /// stand-in's stage has ended (its `stage-ended` marker: an event, never
    /// a guessed delay). A survivor that outlives the stand-in is killed
    /// here, by pid, whatever the caller goes on to assert.
    #[cfg(unix)]
    fn interrupt_with_survivor(
        test: &str,
        dir: &Path,
        first: &str,
        second: Option<&str>,
        stays: bool,
    ) -> Interrupted {
        std::fs::create_dir_all(dir).expect("mkdir");
        let mut stand_in =
            std::process::Command::new(std::env::current_exe().expect("the test binary"));
        stand_in
            .args(["--exact", test, "--nocapture", "--test-threads=1"])
            .env(SURVIVOR_DIR, dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if stays {
            stand_in.env(SURVIVOR_STAYS, "1");
        }
        let mut child = stand_in.spawn().expect("the stand-in gate starts");
        let grandchild = armed_grandchild(&mut child, &dir.join("gc.pid"));
        let gate = child.id().to_string();
        let began = Instant::now();
        let mut t = began;
        send(first, &gate);
        if let Some(second) = second {
            let ended = dir.join("stage-ended");
            let deadline = Instant::now() + HANG_DETECTOR;
            while !ended.exists() {
                // The stand-in outlives its stage by its whole grace unless a
                // second signal comes: one already dead was sent none, and
                // that run tested nothing.
                if Instant::now() > deadline || matches!(child.try_wait(), Ok(Some(_))) {
                    let _ = child.kill();
                    let _ = child.wait();
                    send("KILL", &grandchild);
                    panic!(
                        "the stand-in gate died, or a minute passed, before its stage ended \
                         ({}): it was sent no second signal",
                        ended.display()
                    );
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            t = Instant::now();
            send(second, &gate);
        }
        let status = child.wait().expect("the stand-in exits");
        let (lived, took) = (began.elapsed(), t.elapsed());
        let survivor_gone = gone_within(&grandchild, GONE);
        if !survivor_gone {
            send("KILL", &grandchild);
        }
        Interrupted {
            status,
            lived,
            took,
            trapped: dir.join("trap-ran").exists(),
            survivor_gone,
        }
    }

    /// A second interrupt during the grace is the operator saying "now": the
    /// gate stops waiting, `SIGKILL`s what it sent `SIGTERM`, and dies of that
    /// second signal, whichever of the three each one was (bar a `SIGHUP`
    /// after a `SIGHUP`, which is one hangup). The stand-in's suite leaves a
    /// survivor ([`suite_with_survivor`]), and the second signal is sent once
    /// its shell has been reaped, so the survivor is in a group the live list
    /// no longer holds, which only the handler's own record of its `SIGTERM`s
    /// reaches. The control is
    /// [`an_interrupted_gate_kills_a_survivor_when_its_grace_runs_out`]: sent
    /// no second signal, this stand-in takes its whole grace.
    #[cfg(unix)]
    #[test]
    fn a_second_interrupt_ends_the_grace_and_takes_the_survivor() {
        use std::os::unix::process::ExitStatusExt as _;
        if survivor_stand_in() {
            return;
        }
        let tmp = crate::mktemp_dir("atv-second-interrupt").expect("mktemp");
        for (first, second, number) in [
            // Only where `group::catch` installs with `sigaction` (this is its
            // cfg). Under `signal(3)` a second interrupt of the number being
            // handled is held back on the handling thread, this stand-in has
            // no other thread to take it, and the pair waits out the whole
            // grace, as that arm's doc says (measured 2026-10-13 on a macOS
            // build of that arm: 10.7 s, red 2 of 2). The four pairs below
            // are what both arms promise.
            #[cfg(any(
                target_os = "macos",
                all(
                    target_os = "linux",
                    target_pointer_width = "64",
                    any(target_arch = "x86_64", target_arch = "aarch64"),
                    any(target_env = "gnu", target_env = "musl")
                )
            ))]
            ("INT", "INT", 2),
            ("INT", "TERM", 15),
            ("INT", "HUP", 1),
            ("TERM", "INT", 2),
            ("HUP", "INT", 2),
        ] {
            let case = format!("{first}-then-{second}");
            let it = interrupt_with_survivor(
                "exec::tests::a_second_interrupt_ends_the_grace_and_takes_the_survivor",
                &tmp.join(&case),
                first,
                Some(second),
                false,
            );
            assert_eq!(
                it.status.signal(),
                Some(number),
                "{case}: it dies of the second signal: {:?}",
                it.status
            );
            assert!(it.trapped, "{case}: the suite's EXIT trap ran");
            assert!(
                it.took < TERM_GRACE / 2,
                "{case}: the second signal ends the wait: {:?}",
                it.took
            );
            assert!(it.survivor_gone, "{case}: the survivor outlived the gate");
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// What a stand-in gate whose grace nothing ended did: it lived out its
    /// whole [`TERM_GRACE`] (the survivor's group never empties), `SIGKILL`ed
    /// what it had sent `SIGTERM`, although the live list no longer held it,
    /// and died of `signal`, the first one it was sent. `repeats` is how many
    /// further signals it was sent that end no grace: the handler's wait
    /// counts rounds, and each of those still ends the round it lands in
    /// ([`group::NAP_MS`]).
    #[cfg(unix)]
    fn assert_the_grace_ran_out(it: &Interrupted, signal: i32, repeats: u32) {
        use std::os::unix::process::ExitStatusExt as _;
        assert_eq!(
            it.status.signal(),
            Some(signal),
            "it dies of the signal that interrupted it: {:?}",
            it.status
        );
        assert!(it.trapped, "the suite's EXIT trap ran");
        let whole = TERM_GRACE - Duration::from_millis(u64::from(group::NAP_MS)) * repeats;
        assert!(
            it.lived >= whole,
            "a survivor holds the gate for its whole grace: {:?}",
            it.lived
        );
        // A hang detector: a grace that never ends in the SIGKILL leaves the
        // gate waiting on the survivor's `sleep 600`.
        assert!(
            it.lived < TERM_GRACE + HANG_DETECTOR,
            "the grace ran out and the gate did not die: {:?}",
            it.lived
        );
        assert!(it.survivor_gone, "the survivor outlived the gate");
    }

    /// …and sent no second interrupt, the gate waits out its whole grace
    /// before the `SIGKILL` ([`assert_the_grace_ran_out`]).
    #[cfg(unix)]
    #[test]
    fn an_interrupted_gate_kills_a_survivor_when_its_grace_runs_out() {
        if survivor_stand_in() {
            return;
        }
        let tmp = crate::mktemp_dir("atv-grace-survivor").expect("mktemp");
        let it = interrupt_with_survivor(
            "exec::tests::an_interrupted_gate_kills_a_survivor_when_its_grace_runs_out",
            &tmp,
            "INT",
            None,
            false,
        );
        assert_the_grace_ran_out(&it, 2, 0);
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A hangup told twice is one hangup: a `SIGHUP` after a first `SIGHUP`
    /// (a closing terminal hangs its foreground job up more than once) is not
    /// a second interrupt, and the suites keep their grace, less the one
    /// round of the wait that repeat ends. The stand-in
    /// stays up ([`SURVIVOR_STAYS`]), so that second `SIGHUP` finds what it
    /// finds in a gate: more threads than the one the handler runs on.
    #[cfg(unix)]
    #[test]
    fn a_second_hangup_is_the_same_hangup_and_ends_no_grace() {
        if survivor_stand_in() {
            return;
        }
        let tmp = crate::mktemp_dir("atv-second-hangup").expect("mktemp");
        let it = interrupt_with_survivor(
            "exec::tests::a_second_hangup_is_the_same_hangup_and_ends_no_grace",
            &tmp,
            "HUP",
            Some("HUP"),
            true,
        );
        assert_the_grace_ran_out(&it, 1, 1);
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The variable that makes a test process the stand-in gate of
    /// [`an_interrupt_takes_the_child_it_caught_being_spawned`]: the
    /// directory its files go in.
    #[cfg(unix)]
    const WINDOW_DIR: &str = "ATV_SPAWN_WINDOW_DIR";

    /// Set beside [`WINDOW_DIR`], it moves that stand-in's hold from after
    /// its child's spawn to before `finish` has read whether the gate is
    /// stopping ([`hold_until_interrupted`]).
    #[cfg(unix)]
    const WINDOW_EARLY: &str = "ATV_SPAWN_WINDOW_EARLY";

    /// Set beside [`WINDOW_DIR`], it has that stand-in's stage interrupt the
    /// gate a second time from inside the window, once the first interrupt
    /// has it stopping, and stay there a while longer
    /// ([`hold_until_interrupted`]).
    #[cfg(unix)]
    const WINDOW_TWICE: &str = "ATV_SPAWN_WINDOW_TWICE";

    /// How long that stage stays in the window after its second interrupt:
    /// some seventy rounds of the handler's wait for it, and a handler that
    /// took a second interrupt for the end of that wait would be gone in
    /// one. A window that can only go vacuous, as [`STILL_THERE`] is, and
    /// it comes out of the second or so the handler waits
    /// (`group::SPAWN_ROUNDS`): kept short.
    #[cfg(unix)]
    const STAYS_HELD: Duration = Duration::from_millis(100);

    /// Send the calling thread signal `sig`, as `raise(3)` does: a handler
    /// for it runs on this thread, and has returned by the time this does.
    #[cfg(unix)]
    fn raise_here(sig: i32) {
        unsafe extern "C" {
            fn raise(sig: i32) -> i32;
        }
        // SAFETY: `raise` takes an integer and touches no memory of ours.
        unsafe {
            raise(sig);
        }
    }

    /// What a test process does at the two places `finish` calls
    /// [`in_the_window`] from: nothing, unless it is a stand-in that set one.
    #[cfg(unix)]
    static HOLD: std::sync::OnceLock<fn(Option<u32>)> = std::sync::OnceLock::new();

    /// `finish`'s two calls from inside the spawn window, where its spawn is
    /// counted in flight ([`group::Spawning`]) and the live list does not
    /// hold its child: with `None` as soon as the spawn is counted, before
    /// it reads whether the gate is stopping, and with the child's pid once
    /// there is one. A stand-in gate that set [`HOLD`] keeps the stage
    /// thread at one of the two; every other test process passes through.
    #[cfg(unix)]
    pub(super) fn in_the_window(child: Option<u32>) {
        if let Some(hold) = HOLD.get() {
            hold(child);
        }
    }

    /// THE RULE, CHECKED AT EVERY SIGNAL: the gate signals a process group
    /// only while it holds that group's leader as its own child, running or
    /// unreaped (`group::Live`). `group::send`, the one place the runner
    /// calls `killpg`, comes here first in a test build, so every test that
    /// kills at a ceiling or interrupts a stand-in gate checks each signal
    /// it causes, the handler's among them, and the question (signal 0)
    /// too. The check is the kernel's own word on it: `waitid` with
    /// `WNOWAIT` for the group's id answers for a child of this process,
    /// running or a zombie, and `ECHILD` for anything else, a pid some other
    /// process was handed included.
    ///
    /// A signal that would break the rule is not sent. In a test's own
    /// process that is a panic, which names the group. In a stand-in gate
    /// that is stopping it may be the signal handler asking, where nothing
    /// can be formatted or unwound: the stand-in dies there, of `SIGKILL`,
    /// and the test that expected it to die of its interrupt reads signal 9
    /// in its place.
    #[cfg(unix)]
    pub(super) fn signalling(g: i32, sig: i32) {
        // A scripted group that no process is in: nobody to reach.
        if g >= NOBODY {
            return;
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if group::exited(g, false).is_err() {
            if group::stopping() {
                raise_here(9);
            }
            panic!(
                "signal {sig} was about to go to process group {g}, whose leader this process \
                 does not hold: it was reaped, or was never its child"
            );
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let _ = sig;
    }

    /// What a test process does at the two places `group::Live::reap` calls
    /// [`at_the_door`] from: nothing, unless it is a stand-in that set one.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    static DOOR: std::sync::OnceLock<fn(bool)> = std::sync::OnceLock::new();

    /// The two calls from inside the one door a stage child is reaped
    /// through, both with the child's exit seen and the child unreaped:
    /// with `false` while its group is still on the live list, and with
    /// `true` once the group has left it and before the door reads whether
    /// the gate is stopping. A stand-in gate that set [`DOOR`] keeps the
    /// stage thread at one of the two; every other test process passes
    /// through.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub(super) fn at_the_door(left: bool) {
        if let Some(hold) = DOOR.get() {
            hold(left);
        }
    }

    /// Run `what` on a thread of its own and answer what it returned. A
    /// hang detector for a test that calls the repeated kill in its own
    /// process: rounds with no end would otherwise hang the whole test
    /// binary, and here they fail one test a minute on. (The thread is left
    /// to its rounds, which signal a group nobody is in, or one whose
    /// leader that same thread holds unreaped.)
    #[cfg(unix)]
    fn within_the_hang_detector<T: Send + 'static>(what: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        let running = std::thread::spawn(move || {
            let _ = tx.send(what());
        });
        match rx.recv_timeout(HANG_DETECTOR) {
            Ok(done) => done,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!(
                "still running a minute on: the repeated kill did not end at its bound \
                 (`group::KILL_ROUNDS`)"
            ),
            // It panicked: its own message is the test's.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => match running.join() {
                Err(panic) => std::panic::resume_unwind(panic),
                Ok(()) => unreachable!("a thread that returned had sent its answer"),
            },
        }
    }

    /// What a stage in the spawn window finds of its gate: the groups on the
    /// live list and the spawns in flight.
    #[cfg(unix)]
    fn found_in_the_window() -> String {
        format!(
            "{} live, {} in flight\n",
            group::live_count(),
            group::in_flight()
        )
    }

    /// The [`HOLD`] of the spawn-window stand-in: keep the stage thread in
    /// the window until the gate has been interrupted, and leave what the
    /// test reads.
    ///
    /// `held`, once it is holding: the event to interrupt on, and what the
    /// stage found there before any interrupt ([`found_in_the_window`]). It
    /// is renamed into place, so whoever sees it sees it whole. At the later
    /// place that is when the suite's grandchild has exec'ed its `sleep`,
    /// as [`run_armed`] arms from outside, so the interrupt finds a child
    /// with its trap installed and a grandchild running, in a group the
    /// live list does not hold. At the earlier place ([`WINDOW_EARLY`]) it
    /// is at once, and the interrupt finds a spawn that is counted and that
    /// `finish` has not yet decided to make.
    ///
    /// `suite.pid`, once the child exists. And then, if the gate is
    /// stopping, `in-the-window`: what the interrupt left the stage to find.
    /// Under [`WINDOW_TWICE`] the stage first interrupts the gate again and
    /// stays where it is for [`STAYS_HELD`], its spawn still in flight. That
    /// second interrupt is a `SIGTERM` to its own thread ([`raise_here`]),
    /// so the frame that records it has run before the stage goes on: what
    /// it writes then, it writes in a gate that has been told "now" and is
    /// still waiting for this spawn.
    ///
    /// [`group::stopping`] turning true is an event, and the wait for it
    /// ends by itself in a stand-in nobody interrupted.
    #[cfg(unix)]
    fn hold_until_interrupted(child: Option<u32>) {
        let dir = PathBuf::from(std::env::var_os(WINDOW_DIR).expect("only a stand-in holds"));
        if let Some(suite) = child {
            std::fs::write(dir.join("suite.pid"), format!("{suite}\n")).expect("the suite's pid");
        }
        let early = std::env::var_os(WINDOW_EARLY).is_some();
        if early == child.is_none() {
            let pidfile = dir.join("gc.pid");
            let deadline = Instant::now() + HANG_DETECTOR;
            let mut is_held = false;
            while !group::stopping() && Instant::now() < deadline {
                if is_held {
                    std::thread::sleep(Duration::from_millis(1));
                } else if early
                    || written_pid(&pidfile).is_some_and(|pid| has_become(&pid, "sleep"))
                {
                    let part = dir.join("held.part");
                    std::fs::write(&part, found_in_the_window()).expect("the held marker");
                    std::fs::rename(&part, dir.join("held")).expect("the held marker");
                    is_held = true;
                } else {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
        if child.is_some() && group::stopping() {
            if std::env::var_os(WINDOW_TWICE).is_some() {
                raise_here(15);
                std::thread::sleep(STAYS_HELD);
            }
            std::fs::write(dir.join("in-the-window"), found_in_the_window())
                .expect("what the interrupt found");
        }
    }

    /// An interrupt that lands inside a spawn takes that child too. Between
    /// its spawn and its group's place on the live list a stage child is one
    /// the handler's sweeps cannot see, and until 2026-10-13 it ran on after
    /// the gate had died (`group::Spawning` has the measurement). The
    /// stand-in gate holds its stage's spawn in that window
    /// ([`hold_until_interrupted`]) and is interrupted there, with nothing
    /// on its live list: the handler waits for the spawn, and the suite and
    /// its grandchild are gone when the stand-in is. Measured 2026-10-13 on
    /// the handler as it was, and on this one without its wait, without its
    /// last sweep, and with the count lowered before the child is on the
    /// list: the suite outlived the gate, 9 runs of 9.
    ///
    /// Twice that way, without a grace and with one. Either way it is
    /// `SIGKILL`: the trap of a child born as the gate was interrupted does
    /// not run, which is read once the suite itself has gone. What a
    /// `SIGTERM` does to this same suite is
    /// [`an_interrupted_gate_lets_a_graceful_child_run_its_exit_trap`].
    ///
    /// And once with the hold earlier ([`WINDOW_EARLY`]): `finish` has
    /// counted its spawn and has not yet read whether the gate is stopping.
    /// Interrupted there it reads that it is, and starts no child. This is
    /// the case that pins the order of those two steps, the count and then
    /// the look. Held between them, the stage says what it found before any
    /// interrupt, and it is its own spawn in flight: a count raised after
    /// the look is not there to find, and a look taken before the hold
    /// starts the child. Measured 2026-10-13: with the count raised after
    /// the look the stage found no spawn in flight, 3 runs of 3; with the
    /// look taken before the hold, or never acted on, it started its child,
    /// 9 runs of 9.
    ///
    /// And once interrupted twice ([`WINDOW_TWICE`]): held with its child
    /// off the list, the stage sends the gate a `SIGTERM` as soon as the
    /// `SIGINT` has it stopping, and stays held. A second interrupt ends a
    /// grace; it does not end the wait for a spawn in flight, which is all
    /// that stands between "now" and a child left running. So the gate is
    /// still there when the stage looks again, takes the child once it is
    /// listed, and dies of the second signal. Measured 2026-10-13 with that
    /// wait ending on a second interrupt as the grace does: the suite
    /// outlived the gate, 5 runs of 5.
    #[cfg(unix)]
    #[test]
    fn an_interrupt_takes_the_child_it_caught_being_spawned() {
        use std::os::unix::process::ExitStatusExt as _;
        const GRACE: &str = "ATV_SPAWN_WINDOW_GRACEFUL";
        if let Some(dir) = std::env::var_os(WINDOW_DIR) {
            group::kill_on_interrupt();
            HOLD.set(hold_until_interrupted).expect("the one hold");
            let dir = PathBuf::from(dir);
            let (marker, pidfile) = (dir.join("trap-ran"), dir.join("gc.pid"));
            let mut child = if std::env::var_os(WINDOW_EARLY).is_some() {
                // Never started: the gate is stopping by the time `finish`
                // looks. It forks nothing all the same, for the run that
                // does start it, a red one: that child is SIGKILLed within a
                // millisecond or two of its spawn, and a shell killed while
                // it forks its `sleep` left that `sleep` behind (`killpg`
                // reaches the shell, not the process it is creating: one
                // `sleep 600` left in ten runs, 2026-10-13, before a kill
                // was repeated, `group::KILL_ROUNDS`).
                Cmd::new("/bin/sleep").args(["600"])
            } else {
                suite_with_trap(&marker, &pidfile, false)
            };
            if std::env::var_os(GRACE).is_some() {
                child = child.term_grace(TERM_GRACE);
            }
            let _ = run(&child, ceiled(&dir, None));
            return;
        }
        let tmp = crate::mktemp_dir("atv-spawn-window").expect("mktemp");
        let me = std::env::current_exe().expect("the test binary");
        for (case, shape, number) in [
            ("plain", &[][..], 2),
            ("graceful", &[GRACE], 2),
            ("early", &[WINDOW_EARLY], 2),
            ("twice", &[WINDOW_TWICE], 15),
        ] {
            let dir = tmp.join(case);
            std::fs::create_dir_all(&dir).expect("mkdir");
            let mut stand_in = std::process::Command::new(&me);
            stand_in
                .args([
                    "--exact",
                    "exec::tests::an_interrupt_takes_the_child_it_caught_being_spawned",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(WINDOW_DIR, &dir)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            for flag in shape {
                stand_in.env(flag, "1");
            }
            let mut child = stand_in.spawn().expect("the stand-in gate starts");
            // The stage's child and its grandchild, where it has one, as far
            // as each has been written down.
            let written = || {
                ["suite.pid", "gc.pid"]
                    .iter()
                    .filter_map(|file| written_pid(&dir.join(file)))
                    .collect::<Vec<_>>()
            };
            let held = dir.join("held");
            let deadline = Instant::now() + HANG_DETECTOR;
            while !held.exists() {
                if Instant::now() > deadline || matches!(child.try_wait(), Ok(Some(_))) {
                    let _ = child.kill();
                    let _ = child.wait();
                    for pid in written() {
                        send("KILL", &pid);
                    }
                    panic!(
                        "{case}: the stand-in gate never held its spawn ({})",
                        held.display()
                    );
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            send("INT", &child.id().to_string());
            let status = child.wait().expect("the stand-in exits");
            // What the gate left running is killed here, by pid, whatever is
            // asserted next. One hang detector for all of it.
            let gone_by = Instant::now() + GONE;
            let left: Vec<_> = written()
                .into_iter()
                .filter(|pid| !gone_within(pid, gone_by.saturating_duration_since(Instant::now())))
                .collect();
            for pid in &left {
                send("KILL", pid);
            }
            assert_eq!(
                status.signal(),
                Some(number),
                "{case}: it still dies of its interrupt, the last one it was sent: {status:?}"
            );
            assert!(
                left.is_empty(),
                "{case}: the child being spawned when the interrupt landed outlived the gate: \
                 {left:?}"
            );
            assert_eq!(
                std::fs::read_to_string(&held).unwrap_or_default(),
                "0 live, 1 in flight\n",
                "{case}: what the stage found where it was held, before the interrupt: nothing \
                 of its own on the live list, and its spawn already in flight"
            );
            if case == "early" {
                assert!(
                    !dir.join("suite.pid").exists(),
                    "{case}: the interrupt landed before `finish` had read whether the gate \
                     was stopping, and it started its child all the same"
                );
            } else {
                assert_eq!(
                    std::fs::read_to_string(dir.join("in-the-window")).unwrap_or_default(),
                    "0 live, 1 in flight\n",
                    "{case}: what the stage found once its child existed: the gate stopping, \
                     nothing on the live list, its own spawn in flight (nothing at all: the \
                     gate died without waiting for the spawn)"
                );
            }
            assert!(
                !dir.join("trap-ran").exists(),
                "{case}: a child caught in its spawn is SIGKILLed, with a grace or without"
            );
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The variable that makes a test process the stand-in gate of
    /// [`an_interrupt_taken_on_the_spawning_thread_still_ends_the_gate`]:
    /// the directory its files go in.
    #[cfg(unix)]
    const SAME_THREAD_DIR: &str = "ATV_SPAWNING_THREAD_DIR";

    /// The [`HOLD`] of that stand-in: once the stage's child exists, leave
    /// its pid and what the stage finds there ([`found_in_the_window`]),
    /// and interrupt this very thread. `raise` signals the thread that
    /// calls it, so the handler's frame sits on top of the thread whose
    /// spawn is counted in flight, and nothing runs underneath it to lower
    /// that count.
    #[cfg(unix)]
    fn interrupt_the_spawning_thread(child: Option<u32>) {
        let Some(suite) = child else {
            return;
        };
        let dir = PathBuf::from(std::env::var_os(SAME_THREAD_DIR).expect("only a stand-in holds"));
        std::fs::write(dir.join("suite.pid"), format!("{suite}\n")).expect("the child's pid");
        std::fs::write(dir.join("held"), found_in_the_window()).expect("the held marker");
        raise_here(2);
    }

    /// The handler's wait for a spawn in flight ends by itself. An interrupt
    /// taken on the very thread that is spawning stops that thread under the
    /// handler's frame, so the count the frame waits on cannot drop
    /// (`group::Spawning`, what is left), and only the wait's own bound lets
    /// the gate die of its interrupt. The stand-in gate's stage interrupts
    /// itself in the window, once its child exists
    /// ([`interrupt_the_spawning_thread`]), and the gate is dead of `SIGINT`
    /// inside a minute: a hang detector, for a wait that takes a second.
    /// Measured 2026-10-13 with the bound taken out of that wait: the
    /// stand-in was alive a minute later, 3 runs of 3.
    ///
    /// That child is the one the gate leaves behind in this case, which is
    /// why it is killed here, by pid, and not asserted gone.
    #[cfg(unix)]
    #[test]
    fn an_interrupt_taken_on_the_spawning_thread_still_ends_the_gate() {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(dir) = std::env::var_os(SAME_THREAD_DIR) {
            group::kill_on_interrupt();
            HOLD.set(interrupt_the_spawning_thread)
                .expect("the one hold");
            let dir = PathBuf::from(dir);
            let _ = run(&Cmd::new("/bin/sleep").args(["600"]), ceiled(&dir, None));
            return;
        }
        let tmp = crate::mktemp_dir("atv-spawning-thread").expect("mktemp");
        let mut stand_in =
            std::process::Command::new(std::env::current_exe().expect("the test binary"));
        stand_in
            .args([
                "--exact",
                "exec::tests::an_interrupt_taken_on_the_spawning_thread_still_ends_the_gate",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(SAME_THREAD_DIR, &tmp)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = stand_in.spawn().expect("the stand-in gate starts");
        let deadline = Instant::now() + HANG_DETECTOR;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                _ if Instant::now() > deadline => break None,
                _ => std::thread::sleep(Duration::from_millis(10)),
            }
        };
        if status.is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(pid) = written_pid(&tmp.join("suite.pid")) {
            send("KILL", &pid);
        }
        assert_eq!(
            std::fs::read_to_string(tmp.join("held")).unwrap_or_default(),
            "0 live, 1 in flight\n",
            "what the stage found before it interrupted itself: its child off the live list, \
             and its own spawn in flight"
        );
        assert_eq!(
            status.and_then(|status| status.signal()),
            Some(2),
            "the gate dies of its interrupt once the wait for a spawn in flight has run out \
             (`None`: it was alive a minute later, and was killed here): {status:?}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// Once the gate has been interrupted it starts no child, and no way out
    /// of a spawn leaves that spawn counted in flight (a count left raised
    /// is a second the handler waits for nothing). The stand-in gate runs a
    /// child that exits and one that cannot start, then the survivor suite
    /// ([`suite_with_survivor`]), and is interrupted there: the survivor
    /// holds the handler in its grace, so the gate is stopping and still
    /// alive when its stage asks for one more child. That one is refused
    /// before it is spawned. A second signal then ends the grace.
    ///
    /// The way out that every stage takes is its child reaching the live
    /// list, and that one is read while the suite runs, before anything
    /// interrupts it: once `finish` has returned, a count it kept for as
    /// long as its child ran reads zero too. Measured 2026-10-13 with the
    /// count kept that long: one in flight while the suite ran, 3 runs of 3.
    #[cfg(unix)]
    #[test]
    fn an_interrupted_gate_starts_no_child_and_leaves_no_spawn_in_flight() {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(dir) = std::env::var_os(SURVIVOR_DIR) {
            group::kill_on_interrupt();
            let dir = PathBuf::from(dir);
            let ran = run(&Cmd::new("/bin/sh").args(["-c", ":"]), ceiled(&dir, None));
            let after_ran = group::in_flight();
            let missing = run(&Cmd::new(dir.join("no-such-program")), ceiled(&dir, None));
            let after_missing = group::in_flight();
            let (marker, pidfile) = (dir.join("trap-ran"), dir.join("gc.pid"));
            // The suite is on the live list and nothing has interrupted it,
            // so its spawn is over and the count drops, which is an event:
            // half the minute this stand-in has to arm in, so that a count
            // that stays is what gets reported.
            let mut while_running = 0;
            run_armed_after(
                &suite_with_survivor(&marker, &pidfile).term_grace(TERM_GRACE),
                &pidfile,
                || {
                    let deadline = Instant::now() + HANG_DETECTOR / 2;
                    while group::in_flight() != 0 && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    while_running = group::in_flight();
                },
            );
            let late = Cmd::new("/bin/sh").args([
                "-c",
                &format!(
                    "echo started > '{}'; exec sleep 600",
                    dir.join("late-started").display()
                ),
            ]);
            let refused = run(&late, ceiled(&dir, None));
            let report = format!(
                "a child that exited: ok={}, then {after_ran} in flight\n\
                 a child that could not start: refused={}, then {after_missing} in flight\n\
                 a child that is running: {while_running} in flight\n\
                 a child after the interrupt: {:?}, then {} in flight\n",
                ran.ok,
                missing.spawn_error.is_some(),
                refused.spawn_error,
                group::in_flight()
            );
            std::fs::write(dir.join("late"), report).expect("the report");
            std::fs::write(dir.join("stage-ended"), "ended\n").expect("the stage-ended marker");
            return;
        }
        let tmp = crate::mktemp_dir("atv-late-child").expect("mktemp");
        let it = interrupt_with_survivor(
            "exec::tests::an_interrupted_gate_starts_no_child_and_leaves_no_spawn_in_flight",
            &tmp,
            "INT",
            Some("TERM"),
            false,
        );
        assert_eq!(
            it.status.signal(),
            Some(15),
            "it dies of the second signal: {:?}",
            it.status
        );
        assert!(it.survivor_gone, "the survivor outlived the gate");
        assert_eq!(
            std::fs::read_to_string(tmp.join("late")).unwrap_or_default(),
            "a child that exited: ok=true, then 0 in flight\n\
             a child that could not start: refused=true, then 0 in flight\n\
             a child that is running: 0 in flight\n\
             a child after the interrupt: \
             Some(\"the gate was interrupted before this child started\"), then 0 in flight\n"
        );
        assert!(
            !tmp.join("late-started").exists(),
            "a child was started after the interrupt"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The door's first step, asked of the kernel itself: a child's exit is
    /// seen without reaping it (`group::exited`, which is `waitid` with
    /// `WNOWAIT`, by numbers this crate declares for itself). Everything
    /// `group::Live` promises stands on it: an exited child that is still a
    /// zombie afterwards, its pid still this process's and so its group's id
    /// too. A number read wrong would reap here, and the zombie would be gone.
    ///
    /// And a child that is only stopped has not exited, asked once or
    /// waited for: a stage's `ssh` asking for a passphrase is stopped by the
    /// terminal, and a wait that took that for an exit would take its group
    /// off the live list with the child still there. macOS answers a
    /// `waitid` that asked for exits alone for a stopped child too
    /// (measured 2026-10-13: this test, reading no more than whether the
    /// kernel had answered, took the stopped child for exited), so the
    /// answer is read for what it says. The wait that must not return is
    /// given a fifth of a second, a window that can only go vacuous: kept
    /// short.
    ///
    /// The child is ended before anything is asserted, so a red run leaves
    /// no stopped `sleep` behind.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_childs_exit_is_seen_without_reaping_it() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("600")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("the child starts");
        let (pid, id) = (
            i32::try_from(child.id()).expect("a pid"),
            child.id().to_string(),
        );
        let running = group::exited(pid, false).ok();
        send("STOP", &id);
        let deadline = Instant::now() + HANG_DETECTOR;
        while !ps("stat=", &id).starts_with('T') && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let stopped = (ps("stat=", &id), group::exited(pid, false).ok());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(group::exited(pid, true).ok());
        });
        let early = rx.recv_timeout(Duration::from_millis(200)).ok();
        send("KILL", &id);
        let waited = rx.recv_timeout(HANG_DETECTOR).ok();
        let killed = (ps("stat=", &id), group::exited(pid, false).ok());
        let reaped = child.wait();
        let after = group::exited(pid, false);

        assert_eq!(running, Some(false), "a running child has not exited");
        assert!(
            stopped.0.starts_with('T'),
            "the child never stopped: its `ps` state was {:?}",
            stopped.0
        );
        assert_eq!(stopped.1, Some(false), "a stopped child has not exited");
        assert_eq!(
            early, None,
            "the wait for its exit returned for a child that was only stopped"
        );
        assert_eq!(waited, Some(Some(true)), "its exit ends the wait");
        assert_eq!(
            killed.1,
            Some(true),
            "and it has exited, however often that is asked"
        );
        assert!(
            killed.0.starts_with('Z'),
            "asking left the child unreaped, a zombie whose pid is still this process's: its \
             `ps` state was {:?}",
            killed.0
        );
        assert!(reaped.is_ok(), "and it is there to reap: {reaped:?}");
        assert!(
            after.is_err(),
            "once it is reaped the kernel says it is no child of this process: {after:?}"
        );
    }

    /// Once `group::Live` has reaped its child it sends the group nothing:
    /// the number is the kernel's to hand out again, so `term`, `kill`,
    /// `alive` and `gone` answer without signalling, and asking after the
    /// exit fails. That is how ordinary code keeps the rule by construction
    /// (`group::Live`). A `Live` that went on sending would not send here:
    /// a test build checks every group signal against the rule before it
    /// goes out ([`signalling`]), and this test would fail on that panic,
    /// which names the group. The control: before the reap the same `Live`
    /// reaches its group, and kills it.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_live_group_sends_nothing_once_its_child_is_reaped() {
        use std::os::unix::process::CommandExt as _;
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("600")
            .process_group(0)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("the child starts");
        let mut live = group::Live::enter(child.id(), false);
        let reached = live.alive();
        let killed = live.kill();
        let reaped = live.reap(true).map(|r| r.map(|(status, _)| status));
        if !matches!(reaped, Ok(Some(_))) {
            // Never left running, whatever is asserted next.
            let _ = child.kill();
        }
        // Reaps it if the door did not; once the door has, the kernel
        // answers that it is no child of this process.
        let _ = child.wait();
        let (term, kill, alive, gone, exited) = (
            live.term(),
            live.kill(),
            live.alive(),
            live.gone(),
            live.exited(false),
        );
        assert!(reached, "the control: before the reap the group answers");
        assert!(killed.is_ok(), "and is killed: {killed:?}");
        assert!(
            matches!(reaped, Ok(Some(_))),
            "the door reaps the killed child: {reaped:?}"
        );
        let refused = |what: &str, answer: std::io::Result<()>| {
            assert!(
                answer
                    .as_ref()
                    .is_err_and(|e| e.to_string().contains("no longer the gate's")),
                "{what} after the reap: {answer:?}"
            );
        };
        refused("term", term);
        refused("kill", kill.map(|_| ()));
        refused("asking after the exit", exited.map(|_| ()));
        assert!(!alive, "a reaped child's group reads as having nobody");
        assert!(gone, "and as gone, at once");
    }

    /// The variable that makes a test process the stand-in gate of
    /// [`a_stage_child_an_interrupt_may_have_seen_is_never_reaped`]: the
    /// directory its files go in.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    const DOOR_DIR: &str = "ATV_DOOR_DIR";

    /// Set beside [`DOOR_DIR`]: where that stand-in holds its stage in the
    /// door ([`at_the_door`]). `listed` and `ceiling` hold it with its
    /// child's exit seen and its group still on the live list, a child that
    /// exited by itself and one a ceiling killed; `left` and `beside` hold
    /// it once the group has left the list, before the door reads whether
    /// the gate is stopping, `beside` with a second stage's group on the
    /// list beside it.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    const DOOR_CASE: &str = "ATV_DOOR_CASE";

    /// Is the second stage of the `beside` case ([`DOOR_CASE`]) in place: its
    /// `sleep`, deaf to `SIGTERM`, exec'ed and leading its own group, and
    /// that group the one on the live list? (The held stage's group has
    /// left it by then.)
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn the_stage_beside_is_listed(dir: &Path) -> bool {
        written_pid(&dir.join("beside.pid")).is_some_and(|pid| has_become(&pid, "sleep"))
            && group::live_count() == 1
            && group::in_flight() == 0
    }

    /// The [`DOOR`] of that stand-in: keep the stage thread at its case's
    /// place in the door until the gate has been interrupted, and leave
    /// `held` once it is holding there: the event to interrupt on, and what
    /// the stage found of the live list ([`found_in_the_window`]), renamed
    /// into place so whoever sees it sees it whole. The suite's leader has
    /// exited by then (the door is past that), so `held` waits only for the
    /// `sleep` it left behind to have exec'ed, as [`run_armed`] does, and
    /// at `beside` for the stage beside it to be in place too
    /// ([`the_stage_beside_is_listed`]); the `ceiling` case left nothing
    /// behind and holds at once.
    ///
    /// [`group::stopping`] turning true is an event, and the wait for it
    /// ends by itself in a stand-in nobody interrupted.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn hold_at_the_door(left: bool) {
        let dir = PathBuf::from(std::env::var_os(DOOR_DIR).expect("only a stand-in holds"));
        let case = std::env::var(DOOR_CASE).expect("the stand-in's case");
        if left != matches!(case.as_str(), "left" | "beside") {
            return;
        }
        let pidfile = dir.join("gc.pid");
        let deadline = Instant::now() + HANG_DETECTOR;
        let mut is_held = false;
        while !group::stopping() && Instant::now() < deadline {
            if is_held {
                std::thread::sleep(Duration::from_millis(1));
            } else if case == "ceiling"
                || (written_pid(&pidfile).is_some_and(|pid| has_become(&pid, "sleep"))
                    && (case != "beside" || the_stage_beside_is_listed(&dir)))
            {
                let part = dir.join("held.part");
                std::fs::write(&part, found_in_the_window()).expect("the held marker");
                std::fs::rename(&part, dir.join("held")).expect("the held marker");
                is_held = true;
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    /// The gate signals a group only while it holds the group's leader, and
    /// the interrupt handler signals the ids it read off the live list for
    /// seconds afterwards. So a stage child whose group the handler may
    /// have read is never reaped, and one it cannot have read is not the
    /// gate's to signal (`group::Live` has the rule and the argument; until
    /// 2026-10-13 a stage thread reaped at once, and a stand-in gate
    /// `SIGKILL`ed a group that was not its own). The door a child is
    /// reaped through takes its group off the list and THEN reads whether
    /// the gate is stopping, and the stand-in gate holds its stage on
    /// either side of that first step ([`hold_at_the_door`]), with the
    /// child's exit seen and the child unreaped, and is interrupted there:
    ///
    /// - `listed`: the group is still on the list. The handler reads it
    ///   and sends its `SIGTERM` (the suite asked for a grace, and left a
    ///   `sleep` that ignores it, which holds the handler in that grace).
    ///   The stage goes on, finds the gate stopping, and does not reap: its
    ///   child is still a zombie when the stage has ended, and the stage
    ///   was told the gate was interrupted. A second signal then ends the
    ///   grace, the handler `SIGKILL`s the group it remembered, the `sleep`
    ///   is gone, and every one of those signals found the leader held
    ///   ([`signalling`] ends a stand-in that would send one that did not:
    ///   it dies of signal 9, not of its interrupt).
    /// - `left`: the group is off the list. The handler finds no group
    ///   there, signals nothing and the gate dies at once: the `sleep` the
    ///   suite left runs on, as that of any stage that ended before the
    ///   interrupt does, and is ended here by pid.
    /// - `ceiling`: `listed` again, reached from a ceiling kill, which
    ///   reaps through the same door. The handler reads the group and
    ///   kills it, and again through its rounds; each signal found the
    ///   leader held. A ceiling kill that reaped any other way would never
    ///   be held there at all.
    /// - `beside`: `left` again, with a second stage running beside it
    ///   whose suite asked for a grace and ignores its `SIGTERM`. The
    ///   handler reads that group, not the held one, and waits in its
    ///   grace, so the gate lives on and what the held stage does next can
    ///   be read: it finds the gate stopping and keeps its child, as at
    ///   `listed`, because the door reads the flag only after the group
    ///   has left the list. A door that read the flag first would have read
    ///   it clear before the hold and would reap here. That order is the
    ///   one that breaks the rule, by a window no hold of this test can
    ///   reach: between that read and the leave, the handler can claim,
    ///   read the group off the list and go on signalling it after the reap.
    ///   This case pins the order instead. A second signal then ends the
    ///   grace and the handler `SIGKILL`s the group beside, which it read.
    ///
    /// Measured 2026-10-13 with the door changed, one run each. Reaping
    /// although the gate is stopping: this test red at `listed`, and nine
    /// older interrupt tests with it, each stand-in dead of signal 9.
    /// Reading whether the gate is stopping before the group has left the
    /// list, and leaving the list only after the reap: red at `left`. A
    /// ceiling kill that reaps with std's `wait`: red at `ceiling`, never
    /// held. Reading it before the group has left the list with the door's
    /// two holds where they are, which passed every test before `beside`:
    /// red at `beside` 3 runs of 3, the child reaped, and no other test red.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_stage_child_an_interrupt_may_have_seen_is_never_reaped() {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(dir) = std::env::var_os(DOOR_DIR) {
            group::kill_on_interrupt();
            DOOR.set(hold_at_the_door).expect("the one hold");
            let dir = PathBuf::from(dir);
            let (suite, ceiling) = if std::env::var(DOOR_CASE).is_ok_and(|case| case == "ceiling") {
                (
                    Cmd::new("/bin/sleep").args(["600"]),
                    Some(Duration::from_millis(100)),
                )
            } else {
                // A suite that leaves a helper and ends: the shell exits, and
                // the `sleep` it backgrounded, deaf to SIGTERM as the shell
                // was, lives on in its group.
                let suite = Cmd::new("/bin/sh").args([
                    "-c",
                    &format!(
                        "trap '' TERM; echo $$ > '{}'; sleep 600 & echo $! > '{}'",
                        dir.join("suite.pid").display(),
                        dir.join("gc.pid").display()
                    ),
                ]);
                (suite.term_grace(TERM_GRACE), None)
            };
            // `beside`'s second stage: a `sleep` deaf to SIGTERM in a group
            // of its own, which asked for a grace.
            let beside = std::env::var(DOOR_CASE)
                .is_ok_and(|case| case == "beside")
                .then(|| {
                    Cmd::new("/bin/sh")
                        .args([
                            "-c",
                            &format!(
                                "trap '' TERM; echo $$ > '{}'; exec sleep 600",
                                dir.join("beside.pid").display()
                            ),
                        ])
                        .term_grace(TERM_GRACE)
                });
            // On a thread of its own, as a gate's stages are: the handler
            // runs on the main thread, and a stage held underneath it would
            // never leave the door. The stage beside is not waited for: it
            // ends only when the handler kills it.
            std::thread::scope(|scope| {
                if let Some(beside) = &beside {
                    scope.spawn(|| run(beside, ceiled(&dir, None)));
                }
                if let Ok(r) = scope.spawn(|| run(&suite, ceiled(&dir, ceiling))).join() {
                    std::fs::write(dir.join("stage-ended"), format!("{:?}\n", r.spawn_error))
                        .expect("the stage-ended marker");
                }
            });
            return;
        }
        let tmp = crate::mktemp_dir("atv-door").expect("mktemp");
        let me = std::env::current_exe().expect("the test binary");
        for (case, number) in [("listed", 15), ("left", 2), ("ceiling", 2), ("beside", 15)] {
            let dir = tmp.join(case);
            std::fs::create_dir_all(&dir).expect("mkdir");
            let mut stand_in = std::process::Command::new(&me);
            stand_in
                .args([
                    "--exact",
                    "exec::tests::a_stage_child_an_interrupt_may_have_seen_is_never_reaped",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(DOOR_DIR, &dir)
                .env(DOOR_CASE, case)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            let mut child = stand_in.spawn().expect("the stand-in gate starts");
            let gate = child.id().to_string();
            // Wait for `marker` (an event), or end the stand-in and what its
            // suite left, by pid, and say why.
            let wait_for = |marker: &str, child: &mut std::process::Child| {
                let (marker, deadline) = (dir.join(marker), Instant::now() + HANG_DETECTOR);
                while !marker.exists() {
                    if Instant::now() > deadline || matches!(child.try_wait(), Ok(Some(_))) {
                        let _ = child.kill();
                        let _ = child.wait();
                        if let Some(pid) = written_pid(&dir.join("gc.pid")) {
                            send("KILL", &pid);
                        }
                        if let Some(pid) = written_pid(&dir.join("beside.pid"))
                            && has_become(&pid, "sleep")
                        {
                            send("KILL", &pid);
                        }
                        panic!(
                            "{case}: the stand-in gate died, or a minute passed, before {}",
                            marker.display()
                        );
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            };
            wait_for("held", &mut child);
            send("INT", &gate);
            let mut leader = None;
            if case == "listed" || case == "beside" {
                // The stage has ended, and the handler is in the grace the
                // suite's `sleep` holds it in (at `beside`, the `sleep` of the
                // stage beside): the held stage's leader is read here, from
                // outside.
                wait_for("stage-ended", &mut child);
                leader = Some(ps("stat=", &read_pid(&dir.join("suite.pid"))));
                send("TERM", &gate);
            }
            let status = child.wait().expect("the stand-in exits");
            // What the suite left running is ended here, by pid, whatever is
            // asserted next.
            let left_running = written_pid(&dir.join("gc.pid")).map(|pid| {
                // At `beside` it is looked at once, the gate being dead: `left`
                // has the window it is given to outlive the gate.
                let window = match case {
                    "left" => STILL_THERE,
                    "beside" => Duration::ZERO,
                    _ => GONE,
                };
                let gone = gone_within(&pid, window);
                if !gone {
                    send("KILL", &pid);
                }
                !gone
            });
            // And the stage beside, which the handler read and killed.
            let beside_running = written_pid(&dir.join("beside.pid")).map(|pid| {
                let gone = gone_within(&pid, GONE);
                if !gone && has_become(&pid, "sleep") {
                    send("KILL", &pid);
                }
                !gone
            });
            assert_eq!(
                status.signal(),
                Some(number),
                "{case}: it dies of its interrupt, the last one it was sent (signal 9: it was \
                 about to signal a group whose leader it no longer held, `signalling`): {status:?}"
            );
            assert_eq!(
                std::fs::read_to_string(dir.join("held")).unwrap_or_default(),
                if case == "left" {
                    "0 live, 0 in flight\n"
                } else {
                    "1 live, 0 in flight\n"
                },
                "{case}: what the stage found of the live list where it was held"
            );
            match case {
                "beside" => {
                    assert!(
                        leader.as_deref().is_some_and(|stat| stat.starts_with('Z')),
                        "{case}: a child whose door was passed with the gate stopping was reaped: \
                         the door read the flag before its group left the list (its `ps` state \
                         once the stage had ended, a zombie's if it was kept: {leader:?})"
                    );
                    assert_eq!(
                        std::fs::read_to_string(dir.join("stage-ended")).unwrap_or_default(),
                        "Some(\"the gate was interrupted while this child ran\")\n",
                        "{case}: what the stage was told of the child it did not reap"
                    );
                    assert_eq!(
                        left_running,
                        Some(true),
                        "{case}: a group that had left the list before the interrupt was signalled \
                         all the same: the `sleep` its suite left is gone"
                    );
                    assert_eq!(
                        beside_running,
                        Some(false),
                        "{case}: the stage beside, whose group the handler read off the list, \
                         outlived the gate"
                    );
                }
                "listed" => {
                    assert!(
                        leader.as_deref().is_some_and(|stat| stat.starts_with('Z')),
                        "{case}: a child whose group the handler read off the list was reaped \
                         (its `ps` state once the stage had ended, a zombie's if it was kept: \
                         {leader:?})"
                    );
                    assert_eq!(
                        std::fs::read_to_string(dir.join("stage-ended")).unwrap_or_default(),
                        "Some(\"the gate was interrupted while this child ran\")\n",
                        "{case}: what the stage was told of the child it did not reap"
                    );
                    assert_eq!(
                        left_running,
                        Some(false),
                        "{case}: the `sleep` the suite left outlived the gate"
                    );
                }
                "left" => assert_eq!(
                    left_running,
                    Some(true),
                    "{case}: a group that had left the list before the interrupt was signalled \
                     all the same: the `sleep` its suite left is gone"
                ),
                _ => {}
            }
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// What a test has a group answer the runner's repeated `SIGKILL`s, in
    /// place of what the kernel answered: `group::found` asks
    /// [`answer_of`], in a test build only. `answers` is one bit a round,
    /// from the lowest, and `after` is the answer from the 65th round on;
    /// `asked` counts the rounds. A script that only `watches` counts them
    /// and leaves the kernel's answers alone. All of it atomics, because a
    /// stand-in gate's signal handler asks too. Sixteen at a time: the rule's
    /// own test holds six, beside every test that scripts a real group.
    ///
    /// `group` is the group's id for its repeated `SIGKILL`s, and the id
    /// negated for the rounds that only ask it (signal 0: the quiet a
    /// ceiling's grace waits out, `group::Live::gone`).
    #[cfg(unix)]
    struct Scripted {
        group: AtomicI32,
        answers: AtomicU64,
        after: AtomicBool,
        watches: AtomicBool,
        asked: AtomicU32,
    }

    #[cfg(unix)]
    static SCRIPTED: [Scripted; 16] = [const {
        Scripted {
            group: AtomicI32::new(0),
            answers: AtomicU64::new(0),
            after: AtomicBool::new(false),
            watches: AtomicBool::new(false),
            asked: AtomicU32::new(0),
        }
    }; 16];

    /// Group ids from here up are no process's: far above any pid either
    /// kernel hands out, so a signal sent to one finds nobody
    /// ([`Script::new`] makes its groups here).
    #[cfg(unix)]
    const NOBODY: i32 = 0x7fff_0000;

    /// Set by a stand-in gate whose groups never empty: every repeated
    /// `SIGKILL` answers that it found a process.
    #[cfg(unix)]
    static NEVER_GONE: AtomicBool = AtomicBool::new(false);

    /// What group `g` answers this round of the repeated `SIGKILL`, or of
    /// the question when `sig` is 0, when a test has said: `None` in every
    /// other case, and the kernel's answer stands.
    #[cfg(unix)]
    pub(super) fn answer_of(g: i32, sig: i32) -> Option<bool> {
        if sig != 0 && NEVER_GONE.load(Ordering::SeqCst) {
            return Some(true);
        }
        let key = if sig == 0 { -g } else { g };
        let script = SCRIPTED
            .iter()
            .find(|s| s.group.load(Ordering::SeqCst) == key)?;
        let round = script.asked.fetch_add(1, Ordering::SeqCst);
        if script.watches.load(Ordering::SeqCst) {
            return None;
        }
        Some(if round < 64 {
            (script.answers.load(Ordering::SeqCst) >> round) & 1 == 1
        } else {
            script.after.load(Ordering::SeqCst)
        })
    }

    /// A group no process is in (its id is far above any pid either kernel
    /// hands out, so the kill itself finds nobody) that answers the repeated
    /// `SIGKILL` as scripted, for as long as this value lives.
    #[cfg(unix)]
    struct Script(&'static Scripted, i32);

    #[cfg(unix)]
    impl Script {
        fn new(nth: i32, answers: u64, after: bool) -> Self {
            Self::of(NOBODY + nth, answers, after)
        }

        /// Script what group `g` answers, a real one's included: its kills
        /// are still sent, and only what they found is replaced.
        fn of(g: i32, answers: u64, after: bool) -> Self {
            Self::set(g, answers, after, false)
        }

        /// Script what real group `g` answers the rounds that only ask it
        /// (signal 0), which are still sent.
        fn asking(g: i32, answers: u64, after: bool) -> Self {
            Self::set(-g, answers, after, false)
        }

        /// Count real group `g`'s rounds, its repeated `SIGKILL`s or
        /// (`asking`) the rounds that only ask it, and replace no answer.
        fn watching(g: i32, asking: bool) -> Self {
            Self::set(if asking { -g } else { g }, 0, false, true)
        }

        fn set(key: i32, answers: u64, after: bool, watches: bool) -> Self {
            let slot = SCRIPTED
                .iter()
                .find(|s| {
                    s.group
                        .compare_exchange(0, i32::MIN, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                })
                .expect("a free script");
            slot.answers.store(answers, Ordering::SeqCst);
            slot.after.store(after, Ordering::SeqCst);
            slot.watches.store(watches, Ordering::SeqCst);
            slot.asked.store(0, Ordering::SeqCst);
            slot.group.store(key, Ordering::SeqCst);
            Self(slot, key)
        }

        /// How many rounds the group has been sent: its repeated `SIGKILL`s,
        /// or the rounds that only asked it.
        fn asked(&self) -> u32 {
            self.0.asked.load(Ordering::SeqCst)
        }
    }

    #[cfg(unix)]
    impl Drop for Script {
        fn drop(&mut self) {
            self.0.group.store(0, Ordering::SeqCst);
        }
    }

    /// The rule of the repeated `SIGKILL` (`group::rekill`), on groups that
    /// answer as scripted: a group is killed again every round until
    /// `QUIET_ROUNDS` rounds running have found nobody in it, however often
    /// it had looked empty before, and never more than `KILL_ROUNDS` times.
    ///
    /// The third case is the one the rule is for. A group can answer
    /// "nobody" while one of its processes is still being made (`KILL_ROUNDS`
    /// has the measurement), so one empty round ends nothing: the group that
    /// comes back a round short of the quiet it needs is killed there, and
    /// the count of quiet rounds starts again. And a group that never
    /// empties ends the rounds at their bound, which is all that lets a
    /// ceiling kill return and an interrupted gate die. Rounds with no
    /// bound would never return from that case, so the cases run under a
    /// hang detector of their own ([`within_the_hang_detector`]).
    #[cfg(unix)]
    #[test]
    fn a_killed_group_is_killed_again_until_it_has_stayed_empty() {
        within_the_hang_detector(the_rule_of_the_repeated_kill);
    }

    /// The cases of [`a_killed_group_is_killed_again_until_it_has_stayed_empty`].
    #[cfg(unix)]
    fn the_rule_of_the_repeated_kill() {
        let (quiet, most) = (
            u32::from(group::QUIET_ROUNDS),
            u32::from(group::KILL_ROUNDS),
        );
        if !group::REPEATS {
            // A kernel whose group kill needs no repeat (`KILL_ROUNDS`).
            let never = Script::new(0, u64::MAX, true);
            assert!(!group::rekill(&[never.1]), "no round, so nobody found");
            assert_eq!(never.asked(), 0);
            return;
        }
        assert!(
            (2..=60).contains(&quiet) && 2 * quiet + 5 <= most,
            "the cases below need two quiet spells and a few rounds to fit: {quiet} of {most}"
        );
        // Nothing killed, nothing to repeat.
        assert!(!group::rekill(&[]));

        let gone = Script::new(1, 0, false);
        assert!(!group::rekill(&[gone.1]));
        assert_eq!(
            gone.asked(),
            quiet,
            "a group that died of its first SIGKILL is watched for the quiet rounds and no more"
        );

        let slow = Script::new(2, 0b111, false);
        assert!(!group::rekill(&[slow.1]));
        assert_eq!(
            slow.asked(),
            3 + quiet,
            "three rounds found a process: killed each time, and then the quiet rounds"
        );

        let back = Script::new(3, 1 << (quiet - 1), false);
        assert!(!group::rekill(&[back.1]));
        assert_eq!(
            back.asked(),
            2 * quiet,
            "empty for all but one of its quiet rounds, then a process: the count starts again"
        );

        let never = Script::new(4, u64::MAX, true);
        assert!(
            group::rekill(&[never.1]),
            "the last round still found a process"
        );
        assert_eq!(
            never.asked(),
            most,
            "a group that never empties is killed KILL_ROUNDS times, and that is the end of it"
        );

        // Two groups: both are killed every round for as long as either
        // holds a process, and for the quiet rounds after. The one that
        // holds a process comes first: a round that stopped at the first
        // group to answer would pass the other by.
        let (busy, idle) = (Script::new(5, 0b1_1111, false), Script::new(6, 0, false));
        assert!(!group::rekill(&[busy.1, idle.1]));
        assert_eq!((busy.asked(), idle.asked()), (5 + quiet, 5 + quiet));
    }

    /// Run `stage` under `ceiling`, and script its group as soon as the stage
    /// has written its own pid, which is its group's id, to `pidfile`
    /// (`script` is handed the id). Answers the run, and the script with
    /// how long after the spawn it was set: one set after the ceiling
    /// scripted nothing the kill asked.
    ///
    /// The stage runs on a thread of its own and is waited for here under a
    /// hang detector: a ceiling kill whose rounds had no end would otherwise
    /// hang the whole test binary. A minute past its ceiling the test fails,
    /// the script is dropped with it, and the group's own answers end those
    /// rounds.
    #[cfg(unix)]
    fn run_scripted(
        stage: &Cmd,
        dir: &Path,
        pidfile: &Path,
        ceiling: Duration,
        script: impl Fn(i32) -> Script,
    ) -> (Run, Option<(Script, Duration)>) {
        let started = Instant::now();
        let running = {
            let (stage, dir) = (stage.clone(), dir.to_path_buf());
            std::thread::spawn(move || run(&stage, ceiled(&dir, Some(ceiling))))
        };
        let mut scripted = None;
        while !running.is_finished() {
            if scripted.is_none()
                && let Some(pid) = written_pid(pidfile)
            {
                let group = pid.parse().expect("the stage's pid");
                scripted = Some((script(group), started.elapsed()));
            }
            assert!(
                started.elapsed() < ceiling + HANG_DETECTOR,
                "the ceiling kill had not returned a minute after its {ceiling:?} ceiling: its \
                 repeated kill did not end at its bound (`group::KILL_ROUNDS`)"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        (running.join().expect("the stage's thread"), scripted)
    }

    /// A ceiling kill whose group never empties says so. The stage's group is
    /// scripted to answer every repeated `SIGKILL` as if a process were still
    /// there (its `sleep` dies of the first, as it would), so the rounds run
    /// out and the timeout block, which goes on to say the kill reached the
    /// whole group, first says that a process may be running on. This is the
    /// ceiling kill's own pin that it goes through the rounds at all.
    #[cfg(unix)]
    #[test]
    fn a_ceiling_kill_says_so_when_its_group_never_empties() {
        if !group::REPEATS {
            return;
        }
        let tmp = crate::mktemp_dir("atv-ceil-never-gone").expect("mktemp");
        // The ceiling clock starts at the spawn, and the group can be
        // scripted only once the stage has said which it is. A ceiling that
        // fired first tested nothing, and the next is twice as long
        // ([`a_ceiling_kill_takes_the_childs_whole_process_group`]).
        let mut ceiling = Duration::from_millis(300);
        loop {
            let pidfile = tmp.join(format!("stage-{}ms.pid", ceiling.as_millis()));
            let stage = Cmd::new("/bin/sh").args([
                "-c",
                &format!("echo $$ > '{}'; exec sleep 600", pidfile.display()),
            ]);
            let (r, scripted) = run_scripted(&stage, &tmp, &pidfile, ceiling, |group| {
                Script::of(group, u64::MAX, true)
            });
            assert!(!r.ok && r.output.contains("TIMEOUT"), "{}", r.output);
            if let Some((script, at)) = scripted
                && at < ceiling
            {
                assert!(
                    r.output.contains(&format!(
                        "its process group still held a process after {} more SIGKILLs",
                        group::KILL_ROUNDS
                    )),
                    "the rounds ran out on a group that never emptied ({} asked), and the \
                     report does not say so:\n{}",
                    script.asked(),
                    r.output
                );
                assert_eq!(
                    script.asked(),
                    u32::from(group::KILL_ROUNDS),
                    "every round was taken, and no more"
                );
                break;
            }
            assert!(
                ceiling < GONE,
                "no stage had written its pid within a {ceiling:?} ceiling"
            );
            ceiling *= 2;
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// An ordinary ceiling kill says nothing of a process left, and is over
    /// in its quiet rounds. The stage is one `sleep`, which dies of the first
    /// `SIGKILL`; its group's rounds are counted and its answers left alone
    /// ([`Script::watching`]). Every round then finds nobody: the group is
    /// down to its unreaped leader, which answers a signal with `EPERM`, and
    /// that is the reading the rounds take for "nobody". Read the other way
    /// round (measured 2026-10-13, with the test of what a kill found
    /// inverted) every ceiling kill ran to its bound, took a third of a
    /// second and reported a process that was not there.
    ///
    /// The count is not pinned to the quiet rounds exactly: a process that
    /// is dying can answer a round or two more on a loaded machine
    /// (`group::KILL_ROUNDS` has how long). It is pinned short of the bound.
    #[cfg(unix)]
    #[test]
    fn an_ordinary_ceiling_kill_reports_nothing_left_and_ends_at_its_quiet_rounds() {
        if !group::REPEATS {
            return;
        }
        let (quiet, most) = (
            u32::from(group::QUIET_ROUNDS),
            u32::from(group::KILL_ROUNDS),
        );
        let tmp = crate::mktemp_dir("atv-ceil-quiet").expect("mktemp");
        // As in [`a_ceiling_kill_says_so_when_its_group_never_empties`]: a
        // ceiling that fired before the group could be watched counted
        // nothing, and the next is twice as long.
        let mut ceiling = Duration::from_millis(300);
        loop {
            let pidfile = tmp.join(format!("stage-{}ms.pid", ceiling.as_millis()));
            let stage = Cmd::new("/bin/sh").args([
                "-c",
                &format!("echo $$ > '{}'; exec sleep 600", pidfile.display()),
            ]);
            let (r, watched) = run_scripted(&stage, &tmp, &pidfile, ceiling, |group| {
                Script::watching(group, false)
            });
            assert!(!r.ok && r.output.contains("TIMEOUT"), "{}", r.output);
            if let Some((watch, at)) = watched
                && at < ceiling
            {
                assert!(
                    !r.output.contains("still held a process"),
                    "a group that died of its first SIGKILL is reported as holding a process \
                     ({} rounds):\n{}",
                    watch.asked(),
                    r.output
                );
                assert!(
                    (quiet..most).contains(&watch.asked()),
                    "a group that died of its first SIGKILL is watched for its {quiet} quiet \
                     rounds, and never to the bound of {most}: {} rounds",
                    watch.asked()
                );
                break;
            }
            assert!(
                ceiling < GONE,
                "no stage had written its pid within a {ceiling:?} ceiling"
            );
            ceiling *= 2;
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A ceiling's grace takes its group for gone only once the group has
    /// stayed empty. One empty reading can be of a group that is still
    /// making a process (`group::KILL_ROUNDS`), and until 2026-10-13 the
    /// grace returned on that one reading, with no `SIGKILL` at all and a
    /// note that the group had exited. Twice here, on a stage that dies of
    /// its `SIGTERM` at once (one process, no fork, so nothing of it can
    /// outlive that signal):
    ///
    /// - asked and left to answer for itself ([`Script::watching`]), the
    ///   group is asked the quiet rounds, every one finds nobody, and the
    ///   note says it exited within its grace;
    /// - scripted to answer the third of those rounds as if a process had
    ///   turned up ([`Script::asking`]), it is asked no further and goes on
    ///   to the `SIGKILL` as a group that outlived its grace. That `SIGKILL`
    ///   finds nobody (the process was the script's), and its rounds follow
    ///   all the same: a kill whose first signal finds nobody has not shown
    ///   the group empty either (`group::Live::kill`).
    #[cfg(unix)]
    #[test]
    fn a_grace_takes_its_group_for_gone_only_once_it_has_stayed_empty() {
        if !group::REPEATS {
            return;
        }
        let quiet = u32::from(group::QUIET_ROUNDS);
        let tmp = crate::mktemp_dir("atv-grace-quiet").expect("mktemp");
        for (case, answers) in [("stays-empty", None), ("answers-again", Some(0b100_u64))] {
            // A ceiling that fired before the group could be scripted asked
            // nothing of the script, and the next is twice as long.
            let mut ceiling = Duration::from_millis(300);
            loop {
                let pidfile = tmp.join(format!("{case}-{}ms.pid", ceiling.as_millis()));
                let stage = Cmd::new("/bin/sh")
                    .args([
                        "-c",
                        &format!("echo $$ > '{}'; exec sleep 600", pidfile.display()),
                    ])
                    .term_grace(LONG_GRACE);
                // The rounds of the SIGKILL that follows, counted beside the
                // script of what the group answers when it is only asked.
                let kills = std::cell::OnceCell::new();
                let (r, scripted) =
                    run_scripted(&stage, &tmp, &pidfile, ceiling, |group| match answers {
                        Some(answers) => {
                            let _ = kills.set(Script::watching(group, false));
                            Script::asking(group, answers, false)
                        }
                        None => Script::watching(group, true),
                    });
                assert!(
                    !r.ok && r.output.contains("TIMEOUT"),
                    "{case}: {}",
                    r.output
                );
                if let Some((script, at)) = scripted
                    && at < ceiling
                {
                    let exited = r.output.contains("the group exited within it");
                    let outlived = r
                        .output
                        .contains("the group outlived its 600.0s grace and was SIGKILLed");
                    if answers.is_some() {
                        assert!(
                            outlived && !exited,
                            "{case}: a group that answered inside its quiet rounds is \
                             SIGKILLed as one that outlived its grace:\n{}",
                            r.output
                        );
                        assert_eq!(
                            script.asked(),
                            3,
                            "{case}: the round that finds a process is the last one asked"
                        );
                        assert_eq!(
                            kills.get().map(Script::asked),
                            Some(quiet),
                            "{case}: the SIGKILL's rounds follow a first kill that found nobody"
                        );
                    } else {
                        assert!(
                            exited && !outlived,
                            "{case}: a group that stayed empty exited within its grace:\n{}",
                            r.output
                        );
                        assert_eq!(
                            script.asked(),
                            quiet,
                            "{case}: it is believed gone after the quiet rounds, and not before"
                        );
                    }
                    break;
                }
                assert!(
                    ceiling < GONE,
                    "{case}: no stage had written its pid within a {ceiling:?} ceiling"
                );
                ceiling *= 2;
            }
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// An interrupted gate's repeated kills end by themselves. The stand-in
    /// gate's groups answer every repeated `SIGKILL` as if a process were
    /// still there ([`NEVER_GONE`]: a group that cannot be killed cannot be
    /// built), and the gate is dead of `SIGINT` inside a minute, with its
    /// stage's grandchild gone: a hang detector, for rounds that take a
    /// fraction of a second. It took every round to get there, a
    /// millisecond's nap each, so the bound is what ended them.
    #[cfg(unix)]
    #[test]
    fn a_gate_whose_groups_never_empty_still_dies_of_its_interrupt() {
        use std::os::unix::process::ExitStatusExt as _;
        const PIDFILE: &str = "ATV_NEVER_GONE_PIDFILE";
        if let Some(pidfile) = std::env::var_os(PIDFILE) {
            group::kill_on_interrupt();
            NEVER_GONE.store(true, Ordering::SeqCst);
            let pidfile = PathBuf::from(pidfile);
            run_armed(&with_grandchild(&pidfile), &pidfile);
            return;
        }
        let tmp = crate::mktemp_dir("atv-never-gone").expect("mktemp");
        let pidfile = tmp.join("grandchild.pid");
        let mut stand_in =
            std::process::Command::new(std::env::current_exe().expect("the test binary"));
        stand_in
            .args([
                "--exact",
                "exec::tests::a_gate_whose_groups_never_empty_still_dies_of_its_interrupt",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(PIDFILE, &pidfile)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = stand_in.spawn().expect("the stand-in gate starts");
        let grandchild = armed_grandchild(&mut child, &pidfile);
        let t = Instant::now();
        send("INT", &child.id().to_string());
        let deadline = t + HANG_DETECTOR;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                _ if Instant::now() > deadline => break None,
                _ => std::thread::sleep(Duration::from_millis(1)),
            }
        };
        let took = t.elapsed();
        if status.is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let gone = gone_within(&grandchild, GONE);
        if !gone {
            send("KILL", &grandchild);
        }
        assert_eq!(
            status.and_then(|status| status.signal()),
            Some(2),
            "the gate dies of its interrupt once its repeated kills have run out (`None`: it \
             was alive a minute later, and was killed here): {status:?}"
        );
        assert!(
            !group::REPEATS || took >= Duration::from_millis(u64::from(group::KILL_ROUNDS)),
            "every round was taken, a millisecond's nap each: {took:?}"
        );
        assert!(gone, "the grandchild {grandchild} outlived the gate");
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The variable that makes a test process the stand-in gate of
    /// [`an_interrupted_gate_takes_a_group_that_is_making_processes`]:
    /// the directory its files go in.
    #[cfg(unix)]
    const MAKING_DIR: &str = "ATV_MAKING_PROCESSES_DIR";

    /// Set beside [`MAKING_DIR`]: which stage the stand-in runs, by
    /// [`making_processes`]'s names for them.
    #[cfg(unix)]
    const MAKING_SHAPE: &str = "ATV_MAKING_PROCESSES_SHAPE";

    /// The variable that makes a test process the STAGE of the two tests
    /// below that starts its children from a second thread
    /// ([`start_children_from_a_second_thread`]): the directory its files go
    /// in.
    #[cfg(unix)]
    const STARTER_DIR: &str = "ATV_STARTS_CHILDREN_DIR";

    /// How many children one such stage makes, at most, when it forks them:
    /// the stream is bounded, and it holds three `sleep`s at a time. A stage
    /// that spawns them makes five times as many, in about the same time: a
    /// quarter of a second or so on an idle machine, which a kill sent on
    /// the stream's first sign of life lands inside.
    #[cfg(unix)]
    const STREAM: u32 = 600;

    /// A stage that is making processes for as long as it lives, which is
    /// what a group `SIGKILL` races: the kill does not reach the process a
    /// member is making at that instant (`group::KILL_ROUNDS`). Three
    /// shapes, each a bounded stream of children ([`STREAM`]), each child a
    /// `sleep 613` (a duration nothing else here uses), with no more than
    /// three alive at a time: the stage ends the one before last as it
    /// starts the next.
    ///
    /// - `fork`: a shell that backgrounds them, as a suite does. One thread,
    ///   so the kill finds the shell alive until its fork is over.
    /// - `deaf`: that shell ignoring `SIGTERM`, and so its children: the
    ///   suite a grace does not stop, still forking when the `SIGKILL` comes.
    /// - `spawn`: this test binary started again
    ///   ([`start_children_from_a_second_thread`]), as a `targo` or a test
    ///   harness starts its children: from a thread that is not the one the
    ///   kernel hands the kill to.
    ///
    /// Each leaves `group.pid` (its own pid, which is its group's id) before
    /// its first child, `gc.pid` (that first child, which it keeps, for
    /// [`run_armed`] to arm on) and `stream-done` once the stream is over.
    /// A stage nobody kills ends its last children itself a minute later.
    #[cfg(unix)]
    fn making_processes(shape: &str, dir: &Path) -> Cmd {
        if shape == "spawn" {
            return Cmd::new(std::env::current_exe().expect("the test binary"))
                .args([
                    "--exact",
                    "exec::tests::a_ceiling_kill_takes_a_group_that_is_making_processes",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(STARTER_DIR, dir);
        }
        Cmd::new("/bin/sh").args([
            "-c",
            &format!(
                "{}echo $$ > '{d}/group.pid'; sleep 613 & first=$!; echo $first > '{d}/gc.pid'; \
                 prev=; i=0; while [ $i -lt {STREAM} ]; do sleep 613 & \
                 [ -n \"$prev\" ] && kill -9 $prev; prev=$!; i=$((i+1)); done; \
                 echo done > '{d}/stream-done'; sleep 60; kill -9 $first $prev",
                if shape == "deaf" {
                    "trap '' TERM; "
                } else {
                    ""
                },
                d = dir.display()
            ),
        ])
    }

    /// Be the `spawn` stage of [`making_processes`] when [`STARTER_DIR`]
    /// says so, and answer whether it did. The children are started with
    /// `Command::spawn` by a second thread while this one sleeps, so the
    /// thread that is inside a spawn when the group is killed is not the
    /// thread that takes the kill. Nobody killing it, it ends its last
    /// children itself and exits after a minute.
    #[cfg(unix)]
    fn start_children_from_a_second_thread() -> bool {
        let Some(dir) = std::env::var_os(STARTER_DIR) else {
            return false;
        };
        let dir = PathBuf::from(dir);
        let sleep = || {
            std::process::Command::new("/bin/sleep")
                .arg("613")
                .stdin(std::process::Stdio::null())
                .spawn()
        };
        std::fs::write(dir.join("group.pid"), format!("{}\n", std::process::id()))
            .expect("the group's id");
        let mut first = sleep().expect("the first child");
        std::fs::write(dir.join("gc.pid"), format!("{}\n", first.id())).expect("its pid");
        let starter = std::thread::spawn(move || {
            let mut prev: Option<std::process::Child> = None;
            for _ in 0..5 * STREAM {
                let next = sleep().ok();
                if let Some(mut child) = prev.take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                prev = next;
            }
            let _ = std::fs::write(dir.join("stream-done"), "done\n");
            prev
        });
        std::thread::sleep(HANG_DETECTOR);
        if let Ok(Some(mut last)) = starter.join() {
            let _ = last.kill();
            let _ = last.wait();
        }
        let _ = first.kill();
        let _ = first.wait();
        true
    }

    /// The live processes of group `group`, by pid: `ps` over every process,
    /// since a group whose leader is gone has no other handle.
    #[cfg(unix)]
    fn members_of(group: &str) -> Vec<String> {
        let out = std::process::Command::new("ps")
            .args(["-axo", "pid=,pgid=,stat="])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        out.lines()
            .filter_map(|line| {
                let mut field = line.split_whitespace();
                let (pid, pgid, stat) = (field.next()?, field.next()?, field.next()?);
                (pgid == group && !stat.starts_with('Z')).then(|| pid.to_string())
            })
            .collect()
    }

    /// What the kill left of the group whose stage wrote `dir/group.pid`:
    /// the members still alive once [`GONE`] has passed, each killed here,
    /// by pid, whatever the caller goes on to assert. Empty at once when
    /// the kill took them all; a `sleep 613` it missed outlives the wait.
    #[cfg(unix)]
    fn left_of_the_group(dir: &Path) -> Vec<String> {
        let group = read_pid(&dir.join("group.pid"));
        let deadline = Instant::now() + GONE;
        loop {
            let left = members_of(&group);
            if left.is_empty() || Instant::now() > deadline {
                for pid in &left {
                    send("KILL", pid);
                }
                return left;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// What the two tests below say when a member is left: the property
    /// they assert every run is one the runner gives up to its quiet
    /// window, and no further (`group::KILL_ROUNDS`, what is left).
    #[cfg(unix)]
    const QUIET_WINDOW: &str = "The repeated kill takes a process its group was making only if \
        that process shows up within the quiet rounds (`group::QUIET_ROUNDS`, about 50 ms): one \
        that took longer to appear is left. A red here is that, on a machine loaded past anything \
        this was measured at, or a kill that is no longer repeated.";

    /// A ceiling kill leaves nothing of a group that was making processes
    /// when it landed. A `SIGKILL` reaches the members a group has at that
    /// instant and not the one being made, so until 2026-10-13 a stage
    /// killed in the middle of starting a child could leave that child
    /// running (`group::KILL_ROUNDS` has the measurements). The stage here
    /// makes children back to back ([`making_processes`]) and the ceiling
    /// lands in that stream: no live member of its group is left, with a
    /// grace that it ignores or without one, and whether its children come
    /// from the thread that is killed or from another.
    ///
    /// Whether the kill lands inside a fork is the kernel's to say, so one
    /// run proves little: the count over hundreds is in `KILL_ROUNDS`'s doc.
    /// What each run says of itself is printed, the kill's place in the
    /// stream included.
    #[cfg(unix)]
    #[test]
    fn a_ceiling_kill_takes_a_group_that_is_making_processes() {
        if start_children_from_a_second_thread() {
            return;
        }
        let tmp = crate::mktemp_dir("atv-ceil-making").expect("mktemp");
        let (mut report, mut outlived) = (Vec::new(), Vec::new());
        for (case, shape, grace) in [
            ("fork", "fork", None),
            (
                "fork, with a grace it ignores",
                "deaf",
                Some(Duration::from_millis(50)),
            ),
            ("spawn from a second thread", "spawn", None),
        ] {
            // The ceiling clock starts at the spawn. A ceiling that fires
            // before the stage has written its group's id killed a stage
            // that had made nothing: that run tested nothing, and the next
            // doubles the ceiling
            // ([`a_ceiling_kill_takes_the_childs_whole_process_group`]).
            let mut ceiling = Duration::from_millis(100);
            let dir = loop {
                let dir = tmp.join(format!("{shape}-{}ms", ceiling.as_millis()));
                std::fs::create_dir_all(&dir).expect("mkdir");
                let mut stage = making_processes(shape, &dir);
                if let Some(grace) = grace {
                    stage = stage.term_grace(grace);
                }
                let r = run(&stage, ceiled(&dir, Some(ceiling)));
                assert!(
                    !r.ok && r.output.contains("TIMEOUT"),
                    "{case}: {}",
                    r.output
                );
                if written_pid(&dir.join("group.pid")).is_some() {
                    break dir;
                }
                assert!(
                    ceiling < GONE,
                    "{case}: no stage began its stream within a {ceiling:?} ceiling"
                );
                ceiling *= 2;
            };
            let left = left_of_the_group(&dir);
            report.push(format!(
                "{case}: killed {} its stream, {} left",
                if dir.join("stream-done").exists() {
                    "after"
                } else {
                    "in"
                },
                left.len()
            ));
            if !left.is_empty() {
                outlived.push(format!("{case}: {left:?}"));
            }
        }
        println!("ceiling kill, {}", report.join("; "));
        assert!(
            outlived.is_empty(),
            "a member of a killed group outlived the ceiling kill: {outlived:?}\n{QUIET_WINDOW}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// …and so does an interrupted gate. The stand-in gate runs one such
    /// stage ([`making_processes`]) and is interrupted once the stream is
    /// under way: no live member of the stage's group is left when the gate
    /// has died of its interrupt. The stage with a grace ignores its
    /// `SIGTERM` and goes on forking, and a second signal ends that grace,
    /// so its `SIGKILL` lands in the stream as well.
    #[cfg(unix)]
    #[test]
    fn an_interrupted_gate_takes_a_group_that_is_making_processes() {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(dir) = std::env::var_os(MAKING_DIR) {
            group::kill_on_interrupt();
            let dir = PathBuf::from(dir);
            let shape = std::env::var(MAKING_SHAPE).expect("the stage's shape");
            let mut stage = making_processes(&shape, &dir);
            if shape == "deaf" {
                stage = stage.term_grace(TERM_GRACE);
            }
            run_armed(&stage, &dir.join("gc.pid"));
            return;
        }
        let tmp = crate::mktemp_dir("atv-interrupt-making").expect("mktemp");
        let me = std::env::current_exe().expect("the test binary");
        let (mut report, mut outlived) = (Vec::new(), Vec::new());
        for (case, shape, second, number) in [
            ("fork", "fork", None, 2),
            ("fork, with a grace it ignores", "deaf", Some("TERM"), 15),
            ("spawn from a second thread", "spawn", None, 2),
        ] {
            let dir = tmp.join(shape);
            std::fs::create_dir_all(&dir).expect("mkdir");
            let mut stand_in = std::process::Command::new(&me);
            stand_in
                .args([
                    "--exact",
                    "exec::tests::an_interrupted_gate_takes_a_group_that_is_making_processes",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(MAKING_DIR, &dir)
                .env(MAKING_SHAPE, shape)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            let mut child = stand_in.spawn().expect("the stand-in gate starts");
            let _first = armed_grandchild(&mut child, &dir.join("gc.pid"));
            let gate = child.id().to_string();
            send("INT", &gate);
            if let Some(second) = second {
                send(second, &gate);
            }
            let status = child.wait().expect("the stand-in exits");
            let left = left_of_the_group(&dir);
            assert_eq!(
                status.signal(),
                Some(number),
                "{case}: it still dies of its interrupt, the last one it was sent: {status:?}"
            );
            report.push(format!(
                "{case}: killed {} its stream, {} left",
                if dir.join("stream-done").exists() {
                    "after"
                } else {
                    "in"
                },
                left.len()
            ));
            if !left.is_empty() {
                outlived.push(format!("{case}: {left:?}"));
            }
        }
        println!("interrupt, {}", report.join("; "));
        assert!(
            outlived.is_empty(),
            "a member of a killed group outlived the interrupted gate: {outlived:?}\n{QUIET_WINDOW}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_timed_out_test_child_names_the_test_it_was_still_running() {
        // The child writes what libtest writes (cargo's header, the count, one
        // verdict, one slow notice), then hangs; the block must name the slow
        // test, where its notice is, and how to re-run it alone.
        let tmp = crate::mktemp_dir("atv-ceil-named").expect("mktemp");
        let script = "printf '     Running unittests src/lib.rs (target/debug/deps/x-abc)\\n\\n\
                      running 2 tests\\ntest a::b ... ok\\n\
                      test a::hang has been running for over 60 seconds\\n'; exec sleep 600";
        let hang = Cmd::new("/bin/sh").args(["-c", script]);

        let t = Instant::now();
        let r = run(&hang, ceiled(&tmp, Some(PRINTS_FIRST)));
        let waited = t.elapsed();

        // The ceiling ENDS the wait, never early, and the child is FAILED by
        // a signal — never read as a spawn failure. A hang detector: a ceiling
        // that does not end the wait waits out the child's `sleep 600`.
        assert!(waited < HANG_DETECTOR, "waited {waited:?}");
        assert!(waited >= PRINTS_FIRST, "and not end it early");
        assert!(!r.ok, "a child that never finished is a FAILURE");
        assert!(r.spawn_error.is_none());
        assert_eq!(r.code, None, "killed by a signal, so there is no exit code");
        let out = r.trimmed_output();
        assert!(out.contains("  child: /bin/sh -c printf "), "{out}");
        assert!(out.contains("child killed after "), "{out}");
        assert!(out.contains("over the 5.0s wall-clock ceiling"), "{out}");
        assert!(out.contains("--stage-timeout <seconds>"), "{out}");
        assert!(out.contains("--stage-timeout off"), "{out}");
        assert!(out.contains("whole process group"), "{out}");

        // The order: the child's bytes, the TIMEOUT line, the name, the verdict
        // sentence — the note sits inside the block, not in front of it.
        let alive = out.find("running 2 tests").expect("the child's own output");
        let verdict = out
            .find("aterm-verify: TIMEOUT")
            .expect("the ceiling diagnostic");
        let named = out.find("still running when killed").expect("the name");
        let nothing = out
            .find("Never a pass and never a skip")
            .expect("the verdict sentence");
        assert!(
            alive < verdict && verdict < named && named < nothing,
            "{out}"
        );
        assert!(
            out.contains(
                "  test binary: unittests src/lib.rs (target/debug/deps/x-abc) — it never \
                 printed its `test result:` line\n"
            ),
            "{out}"
        );
        assert!(
            out.contains("    a::hang   (slow at child line 5; 0 lines followed)\n"),
            "{out}"
        );
        assert!(
            out.contains("    re-run alone: target/debug/deps/x-abc --exact a::hang --nocapture\n"),
            "{out}"
        );
        assert!(
            out.contains("  1 of that binary's 2 tests have a verdict in the log above.\n"),
            "{out}"
        );
        assert!(
            !out.contains("    a::b "),
            "a test with a verdict is not named:\n{out}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_timed_out_child_with_no_libtest_output_gets_the_diagnostic_it_gets_today() {
        // A wedged compile, a hung script: no libtest lines, so the block adds
        // no test-binary line — and no guess dressed up as one.
        let tmp = crate::mktemp_dir("atv-ceil-plain").expect("mktemp");
        let hang = Cmd::new("/bin/sh").args(["-c", "echo got-this-far; exec sleep 600"]);
        let r = run(&hang, ceiled(&tmp, Some(Duration::from_millis(200))));
        assert!(!r.ok);
        let out = r.trimmed_output();
        assert!(out.contains("aterm-verify: TIMEOUT"), "{out}");
        assert!(!out.contains("still running"), "{out}");
        assert!(!out.contains("test binary:"), "{out}");
        // The `child:` line is followed directly by the verdict sentence.
        assert!(
            out.contains("exec sleep 600\n  Never a pass and never a skip"),
            "{out}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_silenced_child_still_reports_its_own_timeout() {
        // `Capture::Silent` throws the child's bytes away; it must not throw the
        // ceiling's verdict away with them, or the ladder would show a bare FAIL
        // with no reason under it.
        let tmp = crate::mktemp_dir("atv-ceil-silent").expect("mktemp");
        let hang = Cmd::new("/bin/sh")
            .args(["-c", "exec sleep 600"])
            .capture(Capture::Silent);
        let r = run(&hang, ceiled(&tmp, Some(Duration::from_millis(200))));
        assert!(!r.ok);
        assert!(
            r.trimmed_output().starts_with("aterm-verify: TIMEOUT"),
            "{r:?}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn the_flag_moves_the_ceiling_and_only_a_typed_word_removes_it() {
        // Anything unreadable is refused (the command line makes it a usage
        // error): the direction of failure is fixed at "keep the backstop".
        for junk in ["", "  ", "45m", "soon", "-5", "NaN", "1e400", "4e9"] {
            assert_eq!(
                parse_ceiling(junk),
                None,
                "{junk:?} is not a number of seconds, and must not disable the ceiling"
            );
        }
        // Only a value that says so removes it.
        for off in ["0", "0.0", "off", "OFF", "none", "Never", " off "] {
            assert_eq!(
                parse_ceiling(off),
                Some(None),
                "{off:?} must disable the ceiling"
            );
        }
        assert_eq!(parse_ceiling("90"), Some(Some(Duration::from_secs(90))));
        assert_eq!(
            parse_ceiling(" 5400 "),
            Some(Some(Duration::from_secs(5400)))
        );
        assert_eq!(
            parse_ceiling("0.25"),
            Some(Some(Duration::from_millis(250)))
        );
    }

    #[test]
    fn a_disabled_ceiling_is_the_old_unbounded_wait() {
        // `off` has to be a real escape hatch: no polling, no kill, no
        // diagnostic — exactly `Command::status()` as before.
        let tmp = crate::mktemp_dir("atv-ceil-off").expect("mktemp");
        let r = run(
            &Cmd::new("/bin/sh").args(["-c", "sleep 0.4; echo late"]),
            ceiled(&tmp, parse_ceiling("off").expect("off parses")),
        );
        assert!(r.ok, "nothing kills a child when the ceiling is off");
        assert_eq!(r.trimmed_output(), "late");
        std::fs::remove_dir_all(&tmp).ok();
    }
}
