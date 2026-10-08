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
    /// A RELEASE's MEASURE run keeps even these at the inherited tier
    /// ([`CompileQos::Inherited`]): there the compile is the release's critical
    /// path, and UTILITY let a peer's build starve it.
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
    /// The tier a [`Cmd::demoted`] child runs at this run: UTILITY unless the
    /// run is a release's MEASURE (`--compile-qos inherited`, [`CompileQos`]).
    pub compile_qos: CompileQos,
}

/// The tier every [`Cmd::demoted`] (compile-only) child of ONE run gets.
///
/// `Utility` is every run's default, for the reason [`Cmd::demoted`] measured:
/// a gate's compiles yield to the programs a person is typing into.
///
/// `Inherited` is a RELEASE's MEASURE run, and nothing else's (`--compile-qos
/// inherited`, which `tools/release.sh` step 7 passes and the parser admits
/// only with `--measure`). There the compile IS the release's critical path,
/// and UTILITY hands it to whatever else the machine runs: 0.100.0's MEASURE
/// (2026-09-29) built its fat-LTO artifact in 3943.6 s (`load 75.84 -> 53.53 on
/// 18 cores`, its ladder's own line) while a peer's default-QoS build held the
/// cores — the release's operator saw its compile at 2-4% of one core — and
/// the step took 4858 s (the release's steps.tsv). The release already owns the
/// machine's gate lock, so the peers it now competes with as an equal are
/// builds, not a person's typing; the measured stages keep the inherited tier
/// in every run, as before.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CompileQos {
    #[default]
    Utility,
    Inherited,
}

