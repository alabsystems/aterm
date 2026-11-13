// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `update` and `status`: build origin/main in a tree of its own, package it
//! with that tree's `apps\aterm-win\build.ps1`, smoke the dist, point
//! `latest.json` at it and hand it to its own `install.ps1 -Staged`.
//!
//! Everything lives under `%LOCALAPPDATA%\aterm`: the build tree at
//! `build\src` (never the shared checkout), the dists at `dist\<commit12>`,
//! the pointer at `dist\latest.json`. A build is assembled in
//! `dist\<commit12>.building` and takes its name only once it is whole, so a
//! failed build leaves the installed copy, the staged build and the pointer
//! as they were, and the pointer never names a folder that is being filled.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::Stdio;

use crate::plan::{self, Head, Install, Plan};
use crate::pointer::{self, Build, Pointer, Smoke};
use crate::proc::{self, Place};
use crate::repo::{self, GIT_LOCAL, GIT_NETWORK, Repo, Source, git};
use crate::smoke::{self, Outcome, one_line};
use crate::{FAILED, NOT_RUN, PASS};

/// Where the loop keeps its files, all under one `%LOCALAPPDATA%`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// `%LOCALAPPDATA%`.
    pub local: PathBuf,
}

impl Layout {
    /// The layout under the `LOCALAPPDATA` of `env`.
    ///
    /// # Errors
    /// `env` has no `LOCALAPPDATA`, or it is not absolute.
    pub fn from_env(env: &[(String, String)]) -> Result<Self, String> {
        let local = crate::env::lookup(env, "LOCALAPPDATA")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .ok_or("the environment has no absolute LOCALAPPDATA")?;
        Ok(Self { local })
    }

    /// The build tree: a git worktree (or clone) checked out at origin/main.
    #[must_use]
    pub fn src(&self) -> PathBuf {
        self.local.join("aterm").join("build").join("src")
    }

    /// The copy of this tool the scheduled task runs.
    #[must_use]
    pub fn tool(&self) -> PathBuf {
        self.local
            .join("aterm")
            .join("build")
            .join("aterm-winship.exe")
    }

    /// The repository the build tree is a worktree of, recorded for the runs
    /// that are given no `--repo` (`crate::repo`).
    #[must_use]
    pub fn repo_record(&self) -> PathBuf {
        self.local.join("aterm").join("build").join("repo.txt")
    }

    /// The scheduled task's definition, as it was handed to `schtasks.exe`.
    #[must_use]
    pub fn task_xml(&self) -> PathBuf {
        self.local
            .join("aterm")
            .join("build")
            .join(format!("{}.xml", crate::schedule::TASK))
    }

    /// The folder of dists and the pointer.
    #[must_use]
    pub fn dist_root(&self) -> PathBuf {
        self.local.join("aterm").join("dist")
    }

    /// The dist folder of the commit whose first twelve hex digits are `commit`.
    #[must_use]
    pub fn dist(&self, commit: &str) -> PathBuf {
        self.dist_root().join(commit)
    }

    /// Where the dist of `commit` is assembled before it takes its name.
    #[must_use]
    pub fn building(&self, commit: &str) -> PathBuf {
        self.dist_root().join(format!("{commit}{}", plan::BUILDING))
    }

    /// Where the dist of `commit` that a rebuild replaces waits to be deleted.
    #[must_use]
    pub fn replaced(&self, commit: &str) -> PathBuf {
        self.dist_root().join(format!("{commit}{}", plan::REPLACED))
    }

    /// The staged-build pointer.
    #[must_use]
    pub fn latest(&self) -> PathBuf {
        self.dist_root().join("latest.json")
    }

    /// The smoke report kept beside a dist.
    #[must_use]
    pub fn smoke_report(&self, commit: &str) -> PathBuf {
        self.dist_root().join(format!("{commit}.smoke.txt"))
    }

