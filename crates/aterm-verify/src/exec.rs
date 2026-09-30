// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Running the stages' children.
//!
//! Every stage that shelled out still shells out — to the SAME program with the
//! same arguments and the same environment. A [`Cmd`] is that invocation as a
//! VALUE, which is the point: a test can assert the exact argv of the tippy stage
//! without a Trust toolchain installed, and drift in a ported command is caught
//! by a unit test instead of by a reviewer reading two files side by side.
//!
//! Output is captured to a file that both stdout and stderr share (via
//! `File::try_clone`, so the two streams interleave in the order the child wrote
//! them — the faithful equivalent of the shell's `2>&1`). Capturing rather than
//! streaming is what lets independent stages run concurrently while the ladder
//! still prints in its declared order.
//!
//! Every child also runs under a WALL-CLOCK CEILING ([`DEFAULT_CHILD_CEILING`]),
//! because the one verdict this gate could not previously reach is "this never
//! finished" — see that constant for the whole argument.

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
    /// Spawn under UTILITY QoS rather than the gate's inherited tier — see
    /// [`Cmd::demoted`] for which children may, and why the rest may not.
    pub demoted: bool,
    /// The directory the child starts in, in place of [`ExecEnv::cwd`] — a
    /// test binary replayed from cargo's own invocation starts where cargo
    /// started it, in its package's root ([`Cmd::replayed`]).
    pub cwd: Option<PathBuf>,
    /// [`Cmd::envs`] is the child's WHOLE environment: nothing is inherited, and
    /// [`ExecEnv`]'s PATH, removals and additions are not applied, because the
    /// environment was recorded from a child that already had them
    /// ([`Cmd::replayed`]).
    pub exact_env: bool,
    /// Text written at the head of the child's captured log before it starts
    /// ([`Capture::Emit`] only), so a ceiling kill's diagnostic — which reads
    /// that log — sees it too: cargo's `Running <target> (<path>)` header for a
    /// test binary the gate runs itself, which is how the TIMEOUT block names
    /// the binary and the test still running ([`crate::libtest::note`]).
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

    /// Run this child at UTILITY QoS (`taskpolicy -c utility`, where macOS has
    /// it), so a compile yields the CPU to the programs a person is typing into.
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
    #[must_use]
    pub fn demoted(mut self) -> Self {
        self.demoted = true;
        self
    }

    /// End this child with `SIGTERM` and a `grace` before the `SIGKILL`
    /// (2026-09-26), so a shell suite's `trap … EXIT` runs.
    ///
    /// WHY. Every stage child leads a process group of its own, and both ways
    /// the gate ends one early — the wall-clock ceiling ([`kill_and_reap`]) and
    /// an interrupted gate ([`group::kill_on_interrupt`]) — sent that group
    /// `SIGKILL`, which no process can catch. A shell suite cleans up in its
    /// EXIT trap, and bash runs that trap on `SIGTERM` but never on `SIGKILL`
    /// (measured under /bin/bash 3.2 and bash 5.3: a group `SIGTERM` leaves the
    /// trap's marker, a group `SIGKILL` does not). So a killed suite left what
    /// only its trap removes: `tools/test-dev-sign-id.sh` a scratch keychain in
    /// the owner's real `~/Library/Keychains`, the live lanes their private
    /// instance's temp tree, and `tools/test-codex-live-upgrade.sh` its
    /// `/tmp/cxlive.*` tree plus the Codex app-server daemons and background
    /// terminals that left the group on purpose and that only its pid-walking
    /// cleanup finds.
    ///
    /// OPT-IN, like [`Cmd::demoted`]. A compile has no trap to run and a
    /// `SIGTERM` would only delay its death, so the default stays the
    /// immediate `SIGKILL`. The grace is a BOUND, not a wait: the gate moves on
    /// as soon as the group has emptied, and a child still alive at its end is
    /// `SIGKILL`ed as before, so a suite that ignores `SIGTERM` costs `grace`
    /// and nothing more. An interrupted gate waits at most [`TERM_GRACE`],
    /// whatever a child asked for.
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

/// The invariant part of every invocation: cwd is the repo root (the script
/// `cd`s there, and several stages pass root-relative manifest paths), PATH is
/// the stage2-first one.
#[derive(Clone, Copy, Debug)]
pub struct ExecEnv<'a> {
    pub cwd: &'a Path,
    pub path: &'a OsStr,
    pub scratch: &'a Path,
    /// The wall-clock ceiling on ONE child, or `None` for "wait forever" — the
    /// behaviour every child had before [`DEFAULT_CHILD_CEILING`] existed.
    /// Resolved once, from `--stage-timeout` ([`crate::Ctx::exec_env`]); carried
    /// per invocation rather than read here so a stage's decision stays a
    /// function of a value a test can construct.
    pub child_ceiling: Option<Duration>,
    /// Variables REMOVED from every child's inherited environment before its
    /// own [`Cmd::envs`] are applied — so a stage that names a variable
    /// explicitly still wins. Every run removes `CARGO_TARGET_DIR`
    /// ([`crate::CHILD_ENV_REMOVED`]): the lanes are the run's own directories,
    /// and a caller's redirect would put every main-lane cargo child straight
    /// back into the shared, contended target dir the snapshot exists to get
    /// away from.
    pub remove_env: &'a [&'a str],
    /// Variables ADDED to every child, after [`Self::remove_env`] and BEFORE the
    /// stage's own [`Cmd::envs`] — so a stage that names a variable still wins.
    ///
    /// This is where a fact the whole run must agree on lives, resolved ONCE on
    /// the main thread instead of by each child for itself. Three of them today:
    /// the pinned git stamp (`ATERM_BUILD_GIT_COMMIT` / `ATERM_BUILD_DEV_COMMITS`),
    /// which stops `aterm-gui`'s build script watching a path in the shared
    /// common git dir; `RUST_TEST_THREADS`, which the run must PIN rather
    /// than inherit from whatever shell invoked it; and `CARGO_INCREMENTAL=0`
    /// (2026-09-21), because the gate builds a commit once and the incremental
    /// caches it never reused had grown one lane's target dir from 36 GB to
    /// 55 GB and run the volume to `No space left on device`
    /// ([`crate::Ctx::with_pinned_child_facts`]).
    pub add_env: &'a [(OsString, OsString)],
}

