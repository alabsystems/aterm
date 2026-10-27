// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Running the stages' children.

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
    /// Extra environment on top of the inherited one, or the whole one under [`Cmd::exact_env`].
    pub envs: Vec<(OsString, OsString)>,
    pub capture: Capture,
    /// Spawn under utility QoS rather than the run's inherited tier.
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

    #[must_use]
    pub fn env(mut self, k: impl Into<OsString>, v: impl Into<OsString>) -> Self {
        self.envs.push((k.into(), v.into()));
        self
    }

    #[must_use]
    pub const fn capture(mut self, c: Capture) -> Self {
        self.capture = c;
        self
    }

    /// Run this child at utility QoS (`taskpolicy -c utility`) where macOS has it.
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
    /// hang its ceiling still ends.
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

/// The invariant part of every invocation.
#[derive(Clone, Copy, Debug)]
pub struct ExecEnv<'a> {
    pub cwd: &'a Path,
    pub path: &'a OsStr,
    pub scratch: &'a Path,
    /// The ceiling on one child's work, or `None` to wait forever: the time a wait of its tree
    /// for admission or a lock is marked ([`aterm_toolchain::waits`]) is left out.
    pub child_ceiling: Option<Duration>,
    /// Variables removed from every child's inherited environment before [`Cmd::envs`].
    pub remove_env: &'a [&'a str],
    /// Variables added to every child after [`Self::remove_env`] and before [`Cmd::envs`].
    pub add_env: &'a [(OsString, OsString)],
    /// The run's record of its stage groups, when it keeps one ([`crate::runs`]).
    pub stage_groups: Option<&'a dyn StageGroupEnd>,
}

/// Process-group lifecycle hooks invoked while group custody ([`group::Live`]) holds the leader.
pub trait StageGroupEnd: Sync + std::fmt::Debug {
    /// Record a newly created stage process group.
    fn group_made(&self, pgid: u32, label: &str);
    /// Sweep and record a stage process group whose leader has exited but is not yet reaped.
    fn group_ended(&self, pgid: u32, label: &str);
}

/// What a child did.
#[derive(Clone, Debug)]
pub struct Run {
    /// True only on exit status 0.
    pub ok: bool,
    pub output: String,
    /// The exit code.
    pub code: Option<i32>,
    /// Set when the child could not be spawned at all (tool vanished mid-run).
    pub spawn_error: Option<String>,
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
}

/// Whether a failed child's log says the disk ran out: when it accounts for its failures as failed
/// tests, only if each one's own block names [`ENOSPC_SENTENCE`] (a test that passes may print a full
/// disk it simulated); otherwise anywhere, as a build or a tool that died says it. Passing test
/// binaries and ceiling kills are excluded ([`crate::libtest::out_of_disk`]).
fn out_of_disk(output: &str) -> bool {
    crate::libtest::out_of_disk(output, ENOSPC_SENTENCE)
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

/// Run an in-process walk on a thread of its own under the child ceiling, on the wall clock: no
/// walk waits for admission or a lock.
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

#[path = "exec_output.rs"]
mod output;

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
    }
    for (k, v) in &cmd.envs {
        c.env(k, v);
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

    let label = cmd.argv().join(" ");
    match &cmd.capture {
        Capture::Silent => {
            c.stdout(Stdio::null()).stderr(Stdio::null());
            finish(
                c,
                cmd,
                env.child_ceiling,
                None,
                env.stage_groups,
                &label,
                marks,
            )
            .0
        }
        Capture::Emit => {
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
            let (mut r, logged) = finish(
                c,
                cmd,
                env.child_ceiling,
                Some(&log),
                env.stage_groups,
                &label,
                marks,
            );
            r.output
                .insert_str(0, logged.as_deref().unwrap_or_default());
            std::fs::remove_file(&log).ok();
            r
        }
    }
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

/// A child's clock (`docs/DESIGN-adaptive-build-2026-10-24.md`, CEILING-WAIT): the time it
/// worked, which its ceiling bounds, apart from the time a wait of its tree for admission or a
/// lock was marked in `marks` ([`aterm_toolchain::waits`]), which nothing bounds. A wait's
/// liveness is its own: its holder named once a minute, and a dead holder lets its lock go. Each
/// span counts as the last look found it.
struct Clock<'a> {
    marks: Option<&'a Path>,
    last: Instant,
    looked: Option<Instant>,
    worked: Duration,
    waited: Duration,
    /// The waits the last look found.
    waits: Vec<aterm_toolchain::waits::Waiting>,
}

