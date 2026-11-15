// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The git program at a path, asked agh's [`NoLazyFetch::of`] as a caller that runs it by that
//! path runs it: hermetically ([`Hermetic`]), and once per file in a process ([`guard`]).
//! atpkg's build-cache retirement and aterm-release's local reads each run a git by its path and
//! ask here, so their question, its isolation and what a process remembers of the answer are
//! one. It lives here, not beside the question in agh, because agh starts a child only through
//! its effect seam (`agh::fx`, which a tape records), and these runs are local questions of a
//! program, not effects of agh's.

use agh::git::lazy_fetch::{Git, NoLazyFetch, Ran};
use aterm_types::MutexExt as _;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

/// The null device, as git's configuration path for none.
const NO_CONFIG: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

/// How long one of the question's runs may take, each a local git in a private scratch folder: a
/// hang detector, past which the question is unanswered rather than answered either way.
const RUN_BOUND: Duration = Duration::from_secs(60);

/// `cmd` without any `GIT_*` variable this process holds, in any case (Windows folds names), so
/// no inherited repository, object directory, config or trace setting reaches its git.
pub fn without_git_environment(cmd: &mut aterm_uds::spawnfd::Command) {
    remove_git_names(cmd, std::env::vars_os().map(|(name, _)| name));
}

/// `cmd` without each `GIT_*` name of `inherited`, in any case.
fn remove_git_names(
    cmd: &mut aterm_uds::spawnfd::Command,
    inherited: impl IntoIterator<Item = OsString>,
) {
    for name in inherited {
        if name
            .as_encoded_bytes()
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"GIT_"))
        {
            cmd.env_remove(name);
        }
    }
}

/// The git program at a path as the question asks it: with no `GIT_*` variable this process
/// holds ([`without_git_environment`]), then the program's own declared environment (a package's
/// `GIT_EXEC_PATH`, say: [`Hermetic::with_env`]); no system or global configuration, no prompt
/// or askpass program and no optional lock; its input empty, its output complete and bounded
/// ([`aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES`]) within [`RUN_BOUND`], and its scratch a
/// private folder of this process's.
#[derive(Clone, Copy, Debug)]
pub struct Hermetic<'a> {
    program: &'a Path,
    env: &'a [(String, String)],
}

impl<'a> Hermetic<'a> {
    /// The git at `program`, with no environment of its own.
    #[must_use]
    pub const fn new(program: &'a Path) -> Self {
        Self { program, env: &[] }
    }

    /// The git at `program` with `env` set after every inherited `GIT_*` variable is removed: the
    /// environment the program's package declares, which every run of it carries.
    #[must_use]
    pub const fn with_env(program: &'a Path, env: &'a [(String, String)]) -> Self {
        Self { program, env }
    }

    /// The program's path.
    #[must_use]
    pub const fn program(&self) -> &'a Path {
        self.program
    }
}

impl Hermetic<'_> {
    /// One run's command, `inherited` naming the variables this process holds: each `GIT_*` of
    /// them removed first, then the declared environment, the isolation every run carries and
    /// the run's own `env`, so a declared `GIT_*` survives the removal.
    fn command(
        &self,
        args: &[&OsStr],
        env: &[(&str, &str)],
        inherited: impl IntoIterator<Item = OsString>,
    ) -> aterm_uds::spawnfd::Command {
        let mut cmd = aterm_uds::spawnfd::command(self.program);
        remove_git_names(&mut cmd, inherited);
        cmd.envs(self.env.iter().map(|(key, value)| (key, value)))
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", NO_CONFIG)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "")
            .env("GIT_OPTIONAL_LOCKS", "0");
        for (key, value) in env {
            cmd.env(key, value);
        }
        cmd
    }
}

impl Git for Hermetic<'_> {
    fn run(&self, args: &[&OsStr], env: &[(&str, &str)]) -> Result<Ran, String> {
        let mut cmd = self.command(args, env, std::env::vars_os().map(|(name, _)| name));
        let output = aterm_uds::deadline::output_with_input_until(
            &mut cmd,
            aterm_uds::secret::Secret::new(b""),
            Instant::now() + RUN_BOUND,
            aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES,
        )
        .map_err(|error| format!("git did not run to completion: {error}"))?;
        Ok(Ran {
            status: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    fn scratch(&self) -> Result<Box<dyn AsRef<Path> + '_>, String> {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("atpkg-git-lazy-fetch-")
            .suffix(".noindex")
            .tempdir()
            .map_err(|error| format!("cannot make a scratch directory ({error})"))?;
        Ok(Box::new(scratch))
    }

    fn trace_file(&self, path: &Path) -> Result<(), String> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options
            .open(path)
            .map(drop)
            .map_err(|error| format!("cannot prepare trace ({error})"))
    }

    fn read_trace(&self, path: &Path, limit: usize) -> Result<Vec<u8>, String> {
        let file =
            std::fs::File::open(path).map_err(|error| format!("cannot read trace ({error})"))?;
        let regular = file
            .metadata()
            .map_err(|error| format!("cannot read trace ({error})"))?
            .is_file();
        if !regular {
            return Err("cannot read trace (not a regular file)".to_owned());
        }
        let mut bytes = Vec::new();
        file.take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| format!("cannot read trace ({error})"))?;
        if bytes.len() > limit {
            return Err(format!("cannot read trace (over {limit} bytes)"));
        }
        Ok(bytes)
    }
}

/// Why [`guard`] gives a git no guard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unguarded {
    /// The git neither takes `--no-lazy-fetch` nor obeys `GIT_NO_LAZY_FETCH` (its read fetched
    /// or tried to): a decided answer, remembered for the file. The text names the git and the
    /// gits that do keep a read local.
    Fetches(String),
    /// The question could not be asked ([`NoLazyFetch::of`]'s error), which shows nothing about
    /// the git: not remembered, so the next [`guard`] asks again.
    Unasked(String),
}