    /// The log a scheduled run appends to.
    #[must_use]
    pub fn log(&self) -> PathBuf {
        self.dist_root().join("update.log")
    }

    /// Where `install.ps1` installs by default.
    #[must_use]
    pub fn installed(&self) -> PathBuf {
        self.local.join("Programs").join("aterm")
    }
}

/// Where a run's lines go: the terminal, and a log file when it has one.
pub struct Out {
    log: Option<std::fs::File>,
}

impl Out {
    /// Lines to the terminal only.
    #[must_use]
    pub const fn terminal() -> Self {
        Self { log: None }
    }

    /// Lines to the terminal and appended to `path`.
    ///
    /// # Errors
    /// `path` could not be opened to append.
    pub fn logged(path: &Path) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self { log: Some(log) })
    }

    /// One line.
    pub fn say(&mut self, text: &str) {
        println!("{text}");
        if let Some(log) = self.log.as_mut() {
            let _ = writeln!(log, "{} {text}", pointer::now_rfc3339());
        }
    }

    /// Where a child's output goes: the log when there is one (a scheduled run
    /// has no terminal), else this process's own stream.
    fn stdio(&self) -> Stdio {
        self.log
            .as_ref()
            .and_then(|log| log.try_clone().ok())
            .map_or_else(Stdio::inherit, Stdio::from)
    }
}

/// What `update` was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// The repository the build tree is a worktree of; decided at run time
    /// when `None` (`crate::repo::resolve`).
    pub repo: Option<PathBuf>,
    /// Stage only: do not run the dist's installer.
    pub no_install: bool,
    /// Let the installer restart running windows (`install.ps1 -Restart`).
    pub restart: bool,
    /// Build even when the pointer already names origin/main, and install over
    /// a newer installed build.
    pub force: bool,
}

/// Makes the build tree where there is none: a detached worktree of `repo` at
/// origin/main, so it shares `repo`'s objects and costs no second clone. Where
/// there is one, it must be a worktree of `repo`: a tree of another repository
/// is refused, never silently built in `repo`'s name.
pub(crate) fn ensure_tree(repo: &Repo, layout: &Layout, out: &mut Out) -> Result<(), String> {
    let src = layout.src();
    if src.join(".git").exists() {
        if repo.from == Source::Tree {
            return Ok(());
        }
        let owner = repo::main_checkout(&src).map_or_else(
            |_| "a repository that is gone".to_string(),
            |dir| dir.display().to_string(),
        );
        return match repo::same(&src, &repo.dir) {
            Ok(true) => Ok(()),
            Ok(false) => Err(format!(
                "the build tree {} is a worktree of {owner}, not of {} ({}); to switch, remove it (git -C \"{owner}\" worktree remove --force \"{}\") and run again",
                src.display(),
                repo.dir.display(),
                repo.from.word(),
                src.display()
            )),
            Err(e) => Err(format!(
                "the build tree {} (a worktree of {owner}) could not be compared with {} ({}): {e}",
                src.display(),
                repo.dir.display(),
                repo.from.word()
            )),
        };
    }
    if let Some(dir) = src.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    out.say(&format!(
        "update: making the build tree {} from {} ({})",
        src.display(),
        repo.dir.display(),
        repo.from.word()
    ));
    git(
        &repo.dir,
        &["fetch", "--no-recurse-submodules", "origin", "main"],
        GIT_NETWORK,
    )?;
    // A tree deleted by hand leaves its registration behind. --force lets the
    // add take that one path back; the repository's other worktrees, missing
    // or not, are never pruned.
    let to = src.to_string_lossy();
    git(
        &repo.dir,
        // No submodule here: under the repository's `submodule.recurse` the add
        // leaves vendor/astream half made (a gitfile and a module folder with
        // only its config), which `submodule update` cannot take a handle on.
        // `stage` makes it whole with `submodule update --init`.
        &[
            "-c",
            "submodule.recurse=false",
            "worktree",
            "add",
            "--force",
            "--detach",
            &to,
            "origin/main",
        ],
        GIT_NETWORK,
    )?;
    // The scheduled copy is given no working directory to find it from.
    repo::record(layout, &repo.dir)
}

