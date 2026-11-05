// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! External tool runner for package builds.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Hang-detector deadline for external tool calls that could block indefinitely.
pub const HANG: Duration = Duration::from_secs(60);

/// One child to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    /// The program.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<OsString>,
    /// Its working directory (`None`: this process's).
    pub cwd: Option<PathBuf>,
    /// Start from an empty environment (`env -i`) before [`Run::env`] is applied.
    pub clear_env: bool,
    /// Variables removed from the inherited environment (before [`Run::env`] is applied).
    pub env_remove: Vec<String>,
    /// Variables set for it.
    pub env: Vec<(String, OsString)>,
    /// A hang detector: the runner owns termination and cleanup past it.
    pub deadline: Option<Duration>,
}

impl Run {
    /// `program args…`, inheriting this environment, unbounded.
    pub fn new<I, S>(program: impl Into<PathBuf>, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            cwd: None,
            clear_env: false,
            env_remove: Vec::new(),
            env: Vec::new(),
            deadline: None,
        }
    }

    /// The same, bounded by [`HANG`].
    #[must_use]
    pub const fn bounded(mut self) -> Self {
        self.deadline = Some(HANG);
        self
    }

    /// The arguments as text (for a fake's dispatch and a transcript).
    #[must_use]
    pub fn argv(&self) -> Vec<String> {
        self.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// The value it sets for `key`, if any.
    #[must_use]
    pub fn env_of(&self, key: &str) -> Option<&OsString> {
        self.env
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }
}

/// How a child ended.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ran {
    /// Its exit code; `None` when a signal ended it.
    pub code: Option<i32>,
    /// Its stdout.
    pub stdout: Vec<u8>,
    /// Its stderr.
    pub stderr: Vec<u8>,
}

impl Ran {
    /// Exit 0.
    #[must_use]
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// Whether the program actually executed.
    #[must_use]
    pub fn executed(&self) -> bool {
        self.code.is_some_and(|c| c < 126)
    }

    /// stdout then stderr, as text.
    #[must_use]
    pub fn text(&self) -> String {
        let mut s = String::from_utf8_lossy(&self.stdout).into_owned();
        s.push_str(&String::from_utf8_lossy(&self.stderr));
        s
    }
}

/// Why a child produced no [`Ran`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunError {
    /// Preparation or spawning failed before an owned child was returned.
    Spawn(String),
    /// A child was started but no usable result was established. It may have acted;
    /// this cause alone does not establish that cleanup or reaping completed.
    AfterStart(String),
    /// The collection deadline expired; cleanup follows the runner's contract.
    TimedOut,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "could not start: {e}"),
            Self::AfterStart(e) => write!(f, "failed after start: {e}"),
            Self::TimedOut => write!(f, "did not finish within {} s", HANG.as_secs()),
        }
    }
}

impl RunError {
    fn from_collector(error: aterm_uds::deadline::RunError) -> Self {
        use aterm_uds::deadline::RunError;
        match error {
            error @ RunError::Spawn(_) => Self::Spawn(error.to_string()),
            error @ (RunError::Capture(_) | RunError::Wait(_)) => {
                Self::AfterStart(error.to_string())
            }
            RunError::TimedOut => Self::TimedOut,
        }
    }

    fn from_complete_collector(error: aterm_uds::deadline::CompleteOutputError) -> Self {
        use aterm_uds::deadline::CompleteOutputError;
        match error {
            CompleteOutputError::Run(error) => Self::from_collector(error),
            error @ (CompleteOutputError::StdoutRead(_)
            | CompleteOutputError::StdoutTooLarge { .. }
            | CompleteOutputError::StdoutIncomplete) => Self::AfterStart(error.to_string()),
        }
    }
}

/// Every external program a pack runs.
pub trait Tools {
    /// Run a child to completion ([`RunError`]).
    /// # Errors
    /// [`RunError`] when preparation, spawning or post-start collection failed, or its
    /// deadline expired.
    fn run(&self, run: &Run) -> Result<Ran, RunError>;

    /// Run a metadata query whose stdout must be complete within `limit`, not a retained tail.
    /// Injected runners return complete `Ran` data; the native runner enforces EOF and size.
    /// Limits above the native hard maximum refuse before dispatch.
    /// # Errors
    /// Child refusal, deadline, incomplete stdout or the complete-output bound.
    fn run_complete(&self, run: &Run, limit: usize) -> Result<Ran, RunError> {
        if limit > aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES {
            return Err(RunError::Spawn(
                "complete stdout limit exceeds hard bound".into(),
            ));
        }
        let output = self.run(run)?;
        if output.stdout.len() > limit {
            return Err(RunError::AfterStart(
                "complete stdout exceeds its bound".into(),
            ));
        }
        Ok(output)
    }