/// THE ONE-MINUTE LOAD AVERAGE (2026-09-26): `/proc/loadavg` where it exists,
/// else `sysctl -n vm.loadavg` (macOS prints `{ 1.23 2.34 3.45 }`). Read at
/// each stage's start and end for its `time` line, its receipt `load` line and
/// the verdict's under-load label ([`crate::ladder::StageLoad`]); `None` when
/// neither answers, and then the line simply carries no load.
#[must_use]
pub fn load_average() -> Option<f64> {
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

/// What a child did.
#[derive(Clone, Debug)]
pub struct Run {
    /// True only on exit status 0 — a signal death is a failure, as in the shell.
    pub ok: bool,
    pub output: String,
    /// The exit code, for the stages whose child says MORE than pass/fail — the
    /// redraw gate answers `2` for "could not run here", which must not read as
    /// either. `None` for a signal death or a child that never spawned.
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

    /// Why this child is NOT a finding about the tree: `Some(reason)` when it
    /// never spawned ([`Run::spawn_error`]) or when it ran out of disk — its
    /// output carries [`ENOSPC_SENTENCE`]. `None` for a child that ran and
    /// answered, pass or fail, including one that only PRINTED the sentence on
    /// its way to passing.
    ///
    /// MEASURED 2026-09-20, the two contract runs that died on a full volume:
    /// `aterm-verify: cannot run …/targo: No space left on device (os error 28)`
    /// (a stage log that could not be created, so a spawn failure) and, inside
    /// a build child, `error: failed to write …/target/debug/deps/…/lib.rmeta:
    /// No space left on device (os error 28)`. Both were recorded as `FAIL`
    /// rows of a FINDING's severity, so a run that had reached its verdict
    /// would have written `verdict FAIL` into the receipt of a tree nobody
    /// judged. [`crate::ladder::Report::fail_child`] asks this first.
    ///
    /// The sentence is matched anywhere in the output, so a FAILING test that
    /// prints it for its own reasons is misread as an environment failure —
    /// COULD NOT RUN instead of FAIL, both red, neither the merge contract; the
    /// cost is a severity, never a green. A passing child is never asked.
    ///
    /// THE THIRD SHAPE (2026-09-23): a `targo test` child whose every failed
    /// test says the MACHINE refused it — [`crate::libtest::COULD_NOT_RUN_SENTINEL`],
    /// printed by aterm-link's stray-daemon guard and by a starved paint take.
    /// 4 of the 13 red full ladders on m3 between 2026-09-18 and 09-22 held no
    /// finding about the tree at all (the stray guard firing on a leftover
    /// `aterm-gui --headless` was one shape), and each wrote `verdict FAIL`.
    /// One failure without the sentinel keeps the whole child a finding
    /// ([`crate::libtest::environment_refusals`]).
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

/// The default wall-clock ceiling on ONE stage child.
///
/// WHY A CEILING AT ALL. This gate's whole vocabulary — `ok`, `FAIL`, `skip` — is
/// decisions, and it had no way to spell the one failure that matters most to an
/// unattended run: *the child never came back*. `finish` below waited on
/// `Command::status()` with no deadline and [`crate::sched`] has no global one,
/// so a deadlocked stage did not turn `tools/verify.sh --fast` red. It turned it
/// SILENT — the ladder printed the rungs above the hang and then stopped, which
/// to a developer who stepped away is indistinguishable from a slow build. THE
/// MERGE GATE CANNOT GO RED ON A HANG; IT JUST HANGS.
///
/// That is not a hypothetical class. The v0.65.0 self-recursive `OnceLock` was
/// exactly it (an initializer that re-entered itself and blocked forever), and
/// the regression test pinning it reproduces the defect BY HANGING — so the
/// tree's own guard against that bug, run through this gate, produced no verdict
/// at all.
///
/// The repo already owns the right backstop and points it elsewhere.
/// `.config/nextest.toml` kills a test at 180 s wall (`slow-timeout` 60 s,
/// `terminate-after` 3), and its own body says why that does not help here:
/// "`tools/verify.sh` still drives stock `cargo test` (its scoping and doctest
/// stages depend on it)", and stock `cargo test` has no per-test timeout. This
/// is that backstop for the verify.sh path, one level up — per stage CHILD
/// rather than per test, because the child is the only thing this crate spawned
/// and therefore the only thing it can honestly account for.
///
/// WHY 45 MINUTES, AND NOT LESS. The ceiling's only job is to tell "wedged" from
/// "slow", so it must sit far above the slowest HONEST child; a ceiling that
/// kills real work converts a true green into a false red, which is strictly
/// worse than the hang it was meant to catch. The measured shape of honest work
/// in this tree:
///
///  * `tools/paint_guard.sh` — 720 s in the pathological run recorded in
///    docs/RELEASE-PROOF-DISCIPLINE.md, harness-defect item 5 (a four-row
///    matrix stuck at 4 x its own 180 s budget, before the orphaned-watchdog
///    defect was fixed); 242 s for the paint matrix as it stands today.
///  * The ay full-domain SimHash obligation behind `--full`: 129.8 s, with
///    drafting probes measured at 76 s on other encodings
///    (docs/sparkle-words-v2-design.md:1748).
///  * The real ceiling-setter is none of those: it is a COLD full-workspace
///    `targo build` / `targo test` on the Trust stage2 — a verified-by-default
///    compiler over ~100 crates — which is a tens-of-minutes child on a loaded
///    machine, and the `--full` Kani floor spawns three model-checking children
///    besides.
///
/// RAISED 45 min -> 90 min on 2026-09-11, and the PREMISE moved rather than the
/// tolerance. `--no-fail-fast` landed on every test argv the ladder builds that
/// day, because `targo test --workspace` stops scheduling at the FIRST failing
/// binary and three consecutive release gates had therefore reported on 41 of
/// ~319 test binaries while reading like a statement about all of them. The
/// stage now runs every binary by design, so "the worst honest child" is a
/// different child.
///
/// MEASURED, not guessed. The first run under the new argv hit the 45-minute
/// ceiling and was killed. That kill was NOT evidence the stage had outgrown
/// the ceiling: sixty leaked `while :; do :; done` shells were eating roughly
/// seventeen cores at the time, and they had been for 27 h 50 m, so every "the
/// box is loaded" judgement made that day was reasoning from a number nobody
/// had asked the cause of. On an IDLE machine, with the same argv, the stage
/// measured ~60 min and the whole ladder 96 min across 402 green stages.
///
/// RAISED AGAIN, 90 min -> 3 h, hours later the same night, because 90 min was
/// set from ONE sample and then tripped by the next run. Measured: the same
/// stage on the same idle machine took ~60 min once and OVER 90 min the next
/// time. Two samples an order of magnitude apart in margin is a distribution,
/// not a number, and 1.5 x the only sample I had was a bound on my own
/// confidence dressed up as a fact about the workload — the exact move this
/// file's own history spends three paragraphs warning about.
///
/// WHAT THIS CEILING IS FOR, restated because getting it wrong twice in one
/// night came from forgetting it: it catches a child that NEVER EXITS. It is
/// not a performance budget and it may not be tuned like one. A false kill
/// costs a full re-run of a 90-minute ladder and — worse — teaches whoever
/// hits it to raise the number reflexively, which is how a backstop becomes a
/// formality. Late detection of a real wedge costs one night. So the factor
/// belongs on the generous side of the honest worst, and 3 h is ~2 x a
/// measured worst that has already surprised me once.
///
/// A deadlock does not get faster with patience; honest work does finish.
///
/// WHAT THE KILL REACHES (2026-09-23). Every stage child leads a process group
/// of its own ([`group`]), and the ceiling sends `SIGKILL` to that whole group:
/// the `trustc`s a `targo` forked and the aterms a test harness launched go
/// with it. Until then the kill reached the DIRECT child only, and those
/// grandchildren kept running — holding sockets and CPU, and tripping the next
/// run's stray-daemon guard. What still escapes is a process that left the
/// group on purpose (one that started its own session, as a PTY-hosted shell
/// does), and the diagnostic says so where an operator will read it.
pub const DEFAULT_CHILD_CEILING: Duration = Duration::from_secs(3 * 60 * 60);

/// The `SIGTERM` grace the stage suites ask for ([`Cmd::term_grace`]), and the
/// longest an interrupted gate waits for any group it sent `SIGTERM` before it
/// `SIGKILL`s the rest and dies. A bound on the slowest cleanup, never a
/// sleep: the gate stops waiting the moment every such group has exited.
pub const TERM_GRACE: Duration = Duration::from_secs(10);

/// Anything past a century is not a number of seconds anyone meant, and
/// `Duration::from_secs_f64` panics outright on a value it cannot hold.
const MAX_CEILING_SECS: f64 = 3_153_600_000.0;

/// Parse a `--stage-timeout` value: `Some(Some(d))` moves
/// [`DEFAULT_CHILD_CEILING`] to `d` (seconds, fractional accepted),
/// `Some(None)` removes it (`0`, `off`, `none`, `never`) and restores the
/// unbounded wait, and `None` is a value this cannot read — which the command
/// line refuses as a usage error. Pure, so every branch is a unit test.
///
/// The direction of failure is fixed the way [`crate::changed`] fixes it:
/// removing the only backstop against a silent gate has to be something an
/// operator TYPED — `--stage-timeout off` — never something they achieved by
/// fat-fingering `45m` into a field that wants seconds.
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
        // `is_finite` first, so a NaN never reaches the range test — a NaN
        // compares false against everything, which would land it in the refused
        // arm anyway, but only by accident.
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
            finish(c, cmd, env.child_ceiling, None).0
        }
        Capture::Append(path) => match open_append(path) {
            Ok(f) => match f.try_clone() {
                Ok(f2) => {
                    c.stdout(f).stderr(f2);
                    finish(c, cmd, env.child_ceiling, None).0
                }
                Err(e) => spawn_failure(cmd, &e.to_string()),
            },
            Err(e) => spawn_failure(cmd, &e.to_string()),
        },
        Capture::Emit => {
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            let log = env
                .scratch
                .join(format!("stage.{}.{seq}.log", std::process::id()));
            let mut file = match File::create(&log) {
                Ok(f) => f,
                Err(e) => return spawn_failure(cmd, &e.to_string()),
            };
            if let Some(text) = &cmd.preamble
                && let Err(e) = file.write_all(text.as_bytes())
            {
                return spawn_failure(cmd, &e.to_string());
            }
            let file2 = match file.try_clone() {
                Ok(f) => f,
                Err(e) => return spawn_failure(cmd, &e.to_string()),
            };
            c.stdout(file).stderr(file2);
            let (mut r, logged) = finish(c, cmd, env.child_ceiling, Some(&log));
            // `finish` leaves `output` empty except for a ceiling diagnostic, so
            // the child's own bytes go IN FRONT of it rather than over it: a
            // timed-out stage must still show how far it got, and the last line
            // the child managed to write is usually the whole diagnosis. Those
            // bytes are `finish`'s single read of the log, which is also what the
            // ceiling diagnostic's note was computed from.
            r.output
                .insert_str(0, logged.as_deref().unwrap_or_default());
            std::fs::remove_file(&log).ok();
            r
        }
    }
}

