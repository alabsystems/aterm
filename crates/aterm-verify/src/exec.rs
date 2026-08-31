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
use std::io;
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
}

impl Cmd {
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            envs: Vec::new(),
            capture: Capture::Emit,
        }
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
    /// Resolved once, on the main thread, from the environment snapshot
    /// ([`crate::Ctx::exec_env`]); carried per invocation rather than read here
    /// so a stage's decision stays a function of a value a test can construct.
    pub child_ceiling: Option<Duration>,
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

impl Run {
    #[must_use]
    pub fn trimmed_output(&self) -> &str {
        self.output.trim_end_matches('\n')
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
///    docs/RELEASE-PROOF-DISCIPLINE.md:221 (a four-row matrix stuck at 4 x its
///    own 180 s budget, before the orphaned-watchdog defect was fixed); 242 s
///    for the paint matrix as it stands today.
///  * The ay full-domain SimHash obligation behind `--full`: 129.8 s, with
///    drafting probes measured at 76 s on other encodings
///    (docs/sparkle-words-v2-design.md:1748).
///  * The real ceiling-setter is none of those: it is a COLD full-workspace
///    `targo build` / `targo test` on the Trust stage2 — a verified-by-default
///    compiler over ~100 crates — which is a tens-of-minutes child on a loaded
///    machine, and the `--full` Kani floor spawns three model-checking children
///    besides.
///
/// 45 min = 2700 s is 3.75 x the worst honest child ever measured here and
/// several times a cold full build, while still bounding a wedged run to
/// something a human comes back to and finds RED. A deadlock does not get faster
/// with patience; honest work does finish.
///
/// WHAT THE KILL DOES NOT DO — stated plainly, because a backstop that oversells
/// itself is worse than none. [`std::process::Child::kill`] sends `SIGKILL` to
/// the DIRECT child and nothing else. A `targo` that had already forked `trustc`
/// processes, or a harness that spawned an aterm under a PTY, leaves those
/// GRANDCHILDREN running: they are not in a process group we created and we do
/// not track their pids. What the ceiling guarantees is that the GATE reaches a
/// verdict and exits; it does not guarantee the machine is idle afterwards, and
/// the diagnostic says so where an operator will read it.
pub const DEFAULT_CHILD_CEILING: Duration = Duration::from_secs(45 * 60);

/// The environment variable that moves, or removes, [`DEFAULT_CHILD_CEILING`].
///
/// Seconds, fractional accepted; `0`, `off`, `none` or `never` disable the
/// ceiling entirely and restore the unbounded wait.
pub const CEILING_ENV: &str = "ATERM_VERIFY_STAGE_TIMEOUT";

/// Anything past a century is not a number of seconds anyone meant, and
/// `Duration::from_secs_f64` panics outright on a value it cannot hold.
const MAX_CEILING_SECS: f64 = 3_153_600_000.0;

/// Parse [`CEILING_ENV`] into a ceiling. Pure, so every branch below is a unit
/// test rather than an exported variable in a live process.
///
/// The direction of failure is fixed the way [`crate::changed`] fixes it: a value
/// this cannot read honestly falls back to the DEFAULT, never to "disabled".
/// Removing the only backstop against a silent gate has to be something an
/// operator TYPED — `ATERM_VERIFY_STAGE_TIMEOUT=off` — not something they
/// achieved by fat-fingering `45m` into a field that wants seconds.
#[must_use]
pub fn ceiling_from_env(raw: Option<&OsStr>) -> Option<Duration> {
    let Some(text) = raw.and_then(OsStr::to_str) else {
        return Some(DEFAULT_CHILD_CEILING);
    };
    let text = text.trim();
    if text == "0"
        || text.eq_ignore_ascii_case("off")
        || text.eq_ignore_ascii_case("none")
        || text.eq_ignore_ascii_case("never")
    {
        return None;
    }
    match text.parse::<f64>() {
        // `is_finite` first, so a NaN never reaches the range test — a NaN
        // compares false against everything, which would land it in the default
        // arm anyway, but only by accident.
        Ok(v) if v.is_finite() && (0.0..=MAX_CEILING_SECS).contains(&v) => {
            // `0.0` (and `0.000`) mean what the bare `0` above means.
            if v > 0.0 {
                Some(Duration::from_secs_f64(v))
            } else {
                None
            }
        }
        // Empty, negative, `45m`, `NaN`, a century: the default.
        _ => Some(DEFAULT_CHILD_CEILING),
    }
}

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Spawn, wait, and collect. Never panics on a missing tool: an unspawnable
/// child is a failed run whose "output" names the reason, so the caller still
/// reaches its own fail-closed branch.
#[must_use]
pub fn run(cmd: &Cmd, env: ExecEnv<'_>) -> Run {
    let mut c = Command::new(&cmd.program);
    c.args(&cmd.args).current_dir(env.cwd).env("PATH", env.path);
    for (k, v) in &cmd.envs {
        c.env(k, v);
    }

    match &cmd.capture {
        Capture::Silent => {
            c.stdout(Stdio::null()).stderr(Stdio::null());
            finish(c, cmd, env.child_ceiling)
        }
        Capture::Append(path) => match open_append(path) {
            Ok(f) => match f.try_clone() {
                Ok(f2) => {
                    c.stdout(f).stderr(f2);
                    finish(c, cmd, env.child_ceiling)
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
            let file = match File::create(&log) {
                Ok(f) => f,
                Err(e) => return spawn_failure(cmd, &e.to_string()),
            };
            let file2 = match file.try_clone() {
                Ok(f) => f,
                Err(e) => return spawn_failure(cmd, &e.to_string()),
            };
            c.stdout(file).stderr(file2);
            let mut r = finish(c, cmd, env.child_ceiling);
            // `finish` leaves `output` empty except for a ceiling diagnostic, so
            // the child's own bytes go IN FRONT of it rather than over it: a
            // timed-out stage must still show how far it got, and the last line
            // the child managed to write is usually the whole diagnosis.
            let logged = std::fs::read_to_string(&log).unwrap_or_default();
            r.output.insert_str(0, &logged);
            std::fs::remove_file(&log).ok();
            r
        }
    }
}

/// Spawn and wait, under `ceiling`. Returns a [`Run`] whose `output` is EMPTY
/// unless the ceiling fired — the callers own the captured bytes and splice the
/// diagnostic onto them.
///
/// `Command::spawn` + `Child::wait` is exactly what `Command::status` does (the
/// same inherited stdio defaults, the same `waitpid`), so the no-ceiling path
/// below is byte-for-byte the behaviour this function had before and costs
/// nothing.
fn finish(mut c: Command, cmd: &Cmd, ceiling: Option<Duration>) -> Run {
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return failed_to_wait(&e.to_string()),
    };
    let Some(limit) = ceiling else {
        return reaped(child.wait());
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
            Ok(Some(st)) => return reaped(Ok(st)),
            Ok(None) => {}
            Err(e) => return failed_to_wait(&e.to_string()),
        }
        let elapsed = started.elapsed();
        let Some(left) = limit.checked_sub(elapsed) else {
            return over_ceiling(&mut child, cmd, elapsed, limit);
        };
        // Never sleep past the ceiling: the last nap lands exactly on it.
        std::thread::sleep(nap.min(left));
        nap = (nap * 2).min(Duration::from_millis(25));
    }
}

/// The ceiling fired. Kill, reap, and return a FAILURE that says so out loud —
/// never a skip, never a pass, and never something a caller could mistake for a
/// child that merely exited nonzero.
fn over_ceiling(
    child: &mut std::process::Child,
    cmd: &Cmd,
    elapsed: Duration,
    limit: Duration,
) -> Run {
    let kill = match child.kill() {
        Ok(()) => String::new(),
        Err(e) => format!("  (SIGKILL itself failed: {e})\n"),
    };
    // Reap, so the gate does not leave a zombie behind for the rest of the run.
    // SIGKILL is not maskable, so this returns as soon as the kernel has torn
    // the process down.
    let _ = child.wait();
    let secs = elapsed.as_secs_f64();
    let limit_secs = limit.as_secs_f64();
    let argv = cmd.argv().join(" ");
    Run {
        ok: false,
        output: format!(
            "aterm-verify: TIMEOUT — child killed after {secs:.1}s, over the {limit_secs:.1}s \
             wall-clock ceiling\n\
             \x20 child: {argv}\n\
             {kill}\
             \x20 This stage decided NOTHING: a child that never exits is a FAIL, never a pass \
             and never a skip.\n\
             \x20 Raise the ceiling with {CEILING_ENV}=<seconds>, or remove it with \
             {CEILING_ENV}=off.\n\
             \x20 The kill reached the DIRECT child only — anything it had already spawned (a \
             targo's trustc processes, a harness's own children) may still be running.\n"
        ),
        // A signal death, which is what this is, and what the shell would have
        // reported too. `spawn_error` stays `None`: the child ran fine, it just
        // never finished.
        code: None,
        spawn_error: None,
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

    fn ceiled(dir: &Path, child_ceiling: Option<Duration>) -> ExecEnv<'_> {
        ExecEnv {
            cwd: dir,
            path: OsStr::new("/usr/bin:/bin"),
            scratch: dir,
            child_ceiling,
        }
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

        let mut unbounded = Duration::ZERO;
        let mut ceilinged = Duration::ZERO;
        for _ in 0..N {
            let t = Instant::now();
            assert!(run(&quick, ceiled(&tmp, None)).ok);
            unbounded += t.elapsed();

            let t = Instant::now();
            let r = run(&quick, env_in(&tmp));
            ceilinged += t.elapsed();
            assert!(r.ok, "a child that exits 0 is untouched by the ceiling");
            assert!(r.output.is_empty(), "and gains no diagnostic");
        }

        // A poll loop that had (say) settled at a 1-second tick would blow this
        // by two orders of magnitude; the real schedule tops out at 25 ms and
        // reaches that only after 127 ms, which no child here survives.
        let slack = Duration::from_millis(50) * N;
        assert!(
            ceilinged <= unbounded + slack,
            "polling cost {ceilinged:?} for {N} children against {unbounded:?} unbounded — \
             more than {slack:?} of overhead means the backoff is wrong"
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
        let r = run(&hang, ceiled(&tmp, Some(Duration::from_millis(300))));
        let waited = t.elapsed();

        assert!(
            waited < Duration::from_secs(30),
            "the ceiling has to END the wait, not merely describe it (waited {waited:?})"
        );
        assert!(waited >= Duration::from_millis(300), "and not end it early");
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
        // exactly; a loaded machine may overshoot 300 ms by a whole tick.
        assert!(out.contains("child killed after "), "{out}");
        assert!(out.contains("over the 0.3s wall-clock ceiling"), "{out}");
        assert!(out.contains(CEILING_ENV), "{out}");
        assert!(out.contains("=off"), "{out}");
        assert!(out.contains("DIRECT child only"), "{out}");
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
    fn the_env_override_moves_the_ceiling_and_only_a_typed_word_removes_it() {
        // Unset is the default, and so is anything unreadable: the direction of
        // failure is fixed at "keep the backstop".
        assert_eq!(ceiling_from_env(None), Some(DEFAULT_CHILD_CEILING));
        for junk in ["", "  ", "45m", "soon", "-5", "NaN", "1e400", "4e9"] {
            assert_eq!(
                ceiling_from_env(Some(OsStr::new(junk))),
                Some(DEFAULT_CHILD_CEILING),
                "{junk:?} is not a number of seconds, and must not disable the ceiling"
            );
        }
        // Only a value that says so removes it.
        for off in ["0", "0.0", "off", "OFF", "none", "Never", " off "] {
            assert_eq!(
                ceiling_from_env(Some(OsStr::new(off))),
                None,
                "{off:?} must disable the ceiling"
            );
        }
        assert_eq!(
            ceiling_from_env(Some(OsStr::new("90"))),
            Some(Duration::from_secs(90))
        );
        assert_eq!(
            ceiling_from_env(Some(OsStr::new(" 5400 "))),
            Some(Duration::from_secs(5400))
        );
        assert_eq!(
            ceiling_from_env(Some(OsStr::new("0.25"))),
            Some(Duration::from_millis(250))
        );
    }

    #[test]
    fn an_overridden_ceiling_is_the_one_that_actually_fires() {
        // The parse above is pure; this is the same value threaded through the
        // path the gate uses — snapshot string, `ceiling_from_env`, `ExecEnv` —
        // ending in a real child that really dies.
        let tmp = crate::mktemp_dir("atv-ceil-env").expect("mktemp");
        let ceiling = ceiling_from_env(Some(OsStr::new("0.4")));
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
            ceiled(&tmp, ceiling_from_env(Some(OsStr::new("off")))),
        );
        assert!(r.ok, "nothing kills a child when the ceiling is off");
        assert_eq!(r.trimmed_output(), "late");
        std::fs::remove_dir_all(&tmp).ok();
    }
}
