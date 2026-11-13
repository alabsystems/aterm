// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The publication engine as a reader outside it judges it: the files of one of its commits,
//! never a checkout's working tree.
//!
//! - An explicit [`GIT_DIR_ENV`]/[`COMMIT_ENV`] pair pins local immutable reader objects.
//! - Otherwise `PUBLICATION_ENGINE` names a checkout, and its HEAD commit is the engine. A release hands
//!   each child its own engine this way, synced to the origin's main ([`engine_copy`]).
//! - Otherwise the engine is its origin's main, imported now ([`OriginMain`]). The origin is the
//!   one the release's own engine names, else `~/publication`'s. GitHub is reached only through
//!   the caller's native authority ([`Import`]): [`native`], agh through [`crate::remote`], for a
//!   reader outside a release, and the release's own identity in `ship release`. git here only
//!   reads what was imported, and never runs a transport.
//!
//! No checkout here is kept level with the origin: a release runs its own engine and no longer
//! moves `~/publication`. A working tree holds whatever a session last left in it, and the
//! engine itself refuses to stage or promote from a dirty checkout or from a HEAD that is not
//! its origin's main. So the readers here (the ledger of a cut, status or yank, the baseline and
//! registry of `export-scan`, the baseline `capture-privacy` holds captures to, the registry
//! `ship provision` reaches) read a commit, and say which.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::Command;

/// How a git child is run and waited for (a release forwards its interrupts to it).
pub type RunGit<'a> = &'a dyn Fn(&mut Command) -> std::io::Result<std::process::Output>;

/// Brings `origin`'s main into the scratch bare repository `at`, whose `origin` it is, read
/// through the caller's authority, and returns that commit; `by` is the engine checkout that
/// named the origin. Its objects are in `at` when it returns.
pub type Import<'a> = &'a dyn Fn(&Path, &Path) -> Result<String, String>;

/// The finite wall hang detector for a reader's local Git child, and the request component of
/// [`REMOTE_PATIENCE`]. Agh's import of the origin's main into an empty repository ordinarily
/// takes one to two seconds (1.19-1.76 s on m19, 2026-11-07).
pub const PATIENCE: Duration = Duration::from_secs(120);

/// An Agh child may legitimately be quiet through the shared policy's largest admitted
/// `Retry-After`, then spend [`PATIENCE`] making its next request. This remains an inactivity
/// window: output or descendant progress renews it, so it is not a transfer latency budget.
pub const REMOTE_PATIENCE: Duration =
    PATIENCE.saturating_add(aterm_types::http_retry::Policy::STANDARD.max_total_pause());

/// Runs a local Git `cmd` within [`PATIENCE`], its whole stdout kept up to
/// [`aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES`] (16 MiB) and refused past it, never cut
/// short, and ended once a verify run in this process is ending ([`crate::exec::ending`]: an
/// interrupt, or the halt at its first red), so a stalled local child never holds a stage or
/// outlives the run: the [`RunGit`] of a reader outside a release. Networked Agh import uses the
/// progress-aware [`bounded_remote`] instead. The largest file a reader here reads is the ledger,
/// 0.18 MB on 2026-11-07.
///
/// # Errors
/// git could not start, did not end in time, was ended, or printed more than the bound.
pub fn bounded(cmd: &mut Command) -> std::io::Result<std::process::Output> {
    aterm_uds::deadline::output_until_complete_stdout_with_limit_unless(
        cmd,
        Instant::now() + PATIENCE,
        aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES,
        &mut crate::exec::ending,
    )
    .map_err(|e| std::io::Error::other(e.to_string()))
}

/// Wait on one child of the native import. Local Git has the reader's finite wall hang detector;
/// Agh owns a network transfer whose retry pauses and useful transfer progress must not consume a
/// latency budget, so it has an inactivity detector instead. Both observe the verify run ending.
fn bounded_remote(
    tool: crate::remote::Tool,
    cmd: &mut Command,
) -> std::io::Result<std::process::Output> {
    bounded_remote_with(tool, cmd, bounded, |cmd| {
        aterm_uds::deadline::output_while_process_progressing_complete_stdout_unless(
            cmd,
            REMOTE_PATIENCE,
            "publication engine origin read",
            &mut crate::exec::ending,
        )
        .map_err(|e| std::io::Error::other(e.to_string()))
    })
}