    /// Drop symbols from one object in place.
    /// # Errors
    /// Why it failed.
    fn strip(&self, file: &Path, locals_and_debug: bool) -> Result<(), String>;

    /// Re-sign one Mach-O ad hoc (`codesign -f -s -`).
    /// # Errors
    /// Why it failed, or that the signing service did not answer.
    fn resign(&self, file: &Path) -> Result<(), String>;

    /// Whether two Mach-O binaries are identical after stripping code signatures.
    /// # Errors
    /// When the comparison could not be made.
    fn same_unsigned(&self, a: &Path, b: &Path) -> Result<bool, String>;

    /// Write the `arch` slice of a fat Mach-O to `out` (`lipo -thin`).
    /// # Errors
    /// Why it failed.
    fn thin(&self, file: &Path, arch: &str, out: &Path) -> Result<(), String>;

    /// Pack-time relocation (`crate::artifact::relocate::relocate_stage`, donors already admitted).
    /// # Errors
    /// The relocator's refusal.
    fn relocate(
        &self,
        stage: &Path,
        donors: &[PathBuf],
    ) -> Result<crate::artifact::relocate::Report, String>;

    /// What compresses the pack's tar stream.
    fn compressor(&self) -> &dyn super::tarwrite::Compressor;
}

/// A compiler's `-vV`, bounded — the probe the client's lineage checker takes.
/// # Errors
/// The text of why it did not answer.
pub fn version(tools: &dyn Tools, compiler: &Path) -> Result<String, String> {
    let ran = tools
        .run(&Run::new(compiler, ["-vV"]).bounded())
        .map_err(|e| format!("`{} -vV` {e}", compiler.display()))?;
    if ran.success() {
        Ok(String::from_utf8_lossy(&ran.stdout).into_owned())
    } else {
        Err(format!(
            "`{} -vV` exited {:?}: {}",
            compiler.display(),
            ran.code,
            String::from_utf8_lossy(&ran.stderr).trim()
        ))
    }
}

/// Host implementation of [`Tools`].
#[derive(Clone, Copy, Debug, Default)]
pub struct HostTools;

fn first_line(b: &[u8]) -> String {
    String::from_utf8_lossy(b)
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("no message")
        .to_string()
}

impl HostTools {
    fn checked(&self, run: &Run) -> Result<(), String> {
        let ran = self
            .run(run)
            .map_err(|e| format!("`{}` {e}", run.program.display()))?;
        if ran.success() {
            Ok(())
        } else {
            Err(format!(
                "`{} {}` failed: {}",
                run.program.display(),
                run.argv().join(" "),
                first_line(&ran.stderr)
            ))
        }
    }
}

fn host_command(run: &Run) -> aterm_uds::spawnfd::Command {
    let mut cmd = aterm_uds::spawnfd::command(&run.program);
    cmd.args(&run.args);
    if run.clear_env {
        cmd.env_clear();
    }
    for k in &run.env_remove {
        cmd.env_remove(k);
    }
    for (k, v) in &run.env {
        cmd.env(k, v);
    }
    if let Some(dir) = &run.cwd {
        cmd.current_dir(dir);
    }
    cmd
}