impl fmt::Display for Unguarded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fetches(why) | Self::Unasked(why) => f.write_str(why),
        }
    }
}

/// How `git`'s reads are kept from fetching lazily ([`NoLazyFetch::of`], asked of it run
/// [`Hermetic`]ally) the first time this process needs it for that file and environment, and
/// remembered for the process, so a caller asks once, not once per git run. The answer is
/// remembered for the file the path names ([`Identity`]), so a git replaced at that path is
/// asked again. Only a decided answer is remembered: a guard, or [`Unguarded::Fetches`]. A
/// question that could not be asked ([`Unguarded::Unasked`]) is asked again next time, so one
/// transient failure does not refuse every later read. The question runs with the memory
/// unlocked, so a slow or hanging git holds no other caller's answer; two callers that ask at
/// once both ask, and the first decided answer is the one kept.
///
/// # Errors
/// [`Unguarded::Fetches`]: the git neither takes `--no-lazy-fetch` nor was seen to obey
/// `GIT_NO_LAZY_FETCH`, so no read of it can be kept from fetching. [`Unguarded::Unasked`]: the
/// file the path names cannot be read, or the question could not be asked
/// ([`NoLazyFetch::of`]).
pub fn guard(git: &Hermetic<'_>) -> Result<NoLazyFetch, Unguarded> {
    static ASKED: Mutex<BTreeMap<Asked, Result<NoLazyFetch, String>>> = Mutex::new(BTreeMap::new());
    let key = (
        Identity::of(git.program).map_err(Unguarded::Unasked)?,
        git.env.to_vec(),
    );
    if let Some(answer) = ASKED.lock_or_recover().get(&key).cloned() {
        return answer.map_err(Unguarded::Fetches);
    }
    let answer = match NoLazyFetch::of(git) {
        Ok(Some(guard)) => Ok(guard),
        Ok(None) => Err(fetches(git)),
        Err(why) => {
            return Err(Unguarded::Unasked(format!(
                "{}: {why}",
                git.program.display()
            )));
        }
    };
    ASKED
        .lock_or_recover()
        .entry(key)
        .or_insert(answer)
        .clone()
        .map_err(Unguarded::Fetches)
}

/// The refusal of a git whose read fetched: the git (its `git version` line, where it prints
/// one) and the gits that keep a read local.
fn fetches(git: &Hermetic<'_>) -> String {
    let version = git
        .run(&[OsStr::new("--version")], &[])
        .ok()
        .filter(Ran::success)
        .map(|ran| ran.out().trim().to_owned())
        .filter(|line| !line.is_empty() && !line.contains('\n'));
    let named = match version {
        Some(version) => format!("{} ({version})", git.program.display()),
        None => git.program.display().to_string(),
    };
    format!(
        "{named} lets a read fetch an object a partial clone lacks: it neither takes \
         --no-lazy-fetch (git 2.45 and later) nor obeys GIT_NO_LAZY_FETCH=1 (the May 2024 \
         security releases 2.39.4, 2.40.2, 2.41.1, 2.42.2, 2.43.4 and 2.44.1, and distribution \
         builds that carry their fix for CVE-2024-32465), so its reads cannot be kept from \
         fetching"
    )
}

/// What one answer of [`guard`] is remembered for: the file a program names ([`Identity`]) and the
/// environment it is declared to run with ([`Hermetic::with_env`]).
type Asked = (Identity, Vec<(String, String)>);

/// The file a git program's path names now (a bare name as a shell finds it on `PATH`,
/// [`on_path`]): the path resolved, and the file's device and inode (on Unix), length and
/// modification time, so another file put at the same path is another identity.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Identity {
    path: PathBuf,
    file: (u64, u64),
    len: u64,
    modified: Option<SystemTime>,
}

impl Identity {
    /// The identity of the file `program` names.
    ///
    /// # Errors
    /// The path cannot be resolved (a bare name is on no `PATH` folder) or its file read.
    fn of(program: &Path) -> Result<Self, String> {
        let unread = |error: std::io::Error| format!("{}: {error}", program.display());
        let path = std::fs::canonicalize(on_path(program)?).map_err(unread)?;
        let meta = std::fs::metadata(&path).map_err(unread)?;
        #[cfg(unix)]
        let file = {
            use std::os::unix::fs::MetadataExt as _;
            (meta.dev(), meta.ino())
        };
        #[cfg(not(unix))]
        let file = (0, 0);
        Ok(Self {
            path,
            file,
            len: meta.len(),
            modified: meta.modified().ok(),
        })
    }
}

/// `program` where it names a folder; a bare name as a shell finds it: the first executable
/// file of that name in an absolute `PATH` folder, with `PATHEXT`'s extensions on Windows
/// ([`crate::vendor::first_on_path`]). On Windows a run of a bare name also looks in this
/// process's own folder and the system folders before `PATH`, so there the identity is the
/// shell's copy, which can differ from the one a run starts.
///
/// # Errors
/// A bare name that is not UTF-8, or that no `PATH` folder holds.
fn on_path(program: &Path) -> Result<PathBuf, String> {
    if program
        .parent()
        .is_none_or(|parent| !parent.as_os_str().is_empty())
    {
        return Ok(program.to_path_buf());
    }
    program
        .to_str()
        .and_then(|name| crate::vendor::first_on_path(name, std::env::var_os("PATH").as_deref()))
        .ok_or_else(|| format!("{}: no such program on PATH", program.display()))
}

#[cfg(all(test, unix))]
#[path = "git_lazy_fetch_tests.rs"]
mod tests;
