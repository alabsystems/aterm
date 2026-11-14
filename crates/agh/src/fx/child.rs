// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The one place agh spawns a child (§1, rule 4): `aterm_uds::spawnfd::command`, bounded by
//! `aterm_uds::deadline::output_until`.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::deadline::{self, RunError};
use aterm_uds::spawnfd::{self, Tether as _};

use crate::error::{Error, Result, SideEffect};

/// A child's bound when nothing names another.
const DEFAULT_BUDGET: Duration = Duration::from_secs(60);

/// Maximum complete record spool, shared by the native writer and its consumer.
#[cfg(unix)]
pub(crate) const MAX_RECORD_SPOOL_BYTES: usize = deadline::MAX_COMPLETE_LISTING_BYTES;

/// One child.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChildSpec {
    /// The program, absolute wherever a child runs: WP8's `gh` is one of `GH_ABSOLUTE`, given
    /// as `Path::new(GH_ABSOLUTE[i])`, and WP7's runner resolves `git` to `{PATH entry}/git`
    /// first (§4.6.13, step 7). A replaying git run names the bare `git`, which nothing runs
    /// (`Fx::git` answers from the tape). `run` refuses a relative program as could_not_run,
    /// why `{program} is not an absolute path`, before anything is spawned.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<OsString>,
    /// Its whole environment: `run` calls `env_clear()` and then `envs(..)` on the
    /// `spawnfd::Command` that `spawnfd::command` returns, so nothing is inherited.
    /// (`env_exactly` is `spawnfd::LaneCommand`'s, which agh does not use.)
    pub env: Vec<(OsString, OsString)>,
    /// Its working directory.
    pub cwd: Option<PathBuf>,
    /// Its wall bound, used only when [`Self::progress`] is absent.
    pub budget: Duration,
    /// A progress inactivity bound and the safe operation it is waiting on. When present,
    /// execution has no wall-clock deadline: output or descendant CPU/I/O keeps it live.
    pub progress: Option<ProgressBound>,
    /// Explicit owned input; never the caller's ambient stdin. Sensitive Git
    /// configuration uses `config_input`, which withholds parser stderr.
    pub(crate) input: Option<Vec<u8>>,
    #[cfg(unix)]
    config_input: Option<ConfigQuery>,
    /// Complete newline records spooled to one new owned regular file. This
    /// changes storage, never the one-MiB diagnostic retention policy.
    #[cfg(unix)]
    pub(crate) stdout_records: Option<PathBuf>,
}

/// A child execution bound based on evidence of progress, not elapsed wall time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgressBound {
    /// The longest interval with neither output nor descendant CPU/I/O progress.
    pub window: Duration,
    /// A credential-free operation label for a stall diagnostic.
    pub waiting_on: String,
}

/// Only these fixed Git parsers may consume repository configuration input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(unix)]
pub(crate) enum ConfigQuery {
    Names { read_only: bool },
    CommitRead,
    WorktreeFlag,
}

#[cfg(unix)]
impl ConfigQuery {
    pub(crate) fn args(self) -> &'static [&'static str] {
        match self {
            Self::Names { .. } => &[
                "config",
                "--file",
                "-",
                "--no-includes",
                "--list",
                "--name-only",
                "-z",
            ],
            Self::CommitRead => &["config", "--file", "-", "--no-includes", "--list", "-z"],
            Self::WorktreeFlag => &[
                "config",
                "--file",
                "-",
                "--no-includes",
                "--type=bool",
                "--get",
                "extensions.worktreeconfig",
            ],
        }
    }
}

impl fmt::Debug for ChildSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChildSpec")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("env", &self.env)
            .field("cwd", &self.cwd)
            .field("budget", &self.budget)
            .field("progress", &self.progress)
            .field("input_bytes", &self.input.as_ref().map(Vec::len))
            .finish()
    }
}

impl ChildSpec {
    /// `program` with no arguments, an empty environment, no directory and a 60 s bound.
    #[must_use]
    pub fn new(program: &Path) -> ChildSpec {
        ChildSpec {
            program: program.to_path_buf(),
            args: Vec::new(),
            env: Vec::new(),
            cwd: None,
            budget: DEFAULT_BUDGET,
            progress: None,
            input: None,
            #[cfg(unix)]
            config_input: None,
            #[cfg(unix)]
            stdout_records: None,
        }
    }

    /// The same child with `arg` appended.
    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> ChildSpec {
        self.args.push(arg.into());
        self
    }

    /// The same child with `NAME=value` in its environment.
    #[must_use]
    pub fn env(mut self, name: &str, value: impl Into<OsString>) -> ChildSpec {
        self.env.push((OsString::from(name), value.into()));
        self
    }

