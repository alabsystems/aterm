// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Running the stages' children.
//!
//! A [`Cmd`] is one invocation as a value, so a test can assert a stage's exact
//! argv without a Trust toolchain installed. Output goes to one file shared by
//! stdout and stderr (`File::try_clone`, interleaved as the child wrote it, like
//! `2>&1`), so independent stages run concurrently while the ladder prints in
//! declared order. Every child runs under a wall-clock ceiling
//! ([`DEFAULT_CHILD_CEILING`]), so a hang ends the run red instead of silent.

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
    /// Into the stage report, verbatim, above the ladder line it explains.
    Emit,
    /// `>/dev/null 2>&1`.
    Silent,
}

/// One child invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cmd {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// Extra environment on top of the inherited one (the whole environment
    /// under [`Cmd::exact_env`]).
    pub envs: Vec<(OsString, OsString)>,
    pub capture: Capture,
    /// Spawn under utility QoS rather than the run's inherited tier — see
    /// [`Cmd::demoted`] for which children may, and why the rest may not.
    pub demoted: bool,
    /// The directory the child starts in, in place of [`ExecEnv::cwd`] — a
    /// test binary replayed from cargo's own invocation starts where cargo
    /// started it, in its package's root ([`Cmd::replayed`]).
    pub cwd: Option<PathBuf>,
    /// [`Cmd::envs`] is the child's whole environment: nothing is inherited, and
    /// [`ExecEnv`]'s PATH, removals and additions are not applied, because the
    /// environment was recorded from a child that already had them
    /// ([`Cmd::replayed`]).
    pub exact_env: bool,
    /// Text written at the head of the child's captured log before it starts
    /// ([`Capture::Emit`] only): cargo's `Running <target> (<path>)` header for
    /// a test binary verify runs itself, so the combined log reads as cargo's
    /// ([`crate::testrun::combined`]).
    pub preamble: Option<String>,
    /// How long this child may take to end after a ceiling `SIGTERM` before
    /// its group is `SIGKILL`ed ([`Cmd::term_grace`]); `None` is a `SIGKILL`
    /// at once.
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
    /// it), so a compile yields the CPU to the programs a person is typing into
    /// (`pri 20` against the inherited `31`). Utility, not background (`-b`):
    /// background also throttles disk I/O hard.
    ///
    /// ONLY FOR A CHILD THAT JUST COMPILES. A QoS clamp is inherited by
    /// everything the child starts: an aterm launched under UTILITY fires its
    /// paint probe's timers late and goes red. So the test run, the measuring
    /// tests, the GUI smoke, the drives and every other child that RUNS code keep
    /// the inherited tier. That is why this is opt-in per child, not a default.
    #[must_use]
    pub fn demoted(mut self) -> Self {
        self.demoted = true;
        self
    }

    /// At its ceiling, send this child's group `SIGTERM` and give it up to
    /// `grace` to tear down what it started (a lane that booted a VM stops
    /// it) before the group `SIGKILL`. A bound, not a wait: the run moves on
    /// as soon as the child has exited.
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
    /// The wall-clock ceiling on one child, or `None` to wait forever; resolved once from
    /// `--timeout` ([`crate::Ctx::exec_env`]).
    pub child_ceiling: Option<Duration>,
    /// Variables removed from every child's inherited environment before its own
    /// [`Cmd::envs`] are applied, so a stage that names a variable still wins
    /// ([`crate::CHILD_ENV_REMOVED`]). A name ending in `*` removes every
    /// inherited variable that begins with the rest (`CARGO_PKG_*`).
    pub remove_env: &'a [&'a str],
    /// Variables added to every child, after [`Self::remove_env`] and before the
    /// stage's own [`Cmd::envs`], so a stage that names a variable still wins:
    /// facts the whole run must agree on, resolved once on the main thread
    /// (`CARGO_TARGET_DIR`, `TRUST_STAGE2_BIN`, `RUST_TEST_THREADS`).
    pub add_env: &'a [(OsString, OsString)],
}

/// What a child did.
#[derive(Clone, Debug)]
pub struct Run {
    /// True only on exit status 0 — a signal death is a failure, as in the shell.
    pub ok: bool,
    pub output: String,
    /// The exit code, for the stages whose child says more than pass/fail (the
    /// redraw harness's `2` is "could not run here"). `None` for a signal death or
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
    /// [`ENOSPC_SENTENCE`], or when every failed test says the machine refused it
    /// ([`crate::libtest::environment_refusals`]). `None` for a child that ran and
    /// answered, pass or fail.
    ///
    /// The disk sentence is matched anywhere in the output, so a failing test that
    /// prints it for its own reasons reads as COULD NOT RUN instead of FAIL: both
    /// red, never a green. A passing child is never asked.
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
        // The row names what the machine lacks and its provider's fix (no fish here:
        // `targo --unverified verify provision` builds …), where its reader looks, not
        // only in the log.
        crate::libtest::environment_refusals(&self.output)
            .map(|names| crate::libtest::refusal_reason("every failing test", &names, &self.output))
    }
}

/// The default wall-clock ceiling on one stage child: 3 hours.
///
/// It tells wedged from slow, so it sits far above the slowest honest child (a
/// cold full-workspace build or `--no-fail-fast` test run on a loaded machine): a
/// false kill turns a true green red and costs a re-run. It is a hang detector,
/// not a performance budget. The kill reaches the child's whole process group
/// ([`group`]); only a process that left the group on purpose escapes, and the
/// diagnostic says so.
pub const DEFAULT_CHILD_CEILING: Duration = Duration::from_secs(3 * 60 * 60);

/// Why an in-process walk ([`bounded`]) gave no answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unfinished {
    /// Still running at the ceiling. Its command scope has been closed and
    /// its children reaped; a stuck in-process walk may outlive the row.
    TimedOut(Duration),
    /// It panicked (the panic printed on stderr).
    Panicked,
}

impl Unfinished {
    /// The reason, for a NOT RUN row.
    #[must_use]
    pub fn sentence(self) -> String {
        match self {
            Self::TimedOut(d) => format!("did not finish within the {}s ceiling", d.as_secs()),
            Self::Panicked => "panicked (above, on stderr)".to_string(),
        }
    }
}