/// The `Command` for `cmd` before its cwd and environment are applied: the
/// program itself or, for a [`Cmd::demoted`] child, the program behind
/// `taskpolicy -c utility`.
///
/// Wrapped only when both are real: `taskpolicy` exists and the program
/// resolves to an executable. If the program is missing, `taskpolicy` exits 66
/// on its own ("taskpolicy: posix_spawn: No such file or directory", measured).
/// The gate would see an ordinary nonzero exit instead of a [`Run::spawn_error`],
/// losing the fact that the tool could not run. So an unresolvable program is
/// spawned bare and fails exactly as it always did.
///
/// `taskpolicy` replaces itself with the program rather than parenting it
/// (measured: the program's parent is the process that ran `taskpolicy`), so the
/// pid [`over_ceiling`] kills is the tool's own. The timing rows and the ceiling
/// diagnostic name `cmd`'s argv, so neither ever mentions the wrapper.
///
/// The child LEADS A PROCESS GROUP OF ITS OWN ([`group`]) and reads no stdin:
/// a process in a background group that reads the terminal is stopped by
/// `SIGTTIN`, which would be a hang, and no stage child reads stdin anyway.
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

/// Spawn and wait, under `ceiling`. Returns a [`Run`] whose `output` is EMPTY
/// unless the ceiling fired — the callers own the captured bytes and splice the
/// diagnostic onto them.
///
/// `Command::spawn` + `Child::wait` is exactly what `Command::status` does (the
/// same inherited stdio defaults, the same `waitpid`), so the no-ceiling path
/// below is byte-for-byte the behaviour this function had before and costs
/// nothing.
///
/// `log` is the file the child's stdout and stderr are going to, when the
/// caller has one. It is read back ONCE, here, after the child is reaped, and
/// returned beside the `Run`: the ceiling diagnostic needs it to name the test a
/// killed `targo test` child was still running ([`crate::libtest`]) and the
/// caller needs the same bytes to splice in front of that diagnostic. Reading it
/// twice would let a surviving grandchild — one that left the child's process
/// group — append between the reads, and then the note's line numbers would
/// describe text the ladder does not show. Lossy, so one stray byte costs
/// neither the log nor the note.
///
/// While the child runs, its process group is on [`group`]'s live list, so an
/// interrupted gate takes it down too.
fn finish(
    mut c: Command,
    cmd: &Cmd,
    ceiling: Option<Duration>,
    log: Option<&Path>,
) -> (Run, Option<String>) {
    let read_log = |log: Option<&Path>| {
        log.map(|p| String::from_utf8_lossy(&std::fs::read(p).unwrap_or_default()).into_owned())
    };
    // An interrupted gate is on its way down: a child spawned now would only
    // be killed before it could say anything. (The handler raises the flag
    // before it signals anything, so at most a spawn already past this check
    // races it — and that child is on the live list for the final SIGKILL.)
    #[cfg(unix)]
    if group::stopping() {
        return (
            failed_to_wait("the gate was interrupted before this child started"),
            read_log(log),
        );
    }
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return (failed_to_wait(&e.to_string()), read_log(log)),
    };
    #[cfg(unix)]
    let _live = group::Live::enter(child.id(), cmd.term_grace.is_some());
    let Some(limit) = ceiling else {
        let r = reaped(child.wait());
        return (r, read_log(log));
    };

    // POLL, DON'T BLOCK. std offers no wait-with-deadline: `Child::wait` takes
    // `&mut self` and blocks forever, and handing the `Child` to a helper thread
    // so the main one could `recv_timeout` would leave the killer holding only a
    // raw pid — a pid the waiter may already have reaped, which is a pid-reuse
    // race, i.e. a gate that occasionally SIGKILLs an innocent process. With no
    // `libc` to reach for (see this crate's Cargo.toml, where the empty
    // `[dependencies]` is load-bearing), `try_wait` plus a sleep is the only
    // honest construction.
    //
    // So do not make short children pay for it. Many stage children are
    // sub-second — the grep guards, the license sweep, the install-channel
    // harness — so the nap starts at 1 ms and doubles to a 25 ms cap: it has
    // slept only 15 ms in total by its fifth wake and reaches the cap after
    // 127 ms, by which point nothing short is still running.
    //
    // MEASURED on this tree (release, 200 `/bin/sh -c 'exit 0'` children per arm
    // per round, arms alternated ABBA, 3 rounds):
    //     unbounded  4.09 / 3.77 / 4.14 ms per child
    //     ceiling    5.16 / 4.69 / 5.93 ms per child
    // so about +1 ms per child, which is one nap of the early schedule and under
    // 50 ms across a whole gate run's ~25 children — against stage children
    // measured in seconds to minutes. The steady-state cost at the other extreme
    // is 40 `waitpid(WNOHANG)` calls per second per running child: ~108k
    // syscalls across a full 45-minute child, a fraction of a second of CPU
    // against 45 minutes of compiling.
    let started = Instant::now();
    let mut nap = Duration::from_millis(1);
    loop {
        match child.try_wait() {
            Ok(Some(st)) => return (reaped(Ok(st)), read_log(log)),
            Ok(None) => {}
            Err(e) => return (failed_to_wait(&e.to_string()), read_log(log)),
        }
        let elapsed = started.elapsed();
        let Some(left) = limit.checked_sub(elapsed) else {
            // Kill and reap FIRST, then read: the bytes the child wrote before it
            // died are the ones the note and the splice both describe.
            let killed = kill_and_reap(&mut child, cmd.term_grace);
            let logged = read_log(log);
            return (
                over_ceiling(cmd, elapsed, limit, &killed, logged.as_deref()),
                logged,
            );
        };
        // Never sleep past the ceiling: the last nap lands exactly on it.
        std::thread::sleep(nap.min(left));
        nap = (nap * 2).min(Duration::from_millis(25));
    }
}

