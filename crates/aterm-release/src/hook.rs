// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `ship hook <name>`: the publication engine's hooks.

use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};

use aterm_uds::spawnfd::{Stdio, command};

/// What `ship hook` takes.
pub const USAGE: &str =
    "usage: targo --unverified ship hook pre-promote | post-promote [ship site's \
     flags] | transforms
  the publication engine's hooks, run by publish/pre-promote, publish/post-promote and
  publish/transforms.sh:
  pre-promote   would a real cut of PUB_SOURCE_COMMIT pass its pre-claim gates (`ship
                check --commit <it> --cut`)? Asked by that commit's own cutter:
                ATERM_RELEASE_CUTTER (the running release's) when set, else one built
                in a throwaway worktree of it. Exit 0 yes, 1 a gate refuses (named),
                2 it could not be asked
  post-promote  `ship site`; its promote-time run (PUB_STAGING_COMMIT set, no --latest)
                does nothing
  transforms    rewrite the export at EXPORT by CAPTURED_COMMIT's own rules (ROOT: the
                repository); exit 0 done, 1 refused or failed (named)";

/// `ship hook …`: the hook's exit code.
#[must_use]
pub fn run_cli(args: &[String], cwd: &Path) -> i32 {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("pre-promote") if rest.is_empty() => {
            // A signal is recorded, never fatal: the commit's cutter (in this process
            crate::interrupt::install();
            let facts = PrePromote::from_process();
            let mut check = |asked: Asked<'_>, full: &str| {
                let args = ["check", "--commit", full, "--cut"];
                match asked {
                    Asked::Running { cutter, root } => {
                        let inherited =
                            std::env::vars_os().filter_map(|(k, _)| k.into_string().ok());
                        exit_code(
                            running_cutter_command(cutter, root, &args, inherited).status(),
                            cutter,
                        )
                    }
                    Asked::Built { tree, target } => own_cutter(tree, &args, target, &[]),
                }
            };
            let code = pre_promote(
                &facts,
                cwd,
                &mut check,
                &mut std::io::stdout(),
                &mut std::io::stderr(),
            );
            crate::interrupt::caught().map_or(code, |sig| 128 + sig)
        }
        Some("post-promote") => match toplevel(cwd, &[]) {
            Some(root) => crate::site::run_cli(rest, &root),
            None => {
                eprintln!("publish/post-promote: not run from inside a git checkout");
                2
            }
        },
        Some("transforms") if rest.is_empty() => {
            let facts = match Transforms::from_process() {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("FAIL: {e}");
                    return 1;
                }
            };
            let mut there = |tree: &Path, full: &str, target: &Path| {
                own_cutter(
                    tree,
                    &["hook", "transforms"],
                    target,
                    &[
                        ("ROOT", facts.inputs.root.as_os_str()),
                        ("EXPORT", facts.inputs.export.as_os_str()),
                        ("CAPTURED_COMMIT", OsStr::new(full)),
                        (DELEGATED, OsStr::new(full)),
                    ],
                )
            };
            transforms(
                &facts,
                &mut crate::transforms::run,
                &mut there,
                &mut std::io::stdout(),
                &mut std::io::stderr(),
            )
        }
        Some("-h" | "--help") => {
            println!("{USAGE}");
            0
        }
        _ => {
            eprintln!("{USAGE}");
            2
        }
    }
}

/// What the engine hands `pre-promote`.
#[derive(Debug, Clone, Default)]
pub struct PrePromote {
    /// `PUB_SOURCE_COMMIT`: the dev commit being published.
    pub commit: Option<String>,
    /// `PUB_VERSION`, named in the transcript.
    pub version: Option<String>,
    /// `CARGO_TARGET_DIR`: the build goes in its `pre-promote.noindex` directory.
    pub target: Option<PathBuf>,
    /// `ATERM_RELEASE_CUTTER`.
    pub cutter: Option<PathBuf>,
    /// Environment for every git child (a test isolates git from this machine's).
    pub git_env: Vec<(String, String)>,
}