/// Run an in-process walk on a thread of its own under the same wall-clock
/// ceiling as a child (`None` is `--timeout off`): a walk that hangs ends its
/// row, never the run.
///
/// # Errors
/// [`Unfinished`]: the walk timed out or panicked.
pub fn bounded<T: Send + 'static>(
    ceiling: Option<Duration>,
    walk: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Unfinished> {
    bounded_wait(walk, |rx| match ceiling {
        Some(limit) => rx.recv_timeout(limit).map_err(|e| match e {
            std::sync::mpsc::RecvTimeoutError::Timeout => Unfinished::TimedOut(limit),
            std::sync::mpsc::RecvTimeoutError::Disconnected => Unfinished::Panicked,
        }),
        None => rx.recv().map_err(|_| Unfinished::Panicked),
    })
}

/// [`bounded`] with the wait injected, so a test times out only once its child
/// is running instead of racing a short timer.
fn bounded_wait<T: Send + 'static>(
    walk: impl FnOnce() -> T + Send + 'static,
    wait: impl FnOnce(std::sync::mpsc::Receiver<T>) -> Result<T, Unfinished>,
) -> Result<T, Unfinished> {
    struct Close(std::sync::Arc<output::Scope>);
    impl Drop for Close {
        fn drop(&mut self) {
            self.0.close();
        }
    }
    let scope = Close(std::sync::Arc::new(output::Scope::default()));
    let executor = std::sync::Arc::clone(&scope.0);
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        aterm_uds::output::with_executor(
            std::sync::Arc::new(move |command| executor.output(command)),
            || {
                // A timed-out receiver refuses the value, which is then dropped
                // here, still under the walk's (closed) command scope.
                tx.send(walk()).ok();
            },
        );
    });
    let answer = wait(rx);
    // Closing takes admission's lock: an admitted command is killed and reaped
    // before its permit goes, a later one is refused. A walk still stuck is not
    // joined.
    drop(scope);
    if worker.is_finished() {
        let _ = worker.join();
    }
    answer
}

#[path = "exec_output.rs"]
mod output;

/// Anything past a century is not a number of seconds anyone meant, and
/// `Duration::from_secs_f64` panics outright on a value it cannot hold.
const MAX_CEILING_SECS: f64 = 3_153_600_000.0;

/// Parse a `--timeout` value: `Some(Some(d))` moves [`DEFAULT_CHILD_CEILING`]
/// to `d` (seconds, fractional accepted), `Some(None)` removes it (`0`, `off`,
/// `none`, `never`), and `None` is a usage error. Removing the ceiling must be
/// typed, never reached by typing `45m` into a field that wants seconds.
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
        // `is_finite` first, so a NaN never reaches the range test.
        Ok(v) if v.is_finite() && (0.0..=MAX_CEILING_SECS).contains(&v) => {
            // `0.0` (and `0.000`) mean what the bare `0` above means.
            Some((v > 0.0).then(|| Duration::from_secs_f64(v)))
        }
        // Empty, negative, `45m`, `NaN`, over a century: not a ceiling.
        _ => None,
    }
}

static SEQ: AtomicU64 = AtomicU64::new(0);