fn bounded_remote_with<T>(
    tool: crate::remote::Tool,
    cmd: &mut Command,
    local: impl FnOnce(&mut Command) -> T,
    network: impl FnOnce(&mut Command) -> T,
) -> T {
    match tool {
        crate::remote::Tool::Git => local(cmd),
        crate::remote::Tool::Agh => network(cmd),
    }
}

/// `<git common dir>/aterm-release` of the clone at `repo`: its release journal, run claim and
/// publication engine ([`engine_copy`]); `<repo>/.git/aterm-release` where git names no common
/// directory.
#[must_use]
pub fn release_dir(repo: &Path) -> PathBuf {
    let mut cmd = git();
    cmd.arg("-C")
        .arg(repo)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"]);
    bounded(&mut cmd)
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|dir| dir.trim().to_string())
        .filter(|dir| !dir.is_empty())
        .map_or_else(|| repo.join(".git"), PathBuf::from)
        .join("aterm-release")
}

/// The release's own publication engine in its release directory `release` ([`release_dir`]):
/// the checkout `ship release` runs `pub` in, syncs to the engine's origin and reads the ledger
/// of, and hands every child as `PUBLICATION_ENGINE`. Only the run holding that directory's run
/// claim writes it: the engine refuses a stage whose own HEAD moved while it ran, and a checkout
/// anything else aligns (`~/publication`) moved under 0.172.0's stage on m3 (2026-11-07).
#[must_use]
pub fn engine_copy(release: &Path) -> PathBuf {
    release.join(aterm_tempfile::QUIET).join("engine")
}

/// What kept a reader from the engine's file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Nothing here names an engine: no `PUBLICATION_ENGINE`, and no checkout with an origin.
    Unnamed,
    /// The engine's commit could not be read: the fetch or git failed, or did not end in time.
    Unread,
    /// The commit holds no such file, or not as text.
    Absent,
}

/// Why a reader has no file of the engine, in words that name where it looked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: Kind,
    pub why: String,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.why)
    }
}

/// Exact local object repository selected for publication-policy readers.
pub const GIT_DIR_ENV: &str = "PUBLICATION_ENGINE_GIT_DIR";
/// Full immutable commit selected together with [`GIT_DIR_ENV`].
pub const COMMIT_ENV: &str = "PUBLICATION_ENGINE_COMMIT";

/// The engine a reader judges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Named {
    /// An explicitly pinned local repository and commit; neither HEAD nor a remote is read.
    Pinned { git_dir: PathBuf, commit: String },
    /// `PUBLICATION_ENGINE`'s checkout, read at its HEAD commit.
    Checkout(PathBuf),
    /// The engine's origin, read at its main.
    Origin {
        /// Its URL, as `by` records it.
        url: String,
        /// The engine checkout whose `origin` it is.
        by: PathBuf,
    },
}

