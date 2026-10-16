// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a git read would load that can run a program: the check behind the
//! read-only rule's git reads ([`super::approval::RULE_READ_ONLY`]).
//!
//! Under `approve = "safe"` a git read is approved only when no configuration
//! it would load can execute a program. The classifier
//! ([`crate::supervise::classify`]) judges the line; this module judges the
//! machine: an accept-edits worker can write `.git/config`, and `git status`
//! then runs whatever `core.fsmonitor` names.
//!
//! **Where** ([`git_reads`]): each segment's command is found as
//! [`crate::supervise::classify::runs`] finds it, and runs in the session's cwd
//! as the worker's transcript last recorded it ([`WorkerEnv::claude_dir`],
//! [`crate::harness::footer::shell_cwds`]), every `cd`/`pushd` target, an
//! `env -C` directory, and every `-C` target against each (a superset). An
//! unresolvable directory escalates.
//!
//! **What** ([`hazard`]): the effective configuration (`git config --list
//! --show-scope`, includes and `GIT_CONFIG_*`) in each directory and every
//! populated submodule, recursively:
//!
//! * `core.fsmonitor` naming a program;
//! * `diff.external`, `diff.<driver>.textconv`/`command`, `GIT_EXTERNAL_DIFF`;
//! * `filter.<driver>.*` when the attributes select it for a tracked path;
//! * a partial clone (a read may fetch from the promisor remote);
//! * `gpg.program` when the read displays signatures;
//! * an executable `post-index-change` hook where the read would run it.
//!
//! A read that contacts a remote escalates outright. `core.pager`/`pager.*`
//! are exempt: Claude Code's Bash tool gives git no terminal, and the
//! classifier refuses the reads that start a pager or viewer without one
//! (`git grep -O`, `git <cmd> --help`).
//!
//! **As the worker reads it** ([`WorkerEnv`]): every probe runs the git the
//! worker's `PATH` finds with the worker process's environment, read from the
//! kernel, never aterm's own. Escalated instead: an unreadable environment; a
//! `PATH` entry ahead of git the worker could write ([`WorkerEnv::view`]); a
//! git-relevant variable Claude Code's settings give the Bash tool but not the
//! process ([`WorkerEnv::settings_env`]).
//!
//! Not covered: a box that runs on another machine (escalated). The shell
//! snapshot, `CLAUDE_ENV_FILE` and rc files Claude Code sources before each
//! command are [`super::shell_startup`]'s.

use aterm_uds::spawnfd::{Command, Stdio};
use std::path::{Component, Path, PathBuf};

use aterm_json::Value;
use atpkg::caller_shell::ProcArgs;

use crate::supervise::classify::{SUBSTITUTION, command_words, program, runs};

/// Where a line's git reads run, and what they may do beyond reading.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct GitReads {
    /// Every directory a git invocation on the line may run in, deduplicated.
    pub dirs: Vec<PathBuf>,
    /// A read that displays signatures on its own (`--show-signature`, a `%G`
    /// format placeholder, `tag -v`).
    pub signatures: bool,
    /// A `log`/`show`/`whatchanged`/`reflog`, which `log.showSignature` turns
    /// into a signature display.
    pub log_like: bool,
    /// The first read that contacts a remote, as a reason.
    pub remote: Option<String>,
}