/// SIGKILL the child's whole process group and reap the child, reporting only
/// a kill that itself failed. Separate from [`over_ceiling`] so the log is read
/// AFTER the processes are gone and the same bytes serve the note and the
/// ladder.
///
/// The group goes FIRST, while the unreaped child still holds its id, so the
/// signal cannot land on a group some later process was given.
///
/// With a `grace` ([`Cmd::term_grace`]) the group is sent `SIGTERM` first and
/// given up to `grace` to empty — polled without reaping, so the child's id
/// stays held and the rule above still holds for the `SIGKILL` that follows
/// if it did not. A group that emptied in time needs no `SIGKILL`: its EXIT
/// traps ran, and the note says so.
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
    // Reap, so the gate does not leave a zombie behind for the rest of the run.
    // SIGKILL is not maskable, so this returns as soon as the kernel has torn
    // the process down.
    let _ = child.wait();
    term + &kill
}

/// The ceiling fired: a FAILURE that says so out loud — never a skip, never a
/// pass, and never something a caller could mistake for a child that merely
/// exited nonzero.
fn over_ceiling(
    cmd: &Cmd,
    elapsed: Duration,
    limit: Duration,
    kill: &str,
    logged: Option<&str>,
) -> Run {
    // NAME THE TEST. A killed `targo test` child's own log says which test
    // binary never printed its `test result:` line and which test libtest had
    // reported slow with no verdict after — the whole diagnosis of the 3-hour
    // hang of 2026-09-16, which this block had left at the argv. The bytes come
    // from the caller's ONE read of the log, so the note and the log the ladder
    // shows describe each other.
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

/// EVERY STAGE CHILD LEADS A PROCESS GROUP OF ITS OWN (2026-09-23), so the
/// gate can end everything a stage started rather than only the process it
/// spawned: the wall-clock ceiling kills the whole group ([`kill_and_reap`]),
/// and an interrupted gate kills every group still running
/// ([`group::kill_on_interrupt`]).
///
/// WHY. A ceiling kill used to reach the DIRECT child only, so a hung `targo
/// test`'s test binaries and the `aterm-gui --headless` daemons they had
/// launched survived it; so did every child of a gate stopped with `Ctrl-C` or
/// `kill`. A survivor of that shape is exactly what aterm-link's stray-daemon
/// guard refuses the NEXT run over: one leftover `aterm-gui --headless`
/// (running from `~/aterm/target/release` since 2026-09-22, origin
/// unrecorded) failed the aterm-link suites of every later gate on m3.
///
/// THE INTERRUPT HALF IS WHY THIS NEEDS `unsafe`. A child in a group of its own
/// no longer receives the terminal's `SIGINT` — the terminal signals its
/// foreground group, and that is the gate's — so the gate must forward it, and
/// std has no signal handling. The handler does only async-signal-safe work:
/// atomic loads and stores, `killpg`, `nanosleep` (the bounded `SIGTERM` grace,
/// 2026-09-26 — see [`Cmd::term_grace`]), then `signal(SIG_DFL)` and `raise`,
/// so the gate still dies of the signal it was sent, with the exit status that
/// says so. This crate has no dependencies on purpose (Cargo.toml), and four
/// libc symbols declared here are not one.
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

    unsafe extern "C" {
        fn killpg(pgrp: i32, sig: i32) -> i32;
        fn signal(sig: i32, handler: usize) -> usize;
        fn raise(sig: i32) -> i32;
        fn nanosleep(req: *const Timespec, rem: *mut Timespec) -> i32;
    }

    /// The process groups of the stage children running right now, `0` for a
    /// free slot. Fixed-size and lock-free because a signal handler reads it.
    /// Stages run their children one at a time, so a few dozen are ever live;
    /// a child that finds every slot taken still runs, and only an interrupt
    /// would miss it.
    ///
    /// A NEGATIVE entry is a group that asked for a `SIGTERM` grace
    /// ([`crate::exec::Cmd::term_grace`]): one atomic carries both facts, so
    /// the handler can never read a group with another child's flag.
    static LIVE: [AtomicI32; 256] = [const { AtomicI32::new(0) }; 256];

    /// Raised by the interrupt handler before it signals anything, so no stage
    /// starts a child the gate is about to kill (`finish` asks).
    static STOPPING: AtomicBool = AtomicBool::new(false);

    /// How many stage groups are on the live list right now (a graced group's
    /// negative entry counts) — what a test waits on before it interrupts, so
    /// the signal cannot outrun the registration it is testing.
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

    /// Does group `pgid` still hold a process that a signal would reach?
    /// `killpg(g, 0)`, which delivers nothing. On macOS a group whose only
    /// member is its unreaped, exited leader answers `EPERM` (measured
    /// 2026-09-26), so this reads it as empty without reaping the leader —
    /// which keeps the leader's id, and so the group's, from being reused.
    #[must_use]
    pub fn alive(pgid: u32) -> bool {
        signal_group(pgid, 0).is_ok()
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
        // A SECOND interrupt while the first one waits out its grace is the
        // operator saying "now": it skips straight to the SIGKILLs below.
        let first = !STOPPING.swap(true, Ordering::SeqCst);
        // Phase 1: SIGKILL every plain group at once, as before; SIGTERM every
        // group that asked for a grace, and remember which, so none of them is
        // ever sent a second SIGTERM (bash would take it in the middle of its
        // EXIT trap, and the trap is the point).
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

    /// `SIGINT` back at its default action, for a test's stand-in gate that
    /// models a terminal's Ctrl-C without the handler: a process started as a
    /// background job with job control off inherits `SIGINT` IGNORED (POSIX),
    /// and a stand-in that ignores the interrupt measures nothing.
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
/// stripped — the shell's `got="$(cmd 2>&1)"`, which is how every control-socket
/// round trip in the smokes reads its reply.
#[must_use]
pub fn capture_reply(cmd: &Cmd, env: ExecEnv<'_>) -> String {
    let r = run(&cmd.clone().capture(Capture::Emit), env);
    r.trimmed_output().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default the gate itself runs under, so every test that does not
    /// say otherwise exercises the POLLING wait rather than a private
    /// no-ceiling path the real gate never takes.
    fn env_in(dir: &Path) -> ExecEnv<'_> {
        ceiled(dir, Some(DEFAULT_CHILD_CEILING))
    }

    /// The ceiling for a hung child whose OWN output a test then asserts: it has
    /// to outlast `/bin/sh` starting and printing, which a loaded gate can
    /// stretch past the 300 ms these tests used, killing the child before it
    /// wrote a byte (sweep 4, 2026-09-29). The child sleeps 600 s after, so the
    /// ceiling still always fires.
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

    /// THE GATE'S OWN SETTING REACHES THE CHILD, over whatever the caller's
    /// shell exported (`Command::env` after inheriting is an override), and a
    /// stage that names the variable itself still wins — the same precedence
    /// `remove_env` has. [`crate::Ctx::exec_env`] is what carries the run's
    /// pinned facts here; `lib.rs` pins that `CARGO_INCREMENTAL=0` is one of
    /// them.
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
        // The CODE survives too: a child that distinguishes "failed" from "could
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

    /// THE LOAD READER (2026-09-26): the first number of either spelling —
    /// macOS's braced `sysctl -n vm.loadavg` and Linux's `/proc/loadavg` —
    /// and nothing from a line that holds no number.
    #[test]
    fn the_load_reader_takes_the_one_minute_figure_of_either_spelling() {
        assert_eq!(parse_load_average("{ 74.82 58.31 32.86 }\n"), Some(74.82));
        assert_eq!(parse_load_average("1.50 0.90 0.40 2/613 4242\n"), Some(1.5));
        assert_eq!(parse_load_average(""), None);
        assert_eq!(parse_load_average("{ }"), None);
        assert_eq!(parse_load_average("{ busy 1.0 }"), None);
    }

    /// A REPLAYED CHILD IS THE PROCESS CARGO WOULD HAVE STARTED (2026-09-26):
    /// its own directory, exactly the recorded environment — nothing the gate
    /// inherited, and none of the gate's PATH, removals or additions, which the
    /// recorded environment already reflects — and a preamble at the head of its
    /// log, above its own bytes.
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

    /// THE PREAMBLE IS WHAT A CEILING KILL READS: a test binary the gate runs
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

    #[test]
    fn a_fast_child_pays_at_most_one_nap_for_the_ceiling() {
        // The whole objection to polling is latency, and many stage children are
        // short. Alternate the arms (unbounded wait vs the real 45-minute
        // ceiling) over the same trivial child so scheduler noise lands on both.
        // The measurement quoted in `finish` puts the cost at ~+1 ms per child;
        // the bound asserted here is 50 ms, loose enough never to flake on a
        // loaded machine and still two orders of magnitude below what a poll
        // that had settled at a 1-second tick would cost.
        let tmp = crate::mktemp_dir("atv-ceil-fast").expect("mktemp");
        let quick = Cmd::new("/bin/sh").args(["-c", "exit 0"]);
        const N: u32 = 20;

        // The overhead of each ceilinged child over the unbounded one beside
        // it, in ms. The MEDIAN decides: a wrong backoff charges EVERY child,
        // while one exec stall or preemption charges one pair. Two sums against
        // one 1 s slack let a single slow spawn decide it (the load-sensitive
        // test audit of 2026-09-27).
        let mut overhead_ms = Vec::with_capacity(N as usize);
        for _ in 0..N {
            let t = Instant::now();
            assert!(run(&quick, ceiled(&tmp, None)).ok);
            let unbounded = t.elapsed();

            let t = Instant::now();
            let r = run(&quick, env_in(&tmp));
            let ceilinged = t.elapsed();
            assert!(r.ok, "a child that exits 0 is untouched by the ceiling");
            assert!(r.output.is_empty(), "and gains no diagnostic");
            overhead_ms.push((ceilinged.as_secs_f64() - unbounded.as_secs_f64()) * 1e3);
        }
        overhead_ms.sort_by(f64::total_cmp);
        let median = overhead_ms[overhead_ms.len() / 2];

        // A poll loop that had (say) settled at a 1-second tick would blow this
        // by two orders of magnitude; the real schedule tops out at 25 ms and
        // reaches that only after 127 ms, which no child here survives.
        assert!(
            median <= 50.0,
            "polling cost a median {median:.1} ms per child over the unbounded wait \
             ({overhead_ms:.1?}) — more than 50 ms of overhead means the backoff is wrong"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_child_that_outlives_the_ceiling_is_killed_and_reported_as_a_failure() {
        // This is the whole point: BEFORE the ceiling this call never returned,
        // so `tools/verify.sh` printed the rungs above it and sat there forever.
        // `exec sleep` rather than a bare `sleep` keeps the test hermetic — one
        // process, so the kill leaves nothing behind to reap by hand.
        let tmp = crate::mktemp_dir("atv-ceil-hang").expect("mktemp");
        let hang = Cmd::new("/bin/sh").args(["-c", "echo got-this-far; exec sleep 600"]);

        let t = Instant::now();
        let r = run(&hang, ceiled(&tmp, Some(PRINTS_FIRST)));
        let waited = t.elapsed();

        assert!(
            waited < Duration::from_secs(30),
            "the ceiling has to END the wait, not merely describe it (waited {waited:?})"
        );
        assert!(waited >= PRINTS_FIRST, "and not end it early");
        assert!(!r.ok, "a child that never finished is a FAILURE");
        assert!(
            r.spawn_error.is_none(),
            "it spawned fine — calling this a spawn failure would blame the environment"
        );
        assert_eq!(r.code, None, "killed by a signal, so there is no exit code");

        let out = r.trimmed_output();
        // The bytes the child DID write survive, in front of the diagnostic: a
        // timed-out stage's last line is usually the whole diagnosis.
        let alive = out.find("got-this-far").expect("the child's own output");
        let verdict = out.find("TIMEOUT").expect("the ceiling diagnostic");
        assert!(alive < verdict, "the child's bytes come first:\n{out}");
        // Named: the child, the elapsed time, the override, and the honest limit
        // of what the kill reached.
        assert!(
            out.contains("/bin/sh -c echo got-this-far; exec sleep 600"),
            "{out}"
        );
        // The elapsed figure is a measurement, so only the LIMIT is asserted
        // exactly; a loaded machine may overshoot the ceiling by a whole tick.
        assert!(out.contains("child killed after "), "{out}");
        assert!(out.contains("over the 5.0s wall-clock ceiling"), "{out}");
        assert!(out.contains("--stage-timeout <seconds>"), "{out}");
        assert!(out.contains("--stage-timeout off"), "{out}");
        assert!(out.contains("whole process group"), "{out}");
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// How long a grandchild that MUST be gone gets to go: a hang detector (a
    /// killed grandchild is reaped by launchd, not by us, and a loaded box
    /// takes its time). Every such grandchild is a `sleep 600`, so this stays
    /// far below its own life.
    #[cfg(unix)]
    const GONE: Duration = Duration::from_secs(60);

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

    /// The pid a stage child's shell wrote for the `sleep` it backgrounded.
    /// The bound is LIVENESS, not latency: a loaded gate (the aterm-gui suite
    /// beside it) can take seconds to start a shell.
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

    /// THE CEILING ENDS THE WHOLE GROUP (2026-09-23). A grandchild the stage
    /// child started is gone once the ceiling fires — and the negative control
    /// is the kill this replaced, `Child::kill` on the direct child, which
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

    /// AN INTERRUPTED GATE TAKES ITS STAGE CHILDREN WITH IT (2026-09-23). The
    /// test re-executes itself as a stand-in gate that installs
    /// [`group::kill_on_interrupt`] (or, for the negative control, does not),
    /// runs a stage child that has a grandchild, and is sent `SIGINT` the way a
    /// terminal's Ctrl-C would reach it. With the handler the grandchild is
    /// gone and the stand-in died of `SIGINT`; without it, the child's own
    /// process group never heard the signal and the grandchild is still there.
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
                // The Ctrl-C this models meets SIGINT at its default action. Run
                // as a background job (`cmd &` with job control off, as a gate
                // lane runs its suites) the stand-in inherits it IGNORED, never
                // died of the SIGINT, waited out its stage's 600 s `sleep` —
                // the binary's 600 s run — and its grandchild ended with it,
                // which the negative control read as "died anyway".
                group::default_interrupt();
            }
            let pidfile = PathBuf::from(pidfile);
            let dir = pidfile.parent().expect("a dir").to_path_buf();
            std::thread::scope(|scope| {
                let stage = scope.spawn(|| run(&with_grandchild(&pidfile), ceiled(&dir, None)));
                // THE EVENT THE INTERRUPT WAITS ON: the stage child's group is
                // on the live list AND its grandchild has written its pid. The
                // pidfile alone raced the registration — the shell can write it
                // before `run` puts the group on the list — and a SIGINT in that
                // gap left nothing to kill.
                while !(group::live_count() > 0
                    && std::fs::read_to_string(&pidfile).is_ok_and(|t| t.ends_with('\n')))
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                std::fs::write(armed(&pidfile), "armed\n").expect("the armed marker");
                let _ = stage.join();
            });
            return;
        }
        fn armed(pidfile: &Path) -> PathBuf {
            pidfile.with_extension("armed")
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
            let mut child = c.spawn().expect("the stand-in gate starts");
            // Interrupt only once the stand-in says its stage group is live —
            // an event, never a guessed delay. A stand-in that never arms is
            // killed, so a failure here cannot leave a 600 s `sleep` behind.
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
            std::process::Command::new("/bin/kill")
                .args(["-INT", &child.id().to_string()])
                .status()
                .expect("kill -INT");
            let status = child.wait().expect("the stand-in exits");
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
    /// spawn, so it must outlast bash's startup to its traps and the pidfile:
    /// one that fires first kills a shell with no trap installed, and the
    /// tests below then read "no marker" or "no pid" for a reason that is not
    /// about the grace. It was 300-500 ms, 20-60x a quiet startup but
    /// reachable under a loaded gate (the load-sensitive test audit of
    /// 2026-09-27); 3 s is still a small fraction of every grace compared.
    #[cfg(unix)]
    const SUITE_CEILING: Duration = Duration::from_secs(3);

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

    /// A GRACEFUL CHILD'S EXIT TRAP RUNS BEFORE THE CEILING KILL (2026-09-26).
    /// The ceiling fires, the group is sent `SIGTERM`, bash runs its trap, the
    /// group empties, and the gate moves on without waiting out the grace —
    /// and the grandchild is gone too. The NEGATIVE CONTROL is the same suite
    /// with no grace: the `SIGKILL` this replaced, which bash cannot trap, so
    /// the marker never appears.
    #[cfg(unix)]
    #[test]
    fn a_graceful_child_runs_its_exit_trap_before_the_ceiling_kill() {
        let tmp = crate::mktemp_dir("atv-ceil-grace").expect("mktemp");
        let (marker, pidfile) = (tmp.join("trap-ran"), tmp.join("grandchild.pid"));
        let t = Instant::now();
        let r = run(
            &suite_with_trap(&marker, &pidfile, false).term_grace(Duration::from_secs(20)),
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
                .contains("SIGTERM first, with a 20.0s grace: the group exited within it"),
            "{}",
            r.output
        );
        assert!(
            waited < SUITE_CEILING + Duration::from_secs(10),
            "the grace is a bound, not a sleep: {waited:?}"
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

    /// …AND A CHILD THAT IGNORES `SIGTERM` IS STILL KILLED when its grace runs
    /// out: the grace delays the `SIGKILL`, it never replaces it.
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
        assert!(waited < Duration::from_secs(30), "{waited:?}");
        let grandchild = read_pid(&pidfile);
        assert!(
            gone_within(&grandchild, GONE),
            "{grandchild} outlived the kill"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// AN INTERRUPTED GATE LETS A GRACEFUL CHILD RUN ITS EXIT TRAP (2026-09-26).
    /// The stand-in gate of [`an_interrupted_gate_kills_every_live_stage_group`],
    /// running a suite-shaped child with a grace: `SIGINT` reaches it, its
    /// handler sends the child's group `SIGTERM`, the trap runs, the
    /// grandchild is gone, and the stand-in still dies of `SIGINT`. The
    /// NEGATIVE CONTROL is the same child without a grace: `SIGKILL`, no trap.
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
            let _ = run(&child, ceiled(&dir, None));
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
            let mut child = c.spawn().expect("the stand-in gate starts");
            let grandchild = read_pid(&dir.join("gc.pid"));
            let t = Instant::now();
            std::process::Command::new("/bin/kill")
                .args(["-INT", &child.id().to_string()])
                .status()
                .expect("kill -INT");
            let status = child.wait().expect("the stand-in exits");
            (
                status,
                grandchild,
                t.elapsed(),
                dir.join("trap-ran").exists(),
            )
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
        // The 3-hour hang of 2026-09-16: the ceiling ended it and the block
        // named the argv, 4,874 tests wide. The child here writes what libtest
        // writes — cargo's header, the count, one verdict, one slow notice —
        // then hangs, and the block has to name the slow test, where its
        // notice is, and how to re-run it alone.
        let tmp = crate::mktemp_dir("atv-ceil-named").expect("mktemp");
        let script = "printf '     Running unittests src/lib.rs (target/debug/deps/x-abc)\\n\\n\
                      running 2 tests\\ntest a::b ... ok\\n\
                      test a::hang has been running for over 60 seconds\\n'; exec sleep 600";
        let hang = Cmd::new("/bin/sh").args(["-c", script]);

        let t = Instant::now();
        let r = run(&hang, ceiled(&tmp, Some(PRINTS_FIRST)));
        let waited = t.elapsed();

        // Everything the plain ceiling test holds, holds here too.
        assert!(waited < Duration::from_secs(30), "waited {waited:?}");
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
    fn an_overridden_ceiling_is_the_one_that_actually_fires() {
        // The parse above is pure; this is the same value threaded through the
        // path the gate uses — the flag's string, `parse_ceiling`, `ExecEnv` —
        // ending in a real child that really dies.
        let tmp = crate::mktemp_dir("atv-ceil-env").expect("mktemp");
        let ceiling = parse_ceiling("0.4").expect("a ceiling");
        assert_eq!(ceiling, Some(Duration::from_millis(400)));

        let t = Instant::now();
        let r = run(
            &Cmd::new("/bin/sh").args(["-c", "exec sleep 600"]),
            ceiled(&tmp, ceiling),
        );
        let waited = t.elapsed();
        assert!(!r.ok);
        assert!(
            (Duration::from_millis(400)..Duration::from_secs(30)).contains(&waited),
            "the override, not the 45-minute default, decided when to kill (waited {waited:?})"
        );
        assert!(
            r.trimmed_output()
                .contains("over the 0.4s wall-clock ceiling"),
            "{}",
            r.trimmed_output()
        );
        std::fs::remove_dir_all(&tmp).ok();
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