/// PowerShell 7 where it is on `PATH`, else Windows PowerShell.
fn powershell(env: &[(String, String)]) -> PathBuf {
    crate::env::lookup(env, "PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(path).collect::<Vec<_>>())
        .map(|dir| dir.join("pwsh.exe"))
        .find(|exe| exe.is_file())
        .unwrap_or_else(|| PathBuf::from("powershell.exe"))
}

/// How a script ended, for a person.
fn exited(code: Option<i32>) -> String {
    code.map_or_else(
        || "was ended without an exit code".to_string(),
        |c| format!("exited {c}"),
    )
}

/// Runs a PowerShell script of the build tree or the dist to its end, its
/// output going where `out` says.
fn script(
    shell: &Path,
    file: &Path,
    args: &[&std::ffi::OsStr],
    cwd: &Path,
    out: &Out,
) -> Result<Option<i32>, String> {
    let mut all: Vec<&std::ffi::OsStr> = vec![
        "-NoProfile".as_ref(),
        "-NonInteractive".as_ref(),
        "-ExecutionPolicy".as_ref(),
        "Bypass".as_ref(),
        "-File".as_ref(),
        file.as_os_str(),
    ];
    all.extend(args);
    let mut cmd = proc::command(
        shell,
        &all,
        Place {
            env: None,
            cwd: Some(cwd),
        },
    );
    cmd.stdin(Stdio::null())
        .stdout(out.stdio())
        .stderr(out.stdio());
    cmd.status()
        .map(|status| status.code())
        .map_err(|e| format!("{}: {e}", shell.display()))
}

/// Deletes the dists retention names and their smoke reports, and what an
/// interrupted run left half-made. Only names of those shapes under the dist
/// root are ever removed.
fn sweep(layout: &Layout, keep: &[&str], out: &mut Out) {
    let existing: Vec<String> = std::fs::read_dir(layout.dist_root())
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    for name in plan::retention(&existing, keep) {
        match std::fs::remove_dir_all(layout.dist(name)) {
            Ok(()) => out.say(&format!("update: removed the old dist {name}")),
            Err(e) => out.say(&format!(
                "update: could not remove the old dist {name}: {e}"
            )),
        }
        let _ = std::fs::remove_file(layout.smoke_report(name));
    }
    // This run holds the update lock, so nothing is filling or leaving these.
    for name in plan::leftovers(&existing) {
        if let Err(e) = std::fs::remove_dir_all(layout.dist_root().join(name)) {
            out.say(&format!(
                "update: could not remove the leftover {name}: {e}"
            ));
        }
    }
}

fn remove_dir_if_there(dir: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {e}", dir.display())),
    }
}

/// Gives the finished folder `building` the name `dist`. A dist already of
/// that name is moved aside to `replaced` first (Windows renames no folder
/// onto a folder that holds anything), moved back if `building` cannot take
/// the name, and deleted once it has.
fn swap_in(building: &Path, dist: &Path, replaced: &Path) -> Result<(), String> {
    remove_dir_if_there(replaced)?;
    let had = dist.exists();
    if had {
        std::fs::rename(dist, replaced).map_err(|e| {
            format!(
                "{} could not be moved aside (is something running from it?): {e}",
                dist.display()
            )
        })?;
    }
    if let Err(e) = std::fs::rename(building, dist) {
        if had {
            let _ = std::fs::rename(replaced, dist);
        }
        return Err(format!(
            "{} could not be named {}: {e}",
            building.display(),
            dist.display()
        ));
    }
    if had {
        // Best effort: `sweep` takes what is left.
        let _ = std::fs::remove_dir_all(replaced);
    }
    Ok(())
}