/// The git reads on `line`, run from any of `cwds` (the session's directory
/// and wherever its Bash tool may stand): `Ok(None)` when it runs no git at
/// all, `Err` when a directory one may run in cannot be resolved, or a
/// segment's command cannot be read. A segment's command is found exactly as
/// the classifier's head check finds it ([`runs`]): through its prefixes and
/// through `env` and `xargs`, so every git the classifier approves as a read
/// is one this check places.
pub(crate) fn git_reads(
    line: &str,
    cwds: &[PathBuf],
    home: Option<&Path>,
) -> Result<Option<GitReads>, String> {
    let segments = command_words(line)?;
    let mut commands = Vec::new();
    for seg in &segments {
        if let Some(run) = runs(seg)? {
            commands.push((seg, run));
        }
    }
    let is_git = |seg: &[String], at: usize| program(&seg[at]) == "git";
    if !commands.iter().any(|(seg, run)| is_git(seg, run.at)) {
        return Ok(None);
    }
    // Every directory a `cd`/`pushd` on the line may leave the shell in.
    // A loop repeats a relative one (`while …; do cd ..; git status; done`
    // climbs one level a pass), so there the set has no bound this check can
    // name.
    let looped = segments.iter().any(|seg| {
        seg.iter()
            .find(|w| w.as_str() != "!")
            .is_some_and(|w| matches!(w.as_str(), "for" | "while" | "until" | "select"))
    });
    let mut bases = cwds.to_vec();
    for (seg, run) in &commands {
        if !matches!(program(&seg[run.at]), "cd" | "pushd") {
            continue;
        }
        let target = seg[run.at + 1..]
            .iter()
            .find(|w| !w.starts_with('-') || w.as_str() == "-");
        if looped && target.is_some_and(|t| !t.starts_with('/') && !t.starts_with('~')) {
            return Err(format!(
                "a git read on a line whose loop repeats `cd {}`",
                target.map(String::as_str).unwrap_or("")
            ));
        }
        let found = match target.map(String::as_str) {
            None => vec![
                home.map(Path::to_path_buf)
                    .ok_or_else(|| "a git read after `cd` with no home".to_string())?,
            ],
            Some(t) => {
                let mut found = Vec::new();
                for base in &bases {
                    found.extend(resolve(base, t, home)?);
                }
                found
            }
        };
        bases.extend(found);
    }
    let mut out = GitReads::default();
    for (seg, run) in &commands {
        let i = run.at;
        if !is_git(seg, i) {
            continue;
        }
        let fed = run.fed;
        // `env -C DIR` moves the git before its own `-C` does.
        let mut dirs = bases.clone();
        if !run.chdir.is_empty() {
            if fed {
                return Err(format!(
                    "env -C {} behind xargs (its input may name the directory)",
                    run.chdir[0]
                ));
            }
            let mut moved = Vec::new();
            for d in &run.chdir {
                for base in &bases {
                    moved.extend(resolve(base, d, home)?);
                }
            }
            dirs = moved;
        }
        // Global options up to the subcommand; `-C` chains.
        let mut j = i + 1;
        while let Some(t) = seg.get(j).map(String::as_str) {
            if t == "-C" {
                let Some(d) = seg.get(j + 1) else {
                    return Err("git -C without a directory".to_string());
                };
                if fed {
                    return Err(format!(
                        "git -C {d} behind xargs (its input names the directory)"
                    ));
                }
                let mut next = Vec::new();
                for base in &dirs {
                    next.extend(resolve(base, d, home)?);
                }
                dirs = next;
                j += 2;
            } else if t == "-c"
                || t.starts_with("--git-dir")
                || t.starts_with("--work-tree")
                || t.starts_with("--config-env")
                || t.starts_with("--exec-path")
            {
                return Err(format!("git {t}"));
            } else if t == "--namespace" {
                j += 2;
            } else if t.starts_with('-') {
                j += 1;
            } else {
                break;
            }
        }
        let sub = seg.get(j).map(String::as_str).unwrap_or("");
        let args = seg.get(j + 1..).unwrap_or(&[]);
        let has = |f: &str| args.iter().any(|a| a == f);
        if args
            .iter()
            .any(|a| a == "--show-signature" || a.contains("%G"))
            || (sub == "tag" && (has("-v") || has("--verify")))
            || matches!(sub, "verify-commit" | "verify-tag")
        {
            out.signatures = true;
        }
        if matches!(sub, "log" | "show" | "whatchanged" | "reflog") {
            out.log_like = true;
        }
        let contacts_remote = sub == "ls-remote"
            || (sub == "remote" && args.first().map(String::as_str) == Some("show") && !has("-n"));
        if contacts_remote && out.remote.is_none() {
            out.remote = Some(format!(
                "git {sub} contacts a remote (credential helpers, ssh and the remote's \
                 programs run)"
            ));
        }
        for d in dirs {
            if !out.dirs.contains(&d) {
                out.dirs.push(d);
            }
        }
    }
    Ok(Some(out))
}

