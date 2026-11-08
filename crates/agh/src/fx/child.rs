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
use aterm_uds::spawnfd;

use crate::error::{Error, Result, SideEffect};

/// A child's bound when nothing names another.
const DEFAULT_BUDGET: Duration = Duration::from_secs(60);

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
    /// fixed name-only or boolean queries: their stdout contains no values,
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
    /// A full retention window cannot prove that stdout is a complete inventory.
    pub(crate) fn stdout_may_be_truncated(&self) -> bool {
        self.stdout.len() >= deadline::OUTPUT_KEEP
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
    check(spec)?;
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
    let output = match &spec.progress {
        Some(progress) => deadline::output_from_while_process_progressing(
            &mut cmd,
            input,
            progress.window,
            &progress.waiting_on,
        )
        .map_err(|error| classify_progress_failure(spec, error))?,
        None => deadline::output_until_from(
            &mut cmd,
            input,
            until.expect("a wall-bound child has a deadline"),
        )
        .map_err(|error| classify_failure(spec, error))?,
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

#[cfg(unix)]
fn config_output(spec: &ChildSpec, mut out: ChildOut) -> ChildOut {
    if let Some(query) = spec.config_input {
        out.stdout = match query {
            ConfigQuery::Names { read_only } if out.code == Some(0) => {
                crate::git::project_config_names(&out.stdout, read_only)
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
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use super::{
        ChildOut, ChildSpec, RunError, ScriptedChild, classify_failure, classify_progress_failure,
        run,
    };
    use crate::error::{Kind, SideEffect, Sub};
    use crate::fx::Fx;

    #[cfg(unix)]
    const ROUTED_ENV_CASE: &str = "AGH_ROUTED_CHILD_ENV_CASE";
    #[cfg(unix)]
    const ROUTED_ENV_MARKER: &str = "AGH_ROUTED_CHILD_ENV_MARKER";
    #[cfg(unix)]
    const ROUTED_ENV_TEST: &str =
        "fx::child::tests::a_routed_git_child_gets_only_its_explicit_environment";

    /// A child with routed Git's explicit authentication environment is actually re-executed
    /// beneath a parent holding both ambient token variables. It sees neither, and it sees
    /// prompting disabled. NEGATIVE CONTROL: removing `run`'s `env_clear` makes the probe
    /// inherit the planted variables and fail.
    #[cfg(unix)]
    #[test]
    fn a_routed_git_child_gets_only_its_explicit_environment() {
        match std::env::var(ROUTED_ENV_CASE).as_deref() {
            Ok("probe") => {
                assert!(std::env::var_os("GH_TOKEN").is_none());
                assert!(std::env::var_os("GITHUB_TOKEN").is_none());
                assert_eq!(
                    std::env::var_os("GIT_TERMINAL_PROMPT").as_deref(),
                    Some(std::ffi::OsStr::new("0"))
                );
                let marker = std::env::var_os(ROUTED_ENV_MARKER).expect("the marker path");
                std::fs::write(marker, b"exact environment").expect("write the marker");
                return;
            }
            Ok("parent") => {
                assert!(std::env::var_os("GH_TOKEN").is_some());
                assert!(std::env::var_os("GITHUB_TOKEN").is_some());
                let marker = std::env::var_os(ROUTED_ENV_MARKER).expect("the marker path");
                let exe = std::env::current_exe().expect("the test binary");
                let out = run(&ChildSpec::new(&exe)
                    .arg("--exact")
                    .arg(ROUTED_ENV_TEST)
                    .arg("--test-threads=1")
                    .arg("--nocapture")
                    .env(ROUTED_ENV_CASE, "probe")
                    .env(ROUTED_ENV_MARKER, marker)
                    .env("GIT_TERMINAL_PROMPT", "0"))
                .expect("the exact-environment probe ran");
                assert_eq!(out.code, Some(0), "{out:?}");
                return;
            }
            Err(_) => {}
            other => panic!("unknown routed child environment case {other:?}"),
        }

        let root = aterm_tempfile::tempdir().expect("scratch");
        let marker = root.path().join("exact-environment");
        let mut parent = aterm_uds::spawnfd::command(
            std::env::current_exe().expect("the test binary's own path"),
        );
        parent
            .args([
                "--exact",
                ROUTED_ENV_TEST,
                "--test-threads=1",
                "--nocapture",
            ])
            .env(ROUTED_ENV_CASE, "parent")
            .env(ROUTED_ENV_MARKER, &marker)
            .env("GH_TOKEN", "planted-dead-token")
            .env("GITHUB_TOKEN", "planted-dead-token");
        let out = aterm_uds::deadline::output_within(&mut parent, Duration::from_secs(60))
            .expect("the parent finished within the hang detector");
        assert!(out.status.success(), "the parent failed");
        assert_eq!(
            std::fs::read(marker).expect("the probe's marker"),
            b"exact environment"
        );
    }

    /// Successful dispatch permits effects even if the output reader never starts. The
    /// other collector failures keep their previous classifications and diagnostic stages.
    #[test]
    fn collector_failures_preserve_dispatch_and_side_effect_classification() {
        let spec = ChildSpec::new(Path::new("/fixture/git")).budget(Duration::from_secs(73));
        let cause = || std::io::Error::other("collector-classification-marker");
        for (error, effect, why) in [
            (
                RunError::Spawn(cause()),
                SideEffect::None,
                "/fixture/git could not start: collector-classification-marker",
            ),
            (
                RunError::Capture(cause()),
                SideEffect::Unknown,
                "/fixture/git was dispatched but output capture failed: collector-classification-marker",
            ),
            (
                RunError::Wait(cause()),
                SideEffect::Unknown,
                "/fixture/git could not be waited for: collector-classification-marker",
            ),
            (
                RunError::TimedOut,
                SideEffect::Unknown,
                "/fixture/git did not finish in 73s",
            ),
        ] {
            let data = classify_failure(&spec, error).into_data();
            assert_eq!(
                (data.kind, data.sub, data.side_effect),
                (Kind::CouldNotRun, Sub::Local, effect)
            );
            assert_eq!(data.why, why);
        }
    }

    /// A progress stall is a post-dispatch unknown-effect result, and its reason preserves both
    /// the evidence window and the credential-free native operation name.
    #[test]
    fn progress_stall_names_its_evidence_and_wait() {
        let spec = ChildSpec::new(Path::new("/fixture/git"));
        let error = aterm_uds::deadline::ProgressError::Stalled {
            window: Duration::from_secs(60),
            waiting_on: "git fetch-batch fixture/app, step fetch".into(),
        };
        let data = classify_progress_failure(&spec, error).into_data();
        assert_eq!(
            (data.kind, data.sub, data.side_effect),
            (Kind::CouldNotRun, Sub::Local, SideEffect::Unknown)
        );
        for words in [
            "no stdout/stderr bytes",
            "descendant tree made no CPU or I/O progress",
            "60s",
            "git fetch-batch fixture/app, step fetch",
        ] {
            assert!(data.why.contains(words), "{}", data.why);
        }
    }

    /// `Instant + Duration::MAX` panics; the run answers could_not_run instead, before anything
    /// is spawned, so nothing was written. The child is one that runs and exits 0 under a
    /// budget the clock holds (NEGATIVE CONTROL, first), so only the budget's refusal can make
    /// the second run fail.
    #[cfg(unix)]
    #[test]
    fn a_budget_past_the_clock_is_could_not_run() {
        let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
        let runs = ChildSpec::new(&sleep)
            .arg("0")
            .budget(Duration::from_secs(60));
        assert_eq!(run(&runs).expect("it runs").code, Some(0));
        let spec = ChildSpec::new(&sleep).arg("0").budget(Duration::MAX);
        let err = run(&spec).expect_err("refused").into_data();
        assert_eq!(
            (err.kind, err.sub, err.side_effect),
            (Kind::CouldNotRun, Sub::Local, SideEffect::None)
        );
    }

    #[cfg(unix)]
    #[test]
    fn explicit_child_input_is_bounded_and_never_inherited_or_debug_printed() {
        let cat = ChildSpec::new(Path::new("/bin/cat"));
        assert!(run(&cat).expect("null input").stdout.is_empty());
        let mut input = vec![b'x'; 128 * 1024];
        input.extend_from_slice(b"input-content-marker");
        let spec = cat.clone().input(&input);
        assert!(!format!("{spec:?}").contains("input-content-marker"));
        assert_eq!(
            run(&spec).expect("input larger than a pipe buffer").stdout,
            input
        );
        let oversized = cat.input(&vec![0; 1024 * 1024 + 1]);
        let error = run(&oversized).expect_err("bounded before spawn");
        assert_eq!(error.side_effect, SideEffect::None);
    }

    /// A child that started and was killed at its bound may have acted first (a git push may
    /// have reached its remote): could_not_run, with an unknown side effect. NEGATIVE CONTROL:
    /// the refusals before a spawn carry none (the tests beside this one).
    #[cfg(unix)]
    #[test]
    fn a_child_killed_at_its_bound_may_have_acted() {
        let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
        let spec = ChildSpec::new(&sleep)
            .arg("60")
            .budget(Duration::from_millis(200));
        let err = run(&spec).expect_err("killed at its bound").into_data();
        assert_eq!(
            (err.kind, err.sub, err.side_effect),
            (Kind::CouldNotRun, Sub::Local, SideEffect::Unknown)
        );
    }

    /// A child's `Debug` never holds its streams: `gh auth token`'s stdout is a token.
    #[test]
    fn a_childs_debug_names_its_stream_lengths_not_their_bytes() {
        let out = ChildOut {
            code: Some(0),
            stdout: b"agh-child-stdout-marker".to_vec(),
            stderr: b"agh-child-stderr-marker".to_vec(),
        };
        let shown = format!("{out:?} {:?}", Ok::<_, ()>(out.clone()));
        assert!(!shown.contains("marker"), "{shown}");
        assert!(shown.contains("23 bytes"), "{shown}");
    }

    /// `git` follows `child`'s rule: a spec `child::check` refuses is not logged.
    #[test]
    fn a_relative_git_is_refused_and_not_logged_under_simulation() {
        let fx = Fx::simulated(1_760_000_000);
        let spec = ChildSpec::new(Path::new("git")).arg("status");
        let err = fx.git(&spec, b"").expect_err("refused").into_data();
        assert_eq!(
            (err.kind, err.sub, err.side_effect),
            (Kind::CouldNotRun, Sub::Local, SideEffect::None)
        );
        assert!(fx.children_run().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn git_child_observation_preserves_config_input_exits_and_errors() {
        use super::ConfigQuery;
        use std::sync::{Arc, Mutex};
        use std::time::Instant;

        let git = aterm_uds::fixture_git::git();
        let mut base = ChildSpec::new(Path::new(git.get_program()));
        base.env.extend(
            git.get_envs()
                .filter_map(|(k, v)| v.map(|v| (k.to_owned(), v.to_owned()))),
        );
        let query = |bytes: &[u8], query: ConfigQuery| {
            let mut spec = base.clone().config_input(bytes, query);
            spec.args = query.args().iter().map(|arg| (*arg).into()).collect();
            spec
        };
        let specs = [
            base.clone().arg("--version"),
            query(
                b"[user]\nname = private-config-marker\n",
                ConfigQuery::Names { read_only: false },
            ),
            query(
                b"[core]\nrepositoryformatversion = 0\n",
                ConfigQuery::WorktreeFlag,
            ),
            query(
                b"[broken\nprivate-config-marker\n",
                ConfigQuery::Names { read_only: false },
            ),
        ];
        let spans = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&spans);
        let fx = Fx::simulated(1_760_000_000).with_git_child_observer_for_tests(move |span| {
            captured.lock().unwrap().push(span);
        });
        for (ordinal, spec) in specs.iter().enumerate() {
            let live = Fx::live().git(spec, b"").unwrap();
            let inactive = Fx::simulated(1_760_000_000).git(spec, b"").unwrap();
            let began = Instant::now();
            let observed = fx.git(spec, b"").unwrap();
            let finished = Instant::now();
            assert_eq!(observed, live);
            assert_eq!(observed, inactive);
            assert_eq!(observed.code == Some(0), ordinal < 2);
            assert!(!String::from_utf8_lossy(&observed.stdout).contains("private-config-marker"));
            assert!(!String::from_utf8_lossy(&observed.stderr).contains("private-config-marker"));
            let spans = spans.lock().unwrap();
            assert_eq!(spans.len(), ordinal + 1);
            let span = spans.last().unwrap();
            assert_eq!(
                (span.ordinal, span.code, span.error),
                (ordinal, observed.code, None)
            );
            assert!(
                began <= span.started && span.started <= span.finished && span.finished <= finished
            );
        }
        let refused = ChildSpec::new(Path::new("git")).arg("status");
        let error = fx.git(&refused, b"").unwrap_err();
        assert_eq!(error.side_effect, SideEffect::None);
        assert_eq!(spans.lock().unwrap().len(), 4);
        assert_eq!(fx.children_run().len(), 4);
        let root = aterm_tempfile::tempdir().unwrap();
        let generic = ChildSpec::new(&root.path().join("scripted-child"));
        let fx = fx.with_children(vec![ScriptedChild {
            program: generic.program.clone(),
            args: Vec::new(),
            out: ChildOut {
                code: Some(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        }]);
        fx.child(&generic).unwrap();
        assert_eq!(
            spans.lock().unwrap().len(),
            4,
            "other child door is not observed"
        );
        assert_eq!(fx.children_run().len(), 5);
        let absent = ChildSpec::new(&root.path().join("absent-git"));
        let live = Fx::live().git(&absent, b"").unwrap_err().into_data();
        let observed = fx.git(&absent, b"").unwrap_err().into_data();
        assert_eq!(observed, live);
        let spans = spans.lock().unwrap();
        assert_eq!(spans.len(), 5);
        assert_eq!(
            (spans[4].ordinal, spans[4].code, spans[4].error),
            (5, None, Some(Kind::CouldNotRun))
        );
        assert!(spans.windows(2).all(|p| p[0].finished <= p[1].started));
    }

    #[cfg(unix)]
    #[test]
    fn git_child_observation_refuses_other_modes_and_replacement() {
        use crate::fx::tape::{TapeEndpoints, TapeHeader, TapeMode};
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let root = aterm_tempfile::tempdir().unwrap();
        let path = root.path().join("record.atape");
        let header = TapeHeader {
            agh_version: crate::version().to_owned(),
            api_version: crate::API_VERSION.to_owned(),
            argv: vec!["fixture".into()],
            mode: TapeMode::Anonymous,
            endpoints: TapeEndpoints::Loopback,
        };
        let reject = |fx: Fx| {
            assert!(
                catch_unwind(AssertUnwindSafe(
                    || fx.with_git_child_observer_for_tests(|_| {})
                ))
                .is_err()
            )
        };
        reject(Fx::live());
        reject(Fx::record_for_tests(&path, &header).unwrap());
        reject(Fx::replay_for_tests(&path).unwrap().0);
        reject(
            Fx::rerecord_for_tests(&path, &root.path().join("new.atape"))
                .unwrap()
                .0,
        );
        reject(Fx::simulated(1).with_git_child_observer_for_tests(|_| {}));
    }

    #[test]
    fn a_relative_program_is_refused_under_simulation_as_it_is_live() {
        let spec = ChildSpec::new(Path::new("gh")).arg("auth");
        let fx = Fx::simulated(1_760_000_000).with_children(vec![ScriptedChild {
            program: Path::new("gh").to_path_buf(),
            args: vec!["auth".to_string()],
            out: ChildOut {
                code: Some(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        }]);
        let simulated = fx.child(&spec).expect_err("refused").into_data();
        let live = run(&spec).expect_err("refused").into_data();
        assert_eq!(simulated, live);
        assert_eq!(
            (live.kind, live.sub, live.side_effect),
            (Kind::CouldNotRun, Sub::Local, SideEffect::None)
        );
        assert!(fx.children_run().is_empty());
    }

    #[test]
    #[should_panic(expected = "Fx::with_children scripts only a simulated Fx")]
    fn scripting_children_on_a_live_fx_panics() {
        let _ = Fx::live().with_children(Vec::new());
    }
}