impl PrePromote {
    /// The engine's facts in this process's environment.
    #[must_use]
    pub fn from_process() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Self {
            commit: var("PUB_SOURCE_COMMIT"),
            version: var("PUB_VERSION"),
            target: var("CARGO_TARGET_DIR").map(PathBuf::from),
            cutter: var(crate::release_steps::RELEASE_CUTTER_ENV).map(PathBuf::from),
            git_env: Vec::new(),
        }
    }
}

/// Which of the commit's own cutters answers the pre-promote's question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asked<'a> {
    /// The running release's binary (`ATERM_RELEASE_CUTTER`), from the repository root.
    Running { cutter: &'a Path, root: &'a Path },
    /// One built in the commit's worktree `tree`, into `target`.
    Built { tree: &'a Path, target: &'a Path },
}

/// The commit's own check for the full sha, asked as [`Asked`] says.
pub type Check<'a> = dyn FnMut(Asked<'_>, &str) -> Result<i32, String> + 'a;

/// A finished child's exit code, a signal's as a shell reports it (128 + the signal).
fn exit_code(
    status: std::io::Result<std::process::ExitStatus>,
    what: &Path,
) -> Result<i32, String> {
    let status = status.map_err(|e| format!("cannot run {}: {e}", what.display()))?;
    #[cfg(unix)]
    let by_signal = std::os::unix::process::ExitStatusExt::signal(&status).unwrap_or(0);
    #[cfg(not(unix))]
    let by_signal = 0;
    Ok(status.code().unwrap_or(128 + by_signal))
}

/// The running release's `cutter` asked `args` from `root`, as from the operator's shell.
#[must_use]
pub fn running_cutter_command(
    cutter: &Path,
    root: &Path,
    args: &[&str],
    inherited: impl IntoIterator<Item = String>,
) -> aterm_uds::spawnfd::Command {
    let mut cmd = command(cutter);
    cmd.args(args).current_dir(root).stdin(Stdio::null());
    for name in inherited {
        if crate::release_steps::launcher_env(&name) {
            cmd.env_remove(name);
        }
    }
    cmd
}