/// The engine `var` (the environment) names for a reader in the clone at `repo`: the checkout
/// `PUBLICATION_ENGINE` names, else the origin of the clone's release engine ([`engine_copy`])
/// or, where a release made none, of `~/publication` (where a first release clones from).
///
/// # Errors
/// [`Kind::Unread`] for an incomplete or malformed explicit pin; [`Kind::Unnamed`] when no engine names an origin.
pub fn named(repo: &Path, var: &dyn Fn(&str) -> Option<OsString>) -> Result<Named, Error> {
    let directory = var(GIT_DIR_ENV);
    let commit = var(COMMIT_ENV);
    if directory.is_some() || commit.is_some() {
        let invalid = || {
            Error { kind: Kind::Unread, why: "publication engine pin requires a nonempty absolute Git directory and one full lowercase commit ID".into() }
        };
        let directory = directory
            .filter(|value| !value.is_empty())
            .ok_or_else(invalid)?;
        let git_dir = PathBuf::from(directory);
        let commit = commit
            .and_then(|value| value.into_string().ok())
            .filter(|value| aterm_types::land_trailer::object_id(value))
            .ok_or_else(invalid)?;
        if !git_dir.is_absolute() {
            return Err(invalid());
        }
        return Ok(Named::Pinned { git_dir, commit });
    }
    if let Some(dir) = var("PUBLICATION_ENGINE").filter(|v| !v.is_empty()) {
        return Ok(Named::Checkout(PathBuf::from(dir)));
    }
    let copy = engine_copy(&release_dir(repo));
    let home = var("HOME")
        .filter(|h| !h.is_empty())
        .map(|home| PathBuf::from(home).join("publication"));
    [Some(copy.clone()), home.clone()]
        .into_iter()
        .flatten()
        .filter(|engine| engine.join(".git").exists())
        .find_map(|engine| {
            let mut cmd = git();
            cmd.arg("-C")
                .arg(&engine)
                .args(["remote", "get-url", "origin"]);
            bounded(&mut cmd)
                .ok()
                .filter(|out| out.status.success())
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .map(|url| url.trim().to_string())
                .filter(|url| !url.is_empty())
                .map(|url| Named::Origin { url, by: engine })
        })
        .ok_or_else(|| Error {
            kind: Kind::Unnamed,
            why: format!(
                "no publication engine here names its origin: PUBLICATION_ENGINE is unset, and \
                 neither {} nor {} is a checkout with an origin",
                copy.display(),
                home.map_or_else(|| "~/publication".into(), |h| h.display().to_string()),
            ),
        })
}

/// One commit of the engine, read by a reader outside it ([`Commit::read`]).
pub struct Commit {
    at: At,
    sha: String,
    shown: String,
}

enum At {
    Pinned(PathBuf),
    Checkout(PathBuf),
    Origin(OriginMain),
}

impl Commit {
    /// The commit `named` is: its explicit local pin, the checkout's HEAD, or the origin's main brought in by `import`,
    /// each git child run by `run`.
    ///
    /// # Errors
    /// [`Kind::Unread`]: the pin is invalid or unavailable, the checkout has no HEAD commit, or the import failed, each in its own
    /// words and naming where.
    pub fn read(named: &Named, run: RunGit<'_>, import: Import<'_>) -> Result<Self, Error> {
        let unread = |why: String| Error {
            kind: Kind::Unread,
            why,
        };
        match named {
            Named::Pinned { git_dir, commit } => {
                if !git_dir.is_absolute() || !aterm_types::land_trailer::object_id(commit) {
                    return Err(unread("invalid publication engine pin".into()));
                }
                let resolved = in_pinned(
                    git_dir,
                    &[
                        "rev-parse",
                        "--verify",
                        "--end-of-options",
                        &format!("{commit}^{{commit}}"),
                    ],
                    run,
                )
                .map_err(|error| {
                    unread(format!(
                        "cannot read pinned publication engine commit {commit}: {error}"
                    ))
                })?;
                if resolved != format!("{commit}\n").as_bytes() {
                    return Err(unread(
                        "pinned publication engine object is not the exact commit".into(),
                    ));
                }
                let kind =
                    in_pinned(git_dir, &["cat-file", "-t", commit], run).map_err(|error| {
                        unread(format!(
                            "cannot inspect pinned publication engine commit: {error}"
                        ))
                    })?;
                if kind != b"commit\n" {
                    return Err(unread(
                        "pinned publication engine object is not a commit".into(),
                    ));
                }
                Ok(Self {
                    at: At::Pinned(git_dir.clone()),
                    sha: commit.clone(),
                    shown: format!("{} at pinned {}", git_dir.display(), short(commit)),
                })
            }
            Named::Checkout(dir) => {
                // `git -C` searches upward: a directory inside another repository would be read
                // as that repository's HEAD.
                if !dir.join(".git").exists() {
                    return Err(unread(format!(
                        "PUBLICATION_ENGINE names {}, which is no git checkout of its own (no \
                         .git)",
                        dir.display()
                    )));
                }
                let sha = in_checkout(dir, &["rev-parse", "--verify", "HEAD^{commit}"], run)
                    .map_err(|e| {
                        unread(format!(
                            "PUBLICATION_ENGINE names {}, whose HEAD commit git could not read \
                             ({e})",
                            dir.display()
                        ))
                    })?;
                let sha = utf8_line(sha);
                let shown = format!("{} at HEAD {}", dir.display(), short(&sha));
                Ok(Self {
                    at: At::Checkout(dir.clone()),
                    sha,
                    shown,
                })
            }
            Named::Origin { url, by } => {
                let main = OriginMain::fetch(url, by, run, import).map_err(|e| {
                    unread(format!(
                        "cannot read the main of the publication engine's origin {url} ({e}); \
                         check the network and this Mac's identity for it (agh whoami --for \
                         <owner>/<repo>)"
                    ))
                })?;
                let sha = main.commit.clone();
                let shown = format!("{url}'s main {}", short(&sha));
                Ok(Self {
                    at: At::Origin(main),
                    sha,
                    shown,
                })
            }
        }
    }

