// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How a git keeps a read from fetching an object a partial clone lacks ([`NoLazyFetch`]), told
//! by what the git does rather than by its version. One answer for every reader that must judge
//! only the objects a repository has: aterm-machines' checkout and dependency reads and atpkg's
//! build-cache cleanup each ask it of their own [`Git`], the program, account and environment
//! they run. This module starts no process itself.

use std::ffi::OsStr;
use std::path::Path;

mod trace;

/// The probe's small local-object trace is bounded before decoding.
const TRACE_LIMIT: usize = 256 * 1024;

/// The option git 2.45 and later take, which an older git refuses as unknown.
const OPTION: &str = "--no-lazy-fetch";

/// The question's configuration files: none ([`guarded_read`]).
const NO_CONFIG: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

/// Nothing else runs under a git for this one run (no configuration is written): no repository
/// hook (a hook directory that does not exist), no fsmonitor hook or daemon `core.fsmonitor`
/// names (a merge's index update and a status each start one; set empty, which every git reads
/// as none, where `false` names a hook program to git 2.16 to 2.35), and no automatic
/// maintenance. A merge or a fetch otherwise ends by starting `git maintenance run --auto
/// --detach`, whose fork can still hold the run's stdout in its process group when git exits,
/// and a drain that waits for complete output refuses the run's output.
pub const NO_SIDE_RUNS: [&str; 6] = [
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=",
    "-c",
    "maintenance.auto=false",
];

/// How a git keeps a read from fetching an object a partial clone lacks, beside the
/// `GIT_NO_LAZY_FETCH=1` every such read carries, which a child git inherits
/// ([`NoLazyFetch::of`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoLazyFetch {
    /// git takes `--no-lazy-fetch` (2.45 and later), which [`NoLazyFetch::option`] names.
    Flag,
    /// git refuses `--no-lazy-fetch` as unknown (a git before 2.45) and obeys the variable
    /// alone, as it was seen to ([`NoLazyFetch::of`]): the May 2024 security releases from
    /// 2.39.4 on, which distributions carry to older versions (Ubuntu 24.04's 2.43.0 is one).
    /// [`NoLazyFetch::option`] names no option such a git refuses.
    Variable,
}

impl NoLazyFetch {
    /// How `git` keeps a read from fetching lazily: [`NoLazyFetch::Flag`] where
    /// `git --no-lazy-fetch --version` succeeds; otherwise [`NoLazyFetch::Variable`] where git
    /// was seen to obey the variable ([`guarded_read`]: its protected read failed and a complete
    /// trace shows no fetch attempt); otherwise `None`, where git's read fetched or attempted to
    /// fetch the object. Only a git that refuses the option is asked more than the one
    /// `--version`.
    ///
    /// # Errors
    /// git could not be asked: a run did not start, finish or exit normally, the scratch folder
    /// its question needs could not be made, a setup step failed, its trace was unavailable or
    /// incomplete, or the local-fetch control did not establish the observation
    /// ([`guarded_read`]).
    pub fn of(git: &dyn Git) -> Result<Option<Self>, String> {
        let flag = git
            .run(&[OsStr::new(OPTION), OsStr::new("--version")], &[])
            .map_err(|_| "cannot ask Git whether it takes --no-lazy-fetch")?;
        if flag.status.is_none() {
            return Err(format!(
                "cannot ask Git whether it takes --no-lazy-fetch: {}",
                flag.describe()
            ));
        }
        if flag.success() {
            return Ok(Some(Self::Flag));
        }
        let shown = guarded_read(git, Self::Variable)?;
        Ok(
            (shown.read.status.is_some_and(|code| code != 0) && shown.no_fetch)
                .then_some(Self::Variable),
        )
    }

    /// The option a read under this guard passes before its subcommand: `--no-lazy-fetch` where
    /// git takes it ([`NoLazyFetch::Flag`]), none where git would refuse it
    /// ([`NoLazyFetch::Variable`]), whose read carries the variable alone.
    #[must_use]
    pub const fn option(self) -> Option<&'static str> {
        match self {
            Self::Flag => Some(OPTION),
            Self::Variable => None,
        }
    }
}

