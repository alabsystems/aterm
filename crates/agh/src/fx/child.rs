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

use crate::error::{Error, Result, SideEffect, Sub};

/// A child's bound when nothing names another.
const DEFAULT_BUDGET: Duration = Duration::from_secs(60);

/// One child.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// Its bound.
    pub budget: Duration,
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

/// could_not_run, `Sub::Local`, naming `program`.
fn local(why: String) -> Error {
    Error::could_not_run(why).with_sub(Sub::Local)
}

/// The refusal every door that runs or logs a spec shares (`run`, and the simulated arms of
/// `Fx::child` and `Fx::git`), before anything runs or is logged: a relative program is
/// could_not_run, `Sub::Local`, why `{program} is not an absolute path`.
///
/// # Errors
///
/// As above.
pub(crate) fn check(spec: &ChildSpec) -> Result<()> {
    if spec.program.is_absolute() {
        Ok(())
    } else {
        Err(local(format!(
            "{} is not an absolute path",
            spec.program.display()
        )))
    }
}

/// Run `spec` to completion or its bound: `check(spec)`, then `spawnfd::command(program)`,
/// `args`, `env_clear()` then `envs(env)`, and `current_dir(cwd)` when set, then
/// `deadline::output_until(&mut cmd, until)`, with `until` =
/// `Instant::now().checked_add(spec.budget)` (a budget past the clock is refused before
/// anything spawns). `output_until` takes an
/// `Instant`, not a `Duration`, so the deadline is computed here, inside `fx/`, the one place
/// census's agh-fx clause lets agh read a clock. `output_until` itself makes stdin
/// `/dev/null`, pipes both streams, keeps the last `deadline::OUTPUT_KEEP` bytes of each, and
/// puts the child at the head of a new process group (`process_group(0)`), which the bound
/// kills.
///
/// # Errors
///
/// could_not_run, `Sub::Local`. Before anything is spawned: why `{program} is not an absolute
/// path`, or `{program}'s budget {budget} is past what the clock holds`. After: `{program}
/// could not start: {e}` (it never ran, or its stream readers could not start and it was
/// killed at once), `{program} could not be waited for: {e}`, or `{program} did not finish in
/// {budget}`. The last two come from a child that started, was killed and was reaped, and it
/// may have acted first: a git push may have reached its remote, so they carry
/// `SideEffect::Unknown` and never read as not sent. Every earlier one carries
/// `SideEffect::None`. A caller that only reads (ls-remote, clone, `gh auth token`) may lower
/// the side effect to none.
pub(crate) fn run(spec: &ChildSpec) -> Result<ChildOut> {
    check(spec)?;
    let program = spec.program.display();
    let Some(until) = Instant::now().checked_add(spec.budget) else {
        return Err(local(format!(
            "{program}'s budget {:?} is past what the clock holds",
            spec.budget
        )));
    };
    let mut cmd = spawnfd::command(&spec.program);
    cmd.args(&spec.args);
    cmd.env_clear();
    cmd.envs(spec.env.iter().map(|(name, value)| (name, value)));
    if let Some(dir) = &spec.cwd {
        cmd.current_dir(dir);
    }
    match deadline::output_until(&mut cmd, until) {
        Ok(output) => Ok(ChildOut {
            code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        }),
        Err(RunError::Spawn(e)) => Err(local(format!("{program} could not start: {e}"))),
        Err(RunError::Wait(e)) => Err(local(format!("{program} could not be waited for: {e}"))
            .with_side_effect(SideEffect::Unknown)),
        Err(RunError::TimedOut) => Err(local(format!(
            "{program} did not finish in {:?}",
            spec.budget
        ))
        .with_side_effect(SideEffect::Unknown)),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    #[cfg(unix)]
    use std::time::Duration;

    use super::{ChildOut, ChildSpec, ScriptedChild, run};
    use crate::error::{Kind, SideEffect, Sub};
    use crate::fx::Fx;

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
