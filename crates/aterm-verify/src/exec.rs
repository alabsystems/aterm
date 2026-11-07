// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Running the stages' children.

use aterm_uds::spawnfd::{Child, Command, Stdio};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
    /// Extra environment on top of the inherited one, or the whole one under [`Cmd::exact_env`].
    pub envs: Vec<(OsString, OsString)>,
    pub capture: Capture,
    /// Spawn under utility QoS rather than the run's inherited tier while a person may be at the
    /// desk ([`AWAY_AFTER`]).
    pub demoted: bool,
    /// The directory the child starts in, in place of [`ExecEnv::cwd`].
    pub cwd: Option<PathBuf>,
    /// Whether [`Cmd::envs`] is the child's whole environment with nothing inherited.
    pub exact_env: bool,
    /// Text written at the head of the child's captured log before it starts ([`Capture::Emit`]).
    pub preamble: Option<String>,
    /// Grace period after a ceiling `SIGTERM` before the child's group is `SIGKILL`ed.
    pub term_grace: Option<Duration>,
    /// Whether the child's waits for admission or a lock count against its ceiling as work
    /// ([`Cmd::counting_waits`]); otherwise its tree marks them and the ceiling leaves them out.
    pub waits_count: bool,
    /// A test binary whose cases are read as they end ([`CaseWatch`]).
    pub test_cases: Option<CaseWatch>,
}

/// How a test binary's cases are read while it runs (`exec_test_progress.rs`'s `LiveCases`):
/// each failed case is said at once (a `test-case-failed` event), and under `stop_at_red` the
/// first decisive one ends the binary ([`Run::stopped_at`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaseWatch {
    /// cargo's re-run spec for the binary.
    pub spec: String,
    /// The binary writes libtest's JSON events; a human log's failures are decided at its end.
    pub json: bool,
    /// End the binary at its first decisive red case: the run halts at its first red.
    pub stop_at_red: bool,
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
            waits_count: false,
            test_cases: None,
        }
    }

    /// A child replayed from a recorded invocation in `cwd` with `env` and nothing inherited.
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

    /// Write `text` at the head of the captured log before the child starts ([`Cmd::preamble`]).
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

    /// Set `k` to `v` for the child: in place of a value set before, as the child sees only the
    /// last, so each variable is named once wherever the command is read back (a rerun line).
    #[must_use]
    pub fn env(mut self, k: impl Into<OsString>, v: impl Into<OsString>) -> Self {
        let (k, v) = (k.into(), v.into());
        match self.envs.iter_mut().find(|(key, _)| *key == k) {
            Some(set) => set.1 = v,
            None => self.envs.push((k, v)),
        }
        self
    }

    #[must_use]
    pub const fn capture(mut self, c: Capture) -> Self {
        self.capture = c;
        self
    }

    /// Run this child at utility QoS (`taskpolicy -c utility`) where macOS has it, while a person
    /// may be at the desk ([`AWAY_AFTER`]): a compile-only child at the default tier stretches a
    /// typed program's echo, and with nobody there utility QoS only starves the check.
    #[must_use]
    pub const fn demoted(mut self) -> Self {
        self.demoted = true;
        self
    }

    /// At its ceiling, send this child's group `SIGTERM` and give it up to `grace` before
    /// `SIGKILL`.
    #[must_use]
    pub const fn term_grace(mut self, grace: Duration) -> Self {
        self.term_grace = Some(grace);
        self
    }

    /// Count this child's waits for admission or a lock as work ([`Cmd::waits_count`]): a test
    /// run's, whose tests wait on their own scratch locks and ledgers, so a test stuck in one is a
    /// hang its ceiling still ends. Its waits for a core are still left out, as any child's are,
    /// until [`NO_PROGRESS_HANG`] passes with no progress.
    #[must_use]
    pub const fn counting_waits(mut self) -> Self {
        self.waits_count = true;
        self
    }

    /// The argv as printable strings.
    #[must_use]
    pub fn argv(&self) -> Vec<String> {
        std::iter::once(self.program.to_string_lossy().into_owned())
            .chain(self.args.iter().map(|a| a.to_string_lossy().into_owned()))
            .collect()
    }
}

/// The bytes a child's label grows to before its next argument is left out ([`stage_label`]);
/// a program longer than this is kept whole all the same.
pub const LABEL_MAX: usize = 160;

/// A child's label, the one its run record's `group` line, its survivor line and its `verify:`
/// lines carry: its argv joined by spaces, the program always and each next argument while the
/// whole fits [`LABEL_MAX`] bytes, then ` …+N args` naming how many were left out. So it is at
/// most [`LABEL_MAX`] bytes and that count unless its program alone is longer; the run record
/// bounds its encoded `group` line on its own (`runs::stage_field`).
#[must_use]
pub fn stage_label(argv: &[String]) -> String {
    let mut label = String::new();
    for (i, arg) in argv.iter().enumerate() {
        if i > 0 {
            if label.len() + 1 + arg.len() > LABEL_MAX {
                let left = argv.len() - i;
                return format!("{label} …+{left} arg{}", if left == 1 { "" } else { "s" });
            }
            label.push(' ');
        }
        label.push_str(arg);
    }
    label
}

/// The invariant part of every invocation.
#[derive(Clone, Copy, Debug)]
pub struct ExecEnv<'a> {
    pub cwd: &'a Path,
    pub path: &'a OsStr,
    pub scratch: &'a Path,
    /// The ceiling on one child's work, or `None` to wait forever: the time a wait of its tree
    /// for admission or a lock is marked ([`aterm_toolchain::waits`]) is left out, and so is the
    /// time it waited for a core ([`Host::group_usage`]).
    pub child_ceiling: Option<Duration>,
    /// Variables removed from every child's inherited environment before [`Cmd::envs`].
    pub remove_env: &'a [&'a str],
    /// Variables added to every child after [`Self::remove_env`] and before [`Cmd::envs`].
    pub add_env: &'a [(OsString, OsString)],
    /// The run's record of its stage groups, when it keeps one ([`crate::runs`]).
    pub stage_groups: Option<&'a dyn StageGroupEnd>,
    /// What a child's spawn and its clock read of the machine ([`LIVE_HOST`] in a run).
    pub host: &'a dyn Host,
}

/// What a child's spawn and its clock read of the machine
/// (`docs/DESIGN-adaptive-build-2026-10-24.md`, CEILING-WAIT), injected through
/// [`ExecEnv::host`]: [`LIVE_HOST`] reads the OS, a test's stand-in says what it is told.
pub trait Host: Sync + std::fmt::Debug {
    /// What each member of process group `pgid` has used, read now
    /// ([`aterm_sysprobe::group_usage`]); `None` where unread, and the clock then counts every
    /// span with no marked wait as work.
    fn group_usage(&self, pgid: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>>;
    /// How long since a person last touched this machine's keyboard or mouse
    /// ([`aterm_sysprobe::hid_idle`]); `None` where unread.
    fn idle(&self) -> Option<Duration>;
    /// How many cores a child's tree can run on at once.
    fn cores(&self) -> u32;
}

/// The OS's answers ([`Host`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct LiveHost;

impl Host for LiveHost {
    fn group_usage(&self, pgid: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
        aterm_sysprobe::group_usage(pgid)
    }

    fn idle(&self) -> Option<Duration> {
        aterm_sysprobe::hid_idle()
    }

    fn cores(&self) -> u32 {
        std::thread::available_parallelism()
            .map_or(1, |n| u32::try_from(n.get()).unwrap_or(u32::MAX))
    }
}

/// The host a run's children are spawned and timed on.
pub static LIVE_HOST: LiveHost = LiveHost;

/// How long nobody may have touched the keyboard or mouse before a [`Cmd::demoted`] child is
/// spawned at the run's own tier: no one is at the desk to type, so utility QoS would protect no
/// one's echo and only starve the check behind other sessions' work (measured on 2026-10-27 under
/// `docs/DESIGN-adaptive-build-2026-10-24.md`, CEILING-WAIT).
pub const AWAY_AFTER: Duration = Duration::from_secs(10 * 60);

/// Whether a person may be at the desk: input within [`AWAY_AFTER`], or an input clock that
/// cannot be read.
fn at_desk(host: &dyn Host) -> bool {
    host.idle().is_none_or(|idle| idle < AWAY_AFTER)
}

/// Process-group lifecycle hooks invoked while group custody ([`group::Live`]) holds the leader.
pub trait StageGroupEnd: Sync + std::fmt::Debug {
    /// Record a newly created stage process group.
    fn group_made(&self, pgid: u32, label: &str);
    /// Sweep a stage process group whose leader has exited but is not yet reaped.
    fn group_ended(&self, pgid: u32, label: &str);
    /// Record the group [`Self::group_ended`] swept, once its leader is reaped: the leader no
    /// longer holds the group's id then, so the kernel's answer about the whole group finds it
    /// empty only when no process is left in it either, and can prove it empty where the
    /// sweep's listing could not ([`group::empty`]).
    fn group_reaped(&self, pgid: u32, label: &str);
}

/// What a child did.
#[derive(Clone, Debug, Default)]
pub struct Run {
    /// True only on exit status 0.
    pub ok: bool,
    pub output: String,
    /// The exit code.
    pub code: Option<i32>,
    /// Set when the child could not be spawned at all (tool vanished mid-run).
    pub spawn_error: Option<String>,
    /// What its tree used, as its clock read it ([`TreeUsage`]); `None` where no read of its tree
    /// found a member (a host with no reading: every platform but macOS; a child that never
    /// started), never zeros.
    pub usage: Option<TreeUsage>,
    /// Set when verify ended this test binary at its first decisive red case ([`CaseWatch`]):
    /// its run is a FAIL by construction, read from the blocks of the cases that finished.
    pub stopped_at: Option<crate::libtest_json::Stopped>,
}

/// WHAT A CHILD'S TREE USED (`docs/DESIGN-fast-build-release-2026-11-05.md` §4.1, A1), as its
/// clock read its process group ([`Host::group_usage`]): on exactly the reads that book its
/// stage's meter ([`crate::memory::Meter::book`]: the clock's reads and the sweep's read of its
/// ended leader), booked into a meter of its own as well, so the CPU of a stage's children sums
/// to the stage's observed CPU ([`crate::memory::Meter::observed_cpu`]), apart from what the
/// output children of a walk the stage runs ([`bounded`]) book into its meter, which no run
/// carries, and from an incarnation two children's reads both find, which each of them counts.
/// A lower bound, as the stage's is: a member that ends between two reads takes what it used
/// since the read before with it, and a descendant that leaves the group (`setsid`, `setpgid`)
/// is never read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TreeUsage {
    /// CPU time: each process incarnation read (by pid and start) at the most its counter was
    /// read at, summed.
    pub cpu: Duration,
    /// Time runnable, on a core or waiting for one, read and summed as `cpu` is.
    pub runnable: Duration,
    /// The part of its work its tree spent waiting for a core, which its ceiling left out
    /// ([`core_wait`]).
    pub starved: Duration,
    /// The time a wait of its tree for admission or a lock was marked
    /// ([`aterm_toolchain::waits`]), which its ceiling left out too.
    pub waited: Duration,
    /// From its spawn to its end.
    pub wall: Duration,
    /// The most its tree held at one read, footprint bytes, and never less than the largest
    /// lifetime peak a read found ([`crate::memory::Meter::peak`]).
    pub peak_bytes: u64,
    /// The process incarnations read.
    pub processes: usize,
}

/// The OS's own sentence for ENOSPC.
pub const ENOSPC_SENTENCE: &str = "No space left on device";

impl Run {
    #[must_use]
    pub fn trimmed_output(&self) -> &str {
        self.output.trim_end_matches('\n')
    }

    /// Why this child is not a finding about the tree, if it failed for an environment reason.
    #[must_use]
    pub fn environment_failure(&self) -> Option<String> {
        if self.ok {
            return None;
        }
        if let Some(e) = &self.spawn_error {
            return Some(e.clone());
        }
        if crate::libtest::has_ceiling_kill(&self.output) {
            return None;
        }
        if out_of_disk(&self.output) {
            return Some(format!("the child ran out of disk ({ENOSPC_SENTENCE})"));
        }
        crate::libtest::environment_refusals(&self.output)
            .map(|names| crate::libtest::refusal_reason("every failing test", &names, &self.output))
    }

    /// How this child ended, for a NOT RUN row whose child named no cause of its own: its exit
    /// code, or that it had none (a signal ended it, the run's interrupt among them).
    #[must_use]
    pub fn unnamed_cause(&self) -> String {
        let ended = self.code.map_or_else(
            || "ended with no exit code (a signal ended it, the run's interrupt among them)".into(),
            |code| format!("exited {code}"),
        );
        format!("it {ended} and named no cause (what it printed is in the check's log)")
    }
}

/// Whether a failed child's log says the disk ran out: when it accounts for its failures as failed
/// tests, only if each one's own block names [`ENOSPC_SENTENCE`] (a test that passes may print a full
/// disk it simulated); otherwise anywhere, as a build or a tool that died says it. Passing test
/// binaries and ceiling kills are excluded ([`crate::libtest::out_of_disk`]).
fn out_of_disk(output: &str) -> bool {
    crate::libtest::out_of_disk(output)
}

/// The default ceiling on one stage child's work ([`ExecEnv::child_ceiling`]).
pub const DEFAULT_CHILD_CEILING: Duration = Duration::from_secs(3 * 60 * 60);

/// Why an in-process walk ([`bounded`]) gave no answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unfinished {
    /// Still running at the ceiling.
    TimedOut(Duration),
    /// It panicked (the panic printed on stderr).
    Panicked,
    /// The run's end left it unfinished: these words, the halt's once its first proven red ended
    /// the run ([`halted`]), else an interrupt's. The walk's thread is left to the process's exit.
    Stopped(&'static str),
}

impl Unfinished {
    /// The reason, for a NOT RUN row.
    #[must_use]
    pub fn sentence(self) -> String {
        match self {
            Self::TimedOut(d) => format!("did not finish within the {}s ceiling", d.as_secs()),
            Self::Panicked => "panicked (above, on stderr)".to_string(),
            Self::Stopped(why) => format!("was left unfinished: {why}"),
        }
    }
}

/// How often a wait for an in-process walk ([`bounded`]) looks whether the run is ending.
const WALK_LOOK: Duration = Duration::from_millis(50);

/// Run an in-process walk on a thread of its own under the child ceiling, on the wall clock: no
/// walk waits for admission or a lock. Each child the walk starts books into the meter of the
/// stage that called this ([`crate::memory::current`]), on the walk's thread or through the
/// output executor it installs. Its quiet reservation, when the calling stage has one, stays
/// held until the worker actually ends even if the ceiling returns first. The actual stage
/// memory reservation is shared across the same handoff; a cover marker alone cannot hold it. The
/// run's end does not wait for it: once a halt or an interrupt is ending the run ([`ending`]), the
/// wait gives up within [`WALK_LOOK`], closing the walk's command scope so it starts no child.
/// # Errors
/// [`Unfinished`]: the walk timed out, panicked, or was left to the run's end.
pub fn bounded<T: Send + 'static>(
    ceiling: Option<Duration>,
    walk: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Unfinished> {
    bounded_wait(walk, |rx| {
        let started = Instant::now();
        loop {
            let left = ceiling.map(|limit| limit.saturating_sub(started.elapsed()));
            match rx.recv_timeout(left.map_or(WALK_LOOK, |left| left.min(WALK_LOOK))) {
                Ok(answer) => return Ok(answer),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(Unfinished::Panicked);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
            if ending() {
                return Err(Unfinished::Stopped(
                    halted().unwrap_or("the run was interrupted while it ran"),
                ));
            }
            if let (Some(limit), Some(left)) = (ceiling, left)
                && left <= WALK_LOOK
            {
                return Err(Unfinished::TimedOut(limit));
            }
        }
    })
}

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
    let meter = crate::memory::current();
    let cover = crate::memory::cover();
    let quiet = crate::current_quiet_claim();
    let memory = crate::current_memory_claim();
    let scope = Close(std::sync::Arc::new(output::Scope::metered(meter.clone())));
    let executor = std::sync::Arc::clone(&scope.0);
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _metering = crate::memory::metering(meter);
        let _covering = crate::memory::covering(cover);
        let _quiet = crate::holding_quiet_claim(quiet);
        let _memory = crate::holding_memory_claim(memory);
        aterm_uds::output::with_executor(
            std::sync::Arc::new(move |command| executor.output(command)),
            || {
                tx.send(walk()).ok();
            },
        );
    });
    let answer = wait(rx);
    drop(scope);
    if worker.is_finished() {
        let _ = worker.join();
    }
    answer
}

#[cfg(test)]
#[path = "exec_memory_custody_tests.rs"]
mod memory_custody_tests;
#[path = "exec_output.rs"]
mod output;
#[path = "exec_test_progress.rs"]
mod test_progress;

const MAX_CEILING_SECS: f64 = 3_153_600_000.0;

/// Parse a `--timeout` value ([`DEFAULT_CHILD_CEILING`]).
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
        Ok(v) if v.is_finite() && (0.0..=MAX_CEILING_SECS).contains(&v) => {
            Some((v > 0.0).then(|| Duration::from_secs_f64(v)))
        }
        _ => None,
    }
}

static SEQ: AtomicU64 = AtomicU64::new(0);

/// macOS's QoS launcher.
pub const TASKPOLICY: &str = aterm_uds::os_path!("taskpolicy");

/// Spawn, wait, and collect.
#[must_use]
pub fn run(cmd: &Cmd, env: ExecEnv<'_>) -> Run {
    run_captured(cmd, env, None)
}

/// One compiler invocation's machine stdout, separate from its diagnostic stderr.
pub(crate) struct MachineRun {
    pub run: Run,
    pub stdout: Result<String, String>,
}

/// Capture compiler stdout without allowing diagnostic text to become machine evidence.
pub(crate) fn run_machine(cmd: &Cmd, env: ExecEnv<'_>) -> MachineRun {
    let mut stdout = Err("compiler stdout was not captured".to_owned());
    let run = run_captured(cmd, env, Some(&mut stdout));
    MachineRun { run, stdout }
}