/// Where directory operand `t` of a `cd` or `git -C` in `base` leads: every
/// reading of it (`~/x` is `$HOME/x` unquoted and `./~/x` quoted, and the
/// word no longer says which), lexically normalised.
fn resolve(base: &Path, t: &str, home: Option<&Path>) -> Result<Vec<PathBuf>, String> {
    if t.contains(SUBSTITUTION) || t.contains('$') {
        return Err(format!(
            "a git read in a directory this check cannot resolve ({})",
            t.replace(SUBSTITUTION, "$(…)")
        ));
    }
    if t == "-" || (t.starts_with('+') && t.len() > 1) {
        return Err(format!("a git read after `cd {t}` (the directory stack)"));
    }
    let mut out = Vec::new();
    if let Some(rest) = t.strip_prefix('~') {
        if !(rest.is_empty() || rest.starts_with('/')) {
            return Err(format!("a git read in `{t}` (another user's home)"));
        }
        let home = home.ok_or_else(|| format!("a git read in `{t}` with no home"))?;
        out.push(normalise(&home.join(rest.trim_start_matches('/'))));
    }
    out.push(normalise(&base.join(t)));
    Ok(out)
}

/// `.` and `..` resolved lexically (git's `-C` and the shell's `cd` resolve
/// them the same way for a path whose parents are not symlinks).
pub(crate) fn normalise(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// What a git command in one directory would load.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitView {
    /// `(scope, key, value)` as `git config --list --show-scope` reports them
    /// (section and variable names lowercased by git); `None` for a bare key.
    pub config: Vec<(String, String, Option<String>)>,
    /// The executable `post-index-change` hook of the repository, if any.
    pub index_hook: Option<PathBuf>,
    /// The filter drivers the repository's attributes select for a tracked
    /// path (`git check-attr filter`), read only when a filter is configured.
    /// `None` when it was not read: every configured filter then counts.
    pub filters_selected: Option<Vec<String>>,
    /// Each populated submodule, recursively, with what git loads there: a
    /// read that recurses (`status`, `diff`) runs git in it.
    pub submodules: Vec<(PathBuf, GitView)>,
    /// The worker's `GIT_EXTERNAL_DIFF`, which git runs for a diff as it runs
    /// `diff.external`.
    pub external_diff: Option<String>,
}

/// How deep submodules are followed before the check gives up (and escalates).
const MAX_SUBMODULE_DEPTH: usize = 8;
/// How many populated submodules one view reads before the check gives up
/// (and escalates) rather than read at length.
const MAX_SUBMODULES: usize = 64;

/// The worker process as the kernel recorded it at exec
/// ([`atpkg::caller_shell::process_args`]): its environment, `KEY=value` in
/// the kernel's order, which every command its Bash tool runs starts from,
/// and its argv (a `--settings` it was launched with,
/// [`Self::settings_env`]) — and git as it runs there ([`Self::view`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkerEnv {
    process: ProcArgs,
    /// The worker's pid — the session's foreground process group's leader —
    /// where it was read from the kernel ([`Self::with_pid`]): what the files
    /// its job holds open are read by.
    pid: Option<u32>,
}