impl CompileQos {
    /// The tier a `--compile-qos` value names, `None` for anything else.
    #[must_use]
    pub fn parse(v: &str) -> Option<Self> {
        match v {
            "utility" => Some(CompileQos::Utility),
            "inherited" => Some(CompileQos::Inherited),
            _ => None,
        }
    }
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

/// The host's physical memory in bytes, read once per run for
/// [`crate::plan::measure_overlap`], and whether it is Linux's `MemTotal`
/// (a few percent under the installed RAM); `None` when the host will not say.
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
/// once every such group has exited.
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
/// program itself or, for a [`Cmd::demoted`] child of a run whose
/// [`ExecEnv::compile_qos`] is [`CompileQos::Utility`], the program behind
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
    let mut c = if cmd.demoted
        && env.compile_qos == CompileQos::Utility
        && crate::is_executable_file(taskpolicy)
        && resolves(cmd, env)
    {
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
/// interrupted gate takes it down too.
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
    // a spawn already past this check races it, and that child is on the live
    // list for the final SIGKILL.
    #[cfg(unix)]
    if group::stopping() {
        return (
            failed_to_wait("the gate was interrupted before this child started"),
            read_log(log),
            None,
        );
    }
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return (failed_to_wait(&e.to_string()), read_log(log), None),
    };
    #[cfg(unix)]
    let _live = group::Live::enter(child.id(), cmd.term_grace.is_some());
    let Some(limit) = ceiling else {
        return match reap(&mut child, true) {
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
    // pay about a millisecond, and a long child costs 40 `waitpid(WNOHANG)`
    // calls a second.
    let started = Instant::now();
    let mut nap = Duration::from_millis(1);
    loop {
        match reap(&mut child, false) {
            Ok(Some((st, cpu))) => return (reaped(Ok(st)), read_log(log), cpu),
            Ok(None) => {}
            Err(e) => return (failed_to_wait(&e.to_string()), read_log(log), None),
        }
        let elapsed = started.elapsed();
        let Some(left) = limit.checked_sub(elapsed) else {
            // Kill and reap first, then read: the bytes the child wrote before it
            // died are the ones the note and the splice both describe.
            let killed = kill_and_reap(&mut child, cmd.term_grace);
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

/// Reap `child` once it has exited: its status, and the user plus system CPU it
/// and the descendants it reaped spent. `Ok(None)` while it still runs (never
/// when `block`ing).
///
/// On Unix this is `wait4(2)`, std's `wait` with the `rusage` it drops: a test
/// run's wall time on a saturated machine says how long a binary waited, its CPU
/// what it cost everything else (measured 2026-10-05: a gate test run at load
/// 12-20 on 14 cores moved 3 % while summed binary walls moved 7 %). Once this
/// reaps, the `Child` is never waited on again: [`kill_and_reap`] runs only on
/// a child that has not exited.
fn reap(
    child: &mut std::process::Child,
    block: bool,
) -> io::Result<Option<(std::process::ExitStatus, Option<Duration>)>> {
    #[cfg(unix)]
    {
        group::wait_rusage(child.id(), block).map(|r| r.map(|(st, cpu)| (st, Some(cpu))))
    }
    #[cfg(not(unix))]
    {
        let st = if block {
            child.wait().map(Some)
        } else {
            child.try_wait()
        };
        st.map(|st| st.map(|st| (st, None)))
    }
}

/// `SIGKILL` the child's whole process group and reap the child, reporting only
/// a kill that itself failed. Separate from [`over_ceiling`] so the log is read
/// after the processes are gone.
///
/// The group goes first, while the unreaped child still holds its id, so the
/// signal cannot land on a group some later process was given.
///
/// With a `grace` ([`Cmd::term_grace`]) the group is sent `SIGTERM` first and
/// given up to `grace` to empty, polled without reaping so the id stays held. A
/// group that emptied in time needs no `SIGKILL`: its EXIT traps ran, and the
/// note says so.
fn kill_and_reap(child: &mut std::process::Child, grace: Option<Duration>) -> String {
    #[cfg(unix)]
    let term = match grace {
        None => String::new(),
        Some(grace) => match group::term(child.id()) {
            Ok(()) => {
                let deadline = Instant::now() + grace;
                while group::alive(child.id()) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(25));
                }
                if !group::alive(child.id()) {
                    let _ = child.wait();
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
    let kill = match group::kill(child.id()) {
        Ok(()) => String::new(),
        Err(e) => match child.kill() {
            Ok(()) => format!(
                "  (killing its process group failed: {e} — SIGKILL reached the direct child \
                 only)\n"
            ),
            Err(k) => format!("  (SIGKILL itself failed: {e}; {k})\n"),
        },
    };
    #[cfg(not(unix))]
    let kill = match child.kill() {
        Ok(()) => String::new(),
        Err(e) => format!("  (SIGKILL itself failed: {e})\n"),
    };
    // Reap, so no zombie is left for the rest of the run. SIGKILL is not
    // maskable, so this returns once the kernel has torn the process down.
    let _ = child.wait();
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
/// ([`group::kill_on_interrupt`]). Survivors (a hung test's `aterm-gui
/// --headless` daemons, say) would otherwise hold sockets and CPU and trip the
/// next run's stray-daemon guard.
///
/// The interrupt half is why this needs `unsafe`: a child in its own group no
/// longer receives the terminal's `SIGINT`, so the gate must forward it, and std
/// has no signal handling. The handler does only async-signal-safe work: atomic
/// loads and stores, `killpg`, `nanosleep` (the bounded `SIGTERM` grace,
/// [`Cmd::term_grace`]), then `signal(SIG_DFL)` and `raise`, so the gate still
/// dies of the signal it was sent. This crate has no dependencies, so its libc
/// symbols are declared here, with `wait4`, which [`reap`] uses.
#[cfg(unix)]
pub mod group {
    use std::ffi::c_long;
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

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

    unsafe extern "C" {
        fn killpg(pgrp: i32, sig: i32) -> i32;
        fn signal(sig: i32, handler: usize) -> usize;
        fn raise(sig: i32) -> i32;
        fn nanosleep(req: *const Timespec, rem: *mut Timespec) -> i32;
        fn wait4(pid: i32, status: *mut i32, options: i32, rusage: *mut Rusage) -> i32;
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

    /// Raised by the interrupt handler before it signals anything, so no stage
    /// starts a child the gate is about to kill (`finish` asks).
    static STOPPING: AtomicBool = AtomicBool::new(false);

    /// How many stage groups are on the live list now (a graced group's negative
    /// entry counts): what a test waits on before it interrupts.
    #[cfg(test)]
    pub(crate) fn live_count() -> usize {
        LIVE.iter()
            .filter(|slot| slot.load(Ordering::SeqCst) != 0)
            .count()
    }

    /// A child's group on the live list, for as long as this value lives.
    pub struct Live(Option<usize>);

    impl Live {
        /// Put `pgid` — the child's pid, which leads its group — on the list;
        /// `graceful` when the child asked for a `SIGTERM` grace.
        #[must_use]
        pub fn enter(pgid: u32, graceful: bool) -> Self {
            let Ok(g) = i32::try_from(pgid) else {
                return Self(None);
            };
            let entry = if graceful { -g } else { g };
            Self(LIVE.iter().position(|slot| {
                slot.compare_exchange(0, entry, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            }))
        }
    }

    impl Drop for Live {
        fn drop(&mut self) {
            if let Some(i) = self.0 {
                LIVE[i].store(0, Ordering::SeqCst);
            }
        }
    }

    fn signal_group(pgid: u32, sig: i32) -> std::io::Result<()> {
        let g = i32::try_from(pgid).map_err(std::io::Error::other)?;
        // SAFETY: `killpg` takes two integers and touches no memory of ours.
        if unsafe { killpg(g, sig) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    /// `SIGKILL` every process in group `pgid`.
    ///
    /// # Errors
    /// The OS's reason, when no process was signalled.
    pub fn kill(pgid: u32) -> std::io::Result<()> {
        signal_group(pgid, SIGKILL)
    }

    /// `SIGTERM` every process in group `pgid`.
    ///
    /// # Errors
    /// The OS's reason, when no process was signalled.
    pub fn term(pgid: u32) -> std::io::Result<()> {
        signal_group(pgid, SIGTERM)
    }

    /// Does group `pgid` still hold a process a signal would reach? `killpg(g,
    /// 0)`, which delivers nothing. On macOS a group whose only member is its
    /// unreaped, exited leader answers `EPERM`, so this reads it as empty without
    /// reaping the leader, whose id (and so the group's) stays reserved.
    #[must_use]
    pub fn alive(pgid: u32) -> bool {
        signal_group(pgid, 0).is_ok()
    }

    /// `wait4(2)` on `pid` ([`super::reap`]): `Ok(None)` while it runs
    /// (`WNOHANG` unless `block`), else its status and the user plus system
    /// CPU of it and the descendants it reaped. Retried on `EINTR`.
    pub fn wait_rusage(
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
        STOPPING.load(Ordering::SeqCst)
    }

    /// Sleep `ms` milliseconds, async-signal-safely.
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

    extern "C" fn on_signal(sig: i32) {
        // A second interrupt while the first one waits out its grace is the
        // operator saying "now": it skips straight to the SIGKILLs below.
        let first = !STOPPING.swap(true, Ordering::SeqCst);
        // Phase 1: SIGKILL every plain group at once; SIGTERM every group that
        // asked for a grace and remember which, so none is sent a second SIGTERM
        // (bash would take it in the middle of its EXIT trap).
        let mut termed = [0_i32; 256];
        let mut n = 0;
        for slot in &LIVE {
            let v = slot.load(Ordering::SeqCst);
            if v > 0 || (v < 0 && !first) {
                // SAFETY: async-signal-safe, and touches no memory of ours.
                unsafe {
                    killpg(v.abs(), SIGKILL);
                }
            } else if v < 0 {
                // SAFETY: as above.
                unsafe {
                    killpg(-v, SIGTERM);
                }
                termed[n] = -v;
                n += 1;
            }
        }
        // Phase 2: wait — at most TERM_GRACE — for those groups to empty.
        let grace_ms = c_long::try_from(super::TERM_GRACE.as_millis()).unwrap_or(10_000);
        let mut waited: c_long = 0;
        // SAFETY (the `killpg` probes): as above; signal 0 delivers nothing.
        while n > 0
            && waited < grace_ms
            && termed[..n].iter().any(|&g| unsafe { killpg(g, 0) } == 0)
        {
            nap(50);
            waited += 50;
        }
        // Phase 3: SIGKILL whatever is still there — the graced groups that
        // outlived their grace, and any child that started in the meantime.
        for g in &termed[..n] {
            // SAFETY: as above.
            unsafe {
                killpg(*g, SIGKILL);
            }
        }
        for slot in &LIVE {
            let v = slot.load(Ordering::SeqCst);
            if v != 0 {
                // SAFETY: as above.
                unsafe {
                    killpg(v.abs(), SIGKILL);
                }
            }
        }
        // SAFETY: both are async-signal-safe. Restoring the default action and
        // re-raising ends the gate by the signal it was sent, as before.
        unsafe {
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
    /// traps run — before the `SIGKILL`. Idempotent: it installs the same
    /// handler.
    pub fn kill_on_interrupt() {
        let handler = on_signal as extern "C" fn(i32) as usize;
        for sig in [SIGHUP, SIGINT, SIGTERM] {
            // SAFETY: `on_signal` is an `extern "C" fn(i32)` that lives for the
            // whole program and does only async-signal-safe work.
            unsafe {
                signal(sig, handler);
            }
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
            compile_qos: CompileQos::Utility,
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

    /// A RELEASE's MEASURE run (`--compile-qos inherited`) spawns even a demoted
    /// compile bare, at the gate's own tier, with a real `taskpolicy` and a
    /// resolvable tool both present — the case that WOULD be wrapped in any
    /// other run (0.100.0's fat-LTO build, starved at UTILITY by a peer's
    /// default-QoS build). The negative control is the same child, same env,
    /// with the run's default tier: wrapped.
    #[cfg(unix)]
    #[test]
    fn a_releases_measure_run_keeps_its_compiles_at_the_inherited_tier() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = crate::mktemp_dir("atv-qos-release").expect("mktemp");
        let fake = tmp.join("taskpolicy");
        std::fs::write(&fake, "#!/bin/sh\nexit 0\n").expect("write");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let fake_s = fake.to_string_lossy().into_owned();
        let compile = Cmd::new("/bin/sh").args(["-c", "exit 0"]).demoted();
        let release = ExecEnv {
            compile_qos: CompileQos::Inherited,
            ..env_in(&tmp)
        };
        assert_eq!(
            spawned_argv(&spawn_command(&compile, release, &fake)),
            ["/bin/sh", "-c", "exit 0"],
            "a release's MEASURE run: the compile keeps the inherited tier"
        );
        assert_eq!(
            spawned_argv(&spawn_command(&compile, env_in(&tmp), &fake))[..3],
            [fake_s.as_str(), "-c", "utility"],
            "negative control: any other run still demotes the same compile"
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

    /// Is `pid` gone (or a zombie) within `window`? Polls, because a killed
    /// grandchild is reaped by launchd, not by us.
    #[cfg(unix)]
    fn gone_within(pid: &str, window: Duration) -> bool {
        let deadline = Instant::now() + window;
        loop {
            let stat = std::process::Command::new("ps")
                .args(["-o", "stat=", "-p", pid])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
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
            if let Ok(t) = std::fs::read_to_string(file)
                && t.ends_with('\n')
            {
                return t.trim().to_string();
            }
            assert!(Instant::now() < deadline, "no pid in {}", file.display());
            std::thread::sleep(Duration::from_millis(20));
        }
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
        let pidfile = tmp.join("grandchild.pid");
        let r = run(
            &with_grandchild(&pidfile),
            ceiled(&tmp, Some(Duration::from_millis(500))),
        );
        assert!(!r.ok && r.output.contains("TIMEOUT"), "{}", r.output);
        let grandchild = read_pid(&pidfile);
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
    /// the child's group is on the live list and its grandchild has written
    /// `pidfile`. The pidfile alone would race the registration.
    #[cfg(unix)]
    fn run_armed(child: &Cmd, pidfile: &Path) {
        let dir = pidfile.parent().expect("a dir");
        std::thread::scope(|scope| {
            let stage = scope.spawn(|| run(child, ceiled(dir, None)));
            while !(group::live_count() > 0
                && std::fs::read_to_string(pidfile).is_ok_and(|t| t.ends_with('\n')))
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            std::fs::write(armed(pidfile), "armed\n").expect("the armed marker");
            let _ = stage.join();
        });
    }

    /// Send a stand-in gate `SIGINT` once it has armed (an event, never a guessed
    /// delay), and answer how it exited, its stage's grandchild, and how long it
    /// took to exit after the signal. A stand-in that never arms is killed, so a
    /// failure cannot leave a 600 s `sleep` behind.
    #[cfg(unix)]
    fn interrupt_when_armed(
        mut child: std::process::Child,
        pidfile: &Path,
    ) -> (std::process::ExitStatus, String, Duration) {
        let marker = armed(pidfile);
        let deadline = Instant::now() + Duration::from_secs(60);
        while !marker.exists() {
            if Instant::now() > deadline || matches!(child.try_wait(), Ok(Some(_))) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the stand-in gate never armed ({})", marker.display());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let grandchild = read_pid(pidfile);
        let t = Instant::now();
        std::process::Command::new("/bin/kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .expect("kill -INT");
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
    /// spawn, so it must outlast bash's startup to its traps and the pidfile on
    /// a loaded machine; firing first would kill a shell with no trap installed.
    /// 3 s is still a small fraction of every grace compared.
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
    #[cfg(unix)]
    #[test]
    fn an_interrupted_gate_lets_a_graceful_child_run_its_exit_trap() {
        use std::os::unix::process::ExitStatusExt as _;
        const DIR: &str = "ATV_GRACE_INTERRUPT_DIR";
        const GRACE: &str = "ATV_GRACE_INTERRUPT_GRACEFUL";
        if let Some(dir) = std::env::var_os(DIR) {
            group::kill_on_interrupt();
            let dir = PathBuf::from(dir);
            let mut child = suite_with_trap(&dir.join("trap-ran"), &dir.join("gc.pid"), false);
            if std::env::var_os(GRACE).is_some() {
                child = child.term_grace(TERM_GRACE);
            }
            run_armed(&child, &dir.join("gc.pid"));
            return;
        }
        let tmp = crate::mktemp_dir("atv-grace-interrupt").expect("mktemp");
        let me = std::env::current_exe().expect("the test binary");
        let stand_in = |dir: &Path, graceful: bool| {
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
            if graceful {
                c.env(GRACE, "1");
            }
            let child = c.spawn().expect("the stand-in gate starts");
            let (status, grandchild, took) = interrupt_when_armed(child, &dir.join("gc.pid"));
            (status, grandchild, took, dir.join("trap-ran").exists())
        };

        let (status, grandchild, took, trapped) = stand_in(&tmp.join("graceful"), true);
        assert_eq!(
            status.signal(),
            Some(2),
            "it still dies of SIGINT: {status:?}"
        );
        assert!(trapped, "the graceful child's EXIT trap ran");
        assert!(
            gone_within(&grandchild, GONE),
            "{grandchild} outlived the gate"
        );
        assert!(
            took < TERM_GRACE,
            "the handler stops waiting once the group has gone: {took:?}"
        );

        let (status, grandchild, _, trapped) = stand_in(&tmp.join("plain"), false);
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