/// Assembles, checks, names, smokes and stages the dist of `head`.
///
/// `build` fills the folder it is given and says how the build ended; `smoke`
/// judges the named dist and returns its verdict and report. Until `build`'s
/// folder is whole and is the build of `head`, nothing a reader can see has
/// changed: a dist of this commit and a pointer that names it are as they
/// were. A pointer that names the dist being replaced is taken down for the
/// rename, so no reader finds one build under another's verdict.
fn stage_built(
    layout: &Layout,
    head: &str,
    previous: Option<&Pointer>,
    out: &mut Out,
    build: impl FnOnce(&Path, &mut Out) -> Result<Option<i32>, String>,
    smoke: impl FnOnce(&Path, &mut Out) -> (Smoke, String),
) -> Result<Pointer, (i32, String)> {
    const AS_THEY_WERE: &str =
        "the installed copy, the staged build and the pointer are as they were";
    let failed = |e: String| (FAILED, e);
    let commit = head.get(..12).unwrap_or(head);
    let dist = layout.dist(commit);
    let building = layout.building(commit);
    remove_dir_if_there(&building).map_err(failed)?;
    let began = Instant::now();
    let built = build(&building, out)
        .and_then(|code| match code {
            Some(0) => Ok(()),
            code => Err(format!(
                "build.ps1 {} after {} s",
                exited(code),
                began.elapsed().as_secs()
            )),
        })
        .and_then(|()| {
            Build::read(&building)?
                .ok_or_else(|| format!("build.ps1 wrote no BUILD.json in {}", building.display()))
        })
        .and_then(|record| {
            if record.commit_full == head {
                Ok(record)
            } else {
                Err(format!(
                    "BUILD.json names commit {}, the build tree is at {head}",
                    record.commit_full
                ))
            }
        });
    let record = match built {
        Ok(record) => record,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&building);
            return Err(failed(format!("{e}; {AS_THEY_WERE}")));
        }
    };
    out.say(&format!(
        "update: built {} build {} ({}) in {} s; smoking it",
        record.version,
        record.build_number,
        record.toolchain,
        began.elapsed().as_secs()
    ));
    let pointed = previous.filter(|p| p.dir == dist);
    if pointed.is_some() {
        match std::fs::remove_file(layout.latest()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                let _ = std::fs::remove_dir_all(&building);
                return Err(failed(format!(
                    "{}: {e}; {AS_THEY_WERE}",
                    layout.latest().display()
                )));
            }
        }
    }
    if let Err(e) = swap_in(&building, &dist, &layout.replaced(commit)) {
        let _ = std::fs::remove_dir_all(&building);
        // The dist it named is back under its name: so is the pointer.
        if let Some(p) = pointed {
            let _ = p.write(&layout.latest());
        }
        return Err(failed(format!("{e}; {AS_THEY_WERE}")));
    }
    let (verdict, report) = smoke(&dist, out);
    let _ = std::fs::write(layout.smoke_report(commit), report);
    let staged = Pointer::of(&record, &dist, pointer::now_rfc3339(), verdict);
    staged.write(&layout.latest()).map_err(failed)?;
    out.say(&format!(
        "update: staged {} at {} (smoke {})",
        staged.commit,
        staged.dir.display(),
        verdict.word()
    ));
    let installed = Build::read(&layout.installed()).ok().flatten();
    let mut keep = vec![staged.commit.as_str()];
    keep.extend(previous.map(|p| p.commit.as_str()));
    keep.extend(installed.as_ref().map(|b| b.commit.as_str()));
    sweep(layout, &keep, out);
    Ok(staged)
}