/// A regular file someone may execute (symlinks followed), as `[ -f p ] && [ -x p ]`.
fn executable_file(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// else the one on PATH: its exit code, a signal's as a shell reports it.
fn own_cutter(
    tree: &Path,
    args: &[&str],
    target: &Path,
    envs: &[(&str, &OsStr)],
) -> Result<i32, String> {
    let targo = std::env::var_os("TRUST_STAGE2_BIN")
        .filter(|b| !b.is_empty())
        .map_or_else(
            || PathBuf::from("targo"),
            |b| PathBuf::from(b).join("targo"),
        );
    let inherited = std::env::vars_os().filter_map(|(k, _)| k.into_string().ok());
    exit_code(
        cutter_command(&targo, tree, args, target, envs, inherited).status(),
        &targo,
    )
}

/// [`own_cutter`]'s command, run as from the operator's shell.
#[must_use]
pub fn cutter_command(
    targo: &Path,
    tree: &Path,
    args: &[&str],
    target: &Path,
    envs: &[(&str, &OsStr)],
    inherited: impl IntoIterator<Item = String>,
) -> aterm_uds::spawnfd::Command {
    let mut cmd = command(targo);
    cmd.args(["--unverified", "ship"])
        .args(args)
        .current_dir(tree)
        .env("CARGO_TARGET_DIR", target)
        .stdin(Stdio::null());
    for name in inherited {
        if crate::release_steps::launcher_env(&name) {
            cmd.env_remove(name);
        }
    }
    for (name, value) in envs {
        cmd.env(name, value);
    }
    cmd
}

fn git_output(dir: &Path, env: &[(String, String)], args: &[&str]) -> Option<std::process::Output> {
    command("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::null())
        .output()
        .ok()
}

fn git(dir: &Path, env: &[(String, String)], args: &[&str]) -> Option<String> {
    let out = git_output(dir, env, args)?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The full sha `rev` names in `root`, when it names a commit.
fn commit_of(root: &Path, env: &[(String, String)], rev: &str) -> Option<String> {
    git(
        root,
        env,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
    )
    .filter(|s| !s.is_empty())
}

/// Named `*.noindex` so Spotlight skips the build with all it holds.
fn worktree_target(root: &Path, target: Option<&Path>) -> PathBuf {
    let base = match target {
        Some(t) if t.is_absolute() => t.to_path_buf(),
        Some(t) => root.join(t),
        None => root.join("target"),
    };
    base.join("pre-promote.noindex")
}

fn toplevel(cwd: &Path, env: &[(String, String)]) -> Option<PathBuf> {
    git(cwd, env, &["rev-parse", "--show-toplevel"])
        .filter(|t| !t.is_empty())
        .map(PathBuf::from)
}

/// the commit records; removed on drop, worktree and all, whatever ended its use.
struct Worktree {
    root: PathBuf,
    scratch: PathBuf,
    tree: PathBuf,
    env: Vec<(String, String)>,
}

impl Worktree {
    /// Lay out `full` from `root`'s object store.
    fn of(root: &Path, env: &[(String, String)], full: &str) -> Result<Self, String> {
        let scratch = crate::site::private_dir("aterm-hook-worktree").map_err(|e| {
            format!("cannot make a scratch directory for the commit's worktree: {e}")
        })?;
        let made = Self {
            root: root.to_path_buf(),
            tree: scratch.join("tree"),
            scratch,
            env: env.to_vec(),
        };
        let tree = made.tree.to_string_lossy().into_owned();
        if git(
            root,
            env,
            &["worktree", "add", "--quiet", "--detach", &tree, full],
        )
        .is_none()
        {
            return Err(format!("cannot lay out a worktree of {full} at {tree}"));
        }
        if git(
            &made.tree,
            env,
            &["submodule", "update", "--init", "--recursive", "--quiet"],
        )
        .is_none()
        {
            return Err(format!(
                "cannot check out the submodules {full} records (vendor/astream: git \
                 submodule update --init, with its remote reachable)"
            ));
        }
        Ok(made)
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        let tree = self.tree.to_string_lossy().into_owned();
        let _ = git(
            &self.root,
            &self.env,
            &["worktree", "remove", "--force", &tree],
        );
        let _ = std::fs::remove_dir_all(&self.scratch);
        let _ = git(&self.root, &self.env, &["worktree", "prune"]);
    }
}

/// Would a real cut of the engine's commit pass its pre-claim gates? `check` answers as.
pub fn pre_promote(
    facts: &PrePromote,
    cwd: &Path,
    check: &mut Check<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let mut refuse = |line: String| {
        let _ = writeln!(err, "publish/pre-promote: {line}");
        2
    };
    let Some(commit) = facts.commit.as_deref() else {
        return refuse(
            "PUB_SOURCE_COMMIT is not set — the engine names the commit it publishes there; \
             refusing rather than checking some other commit"
                .into(),
        );
    };
    let env = &facts.git_env;
    let Some(root) = toplevel(cwd, env) else {
        return refuse(
            "not run from inside a git checkout — refusing rather than checking nothing".into(),
        );
    };
    let Some(full) = commit_of(&root, env, commit) else {
        return refuse(format!(
            "PUB_SOURCE_COMMIT={commit} names no commit in {} — refusing rather than checking \
             some other commit",
            root.display()
        ));
    };
    let version = facts
        .version
        .as_deref()
        .map(|v| format!(" (v{v})"))
        .unwrap_or_default();
    if let Some(cutter) = facts.cutter.as_deref() {
        if !executable_file(cutter) {
            return refuse(format!(
                "{}={} is not an executable file — refusing rather than checking with another \
                 cutter",
                crate::release_steps::RELEASE_CUTTER_ENV,
                cutter.display()
            ));
        }
        let _ = writeln!(
            out,
            "publish/pre-promote: would a real cut of {}{version} pass its pre-claim gates? Asked \
             by the running release's own cutter ({} check)",
            &full[..full.len().min(12)],
            cutter.display()
        );
        let _ = out.flush();
        return match check(
            Asked::Running {
                cutter,
                root: &root,
            },
            &full,
        ) {
            Ok(code) => code,
            Err(e) => refuse(e),
        };
    }
    let target = worktree_target(&root, facts.target.as_deref());
    let _ = writeln!(
        out,
        "publish/pre-promote: would a real cut of {}{version} pass its pre-claim gates? Asked \
         by that commit's own cutter, built in a throwaway worktree of it (targo --unverified \
         ship check)",
        &full[..full.len().min(12)]
    );
    let worktree = match Worktree::of(&root, env, &full) {
        Ok(w) => w,
        Err(e) => {
            return refuse(format!(
                "{e} — refusing rather than checking with some other commit's code"
            ));
        }
    };
    let _ = out.flush();
    match check(
        Asked::Built {
            tree: &worktree.tree,
            target: &target,
        },
        &full,
    ) {
        Ok(code) => code,
        Err(e) => refuse(e),
    }
}

/// Set by a `transforms` run that hands the export to the captured commit's own cutter.
pub const DELEGATED: &str = "ATERM_HOOK_TRANSFORMS_DELEGATED";

/// What the engine hands `transforms`, and where this cutter came from.
#[derive(Debug, Clone)]
pub struct Transforms {
    /// `ROOT`, `EXPORT`, `CAPTURED_COMMIT`.
    pub inputs: crate::transforms::Inputs,
    /// A directory of the checkout this cutter was compiled from.
    pub source: PathBuf,
    /// `CARGO_TARGET_DIR`: the captured commit's cutter builds in its `pre-promote.noindex`.
    pub target: Option<PathBuf>,
    /// [`DELEGATED`], when a delegating run set it.
    pub delegated: Option<String>,
    /// Environment for every git child (a test isolates git from this machine's).
    pub git_env: Vec<(String, String)>,
}

impl Transforms {
    /// The engine's facts in this process's environment; the source is this crate's.
    /// # Errors
    /// `ROOT` or `EXPORT` unset.
    pub fn from_process() -> Result<Self, String> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Ok(Self {
            inputs: crate::transforms::Inputs::from_env()?,
            source: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            target: var("CARGO_TARGET_DIR").map(PathBuf::from),
            delegated: var(DELEGATED),
            git_env: Vec::new(),
        })
    }
}

/// Why the checkout at `source` is not `full`'s source.
#[must_use]
pub fn source_differs(source: &Path, env: &[(String, String)], full: &str) -> Option<String> {
    let short = &full[..full.len().min(12)];
    let Some(top) = toplevel(source, env) else {
        return Some(format!("{} is not inside a git checkout", source.display()));
    };
    let mut pending = vec![(top, full.to_string())];
    while let Some((tree, revision)) = pending.pop() {
        match git_output(
            &tree,
            env,
            &[
                "diff",
                "--quiet",
                "--ignore-submodules=none",
                &revision,
                "--",
            ],
        )
        .and_then(|o| o.status.code())
        {
            Some(0) => {}
            Some(1) => {
                return Some(format!(
                    "{}'s source tree differs from {short}",
                    tree.display()
                ));
            }
            _ => {
                return Some(format!(
                    "{} cannot compare itself with {short}",
                    tree.display()
                ));
            }
        }
        let Some(list) = git(
            &tree,
            env,
            &[
                "ls-files",
                "--others",
                "--directory",
                "--no-empty-directory",
                "-z",
            ],
        ) else {
            return Some(format!("{} cannot list its local files", tree.display()));
        };
        if !list.is_empty() {
            return Some(format!(
                "{} holds files {short} does not: {}",
                tree.display(),
                list.split('\0')
                    .filter(|p| !p.is_empty())
                    .take(4)
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
        }
        let Some(index) = git(&tree, env, &["ls-files", "--stage", "-z"]) else {
            return Some(format!("{} cannot list its gitlinks", tree.display()));
        };
        for entry in index.split('\0').filter(|e| e.starts_with("160000 ")) {
            let Some((record, path)) = entry.split_once('\t') else {
                return Some(format!("{} has an unreadable gitlink", tree.display()));
            };
            let Some(revision) = record.split_whitespace().nth(1) else {
                return Some(format!("{} has an unreadable gitlink", tree.display()));
            };
            let sub = tree.join(path);
            let initialized = toplevel(&sub, env)
                .and_then(|top| top.canonicalize().ok())
                .zip(sub.canonicalize().ok())
                .is_some_and(|(top, sub)| top == sub);
            if !initialized {
                return Some(format!("{} is not an initialized submodule", sub.display()));
            }
            pending.push((sub, revision.to_string()));
        }
    }
    None
}

/// The commit's own cutter, run in its worktree `tree` for the full sha, building into `target`.
pub type There<'a> = dyn FnMut(&Path, &str, &Path) -> Result<i32, String> + 'a;

/// The rewrites over a commit's export, run here.
pub type Here<'a> = dyn FnMut(&crate::transforms::Inputs) -> Result<Vec<String>, String> + 'a;

/// THE EXPORT IS REWRITTEN BY THE EXPORTED COMMIT'S OWN RULES.
pub fn transforms(
    facts: &Transforms,
    here: &mut Here<'_>,
    there: &mut There<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let mut refuse = |line: String| {
        let _ = writeln!(err, "FAIL: publish/transforms.sh: {line}");
        1
    };
    let env = &facts.git_env;
    let root = &facts.inputs.root;
    let captured = &facts.inputs.captured;
    let Some(full) = commit_of(root, env, captured) else {
        return refuse(format!(
            "CAPTURED_COMMIT={captured} names no commit in {} — refusing rather than rewriting \
             the export with some other commit's rules",
            root.display()
        ));
    };
    let short = &full[..full.len().min(12)];
    let Some(why) = source_differs(&facts.source, env, &full) else {
        let inputs = crate::transforms::Inputs {
            captured: full.clone(),
            ..facts.inputs.clone()
        };
        return match here(&inputs) {
            Ok(said) => {
                for line in said {
                    let _ = writeln!(out, "{line}");
                }
                0
            }
            Err(e) => refuse(e),
        };
    };
    if facts.delegated.as_deref() == Some(full.as_str()) {
        return refuse(format!(
            "{short}'s own cutter does not find its source to be {short} ({why}) — refusing \
             rather than handing the export on again"
        ));
    }
    let rules = "crates/aterm-release/src/transforms.rs";
    if git(root, env, &["cat-file", "-e", &format!("{full}:{rules}")]).is_none() {
        return refuse(format!(
            "{short} has no `ship hook transforms` ({rules} is not in it) — stage it with the \
             transforms its own publish/transforms.sh names"
        ));
    }
    let _ = writeln!(
        out,
        "== transforms: {why} — running {short}'s own `ship hook transforms`, built in a \
         throwaway worktree of it"
    );
    let _ = out.flush();
    let target = worktree_target(root, facts.target.as_deref());
    let worktree = match Worktree::of(root, env, &full) {
        Ok(w) => w,
        Err(e) => {
            return refuse(format!(
                "{e} — refusing rather than rewriting the export with other code"
            ));
        }
    };
    match there(&worktree.tree, &full, &target) {
        Ok(code) => code,
        Err(e) => refuse(e),
    }
}