impl Tools for HostTools {
    fn run(&self, run: &Run) -> Result<Ran, RunError> {
        let mut cmd = host_command(run);
        let out = match run.deadline {
            Some(limit) => {
                let now = Instant::now();
                let until = now
                    .checked_add(limit)
                    .unwrap_or_else(|| now + Duration::from_secs(365 * 86_400));
                aterm_uds::deadline::output_until(&mut cmd, until)
                    .map_err(RunError::from_collector)?
            }
            None => {
                // Match Command::output's stdio and collection, keeping the owned-child
                // boundary visible: a capture or wait failure cannot mean no dispatch.
                let child = cmd
                    .stdin(aterm_uds::spawnfd::Stdio::null())
                    .stdout(aterm_uds::spawnfd::Stdio::piped())
                    .stderr(aterm_uds::spawnfd::Stdio::piped())
                    .spawn()
                    .map_err(|e| RunError::Spawn(e.to_string()))?;
                child
                    .wait_with_output()
                    .map_err(|e| RunError::AfterStart(e.to_string()))?
            }
        };
        Ok(Ran {
            code: out.status.code(),
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }

    fn run_complete(&self, run: &Run, limit: usize) -> Result<Ran, RunError> {
        let mut cmd = host_command(run);
        let out = aterm_uds::deadline::output_until_complete_stdout_with_limit(
            &mut cmd,
            Instant::now() + run.deadline.unwrap_or(HANG),
            limit,
        )
        .map_err(RunError::from_complete_collector)?;
        Ok(Ran {
            code: out.status.code(),
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }

    fn strip(&self, file: &Path, locals_and_debug: bool) -> Result<(), String> {
        let mut args: Vec<OsString> = Vec::new();
        if locals_and_debug {
            args.extend(["-x".into(), "-S".into()]);
        }
        args.push(file.into());
        self.checked(&Run::new(aterm_uds::os_path!("strip"), args).bounded())
    }

    fn resign(&self, file: &Path) -> Result<(), String> {
        let run = Run::new(
            aterm_uds::os_path!("codesign"),
            [OsString::from("-f"), "-s".into(), "-".into(), file.into()],
        )
        .bounded();
        match self.run(&run) {
            Err(RunError::TimedOut) => Err(format!(
                "macOS's code-signing service (trustd) did not answer within {} s re-signing \
                 {}; pack again once it answers",
                HANG.as_secs(),
                file.display()
            )),
            Err(e) => Err(format!("`codesign` {e}")),
            Ok(ran) if ran.success() => Ok(()),
            Ok(ran) => Err(format!(
                "`codesign -f -s - {}` failed: {}",
                file.display(),
                first_line(&ran.stderr)
            )),
        }
    }

    fn same_unsigned(&self, a: &Path, b: &Path) -> Result<bool, String> {
        let tmp = aterm_tempfile::Builder::new()
            .prefix("atpkg-same-program-")
            .tempdir()
            .map_err(|e| e.to_string())?;
        let (ca, cb) = (tmp.path().join("a"), tmp.path().join("b"));
        crate::identity::copy_regular_file(a, &ca).map_err(|e| e.to_string())?;
        crate::identity::copy_regular_file(b, &cb).map_err(|e| e.to_string())?;
        for c in [&ca, &cb] {
            self.checked(
                &Run::new(
                    aterm_uds::os_path!("codesign"),
                    [OsString::from("--remove-signature"), c.into()],
                )
                .bounded(),
            )?;
        }
        Ok(crate::identity::read_regular_bytes(&ca, 512 * 1024 * 1024)
            .map_err(|e| e.to_string())?
            == crate::identity::read_regular_bytes(&cb, 512 * 1024 * 1024)
                .map_err(|e| e.to_string())?)
    }

    fn thin(&self, file: &Path, arch: &str, out: &Path) -> Result<(), String> {
        self.checked(
            &Run::new(
                aterm_uds::os_path!("lipo"),
                [
                    file.as_os_str().to_owned(),
                    "-thin".into(),
                    arch.into(),
                    "-output".into(),
                    out.into(),
                ],
            )
            .bounded(),
        )
    }

    fn relocate(
        &self,
        stage: &Path,
        donors: &[PathBuf],
    ) -> Result<crate::artifact::relocate::Report, String> {
        crate::artifact::relocate::relocate_stage(stage, None, donors)
    }

    fn compressor(&self) -> &dyn super::tarwrite::Compressor {
        &super::tarwrite::Zstd
    }
}

#[cfg(test)]
mod tests {
    use super::RunError;
    use aterm_uds::deadline::{CompleteOutputError, RunError as CollectorError};

    #[test]
    fn collector_failures_keep_dispatch_phase_timeout_and_original_cause() {
        let cause = || std::io::Error::other("publisher-collector-marker");
        for (error, started) in [
            (CollectorError::Spawn(cause()), false),
            (CollectorError::Capture(cause()), true),
            (CollectorError::Wait(cause()), true),
        ] {
            let why = error.to_string();
            let mapped = RunError::from_collector(error);
            assert_eq!(
                mapped,
                if started {
                    RunError::AfterStart(why)
                } else {
                    RunError::Spawn(why)
                }
            );
        }
        assert_eq!(
            RunError::from_collector(CollectorError::TimedOut),
            RunError::TimedOut
        );
    }

    #[test]
    fn complete_collection_failure_keeps_dispatch_phase_and_timeout() {
        let error = CompleteOutputError::Run(CollectorError::Spawn(std::io::Error::other(
            "publisher-spawn-marker",
        )));
        let why = error.to_string();
        assert_eq!(
            RunError::from_complete_collector(error),
            RunError::Spawn(why)
        );
        assert_eq!(
            RunError::from_complete_collector(CompleteOutputError::Run(CollectorError::TimedOut)),
            RunError::TimedOut
        );
        for error in [
            CompleteOutputError::Run(CollectorError::Capture(std::io::Error::other(
                "publisher-capture-marker",
            ))),
            CompleteOutputError::Run(CollectorError::Wait(std::io::Error::other(
                "publisher-wait-marker",
            ))),
            CompleteOutputError::StdoutRead(std::io::Error::other("publisher-read-marker")),
            CompleteOutputError::StdoutTooLarge { limit: 3 },
            CompleteOutputError::StdoutIncomplete,
        ] {
            let why = error.to_string();
            assert_eq!(
                RunError::from_complete_collector(error),
                RunError::AfterStart(why)
            );
        }
    }
}