/// Builds, packages, smokes and stages `head`; the pointer it wrote.
fn stage(
    layout: &Layout,
    head: &str,
    previous: Option<&Pointer>,
    env: &[(String, String)],
    out: &mut Out,
) -> Result<Pointer, (i32, String)> {
    let failed = |e: String| (FAILED, e);
    let src = layout.src();
    // The build tree is this tool's alone: --force drops what a build left in it.
    // The submodule is left to the `submodule update` below: under the
    // repository's `submodule.recurse`, which aterm's checkouts set, a checkout
    // would recurse into vendor/astream, whose repository a new tree does not
    // have yet (and a pin the old one has not fetched).
    git(
        &src,
        &[
            "checkout",
            "--no-recurse-submodules",
            "--detach",
            "--force",
            head,
        ],
        GIT_LOCAL,
    )
    .map_err(failed)?;
    git(
        &src,
        &["submodule", "update", "--init", "vendor/astream"],
        GIT_NETWORK,
    )
    .map_err(failed)?;
    let shell = powershell(env);
    let script_path = src.join("apps").join("aterm-win").join("build.ps1");
    stage_built(
        layout,
        head,
        previous,
        out,
        |to, out| {
            out.say(&format!(
                "update: building {} with {} into {}",
                head.get(..12).unwrap_or(head),
                script_path.display(),
                to.display()
            ));
            script(
                &shell,
                &script_path,
                &["-Out".as_ref(), to.as_os_str()],
                &src,
                out,
            )
        },
        |dist, out| {
            let opts = smoke::Options {
                dist: dist.to_path_buf(),
                scratch: None,
                keep: false,
                frame: None,
            };
            let mut report = String::new();
            let outcome = smoke::smoke(&opts, env, &mut |line| {
                out.say(line);
                report.push_str(line);
                report.push('\n');
            });
            let verdict = match &outcome {
                Outcome::Ran(_) if outcome.code() == PASS => Smoke::Pass,
                Outcome::Ran(_) => Smoke::Fail,
                Outcome::NotRun(why) => {
                    out.say(&format!("NOT RUN smoke: {why}"));
                    report.push_str(&format!("NOT RUN smoke: {why}\n"));
                    Smoke::NotRun
                }
            };
            (verdict, report)
        },
    )
}

/// What the dist's installer is started with: `-Staged` (it never stops a
/// running window by itself), `-Restart` when the person asked for the windows
/// to be restarted, and `-Update` over an install that is already there, so
/// the first install's decisions (the shortcuts, the PATH entry) are not made
/// again by a daily run.
#[must_use]
pub fn installer_args(restart: bool, over_existing: bool) -> Vec<&'static str> {
    let mut args = vec!["-Staged"];
    if restart {
        args.push("-Restart");
    }
    if over_existing {
        args.push("-Update");
    }
    args
}

/// Hands a staged build to its own installer.
fn install(
    staged: &Pointer,
    restart: bool,
    over_existing: bool,
    env: &[(String, String)],
    out: &mut Out,
) -> i32 {
    let installer = staged.dir.join("install.ps1");
    if !installer.is_file() {
        out.say(&format!(
            "update: {} is missing: the dist cannot install itself",
            installer.display()
        ));
        return FAILED;
    }
    let words = installer_args(restart, over_existing);
    let args: Vec<&std::ffi::OsStr> = words.iter().map(std::ffi::OsStr::new).collect();
    out.say(&format!(
        "update: installing {} with install.ps1 {}",
        staged.commit,
        words.join(" ")
    ));
    match script(&powershell(env), &installer, &args, &staged.dir, out) {
        Ok(Some(0)) => {
            out.say("update: installed");
            PASS
        }
        Ok(code) => {
            out.say(&format!("update: install.ps1 {}", exited(code)));
            FAILED
        }
        Err(e) => {
            out.say(&format!("update: {e}"));
            FAILED
        }
    }
}