/// macOS's QoS launcher. Absent elsewhere, where a [`Cmd::demoted`] child runs
/// as is.
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
            match k.strip_suffix('*') {
                // A prefix: every inherited variable it begins.
                Some(prefix) => {
                    for (name, _) in std::env::vars_os() {
                        if name.as_encoded_bytes().starts_with(prefix.as_bytes()) {
                            c.env_remove(name);
                        }
                    }
                }
                None => {
                    c.env_remove(k);
                }
            }
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
            // `finish`'s output is empty but for a ceiling diagnostic, so the
            // child's own bytes (`finish`'s one read of the log) go in front of
            // it: a timed-out stage still shows how far it got.
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
/// Wrapped only when `taskpolicy` exists and the program resolves: for a
/// missing program `taskpolicy` exits 66 itself, hiding the
/// [`Run::spawn_error`]. `taskpolicy` execs the program, so the pid the ceiling
/// kills is the tool's own, and the diagnostic names `cmd`'s argv.
///
/// The child leads its own process group ([`group`]) and reads no stdin: a
/// background-group process that reads the terminal is stopped by `SIGTTIN`,
/// which would be a hang.
///
/// `taskpolicy` is a SIP-protected platform binary, and dyld purges every
/// `DYLD_*` variable from such a process before it runs, so it never hands
/// one on. A demoted child's own `DYLD_*` (a stock toolchain's loader path,
/// [`crate::rustup::Toolchain::loader_env`]) therefore rides through
/// [`ENV`]'s argv, which sets it after `env`'s own launch and execs the
/// program with it.
fn spawn_command(cmd: &Cmd, env: ExecEnv<'_>, taskpolicy: &Path) -> Command {
    let mut c = if cmd.demoted && crate::is_executable_file(taskpolicy) && resolves(cmd, env) {
        let mut c = Command::new(taskpolicy);
        c.args(["-c", "utility"]);
        let dyld = dyld_assignments(&cmd.envs);
        if !dyld.is_empty()
            && crate::is_executable_file(Path::new(ENV))
            && !cmd.program.as_os_str().to_string_lossy().contains('=')
        {
            c.arg(ENV).args(dyld);
        }
        c.arg(&cmd.program);
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

/// The program [`spawn_command`] puts between `taskpolicy` and a demoted child
/// that carries `DYLD_*` variables.
const ENV: &str = "/usr/bin/env";

/// `K=V` for each `DYLD_*` variable in `envs`, in order (a later one wins, in
/// `env` as in the `Command`).
fn dyld_assignments(envs: &[(OsString, OsString)]) -> Vec<OsString> {
    envs.iter()
        .filter(|(k, _)| k.to_string_lossy().starts_with("DYLD_"))
        .map(|(k, v)| {
            let mut a = k.clone();
            a.push("=");
            a.push(v);
            a
        })
        .collect()
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
/// unless the ceiling fired, beside the child's captured `log`, read once after
/// the child is gone (lossy, so one stray byte costs nothing).
///
/// While the child runs its process group is on [`group`]'s live list, so an
/// interrupted run ends it too; a run already interrupted starts no child.
fn finish(
    mut c: Command,
    cmd: &Cmd,
    ceiling: Option<Duration>,
    log: Option<&Path>,
) -> (Run, Option<String>) {
    let read_log = |log: Option<&Path>| {
        log.map(|p| String::from_utf8_lossy(&std::fs::read(p).unwrap_or_default()).into_owned())
    };
    #[cfg(unix)]
    if group::stopping() {
        return (
            failed_to_wait("the run was interrupted before this child started"),
            read_log(log),
        );
    }
    #[cfg(unix)]
    let spawning = group::Spawning::begin();
    #[cfg(unix)]
    if group::stopping() {
        return (
            failed_to_wait("the run was interrupted before this child started"),
            read_log(log),
        );
    }
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return (failed_to_wait(&e.to_string()), read_log(log)),
    };
    #[cfg(unix)]
    let mut live = group::Live::enter(child.id());
    #[cfg(unix)]
    drop(spawning);
    let Some(limit) = ceiling else {
        #[cfg(unix)]
        let status = live
            .reap_stage(&mut child, true)
            .and_then(|s| s.ok_or_else(|| io::Error::other("blocking wait returned no status")));
        #[cfg(not(unix))]
        let status = child.wait();
        return (reaped(status), read_log(log));
    };

    // Poll, don't block: std has no wait-with-deadline, and a helper thread
    // holding the `Child` would leave the killer a raw pid the waiter may already
    // have reaped. The nap doubles from 1 ms to a 25 ms cap: a sub-second child
    // pays about a millisecond, a long one 40 non-blocking waits a second.
    let started = Instant::now();
    let mut nap = Duration::from_millis(1);
    loop {
        #[cfg(unix)]
        let status = live.reap_stage(&mut child, false);
        #[cfg(not(unix))]
        let status = child.try_wait();
        match status {
            Ok(Some(st)) => return (reaped(Ok(st)), read_log(log)),
            Ok(None) => {}
            Err(e) => return (failed_to_wait(&e.to_string()), read_log(log)),
        }
        let elapsed = started.elapsed();
        let Some(left) = limit.checked_sub(elapsed) else {
            let killed = kill_and_reap(
                &mut child,
                #[cfg(unix)]
                &mut live,
                cmd.term_grace,
            );
            return (over_ceiling(cmd, elapsed, limit, &killed), read_log(log));
        };
        // Never sleep past the ceiling: the last nap lands exactly on it.
        std::thread::sleep(nap.min(left));
        nap = (nap * 2).min(Duration::from_millis(25));
    }
}

/// End the group while its leader remains our unreaped child, then reap through
/// the same live-list/interrupt handshake as an ordinary exit.
fn kill_and_reap(
    child: &mut std::process::Child,
    #[cfg(unix)] live: &mut group::Live,
    grace: Option<Duration>,
) -> String {
    #[cfg(unix)]
    if let Some(grace) = grace
        && live.term().is_ok()
    {
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            // Observe without reaping: even a leader that exited during the
            // grace still pins the group id until the final signal is sent.
            if live.exited(child, false).unwrap_or(false) {
                let _ = live.kill();
                let _ = live.reap_stage(child, true);
                return format!(
                    "  (SIGTERM first, with a {:.1}s grace: the child exited within it)\n",
                    grace.as_secs_f64()
                );
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    #[cfg(not(unix))]
    let _ = grace;
    #[cfg(unix)]
    let note = match live.kill() {
        Ok(()) => String::new(),
        Err(e) if live.holds(child.id()) => match child.kill() {
            Ok(()) => format!(
                "  (killing its process group failed: {e} — SIGKILL reached the direct child only)\n"
            ),
            Err(k) => format!("  (SIGKILL itself failed: {e}; {k})\n"),
        },
        Err(e) => format!("  (group custody was lost: {e}; its old id was not signalled)\n"),
    };
    #[cfg(not(unix))]
    let note = match child.kill() {
        Ok(()) => String::new(),
        Err(e) => format!("  (SIGKILL itself failed: {e})\n"),
    };
    #[cfg(unix)]
    let _ = live.reap_stage(child, true);
    #[cfg(not(unix))]
    let _ = child.wait();
    note
}

/// The ceiling fired: a failure that says so ([`crate::findings::CEILING_KILL`]),
/// never a skip or a pass, and never mistakable for a child that merely exited
/// nonzero.
fn over_ceiling(cmd: &Cmd, elapsed: Duration, limit: Duration, kill: &str) -> Run {
    let argv = cmd.argv().join(" ");
    Run {
        ok: false,
        output: format!(
            "{} — child killed after {:.1}s, over the {:.1}s wall-clock ceiling \
             (--timeout <seconds|off>)\n\x20 child: {argv}\n{kill}",
            crate::findings::CEILING_KILL,
            elapsed.as_secs_f64(),
            limit.as_secs_f64(),
        ),
        // A signal death, as the shell would report it; `spawn_error` stays
        // `None`: the child ran, it just never finished.
        code: None,
        spawn_error: None,
    }
}

/// Every stage child leads a process group of its own, so the run can end
/// everything a stage started: the ceiling `SIGKILL`s the child's group
/// ([`group::Live::kill`]), and an interrupted run ends every group still running
/// ([`group::kill_on_interrupt`]). Survivors (a hung test's daemons, say) would
/// otherwise hold sockets and CPU past the run.
///
/// An interrupt can remember a listed group only after setting STOPPING. A
/// reaper removes the group from the list before reading STOPPING, and leaves
/// the leader unreaped if an interrupt could have remembered it. Both use
/// SeqCst ordering. Exit observation on macOS/Linux uses WNOWAIT; other Unix
/// hosts do not install the forwarding handler. A single group kill can still
/// miss a process being created on macOS; this does not claim a full descendant
/// census or restore the old repeated-kill/procfs machinery.
#[cfg(unix)]
pub mod group {
    use std::ffi::c_long;
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    use libc::{P_PID, WNOHANG, WNOWAIT, siginfo_t, waitid};
    use libc::{killpg, nanosleep, signal};

    const SIGHUP: i32 = 1;
    const SIGINT: i32 = 2;
    const SIGKILL: i32 = 9;
    const SIGTERM: i32 = 15;
    /// `SIG_DFL`, as the handler-pointer value `signal(3)` takes.
    const SIG_DFL: libc::sighandler_t = libc::SIG_DFL;

    /// How long an interrupted run gives its groups to exit after `SIGTERM`
    /// (EXIT traps, the trust lane's own teardown) before it `SIGKILL`s what is
    /// left: rounds of [`NAP_MS`], ended early once every listed child has
    /// exited and left the list and no spawn is in flight.
    const GRACE_ROUNDS: u32 = 200;
    const NAP_MS: c_long = 10;

    /// The process groups of the stage children running now, `0` for a free
    /// slot. Fixed-size and lock-free because the handler reads it; a child
    /// that finds every slot taken still runs, and only an interrupt misses it.
    static LIVE: [AtomicI32; 256] = [const { AtomicI32::new(0) }; 256];

    /// Set by the first interrupt, before it signals anything, so no stage
    /// starts a child the run is about to leave behind.
    static STOPPING: AtomicBool = AtomicBool::new(false);
    static SPAWNING: AtomicUsize = AtomicUsize::new(0);

    /// Counts the spawn-to-registration window so an empty live list cannot
    /// end the handler's grace while a child is still being registered.
    pub struct Spawning(());
    impl Spawning {
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

    /// A group may be signalled only while this handle holds its leader.
    pub struct Live {
        slot: Option<usize>,
        group: Option<u32>,
    }
    impl Live {
        #[must_use]
        pub fn enter(pgid: u32) -> Self {
            let g = i32::try_from(pgid).unwrap_or(0);
            let live = Self {
                slot: LIVE.iter().position(|slot| {
                    g > 0
                        && slot
                            .compare_exchange(0, g, Ordering::SeqCst, Ordering::SeqCst)
                            .is_ok()
                }),
                group: (g > 0).then_some(pgid),
            };
            // A spawn that raced STOPPING must end its own group even if the
            // handler's earlier sweep did not yet see this registration.
            if stopping() {
                let _ = live.kill();
            }
            live
        }
        fn leave(&mut self) {
            if let Some(i) = self.slot.take() {
                LIVE[i].store(0, Ordering::SeqCst);
            }
        }
        fn held(&self) -> std::io::Result<u32> {
            self.group
                .ok_or_else(|| std::io::Error::other("the group leader is no longer held unreaped"))
        }
        pub(super) fn holds(&self, pid: u32) -> bool {
            self.group == Some(pid)
        }
        pub fn kill(&self) -> std::io::Result<()> {
            signal_group(self.held()?, SIGKILL)
        }
        pub fn term(&self) -> std::io::Result<()> {
            signal_group(self.held()?, SIGTERM)
        }
        fn release_for_reap(&mut self, pid: u32) -> std::io::Result<()> {
            if self.held()? != pid {
                return Err(std::io::Error::other("child does not own this group"));
            }
            self.leave();
            // If the handler could have read the old slot, its earlier
            // STOPPING store is visible here and the leader must remain held.
            if stopping() {
                let _ = self.kill();
                return Err(std::io::Error::other(
                    "the run was interrupted while this child ran",
                ));
            }
            self.group = None;
            Ok(())
        }
        pub(super) fn exited(
            &mut self,
            child: &mut std::process::Child,
            block: bool,
        ) -> std::io::Result<bool> {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            {
                if self.held()? != child.id() {
                    return Err(std::io::Error::other("child does not own this group"));
                }
                let seen = exited(self.held()?, block);
                if seen.is_err() {
                    self.leave();
                    self.group = None;
                }
                seen
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            {
                let seen = if block {
                    child.wait().map(Some)
                } else {
                    child.try_wait()
                };
                if !matches!(seen, Ok(None)) {
                    self.leave();
                    self.group = None;
                }
                seen.map(|status| status.is_some())
            }
        }
        pub(super) fn reap_stage(
            &mut self,
            child: &mut std::process::Child,
            block: bool,
        ) -> std::io::Result<Option<std::process::ExitStatus>> {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            {
                if !self.exited(child, block)? {
                    return Ok(None);
                }
                self.release_for_reap(child.id())?;
                child.wait().map(Some)
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            {
                self.exited(child, block)?;
                child.try_wait()
            }
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        pub(super) fn exited_output(
            &mut self,
            child: &aterm_uds::spawnfd::Child,
        ) -> std::io::Result<bool> {
            if self.held()? != child.id() {
                return Err(std::io::Error::other(
                    "output child does not own this group",
                ));
            }
            let seen = exited(self.held()?, false);
            if seen.is_err() {
                self.leave();
                self.group = None;
            }
            seen
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        pub(super) fn reap_output(
            &mut self,
            child: &mut aterm_uds::spawnfd::Child,
        ) -> std::io::Result<std::process::ExitStatus> {
            if !self.holds(child.id()) {
                return Err(std::io::Error::other(
                    "output child does not own this group",
                ));
            }
            exited(self.held()?, true)?;
            self.release_for_reap(child.id())?;
            child.wait()
        }
    }
    impl Drop for Live {
        fn drop(&mut self) {
            self.leave();
        }
    }

    /// Scoped only to the exact child owned by one custody fixture. The real
    /// WNOWAIT query refuses an invalid signal instead of risking a reused ID.
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    pub(super) mod custody_audit {
        use std::cell::Cell;
        use std::marker::PhantomData;
        use std::rc::Rc;

        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub(crate) struct Report {
            pub(crate) attempts: usize,
            pub(crate) terms: usize,
            pub(crate) kills: usize,
            pub(crate) violations: usize,
        }
        #[derive(Clone, Copy)]
        struct Check {
            pid: u32,
            report: Report,
        }
        thread_local! { static CHECK: Cell<Option<Check>> = const { Cell::new(None) }; }

        pub(crate) struct Scope {
            previous: Option<Check>,
            _same_thread: PhantomData<Rc<()>>,
        }
        impl Scope {
            pub(crate) fn arm(pid: u32) -> Self {
                let previous = CHECK.with(|c| {
                    c.replace(Some(Check {
                        pid,
                        report: Report::default(),
                    }))
                });
                Self {
                    previous,
                    _same_thread: PhantomData,
                }
            }
            pub(crate) fn report(&self) -> Report {
                CHECK.with(|c| c.get().expect("armed custody check").report)
            }
        }
        impl Drop for Scope {
            fn drop(&mut self) {
                CHECK.with(|c| c.set(self.previous));
            }
        }
        pub(super) fn before_signal(pid: u32, sig: i32) -> std::io::Result<()> {
            CHECK.with(|c| {
                let Some(mut check) = c.get() else {
                    return Ok(());
                };
                check.report.attempts += 1;
                // Ok(false) is live; Ok(true) is exited but still unreaped.
                // ECHILD or any other observation error proves no permission
                // to send a group signal from this fixture.
                let valid = check.pid == pid && super::exited(pid, false).is_ok();
                if valid {
                    check.report.terms += usize::from(sig == super::SIGTERM);
                    check.report.kills += usize::from(sig == super::SIGKILL);
                } else {
                    check.report.violations += 1;
                }
                c.set(Some(check));
                if valid {
                    Ok(())
                } else {
                    Err(std::io::Error::other(
                        "custody fixture refused a signal without its unreaped child",
                    ))
                }
            })
        }
    }

    fn signal_group(pgid: u32, sig: i32) -> std::io::Result<()> {
        #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
        custody_audit::before_signal(pgid, sig)?;
        let g = i32::try_from(pgid).map_err(|_| std::io::Error::other("pid out of range"))?;
        // SAFETY: callers retain the unreaped group leader; two integer arguments.
        if unsafe { killpg(g, sig) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub(super) fn exited(pid: u32, block: bool) -> std::io::Result<bool> {
        const EINTR: i32 = 4;
        let options = libc::WEXITED | WNOWAIT | if block { 0 } else { WNOHANG };
        let mut nap = std::time::Duration::from_millis(1);
        loop {
            // SAFETY: every field of `siginfo_t` is an integer or a pointer,
            // so all-zero is a valid value; the kernel overwrites it.
            let mut info: siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: `info` is live and writable, and `pid` names the child
            // whose exit is observed without reaping it (`WNOWAIT`).
            let got = unsafe { waitid(P_PID, pid, &raw mut info, options) };
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

    /// Has an interrupt begun ending the run?
    #[must_use]
    pub fn stopping() -> bool {
        STOPPING.load(Ordering::SeqCst)
    }

    /// Send `sig` to every listed group. Async-signal-safe: atomic loads and
    /// `killpg`.
    fn sweep(sig: i32) {
        for slot in &LIVE {
            let g = slot.load(Ordering::SeqCst);
            if g > 0 {
                // SAFETY: as in `kill`.
                unsafe {
                    killpg(g, sig);
                }
            }
        }
    }

    /// The handler: `SIGTERM` every live group, wait up to [`GRACE_ROUNDS`]
    /// for the stage threads to reap them, `SIGKILL` whatever is still listed,
    /// then die of the signal. Only the first interrupt does this; a later one
    /// returns at once. Async-signal-safe throughout: atomics, `killpg`,
    /// `nanosleep`, `signal` and `raise`.
    extern "C" fn on_signal(sig: i32) {
        if STOPPING.swap(true, Ordering::SeqCst) {
            return;
        }
        sweep(SIGTERM);
        let nap = libc::timespec {
            tv_sec: 0,
            tv_nsec: NAP_MS * 1_000_000,
        };
        for _ in 0..GRACE_ROUNDS {
            if LIVE.iter().all(|slot| slot.load(Ordering::SeqCst) == 0)
                && SPAWNING.load(Ordering::SeqCst) == 0
            {
                break;
            }
            // SAFETY: `nanosleep` reads the struct we own and writes nothing
            // when `rem` is null.
            unsafe {
                nanosleep(&raw const nap, std::ptr::null_mut());
            }
        }
        sweep(SIGKILL);
        // SAFETY: both take integers only. The default action, raised, ends
        // the process once this handler returns.
        unsafe {
            signal(sig, SIG_DFL);
            reraise(sig);
        }
    }

    /// `raise(3)` of the interrupt the run was sent (`SIGHUP`, `SIGINT` or
    /// `SIGTERM`, at its default action): the run dies of it, which writes no
    /// crash report. Async-signal-safe.
    ///
    /// # Safety
    /// As `raise`: none beyond an integer argument.
    unsafe fn reraise(sig: i32) {
        #[expect(
            clippy::disallowed_methods,
            reason = "re-raises the interrupt the run was sent (SIGHUP/SIGINT/SIGTERM), never a \
                      core-dump signal"
        )]
        // SAFETY: `raise` takes an integer and touches no memory of ours.
        unsafe {
            libc::raise(sig);
        }
    }

    /// On `SIGINT`, `SIGTERM` or `SIGHUP`, end every live stage child's group
    /// (`on_signal`), then die of the signal. Idempotent.
    pub fn kill_on_interrupt() {
        if !cfg!(any(target_os = "macos", target_os = "linux")) {
            return;
        }
        for sig in [SIGHUP, SIGINT, SIGTERM] {
            // SAFETY: `on_signal` is an `extern "C" fn(i32)` that lives for
            // the whole program and does only async-signal-safe work.
            unsafe {
                signal(sig, on_signal as extern "C" fn(i32) as usize);
            }
        }
    }

    /// How many stage groups are listed now: what a test waits on before it
    /// interrupts.
    #[cfg(test)]
    pub(crate) fn live_count() -> usize {
        LIVE.iter()
            .filter(|slot| slot.load(Ordering::SeqCst) != 0)
            .count()
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

/// Run a child and return its merged stdout+stderr with trailing newlines
/// stripped (the shell's `got="$(cmd 2>&1)"`), how every control-socket round
/// trip in the GUI smoke reads its reply.
#[must_use]
pub fn capture_reply(cmd: &Cmd, env: ExecEnv<'_>) -> String {
    let r = run(&cmd.clone().capture(Capture::Emit), env);
    r.trimmed_output().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default the run itself runs under, so every test that does not say
    /// otherwise exercises the polling wait.
    fn env_in(dir: &Path) -> ExecEnv<'_> {
        ceiled(dir, Some(DEFAULT_CHILD_CEILING))
    }

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

    /// A removal ending in `*` takes every inherited variable it begins: what
    /// `cargo run` told this process about itself (`CARGO_PKG_*`) never reaches
    /// a child, while a plain name is removed exactly.
    #[cfg(unix)]
    #[test]
    fn a_starred_removal_takes_every_inherited_variable_it_begins() {
        let tmp = crate::mktemp_dir("atv-env-prefix").expect("mktemp");
        let env_of = |remove: &[&str]| {
            let env = ExecEnv {
                remove_env: remove,
                // A hang detector, never a latency budget.
                ..ceiled(&tmp, Some(Duration::from_secs(60)))
            };
            run(&Cmd::new("/usr/bin/env"), env).output
        };
        let removed = env_of(&["CARGO_PKG_*", "CARGO_MANIFEST_DIR"]);
        assert!(
            !removed
                .lines()
                .any(|l| l.starts_with("CARGO_PKG_") || l.starts_with("CARGO_MANIFEST_DIR=")),
            "{removed}"
        );
        // The control: cargo hands a test binary its CARGO_PKG_NAME, and an
        // unstarred list leaves it in place.
        if std::env::var_os("CARGO_PKG_NAME").is_some() {
            let kept = env_of(&["CARGO_PKG"]);
            assert!(
                kept.lines().any(|l| l.starts_with("CARGO_PKG_NAME=")),
                "{kept}"
            );
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The child runs in the repo root with the run's PATH; the run's own
    /// setting reaches it over whatever the caller's shell exported, and a
    /// stage that names the variable itself still wins.
    #[test]
    fn a_child_runs_in_the_root_with_the_runs_variables_and_its_own_win() {
        let tmp = crate::mktemp_dir("atv-setenv").expect("mktemp");
        std::fs::write(tmp.join("marker"), b"x").expect("write");
        let added = [(OsString::from("ATV_ADDED_PROBE"), OsString::from("0"))];
        let env = ExecEnv {
            add_env: &added,
            ..env_in(&tmp)
        };
        let ask = Cmd::new("/bin/sh").args([
            "-c",
            "cat marker; printf ' %s %s' \"${ATV_ADDED_PROBE-unset}\" \"$PATH\"",
        ]);
        assert_eq!(run(&ask, env).trimmed_output(), "x 0 /usr/bin:/bin");
        let stage_says = ask.clone().env("ATV_ADDED_PROBE", "1");
        assert_eq!(run(&stage_says, env).trimmed_output(), "x 1 /usr/bin:/bin");
        assert_eq!(
            run(&ask, env_in(&tmp)).trimmed_output(),
            "x unset /usr/bin:/bin"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A missing tool, demoted or not, is a spawn failure: never a panic, and
    /// never a pass (a demoted one is not handed to `taskpolicy`, whose own exit
    /// would hide it).
    #[test]
    fn a_missing_tool_is_a_failed_run_not_a_panic_and_never_a_pass() {
        let tmp = crate::mktemp_dir("atv-exec3").expect("mktemp");
        for cmd in [
            Cmd::new(tmp.join("absent-driver")),
            Cmd::new(tmp.join("absent-driver")).demoted(),
        ] {
            let r = run(&cmd, env_in(&tmp));
            assert!(!r.ok, "{r:?}");
            assert!(r.spawn_error.is_some(), "{r:?}");
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// stdout and stderr land in one capture in write order, and a nonzero
    /// exit keeps both its output and its code.
    #[test]
    fn output_is_captured_with_stderr_merged_and_a_nonzero_exit_keeps_its_code() {
        let tmp = crate::mktemp_dir("atv-exec").expect("mktemp");
        let r = run(
            &Cmd::new("/bin/sh").args(["-c", "echo out; echo err >&2; echo out2"]),
            env_in(&tmp),
        );
        assert!(r.ok);
        assert_eq!(r.trimmed_output(), "out\nerr\nout2");
        let r = run(
            &Cmd::new("/bin/sh").args(["-c", "echo why >&2; exit 3"]),
            env_in(&tmp),
        );
        assert!(!r.ok);
        assert_eq!(r.trimmed_output(), "why");
        assert_eq!(r.code, Some(3));
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A replayed child is the process cargo would have started: its own
    /// directory, exactly the recorded environment (none of the run's PATH or
    /// additions), and a preamble at the head of its log, above its own bytes.
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
            parse_ceiling("0.25"),
            Some(Some(Duration::from_millis(250)))
        );
    }

    // -----------------------------------------------------------------------
    // Ending a child's whole group: the ceiling and an interrupt.
    // -----------------------------------------------------------------------

    /// How long a grandchild that must be gone gets to go: a hang detector (a
    /// killed grandchild is reaped by launchd, and a loaded machine takes its
    /// time). Every such grandchild is a `sleep 600`, so this stays far below
    /// its life.
    #[cfg(unix)]
    const GONE: Duration = Duration::from_secs(60);

    /// A wait for something else that must happen is a hang detector too,
    /// never a latency budget.
    #[cfg(unix)]
    const HANG_DETECTOR: Duration = Duration::from_secs(60);

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

    /// The pid in `file` once its writer has finished the line, else `None`.
    #[cfg(unix)]
    fn written_pid(file: &Path) -> Option<String> {
        let t = std::fs::read_to_string(file).ok()?;
        t.ends_with('\n').then(|| t.trim().to_string())
    }

    /// [`written_pid`], waited for: a loaded machine can take seconds to start a
    /// shell, so the bound is liveness, not latency.
    #[cfg(unix)]
    fn read_pid(file: &Path) -> String {
        let deadline = Instant::now() + HANG_DETECTOR;
        loop {
            if let Some(pid) = written_pid(file) {
                return pid;
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
    /// gone once it fires (a `Child::kill` of the direct child alone would leave
    /// it running).
    #[cfg(unix)]
    #[test]
    fn a_ceiling_kill_takes_the_childs_whole_process_group() {
        let tmp = crate::mktemp_dir("atv-ceil-group").expect("mktemp");
        // The ceiling clock starts at the spawn, so a ceiling that fires before
        // the shell has backgrounded its `sleep` kills a group with no
        // grandchild in it, and no pid ever comes: that run tested nothing, and
        // the next doubles the ceiling.
        let mut ceiling = Duration::from_millis(500);
        let grandchild = loop {
            let pidfile = tmp.join(format!("grandchild-{}ms.pid", ceiling.as_millis()));
            let r = run(&with_grandchild(&pidfile), ceiled(&tmp, Some(ceiling)));
            assert!(
                !r.ok && r.output.contains(crate::findings::CEILING_KILL),
                "{}",
                r.output
            );
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
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A shell that traps its exit into `marker` (and, `ignore`, ignores
    /// `SIGTERM`), backgrounds a long `sleep` whose pid it writes to `pidfile`,
    /// and waits on it.
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

    /// A child with a grace ([`Cmd::term_grace`]) is sent `SIGTERM` at its
    /// ceiling, so its EXIT trap runs, and the run moves on once it has exited
    /// rather than waiting out the grace. Without a grace the same child dies
    /// of the `SIGKILL` and runs no trap (the negative control); a child that
    /// ignores the `SIGTERM` is `SIGKILL`ed with its group when the grace runs
    /// out. Every grandchild is gone.
    #[cfg(unix)]
    #[test]
    fn a_graceful_child_runs_its_exit_trap_and_its_grace_is_a_bound() {
        let tmp = crate::mktemp_dir("atv-ceil-grace").expect("mktemp");
        let cases = [
            ("graced", Some(Duration::from_secs(600)), false, true),
            ("ungraced", None, false, false),
            (
                "ignores-term",
                Some(Duration::from_millis(700)),
                true,
                false,
            ),
        ];
        for (name, grace, ignore, trap_runs) in cases {
            // As in the group-kill test: a ceiling that fires before the shell
            // has backgrounded its `sleep` tests nothing, so it doubles.
            let mut ceiling = Duration::from_millis(500);
            let (r, marker, grandchild) = loop {
                let stem = format!("{name}-{}ms", ceiling.as_millis());
                let (marker, pidfile) = (
                    tmp.join(format!("{stem}.trap")),
                    tmp.join(format!("{stem}.pid")),
                );
                let mut cmd = suite_with_trap(&marker, &pidfile, ignore);
                if let Some(g) = grace {
                    cmd = cmd.term_grace(g);
                }
                let r = run(&cmd, ceiled(&tmp, Some(ceiling)));
                if let Some(pid) = written_pid(&pidfile) {
                    break (r, marker, pid);
                }
                assert!(
                    ceiling < GONE,
                    "{name}: no shell backgrounded its sleep within {ceiling:?}"
                );
                ceiling *= 2;
            };
            assert!(
                !r.ok && r.output.contains(crate::findings::CEILING_KILL),
                "{name}: {}",
                r.output
            );
            assert_eq!(
                marker.exists(),
                trap_runs,
                "{name}: the EXIT trap\n{}",
                r.output
            );
            assert!(
                gone_within(&grandchild, GONE),
                "{name}: {grandchild} outlived the kill"
            );
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// An interrupted run takes its stage children with it. The test re-executes
    /// itself as a stand-in run that installs [`group::kill_on_interrupt`], runs
    /// a stage child with a grandchild, and is sent `SIGINT` as a terminal's
    /// Ctrl-C would once the child is listed and the grandchild's pid written:
    /// the grandchild is gone (its own group never heard the Ctrl-C) and the
    /// stand-in died of `SIGINT`.
    #[cfg(unix)]
    #[test]
    fn an_interrupted_gate_kills_every_live_stage_group() {
        use std::os::unix::process::ExitStatusExt as _;
        const PIDFILE: &str = "ATV_INTERRUPT_PIDFILE";
        if let Some(pidfile) = std::env::var_os(PIDFILE) {
            group::kill_on_interrupt();
            let pidfile = PathBuf::from(pidfile);
            let armed = pidfile.with_extension("armed");
            std::thread::scope(|scope| {
                let stage = scope.spawn(|| {
                    run(
                        &with_grandchild(&pidfile),
                        ceiled(pidfile.parent().expect("a dir"), None),
                    )
                });
                let deadline = Instant::now() + HANG_DETECTOR;
                while !stage.is_finished() && Instant::now() < deadline {
                    if group::live_count() > 0 && written_pid(&pidfile).is_some() {
                        std::fs::write(&armed, "armed\n").expect("the armed marker");
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                let _ = stage.join();
            });
            return;
        }
        let tmp = crate::mktemp_dir("atv-interrupt").expect("mktemp");
        let me = std::env::current_exe().expect("the test binary");
        let stand_in = |pidfile: &Path| {
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
            let mut child = c.spawn().expect("the stand-in run starts");
            let armed = pidfile.with_extension("armed");
            let deadline = Instant::now() + HANG_DETECTOR;
            while !armed.exists() {
                if Instant::now() > deadline || matches!(child.try_wait(), Ok(Some(_))) {
                    let _ = child.kill();
                    let _ = child.wait();
                    if let Some(pid) = written_pid(pidfile) {
                        let _ = std::process::Command::new("/bin/kill")
                            .args(["-KILL", &pid])
                            .status();
                    }
                    panic!("the stand-in run never armed ({})", armed.display());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let grandchild = read_pid(pidfile);
            std::process::Command::new("/bin/kill")
                .args(["-INT", &child.id().to_string()])
                .status()
                .expect("/bin/kill runs");
            (child.wait().expect("the stand-in exits"), grandchild)
        };

        let (status, grandchild) = stand_in(&tmp.join("handled.pid"));
        assert_eq!(
            status.signal(),
            Some(2),
            "it still dies of SIGINT: {status:?}"
        );
        assert!(
            gone_within(&grandchild, GONE),
            "the grandchild {grandchild} outlived the interrupted run"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    // -----------------------------------------------------------------------
    // In-process walks under the ceiling, and the children they launch.
    // -----------------------------------------------------------------------

    /// A walk answers, panics or outlives its ceiling; only the first is an
    /// answer, and a hung one never holds the caller past the ceiling.
    #[test]
    fn an_in_process_walk_answers_or_is_named_unfinished() {
        assert_eq!(bounded(Some(DEFAULT_CHILD_CEILING), || 7), Ok(7));
        assert_eq!(bounded(None, || 7), Ok(7));
        let panicked = bounded(None, || -> u8 { panic!("a walk that panics") });
        assert_eq!(panicked, Err(Unfinished::Panicked));
        let (_hold, never) = std::sync::mpsc::channel::<()>();
        let ceiling = Duration::from_millis(50);
        let hung = bounded(Some(ceiling), move || never.recv().ok());
        assert_eq!(hung, Err(Unfinished::TimedOut(ceiling)));
    }

    /// The bounded walks' child: this test binary, no shell or real tools. Unless
    /// it is the normal case, it ends itself by `SIGKILL` once `stop` appears, so
    /// no test signals a pid its owner may have reaped.
    #[cfg(unix)]
    #[test]
    fn bounded_output_stand_in() {
        let Some(dir) = std::env::var_os("ATERM_VERIFY_BOUNDED_OUTPUT_CHILD") else {
            return;
        };
        let dir = PathBuf::from(dir);
        println!("owned stdout");
        eprintln!("owned stderr");
        std::fs::write(dir.join("ready"), format!("{}\n", std::process::id())).unwrap();
        if std::env::var_os("ATERM_VERIFY_BOUNDED_OUTPUT_NORMAL").is_some() {
            return;
        }
        while !dir.join("stop").exists() {
            std::thread::sleep(Duration::from_millis(1));
        }
        aterm_uds::process::kill_self_now();
    }

    #[cfg(unix)]
    fn bounded_output_command(dir: &Path, normal: bool) -> aterm_uds::spawnfd::Command {
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "exec::tests::bounded_output_stand_in",
                "--nocapture",
            ])
            .env("ATERM_VERIFY_BOUNDED_OUTPUT_CHILD", dir);
        if normal {
            command.env("ATERM_VERIFY_BOUNDED_OUTPUT_NORMAL", "1");
        }
        command
    }

    /// The deadline is injected once the real child is live, and the timeout is
    /// not reported while that child runs. `Cleanup` ends the child even when the
    /// assertion fails, so no test process is orphaned.
    #[cfg(unix)]
    #[test]
    fn a_bounded_walk_reaps_its_output_child_before_reporting_timeout() {
        let dir = aterm_tempfile::tempdir().unwrap();
        struct Cleanup {
            stop: PathBuf,
            done: std::sync::mpsc::Receiver<std::io::Result<std::process::Output>>,
        }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                std::fs::write(&self.stop, "stop").unwrap();
                let output = self
                    .done
                    .recv_timeout(HANG_DETECTOR)
                    .expect("owned output worker ended");
                if let Ok(output) = output {
                    use std::os::unix::process::ExitStatusExt as _;
                    assert_eq!(output.status.signal(), Some(9), "stand-in dies by SIGKILL");
                }
            }
        }
        let (done, rx) = std::sync::mpsc::channel();
        let cleanup = Cleanup {
            stop: dir.path().join("stop"),
            done: rx,
        };
        let work = dir.path().to_owned();
        let mut pid = String::new();
        let timed_out: Result<(), _> = bounded_wait(
            move || {
                let output = aterm_uds::output::output(&mut bounded_output_command(&work, false));
                done.send(output).unwrap();
            },
            |_| {
                pid = read_pid(&dir.path().join("ready"));
                assert!(
                    aterm_uds::process::pid_alive(pid.parse().unwrap()),
                    "actual stand-in is live"
                );
                Err(Unfinished::TimedOut(HANG_DETECTOR))
            },
        );
        assert_eq!(timed_out, Err(Unfinished::TimedOut(HANG_DETECTOR)));
        let absent = !aterm_uds::process::pid_alive(pid.parse().unwrap());
        drop(cleanup);
        assert!(
            absent,
            "bounded returned with its output child {pid} still alive"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_bounded_walk_keeps_normal_child_output() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let work = dir.path().to_owned();
        let output = bounded(Some(HANG_DETECTOR), move || {
            aterm_uds::output::output(&mut bounded_output_command(&work, true))
        })
        .unwrap()
        .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("owned stdout"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("owned stderr"));
    }

    /// A walk stuck outside a command when its row times out cannot start a
    /// child once it resumes; a new walk still can
    /// ([`a_bounded_walk_keeps_normal_child_output`]).
    #[cfg(unix)]
    #[test]
    fn a_timed_out_walk_cannot_launch_a_child_later() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let work = dir.path().to_owned();
        let (parked, ready) = std::sync::mpsc::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let (sent, result) = std::sync::mpsc::channel();
        let timed_out: Result<(), _> = bounded_wait(
            move || {
                parked.send(()).unwrap();
                resume
                    .recv_timeout(HANG_DETECTOR)
                    .expect("release the owned walk");
                sent.send(aterm_uds::output::output(&mut bounded_output_command(
                    &work, true,
                )))
                .unwrap();
            },
            |_| {
                ready
                    .recv_timeout(HANG_DETECTOR)
                    .expect("walk parked before launching");
                Err(Unfinished::TimedOut(HANG_DETECTOR))
            },
        );
        assert_eq!(timed_out, Err(Unfinished::TimedOut(HANG_DETECTOR)));
        release.send(()).unwrap();
        let output = result
            .recv_timeout(HANG_DETECTOR)
            .expect("late launch answered");
        assert_eq!(output.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
        assert!(
            !dir.path().join("ready").exists(),
            "the actual child never started"
        );
    }

    /// Nor can the value a timed-out walk returns: it is dropped under the
    /// walk's closed scope, and its destructor's command is refused.
    #[cfg(unix)]
    #[test]
    fn a_timed_out_walk_cannot_launch_from_its_returned_value_drop() {
        struct OutputOnDrop {
            work: PathBuf,
            sent: std::sync::mpsc::Sender<std::io::Result<std::process::Output>>,
        }
        impl Drop for OutputOnDrop {
            fn drop(&mut self) {
                let output =
                    aterm_uds::output::output(&mut bounded_output_command(&self.work, true));
                self.sent.send(output).unwrap();
            }
        }
        let dir = aterm_tempfile::tempdir().unwrap();
        let work = dir.path().to_owned();
        let (parked, ready) = std::sync::mpsc::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let (sent, result) = std::sync::mpsc::channel();
        let timed_out = bounded_wait(
            move || {
                parked.send(()).unwrap();
                resume
                    .recv_timeout(HANG_DETECTOR)
                    .expect("release the owned walk");
                OutputOnDrop { work, sent }
            },
            |_| {
                ready
                    .recv_timeout(HANG_DETECTOR)
                    .expect("walk parked before returning");
                Err(Unfinished::TimedOut(HANG_DETECTOR))
            },
        );
        assert!(matches!(timed_out, Err(Unfinished::TimedOut(limit)) if limit == HANG_DETECTOR));
        // The receiver is gone, so the worker's failed send drops the value
        // there, with no race.
        release.send(()).unwrap();
        let output = result
            .recv_timeout(HANG_DETECTOR)
            .expect("returned value was dropped");
        assert_eq!(output.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
        assert!(
            !dir.path().join("ready").exists(),
            "the actual child never started"
        );
    }

    #[cfg(unix)]
    fn spawned_argv(c: &Command) -> Vec<String> {
        std::iter::once(c.get_program())
            .chain(c.get_args())
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }

    /// With a stand-in `taskpolicy`, so it runs the same on every Unix.
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
        // A `DYLD_*` the child carries rides through `env`'s argv past the
        // SIP-protected `taskpolicy`; any other variable stays an env var.
        let loader = tool
            .clone()
            .env("DYLD_FALLBACK_LIBRARY_PATH", "/tc/lib:/usr/lib")
            .env("RUSTC", "/tc/bin/rustc")
            .demoted();
        let argv = spawned_argv(&spawn_command(&loader, env, &fake));
        if crate::is_executable_file(Path::new(ENV)) {
            assert_eq!(
                argv,
                [
                    fake_s.as_str(),
                    "-c",
                    "utility",
                    ENV,
                    "DYLD_FALLBACK_LIBRARY_PATH=/tc/lib:/usr/lib",
                    "/bin/sh",
                    "-c",
                    "exit 0"
                ]
            );
        }
        assert_eq!(
            spawned_argv(&spawn_command(
                &loader.clone().env("X", "1"),
                env,
                &tmp.join("absent")
            )),
            ["/bin/sh", "-c", "exit 0"],
            "no taskpolicy: the variables stay the Command's own"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "exec_custody_tests.rs"]
mod custody_tests;