    /// The same child in `dir`.
    #[must_use]
    pub fn cwd(mut self, dir: &Path) -> ChildSpec {
        self.cwd = Some(dir.to_path_buf());
        self
    }

    /// The same child with another bound.
    #[must_use]
    pub fn budget(mut self, budget: Duration) -> ChildSpec {
        self.budget = budget;
        self
    }

    /// Use `window` as a native-progress inactivity bound, naming the safe operation
    /// `waiting_on` if it stalls.
    #[must_use]
    pub fn progressing(mut self, window: Duration, waiting_on: impl Into<String>) -> ChildSpec {
        self.progress = Some(ProgressBound {
            window,
            waiting_on: waiting_on.into(),
        });
        self
    }

    /// Attach explicit nonsecret input; `check` bounds it before dispatch.
    #[cfg(unix)]
    pub(crate) fn input(mut self, bytes: &[u8]) -> ChildSpec {
        self.input = Some(bytes.to_vec());
        self
    }

    /// Git configuration can contain secrets. Only use this for the runner's
    /// fixed parsers: their stdout is projected to a safe verdict or boolean,
    /// and their stderr is withheld before it reaches Fx recording or errors.
    #[cfg(unix)]
    pub(crate) fn config_input(mut self, bytes: &[u8], query: ConfigQuery) -> ChildSpec {
        self.input = Some(bytes.to_vec());
        self.config_input = Some(query);
        self
    }
}

/// What a child left: the last `aterm_uds::deadline::OUTPUT_KEEP` bytes of each stream. Its
/// `Debug` names the exit code and each stream's length, never the bytes: `gh auth token`'s
/// stdout is a token until its caller moves it into a `Secret`.
#[derive(Clone, PartialEq, Eq)]
pub struct ChildOut {
    /// Its exit code; `None` when a signal ended it.
    pub code: Option<i32>,
    /// stdout.
    pub stdout: Vec<u8>,
    /// stderr.
    pub stderr: Vec<u8>,
}

#[cfg(unix)]
impl ChildOut {
    /// Stream retention bound shared with consumers planning complete frames.
    pub(crate) const OUTPUT_KEEP: usize = deadline::OUTPUT_KEEP;

    /// A full retention window cannot prove that stdout is a complete inventory.
    pub(crate) fn stdout_may_be_truncated(&self) -> bool {
        self.stdout.len() >= Self::OUTPUT_KEEP
    }
}

impl fmt::Debug for ChildOut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ChildOut {{ code: {:?}, stdout: {} bytes, stderr: {} bytes }}",
            self.code,
            self.stdout.len(),
            self.stderr.len()
        )
    }
}

/// A test's scripted child.
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptedChild {
    /// The program the run must ask for.
    pub program: PathBuf,
    /// The arguments it must pass.
    pub args: Vec<String>,
    /// What it answers.
    pub out: ChildOut,
}

/// The refusals every door that runs or logs a spec shares (`run`, and the simulated arms of
/// `Fx::child` and `Fx::git`), before anything runs or is logged: a relative program or
/// explicit input above 1 MiB is could_not_run, `Sub::Local`.
///
/// # Errors
///
/// A relative program or oversized explicit input, as above.
pub(crate) fn check(spec: &ChildSpec) -> Result<()> {
    if spec
        .input
        .as_ref()
        .is_some_and(|input| input.len() > 1024 * 1024)
    {
        return Err(Error::local("child input exceeds 1 MiB"));
    }
    if spec.program.is_absolute() {
        Ok(())
    } else {
        Err(Error::local(format!(
            "{} is not an absolute path",
            spec.program.display()
        )))
    }
}

/// Run `spec` to completion or its bound: `check(spec)`, then `spawnfd::command(program)`,
/// `args`, `env_clear()` then `envs(env)`, and `current_dir(cwd)` when set, then
/// `deadline::output_until_from(&mut cmd, input, until)`, with `until` =
/// `Instant::now().checked_add(spec.budget)`, or the progress collector when
/// [`ChildSpec::progress`] is present. A wall budget past the clock is refused before anything
/// spawns. The progress arm has no wall deadline: output or complete descendant-tree CPU/I/O
/// advances its inactivity window. This runner attaches `/dev/null` unless explicit bounded
/// ref input is attached through a private data file.
/// Both paths pipe the output streams, keep the last `deadline::OUTPUT_KEEP` bytes of each, and
/// put the child at the head of a new process group (`process_group(0)`), which its
/// termination path targets.
///
/// # Errors
///
/// could_not_run, `Sub::Local`. Before anything is spawned: why `{program} is not an absolute
/// path`, oversized explicit input, `{program}'s budget {budget} is past what the clock holds`,
/// private input directory/write/open errors, or `{program} could not start: {e}`.
/// These pre-dispatch refusals carry `SideEffect::None`.
/// A capture failure after successful dispatch, wait failure or timeout carries
/// `SideEffect::Unknown`: a git push may already have reached its remote. Neither the
/// collector failure nor its termination attempt proves that nothing happened; its cleanup
/// contract is separate from dispatch classification. A caller that only reads remotely
/// (ls-remote, clone, `gh auth token`) may lower the side effect to none.
pub(crate) fn run(spec: &ChildSpec) -> Result<ChildOut> {
    run_unless(spec, &mut || false)
}