/// `update`: the exit code. 0 staged (and installed, unless told not to) or
/// nothing to do; 1 the build, the smoke or the install failed; 3 it could not run.
pub fn update(opts: &Options, layout: &Layout, env: &[(String, String)], out: &mut Out) -> i32 {
    let root = layout.dist_root();
    if let Err(e) = std::fs::create_dir_all(&root) {
        out.say(&format!("update: {}: {e}", root.display()));
        return NOT_RUN;
    }
    // One update at a time: a scheduled run and a hand-started one share the build tree.
    let _lock = match aterm_uds::file_lock::LockFile::take(
        &root.join("update.lock"),
        Instant::now() + Duration::from_secs(2),
        "another aterm-winship update is running",
    ) {
        Ok(lock) => lock,
        Err(e) => {
            out.say(&format!("update: not run: {e}"));
            return NOT_RUN;
        }
    };
    let src = layout.src();
    let fetched = std::env::current_dir()
        .map_err(|e| format!("the working directory: {e}"))
        .and_then(|cwd| repo::resolve(opts.repo.as_deref(), layout, &cwd))
        .and_then(|repo| ensure_tree(&repo, layout, out))
        // Only main: the build's own `submodule update` fetches the
        // submodule's pin (`stage`), and a recursing fetch cannot reach the
        // submodule of a tree that has not checked it out yet.
        .and_then(|()| {
            git(
                &src,
                &["fetch", "--no-recurse-submodules", "origin", "main"],
                GIT_NETWORK,
            )
        })
        .and_then(|_| git(&src, &["rev-parse", "origin/main"], GIT_LOCAL))
        .and_then(|head| {
            let time = git(&src, &["show", "-s", "--format=%ct", &head], GIT_LOCAL)?;
            let number = time
                .parse::<u64>()
                .map_err(|_| format!("committer time {time:?}"))?;
            Ok((head, number))
        });
    let (head, number) = match fetched {
        Ok(head) => head,
        Err(e) => {
            out.say(&format!("update: not run: {e}"));
            return NOT_RUN;
        }
    };
    let previous = match Pointer::read(&layout.latest()) {
        Ok(p) => p,
        Err(e) => {
            out.say(&format!(
                "update: ignoring a pointer that does not parse ({e})"
            ));
            None
        }
    };
    let present = previous
        .as_ref()
        .is_some_and(|p| p.dir.join("aterm.exe").is_file());
    let head_ref = Head {
        commit_full: &head,
        build_number: number,
    };
    let staged = match plan::plan(previous.as_ref(), present, head_ref, opts.force) {
        Plan::Build => match stage(layout, &head, previous.as_ref(), env, out) {
            Ok(staged) => staged,
            Err((code, why)) => {
                out.say(&format!("update: failed: {why}"));
                return code;
            }
        },
        Plan::Staged(verdict) => {
            out.say(&format!(
                "update: origin/main {} is already staged (smoke {}); nothing to build",
                head.get(..12).unwrap_or(&head),
                verdict.word()
            ));
            match previous {
                Some(p) => p,
                None => return NOT_RUN,
            }
        }
        Plan::Older {
            staged,
            head: older,
        } => {
            out.say(&format!(
                "update: origin/main (build {older}) is older than the staged build {staged}; nothing built (--force builds it)"
            ));
            return PASS;
        }
    };
    match staged.smoke {
        Smoke::Fail => return FAILED,
        Smoke::NotRun => return NOT_RUN,
        Smoke::Pass => {}
    }
    if opts.no_install {
        out.say("update: not installing (--no-install)");
        return PASS;
    }
    let at = layout.installed();
    let installed = Build::read(&at).ok().flatten();
    let have = installed
        .as_ref()
        .map(|b| (b.commit_full.as_str(), b.build_number, b.dirty));
    match plan::install_needed(&staged, have, opts.force) {
        Install::Yes => install(
            &staged,
            opts.restart,
            at.join("aterm.exe").is_file(),
            env,
            out,
        ),
        Install::Same => {
            out.say(&format!("update: {} is the installed build", staged.commit));
            PASS
        }
        Install::Newer { installed, staged } => {
            out.say(&format!(
                "update: the installed build {installed} is newer than origin/main's {staged}; not installing (--force installs)"
            ));
            PASS
        }
        Install::Unproven => FAILED,
    }
}