/// Variables the probe does not pass on to its own git. `GIT_CONFIG` names
/// the one file `git config` alone reads instead of the effective set — the
/// probe's `git config --list` would list that file, not what a read loads;
/// `GIT_TRACE*` would have the probe write trace files.
fn withheld_from_probe(key: &str) -> bool {
    key == "GIT_CONFIG" || key.starts_with("GIT_TRACE")
}

impl WorkerEnv {
    /// The worker process `process`.
    #[must_use]
    pub fn new(process: ProcArgs) -> Self {
        Self { process, pid: None }
    }

    /// The same worker, known as process `pid`.
    #[must_use]
    pub fn with_pid(mut self, pid: u32) -> Self {
        self.pid = Some(pid);
        self
    }

    /// The worker's pid, where it was read.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// A worker known only by its environment `vars` (`KEY=value`).
    #[cfg(test)]
    pub(crate) fn from_env(vars: Vec<String>) -> Self {
        Self::new(ProcArgs {
            env: vars,
            ..ProcArgs::default()
        })
    }

    /// The worker's argv, `argv[0]` first (empty where it was not read).
    #[must_use]
    pub fn argv(&self) -> &[String] {
        &self.process.argv
    }

    /// The value of `key`, as `getenv` answers it: the first entry.
    #[must_use]
    pub fn var(&self, key: &str) -> Option<&str> {
        self.process.env_var(key)
    }

    /// The worker's Claude Code directory, for a worker standing in `cwd`, as
    /// Claude Code resolves it ([`claude_dir_at`], with this account's home
    /// from the passwd database): where its user settings, session files,
    /// transcripts and shell snapshots are. `Err` says why it cannot be
    /// named.
    pub fn claude_dir(&self, cwd: &Path) -> Result<PathBuf, String> {
        claude_dir_at(
            self.var("CLAUDE_CONFIG_DIR"),
            self.var("HOME"),
            cwd,
            atpkg::platform::account_home,
        )
    }

    /// A test's worker: this process's `PATH`, no system configuration,
    /// `global` (none: empty) as its global one, and no Claude Code directory,
    /// so nothing of the developer's own configuration is read.
    #[cfg(test)]
    pub(crate) fn hermetic(global: Option<&Path>) -> Self {
        let global = global.unwrap_or(Path::new("/dev/null"));
        Self::from_env(vec![
            format!("PATH={}", std::env::var("PATH").unwrap_or_default()),
            "GIT_CONFIG_NOSYSTEM=1".to_string(),
            format!("GIT_CONFIG_GLOBAL={}", global.display()),
            "CLAUDE_CONFIG_DIR=/nonexistent/aterm-hermetic-claude".to_string(),
        ])
    }