fn run_captured(cmd: &Cmd, env: ExecEnv<'_>, machine: Option<&mut Result<String, String>>) -> Run {
    let mut c = spawn_command(cmd, env, Path::new(TASKPOLICY));
    c.current_dir(cmd.cwd.as_deref().unwrap_or(env.cwd));
    if cmd.exact_env {
        c.env_clear();
    } else {
        c.env("PATH", env.path);
        for k in env.remove_env {
            match k.strip_suffix('*') {
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
        if let Some(marker) = crate::memory::cover() {
            c.env(aterm_capacity::COVER_ENV, &*marker);
        }
    }
    for (k, v) in &cmd.envs {
        c.env(k, v);
    }
    // A replayed child's recorded environment holds the marker of the run that recorded it, not
    // the claim its stage holds now: that claim covers it.
    if cmd.exact_env
        && let Some(marker) = crate::memory::cover()
    {
        c.env(aterm_capacity::COVER_ENV, &*marker);
    }
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let marks = if cmd.waits_count {
        None
    } else {
        Marks::lay(env.scratch, seq)
    };
    match &marks {
        Some(m) => c.env(aterm_toolchain::waits::VAR, &m.dir),
        None => c.env_remove(aterm_toolchain::waits::VAR),
    };
    let marks = marks.as_ref().map(|m| m.dir.as_path());

    let label = stage_label(&cmd.argv());
    match &cmd.capture {
        Capture::Silent => {
            c.stdout(Stdio::null()).stderr(Stdio::null());
            finish(c, cmd, env, None, &label, marks).0
        }
        Capture::Emit => {
            // Quiet transcript writes without changing the short root for Unix sockets.
            let log = aterm_tempfile::quiet_in(env.scratch)
                .join(format!("stage.{}.{seq}.log", std::process::id()));
            let mut file = match create_stage_log(&log) {
                Ok(f) => f,
                Err(e) => return spawn_failure(cmd, &e.to_string()),
            };
            if let Some(text) = &cmd.preamble
                && let Err(e) = file.write_all(text.as_bytes())
            {
                std::fs::remove_file(&log).ok();
                return spawn_failure(cmd, &e.to_string());
            }
            let stdout_log = machine.as_ref().map(|_| log.with_extension("stdout"));
            let file2 = match &stdout_log {
                Some(path) => create_stage_log(path),
                None => file.try_clone(),
            };
            let file2 = match file2 {
                Ok(f) => f,
                Err(e) => {
                    std::fs::remove_file(&log).ok();
                    return spawn_failure(cmd, &e.to_string());
                }
            };
            if machine.is_some() {
                c.stdout(file2).stderr(file);
            } else {
                c.stdout(file).stderr(file2);
            }
            let (mut r, logged) = finish(c, cmd, env, Some(&log), &label, marks);
            r.output
                .insert_str(0, logged.as_deref().unwrap_or_default());
            if let Some(stdout) = machine {
                let path = stdout_log.expect("machine capture has a stdout file");
                *stdout = match crate::read_captured(&path) {
                    Ok(bytes) => match String::from_utf8(bytes) {
                        Ok(text) => Ok(text),
                        Err(error) => {
                            r.output
                                .push_str("\n--- invalid UTF-8 compiler stdout ---\n");
                            r.output
                                .push_str(&String::from_utf8_lossy(error.as_bytes()));
                            Err("compiler stdout was not UTF-8".to_owned())
                        }
                    },
                    Err(error) => Err(format!("could not read compiler stdout: {error}")),
                };
                std::fs::remove_file(path).ok();
            }
            std::fs::remove_file(&log).ok();
            r
        }
    }
}

fn create_stage_log(path: &Path) -> io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    options.open(path)
}

/// The directory a child's tree marks its waits in ([`aterm_toolchain::waits::VAR`]), removed
/// with what a waiter killed mid-wait left in it once the child has been reaped.
struct Marks {
    dir: PathBuf,
}

impl Marks {
    /// A fresh one under `scratch`; `None` where it cannot be made, and the child's clock then
    /// counts every wait as work, as a run with no marks did.
    fn lay(scratch: &Path, seq: u64) -> Option<Marks> {
        let dir = scratch.join(format!("waits.{}.{seq}", std::process::id()));
        std::fs::create_dir(&dir).ok()?;
        Some(Marks { dir })
    }
}

impl Drop for Marks {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// How often a child's clock looks for a marked wait in its tree: as often as its poll loop wakes
/// at most, well inside the [`aterm_toolchain::named_lock::TICK`] a lock's waiter lets pass before
/// it first asks whether to stop, so a wait the run's end stops has been seen before it ends.
const LOOK_EVERY: Duration = Duration::from_millis(25);

/// How often a child's clock reads what its tree used ([`Host::group_usage`]) while its work is
/// short of its ceiling; at the ceiling it reads once more before it judges.
pub(crate) const READ_USAGE_EVERY: Duration = Duration::from_millis(500);

/// How often a child's stage meter reads its process group in its first [`READ_USAGE_EVERY`]: a
/// child that ends before the first full read (a 400 ms link of 3 GiB) is still read near its peak,
/// and not booked as nothing or not at all.
const EARLY_METER_EVERY: Duration = Duration::from_millis(25);

/// Whether a child begun at `begun`, its group last read for its meter at `last`, is due a read
/// at `now`: at once, then every [`EARLY_METER_EVERY`] through its first [`READ_USAGE_EVERY`],
/// then every [`READ_USAGE_EVERY`].
fn meter_due(begun: Instant, last: Option<Instant>, now: Instant) -> bool {
    let every = if now.saturating_duration_since(begun) < READ_USAGE_EVERY {
        EARLY_METER_EVERY
    } else {
        READ_USAGE_EVERY
    };
    last.is_none_or(|t| now.saturating_duration_since(t) >= every)
}

/// How long a child's tree may go with no CPU time and no I/O before a span it spent waiting for a
/// core counts as work again: a tree that gets no core at all for this long is a hang its ceiling
/// ends, as a stopped one is.
const NO_PROGRESS_HANG: Duration = Duration::from_secs(10);

/// How far back a child's clock judges its tree's wait for a core ([`CoreWaitSpan`]). The kernel
/// books a thread's runnable and CPU time only when the thread next gets a core or leaves one, so
/// the wait of a starved tree shows in a later read than the window it fell in: a utility-tier busy
/// loop's runnable time read every 25 ms on this Mac on 2026-10-27 stayed flat in 61-66% of the
/// reads and then grew in lumps of up to 925 ms. A tree that gets no core for longer than this is a
/// hang ([`NO_PROGRESS_HANG`]).
const CORE_WAIT_SPAN: Duration = Duration::from_secs(10);

/// A child's tree's wait for a core longer than this is said as the child ends
/// ([`Clock::starved_line`]).
const STARVED_SAID: Duration = Duration::from_secs(60);

/// What a child's tree used between two reads of it ([`used_between`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Used {
    /// CPU time, every thread's.
    cpu: Duration,
    /// Time runnable, on a core or waiting for one, every thread's.
    runnable: Duration,
    /// Bytes read and written ([`aterm_sysprobe::MemberUsage::io_bytes`]).
    io: u64,
}

/// What `now`'s members used since `before`'s read of them: each member both found, by pid and
/// start, by the growth of its counters; one only `now` found, by all of them (it started since).
/// A member gone since takes what it used after `before` with it. Also `now`'s members, keyed for
/// the next read.
fn used_between(
    before: &BTreeMap<(u32, u64), aterm_sysprobe::MemberUsage>,
    now: Vec<aterm_sysprobe::MemberUsage>,
) -> (Used, BTreeMap<(u32, u64), aterm_sysprobe::MemberUsage>) {
    let mut used = Used::default();
    let mut seen = BTreeMap::new();
    for m in now {
        let was = before.get(&(m.pid, m.start));
        let grew = |pick: fn(&aterm_sysprobe::MemberUsage) -> u64| {
            pick(&m).saturating_sub(was.map_or(0, pick))
        };
        used.cpu += Duration::from_nanos(grew(|u| u.cpu_ns));
        used.runnable += Duration::from_nanos(grew(|u| u.runnable_ns));
        used.io = used.io.saturating_add(grew(|u| u.io_bytes));
        seen.insert((m.pid, m.start), m);
    }
    (used, seen)
}

/// The part of a window of `wall` that a tree which used `used` in it spent waiting for a core,
/// on a machine of `cores`: the window less the share of its demand it got, where its demand is
/// the time it was runnable, at most every core for the whole window, and never more than the
/// time its threads were runnable and not running. A tree that was runnable for none of the window
/// (stopped, or blocked in a read, a lock or a sleep) waited for no core in it.
fn core_wait(wall: Duration, used: Used, cores: u32) -> Duration {
    let waiting = used.runnable.saturating_sub(used.cpu);
    let demand = used.runnable.min(wall.saturating_mul(cores.max(1)));
    if demand.is_zero() || waiting.is_zero() {
        return Duration::ZERO;
    }
    let share = (used.cpu.as_secs_f64() / demand.as_secs_f64()).clamp(0.0, 1.0);
    wall.mul_f64(1.0 - share).min(waiting)
}

/// One read of a child's tree, as its clock's [`CoreWaitSpan`] keeps it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TreeRead {
    /// The wall time since the read before.
    wall: Duration,
    /// What the tree used in that time ([`used_between`]).
    used: Used,
    /// The work the clock counted in that time.
    worked: Duration,
    /// What this read moved from the clock's work to its wait for a core.
    moved: Duration,
}

/// The reads of a child's tree in the last [`CORE_WAIT_SPAN`], judged together for its wait for a
/// core: a wait the kernel books late, in a lump, is still left out, and a tree whose share of the
/// cores changes is judged on its last span, not on its whole life.
#[derive(Debug, Default)]
struct CoreWaitSpan {
    reads: VecDeque<TreeRead>,
}

impl CoreWaitSpan {
    /// Keep a read of `wall`, in which the tree used `used` and the clock counted `worked` as work,
    /// dropping the oldest reads while the ones after them still cover [`CORE_WAIT_SPAN`]. Then say
    /// what this read moves from the clock's work, `left` in all, to its wait for a core: the wait
    /// over the kept reads together, up to the last in which the kernel booked any runnable or CPU
    /// time ([`core_wait`] of their summed wall and use on `cores`), at most the work counted in
    /// them, less what every kept read moved before, and at most `left`. Reads after the last
    /// booking are judged once one covers them, so a tree that stops after a stretch starved
    /// waited for no core in its stop. A read moves nothing unless that wait has grown past what
    /// the kept reads moved.
    fn book(
        &mut self,
        wall: Duration,
        used: Used,
        worked: Duration,
        cores: u32,
        left: Duration,
    ) -> Duration {
        self.reads.push_back(TreeRead {
            wall,
            used,
            worked,
            moved: Duration::ZERO,
        });
        let mut covered: Duration = self.reads.iter().map(|r| r.wall).sum();
        while self.reads.len() > 1
            && let Some(oldest) = self.reads.front()
            && covered.saturating_sub(oldest.wall) >= CORE_WAIT_SPAN
        {
            covered = covered.saturating_sub(oldest.wall);
            self.reads.pop_front();
        }
        let booked = self
            .reads
            .iter()
            .rposition(|r| !r.used.runnable.is_zero() || !r.used.cpu.is_zero())
            .map_or(0, |last| last + 1);
        let mut all = TreeRead::default();
        for (i, r) in self.reads.iter().enumerate() {
            if i < booked {
                all.wall += r.wall;
                all.used.cpu += r.used.cpu;
                all.used.runnable += r.used.runnable;
                all.used.io = all.used.io.saturating_add(r.used.io);
                all.worked += r.worked;
            }
            all.moved += r.moved;
        }
        let moved = core_wait(all.wall, all.used, cores)
            .min(all.worked)
            .saturating_sub(all.moved)
            .min(left);
        if let Some(this) = self.reads.back_mut() {
            this.moved = moved;
        }
        moved
    }

    /// Forget every read: a tree that made no progress for [`NO_PROGRESS_HANG`] spent that time in
    /// a hang, which no later read's lump may book as a wait for a core.
    fn clear(&mut self) {
        self.reads.clear();
    }
}

/// A clock's reads of what its child's tree used (CEILING-WAIT's wait for a core).
struct Usage<'a> {
    host: &'a dyn Host,
    /// The child's process group: its tree.
    pgid: u32,
    cores: u32,
    /// When it last read, and each member that read found.
    read_at: Instant,
    seen: BTreeMap<(u32, u64), aterm_sysprobe::MemberUsage>,
    /// The work counted since that read.
    worked: Duration,
    /// When a read last found CPU time or I/O ([`NO_PROGRESS_HANG`]).
    progressed_at: Instant,
    /// The reads its wait for a core is judged on.
    span: CoreWaitSpan,
}

/// A CHILD'S OWN METER ([`TreeUsage`]): every read its clock and its sweep make of its tree, booked
/// as its stage's meter books them ([`crate::memory::Meter::book`]), and each incarnation's most
/// runnable time beside them.
#[derive(Debug, Default)]
struct Own {
    meter: crate::memory::Meter,
    /// The most runnable time each process incarnation, by pid and start, was read at.
    runnable: std::sync::Mutex<BTreeMap<(u32, u64), u64>>,
}

impl Own {
    /// Book a read of the process group `pgid`, whose members are `members`.
    fn book(&self, pgid: u32, members: &[aterm_sysprobe::MemberUsage]) {
        self.meter.book(pgid, members);
        let mut runnable = self
            .runnable
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for m in members {
            let most = runnable.entry((m.pid, m.start)).or_default();
            *most = (*most).max(m.runnable_ns);
        }
    }

    /// What the child used, its clock `clock` and its wall `wall`; `None` when no read booked a
    /// member of its tree.
    fn usage(&self, clock: &Clock<'_>, wall: Duration) -> Option<TreeUsage> {
        let cpu = self.meter.observed_cpu()?;
        let runnable = self
            .runnable
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .fold(0_u64, |sum, ns| sum.saturating_add(*ns));
        Some(TreeUsage {
            cpu: Duration::from_nanos(cpu.nanoseconds),
            runnable: Duration::from_nanos(runnable),
            starved: clock.starved,
            waited: clock.waited,
            wall,
            peak_bytes: self.meter.peak().unwrap_or(0),
            processes: cpu.processes,
        })
    }
}

/// A child's clock (`docs/DESIGN-adaptive-build-2026-10-24.md`, CEILING-WAIT): the time it
/// worked, which its ceiling bounds, apart from the time a wait of its tree for admission or a
/// lock was marked in `marks` ([`aterm_toolchain::waits`]), which only its holders bound, and the
/// time its tree waited for a core ([`core_wait`]), which the tree's own progress bounds
/// ([`NO_PROGRESS_HANG`]). A wait's liveness is its own: its holder named once a minute, and a
/// dead holder lets its lock go; a live holder whose processes make no progress for its waiter's
/// bound is named stuck in the mark, and the child is then ended ([`Clock::stuck`]), its holder
/// left alone (CW-WAIT-UNBOUNDED). Each span counts as the last look found it; a read of the tree
/// then moves from its work to [`Clock::starved`] what the reads of the last [`CORE_WAIT_SPAN`]
/// show it waited for a core and have not moved yet ([`CoreWaitSpan::book`]). Each read also
/// books the tree's footprint into its stage's memory meter (VERIFY-MEM, [`crate::memory`]) and
/// into the child's own ([`Own`], [`TreeUsage`]), and the clock's end takes the tree out of its
/// stage's.
struct Clock<'a> {
    marks: Option<&'a Path>,
    last: Instant,
    looked: Option<Instant>,
    worked: Duration,
    waited: Duration,
    /// The time its tree waited for a core, which its ceiling leaves out too.
    starved: Duration,
    /// The waits the last look found.
    waits: Vec<aterm_toolchain::waits::Waiting>,
    /// The waits the preceding look found. A mark is rewritten in place, so one read can catch
    /// the tail of the record it replaces: only the same live mark and holder set found stuck
    /// on both looks can end the child ([`Clock::stuck`]).
    waits_before: Vec<aterm_toolchain::waits::Waiting>,
    /// `None` once its host has no reading of the tree: every span with no marked wait is work.
    usage: Option<Usage<'a>>,
    /// The child's own meter, which each read that books its stage's meter books too
    /// ([`TreeUsage`]); `None` for a clock that keeps no account of its child.
    own: Option<&'a Own>,
    /// The stage meter its reads of the tree book into, and the tree's process group.
    meter: Option<(Arc<crate::memory::Meter>, u32)>,
    /// When the child began, and when its first [`READ_USAGE_EVERY`]'s reads for its meter last
    /// read the tree ([`meter_due`]).
    begun: Instant,
    metered_at: Option<Instant>,
}