/// The same owned collector, with an operation owner's cooperative cancellation request.
/// An explicit stop before dispatch has no side effect; after dispatch the collector retires
/// its actual child/group before returning, while a remote write may already have happened.
pub(crate) fn run_unless(spec: &ChildSpec, stopped: &mut dyn FnMut() -> bool) -> Result<ChildOut> {
    check(spec)?;
    if stopped() {
        return Err(interrupted(SideEffect::None));
    }
    #[cfg(unix)]
    if let Some(query) = spec.config_input
        && !spec
            .args
            .iter()
            .map(|arg| arg.as_os_str())
            .eq(query.args().iter().map(std::ffi::OsStr::new))
    {
        return Err(Error::local(
            "sensitive configuration requires a fixed Git parser query",
        ));
    }
    let program = spec.program.display();
    #[cfg(target_os = "macos")]
    if aterm_uds::tether::lost() {
        return Err(Error::local(
            "the recorded Git lifecycle helper is unavailable; no child was started",
        ));
    }
    let until = if spec.progress.is_none() {
        Some(Instant::now().checked_add(spec.budget).ok_or_else(|| {
            Error::local(format!(
                "{program}'s budget {:?} is past what the clock holds",
                spec.budget
            ))
        })?)
    } else {
        None
    };
    // Keep the private input directory until the command drops its descriptor.
    let input_dir;
    let mut cmd = spawnfd::command(&spec.program);
    // A killed frontend cannot run its collector. Git ends none of its children
    // on a SIGTERM, so the tie ends the group this child leads (`process_group(0)`
    // in the collector): its watcher signals the whole group, which its anchor
    // holds as this child's (`aterm_uds::tether`), and stays for the group should
    // git exit first. A descendant that left the group is not reached. Where a
    // Linux tether cannot route (a `hidepid` `/proc`, or this process as pid 1),
    // or its watcher gets no pidfd (a kernel before 5.3, or a seccomp filter that
    // refuses `pidfd_open`), the parent-death signal reaches git alone.
    cmd.tether();
    cmd.args(&spec.args);
    cmd.env_clear();
    cmd.envs(spec.env.iter().map(|(name, value)| (name, value)));
    if let Some(dir) = &spec.cwd {
        cmd.current_dir(dir);
    }
    let input = if let Some(bytes) = &spec.input {
        input_dir = aterm_tempfile::Builder::new()
            .prefix("agh-input-")
            .tempdir()
            .map_err(|error| Error::local(format!("child input directory: {error}")))?;
        let path = input_dir.path().join("input");
        std::fs::write(&path, bytes)
            .map_err(|error| Error::local(format!("child input write: {error}")))?;
        let file = std::fs::File::open(path)
            .map_err(|error| Error::local(format!("child input open: {error}")))?;
        spawnfd::Stdio::from(file)
    } else {
        spawnfd::Stdio::null()
    };
    #[cfg(unix)]
    if let Some(path) = &spec.stdout_records {
        if spec.config_input.is_some() || spec.input.is_some() {
            return Err(Error::local(
                "record spool requires a fixed input-free query",
            ));
        }
        return run_record_spool(spec, &mut cmd, input, path, stopped);
    }
    let output = match &spec.progress {
        Some(progress) => deadline::output_from_while_process_progressing_unless(
            &mut cmd,
            input,
            progress.window,
            &progress.waiting_on,
            stopped,
        )
        .map_err(|error| {
            if stopped() {
                interrupted(SideEffect::Unknown)
            } else {
                classify_progress_failure(spec, error)
            }
        })?,
        None => deadline::output_until_from_unless(
            &mut cmd,
            input,
            until.expect("a wall-bound child has a deadline"),
            stopped,
        )
        .map_err(|error| {
            if stopped() {
                interrupted(SideEffect::Unknown)
            } else {
                classify_failure(spec, error)
            }
        })?,
    };
    let out = ChildOut {
        code: output.status.code(),
        stdout: output.stdout,
        stderr: output.stderr,
    };
    #[cfg(unix)]
    let out = config_output(spec, out);
    Ok(out)
}