    /// This environment with `kv` (`KEY=value`) set ahead of any earlier value.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with(mut self, kv: &str) -> Self {
        self.process.env.insert(0, kv.to_string());
        self
    }

    /// This worker launched with `argv`.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn launched_with(mut self, argv: &[&str]) -> Self {
        self.process.argv = argv.iter().map(|a| a.to_string()).collect();
        self
    }

    /// The git the worker's shell runs: the first executable `git` on its
    /// `PATH`. Refused when an entry at or ahead of it is one the worker could
    /// have put a `git` in: a relative entry (resolved against wherever the
    /// command runs), or one `worker_writes` (its project, a virtualenv's or
    /// `node_modules`' `bin` in it, a scratch directory). A git planted there
    /// would run here, in the supervisor, before anything is approved.
    fn git_program(&self, worker_writes: &dyn Fn(&Path) -> bool) -> Result<PathBuf, String> {
        let path = self
            .var("PATH")
            .ok_or("a git read whose worker has no PATH (which git runs is unknown)")?;
        let name = if cfg!(windows) { "git.exe" } else { "git" };
        for entry in std::env::split_paths(path) {
            if !entry.is_absolute() {
                return Err(format!(
                    "a git read whose worker PATH has `{}` ahead of git (a directory the \
                     command's own directory decides)",
                    entry.display()
                ));
            }
            if worker_writes(&entry) {
                return Err(format!(
                    "a git read whose worker PATH has `{}` ahead of git (a directory the \
                     worker writes without approval: a git put there would run)",
                    entry.display()
                ));
            }
            let candidate = entry.join(name);
            if is_executable(&candidate) {
                return Ok(candidate);
            }
        }
        Err("a git read with no git on the worker's PATH".to_string())
    }

    /// What a git command run in `dir` by the worker would load. A directory
    /// that does not exist yet is read at its nearest existing ancestor, where
    /// git's own repository discovery would start.
    ///
    /// Only commands that run no configured program read it: `git config
    /// --list`, `rev-parse`, and, once the configuration names no
    /// `core.fsmonitor` program (which reading the index would run),
    /// `ls-files` and `check-attr` for the populated submodules and the filter
    /// drivers the attributes select. The git is [`Self::git_program`]'s.
    pub fn view(
        &self,
        dir: &Path,
        worker_writes: &dyn Fn(&Path) -> bool,
    ) -> Result<GitView, String> {
        let git = Probe {
            env: self,
            program: self.git_program(worker_writes)?,
        };
        git.view_at(dir, 0, &mut 0)
    }

    /// `Err` naming the file and the variable when Claude Code's settings
    /// would give the worker's Bash tool a variable that changes what a git
    /// read loads or which git runs ([`git_relevant`]) with a value other than
    /// the worker process's own: an `env` block anywhere along the settings
    /// chain for an agent launched in `launch`
    /// ([`super::claude_settings::sources`]; the project's files are content an
    /// accept-edits worker writes). Claude Code applies those to its Bash
    /// tool's commands, not to the process environment this check reads, so
    /// such a read escalates, as it does where a settings file cannot be read
    /// whole or does not parse (its `env` could say anything).
    pub fn settings_env(&self, launch: &Path) -> Result<(), String> {
        self.settings_env_where(launch, git_relevant, "a git read")
    }

    /// [`Self::settings_env`] for the variables `relevant` names, the
    /// escalation's reason about `subject` (`a git read`): the same settings
    /// files, read the same way — the shell-startup check
    /// ([`super::shell_startup`]) asks it for the variables that choose the
    /// Bash tool's shell and what that shell reads before a command.
    pub fn settings_env_where(
        &self,
        launch: &Path,
        relevant: fn(&str) -> bool,
        subject: &str,
    ) -> Result<(), String> {
        for (source, settings) in super::claude_settings::parsed(self, launch) {
            let unread = |why: &str| {
                format!(
                    "{subject} whose Bash tool's settings ({source}) {why}: its `env` is unknown"
                )
            };
            let Some(settings) = settings.map_err(|why| unread(&why))? else {
                continue;
            };
            let Some(env) = settings.get("env").and_then(Value::as_object) else {
                continue;
            };
            for (key, value) in env {
                let value = value
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| value.to_string());
                if relevant(key) && self.var(key) != Some(value.as_str()) {
                    return Err(format!(
                        "{subject} whose Bash tool gets `{key}` from Claude Code's settings \
                         ({source}), not the environment this check reads"
                    ));
                }
            }
        }
        Ok(())
    }
}

/// The Claude Code directory of a process whose `CLAUDE_CONFIG_DIR` is
/// `config_dir` and whose `HOME` is `home`, standing in `cwd`, as Claude Code
/// resolves it: `path.resolve(CLAUDE_CONFIG_DIR ?? join(os.homedir(),
/// ".claude"))`. A `CLAUDE_CONFIG_DIR` that is set at all counts, even an
/// empty one (`??`); otherwise `os.homedir()` is the non-empty `HOME`, else
/// the account's home from the passwd database (the worker's account is this
/// process's: [`atpkg::caller_shell::process_args`] reads only this uid's
/// processes). A relative result is taken against `cwd`, lexically. `Err`
/// says what is missing where the account names no home either.
pub(crate) fn claude_dir_at(
    config_dir: Option<&str>,
    home: Option<&str>,
    cwd: &Path,
    account_home: impl FnOnce() -> Option<PathBuf>,
) -> Result<PathBuf, String> {
    let dir = match (config_dir, home.filter(|h| !h.is_empty())) {
        (Some(dir), _) => PathBuf::from(dir),
        (None, Some(home)) => Path::new(home).join(".claude"),
        (None, None) => account_home()
            .ok_or("no CLAUDE_CONFIG_DIR, no HOME, and no home for its account")?
            .join(".claude"),
    };
    Ok(normalise(&cwd.join(dir)))
}