impl<'a> Clock<'a> {
    fn new(
        marks: Option<&'a Path>,
        host: &'a dyn Host,
        pgid: u32,
        meter: Option<Arc<crate::memory::Meter>>,
    ) -> Clock<'a> {
        let now = Instant::now();
        if let Some(m) = &meter {
            m.started();
        }
        Clock {
            meter: meter.map(|m| (m, pgid)),
            begun: now,
            metered_at: None,
            marks,
            last: now,
            looked: None,
            worked: Duration::ZERO,
            waited: Duration::ZERO,
            starved: Duration::ZERO,
            waits: Vec::new(),
            waits_before: Vec::new(),
            own: None,
            usage: Some(Usage {
                host,
                pgid,
                cores: host.cores(),
                read_at: now,
                seen: BTreeMap::new(),
                worked: Duration::ZERO,
                progressed_at: now,
                span: CoreWaitSpan::default(),
            }),
        }
    }

    /// This clock books its reads into `own` as well ([`Clock::own`]).
    fn owning(mut self, own: &'a Own) -> Clock<'a> {
        self.own = Some(own);
        self
    }

    /// Book a read of the tree's members into its stage's meter and its own, where it has them.
    fn book(&self, members: &[aterm_sysprobe::MemberUsage]) {
        if let Some((meter, pgid)) = &self.meter {
            meter.book(*pgid, members);
        }
        if let (Some(own), Some(u)) = (self.own, &self.usage) {
            own.book(u.pgid, members);
        }
    }

    /// Count the span since the last tick, then look again once [`LOOK_EVERY`] has passed, and
    /// read the tree once [`READ_USAGE_EVERY`] has; through the first of those, read it for its
    /// meters alone (its stage's, its own) as often as [`meter_due`] says.
    fn tick(&mut self) {
        let now = Instant::now();
        if now.saturating_duration_since(self.begun) < READ_USAGE_EVERY
            && meter_due(self.begun, self.metered_at, now)
        {
            self.metered_at = Some(now);
            if (self.meter.is_some() || self.own.is_some())
                && let Some(u) = &self.usage
                && let Some(members) = u.host.group_usage(u.pgid)
            {
                self.book(&members);
            }
        }
        let span = now.saturating_duration_since(self.last);
        if self.waits.is_empty() {
            self.worked += span;
            if let Some(u) = &mut self.usage {
                u.worked += span;
            }
        } else {
            self.waited += span;
        }
        self.last = now;
        if self
            .looked
            .is_none_or(|t| now.saturating_duration_since(t) >= LOOK_EVERY)
        {
            self.look();
        }
        if self
            .usage
            .as_ref()
            .is_some_and(|u| now.saturating_duration_since(u.read_at) >= READ_USAGE_EVERY)
        {
            self.read_usage();
        }
    }

    /// Read what the tree used since the last read, book what it holds and its live rate into its
    /// stage's meter ([`crate::memory::Meter::book_at`]) and into its own
    /// ([`Clock::own`]), and move to
    /// [`Clock::starved`] the part of its work that the reads of its span show it spent waiting
    /// for a core and have not moved yet
    /// ([`CoreWaitSpan::book`]), unless it has made no progress for [`NO_PROGRESS_HANG`]: then the
    /// span is forgotten and nothing moves. A host with no reading ends the reads.
    fn read_usage(&mut self) {
        let Some(u) = &mut self.usage else {
            return;
        };
        let now = self.last;
        let wall = now.saturating_duration_since(u.read_at);
        let worked = std::mem::take(&mut u.worked);
        u.read_at = now;
        let Some(members) = u.host.group_usage(u.pgid) else {
            self.usage = None;
            return;
        };
        if let Some((meter, pgid)) = &self.meter {
            meter.book_at(*pgid, &members, now);
        }
        if let Some(own) = self.own {
            own.book(u.pgid, &members);
        }
        let (used, seen) = used_between(&u.seen, members);
        u.seen = seen;
        if !used.cpu.is_zero() || used.io > 0 {
            u.progressed_at = now;
        }
        if now.saturating_duration_since(u.progressed_at) >= NO_PROGRESS_HANG {
            u.span.clear();
            return;
        }
        let moved = u.span.book(wall, used, worked, u.cores, self.worked);
        self.worked -= moved;
        self.starved += moved;
    }

    /// Whether its work has reached `limit`, as of a read of its tree that covers it: one more
    /// read first, judged with the reads of its span ([`CoreWaitSpan`]), so a wait for a core the
    /// kernel has booked since the last read is left out before the work is judged. The time since
    /// the tree's last booking is work until a read books it.
    fn over(&mut self, limit: Duration) -> bool {
        if self.worked < limit {
            return false;
        }
        if self.usage.as_ref().is_some_and(|u| u.read_at < self.last) {
            self.read_usage();
        }
        self.worked >= limit
    }

    /// What a child that waited longer than [`STARVED_SAID`] for a core says as it ends.
    fn starved_line(&self) -> Option<String> {
        (self.starved > STARVED_SAID).then(|| {
            format!(
                "it waited {} for a core beside {} of work, which the ceiling leaves out",
                aterm_capacity::span(self.starved),
                aterm_capacity::span(self.worked)
            )
        })
    }

    /// Look for its tree's marked waits now. A look that finds none once the run is ending
    /// ([`ending`]) keeps what the last look found: the interrupt marks the run ending before its
    /// `SIGTERM` reaches the child, and a waiter that ends its wait on that signal lets its mark
    /// go while it still lives to say so, so an empty look after the end may be the end's doing.
    fn look(&mut self) {
        if let Some(dir) = self.marks {
            let waits = aterm_toolchain::waits::waiting(dir);
            self.looked = Some(Instant::now());
            if waits.is_empty() && ending() {
                return;
            }
            if !waits.is_empty() {
                ends::saw();
            }
            self.waits_before = std::mem::replace(&mut self.waits, waits);
        }
    }

    /// Why the child must end now, when the last look found its tree waiting behind holders its
    /// waiter found stuck ([`aterm_toolchain::waits::Stuck`]), none of them in the child's own
    /// process group `pgid` (the group its end signals, so a holder is never signalled), on two
    /// looks in a row at the same mark and holder set: what it waited for, the holder named,
    /// and how long that has made no progress. Another mark or a replacement holder lends no
    /// confirmation, and changing diagnostic text or elapsed span does not discard it.
    fn stuck(&self, pgid: u32) -> Option<String> {
        let (w, stuck) = self.waits.iter().find_map(|w| {
            let stuck = w.stuck.as_ref()?;
            let holders: BTreeSet<_> = stuck.holders.iter().copied().collect();
            if holders.is_empty()
                || !self.waits_before.iter().any(|before| {
                    w.same_mark(before)
                        && before.stuck.as_ref().is_some_and(|stuck| {
                            stuck.holders.iter().copied().collect::<BTreeSet<_>>() == holders
                        })
                })
            {
                return None;
            }
            (!stuck.holders.iter().any(|&h| in_group(h, pgid))).then_some((w, stuck))
        })?;
        let now = aterm_time::unix_secs().unwrap_or(0);
        Some(format!(
            "this child waited for admission or a lock behind a holder that has made no progress \
             for {}, so it was ended and decided nothing (the holder was left alone): {} (waiting \
             {})",
            aterm_capacity::span(stuck.span),
            w.text,
            aterm_capacity::span(Duration::from_secs(now.saturating_sub(w.since)))
        ))
    }

    /// Why a child the run's end ended decided nothing: what it waited for, holder named, when
    /// the last look found it waiting ([`Clock::look`]).
    fn ended(&self) -> String {
        if let Some(words) = halted() {
            return words.to_owned();
        }
        if self.waits.is_empty() {
            return "the run was interrupted while this child ran".to_string();
        }
        let now = aterm_time::unix_secs().unwrap_or(0);
        let waits: Vec<String> = self
            .waits
            .iter()
            .map(|w| {
                format!(
                    "{} (waiting {})",
                    w.text,
                    aterm_capacity::span(Duration::from_secs(now.saturating_sub(w.since)))
                )
            })
            .collect();
        format!(
            "the run was interrupted while this child waited for admission or a lock, so it \
             decided nothing: {}",
            waits.join("; ")
        )
    }
}

impl Drop for Clock<'_> {
    /// The child's tree is reaped or given up: what it held is its stage's no longer.
    fn drop(&mut self) {
        if let Some((meter, pgid)) = &self.meter {
            meter.ended(*pgid);
        }
    }
}

/// Whether `pid` is in process group `pgid`: a holder there would get the child's end.
fn in_group(pid: u32, pgid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        // SAFETY: `getpgid` takes an integer and reads the process table; it signals nothing.
        let group = unsafe { libc::getpgid(pid) };
        group > 0 && u32::try_from(group).is_ok_and(|g| g == pgid)
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, pgid);
        false
    }
}

/// How long a child its stuck holder ends ([`Clock::stuck`]) has after its `SIGTERM` to say so
/// before its group gets `SIGKILL`, as the interrupt's handler gives a stage group.
#[cfg(not(test))]
const STUCK_GRACE: Duration = Duration::from_secs(2);
/// A unit test's child is a stand-in that must say why it ends once its `SIGTERM` lands, so it
/// has the hang detector to say it.
#[cfg(test)]
const STUCK_GRACE: Duration = aterm_spec::wait::MUST_HAPPEN;

/// The words of the halt that ended the run at its first proven red (`group::halt`), which
/// every child it ended or kept from starting says in place of an interrupt's; none while no halt
/// has, and never off Unix, where no group is ended.
pub(crate) fn halted() -> Option<&'static str> {
    #[cfg(unix)]
    let halted = group::halted();
    #[cfg(not(unix))]
    let halted = None;
    halted
}

/// Whether the run must end now: an interrupt is ending it.
pub(crate) fn ending() -> bool {
    #[cfg(unix)]
    let stopping = group::stopping();
    #[cfg(not(unix))]
    let stopping = false;
    stopping || ends::now()
}

/// A test's stand-in for the run's interrupt, on its own thread only: the process-wide one would
/// end every test's children. It marks the run ending, as the handler does first; the test then
/// sends its child the handler's `SIGTERM` itself.
#[cfg(test)]
pub(crate) mod ends {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// The end's flag, and the flag set once a look of this thread's clock finds a wait.
    type Flags = (Arc<AtomicBool>, Arc<AtomicBool>);

    thread_local! {
        static END: std::cell::RefCell<Option<Flags>> = const { std::cell::RefCell::new(None) };
    }

    /// End this thread's run once `flag` is set, setting `seen` once its clock finds a wait.
    pub(crate) fn when(flag: Arc<AtomicBool>, seen: Arc<AtomicBool>) {
        END.with(|e| *e.borrow_mut() = Some((flag, seen)));
    }

    pub(super) fn now() -> bool {
        END.with(|e| {
            e.borrow()
                .as_ref()
                .is_some_and(|(f, _)| f.load(Ordering::SeqCst))
        })
    }

    /// A look of this thread's clock found a wait.
    pub(super) fn saw() {
        END.with(|e| {
            if let Some((_, seen)) = e.borrow().as_ref() {
                seen.store(true, Ordering::SeqCst);
            }
        });
    }
}

#[cfg(not(test))]
mod ends {
    pub(super) const fn now() -> bool {
        false
    }

    pub(super) const fn saw() {}
}

fn spawn_command(cmd: &Cmd, env: ExecEnv<'_>, taskpolicy: &Path) -> Command {
    let native = |program: &OsStr| {
        #[cfg(unix)]
        {
            aterm_uds::spawnfd::transparent(program).into_command()
        }
        #[cfg(not(unix))]
        {
            aterm_uds::spawnfd::command(program)
        }
    };
    let mut c = if cmd.demoted
        && at_desk(env.host)
        && crate::is_executable_file(taskpolicy)
        && resolves(cmd, env)
    {
        let mut c = native(taskpolicy.as_os_str());
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
        native(cmd.program.as_os_str())
    };
    c.args(&cmd.args).stdin(Stdio::null());
    #[cfg(unix)]
    {
        c.process_group(0);
    }
    c
}

const ENV: &str = aterm_uds::os_path!("env");

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

/// Spawn `c` and reap it, ending it at `env.child_ceiling` of work ([`Clock`]: its tree's waits
/// marked in `marks` and its waits for a core read of `env.host` left out). Once the run is ending
/// ([`ending`]) the interrupt's handler ends it, and its row says what it waited for, if it
/// waited. A child whose tree waits behind a stuck holder ([`Clock::stuck`]) is ended
/// (`SIGTERM`, [`STUCK_GRACE`], then `SIGKILL`) and could not run, naming the holder. A child
/// that ends after waiting longer than [`STARVED_SAID`] for a core says so on stderr.
fn finish(
    mut c: Command,
    cmd: &Cmd,
    env: ExecEnv<'_>,
    log: Option<&Path>,
    label: &str,
    marks: Option<&Path>,
) -> (Run, Option<String>) {
    let (ceiling, stage_groups) = (env.child_ceiling, env.stage_groups);
    let read_log = |log: Option<&Path>| {
        log.map(|p| {
            String::from_utf8_lossy(&crate::read_captured(p).unwrap_or_default()).into_owned()
        })
    };
    let own = Own::default();
    #[cfg(unix)]
    let sweep = |pgid: u32| {
        // The unreaped leader still has its final counters, booked into the stage's meter and
        // the child's own alike. Descendants which exited between earlier samples remain absent
        // from this lower bound.
        if let Some(members) = env.host.group_usage(pgid) {
            if let Some(meter) = crate::memory::current() {
                meter.book(pgid, &members);
            }
            own.book(pgid, &members);
        }
        if let Some(groups) = stage_groups {
            groups.group_ended(pgid, label);
        }
    };
    #[cfg(unix)]
    let record = |pgid: u32| {
        if let Some(groups) = stage_groups {
            groups.group_reaped(pgid, label);
        }
    };
    #[cfg(not(unix))]
    let _ = (stage_groups, label);
    #[cfg(unix)]
    if group::stopping() {
        return (
            failed_to_wait(halted().unwrap_or("the run was interrupted before this child started")),
            read_log(log),
        );
    }
    #[cfg(unix)]
    let spawning = group::Spawning::begin();
    #[cfg(unix)]
    if group::stopping() {
        return (
            failed_to_wait(halted().unwrap_or("the run was interrupted before this child started")),
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
    if let Some(groups) = stage_groups {
        groups.group_made(child.id(), label);
    }
    #[cfg(unix)]
    drop(spawning);

    let mut cases = cmd.test_cases.as_ref().and_then(|watch| {
        log.and_then(|path| test_progress::LiveCases::open(path, watch.json).ok())
    });
    let begun = Instant::now();
    let mut clock = Clock::new(marks, env.host, child.id(), crate::memory::current()).owning(&own);
    let used = |run: Run, clock: &Clock<'_>| Run {
        usage: own.usage(clock, begun.elapsed()),
        ..run
    };
    let mut told = false;
    let mut nap = Duration::from_millis(1);
    loop {
        if let (Some(watch), Some(cases)) = (&cmd.test_cases, &mut cases) {
            for seen in cases.poll() {
                use test_progress::Seen;
                let halts = watch.stop_at_red && matches!(seen, Seen::Red { .. }) && !ending();
                // A spared case's own block says its machine, not the tree, refused it: it reads
                // NOT RUN, as its binary's row will; a red or an undecided case reads FAIL.
                let outcome = if matches!(seen, Seen::Spared { .. }) {
                    "NOT RUN"
                } else {
                    "FAIL"
                };
                let (case, says) = match &seen {
                    Seen::Red { case, marks } | Seen::Spared { case, marks } => {
                        (Some(case.as_str()), block_says(*marks))
                    }
                    Seen::Undecided { case } => (case.as_deref(), Some("unknown")),
                };
                eprintln!(
                    "verify-event: {}",
                    aterm_json::json!({
                        "schema": 1, "event": "test-case-failed", "outcome": outcome,
                        "test": watch.spec, "case": case,
                        "at_unix_ms": aterm_time::unix_millis(),
                        "elapsed_s": begun.elapsed().as_secs_f64(),
                        "binary_completed": false,
                        "halts": halts, "block_says": says,
                        "detail": match seen {
                            Seen::Red { .. } => "its block decides its binary a FAIL",
                            Seen::Spared { .. } => "its block says its machine, not the tree",
                            Seen::Undecided { .. } => "its block is decided at its binary's end",
                        },
                    })
                );
                if let (true, Seen::Red { case, .. }) = (halts, seen) {
                    // The red is ended here, inside the child's own run and before anything
                    // calls the run's halt (`Halt::red`, which `testrun` calls once this returns):
                    // the halt makes `ending` true, and a child reaped while the run is ending is
                    // released as interrupted (`Live::release_for_reap`), which would turn this
                    // red into a NOT RUN.
                    let stopped = crate::libtest_json::Stopped {
                        spec: watch.spec.clone(),
                        case,
                    };
                    let killed = kill_and_reap(
                        &mut child,
                        #[cfg(unix)]
                        &mut live,
                        Some(STUCK_GRACE),
                        #[cfg(unix)]
                        REAP_AFTER_KILL,
                    );
                    return (
                        used(
                            Run {
                                ok: false,
                                output: killed,
                                code: None,
                                spawn_error: None,
                                stopped_at: Some(stopped),
                                usage: None,
                            },
                            &clock,
                        ),
                        read_log(log),
                    );
                }
            }
        }
        #[cfg(unix)]
        let status = live.reap_stage_sweeping(&mut child, false, sweep, record);
        #[cfg(not(unix))]
        let status = child.try_wait();
        match status {
            Ok(Some(st)) => {
                if let Some(line) = clock.starved_line() {
                    eprintln!("verify: {label}: {line}");
                }
                return (used(reaped(Ok(st)), &clock), read_log(log));
            }
            Ok(None) => {}
            Err(_) if ending() => {
                clock.look();
                return (used(failed_to_wait(&clock.ended()), &clock), read_log(log));
            }
            Err(e) => {
                return (used(failed_to_wait(&e.to_string()), &clock), read_log(log));
            }
        }
        clock.tick();
        if ending() {
            // What its tree waits for as the run ends, said at once, ahead of the row the run's
            // report gives it once every stage group is ended. The handler ends the child too (its
            // one `SIGTERM`, up to 2 s, then `SIGKILL`), so it is not signalled here: a lab ends
            // its wait on that `SIGTERM` and says NOT RUN, naming the holder, and a second signal
            // would end it at once, unsaid.
            if !told {
                told = true;
                clock.look();
                if !clock.waits.is_empty() {
                    eprintln!("verify: {label}: {}", clock.ended());
                }
            }
        } else if let Some(why) = clock.stuck(child.id()) {
            eprintln!("verify: {label}: {why}");
            let killed = kill_and_reap(
                &mut child,
                #[cfg(unix)]
                &mut live,
                Some(cmd.term_grace.unwrap_or(STUCK_GRACE)),
                #[cfg(unix)]
                REAP_AFTER_KILL,
            );
            return (
                used(
                    Run {
                        stopped_at: None,
                        ok: false,
                        output: killed,
                        code: None,
                        spawn_error: Some(why),
                        usage: None,
                    },
                    &clock,
                ),
                read_log(log),
            );
        } else if let Some(limit) = ceiling
            && clock.over(limit)
        {
            let killed = kill_and_reap(
                &mut child,
                #[cfg(unix)]
                &mut live,
                cmd.term_grace,
                #[cfg(unix)]
                REAP_AFTER_KILL,
            );
            return (
                used(over_ceiling(cmd, &clock, limit, &killed), &clock),
                read_log(log),
            );
        }
        std::thread::sleep(nap);
        nap = (nap * 2).min(Duration::from_millis(25));
    }
}

/// What a failed case's block says of its machine, for its `test-case-failed` event: `null` for a
/// block that says neither (a plain failure).
fn block_says(marks: crate::libtest::Marks) -> Option<&'static str> {
    match (marks.refused, marks.disk) {
        (true, true) => Some("both"),
        (true, false) => Some("refusal"),
        (false, true) => Some("disk"),
        (false, false) => None,
    }
}

