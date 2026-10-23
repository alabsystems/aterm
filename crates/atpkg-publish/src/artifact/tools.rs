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
    /// A hang detector: the child (and its process group) is killed past it.
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
    /// It could not be started.
    Spawn(String),
    /// It did not finish within its deadline (killed and reaped).
    TimedOut,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "could not start: {e}"),
            Self::TimedOut => write!(f, "did not finish within {} s", HANG.as_secs()),
        }
    }
}

/// Every external program a pack runs.
pub trait Tools {
    /// Run a child to completion ([`RunError`]).
    /// # Errors
    /// [`RunError`] when it could not start or outran its deadline.
    fn run(&self, run: &Run) -> Result<Ran, RunError>;

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

impl Tools for HostTools {
    fn run(&self, run: &Run) -> Result<Ran, RunError> {
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
        let out = match run.deadline {
            Some(limit) => aterm_uds::deadline::output_until(&mut cmd, Instant::now() + limit)
                .map_err(|e| match e {
                    aterm_uds::deadline::RunError::TimedOut => RunError::TimedOut,
                    other => RunError::Spawn(other.to_string()),
                })?,
            None => cmd
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .output()
                .map_err(|e| RunError::Spawn(e.to_string()))?,
        };
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
        std::fs::copy(a, &ca).map_err(|e| e.to_string())?;
        std::fs::copy(b, &cb).map_err(|e| e.to_string())?;
        for c in [&ca, &cb] {
            self.checked(
                &Run::new(
                    aterm_uds::os_path!("codesign"),
                    [OsString::from("--remove-signature"), c.into()],
                )
                .bounded(),
            )?;
        }
        Ok(std::fs::read(&ca).map_err(|e| e.to_string())?
            == std::fs::read(&cb).map_err(|e| e.to_string())?)
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
        static ON_PATH: std::sync::LazyLock<super::tarwrite::ZstdCli> =
            std::sync::LazyLock::new(super::tarwrite::ZstdCli::default);
        &*ON_PATH
    }
}