/// Whether a variable changes which git runs or what a git read loads: `PATH`,
/// `HOME` and `XDG_CONFIG_HOME` (where the global config is), and git's own
/// `GIT_*` — but for `GIT_PAGER` (a pager is exempt, the module doc) and
/// `GIT_EDITOR` (a read opens no editor).
fn git_relevant(key: &str) -> bool {
    matches!(key, "PATH" | "HOME" | "XDG_CONFIG_HOME")
        || (key.starts_with("GIT_") && !matches!(key, "GIT_PAGER" | "GIT_EDITOR"))
}

/// The worker's git, ready to run.
struct Probe<'a> {
    env: &'a WorkerEnv,
    program: PathBuf,
}

impl Probe<'_> {
    /// `git --no-pager` in `dir`, with the worker's environment and nothing
    /// of this process's ([`withheld_from_probe`] aside), stdin closed. Built
    /// by `spawnfd::command`, so from a process with no console `git.exe` gets
    /// a windowless one on Windows, not a visible one.
    fn git(&self, dir: &Path) -> Command {
        let mut cmd = aterm_uds::spawnfd::command(&self.program);
        cmd.arg("--no-pager")
            .current_dir(dir)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut set: Vec<&str> = Vec::new();
        for kv in &self.env.process.env {
            let Some((key, value)) = kv.split_once('=') else {
                continue;
            };
            // The first entry is the one git's `getenv` reads.
            if key.is_empty() || withheld_from_probe(key) || set.contains(&key) {
                continue;
            }
            set.push(key);
            cmd.env(key, value);
        }
        cmd
    }

    /// [`WorkerEnv::view`] of `dir`, `depth` submodules down, `count`
    /// populated submodules read so far.
    fn view_at(&self, dir: &Path, depth: usize, count: &mut usize) -> Result<GitView, String> {
        let dir = dir
            .ancestors()
            .find(|a| a.is_dir())
            .unwrap_or_else(|| Path::new("/"));
        let run = |args: &[&str]| self.git(dir).args(args).output();
        let listed = run(&["config", "--list", "-z", "--show-scope"])
            .map_err(|e| format!("cannot read the git config in {}: {e}", dir.display()))?;
        if !listed.status.success() {
            return Err(format!(
                "cannot read the git config in {}: {}",
                dir.display(),
                String::from_utf8_lossy(&listed.stderr).trim()
            ));
        }
        let mut view = GitView {
            config: parse_config_list(&listed.stdout),
            external_diff: self
                .env
                .var("GIT_EXTERNAL_DIFF")
                .filter(|v| !v.is_empty())
                .map(str::to_string),
            ..GitView::default()
        };
        // Outside a repository `rev-parse` fails and there is no hook.
        if let Ok(hook) = run(&["rev-parse", "--git-path", "hooks/post-index-change"])
            && hook.status.success()
        {
            let rel = String::from_utf8_lossy(&hook.stdout).trim().to_string();
            let path = dir.join(rel);
            if is_executable(&path) {
                view.index_hook = Some(path);
            }
        }
        // Reading the index would run a configured fsmonitor program — the
        // very thing checked for. With one set nothing more is read: the
        // hazard escalates on it anyway.
        if fsmonitor_program(&view) {
            return Ok(view);
        }
        // The worktree's top; a bare repository or no repository has none.
        let Some(top) = run(&["rev-parse", "--show-toplevel"])
            .ok()
            .filter(|o| o.status.success())
            .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
            .filter(|t| !t.as_os_str().is_empty())
        else {
            return Ok(view);
        };
        if view
            .config
            .iter()
            .any(|(_, k, _)| filter_driver(k).is_some())
        {
            view.filters_selected = self.filters_selected(&top);
        }
        for path in self.gitlinks(&top)? {
            let sub = normalise(&top.join(&path));
            if !sub.starts_with(&top) || !sub.join(".git").exists() {
                continue; // not populated: a read does not recurse into it
            }
            *count += 1;
            if depth >= MAX_SUBMODULE_DEPTH || *count > MAX_SUBMODULES {
                return Err(format!(
                    "a git read in {}: more submodules than this check reads (over \
                     {MAX_SUBMODULES}, or nested deeper than {MAX_SUBMODULE_DEPTH})",
                    top.display()
                ));
            }
            let nested = self.view_at(&sub, depth + 1, count)?;
            view.submodules.push((sub, nested));
        }
        Ok(view)
    }

    /// The paths of the submodules (gitlinks, mode 160000) in the index of
    /// the worktree at `top`.
    fn gitlinks(&self, top: &Path) -> Result<Vec<String>, String> {
        let out = self
            .git(top)
            .args(["ls-files", "-z", "--stage"])
            .output()
            .map_err(|e| format!("cannot list the index in {}: {e}", top.display()))?;
        if !out.status.success() {
            return Err(format!(
                "cannot list the index in {}: {}",
                top.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .split('\0')
            .filter_map(|entry| {
                let (meta, path) = entry.split_once('\t')?;
                meta.starts_with("160000 ").then(|| path.to_string())
            })
            .collect())
    }

    /// The filter drivers the attributes select for any tracked path of the
    /// worktree at `top` (`git ls-files -z | git check-attr --stdin -z
    /// filter`); `None` when they could not be read.
    fn filters_selected(&self, top: &Path) -> Option<Vec<String>> {
        let paths = self
            .git(top)
            .args(["ls-files", "-z"])
            .output()
            .ok()
            .filter(|o| o.status.success())?
            .stdout;
        let mut child = self
            .git(top)
            .args(["check-attr", "--stdin", "-z", "filter"])
            .stdin(Stdio::piped())
            .spawn()
            .ok()?;
        let mut stdin = child.stdin.take()?;
        // A writer thread: check-attr answers while it reads, and a pipe
        // left full on both sides would stall.
        let writer = std::thread::spawn(move || {
            use std::io::Write as _;
            stdin.write_all(&paths)
        });
        let out = child.wait_with_output().ok()?;
        writer.join().ok()?.ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let fields: Vec<&str> = text.split('\0').collect();
        let mut drivers: Vec<String> = Vec::new();
        for record in fields.chunks(3) {
            if let [_, _, value] = record
                && !matches!(*value, "unspecified" | "unset" | "set" | "")
                && !drivers.iter().any(|d| d == value)
            {
                drivers.push((*value).to_string());
            }
        }
        Some(drivers)
    }
}

/// Whether `view`'s configuration names an fsmonitor program (a boolean is
/// git's own daemon).
fn fsmonitor_program(view: &GitView) -> bool {
    view.config
        .iter()
        .any(|(_, k, v)| k == "core.fsmonitor" && !is_bool(v.as_deref()))
}

/// The driver of a `filter.<driver>.clean|smudge|process` key.
fn filter_driver(key: &str) -> Option<&str> {
    let rest = key.strip_prefix("filter.")?;
    let (driver, var) = rest.rsplit_once('.')?;
    matches!(var, "clean" | "smudge" | "process").then_some(driver)
}

/// `scope NUL key LF value NUL` records (`key NUL` for a bare key).
fn parse_config_list(out: &[u8]) -> Vec<(String, String, Option<String>)> {
    let text = String::from_utf8_lossy(out);
    let mut fields = text.split('\0');
    let mut config = Vec::new();
    while let (Some(scope), Some(entry)) = (fields.next(), fields.next()) {
        if scope.is_empty() && entry.is_empty() {
            break;
        }
        let (key, value) = match entry.split_once('\n') {
            Some((k, v)) => (k, Some(v.to_string())),
            None => (entry, None),
        };
        config.push((scope.to_string(), key.to_string(), value));
    }
    config
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// Whether git reads a config value as a boolean (`core.fsmonitor = true` is
/// git's own daemon, not a program).
fn is_bool(v: Option<&str>) -> bool {
    match v {
        None => true,
        Some(v) => matches!(
            v.to_ascii_lowercase().as_str(),
            "" | "true" | "yes" | "on" | "1" | "false" | "no" | "off" | "0"
        ),
    }
}

/// Whether git reads `v` as boolean false.
fn is_false(v: Option<&str>) -> bool {
    v.is_some_and(|v| {
        matches!(
            v.to_ascii_lowercase().as_str(),
            "false" | "no" | "off" | "0" | ""
        )
    })
}

/// The first thing in `view` that `reads` would run, as a reason naming the
/// key (or the hook) — `None` when nothing it loads runs a program.
pub(crate) fn hazard(view: &GitView, reads: &GitReads) -> Option<String> {
    let enabled = |key: &str| {
        view.config
            .iter()
            .rev()
            .find(|(_, k, _)| k == key)
            .is_some_and(|(_, _, v)| {
                v.as_deref().is_none_or(|v| {
                    matches!(v.to_ascii_lowercase().as_str(), "true" | "yes" | "on" | "1")
                })
            })
    };
    let signatures = reads.signatures || (reads.log_like && enabled("log.showsignature"));
    if view.external_diff.is_some() {
        return Some(
            "git read: `GIT_EXTERNAL_DIFF` is set in the worker's environment: git runs it \
             for every diff"
                .to_string(),
        );
    }
    for (scope, key, value) in &view.config {
        let runs = |what: &str| Some(format!("git read: `{key}` is set ({scope} config): {what}"));
        let k = key.as_str();
        if k == "core.fsmonitor" && !is_bool(value.as_deref()) {
            return runs("git runs it on every read of the index");
        }
        if k == "diff.external" {
            return runs("git runs it for every diff");
        }
        if k == "extensions.partialclone"
            || (k.starts_with("remote.") && k.ends_with(".promisor") && !is_false(value.as_deref()))
        {
            return runs(
                "a partial clone: a read may fetch a missing object from its promisor \
                 remote, and credential helpers, ssh and the remote's programs run there",
            );
        }
        if k.starts_with("diff.") && (k.ends_with(".textconv") || k.ends_with(".command")) {
            return runs("git runs it on a diff of a file the attributes select");
        }
        if let Some(driver) = filter_driver(k)
            && view
                .filters_selected
                .as_ref()
                .is_none_or(|selected| selected.iter().any(|d| d == driver))
        {
            return runs("git runs it on a status or diff of a file the attributes select");
        }
        if signatures && k.starts_with("gpg.") && k.ends_with("program") {
            return runs("git runs it to verify the signatures this read displays");
        }
    }
    if let Some(h) = &view.index_hook {
        return Some(format!(
            "git read: {} is an executable post-index-change hook, which a read that \
             refreshes the index (status, diff) runs",
            h.display()
        ));
    }
    view.submodules.iter().find_map(|(path, sub)| {
        hazard(sub, reads).map(|why| format!("in submodule {}: {why}", path.display()))
    })
}

#[cfg(test)]
#[path = "git_config_tests.rs"]
mod tests;