    /// The commit's full SHA.
    #[must_use]
    pub fn sha(&self) -> &str {
        &self.sha
    }

    /// Where it was read, as a reader names it (`<checkout> at HEAD <sha12>`, or `<origin>'s
    /// main <sha12>`).
    #[must_use]
    pub fn shown(&self) -> &str {
        &self.shown
    }

    /// The text of `path` in this commit, never in a working tree.
    ///
    /// # Errors
    /// [`Kind::Absent`]: the commit has no such file, or it is not UTF-8.
    pub fn text(&self, path: &str, run: RunGit<'_>) -> Result<String, Error> {
        let object = format!("{}:{path}", self.sha);
        let shown = &self.shown;
        let out = match &self.at {
            At::Pinned(dir) => in_pinned(dir, &["show", &object], run),
            At::Checkout(dir) => in_checkout(dir, &["show", &object], run),
            At::Origin(main) => main.git(&["show", &object], run, true),
        };
        let absent = |why: String| Error {
            kind: Kind::Absent,
            why: format!("{path} on {shown}: {why}"),
        };
        String::from_utf8(out.map_err(absent)?).map_err(|e| absent(e.to_string()))
    }
}

/// The main of a publication engine's `origin`, imported ([`Import`]) into a scratch bare
/// repository nothing else reads or writes: the engine as its origin holds it now. What a reader
/// with no synced engine of its own judges (a release before its sync step, a dry run, a cut,
/// status or yank run by hand, `export-scan`), never a checkout's working tree, which is as old
/// as whatever last moved it.
pub struct OriginMain {
    origin: String,
    scratch: aterm_tempfile::TempDir,
    commit: String,
}

impl OriginMain {
    /// Import `origin`'s main (named by the engine checkout `by`) through `import`, each local
    /// git child run by `run`.
    ///
    /// # Errors
    /// No scratch directory, the import's own words, or an import that left no such commit.
    pub fn fetch(
        origin: &str,
        by: &Path,
        run: RunGit<'_>,
        import: Import<'_>,
    ) -> std::result::Result<Self, String> {
        let scratch = aterm_tempfile::tempdir()
            .map_err(|e| format!("no scratch directory to import into: {e}"))?;
        let mut main = Self {
            origin: origin.to_owned(),
            scratch,
            commit: String::new(),
        };
        main.git(&["init", "--quiet", "--bare"], run, false)
            .map_err(|e| format!("git init of the scratch repository: {e}"))?;
        main.git(&["remote", "add", "origin", origin], run, true)
            .map_err(|e| format!("naming the scratch repository's origin: {e}"))?;
        let commit = import(&main.at(), by)?;
        main.commit = main
            .git(
                &[
                    "rev-parse",
                    "--verify",
                    "--end-of-options",
                    &format!("{commit}^{{commit}}"),
                ],
                run,
                true,
            )
            .map(utf8_line)
            .map_err(|e| format!("the import named {commit}, which it did not bring in ({e})"))?;
        Ok(main)
    }