/// A git as [`NoLazyFetch::of`] asks it: the program, account and environment its caller runs,
/// and the private scratch folder its question needs.
pub trait Git {
    /// This git with `args` after the caller's own and `env` set over the caller's, run to
    /// completion with its whole standard output.
    ///
    /// # Errors
    /// The run did not start or did not finish.
    fn run(&self, args: &[&OsStr], env: &[(&str, &str)]) -> Result<Ran, String>;

    /// A private folder this git may write in, removed when the guard returned drops.
    ///
    /// # Errors
    /// Why the folder could not be made or given to this git.
    fn scratch(&self) -> Result<Box<dyn AsRef<Path> + '_>, String>;

    /// An empty private file at `path` in that folder, which this git may write.
    ///
    /// # Errors
    /// Why the file could not be made or given to this git.
    fn trace_file(&self, path: &Path) -> Result<(), String>;

    /// At most `limit` bytes of the private file at `path` this git wrote.
    ///
    /// # Errors
    /// Why the file could not be read whole, as this git's own, within `limit`.
    fn read_trace(&self, path: &Path, limit: usize) -> Result<Vec<u8>, String>;
}

/// What a finished run of a [`Git`] produced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ran {
    /// The exit status, `None` when a signal ended it.
    pub status: Option<i32>,
    /// Standard output.
    pub stdout: Vec<u8>,
    /// Standard error.
    pub stderr: Vec<u8>,
}

impl Ran {
    /// Exited 0.
    #[must_use]
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }

    /// Standard output as text (lossy).
    #[must_use]
    pub fn out(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// Standard error as text (lossy), trimmed.
    #[must_use]
    pub fn err(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_owned()
    }

    /// One line saying how the run ended: `exit 1: <the first line of stderr>`.
    #[must_use]
    pub fn describe(&self) -> String {
        let how = self
            .status
            .map_or_else(|| "killed by a signal".to_owned(), |s| format!("exit {s}"));
        let err = self.err();
        let first = err.lines().next().unwrap_or("");
        if first.is_empty() {
            how
        } else {
            format!("{how}: {first}")
        }
    }
}

/// What a protected read of an object only a private local promisor holds did
/// ([`guarded_read`]).
#[derive(Clone, Debug)]
pub struct GuardedRead {
    /// The protected read.
    pub read: Ran,
    /// A complete trace of that read shows no child or exec attempt, and the unguarded control
    /// read fetched the object and established a trace of the same format.
    pub no_fetch: bool,
}