/// End `child`'s group: with a `grace`, `SIGTERM` first and up to `grace` for its leader to exit;
/// then `SIGKILL` and the leader's reap, which, for a leader still there after the grace, gives
/// up on Unix at `reap_bound` ([`reap_killed`]). What it adds to the child's block: nothing, the
/// grace its leader exited within, what failed, or a leader left as it is.
fn kill_and_reap(
    child: &mut Child,
    #[cfg(unix)] live: &mut group::Live,
    grace: Option<Duration>,
    #[cfg(unix)] reap_bound: Duration,
) -> String {
    #[cfg(unix)]
    if let Some(grace) = grace
        && live.term().is_ok()
    {
        let now = Instant::now();
        let deadline = now
            .checked_add(grace)
            .unwrap_or_else(|| now + Duration::from_secs(365 * 24 * 3600));
        while Instant::now() < deadline {
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
    let note = match reap_killed(child, live, reap_bound) {
        Some(left) => format!("{note}  ({left})\n"),
        None => note,
    };
    #[cfg(not(unix))]
    let _ = child.wait();
    note
}

/// How long a child sent `SIGKILL` may take to end before verify gives up its reap: whether a
/// program held at its first exec dies of it is unmeasured (leftover U11), and a wait on one that
/// does not would hang the run. The one-minute hang detector.
#[cfg(unix)]
const REAP_AFTER_KILL: Duration = Duration::from_secs(60);

/// Reap `child`, just sent `SIGKILL`, looking every 25 ms for at most `bound`: `None` once it is
/// reaped or its custody reads lost, else the clause that says it did not end and was left as it
/// is, never signalled again here (its handle, once dropped, hands it to `aterm_uds`' reaper).
#[cfg(unix)]
fn reap_killed(child: &mut Child, live: &mut group::Live, bound: Duration) -> Option<String> {
    let begun = Instant::now();
    loop {
        match live.exited(child, false) {
            Ok(true) => {
                let _ = live.reap_stage(child, true);
                return None;
            }
            Ok(false) if begun.elapsed() < bound => std::thread::sleep(Duration::from_millis(25)),
            Ok(false) => {
                return Some(format!(
                    "it did not end within {} of SIGKILL: pid {}, left as it is",
                    aterm_capacity::span(bound),
                    child.id()
                ));
            }
            Err(_) => return None,
        }
    }
}

fn over_ceiling(cmd: &Cmd, clock: &Clock<'_>, limit: Duration, kill: &str) -> Run {
    let argv = cmd.argv().join(" ");
    let waited = waited_clause(clock.waited, clock.starved);
    Run {
        stopped_at: None,
        ok: false,
        output: format!(
            "{} — child killed after {:.1}s of work, over the {:.1}s ceiling \
             (--timeout <seconds|off>){waited}\n\x20 child: {argv}\n{kill}",
            crate::findings::CEILING_KILL,
            clock.worked.as_secs_f64(),
            limit.as_secs_f64(),
        ),
        code: None,
        spawn_error: None,
        usage: None,
    }
}

/// What a ceiling kill's line adds about the time the ceiling left out: `lock` spent in marked
/// waits for admission or a lock, `core` waiting for a core; each named only when it shows as more
/// than `0.0s`.
fn waited_clause(lock: Duration, core: Duration) -> String {
    let shows = |d: Duration| d >= Duration::from_millis(50);
    let secs = |d: Duration| d.as_secs_f64();
    match (shows(lock), shows(core)) {
        (false, false) => String::new(),
        (true, false) => format!(
            "; it also waited {:.1}s for admission or a lock, which the ceiling leaves out",
            secs(lock)
        ),
        (false, true) => format!(
            "; it also waited {:.1}s for a core, which the ceiling leaves out",
            secs(core)
        ),
        (true, true) => format!(
            "; it also waited {:.1}s for admission or a lock and {:.1}s for a core, which the \
             ceiling leaves out",
            secs(lock),
            secs(core)
        ),
    }
}

/// Process-group custody and interrupt handling for stage children.
#[cfg(unix)]
pub mod group {
    /// The actual owned child implementations used by stage execution and GUI smoke.
    /// Custody checks the owned child's pid before observation or reap.
    pub(crate) trait WaitChild {
        fn id(&self) -> u32;
        fn wait(&mut self) -> std::io::Result<std::process::ExitStatus>;
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>>;
    }

    impl WaitChild for std::process::Child {
        fn id(&self) -> u32 {
            Self::id(self)
        }
        fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
            Self::wait(self)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
            Self::try_wait(self)
        }
    }

    impl WaitChild for aterm_uds::spawnfd::Child {
        fn id(&self) -> u32 {
            Self::id(self)
        }
        fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
            Self::wait(self)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
            Self::try_wait(self)
        }
    }

    use std::ffi::c_long;
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    use libc::{P_PID, WNOHANG, WNOWAIT, siginfo_t, waitid};
    use libc::{killpg, nanosleep, signal};

    const SIGHUP: i32 = 1;
    const SIGINT: i32 = 2;
    const SIGKILL: i32 = 9;
    const SIGTERM: i32 = 15;
    const SIG_DFL: libc::sighandler_t = libc::SIG_DFL;

    const GRACE_ROUNDS: u32 = 200;
    const NAP_MS: c_long = 10;

    static LIVE: [AtomicI32; 256] = [const { AtomicI32::new(0) }; 256];

    static STOPPING: AtomicBool = AtomicBool::new(false);
    /// Set when a wait for capacity took the interrupt and [`stop_run`] ends the run.
    static WAIT_STOPPED: AtomicBool = AtomicBool::new(false);
    /// Set when the run's first proven red ends it ([`halt`]) before any interrupt did: an
    /// interrupt after it is the run's first, which it reports before it dies of.
    static HALTING: AtomicBool = AtomicBool::new(false);
    static SPAWNING: AtomicUsize = AtomicUsize::new(0);
    /// The interrupt the run dies of once it has reported ([`end_interrupted`]), set by the
    /// handler once every stage group is ended; 0 while there is none.
    static PENDING: AtomicI32 = AtomicI32::new(0);
    /// Set as the handler begins ending the stage groups, before [`PENDING`] is.
    static HANDLING: AtomicBool = AtomicBool::new(false);
    /// The write end of the pipe [`kill_on_interrupt`]'s watchdog reads ([`REPORT_GRACE`]); -1
    /// where it has none, and the handler then re-raises its signal at once.
    static WATCHDOG: AtomicI32 = AtomicI32::new(-1);

    /// How long an interrupted run has, once every stage group is ended, to write its log, its
    /// verdict and its `log:` line and to remove its scratch before its watchdog re-raises the
    /// signal regardless ([`end_interrupted`]).
    pub const REPORT_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

    /// The grace a test's stand-in run set for its watchdog, in milliseconds (0: none set).
    #[cfg(test)]
    static REPORT_GRACE_FOR_TEST_MS: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);

    /// Hold this process's interrupted run to `grace` instead of [`REPORT_GRACE`]: a stand-in run
    /// whose report must happen sets the hang detector, so a loaded host's slow report is not cut
    /// short; one whose grace must fire keeps [`REPORT_GRACE`].
    #[cfg(test)]
    pub(crate) fn set_report_grace_for_test(grace: std::time::Duration) {
        let ms = u64::try_from(grace.as_millis()).unwrap_or(u64::MAX).max(1);
        REPORT_GRACE_FOR_TEST_MS.store(ms, Ordering::SeqCst);
    }

    /// [`REPORT_GRACE`], or the grace a test set with `set_report_grace_for_test`.
    fn report_grace() -> std::time::Duration {
        #[cfg(test)]
        match REPORT_GRACE_FOR_TEST_MS.load(Ordering::SeqCst) {
            0 => {}
            ms => return std::time::Duration::from_millis(ms),
        }
        REPORT_GRACE
    }

    /// Counts the spawn-to-registration window.
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
        pub(crate) fn holds(&self, pid: u32) -> bool {
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
            // `super::ending`, not `stopping`: a test's per-thread end reads as the run's.
            if super::ending() {
                let _ = self.kill();
                return Err(std::io::Error::other(
                    halted().unwrap_or("the run was interrupted while this child ran"),
                ));
            }
            self.group = None;
            Ok(())
        }
        pub(crate) fn exited(
            &mut self,
            child: &mut impl WaitChild,
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
            child: &mut impl WaitChild,
            block: bool,
        ) -> std::io::Result<Option<std::process::ExitStatus>> {
            self.reap_stage_sweeping(child, block, |_| {}, |_| {})
        }

        /// Reap `child`, running `sweep` once after the leader exits and before it is reaped, and
        /// `reaped` with the same group once it is (macOS and Linux only).
        pub(crate) fn reap_stage_sweeping(
            &mut self,
            child: &mut impl WaitChild,
            block: bool,
            sweep: impl FnOnce(u32),
            reaped: impl FnOnce(u32),
        ) -> std::io::Result<Option<std::process::ExitStatus>> {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            {
                if !self.exited(child, block)? {
                    return Ok(None);
                }
                let swept = self.group;
                if let Some(pgid) = swept {
                    sweep(pgid);
                }
                self.release_for_reap(child.id())?;
                let status = child.wait()?;
                if let Some(pgid) = swept {
                    reaped(pgid);
                }
                Ok(Some(status))
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            {
                let _ = (sweep, reaped);
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

    /// Scoped only to the exact child owned by one custody fixture.
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

    /// Whether process group `pgid` has no process left in it, by the kernel's answer about the
    /// whole group (`killpg(pgid, 0)`, which sends nothing): `Some(true)` when the group is gone
    /// (`ESRCH`); `Some(false)` when a process is in it, whether or not this user may signal it
    /// (`0`, or `EPERM`: another user's, and on macOS a zombie its parent has not reaped, which
    /// Linux answers `0` for when it is this user's); `None` for a `pgid` of 1 or less
    /// (`killpg` reads 0 as this process's own group and 1 as every process this user may
    /// signal) or past `i32::MAX`, unasked, and for any other answer. macOS also answers `EPERM`
    /// for a group that holds no process, only the old image a member's exec left, which the
    /// kernel tears down after the exec, on a loaded machine after the member itself is reaped;
    /// there an `EPERM` is settled by the group's listing ([`judged`]). Until its leader is
    /// reaped the leader keeps the group, so it is asked after the reap; a group made since
    /// under the freed id then reads as not empty, never as empty.
    #[must_use]
    pub fn empty(pgid: u32) -> Option<bool> {
        let g = i32::try_from(pgid).ok().filter(|&g| g > 1)?;
        // SAFETY: two integers; signal 0 sends nothing, and `g` > 1 names one group.
        let answer = if unsafe { killpg(g, 0) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error().raw_os_error())
        };
        judged(answer, || aterm_uds::process::group_members(pgid))
    }

    /// [`empty`]'s verdict on `killpg`'s `answer` (`Ok` for 0, else its errno). An `EPERM` on
    /// Apple reads `listing`, the group's members as `proc_listpids` finds them: it lists every
    /// live and zombie process of every user and skips only what an exec left of an old image
    /// (measured on macOS 27: a reaped `/bin/sh` leader, whose shim execs `bash`, answered
    /// `EPERM` and listed nothing for up to 1.6 s on a loaded machine, then `ESRCH`), so a whole
    /// listing that finds no process proves the group empty, as the sweep's whole listing does
    /// ([`crate::runs::sweep_group`]); a listing cut short, or none, leaves it not empty. A
    /// process still being created in the group is in neither answer, as in no listing.
    #[must_use]
    pub(crate) fn judged(
        answer: Result<(), Option<i32>>,
        listing: impl FnOnce() -> Option<aterm_uds::process::GroupMembers>,
    ) -> Option<bool> {
        match answer {
            Ok(()) => Some(false),
            Err(Some(libc::ESRCH)) => Some(true),
            Err(Some(libc::EPERM)) => Some(
                cfg!(target_vendor = "apple")
                    && listing().is_some_and(|members| members.complete && members.pids.is_empty()),
            ),
            Err(_) => None,
        }
    }

    fn signal_group(pgid: u32, sig: i32) -> std::io::Result<()> {
        #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
        custody_audit::before_signal(pgid, sig)?;
        #[cfg(test)]
        if sig == SIGKILL && spared::armed() {
            return Ok(());
        }
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
            // SAFETY: every field of `siginfo_t` is an integer or a pointer, so all-zero is a valid
            // value; the kernel overwrites it.
            let mut info: siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: `info` is live and writable, and `pid` names the child whose exit is observed
            // without reaping it (`WNOWAIT`).
            let got = unsafe { waitid(P_PID, pid, &raw mut info, options) };
            if got == 0 {
                #[cfg(target_os = "macos")]
                let ended = info.si_signo != 0 && (1..=3).contains(&info.si_code);
                #[cfg(target_os = "linux")]
                let ended = info.si_signo != 0;
                if ended || !block {
                    return Ok(ended);
                }
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

    extern "C" fn on_signal(sig: i32) {
        if STOPPING.swap(true, Ordering::SeqCst) {
            if WAIT_STOPPED.load(Ordering::SeqCst) || PENDING.load(Ordering::SeqCst) != 0 {
                // The run is ending on a wait's interrupt ([`stop_run`]), or reporting after one
                // ([`end_interrupted`]); another ends it now.
                // SAFETY: both take integers only.
                unsafe {
                    signal(sig, SIG_DFL);
                    reraise(sig);
                }
            }
            if HALTING.load(Ordering::SeqCst) {
                // A halt is ending the run and has ended its groups ([`halt`]): this is the run's
                // first interrupt, so it reports, then dies of it.
                die_after_report(sig);
            }
            return;
        }
        HANDLING.store(true, Ordering::SeqCst);
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
            // SAFETY: `nanosleep` reads the struct we own and writes nothing when `rem` is null.
            unsafe {
                nanosleep(&raw const nap, std::ptr::null_mut());
            }
        }
        sweep(SIGKILL);
        die_after_report(sig);
    }

    /// The run reports what it has, then dies of `sig` ([`end_interrupted`]); its watchdog
    /// re-raises it past [`REPORT_GRACE`] if it has not. Without a watchdog, at once. Called from
    /// the handler: async-signal-safe.
    fn die_after_report(sig: i32) {
        PENDING.store(sig, Ordering::SeqCst);
        let fd = WATCHDOG.load(Ordering::SeqCst);
        let byte = u8::try_from(sig).unwrap_or(u8::MAX);
        // SAFETY: `write` is async-signal-safe; `byte` lives across the call and `fd` is the
        // watchdog pipe's write end, which nothing closes.
        if fd >= 0 && unsafe { libc::write(fd, (&raw const byte).cast(), 1) } == 1 {
            return;
        }
        // SAFETY: both take integers only.
        unsafe {
            signal(sig, SIG_DFL);
            reraise(sig);
        }
    }

    /// End an interrupted run once it has reported: die of the interrupt the handler ended its
    /// stage groups for, as the run would have without reporting, once the handler has ended
    /// them (a stage's group leaves the live list before the handler's next look sees it gone).
    /// A run no interrupt reached, or one a wait's interrupt ended ([`stop_run`], which exits
    /// with its verdict), returns.
    pub fn end_interrupted() {
        while HANDLING.load(Ordering::SeqCst) && PENDING.load(Ordering::SeqCst) == 0 {
            std::thread::sleep(std::time::Duration::from_millis(NAP_MS.unsigned_abs()));
        }
        let sig = PENDING.load(Ordering::SeqCst);
        if sig != 0 {
            // SAFETY: both take integers only.
            unsafe {
                signal(sig, SIG_DFL);
                reraise(sig);
            }
        }
    }

    /// The watchdog an interrupted run's report is held to: woken by the handler through its
    /// pipe ([`WATCHDOG`]), it re-raises the handler's signal [`REPORT_GRACE`] later unless the
    /// run has died of it by then ([`end_interrupted`]). Set once; `false` where its pipe or its
    /// thread could not be made.
    fn watchdog() -> bool {
        use std::io::Read as _;
        use std::os::fd::IntoRawFd as _;
        if WATCHDOG.load(Ordering::SeqCst) >= 0 {
            return true;
        }
        let Ok((mut read, write)) = std::io::pipe() else {
            return false;
        };
        let spawned = std::thread::Builder::new()
            .name("verify-interrupt-watchdog".into())
            .spawn(move || {
                let mut sig = [0u8];
                if read.read_exact(&mut sig).is_ok() {
                    std::thread::sleep(report_grace());
                    let sig = PENDING.load(Ordering::SeqCst);
                    // SAFETY: both take integers only.
                    unsafe {
                        signal(sig, SIG_DFL);
                        reraise(sig);
                    }
                }
            });
        if spawned.is_err() {
            return false;
        }
        WATCHDOG.store(write.into_raw_fd(), Ordering::SeqCst);
        true
    }

    /// Re-raise `sig` at its default action.
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

    /// The words of the halt that ended the run ([`halt`]); none while no halt has.
    static HALTED: std::sync::OnceLock<String> = std::sync::OnceLock::new();

    /// End the run at its first proven red (`crate::sched::Halt`, design §4.3 A3), as [`stop_run`]
    /// does: no stage child starts after this, and every live one's group is ended (SIGTERM, a
    /// grace, then SIGKILL). Each child it ends or keeps from starting says `words` ([`halted`])
    /// and reads as stopped, not as a finding, so its row is NOT RUN; a run an interrupt already
    /// ends keeps the interrupt's words. An interrupt after it is the run's first: the run reports,
    /// then dies of it. Returns once every live group has left or the grace is spent.
    pub fn halt(words: &str) {
        if !stopping() {
            let _ = HALTED.set(words.to_owned());
            HALTING.store(true, Ordering::SeqCst);
        }
        end_groups();
    }

    /// The words of the halt that ended the run ([`halt`]), if one did.
    #[must_use]
    pub fn halted() -> Option<&'static str> {
        HALTED.get().map(String::as_str)
    }

    /// End the run as [`kill_on_interrupt`]'s handler would, but without dying of it: a wait for
    /// capacity took the interrupt (`aterm_toolchain::interrupt`) and named what it waited for.
    /// No stage child starts after this, every live one's group is ended (SIGTERM, a grace, then
    /// SIGKILL) and reads as interrupted, not as a finding (`Live::release_for_reap`), no stage
    /// starts ([`stopping`]), and the run goes on to report what it has; a further interrupt
    /// ends it at once.
    pub fn stop_run() {
        WAIT_STOPPED.store(true, Ordering::SeqCst);
        end_groups();
    }

    /// Mark the run ending ([`stopping`]) and end every live stage group: SIGTERM, a grace, then
    /// SIGKILL. Returns at once when the run was already ending.
    fn end_groups() {
        if STOPPING.swap(true, Ordering::SeqCst) {
            return;
        }
        sweep(SIGTERM);
        for _ in 0..GRACE_ROUNDS {
            if LIVE.iter().all(|slot| slot.load(Ordering::SeqCst) == 0)
                && SPAWNING.load(Ordering::SeqCst) == 0
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(NAP_MS.unsigned_abs()));
        }
        sweep(SIGKILL);
    }

    /// On `SIGINT`, `SIGTERM` or `SIGHUP`, end every live stage child's group; the run then
    /// reports what it has and dies of the signal ([`end_interrupted`]), held to
    /// [`REPORT_GRACE`] by a watchdog.
    pub fn kill_on_interrupt() {
        if !cfg!(any(target_os = "macos", target_os = "linux")) {
            return;
        }
        watchdog();
        for sig in [SIGHUP, SIGINT, SIGTERM] {
            // SAFETY: `on_signal` is an `extern "C" fn(i32)` that lives for the whole program and
            // does only async-signal-safe work.
            unsafe {
                signal(sig, on_signal as extern "C" fn(i32) as usize);
            }
        }
    }

    /// The interrupt the run will die of once it has reported; 0 while there is none.
    #[cfg(test)]
    pub(crate) fn pending() -> i32 {
        PENDING.load(Ordering::SeqCst)
    }

    /// How many stage groups are listed now.
    #[cfg(test)]
    pub(crate) fn live_count() -> usize {
        LIVE.iter()
            .filter(|slot| slot.load(Ordering::SeqCst) != 0)
            .count()
    }

    /// A unit test's stand-in for a leader `SIGKILL` does not end, as a program macOS holds at
    /// its first exec may not (leftover U11, unmeasured): while a `Scope` is armed on a thread, a
    /// `SIGKILL` that thread sends a group is sent nowhere and answers `Ok`, so the group's leader
    /// runs on.
    #[cfg(test)]
    pub(super) mod spared {
        use std::cell::Cell;
        use std::marker::PhantomData;
        use std::rc::Rc;

        thread_local! { static ARMED: Cell<bool> = const { Cell::new(false) }; }

        pub(crate) struct Scope {
            previous: bool,
            _same_thread: PhantomData<Rc<()>>,
        }
        impl Scope {
            pub(crate) fn arm() -> Self {
                Self {
                    previous: ARMED.with(|armed| armed.replace(true)),
                    _same_thread: PhantomData,
                }
            }
        }
        impl Drop for Scope {
            fn drop(&mut self) {
                ARMED.with(|armed| armed.set(self.previous));
            }
        }
        pub(super) fn armed() -> bool {
            ARMED.with(Cell::get)
        }
    }
}

fn reaped(st: io::Result<std::process::ExitStatus>) -> Run {
    match st {
        Ok(st) => Run {
            stopped_at: None,
            ok: st.success(),
            output: ended_by_signal(st),
            code: st.code(),
            spawn_error: None,
            usage: None,
        },
        Err(e) => failed_to_wait(&e.to_string()),
    }
}

/// The line a child killed by a signal cannot write for itself, naming the signal; empty for
/// a child that exited.
fn ended_by_signal(st: std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(sig) = st.signal() {
            let core = if st.core_dumped() {
                ", core dumped"
            } else {
                ""
            };
            return format!("aterm-verify: the child was killed by signal {sig}{core}\n");
        }
    }
    let _ = st;
    String::new()
}

fn failed_to_wait(why: &str) -> Run {
    Run {
        stopped_at: None,
        ok: false,
        output: String::new(),
        code: None,
        spawn_error: Some(why.to_string()),
        usage: None,
    }
}