/// This uses the same owning group collector and caller stop token. Complete
/// EOF is required; escaped descendants remain outside the collector contract.
#[cfg(unix)]
fn run_record_spool(
    spec: &ChildSpec,
    cmd: &mut spawnfd::Command,
    input: spawnfd::Stdio,
    path: &Path,
    stopped: &mut dyn FnMut() -> bool,
) -> Result<ChildOut> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::sync::{Arc, OnceLock};
    let parent = path
        .parent()
        .ok_or_else(|| Error::local("record spool has no parent"))?;
    aterm_uds::private_file::check_private_dir(parent)
        .map_err(|error| Error::local(format!("record spool directory: {error}")))?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| Error::local(format!("record spool: {error}")))?;
    let failure = Arc::new(OnceLock::<String>::new());
    let observed = failure.clone();
    let mut written = 0usize;
    let consume = move |record: &[u8]| {
        if observed.get().is_none() {
            let next = written.checked_add(record.len() + 1);
            if next.is_none_or(|n| n > MAX_RECORD_SPOOL_BYTES) {
                let _ = observed.set("record spool exceeds its finite byte bound".into());
            } else if let Err(error) = file.write_all(record).and_then(|_| file.write_all(b"\n")) {
                let _ = observed.set(format!("record spool write: {error}"));
            } else {
                written = next.unwrap();
            }
        }
        false // all inventory DATA is on disk, never retained in ChildOut's heap
    };
    let failed_sink = failure.clone();
    let mut stop_on_failure = || stopped() || failed_sink.get().is_some();
    let captured = match &spec.progress {
        Some(progress) => {
            deadline::output_while_process_progressing_complete_stdout_records_unless(
                cmd,
                progress.window,
                &progress.waiting_on,
                b'\n',
                42,
                consume,
                &mut stop_on_failure,
            )
            .map_err(|error| error.to_string())
        }
        None => deadline::output_within_complete_stdout_records_from_unless(
            cmd,
            input,
            spec.budget,
            b'\n',
            42,
            consume,
            &mut stop_on_failure,
        )
        .map_err(|error| error.to_string()),
    };
    drop(stop_on_failure);
    if let Some(why) = failure.get().cloned() {
        return Err(Error::local(why).with_side_effect(SideEffect::Unknown));
    }
    let output = captured.map_err(|why| {
        if stopped() {
            interrupted(SideEffect::Unknown)
        } else {
            Error::local(format!("complete record spool: {why}"))
                .with_side_effect(SideEffect::Unknown)
        }
    })?;
    Ok(ChildOut {
        code: output.status.code(),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

fn interrupted(effect: SideEffect) -> Error {
    Error::new(
        crate::error::Kind::Interrupted,
        "native Git operation interrupted",
    )
    .with_side_effect(effect)
}

#[cfg(unix)]
fn config_output(spec: &ChildSpec, mut out: ChildOut) -> ChildOut {
    if let Some(query) = spec.config_input {
        out.stdout = match query {
            ConfigQuery::Names { read_only } if out.code == Some(0) => {
                crate::git::project_config_names(&out.stdout, read_only)
            }
            ConfigQuery::CommitRead if out.code == Some(0) => {
                crate::git::project_commit_config(&out.stdout)
            }
            ConfigQuery::WorktreeFlag
                if out.code == Some(0)
                    && matches!(out.stdout.as_slice(), b"true\n" | b"false\n") =>
            {
                out.stdout
            }
            _ => Vec::new(),
        };
        if !out.stderr.is_empty() {
            out.stderr = b"configuration parser stderr withheld\n".to_vec();
        }
    }
    out
}

/// Dispatch classification is independent of the collector's cleanup contract.
fn classify_failure(spec: &ChildSpec, error: RunError) -> Error {
    let program = spec.program.display();
    match error {
        RunError::Spawn(e) => Error::local(format!("{program} could not start: {e}")),
        RunError::Capture(e) => Error::local(format!(
            "{program} was dispatched but output capture failed: {e}"
        ))
        .with_side_effect(SideEffect::Unknown),
        RunError::Wait(e) => Error::local(format!("{program} could not be waited for: {e}"))
            .with_side_effect(SideEffect::Unknown),
        RunError::TimedOut => {
            Error::local(format!("{program} did not finish in {:?}", spec.budget))
                .with_side_effect(SideEffect::Unknown)
        }
    }
}

fn classify_progress_failure(spec: &ChildSpec, error: deadline::ProgressError) -> Error {
    match error {
        deadline::ProgressError::Run(error) => classify_failure(spec, error),
        stalled @ deadline::ProgressError::Stalled { .. } => {
            Error::local(format!("{} {stalled}", spec.program.display()))
                .with_side_effect(SideEffect::Unknown)
        }
    }
}

#[cfg(test)]
#[path = "child_tests.rs"]
mod tests;