impl<'a> Clock<'a> {
    fn new(marks: Option<&'a Path>) -> Clock<'a> {
        Clock {
            marks,
            last: Instant::now(),
            looked: None,
            worked: Duration::ZERO,
            waited: Duration::ZERO,
            waits: Vec::new(),
        }
    }

    /// Count the span since the last tick, then look again once [`LOOK_EVERY`] has passed.
    fn tick(&mut self) {
        let now = Instant::now();
        let span = now.saturating_duration_since(self.last);
        if self.waits.is_empty() {
            self.worked += span;
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
            self.waits = waits;
        }
    }

    /// Why a child the run's end ended decided nothing: what it waited for, holder named, when
    /// the last look found it waiting ([`Clock::look`]).
    fn ended(&self) -> String {
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

/// Whether the run must end now: an interrupt is ending it.
fn ending() -> bool {
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
mod ends {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// The end's flag, and the flag set once a look of this thread's clock finds a wait.
    type Flags = (Arc<AtomicBool>, Arc<AtomicBool>);

    thread_local! {
        static END: std::cell::RefCell<Option<Flags>> = const { std::cell::RefCell::new(None) };
    }

    /// End this thread's run once `flag` is set, setting `seen` once its clock finds a wait.
    pub(super) fn when(flag: Arc<AtomicBool>, seen: Arc<AtomicBool>) {
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

/// Spawn `c` and reap it, ending it at `ceiling` of work ([`Clock`]: its tree's waits marked in
/// `marks` left out). Once the run is ending ([`ending`]) the interrupt's handler ends it, and its
/// row says what it waited for, if it waited.
fn finish(
    mut c: Command,
    cmd: &Cmd,
    ceiling: Option<Duration>,
    log: Option<&Path>,
    stage_groups: Option<&dyn StageGroupEnd>,
    label: &str,
    marks: Option<&Path>,
) -> (Run, Option<String>) {
    let read_log = |log: Option<&Path>| {
        log.map(|p| String::from_utf8_lossy(&std::fs::read(p).unwrap_or_default()).into_owned())
    };
    #[cfg(unix)]
    let sweep = |pgid: u32| {
        if let Some(groups) = stage_groups {
            groups.group_ended(pgid, label);
        }
    };
    #[cfg(not(unix))]
    let _ = (stage_groups, label);
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
    if let Some(groups) = stage_groups {
        groups.group_made(child.id(), label);
    }
    #[cfg(unix)]
    drop(spawning);

    let mut clock = Clock::new(marks);
    let mut told = false;
    let mut nap = Duration::from_millis(1);
    loop {
        #[cfg(unix)]
        let status = live.reap_stage_sweeping(&mut child, false, sweep);
        #[cfg(not(unix))]
        let status = child.try_wait();
        match status {
            Ok(Some(st)) => return (reaped(Ok(st)), read_log(log)),
            Ok(None) => {}
            Err(_) if ending() => {
                clock.look();
                return (failed_to_wait(&clock.ended()), read_log(log));
            }
            Err(e) => return (failed_to_wait(&e.to_string()), read_log(log)),
        }
        clock.tick();
        if ending() {
            // What its tree waits for as the run ends, said at once: the handler re-raises its
            // signal once its grace is up, before any report. The handler ends the child too (its
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
        } else if let Some(limit) = ceiling
            && clock.worked >= limit
        {
            let killed = kill_and_reap(
                &mut child,
                #[cfg(unix)]
                &mut live,
                cmd.term_grace,
            );
            return (over_ceiling(cmd, &clock, limit, &killed), read_log(log));
        }
        std::thread::sleep(nap);
        nap = (nap * 2).min(Duration::from_millis(25));
    }
}

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

fn over_ceiling(cmd: &Cmd, clock: &Clock<'_>, limit: Duration, kill: &str) -> Run {
    let argv = cmd.argv().join(" ");
    let waited = if clock.waited.is_zero() {
        String::new()
    } else {
        format!(
            "; it also waited {:.1}s for admission or a lock, which the ceiling leaves out",
            clock.waited.as_secs_f64()
        )
    };
    Run {
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
    }
}

/// Process-group custody and interrupt handling for stage children.
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
    const SIG_DFL: libc::sighandler_t = libc::SIG_DFL;

    const GRACE_ROUNDS: u32 = 200;
    const NAP_MS: c_long = 10;

    static LIVE: [AtomicI32; 256] = [const { AtomicI32::new(0) }; 256];

    static STOPPING: AtomicBool = AtomicBool::new(false);
    /// Set when a wait for capacity took the interrupt and [`stop_run`] ends the run.
    static WAIT_STOPPED: AtomicBool = AtomicBool::new(false);
    static SPAWNING: AtomicUsize = AtomicUsize::new(0);

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
                    "the run was interrupted while this child ran",
                ));
            }
            self.group = None;
            Ok(())
        }
        pub(crate) fn exited(
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
            self.reap_stage_sweeping(child, block, |_| {})
        }

        /// Reap `child`, running `sweep` once after the leader exits and before it is reaped (macOS
        /// and Linux only).
        pub(crate) fn reap_stage_sweeping(
            &mut self,
            child: &mut std::process::Child,
            block: bool,
            sweep: impl FnOnce(u32),
        ) -> std::io::Result<Option<std::process::ExitStatus>> {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            {
                if !self.exited(child, block)? {
                    return Ok(None);
                }
                if let Some(pgid) = self.group {
                    sweep(pgid);
                }
                self.release_for_reap(child.id())?;
                child.wait().map(Some)
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            {
                let _ = sweep;
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
            if WAIT_STOPPED.load(Ordering::SeqCst) {
                // The run is ending on a wait's interrupt ([`stop_run`]); another ends it now.
                // SAFETY: both take integers only.
                unsafe {
                    signal(sig, SIG_DFL);
                    reraise(sig);
                }
            }
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
            // SAFETY: `nanosleep` reads the struct we own and writes nothing when `rem` is null.
            unsafe {
                nanosleep(&raw const nap, std::ptr::null_mut());
            }
        }
        sweep(SIGKILL);
        // SAFETY: both take integers only.
        unsafe {
            signal(sig, SIG_DFL);
            reraise(sig);
        }
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

    /// End the run as [`kill_on_interrupt`]'s handler would, but without dying of it: a wait for
    /// capacity took the interrupt (`aterm_toolchain::interrupt`) and named what it waited for.
    /// No stage child starts after this, every live one's group is ended (SIGTERM, a grace, then
    /// SIGKILL) and reads as interrupted, not as a finding (`Live::release_for_reap`), no stage
    /// starts ([`stopping`]), and the run goes on to report what it has; a further interrupt
    /// ends it at once.
    pub fn stop_run() {
        WAIT_STOPPED.store(true, Ordering::SeqCst);
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

    /// On `SIGINT`, `SIGTERM` or `SIGHUP`, end every live stage child's group.
    pub fn kill_on_interrupt() {
        if !cfg!(any(target_os = "macos", target_os = "linux")) {
            return;
        }
        for sig in [SIGHUP, SIGINT, SIGTERM] {
            // SAFETY: `on_signal` is an `extern "C" fn(i32)` that lives for the whole program and
            // does only async-signal-safe work.
            unsafe {
                signal(sig, on_signal as extern "C" fn(i32) as usize);
            }
        }
    }

    /// How many stage groups are listed now.
    #[cfg(test)]
    pub(crate) fn live_count() -> usize {
        LIVE.iter()
            .filter(|slot| slot.load(Ordering::SeqCst) != 0)
            .count()
    }
}

fn reaped(st: io::Result<std::process::ExitStatus>) -> Run {
    match st {
        Ok(st) => Run {
            ok: st.success(),
            output: ended_by_signal(st),
            code: st.code(),
            spawn_error: None,
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

/// Run a child and return its merged stdout and stderr with trailing newlines stripped.
#[must_use]
pub fn capture_reply(cmd: &Cmd, env: ExecEnv<'_>) -> String {
    let r = run(&cmd.clone().capture(Capture::Emit), env);
    r.trimmed_output().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
                ok: false,
                output: String::new(),
                code: None,
                spawn_error: None,
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

    /// Run by the ceiling tests as a stage child: waits for the lock [`WAIT_LOCK`] names, as a lab
    /// waits for its lane, says when it has it, then works until it is ended. A `SIGTERM` ends its
    /// wait as a lab's interrupt does: NOT RUN, naming the holder, and exit 3.
    #[cfg(unix)]
    #[test]
    fn lock_waiter_stand_in() {
        use aterm_toolchain::named_lock::{Holder, Refused, Wait, take};
        let Some(path) = std::env::var_os(WAIT_LOCK).map(PathBuf::from) else {
            return;
        };
        // SAFETY: `stand_in_stop` is an `extern "C" fn(i32)` that lives for the whole program and
        // only stores to an atomic, which is async-signal-safe.
        unsafe {
            libc::signal(libc::SIGTERM, stand_in_stop as extern "C" fn(i32) as usize);
        }
        let me = Holder::me("lab linux exec", Path::new("/checkouts/waiter"));
        let taken = take(&path, &me, Wait::Block, &mut |_, _| {}, &|| {
            STAND_IN_STOPPED.load(Ordering::SeqCst)
        });
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
        loop {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// A stand-in holder's lock at `dir/lane.lock`, and a stage child that waits on it.
    #[cfg(unix)]
    fn held_lane(dir: &Path) -> (aterm_toolchain::named_lock::NamedLock, Cmd) {
        let lock = dir.join("lane.lock");
        let held = aterm_toolchain::named_lock::take(
            &lock,
            &aterm_toolchain::named_lock::Holder::me(
                "lab linux proof",
                Path::new("/checkouts/holder"),
            ),
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
    /// detector.
    #[cfg(unix)]
    fn marked(scratch: &Path) -> aterm_toolchain::waits::Waiting {
        let deadline = Instant::now() + HANG_DETECTOR;
        loop {
            let found = std::fs::read_dir(scratch)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("waits."))
                .find_map(|e| aterm_toolchain::waits::waiting(&e.path()).pop());
            if let Some(w) = found {
                return w;
            }
            assert!(Instant::now() < deadline, "the child never marked its wait");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// CEILING-WAIT's acceptance: a stage child waits on a lock its holder keeps for longer than
    /// the child's ceiling, and the ceiling does not end it while it waits; once it has the lock,
    /// the ceiling runs, and ends it at that much work, saying how long it waited apart. Negative
    /// control: timed from its spawn, as before, the ceiling ends it while it waits, never
    /// admitted, with no status.
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
        let wait = marked(&tmp);
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
    /// wait its clock last saw; and the Linux lane's row reads NOT RUN with it. Negative controls:
    /// a look after the end that forgets what the last one found reads `the run was interrupted
    /// while this child ran`; a run that ends the child itself cuts the child's NOT RUN.
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
        let (outcome, label) = crate::stages::linux_lane_outcome(out.code, &out.output);
        let mut r = crate::Report::new("linux");
        assert!(r.child_could_not_run(&out, &label), "{outcome:?}");
        let row = r.render();
        assert!(row.contains("NOT RUN") && row.contains(&wait.text), "{row}");
        std::fs::remove_dir_all(&tmp).ok();
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