fn spawn_failure(cmd: &Cmd, why: &str) -> Run {
    Run {
        stopped_at: None,
        ok: false,
        output: format!("aterm-verify: cannot run {}: {why}", cmd.program.display()),
        code: None,
        spawn_error: Some(why.to_string()),
        usage: None,
    }
}

/// Run a child and return its merged stdout and stderr with trailing newlines stripped.
#[must_use]
pub fn capture_reply(cmd: &Cmd, env: ExecEnv<'_>) -> String {
    let r = run(&cmd.clone().capture(Capture::Emit), env);
    r.trimmed_output().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A VARIABLE SET TWICE IS SET ONCE, to its last value, where it was first set: what the child
    /// sees, named once. NEGATIVE CONTROL: two variables keep both, in the order they were set.
    #[test]
    fn a_variable_set_twice_is_named_once_with_its_last_value() {
        let cmd = Cmd::new("targo")
            .env("ATERM_SPEC_CHECKERS", "1")
            .env("TRAIL_LAWS_FULL", "1")
            .env("ATERM_SPEC_CHECKERS", "0");
        assert_eq!(
            cmd.envs,
            [
                ("ATERM_SPEC_CHECKERS".into(), "0".into()),
                ("TRAIL_LAWS_FULL".into(), "1".into())
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_native_stage_keeps_its_inherited_pipe_descriptor() {
        use std::io::Write as _;
        use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
        let dir = aterm_tempfile::tempdir().unwrap();
        let (reader, mut writer) = std::io::pipe().unwrap();
        // SAFETY: F_DUPFD creates one owned, non-close-on-exec duplicate without replacing
        // another descriptor. The original remains live throughout the call.
        let fd = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_DUPFD, 100) };
        assert!(fd >= 100);
        // SAFETY: the successful duplicate is newly owned by this test alone.
        let inherited = unsafe { OwnedFd::from_raw_fd(fd) };
        writer.write_all(b"shared-jobserver-handle\n").unwrap();
        drop(writer);
        let result = run(
            &Cmd::new("/bin/cat").arg(format!("/dev/fd/{fd}")),
            env_in(dir.path()),
        );
        assert!(result.ok, "{}", result.output);
        assert_eq!(result.output, "shared-jobserver-handle\n");
        drop(inherited);
    }

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
            stage_groups: None,
            host: &INERT,
        }
    }

    /// A host that reads nothing, or only the input clock it is given: every span with no marked
    /// wait is work, as before the clock read its tree.
    #[derive(Debug)]
    pub(super) struct Idle(pub(super) Option<Duration>);

    impl Host for Idle {
        fn group_usage(&self, _: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
            None
        }

        fn idle(&self) -> Option<Duration> {
            self.0
        }

        fn cores(&self) -> u32 {
            1
        }
    }

    /// No reading of anything.
    pub(super) static INERT: Idle = Idle(None);

    /// A stand-in host for a tree that is runnable all the time and is given `share` per mille of
    /// that time on a core: the child's group as one member, whose counters run with the wall
    /// clock between reads, or, booked in `lumps`, stay flat until that long has passed since they
    /// last grew and then grow by all of it, as macOS books a starved thread's time when it next
    /// gets a core.
    #[cfg(unix)]
    #[derive(Debug)]
    struct Starving {
        share: AtomicU64,
        lumps: Option<Duration>,
        counters: std::sync::Mutex<Option<(Instant, u64, u64)>>,
    }

    #[cfg(unix)]
    impl Starving {
        fn at(per_mille: u64) -> &'static Self {
            Self::booked(per_mille, None)
        }

        fn lumpy(per_mille: u64, every: Duration) -> &'static Self {
            Self::booked(per_mille, Some(every))
        }

        fn booked(per_mille: u64, lumps: Option<Duration>) -> &'static Self {
            Box::leak(Box::new(Self {
                share: AtomicU64::new(per_mille),
                lumps,
                counters: std::sync::Mutex::new(None),
            }))
        }

        fn give(&self, per_mille: u64) {
            self.share.store(per_mille, Ordering::SeqCst);
        }
    }

    #[cfg(unix)]
    impl Host for Starving {
        fn group_usage(&self, pgid: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
            let mut c = self
                .counters
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = Instant::now();
            let (at, cpu, runnable) = c.get_or_insert((now, 0, 0));
            let since = now.saturating_duration_since(*at);
            if self.lumps.is_none_or(|every| since >= every) {
                let span = u64::try_from(since.as_nanos()).unwrap_or(u64::MAX);
                *at = now;
                *runnable += span;
                *cpu += span / 1000 * self.share.load(Ordering::SeqCst);
            }
            Some(vec![aterm_sysprobe::MemberUsage {
                pid: pgid,
                start: 1,
                cpu_ns: *cpu,
                runnable_ns: *runnable,
                io_bytes: 0,
                ..aterm_sysprobe::MemberUsage::default()
            }])
        }

        fn idle(&self) -> Option<Duration> {
            None
        }

        fn cores(&self) -> u32 {
            1
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_starred_removal_takes_every_inherited_variable_it_begins() {
        let tmp = crate::mktemp_dir("atv-env-prefix").expect("mktemp");
        let env_of = |remove: &[&str]| {
            let env = ExecEnv {
                remove_env: remove,
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
        if std::env::var_os("CARGO_PKG_NAME").is_some() {
            let kept = env_of(&["CARGO_PKG"]);
            assert!(
                kept.lines().any(|l| l.starts_with("CARGO_PKG_NAME=")),
                "{kept}"
            );
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The host shell's argv for a script: `unix` under `/bin/sh -c`, or off Unix `windows` under
    /// `cmd.exe /d /s /c`, which strips the quotes std puts round an argument with a space and
    /// leaves a variable it does not have as its literal `%NAME%`.
    fn shell_argv(unix: &str, windows: &str) -> Vec<OsString> {
        if cfg!(unix) {
            vec!["/bin/sh".into(), "-c".into(), unix.into()]
        } else {
            let cmd = std::env::var_os("ComSpec")
                .unwrap_or_else(|| OsString::from(r"C:\Windows\System32\cmd.exe"));
            vec![cmd, "/d".into(), "/s".into(), "/c".into(), windows.into()]
        }
    }

    fn shell(unix: &str, windows: &str) -> Cmd {
        let argv = shell_argv(unix, windows);
        Cmd::new(&argv[0]).args(&argv[1..])
    }

    /// What a child said, its line ends LF as `/bin/sh`'s are (cmd.exe ends each with CRLF).
    fn said(r: &Run) -> String {
        let text = if cfg!(windows) {
            r.output.replace("\r\n", "\n")
        } else {
            r.output.clone()
        };
        text.trim_end_matches('\n').to_owned()
    }

    /// What the host shell's script reads for a variable the child does not have: the script's
    /// `${NAME-unset}`, or cmd.exe's untouched `%NAME%`.
    fn unset(name: &str) -> String {
        if cfg!(unix) {
            "unset".to_owned()
        } else {
            format!("%{name}%")
        }
    }

    /// A variable every test run inherits, which a child that keeps only its record must not.
    const INHERITED: &str = if cfg!(unix) { "HOME" } else { "USERPROFILE" };

    #[test]
    fn a_child_runs_in_the_root_with_the_runs_variables_and_its_own_win() {
        let tmp = crate::mktemp_dir("atv-setenv").expect("mktemp");
        std::fs::write(tmp.join("marker"), b"x").expect("write");
        let added = [(OsString::from("ATV_ADDED_PROBE"), OsString::from("0"))];
        let env = ExecEnv {
            add_env: &added,
            ..env_in(&tmp)
        };
        let ask = shell(
            "cat marker; printf ' %s %s' \"${ATV_ADDED_PROBE-unset}\" \"$PATH\"",
            "type marker& echo  %ATV_ADDED_PROBE% %PATH%",
        );
        assert_eq!(said(&run(&ask, env)), "x 0 /usr/bin:/bin");
        let stage_says = ask.clone().env("ATV_ADDED_PROBE", "1");
        assert_eq!(said(&run(&stage_says, env)), "x 1 /usr/bin:/bin");
        assert_eq!(
            said(&run(&ask, env_in(&tmp))),
            format!("x {} /usr/bin:/bin", unset("ATV_ADDED_PROBE"))
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A child is handed the cover marker of its stage's claim ([`crate::memory::cover`]) over
    /// the run's, from the stage's own thread and from a worker of its pool alike. NEGATIVE
    /// CONTROL: outside a stage that holds a claim, the run's own (`NO_COVER`) reaches it.
    #[test]
    fn a_child_is_handed_its_stages_cover_marker() {
        let tmp = crate::mktemp_dir("atv-cover").expect("mktemp");
        let added = [(
            OsString::from(aterm_capacity::COVER_ENV),
            OsString::from(aterm_capacity::NO_COVER),
        )];
        let env = ExecEnv {
            add_env: &added,
            ..env_in(&tmp)
        };
        let ask = shell(
            "printf %s \"$ATERM_CAPACITY_COVER\"",
            "echo %ATERM_CAPACITY_COVER%",
        );
        assert_eq!(said(&run(&ask, env)), aterm_capacity::NO_COVER);
        {
            let _covering = crate::memory::covering(Some("0000000000001-7-0".into()));
            assert_eq!(said(&run(&ask, env)), "0000000000001-7-0");
            let pooled = crate::testrun::pool(1, &[0], 1, |_| said(&run(&ask, env)));
            assert_eq!(pooled, [Some("0000000000001-7-0".to_string())]);
            let dispatched = crate::testrun::dispatch_each(1, |_| said(&run(&ask, env)));
            assert_eq!(dispatched, [Some("0000000000001-7-0".to_string())]);
        }
        assert_eq!(said(&run(&ask, env)), aterm_capacity::NO_COVER);
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A child replayed from a recorded invocation ([`Cmd::replayed`]) is handed the cover marker
    /// of its stage's claim over the one its recorded environment holds, from a worker of the
    /// test dispatch too. NEGATIVE CONTROL: outside a stage that holds a claim, its recorded one
    /// reaches it.
    #[cfg(unix)]
    #[test]
    fn a_replayed_child_is_handed_its_stages_cover_marker() {
        let tmp = crate::mktemp_dir("atv-replayed-cover").expect("mktemp");
        let env = env_in(&tmp);
        let ask = Cmd::replayed(
            &[
                OsString::from("/bin/sh"),
                OsString::from("-c"),
                OsString::from("printf %s \"$ATERM_CAPACITY_COVER\""),
            ],
            &tmp,
            vec![(
                OsString::from(aterm_capacity::COVER_ENV),
                OsString::from(aterm_capacity::NO_COVER),
            )],
        );
        assert_eq!(said(&run(&ask, env)), aterm_capacity::NO_COVER);
        {
            let _covering = crate::memory::covering(Some("0000000000001-7-0".into()));
            assert_eq!(said(&run(&ask, env)), "0000000000001-7-0");
            let dispatched = crate::testrun::dispatch_each(1, |_| said(&run(&ask, env)));
            assert_eq!(dispatched, [Some("0000000000001-7-0".to_string())]);
        }
        assert_eq!(said(&run(&ask, env)), aterm_capacity::NO_COVER);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[cfg(unix)]
    #[test]
    fn a_child_killed_by_a_signal_says_which_after_what_it_wrote() {
        let tmp = crate::mktemp_dir("atv-signalled").expect("mktemp");
        let run = run(
            &Cmd::new("/bin/sh").args(["-c", "echo started; kill -KILL $$"]),
            env_in(&tmp),
        );
        assert!(!run.ok && run.code.is_none(), "{run:?}");
        assert_eq!(
            run.trimmed_output(),
            "started\naterm-verify: the child was killed by signal 9"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_missing_tool_is_a_failed_run_not_a_panic_and_never_a_pass() {
        let tmp = crate::mktemp_dir("atv-exec3").expect("mktemp");
        let translator = aterm_uds::process::translator();
        let mut mute = Vec::new();
        for (row, cmd) in [
            ("plain", Cmd::new(tmp.join("absent-driver"))),
            ("demoted", Cmd::new(tmp.join("absent-driver")).demoted()),
        ] {
            let r = run(&cmd, env_in(&tmp));
            assert!(!r.ok, "{row}: {r:?}");
            if translator.is_some() && r.spawn_error.is_none() && r.code == Some(127) {
                mute.push(row);
                continue;
            }
            assert!(r.spawn_error.is_some(), "{row}: {r:?}");
        }
        if let Some(translator) = translator
            && !mute.is_empty()
        {
            println!(
                "NOT RUN under {translator}: exec::tests::\
                 a_missing_tool_is_a_failed_run_not_a_panic_and_never_a_pass: the spawn error \
                 ({}): std's answer comes from glibc's posix_spawn, whose child reports a failed \
                 exec through memory it shares with its parent; the translator runs that child in \
                 a copy, where the report is lost, so std answers a child that exited 127 (still a \
                 failed run); a real CPU of this code's arch decides it",
                mute.join(", ")
            );
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn output_is_captured_with_stderr_merged_and_a_nonzero_exit_keeps_its_code() {
        let tmp = crate::mktemp_dir("atv-exec").expect("mktemp");
        let r = run(
            &shell(
                "echo out; echo err >&2; echo out2",
                "echo out& (echo err)1>&2& echo out2",
            ),
            env_in(&tmp),
        );
        assert!(r.ok, "{r:?}");
        assert_eq!(said(&r), "out\nerr\nout2");
        let r = run(
            &shell("echo why >&2; exit 3", "(echo why)1>&2& exit 3"),
            env_in(&tmp),
        );
        assert!(!r.ok);
        assert_eq!(said(&r), "why");
        assert_eq!(r.code, Some(3));
        std::fs::remove_dir_all(&tmp).ok();
    }

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
        assert!(
            std::env::var_os(INHERITED).is_some(),
            "the probe needs {INHERITED} where the replay could inherit it"
        );
        let argv = shell_argv(
            &format!(
                "cat marker; printf ' %s %s %s' \"${{RECORDED-unset}}\" \"${{GATE_ADDED-unset}}\" \
                 \"${{{INHERITED}-unset}}\""
            ),
            &format!("type marker& echo  %RECORDED% %GATE_ADDED% %{INHERITED}%"),
        );
        let cmd = Cmd::replayed(
            &argv,
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
            said(&r),
            format!(
                "     Running tests/x.rs (target/debug/deps/x-1)\nin-pkg yes {} {}",
                unset("GATE_ADDED"),
                unset(INHERITED)
            ),
            "the preamble first, then the child — in its own directory, with only what \
             was recorded"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn the_flag_moves_the_ceiling_and_only_a_typed_word_removes_it() {
        for junk in ["", "  ", "45m", "soon", "-5", "NaN", "1e400", "4e9"] {
            assert_eq!(
                parse_ceiling(junk),
                None,
                "{junk:?} is not a number of seconds, and must not disable the ceiling"
            );
        }
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

    #[cfg(unix)]
    const GONE: Duration = Duration::from_secs(60);

    #[cfg(unix)]
    const HANG_DETECTOR: Duration = Duration::from_secs(60);

    #[cfg(unix)]
    fn gone_within(pid: &str, window: Duration) -> bool {
        let deadline = Instant::now() + window;
        loop {
            let stat = std::process::Command::new(aterm_uds::os_path!("ps"))
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

    #[cfg(unix)]
    fn written_pid(file: &Path) -> Option<String> {
        let t = std::fs::read_to_string(file).ok()?;
        t.ends_with('\n').then(|| t.trim().to_string())
    }

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

    #[cfg(unix)]
    fn with_grandchild(file: &Path) -> Cmd {
        Cmd::new("/bin/sh").args([
            "-c",
            &format!("sleep 600 & echo $! > '{}'; wait", file.display()),
        ])
    }

    #[cfg(unix)]
    #[test]
    fn a_ceiling_kill_takes_the_childs_whole_process_group() {
        let tmp = crate::mktemp_dir("atv-ceil-group").expect("mktemp");
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

    /// A child of `/bin/sleep 600` leading its own group, under a run in `tmp`.
    #[cfg(unix)]
    fn a_sleeping_leader(tmp: &Path) -> Child {
        spawn_command(
            &Cmd::new("/bin/sleep").arg("600"),
            env_in(tmp),
            Path::new(TASKPOLICY),
        )
        .spawn()
        .expect("spawn sleep")
    }

    /// `pid`'s group, `SIGKILL`ed by `/bin/kill` once a test's wait on it ran past its hang
    /// detector, so the thread blocked on its reap returns.
    #[cfg(unix)]
    fn kill_stray_group(pid: u32) {
        let _ = std::process::Command::new("/bin/kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .status();
    }

    /// The reap after `SIGKILL` is bounded: a child still there at the bound is left as it is,
    /// named, and the wait returns; once it is killed, the same reap takes it.
    #[cfg(unix)]
    #[test]
    fn a_reap_after_sigkill_gives_up_at_its_bound_and_leaves_the_child_as_it_is() {
        let tmp = crate::mktemp_dir("atv-bounded-reap").expect("mktemp");
        let mut child = a_sleeping_leader(&tmp);
        let pid = child.id();
        let (tx, rx) = std::sync::mpsc::channel();
        let reaper = std::thread::spawn(move || {
            let mut live = group::Live::enter(child.id());
            let left = reap_killed(&mut child, &mut live, Duration::from_millis(200));
            let _ = tx.send(left);
            let _ = live.kill();
            reap_killed(&mut child, &mut live, HANG_DETECTOR)
        });
        let Ok(left) = rx.recv_timeout(HANG_DETECTOR) else {
            kill_stray_group(pid);
            let _ = reaper.join();
            panic!("the reap waited past its bound");
        };
        let left = left.expect("a child still there at the bound");
        assert!(left.contains(&format!("pid {pid},")), "{left}");
        assert_eq!(
            reaper.join().expect("the reaper"),
            None,
            "the killed child reaped"
        );
        assert!(gone_within(&pid.to_string(), HANG_DETECTOR));
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A ceiling's kill whose `SIGKILL` does not end the leader (a stand-in: the `SIGKILL` a
    /// [`group::spared::Scope`] sends nowhere, as macOS's held first exec may not die of it) gives
    /// up its reap at its bound and returns, its block naming the leader it left as it is; that
    /// leader is still verify's to kill and reap. Negative control: the unbounded reap the kill
    /// had (`reap_stage(child, true)`) never returns, which the hang detector fails.
    #[cfg(unix)]
    #[test]
    fn a_ceiling_kill_whose_sigkill_does_not_end_the_leader_names_it_and_returns() {
        let tmp = crate::mktemp_dir("atv-spared-kill").expect("mktemp");
        let mut child = a_sleeping_leader(&tmp);
        let pid = child.id();
        let (tx, rx) = std::sync::mpsc::channel();
        let killer = std::thread::spawn(move || {
            let mut live = group::Live::enter(child.id());
            let note = {
                let _spared = group::spared::Scope::arm();
                kill_and_reap(&mut child, &mut live, None, Duration::from_secs(1))
            };
            let _ = tx.send(note);
            let _ = live.kill();
            reap_killed(&mut child, &mut live, HANG_DETECTOR)
        });
        let Ok(note) = rx.recv_timeout(HANG_DETECTOR) else {
            kill_stray_group(pid);
            let _ = killer.join();
            panic!("the kill's reap waited past its bound");
        };
        assert_eq!(
            note,
            format!("  (it did not end within 1s of SIGKILL: pid {pid}, left as it is)\n")
        );
        assert_eq!(
            killer.join().expect("the killer"),
            None,
            "the leader, killed, reaped"
        );
        assert!(gone_within(&pid.to_string(), HANG_DETECTOR));
        std::fs::remove_dir_all(&tmp).ok();
    }

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

    /// [`group::empty`] asks the kernel only about one real group: a pgid of 0 (to `killpg`, this
    /// process's own group), 1 (every process this user may signal), or one past `i32::MAX` is
    /// `None`, never asked; a group whose one member is this test's exited, unreaped child (a
    /// leader of its own session) is not empty (`0` on Linux, `EPERM` on macOS, which signals no
    /// zombie); and once the child is reaped the group is empty. NEGATIVE CONTROLS: let 0 or 1
    /// through its filter and each reads `Some(false)`; read `EPERM` as `None` and the zombie's
    /// group is unknown on macOS.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn only_a_real_group_is_asked_and_an_unreaped_leader_keeps_it() {
        let past = u32::try_from(i32::MAX).expect("i32::MAX") + 1;
        for pgid in [0, 1, past, u32::MAX] {
            assert_eq!(group::empty(pgid), None, "pgid {pgid}");
        }
        let mut leader = aterm_uds::spawnfd::command("/bin/sh");
        leader
            .args(["-c", "exit 0"])
            .setsid(true)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null());
        let mut leader = leader.spawn().expect("a leader");
        let pid = leader.id();
        aterm_spec::wait::until_within(HANG_DETECTOR, "the leader's exit", || {
            group::exited(pid, false)
                .expect("its exit, unreaped")
                .then_some(())
        });
        assert_eq!(
            group::empty(pid),
            Some(false),
            "the unreaped leader keeps its group"
        );
        leader.wait().expect("the leader, reaped");
        assert_eq!(group::empty(pid), Some(true), "reaped, it holds no one");
    }

    /// [`group::judged`] on each answer `killpg` gives: `ESRCH` is empty and `0` is not, neither
    /// reading the listing; any other errno is unknown; and `EPERM` is not empty, unless on Apple
    /// the group's whole listing finds no process, which is all an exec's leftover image holds
    /// there after its process is reaped. A listing cut short, none, or one that names a process
    /// (a zombie, another user's) keeps `EPERM` not empty. NEGATIVE CONTROL: answer
    /// `Some(false)` for every `EPERM` in `group::judged`, as it was, and the leftover's group
    /// reads not empty on Apple.
    #[cfg(unix)]
    #[test]
    fn a_group_held_only_by_an_exec_leftover_reads_empty() {
        use aterm_uds::process::GroupMembers;
        let unread = || -> Option<GroupMembers> { panic!("the listing is not read") };
        assert_eq!(group::judged(Err(Some(libc::ESRCH)), unread), Some(true));
        assert_eq!(group::judged(Ok(()), unread), Some(false));
        assert_eq!(group::judged(Err(Some(libc::EINVAL)), unread), None);
        assert_eq!(group::judged(Err(None), unread), None);
        let whole = GroupMembers {
            pids: Vec::new(),
            complete: true,
        };
        assert_eq!(
            group::judged(Err(Some(libc::EPERM)), || Some(whole.clone())),
            Some(cfg!(target_vendor = "apple")),
            "a whole listing that finds no process"
        );
        let cut = GroupMembers {
            pids: Vec::new(),
            complete: false,
        };
        let named = GroupMembers {
            pids: vec![4242],
            complete: true,
        };
        for listing in [Some(cut), Some(named), None] {
            assert_eq!(
                group::judged(Err(Some(libc::EPERM)), || listing.clone()),
                Some(false),
                "{listing:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_interrupted_gate_kills_every_live_stage_group() {
        use std::os::unix::process::ExitStatusExt as _;
        const PIDFILE: &str = "ATV_INTERRUPT_PIDFILE";
        if let Some(pidfile) = std::env::var_os(PIDFILE) {
            group::kill_on_interrupt();
            let pidfile = PathBuf::from(pidfile);
            let armed = pidfile.with_extension("armed");
            let ctx = crate::Ctx::new(
                PathBuf::from("/wt-a"),
                crate::Scope::workspace(),
                crate::EnvSnapshot::default(),
                PathBuf::from("/tmp/atv-s"),
            );
            let records = pidfile.with_extension("records");
            let record = crate::runs::Runs::open(&ctx, &records, |_, _| Run {
                stopped_at: None,
                ok: false,
                output: String::new(),
                code: None,
                spawn_error: None,
                usage: None,
            });
            if cfg!(any(target_vendor = "apple", target_os = "linux")) {
                assert!(record.is_some(), "the stand-in keeps a record");
            }
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
            group::end_interrupted();
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
        if cfg!(any(target_vendor = "apple", target_os = "linux")) {
            let kept = std::fs::read_dir(tmp.join("handled.records"))
                .map(|d| d.flatten().count())
                .unwrap_or(0);
            assert_eq!(kept, 1, "the interrupted run kept its record");
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The lock [`lock_waiter_stand_in`] waits on.
    #[cfg(unix)]
    const WAIT_LOCK: &str = "ATERM_VERIFY_TEST_WAIT_LOCK";

    /// A complete fixture line, never a word in a command or checkout path.
    #[cfg(unix)]
    const LOCK_ADMITTED: &str = "ATERM_VERIFY_TEST_LOCK_ADMITTED";

    /// Set by [`lock_waiter_stand_in`]'s `SIGTERM` handler, as a lab's interrupt sets its stop.
    #[cfg(unix)]
    static STAND_IN_STOPPED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    #[cfg(unix)]
    extern "C" fn stand_in_stop(_: i32) {
        STAND_IN_STOPPED.store(true, Ordering::SeqCst);
    }

    /// How long [`lock_waiter_stand_in`] takes, once its wait is ended, to say NOT RUN, as a lab
    /// lets its claims go first.
    #[cfg(unix)]
    const STAND_IN_TEARDOWN: Duration = Duration::from_millis(500);

    /// Set, [`lock_waiter_stand_in`] waits at a test's pace: a line every tenth of a second, its
    /// holder called stuck after [`STUCK_AFTER`] of no progress.
    #[cfg(unix)]
    const WAIT_PACED: &str = "ATERM_VERIFY_TEST_LOCK_PACED";

    /// How long a paced [`lock_waiter_stand_in`] lets its holder make no progress before it calls
    /// it stuck.
    #[cfg(unix)]
    const STUCK_AFTER: Duration = Duration::from_secs(1);

    /// Set, [`lock_waiter_stand_in`] exits 0 once it has the lock and has said so, as a lab whose
    /// lane came free runs and ends.
    #[cfg(unix)]
    const WAIT_THEN_EXIT: &str = "ATERM_VERIFY_TEST_LOCK_THEN_EXIT";

    /// Run by the ceiling tests as a stage child: waits for the lock [`WAIT_LOCK`] names, as a lab
    /// waits for its lane, says when it has it, then works until it is ended or its run is gone
    /// ([`WAIT_THEN_EXIT`] set: it ends there).
    /// A `SIGTERM` ends its wait as a lab's interrupt does: NOT RUN, naming the holder, and exit 3.
    #[cfg(unix)]
    #[test]
    fn lock_waiter_stand_in() {
        use aterm_toolchain::named_lock::{Holder, Pace, Refused, Wait, take, take_paced};
        let Some(path) = std::env::var_os(WAIT_LOCK).map(PathBuf::from) else {
            return;
        };
        // SAFETY: `stand_in_stop` is an `extern "C" fn(i32)` that lives for the whole program and
        // only stores to an atomic, which is async-signal-safe.
        unsafe {
            libc::signal(libc::SIGTERM, stand_in_stop as extern "C" fn(i32) as usize);
        }
        let me = Holder::me("lab linux exec", Path::new("/checkouts/waiter"));
        let stop = || STAND_IN_STOPPED.load(Ordering::SeqCst);
        let taken = if std::env::var_os(WAIT_PACED).is_some() {
            let pace = Pace {
                say_every: Duration::from_millis(100),
                stalled_after: STUCK_AFTER,
            };
            take_paced(&path, &me, Wait::Block, &mut |_, _| {}, &stop, pace)
        } else {
            take(&path, &me, Wait::Block, &mut |_, _| {}, &stop)
        };
        let _held = match taken {
            Ok(held) => held,
            Err(Refused::Stopped { holder, .. }) => {
                std::thread::sleep(STAND_IN_TEARDOWN);
                println!(
                    "NOT RUN: interrupted while waiting for {}, which holds the lane",
                    holder.map_or_else(|| "no one".to_string(), |h| h.describe(0))
                );
                std::process::exit(3);
            }
            Err(e) => panic!("the lock, once its holder lets go: {e:?}"),
        };
        println!("\n{LOCK_ADMITTED}");
        if std::env::var_os(WAIT_THEN_EXIT).is_some() {
            return;
        }
        // Its run ends it; a run that died first (a failed test) left it a new parent, and it ends.
        let run = std::os::unix::process::parent_id();
        while std::os::unix::process::parent_id() == run {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// A stand-in holder's lock at `dir/lane.lock`, and a stage child that waits on it.
    #[cfg(unix)]
    fn held_lane(dir: &Path) -> (aterm_toolchain::named_lock::NamedLock, Cmd) {
        held_lane_by(dir, std::process::id())
    }

    /// [`held_lane`], its record naming `pid` as the holder.
    #[cfg(unix)]
    fn held_lane_by(dir: &Path, pid: u32) -> (aterm_toolchain::named_lock::NamedLock, Cmd) {
        let lock = dir.join("lane.lock");
        let held = aterm_toolchain::named_lock::take(
            &lock,
            &aterm_toolchain::named_lock::Holder {
                pid,
                ..aterm_toolchain::named_lock::Holder::me(
                    "lab linux proof",
                    Path::new("/checkouts/holder"),
                )
            },
            aterm_toolchain::named_lock::Wait::No,
            &mut |_, _| {},
            &|| false,
        )
        .expect("a free lock");
        let waiter = Cmd::new(std::env::current_exe().expect("the test binary"))
            .args([
                "--exact",
                "exec::tests::lock_waiter_stand_in",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(WAIT_LOCK, &lock);
        (held, waiter)
    }

    /// The wait the child of a run with `scratch` has marked, once it has: the one-minute hang
    /// detector. A mark this test laid ([`StartMark`]) is not the child's.
    #[cfg(unix)]
    fn marked(scratch: &Path) -> aterm_toolchain::waits::Waiting {
        let deadline = Instant::now() + HANG_DETECTOR;
        loop {
            let found = std::fs::read_dir(scratch)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("waits."))
                .find_map(|e| {
                    aterm_toolchain::waits::waiting(&e.path())
                        .into_iter()
                        .rfind(|w| w.pid != std::process::id())
                });
            if let Some(w) = found {
                return w;
            }
            assert!(Instant::now() < deadline, "the child never marked its wait");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A wait this test marks in the marks' directory of the one run with `scratch`, laid as soon
    /// as the run lays that directory (before or just after it spawns its child, the one-minute
    /// hang detector): the child's start, which its ceiling would count as work until the child
    /// marks a wait of its own, so a start a loaded machine holds back (an exec, a page-in) is
    /// not what its ceiling ends.
    #[cfg(unix)]
    struct StartMark {
        /// Held for its drop, which lets the mark go.
        _mark: aterm_toolchain::waits::Mark,
    }

    #[cfg(unix)]
    impl StartMark {
        fn lay(scratch: &Path) -> StartMark {
            let deadline = Instant::now() + HANG_DETECTOR;
            loop {
                let dir = std::fs::read_dir(scratch)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .find(|e| e.file_name().to_string_lossy().starts_with("waits."));
                if let Some(mark) = dir.and_then(|d| {
                    aterm_toolchain::waits::Mark::at(&d.path(), "the test's child, starting").ok()
                }) {
                    return StartMark { _mark: mark };
                }
                assert!(
                    Instant::now() < deadline,
                    "the run never laid its marks' directory"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    /// CEILING-WAIT's acceptance: a stage child waits on a lock its holder keeps for longer than
    /// the child's ceiling, and the ceiling does not end it while it waits; once it has the lock,
    /// the ceiling runs, and ends it at that much work, saying how long it waited apart. The
    /// child's start is marked a wait ([`StartMark`]) until its own wait is marked, so however
    /// late a loaded machine starts it, its ceiling times only the work after its admission.
    /// Negative control: timed from its spawn, as before, the ceiling ends it while it waits,
    /// never admitted, with no status.
    #[cfg(unix)]
    #[test]
    fn the_ceiling_times_a_childs_work_from_its_admission_not_its_wait() {
        let tmp = crate::mktemp_dir("atv-ceiling-wait").expect("mktemp");
        let (held, waiter) = held_lane(&tmp);
        let ceiling = Duration::from_secs(5);
        let running = {
            let tmp = tmp.clone();
            std::thread::spawn(move || run(&waiter, ceiled(&tmp, Some(ceiling))))
        };
        let start = StartMark::lay(&tmp);
        let wait = marked(&tmp);
        drop(start);
        assert!(
            wait.text.contains(&format!(
                "waiting for pid {} (lab linux proof, /checkouts/holder)",
                std::process::id()
            )),
            "{}",
            wait.text
        );
        // Hold the lane past the ceiling: the condition under test, not a wait for anything.
        std::thread::sleep(ceiling + Duration::from_secs(2));
        assert!(
            !running.is_finished(),
            "the ceiling ended the child while it waited: {:?}",
            running.join().map(|r| r.output)
        );
        drop(held);
        let out = running.join().expect("the run");
        assert!(!out.ok && out.code.is_none(), "{out:?}");
        assert!(
            out.output.lines().any(|line| line == LOCK_ADMITTED),
            "{}",
            out.output
        );
        assert!(
            out.output.contains(crate::findings::CEILING_KILL)
                && out.output.contains("of work, over the 5.0s ceiling")
                && out.output.contains("it also waited"),
            "{}",
            out.output
        );
        // A test run counts the same wait as work: a test stuck in one is a hang the ceiling ends.
        let lane = tmp.join("test-run");
        std::fs::create_dir(&lane).expect("a second lane");
        let (held, test) = held_lane(&lane);
        let r = run(
            &test.counting_waits(),
            ceiled(&tmp, Some(Duration::from_secs(2))),
        );
        assert!(
            r.output.contains(crate::findings::CEILING_KILL)
                && !r.output.lines().any(|line| line == LOCK_ADMITTED)
                && !r.output.contains("it also waited"),
            "{}",
            r.output
        );
        drop(held);
        // The Linux lane's row says so too, not a bare `no exit status`.
        let (_, row) = crate::stages::linux_lane_outcome(out.code, &out.output);
        assert!(
            row.contains("ended at the ceiling")
                && row.contains("of work, over the 5.0s ceiling")
                && row.contains("it also waited"),
            "{row}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A child the run's end finds waiting says so and names the holder, ended as an interrupt
    /// ends it: the run is marked ending first, then the child gets the handler's `SIGTERM`, which
    /// ends its wait, so its mark goes while it lives on to say NOT RUN. The run does not signal it
    /// again, so the child's own NOT RUN, naming the holder, is kept; the run's reason names the
    /// wait its clock last saw; and the Linux lane's row reads NOT RUN with it, the lane named and
    /// then the wait, never `killed by a signal, or never spawned`. Negative controls: a look
    /// after the end that forgets what the last one found reads `the run was interrupted while
    /// this child ran`; a run that ends the child itself cuts the child's NOT RUN; a row read from
    /// the exit code alone begins `no exit status — killed by a signal`.
    #[cfg(unix)]
    #[test]
    fn a_child_ended_while_it_waits_says_whom_it_waited_for() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        let tmp = crate::mktemp_dir("atv-ended-waiting").expect("mktemp");
        let (held, waiter) = held_lane(&tmp);
        let (end, seen) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let running = {
            let (tmp, end, seen) = (tmp.clone(), Arc::clone(&end), Arc::clone(&seen));
            std::thread::spawn(move || {
                ends::when(end, seen);
                run(&waiter, ceiled(&tmp, Some(DEFAULT_CHILD_CEILING)))
            })
        };
        let wait = marked(&tmp);
        // The racing point, held: the run's clock has seen the wait before the end.
        let deadline = Instant::now() + HANG_DETECTOR;
        while !seen.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < deadline,
                "the run's clock never saw the wait"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        end.store(true, Ordering::SeqCst);
        let pgid = i32::try_from(wait.pid).expect("a pid");
        // SAFETY: two integers; `pgid` is the run's own child, which leads its group and cannot be
        // reaped before it exits, and it waits on a lock this test holds until this signal ends
        // that wait.
        assert_eq!(unsafe { libc::killpg(pgid, libc::SIGTERM) }, 0);
        let out = running.join().expect("the run");
        drop(held);
        assert!(
            !out.output.lines().any(|line| line == LOCK_ADMITTED),
            "{}",
            out.output
        );
        assert!(
            out.output.contains(&format!(
                "NOT RUN: interrupted while waiting for pid {} (lab linux proof, /checkouts/holder",
                std::process::id()
            )),
            "the child was ended before it said why: {}",
            out.output
        );
        let why = out
            .spawn_error
            .clone()
            .expect("the run's end is its reason");
        assert!(
            why.starts_with("the run was interrupted while this child waited")
                && why.contains(&wait.text)
                && why.contains(&format!("pid {}", std::process::id())),
            "{why}"
        );
        let mut r = crate::Report::new("linux");
        r.record_driven(
            &out,
            crate::stages::LINUX_LANE,
            crate::stages::linux_lane_outcome,
        );
        let row = r.render();
        let row = row
            .lines()
            .find(|l| {
                l.starts_with(
                    crate::ladder::Outcome::Fail(crate::ladder::Severity::CouldNotRun).tag(),
                )
            })
            .unwrap_or_else(|| panic!("no NOT RUN row: {row}"));
        // The row names the lane, then the wait: the child neither died of a signal of its own
        // nor failed to spawn, so it says neither.
        assert!(
            row.contains(&format!(
                "{} — the run was interrupted while this child waited",
                crate::stages::LINUX_LANE
            )) && row.contains(&wait.text),
            "{row}"
        );
        assert!(
            !row.contains("killed by a signal") && !row.contains("never spawned"),
            "{row}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A wait its waiter calls stuck ends the child only on a second look in a row: a mark is
    /// rewritten in place, and a read between its new record's write and its truncation finds
    /// the replaced record's `stuck` line after the new first line, as the record here is laid.
    /// A holder in the child's own group is never the reason, however long the wait is stuck.
    /// Negative control: a clock that believes one look ends the child on the torn record.
    #[cfg(unix)]
    #[test]
    fn a_stuck_wait_is_believed_on_its_second_look() {
        use aterm_toolchain::waits::{Mark, Stuck};
        let tmp = crate::mktemp_dir("atv-stuck-looks").expect("mktemp");
        let marks = tmp.join("waits");
        std::fs::create_dir(&marks).expect("the marks' dir");
        // The holder: this process, whose group is this test's own.
        let holder = std::process::id();
        // SAFETY: `getpgid(0)` reads this process's group; it signals nothing.
        let group = u32::try_from(unsafe { libc::getpgid(0) }).expect("this process's group");
        let stuck = Stuck {
            holders: vec![holder],
            span: Duration::from_secs(900),
        };
        let mut mark = Mark::at(&marks, "waiting for its holder, which works").expect("a mark");
        let mut clock = Clock::new(Some(&marks), &INERT, std::process::id(), None);
        // The torn record: the new first line, the replaced record's `stuck` line after it.
        let path = std::fs::read_dir(&marks)
            .expect("the marks")
            .flatten()
            .map(|e| e.path())
            .next()
            .expect("the mark's file");
        let torn =
            format!("{holder}\t1\twaiting for its holder, which works\nstuck\t900\t{holder}\n");
        std::fs::write(&path, &torn).expect("the torn record");
        clock.look();
        assert!(
            clock.waits.iter().any(|w| w.stuck.is_some()),
            "{:?}",
            clock.waits
        );
        assert_eq!(clock.stuck(u32::MAX), None, "one look");
        mark.say("waiting for its holder, which works");
        clock.look();
        assert_eq!(clock.stuck(u32::MAX), None, "the record said anew");
        mark.say_stuck(
            "waiting for its holder, which has made no progress",
            Some(&stuck),
        );
        clock.look();
        assert_eq!(clock.stuck(u32::MAX), None, "its first look");
        clock.look();
        let why = clock.stuck(u32::MAX).expect("its second look");
        assert!(
            why.contains("no progress for 15m00s")
                && why.contains("waiting for its holder, which has made no progress"),
            "{why}"
        );
        assert_eq!(clock.stuck(group), None, "a holder in the child's group");
        drop(mark);
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A second stuck mark cannot borrow the first mark's confirmation. Dropping the mark or
    /// observing it without a stuck tail also ends its confirmation, while fresh diagnostic
    /// text and a growing elapsed span on the same wait do not.
    #[cfg(unix)]
    #[test]
    fn stuck_confirmation_belongs_to_the_same_live_mark() {
        use aterm_toolchain::waits::{Mark, Stuck};
        let tmp = crate::mktemp_dir("atv-stuck-mark").expect("mktemp");
        let marks = tmp.join("waits");
        std::fs::create_dir(&marks).expect("the marks' dir");
        let holder = std::process::id();
        let stuck = Stuck {
            holders: vec![holder],
            span: Duration::from_secs(900),
        };
        let mut first = Mark::at(&marks, "waiting").expect("first mark");
        let mut second = Mark::at(&marks, "waiting").expect("second mark");
        let mut clock = Clock::new(Some(&marks), &INERT, holder, None);
        first.say_stuck("waiting", Some(&stuck));
        clock.look();
        assert_eq!(clock.stuck(u32::MAX), None, "first mark's first look");
        first.say("waiting");
        second.say_stuck("waiting", Some(&stuck));
        clock.look();
        assert_eq!(clock.stuck(u32::MAX), None, "second mark's first look");
        second.say_stuck(
            "diagnostic changed",
            Some(&Stuck {
                span: Duration::from_secs(901),
                ..stuck.clone()
            }),
        );
        clock.look();
        assert!(clock.stuck(u32::MAX).is_some(), "same mark's second look");
        second.say("waiting");
        clock.look();
        assert_eq!(clock.stuck(u32::MAX), None, "not stuck this look");
        second.say_stuck("waiting", Some(&stuck));
        clock.look();
        assert_eq!(clock.stuck(u32::MAX), None, "confirmation must start again");
        clock.look();
        assert!(clock.stuck(u32::MAX).is_some());
        drop(first);
        drop(second);
        clock.look();
        assert_eq!(clock.stuck(u32::MAX), None, "no live mark");
        let mut third = Mark::at(&marks, "waiting").expect("third mark");
        third.say_stuck("waiting", Some(&stuck));
        clock.look();
        assert_eq!(
            clock.stuck(u32::MAX),
            None,
            "empty look cleared confirmation"
        );
        clock.look();
        assert!(clock.stuck(u32::MAX).is_some());
        drop(third);
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A wait's replacement or expanded holder set needs its own second look. Ordering,
    /// duplicates, diagnostic words and a growing span are not changes to that holder set.
    #[cfg(unix)]
    #[test]
    fn stuck_confirmation_belongs_to_the_same_holder_set() {
        use aterm_toolchain::waits::{Mark, Stuck};
        let tmp = crate::mktemp_dir("atv-stuck-holders").expect("mktemp");
        let marks = tmp.join("waits");
        std::fs::create_dir(&marks).expect("the marks' dir");
        let holder = std::process::id();
        let other = u32::MAX - 1;
        let mut mark = Mark::at(&marks, "waiting").expect("a mark");
        let mut clock = Clock::new(Some(&marks), &INERT, holder, None);
        let mut say = |holders, span| {
            mark.say_stuck("waiting", Some(&Stuck { holders, span }));
        };
        say(vec![holder], Duration::from_secs(900));
        clock.look();
        say(vec![other], Duration::from_secs(900));
        clock.look();
        assert_eq!(clock.stuck(u32::MAX), None, "a different holder");
        clock.look();
        assert!(clock.stuck(u32::MAX).is_some(), "holder confirmed");
        say(vec![holder, other], Duration::from_secs(901));
        clock.look();
        assert_eq!(clock.stuck(u32::MAX), None, "a new holder joined");
        say(vec![other, holder, holder], Duration::from_secs(902));
        clock.look();
        assert!(clock.stuck(u32::MAX).is_some(), "same normalized set");
        say(vec![holder], Duration::from_secs(903));
        clock.look();
        assert_eq!(clock.stuck(u32::MAX), None, "one holder left");
        clock.look();
        assert!(clock.stuck(u32::MAX).is_some());
        drop(mark);
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// An established wait inside the child's group cannot prime a new external wait. This
    /// checks selection when a confirmed but excluded record remains beside the new record.
    #[cfg(unix)]
    #[test]
    fn an_excluded_mark_cannot_confirm_a_new_external_wait() {
        use aterm_toolchain::waits::{Mark, Stuck};
        let tmp = crate::mktemp_dir("atv-stuck-excluded").expect("mktemp");
        let marks = tmp.join("waits");
        std::fs::create_dir(&marks).expect("the marks' dir");
        let holder = std::process::id();
        // SAFETY: `getpgid(0)` reads this process's group; it signals nothing.
        let group = u32::try_from(unsafe { libc::getpgid(0) }).expect("this process's group");
        let mut internal = Mark::at(&marks, "internal wait").expect("internal mark");
        internal.say_stuck(
            "internal wait",
            Some(&Stuck {
                holders: vec![holder],
                span: Duration::from_secs(900),
            }),
        );
        let mut clock = Clock::new(Some(&marks), &INERT, holder, None);
        clock.look();
        clock.look();
        assert_eq!(clock.stuck(group), None, "its holder would be signalled");
        let mut external = Mark::at(&marks, "external wait").expect("external mark");
        // A synthetic PID outside any representable process group, not a process to signal.
        external.say_stuck(
            "external wait",
            Some(&Stuck {
                holders: vec![u32::MAX - 1],
                span: Duration::from_secs(900),
            }),
        );
        clock.look();
        assert_eq!(clock.stuck(group), None, "new external wait seen only once");
        clock.look();
        assert!(clock.stuck(group).is_some(), "external wait seen twice");
        drop(internal);
        drop(external);
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// Set, the test binary is [`holder_stand_in`]: `idle` naps, `works` naps beside a child that
    /// works (`child`, which loops until its parent is gone), as a lane's run does beside its
    /// guest's.
    #[cfg(unix)]
    const HOLDER_STAND_IN: &str = "ATERM_VERIFY_TEST_HOLDER";

    /// Run as a lock's named holder by [`a_child_waiting_behind_a_stuck_holder_could_not_run`]:
    /// lives for twice the hang detector unless its test ends it first, using no CPU time and
    /// writing nothing (`idle`), or beside a child that keeps a core busy (`works`). Linux counts
    /// a process's CPU time in clock ticks (`/proc/<pid>/stat`, 10 ms at `USER_HZ` 100), so the
    /// child's work must sum to several ticks within each [`STUCK_AFTER`] there, which a busy core
    /// does.
    #[cfg(unix)]
    #[test]
    fn holder_stand_in() {
        let Some(how) = std::env::var_os(HOLDER_STAND_IN) else {
            return;
        };
        if how == "child" {
            let parent = std::os::unix::process::parent_id();
            let until = Instant::now() + HANG_DETECTOR * 2;
            let mut n = 0u64;
            while std::os::unix::process::parent_id() == parent && Instant::now() < until {
                for i in 0..1_000_000u64 {
                    n = std::hint::black_box(n.wrapping_mul(31).wrapping_add(i));
                }
            }
            return;
        }
        let child = (how == "works").then(|| HolderStandIn::start("child"));
        std::thread::sleep(HANG_DETECTOR * 2);
        drop(child);
    }

    /// A [`holder_stand_in`], killed when dropped: no test leaves one behind.
    #[cfg(unix)]
    struct HolderStandIn(aterm_uds::spawnfd::Child);

    #[cfg(unix)]
    impl HolderStandIn {
        fn start(how: &str) -> HolderStandIn {
            HolderStandIn(
                aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
                    .args([
                        "--exact",
                        "exec::tests::holder_stand_in",
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .env(HOLDER_STAND_IN, how)
                    .stdin(aterm_uds::spawnfd::Stdio::null())
                    .stdout(aterm_uds::spawnfd::Stdio::null())
                    .stderr(aterm_uds::spawnfd::Stdio::null())
                    .spawn()
                    .expect("the holder stand-in"),
            )
        }

        /// Whether it still runs: no signal reached it.
        fn runs(&mut self) -> bool {
            matches!(self.0.try_wait(), Ok(None))
        }
    }

    #[cfg(unix)]
    impl Drop for HolderStandIn {
        fn drop(&mut self) {
            if self.runs() {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }

    /// The whole seconds an [`aterm_capacity::span`] under an hour reads (`7s`, `2m05s`).
    #[cfg(unix)]
    fn span_secs(span: &str) -> Option<u64> {
        let secs = span.strip_suffix('s')?;
        match secs.split_once('m') {
            Some((m, s)) => Some(m.parse::<u64>().ok()? * 60 + s.parse::<u64>().ok()?),
            None => secs.parse().ok(),
        }
    }

    /// CW-WAIT-UNBOUNDED: a stage child waits on a lock whose named holder lives on and makes no
    /// progress. Once its waiter calls the holder stuck, the run ends the child (`SIGTERM`, so it
    /// says its own NOT RUN within [`STUCK_GRACE`]) rather than waiting on, inside the hang
    /// detector past that grace and with no ceiling that could end it instead; the child could
    /// not run, naming the holder and how long it made no progress, at least the waiter's bound;
    /// the Linux lane's row reads NOT RUN with it; and the holder is never signalled and still
    /// runs. Negative control: a holder whose child works is waited for past three times the
    /// bound from the child's own wait mark, then the child has the lock and runs to its end.
    /// Neither run has a ceiling, so a starved start of the child, which a ceiling counts as
    /// work, decides neither, and the control's span starts once the child waits, however late
    /// it started.
    #[cfg(unix)]
    #[test]
    fn a_child_waiting_behind_a_stuck_holder_could_not_run() {
        let tmp = crate::mktemp_dir("atv-stuck-holder").expect("mktemp");
        let mut holder = HolderStandIn::start("idle");
        let (held, waiter) = held_lane_by(&tmp, holder.0.id());
        let waiter = waiter.env(WAIT_PACED, "1");
        let started = Instant::now();
        let running = {
            let (tmp, waiter) = (tmp.clone(), waiter.clone());
            std::thread::spawn(move || run(&waiter, ceiled(&tmp, None)))
        };
        aterm_spec::wait::until_within(
            STUCK_GRACE + HANG_DETECTOR,
            "the run to end the child",
            || running.is_finished().then_some(()),
        );
        let out = running.join().expect("the run");
        eprintln!(
            "the stuck holder ended the child {:?} after it began",
            started.elapsed()
        );
        let why = out
            .spawn_error
            .clone()
            .expect("the stuck holder is its reason");
        let named = format!("pid {} (lab linux proof, /checkouts/holder)", holder.0.id());
        let figure = why
            .strip_prefix(
                "this child waited for admission or a lock behind a holder that has made no \
                 progress for ",
            )
            .and_then(|rest| rest.split_once(", so it was ended"))
            .and_then(|(span, _)| span_secs(span));
        assert!(
            figure.is_some_and(|secs| secs >= STUCK_AFTER.as_secs())
                && why.contains(&format!("{named}, which has held"))
                && why.contains(", and which has made no progress for "),
            "{why}"
        );
        assert!(
            out.output.contains(&format!(
                "NOT RUN: interrupted while waiting for {}",
                named.trim_end_matches(')')
            )),
            "the child was ended before it said why: {}",
            out.output
        );
        assert!(
            !out.output.lines().any(|l| l == LOCK_ADMITTED),
            "{}",
            out.output
        );
        let mut r = crate::Report::new("linux");
        r.record_driven(
            &out,
            crate::stages::LINUX_LANE,
            crate::stages::linux_lane_outcome,
        );
        let row = r.render();
        assert!(
            row.lines().any(|l| l.starts_with(
                crate::ladder::Outcome::Fail(crate::ladder::Severity::CouldNotRun).tag()
            ) && l.contains(&format!(
                "{} — this child waited for admission or a lock behind a holder",
                crate::stages::LINUX_LANE
            )) && l.contains(&named)),
            "{row}"
        );
        assert!(holder.runs(), "the holder was signalled");
        assert_eq!(
            aterm_toolchain::named_lock::read(&tmp.join("lane.lock")).map(|h| h.pid),
            Some(holder.0.id()),
            "the holder keeps the lock"
        );
        drop((held, holder));

        let lane = tmp.join("working");
        std::fs::create_dir(&lane).expect("a second lane");
        let mut working = HolderStandIn::start("works");
        let (held, waiter) = held_lane_by(&lane, working.0.id());
        let waiter = waiter.env(WAIT_PACED, "1").env(WAIT_THEN_EXIT, "1");
        let running = {
            let lane = lane.clone();
            std::thread::spawn(move || run(&waiter, ceiled(&lane, None)))
        };
        let wait = marked(&lane);
        assert!(
            wait.text
                .contains(&format!("waiting for pid {} (", working.0.id())),
            "{}",
            wait.text
        );
        // Three times the bound past the child's wait mark with the holder at work: the
        // condition under test.
        std::thread::sleep(STUCK_AFTER * 3 + Duration::from_millis(500));
        assert!(
            !running.is_finished(),
            "a working holder's waiter was ended: {:?}",
            running.join().map(|r| (r.spawn_error, r.output))
        );
        assert!(working.runs());
        drop(held);
        aterm_spec::wait::until_within(HANG_DETECTOR, "the admitted child's end", || {
            running.is_finished().then_some(())
        });
        let out = running.join().expect("the run");
        assert!(
            out.ok && out.spawn_error.is_none(),
            "{:?}: {}",
            out.spawn_error,
            out.output
        );
        assert!(
            out.output.lines().any(|l| l == LOCK_ADMITTED),
            "{}",
            out.output
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A host whose every read of a group finds one member, the group's leader, whose counters
    /// have grown by 1 ms of CPU, 2 ms runnable and 1 MiB held at each read so far, whatever
    /// the group: how many reads a child gets is the clock's affair, and its last is its most.
    #[cfg(unix)]
    #[derive(Debug, Default)]
    struct Counting {
        reads: AtomicU64,
    }

    #[cfg(unix)]
    impl Host for Counting {
        fn group_usage(&self, pgid: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
            let n = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
            Some(vec![aterm_sysprobe::MemberUsage {
                pid: pgid,
                start: 1,
                cpu_ns: n * 1_000_000,
                runnable_ns: n * 2_000_000,
                footprint: n << 20,
                peak_footprint: n << 20,
                ..aterm_sysprobe::MemberUsage::default()
            }])
        }

        fn idle(&self) -> Option<Duration> {
            None
        }

        fn cores(&self) -> u32 {
            1
        }
    }

    /// A STAGE'S CHILDREN SUM TO THEIR STAGE (A1): two children run one after the other under one
    /// stage's meter each return the use their own reads found, booked on exactly the reads that
    /// book the stage (the clock's, then the sweep's of the ended leader, the last and most), so
    /// their CPU sums to the stage's observed CPU to the nanosecond, whatever number of reads each
    /// got. Each child's CPU is its last read's, its runnable time twice that, its peak that
    /// read's footprint. NEGATIVE CONTROL: a sweep that books the stage's meter and not the
    /// child's own leaves each child its second-to-last read, and the sum falls short by 2 ms.
    #[cfg(unix)]
    #[test]
    fn a_stages_children_sum_to_their_stage() {
        let tmp = crate::mktemp_dir("atv-usage-sum").expect("mktemp");
        let host: &'static Counting = Box::leak(Box::default());
        let stage = crate::memory::Meter::new();
        let _metering = crate::memory::metering(Some(Arc::clone(&stage)));
        let env = ExecEnv {
            host,
            ..env_in(&tmp)
        };
        let child = || {
            let out = run(&Cmd::new("/bin/sh").args(["-c", "exit 0"]), env);
            assert!(out.ok, "{out:?}");
            (
                out.usage.expect("its tree was read"),
                host.reads.load(Ordering::SeqCst),
            )
        };
        let (a, after_a) = child();
        let (b, after_b) = child();
        let ms = Duration::from_millis;
        assert_eq!(
            a.cpu,
            ms(after_a),
            "the first child's last read is its most"
        );
        assert_eq!(b.cpu, ms(after_b), "{b:?}");
        assert_eq!(
            (a.runnable, a.peak_bytes, a.processes),
            (ms(2 * after_a), after_a << 20, 1)
        );
        let observed = stage.observed_cpu().expect("the stage was read");
        assert_eq!(
            a.cpu + b.cpu,
            Duration::from_nanos(observed.nanoseconds),
            "the children sum to their stage"
        );
        assert_eq!(a.processes + b.processes, observed.processes);
        assert!(a.wall > Duration::ZERO && b.wall > Duration::ZERO);
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A host with no reading of a child's tree (every platform but macOS) gives its run no
    /// usage, never zeros; nor does a child that never started. NEGATIVE CONTROL: the same child
    /// on a host that reads its tree has usage.
    #[cfg(unix)]
    #[test]
    fn a_host_with_no_reading_gives_a_run_no_usage() {
        let tmp = crate::mktemp_dir("atv-usage-none").expect("mktemp");
        let child = Cmd::new("/bin/sh").args(["-c", "exit 3"]);
        let out = run(&child, env_in(&tmp));
        assert_eq!((out.ok, out.code, out.usage), (false, Some(3), None));
        let missing = run(&Cmd::new("/no/such/tool"), env_in(&tmp));
        assert!(missing.spawn_error.is_some() && missing.usage.is_none());
        let host: &'static Counting = Box::leak(Box::default());
        let read = run(
            &child,
            ExecEnv {
                host,
                ..env_in(&tmp)
            },
        );
        assert!(
            read.usage.is_some_and(|u| u.cpu > Duration::ZERO),
            "{read:?}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A stage child that writes its pid to `pidfile` (its shell leads its group), then runs
    /// `then`.
    #[cfg(unix)]
    fn stand_in(pidfile: &Path, then: &str) -> Cmd {
        Cmd::new("/bin/sh").args(["-c", &format!("echo $$ > '{}'; {then}", pidfile.display())])
    }

    /// `cmd` run on a thread of its own under `ceiling` on `host`.
    #[cfg(unix)]
    fn running(
        cmd: Cmd,
        dir: &Path,
        ceiling: Duration,
        host: &'static dyn Host,
    ) -> (std::thread::JoinHandle<()>, std::sync::mpsc::Receiver<Run>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let dir = dir.to_path_buf();
        let thread = std::thread::spawn(move || {
            let env = ExecEnv {
                host,
                ..ceiled(&dir, Some(ceiling))
            };
            let _ = tx.send(run(&cmd, env));
        });
        (thread, rx)
    }

    /// The run `rx` answers, once its ceiling ends its child: the one-minute hang detector, past
    /// which the child's group (by the pid it wrote) is killed and the test fails.
    #[cfg(unix)]
    fn ended(
        (thread, rx): (std::thread::JoinHandle<()>, std::sync::mpsc::Receiver<Run>),
        pidfile: &Path,
        what: &str,
    ) -> Run {
        let Ok(out) = rx.recv_timeout(HANG_DETECTOR) else {
            if let Some(pid) = written_pid(pidfile) {
                let _ = std::process::Command::new("/bin/kill")
                    .args(["-KILL", "--", &format!("-{pid}")])
                    .status();
            }
            let _ = thread.join();
            panic!("{what}: the ceiling never ended it");
        };
        let _ = thread.join();
        out
    }

    /// CEILING-WAIT's wait for a core: a stand-in child whose tree is given 5% of the time it is
    /// runnable, under a 3 s ceiling, still runs 7 s in, whether its host books that time as it
    /// passes or in lumps a second apart, twice its reads' window. Given every core it asks for,
    /// the ceiling ends it at 3 s of work, and the kill line names apart the time it waited for a
    /// core. Negative controls: the same child on a host with no reading of its tree (the clock
    /// before this step) is ended at its ceiling while it waits, naming no core; a clock that
    /// judges each read alone ends the lumpy one while it is starved.
    #[cfg(unix)]
    #[test]
    fn a_child_starved_of_cores_is_not_ended_for_the_time_it_waited() {
        let tmp = crate::mktemp_dir("atv-starved").expect("mktemp");
        let ceiling = Duration::from_secs(3);
        let starved = [
            ("as it passes", Starving::at(50)),
            ("in lumps", Starving::lumpy(50, Duration::from_secs(1))),
        ]
        .map(|(how, host)| {
            let pidfile = tmp.join(format!("starved {how}.pid"));
            let run = running(stand_in(&pidfile, "exec sleep 600"), &tmp, ceiling, host);
            (how, host, pidfile, run)
        });
        // Hold them starved past the ceiling: the condition under test, not a wait for anything.
        std::thread::sleep(ceiling + Duration::from_secs(4));
        let held = starved
            .each_ref()
            .map(|(_, _, _, run)| !run.0.is_finished());
        for (_, host, _, _) in &starved {
            host.give(1000);
        }
        for ((how, _, pidfile, run), held) in starved.into_iter().zip(held) {
            let out = ended(run, &pidfile, &format!("booked {how}, given every core"));
            assert!(
                held,
                "the ceiling ended a child starved {how}: {}",
                out.output
            );
            assert!(!out.ok && out.code.is_none(), "{how}: {out:?}");
            assert!(
                out.usage.is_some_and(|u| u.starved > Duration::ZERO),
                "{how}: its usage names the wait for a core: {:?}",
                out.usage
            );
            assert!(
                out.output.contains(crate::findings::CEILING_KILL)
                    && out.output.contains("of work, over the 3.0s ceiling")
                    && out
                        .output
                        .contains("for a core, which the ceiling leaves out")
                    && !out.output.contains("admission or a lock"),
                "{how}: {}",
                out.output
            );
        }
        let control = tmp.join("control.pid");
        let out = ended(
            running(stand_in(&control, "exec sleep 600"), &tmp, ceiling, &INERT),
            &control,
            "the control",
        );
        assert!(
            out.output.contains("of work, over the 3.0s ceiling")
                && !out.output.contains("for a core")
                && out.usage.is_none(),
            "{}",
            out.output
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A tree runnable all the time but given no core at all makes no progress: after
    /// `NO_PROGRESS_HANG` its time counts as work again, so a 1 s ceiling ends it, naming the time
    /// it waited for a core before that apart. Negative control: with no such bound the ceiling
    /// never ends it, and the hang detector fails the test.
    #[cfg(unix)]
    #[test]
    fn a_tree_given_no_core_at_all_is_a_hang_its_ceiling_ends() {
        let tmp = crate::mktemp_dir("atv-no-core").expect("mktemp");
        let pidfile = tmp.join("no-core.pid");
        let out = ended(
            running(
                stand_in(&pidfile, "exec sleep 600"),
                &tmp,
                Duration::from_secs(1),
                Starving::at(0),
            ),
            &pidfile,
            "a tree given no core",
        );
        assert!(
            out.output.contains("of work, over the 1.0s ceiling")
                && out
                    .output
                    .contains("for a core, which the ceiling leaves out"),
            "{}",
            out.output
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// On the OS's reading of its tree, a stopped child is runnable for none of its time, so its
    /// ceiling ends it as a hang; and a busy-looping child, whatever share of the cores it gets,
    /// still ends at its ceiling of work. Negative controls: a clock that counts all of a tree's
    /// runnable time as waiting never ends the busy one; one that counts a window its tree was
    /// runnable for none of as a wait ends the stopped one only at `NO_PROGRESS_HANG`, and with
    /// that bound gone too never ends it (the arithmetic test fails on the first alone); the hang
    /// detector fails each that never ends.
    #[cfg(unix)]
    #[test]
    fn a_stopped_child_and_a_busy_one_still_end_at_their_ceiling() {
        let tmp = crate::mktemp_dir("atv-stopped-busy").expect("mktemp");
        for (name, then) in [
            ("stopped", "kill -STOP $$"),
            ("busy", "while :; do :; done"),
        ] {
            let pidfile = tmp.join(format!("{name}.pid"));
            let out = ended(
                running(
                    stand_in(&pidfile, then),
                    &tmp,
                    Duration::from_secs(1),
                    &LIVE_HOST,
                ),
                &pidfile,
                name,
            );
            assert!(
                !out.ok
                    && out.output.contains(crate::findings::CEILING_KILL)
                    && out.output.contains("of work, over the 1.0s ceiling"),
                "{name}: {}",
                out.output
            );
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The arithmetic of a window: what a tree used, read twice, and the part of the window it
    /// waited for a core. Negative controls are the cases each rule decides.
    #[test]
    fn a_window_waits_for_a_core_only_while_its_tree_is_runnable_and_not_running() {
        let ms = Duration::from_millis;
        let used = |cpu: u64, runnable: u64| Used {
            cpu: ms(cpu),
            runnable: ms(runnable),
            io: 0,
        };
        let w = ms(1000);
        let waits = |used: Used, want: Duration| {
            let got = core_wait(w, used, 18);
            assert!(
                got.abs_diff(want) < Duration::from_micros(1),
                "{used:?}: {got:?}, not {want:?}"
            );
        };
        // One thread runnable the whole window, on a core 30% of it: 700 ms waited.
        waits(used(300, 1000), ms(700));
        // Forty threads on 18 cores alone, getting nearly all of them: its own crowd, 50 ms.
        waits(used(17_100, 40_000), ms(50));
        // Ten threads given 10%: 900 ms of the window, not the 9 s their waits sum to.
        waits(used(1000, 10_000), ms(900));
        // One thread runnable half the window, on a core half of that: 250 ms, never 500.
        waits(used(250, 500), ms(250));
        // A blocked tree whose timer thread waited 40 µs to run: 40 µs, never the window.
        waits(
            Used {
                cpu: Duration::from_micros(10),
                runnable: Duration::from_micros(50),
                io: 0,
            },
            Duration::from_micros(40),
        );
        // Stopped, or blocked: runnable for none of it.
        waits(used(0, 0), Duration::ZERO);
        // Running all it was runnable: no wait.
        waits(used(1000, 1000), Duration::ZERO);

        let member =
            |pid: u32, start: u64, cpu: u64, runnable: u64, io: u64| aterm_sysprobe::MemberUsage {
                pid,
                start,
                cpu_ns: cpu,
                runnable_ns: runnable,
                io_bytes: io,
                ..aterm_sysprobe::MemberUsage::default()
            };
        let (first, seen) = used_between(&BTreeMap::new(), vec![member(7, 1, 10, 30, 5)]);
        assert_eq!(
            first,
            Used {
                cpu: Duration::from_nanos(10),
                runnable: Duration::from_nanos(30),
                io: 5
            },
            "a new member counts whole"
        );
        let (next, _) = used_between(&seen, vec![member(7, 1, 25, 70, 5), member(7, 2, 4, 6, 0)]);
        assert_eq!(
            next,
            Used {
                cpu: Duration::from_nanos(15 + 4),
                runnable: Duration::from_nanos(40 + 6),
                io: 0
            },
            "a member by its growth, and a pid taken anew (another start) whole"
        );
    }

    /// The arithmetic of a span of reads ([`CoreWaitSpan::book`]), 500 ms apart on 18 cores.
    /// - One thread given 5% whose time the kernel books every fourth read, in a lump: each lump
    ///   moves all 1.9 s it waited, and the reads between move nothing. Negative control: judged
    ///   read by read, the same lumps move 0.475 s each, a quarter of the wait.
    /// - Ten seconds of 18 threads on every core, then ten of one thread given 5%: by the end of
    ///   the second ten the span has moved all 9.5 s of its wait. Negative control: judged on the
    ///   totals since the first read, the busy past leaves 1 s.
    /// - Four threads given 5% that then stop: the reads with nothing booked move nothing, though
    ///   the span's share alone would call them starved; a booking after them moves their wait.
    /// - What the span's reads moved never passes the work counted in them, nor the clock's work;
    ///   and once forgotten (no progress for `NO_PROGRESS_HANG`), a lump moves at most its own
    ///   read's work.
    #[test]
    fn a_span_of_reads_moves_a_wait_the_kernel_books_late() {
        let ms = Duration::from_millis;
        let close = |got: Duration, want: Duration, what: &str| {
            assert!(
                got.abs_diff(want) < Duration::from_micros(10),
                "{what}: {got:?}, not {want:?}"
            );
        };
        let used = |cpu: u64, runnable: u64| Used {
            cpu: ms(cpu),
            runnable: ms(runnable),
            io: 0,
        };
        let w = ms(500);
        let lots = Duration::from_secs(3600);
        let lump = |i: usize| {
            if i % 4 == 3 {
                used(100, 2000)
            } else {
                used(0, 0)
            }
        };

        let mut span = CoreWaitSpan::default();
        let mut total = Duration::ZERO;
        for i in 0..40 {
            let moved = span.book(w, lump(i), w, 18, lots);
            let want = if i % 4 == 3 { ms(1900) } else { Duration::ZERO };
            close(moved, want, &format!("read {i}"));
            total += moved;
        }
        close(total, ms(19_000), "ten lumps");
        let read_by_read: Duration = (0..40).map(|i| core_wait(w, lump(i), 18)).sum();
        close(read_by_read, ms(4750), "the control, read by read");

        let mut span = CoreWaitSpan::default();
        let (busy, starved) = (used(9000, 9000), used(25, 500));
        let mut total = Duration::ZERO;
        for i in 0..40 {
            total += span.book(w, if i < 20 { busy } else { starved }, w, 18, lots);
        }
        close(total, ms(9500), "the starved ten seconds");
        let all = Used {
            cpu: ms(20 * 9000 + 20 * 25),
            runnable: ms(20 * 9000 + 20 * 500),
            io: 0,
        };
        close(
            core_wait(Duration::from_secs(20), all, 18),
            ms(1000),
            "the control, on the totals",
        );

        let mut span = CoreWaitSpan::default();
        for _ in 0..20 {
            span.book(w, used(100, 2000), w, 18, lots);
        }
        for i in 0..5 {
            let moved = span.book(w, used(0, 0), w, 18, lots);
            close(
                moved,
                Duration::ZERO,
                &format!("four threads stopped, read {i}"),
            );
        }
        close(
            span.book(w, used(100, 2000), w, 18, lots),
            ms(6 * 475),
            "four threads given 5% again: a booking covers the reads before it",
        );

        let mut span = CoreWaitSpan::default();
        let moved: Duration = (0..4)
            .map(|i| span.book(w, lump(i), ms(100), 18, lots))
            .sum();
        close(moved, ms(400), "at most the span's work");
        let mut span = CoreWaitSpan::default();
        let moved: Duration = (0..4).map(|i| span.book(w, lump(i), w, 18, ms(200))).sum();
        close(moved, ms(200), "at most the clock's work");
        let mut span = CoreWaitSpan::default();
        for _ in 0..20 {
            span.book(w, starved, w, 18, lots);
        }
        span.clear();
        let moved = span.book(w, used(100, 12_000), w, 18, lots);
        assert!(
            moved <= w && !moved.is_zero(),
            "a lump after a hang: {moved:?}"
        );
    }

    /// The kill line names a wait the ceiling left out only once it shows as `0.1s` or more, by its
    /// own figure; it tells a wait for admission or a lock from one for a core, and names both
    /// together.
    #[test]
    fn a_ceiling_kill_names_the_waits_it_left_out() {
        let s = Duration::from_secs_f64;
        assert_eq!(waited_clause(s(0.049), s(0.049)), "", "neither shows");
        assert!(
            waited_clause(s(0.05), s(0.0)).contains("0.1s"),
            "50 ms shows"
        );
        let lock = waited_clause(s(7.0), s(0.049));
        assert!(lock.contains("7.0s") && !lock.contains("0.0s"), "{lock}");
        let core = waited_clause(s(0.0), s(340.2));
        assert!(core.contains("340.2s") && !core.contains("0.0s"), "{core}");
        let both = waited_clause(s(12.0), s(3.5));
        assert!(both.contains("12.0s") && both.contains("3.5s"), "{both}");
        assert_ne!(
            waited_clause(s(7.0), s(0.0)),
            waited_clause(s(0.0), s(7.0)),
            "which wait it was"
        );
    }

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

    static BOUNDED_QUIET: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

    fn bounded_quiet_admit(
        need: aterm_capacity::Need,
        mode: aterm_capacity::Admit,
    ) -> Result<aterm_capacity::Claim, aterm_capacity::Error> {
        crate::admit_in(
            &aterm_capacity::Ledger::at(BOUNDED_QUIET.get().expect("scratch ledger")),
            &crate::timing_test_machine::Measured::new(&aterm_capacity::Fixed::new(
                aterm_capacity::Reading::of_gib(24, 20, 14),
            )),
            aterm_capacity::Pace {
                poll: Duration::from_millis(20),
                ..aterm_capacity::Pace::default()
            },
            need,
            mode,
            &mut |_| {},
        )
    }

    /// A completed last stage releases its quiet claim. A last stage whose bounded walk timed
    /// out keeps that same claim while the actual worker is blocked, even after the stage's
    /// owner is dropped, and releases it once that worker ends. The timeout is injected only
    /// after the worker entered; every wait is a hang detector, never an elapsed-time budget.
    #[test]
    fn a_last_stage_keeps_its_quiet_claim_until_its_bounded_worker_ends() {
        const HANG_DETECTOR: Duration = aterm_spec::wait::MUST_HAPPEN;
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let path = dir.path().join("ledger");
        BOUNDED_QUIET.set(path.clone()).expect("set once");
        let ledger = aterm_capacity::Ledger::at(&path);
        let held = |ledger: &aterm_capacity::Ledger| {
            ledger
                .list()
                .expect("ledger")
                .iter()
                .any(|r| r.held.is_some())
        };
        let ctx = crate::Ctx::new(
            dir.path().to_owned(),
            crate::Scope::crate_only("aterm-gui"),
            crate::EnvSnapshot::default(),
            dir.path().join("scratch"),
        )
        .with_quiet(Some(aterm_capacity::Admit::Wait));
        let last_stage = || {
            std::sync::Mutex::new(crate::Quiet {
                claim: None,
                left: 1,
                admit: bounded_quiet_admit,
            })
        };
        let ordinary = last_stage();
        let worker_ledger = aterm_capacity::Ledger::at(&path);
        let (complete, _) = crate::quietly(&ctx, &ordinary, "ordinary last stage", || {
            bounded(Some(HANG_DETECTOR), move || held(&worker_ledger))
        })
        .expect("admitted");
        assert_eq!(complete, Ok(true), "ordinary worker ran while admitted");
        aterm_spec::wait::until_within(
            HANG_DETECTOR,
            "completed worker released its quiet claim",
            || (!held(&ledger)).then_some(()),
        );

        let (entered, ready) = std::sync::mpsc::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let (finished, done) = std::sync::mpsc::channel();
        let last = last_stage();
        let (answer, _) = crate::quietly(&ctx, &last, "timed-out last stage", || {
            bounded_wait(
                move || {
                    entered.send(()).expect("entered");
                    resume.recv_timeout(HANG_DETECTOR).expect("released");
                    finished.send(()).expect("worker finished");
                },
                |_| {
                    ready.recv_timeout(HANG_DETECTOR).expect("worker entered");
                    Err(Unfinished::TimedOut(HANG_DETECTOR))
                },
            )
        })
        .expect("admitted");
        drop(last);
        let held_after_stage = held(&ledger);
        release.send(()).expect("release worker");
        done.recv_timeout(HANG_DETECTOR).expect("worker ended");
        aterm_spec::wait::until_within(HANG_DETECTOR, "worker released its quiet claim", || {
            (!held(&ledger)).then_some(())
        });
        assert_eq!(answer, Err(Unfinished::TimedOut(HANG_DETECTOR)));
        assert!(
            held_after_stage,
            "last stage returned but its actual worker still ran"
        );
    }

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
        let desk = |idle: Option<Duration>| {
            let host: &'static Idle = Box::leak(Box::new(Idle(idle)));
            ExecEnv { host, ..env }
        };
        let wrapped = [fake_s.as_str(), "-c", "utility", "/bin/sh", "-c", "exit 0"];
        for (idle, demoted, why) in [
            (Some(Duration::from_secs(5)), true, "a person typed 5 s ago"),
            (
                Some(AWAY_AFTER - Duration::from_secs(1)),
                true,
                "just inside the bound",
            ),
            (
                None,
                true,
                "an input clock that cannot be read keeps the demotion",
            ),
            (Some(AWAY_AFTER), false, "nobody at the desk for the bound"),
            (
                Some(Duration::from_secs(10 * 3600)),
                false,
                "nobody for ten hours",
            ),
        ] {
            let argv = spawned_argv(&spawn_command(&tool.clone().demoted(), desk(idle), &fake));
            if demoted {
                assert_eq!(argv, wrapped, "{why}");
            } else {
                assert_eq!(argv, ["/bin/sh", "-c", "exit 0"], "{why}");
            }
        }

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

#[cfg(test)]
#[path = "exec_memory_tests.rs"]
mod memory_tests;