/// `status`: the installed build, the staged pointer, origin/main and the
/// build tree, one line each. Reads only.
pub fn status(repo: Option<&Path>, layout: &Layout, out: &mut Out) -> i32 {
    let at = layout.installed();
    let installed = match Build::read(&at) {
        Ok(Some(b)) => format!(
            "{} build {} commit {} ({}) at {}",
            b.version,
            b.build_number,
            b.commit,
            b.toolchain,
            at.display()
        ),
        Ok(None) if at.join("aterm.exe").is_file() => {
            match proc::run(
                &at.join("aterm.exe"),
                &["--version"],
                Place::default(),
                smoke::HANG,
            ) {
                Ok(ran) => match smoke::identity(&ran.stdout) {
                    Some(id) => format!(
                        "{} build {} at {} (no BUILD.json)",
                        id.version,
                        id.build.map_or_else(|| "?".to_string(), |b| b.to_string()),
                        at.display()
                    ),
                    None => format!("{} answered {:?}", at.display(), one_line(&ran.text(), 120)),
                },
                Err(e) => format!("unreadable: {e}"),
            }
        }
        Ok(None) => format!("none at {}", at.display()),
        Err(e) => format!("unreadable: {e}"),
    };
    out.say(&format!("installed:   {installed}"));
    let staged = match Pointer::read(&layout.latest()) {
        Ok(Some(p)) => format!(
            "{} build {} commit {} smoke={} staged {} at {}{}",
            p.version,
            p.build_number,
            p.commit,
            p.smoke.word(),
            p.staged_at,
            p.dir.display(),
            if p.dir.join("aterm.exe").is_file() {
                ""
            } else {
                " (folder gone)"
            }
        ),
        Ok(None) => format!("none ({})", layout.latest().display()),
        Err(e) => format!("unreadable: {e}"),
    };
    out.say(&format!("staged:      {staged}"));
    let repo = std::env::current_dir()
        .map_err(|e| format!("the working directory: {e}"))
        .and_then(|cwd| repo::resolve(repo, layout, &cwd));
    match &repo {
        Ok(repo) => out.say(&format!(
            "repository:  {} ({})",
            repo.dir.display(),
            repo.from.word()
        )),
        Err(e) => out.say(&format!("repository:  unknown: {e}")),
    }
    let remote = match &repo {
        Ok(repo) => git(
            &repo.dir,
            &["ls-remote", "origin", "refs/heads/main"],
            Duration::from_secs(60),
        )
        .map(|line| {
            line.split_whitespace()
                .next()
                .unwrap_or("(no refs/heads/main)")
                .to_string()
        })
        .unwrap_or_else(|e| format!("unreachable: {e}")),
        Err(_) => "unknown: no repository".to_string(),
    };
    out.say(&format!("origin/main: {remote}"));
    let src = layout.src();
    let tree = if src.join(".git").exists() {
        let head = git(&src, &["rev-parse", "HEAD"], GIT_LOCAL).map_or_else(
            |e| format!("unreadable: {e}"),
            |head| format!("{head} at {}", src.display()),
        );
        // The tree is built from the repository it is a worktree of, whatever
        // repository was asked about: say so when they differ.
        let other = repo
            .as_ref()
            .ok()
            .filter(|repo| repo.from != Source::Tree)
            .filter(|repo| !repo::same(&src, &repo.dir).unwrap_or(false))
            .map(|repo| {
                format!(
                    " (a worktree of {}, not of {})",
                    repo::main_checkout(&src).map_or_else(
                        |_| "a repository that is gone".to_string(),
                        |dir| dir.display().to_string()
                    ),
                    repo.dir.display()
                )
            });
        format!("{head}{}", other.unwrap_or_default())
    } else {
        format!("none ({})", src.display())
    };
    out.say(&format!("build tree:  {tree}"));
    PASS
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