    /// The private bare object repository. Its pathname is valid only while this snapshot lives.
    #[must_use]
    pub fn git_dir(&self) -> PathBuf {
        self.at()
    }

    /// The origin passed to the caller-authorized import of this snapshot.
    #[must_use]
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// The commit main was when it was imported.
    #[must_use]
    pub fn commit(&self) -> &str {
        &self.commit
    }

    /// The text of `path` in that commit.
    ///
    /// # Errors
    /// git's own words: no such file there, or not UTF-8.
    pub fn text(&self, path: &str, run: RunGit<'_>) -> std::result::Result<String, String> {
        let out = self.git(&["show", &format!("{}:{path}", self.commit)], run, true)?;
        String::from_utf8(out).map_err(|e| format!("{path}: {e}"))
    }

    fn at(&self) -> PathBuf {
        self.scratch.path().join("main.git")
    }

    fn git(
        &self,
        args: &[&str],
        run: RunGit<'_>,
        within: bool,
    ) -> std::result::Result<Vec<u8>, String> {
        let at = self.at();
        let mut cmd = git();
        // Only the import reaches a remote: a read here never fetches a missing object.
        cmd.args(["-c", "protocol.allow=never"]);
        if within {
            cmd.arg("--git-dir").arg(&at);
        }
        cmd.args(args);
        if !within {
            cmd.arg(&at);
        }
        output(&mut cmd, run)
    }
}

/// The [`Import`] of a reader outside a release: the origin `at` names, through
/// [`crate::remote::Remote::refresh_main`] (agh for GitHub, git's file transport for a local
/// repository), each child run by [`bounded`].
///
/// # Errors
/// The remote's own words: an origin that is neither GitHub nor a local path, or a failed read.
pub fn native(at: &Path, _by: &Path) -> Result<String, String> {
    let run = |tool, args: &[OsString]| {
        let mut cmd = crate::remote::command(at, &in_dir, tool, args)?;
        bounded_remote(tool, &mut cmd).map_err(|e| format!("{tool:?} remote operation: {e}"))
    };
    crate::remote::Remote::open(at, "origin", &run)?.refresh_main()
}

fn in_dir(root: &Path) -> Command {
    let mut cmd = git();
    cmd.arg("-C").arg(root);
    cmd
}

/// A git command that asks no one and that no inherited `GIT_DIR` (or other redirect) points
/// elsewhere.
fn git() -> Command {
    let mut cmd = crate::git_provider::command();
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    for var in crate::changed::GIT_REDIRECTS {
        cmd.env_remove(var);
    }
    cmd
}

fn in_checkout(dir: &Path, args: &[&str], run: RunGit<'_>) -> Result<Vec<u8>, String> {
    let mut cmd = git();
    cmd.arg("-C").arg(dir).args(args);
    output(&mut cmd, run)
}

fn output(cmd: &mut Command, run: RunGit<'_>) -> Result<Vec<u8>, String> {
    let out = run(cmd).map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn utf8_line(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).trim().to_string()
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;

/// Exact local reads ignore inherited Git configuration and cannot lazy-fetch or run a transport.
fn in_pinned(dir: &Path, args: &[&str], run: RunGit<'_>) -> Result<Vec<u8>, String> {
    let mut command = crate::git_provider::command();
    for (name, _) in std::env::vars_os() {
        if name
            .as_encoded_bytes()
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"GIT_"))
        {
            command.env_remove(name);
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args([
            "--no-replace-objects",
            "-c",
            "protocol.allow=never",
            "--git-dir",
        ])
        .arg(dir)
        .args(args);
    output(&mut command, run)
}