/// The read that shows whether `git` under `guard` keeps from fetching, in the scratch folder
/// it gives ([`Git::scratch`]): two bare repositories, `promisor.git`, which holds one object
/// (the blob of its own `HEAD` file), and `partial.git`, which lacks it and names
/// `promisor.git` (a local path, so no fetch leaves this machine) as its promisor remote
/// (`-c remote.origin.promisor=true`); then `partial.git` asked for the object's type under
/// `GIT_NO_LAZY_FETCH=1` and `guard`'s option. A complete Trace2 session must show no child or
/// exec attempt. A second read of the same object with the variable disabled and no option
/// must fetch it and expose that attempt in a complete trace of the same format, so the same
/// object and trace path are seen to fetch. Both reads concern only this private local
/// promisor. Neither the system's nor the person's git config is read (`GIT_CONFIG_NOSYSTEM`,
/// `GIT_CONFIG_GLOBAL` the null device), so none of their settings (a `url.<base>.insteadOf`,
/// a `protocol.file.allow`) changes what the read does: the question is the program's. Every
/// run is in the C locale with automatic side runs off ([`NO_SIDE_RUNS`]). Trace files are
/// private, bounded, and removed with the scratch folder.
///
/// # Errors
/// The question could not be asked, so nothing shows what git's read would do: the scratch
/// folder could not be made, a run did not start or finish normally, or a step setting the
/// question up failed (where the temporary folder is full or unwritable, say), named with the
/// first line git said, or `hash-object` named no object. Missing, oversized or incomplete
/// traces and an unsuccessful local-fetch control also establish no capability and return an
/// error.
pub fn guarded_read(git: &dyn Git, guard: NoLazyFetch) -> Result<GuardedRead, String> {
    const WHY: &str = "cannot see whether Git obeys GIT_NO_LAZY_FETCH";
    // Held to the end, so the folder goes after every read in it.
    let held = git.scratch().map_err(|why| format!("{WHY}: {why}"))?;
    let scratch: &Path = (*held).as_ref();
    let promisor = scratch.join("promisor.git");
    let partial = scratch.join("partial.git");
    let run = |args: &[&OsStr], env: &[(&str, &str)]| -> Result<Ran, String> {
        let args: Vec<&OsStr> = NO_SIDE_RUNS
            .into_iter()
            .map(OsStr::new)
            .chain(args.iter().copied())
            .collect();
        let env: Vec<(&str, &str)> = [
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", NO_CONFIG),
            ("LC_ALL", "C"),
        ]
        .into_iter()
        .chain(env.iter().copied())
        .collect();
        git.run(&args, &env).map_err(|_| WHY.to_owned())
    };
    let set_up = |step: &str, args: &[&OsStr]| -> Result<Ran, String> {
        let ran = run(args, &[])?;
        if ran.success() {
            Ok(ran)
        } else {
            Err(format!("{WHY}: git {step} failed ({})", ran.describe()))
        }
    };
    let words = |words: &[&'static str]| {
        words
            .iter()
            .map(|word| OsStr::new(*word))
            .collect::<Vec<&'static OsStr>>()
    };
    for repository in [&promisor, &partial] {
        let mut init = words(&["init", "--bare", "--template=", "--quiet", "--"]);
        init.push(repository.as_os_str());
        set_up("init", &init)?;
    }
    let head = promisor.join("HEAD");
    let mut hash = vec![OsStr::new("--git-dir"), promisor.as_os_str()];
    hash.extend(words(&["hash-object", "-w", "--no-filters", "--"]));
    hash.push(head.as_os_str());
    let hashed = set_up("hash-object", &hash)?;
    let object = hashed.out();
    let object = object.trim();
    if !matches!(object.len(), 40 | 64) || !object.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{WHY}: git hash-object named no object"));
    }
    let mut url = std::ffi::OsString::from("remote.origin.url=");
    url.push(promisor.as_os_str());
    let read = |protected: bool, name: &str| -> Result<(Ran, std::path::PathBuf), String> {
        let path = scratch.join(name);
        let path_text = path
            .to_str()
            .ok_or_else(|| format!("{WHY}: non-UTF-8 trace path"))?;
        git.trace_file(&path)
            .map_err(|why| format!("{WHY}: {why}"))?;
        let mut args = Vec::new();
        if protected && let Some(option) = guard.option() {
            args.push(OsStr::new(option));
        }
        args.extend(words(&["-c", "remote.origin.promisor=true", "-c"]));
        args.extend([
            url.as_os_str(),
            OsStr::new("--git-dir"),
            partial.as_os_str(),
        ]);
        args.extend(words(&["cat-file", "-t"]));
        args.push(OsStr::new(object));
        let ran = run(
            &args,
            &[
                ("GIT_NO_LAZY_FETCH", if protected { "1" } else { "0" }),
                ("GIT_TRACE2_EVENT", path_text),
            ],
        )?;
        Ok((ran, path))
    };
    let observe = |path: &Path, code: i32| {
        let bytes = git
            .read_trace(path, TRACE_LIMIT)
            .map_err(|why| format!("{WHY}: {why}"))?;
        trace::observe(&bytes, object, code).map_err(|why| format!("{WHY}: {why}"))
    };
    let (subject, path) = read(true, "subject.trace")?;
    let code = subject.status.ok_or_else(|| {
        format!(
            "{WHY}: Git read did not exit normally ({})",
            subject.describe()
        )
    })?;
    if code == 0 {
        return Ok(GuardedRead {
            read: subject,
            no_fetch: false,
        });
    }
    let subject_trace = observe(&path, code)?;
    if subject_trace.attempted {
        return Ok(GuardedRead {
            read: subject,
            no_fetch: false,
        });
    }
    let (control, path) = read(false, "control.trace")?;
    if !control.success() || control.out().trim() != "blob" {
        return Err(format!(
            "{WHY}: local-fetch control failed ({})",
            control.describe()
        ));
    }
    let control_trace = observe(&path, 0)?;
    if !control_trace.attempted || control_trace.format != subject_trace.format {
        return Err(format!(
            "{WHY}: local-fetch control did not establish the trace"
        ));
    }
    Ok(GuardedRead {
        read: subject,
        no_fetch: true,
    })
}
