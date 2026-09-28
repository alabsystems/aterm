// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a git READ would load that can run a program — the check behind the
//! read-only rule's git reads ([`super::approval::RULE_READ_ONLY`]).
//!
//! **The ruling** (owner, 2026-09-25): git reads under `[harness] approve =
//! "safe"` are NOT blanket-escalated — that would interrupt the person on every
//! `git status`. Instead a git read is approved only when no configuration it
//! would load can execute a program. The classifier
//! ([`crate::supervise::classify`]) judges the LINE; this module judges the
//! MACHINE the line runs on: a worker in accept-edits mode can write the
//! repository's `.git/config`, and `git status` then runs whatever
//! `core.fsmonitor` names.
//!
//! **Where a git read runs** ([`git_reads`]): the command of each segment is
//! found as the classifier's head check finds it
//! ([`crate::supervise::classify::runs`] — through assignments, redirects,
//! `timeout`, `time`, `env` and `xargs`), so no wrapper the classifier sees
//! through hides a git from this check. It runs in the session's cwd, the
//! directory the session's Bash tool stands in as Claude Code's transcript
//! under the worker's own Claude Code directory last recorded it
//! ([`WorkerEnv::claude_dir`], [`crate::harness::footer::shell_cwds`] — the
//! box does not say, and an earlier `cd sub`, which Claude Code runs unasked,
//! moves it), every `cd` / `pushd` target on the line, an `env -C` / `--chdir`
//! directory, and every `-C` target resolved against each of those (a
//! superset — a `cd` inside a subshell is counted as if it were not). A directory this check
//! cannot resolve (an expansion, `cd -`, `~user`, a `-C` that `xargs` input
//! supplies, a relative `cd` a loop repeats) escalates. `--git-dir`/`--work-tree` and `-c` are refused by the
//! classifier before this runs, and so are `GIT_*=` assignments and `env -P`.
//!
//! **What is checked** ([`hazard`]) — the effective configuration git itself
//! reports in each directory (`git config --list --show-scope`: system,
//! global, the repository's local and worktree config, anything
//! `include.path` / `includeIf` pulls in, and `GIT_CONFIG_*`), plus the one
//! hook a read runs, and the same again in every populated submodule,
//! recursively (a `status` or `diff` runs git in each, with that submodule's
//! own `.git/modules/<name>/config`):
//!
//! * `core.fsmonitor` naming a program (a boolean is git's built-in daemon);
//! * `diff.external`, `diff.<driver>.textconv`, `diff.<driver>.command`, and
//!   `GIT_EXTERNAL_DIFF` in the worker's environment;
//! * `filter.<driver>.clean` / `smudge` / `process` when the attributes select
//!   that driver for a tracked path (`git status` runs the clean filter on a
//!   modified file; a driver no path selects never runs — `git lfs install`
//!   puts one in every global config);
//! * a partial clone (`extensions.partialClone`, a `remote.<name>.promisor`):
//!   a read may fetch a missing object from the promisor remote, where
//!   credential helpers, ssh and the remote's programs run;
//! * `gpg.program` / `gpg.<format>.program`, when the read displays
//!   signatures (`--show-signature`, a `%G` format, `tag -v`, or
//!   `log.showSignature` on a `log`/`show`);
//! * an executable `post-index-change` hook where the read would run it
//!   (`git status` and `git diff` write the refreshed index).
//!
//! A read that contacts a remote (`git remote show` without `-n`) escalates
//! outright: `credential.helper`, `core.sshCommand`, `core.askPass` and the
//! remote's own programs all run there.
//!
//! **Read as the WORKER reads it** ([`WorkerEnv`]). Every probe runs the git
//! the worker's own `PATH` finds, with the worker process's environment at
//! exec — its `HOME` and `XDG_CONFIG_HOME` (the global config),
//! `GIT_CONFIG_GLOBAL` / `GIT_CONFIG_SYSTEM` / `GIT_CONFIG_NOSYSTEM`,
//! `GIT_CONFIG_COUNT` and `GIT_CONFIG_PARAMETERS`, `GIT_DIR` and the rest of
//! repository discovery — which the supervisor reads from the kernel, bound
//! to the session (`Session::worker_env`), and which every command the
//! worker's Bash tool runs starts from. aterm's own environment is never the
//! stand-in: a window started from the Dock has neither the worker's `PATH`
//! (and so not its git, whose compiled-in system config may differ) nor its
//! variables. Escalated instead, naming why:
//!
//! * a worker whose environment cannot be read;
//! * a `PATH` with an entry ahead of git that the worker could have put a git
//!   in ([`WorkerEnv::view`]): a relative one, or one the worker writes
//!   without approval — its project (a virtualenv's `bin`, `node_modules/.bin`),
//!   a directory its Bash tool stands in, a scratch directory. The probe runs
//!   that git in the supervisor before anything is approved, so it is never
//!   run;
//! * a variable Claude Code's settings give the Bash tool and not the process
//!   ([`WorkerEnv::settings_env`]): a `GIT_*` (but the pager and the editor),
//!   `HOME`, `XDG_CONFIG_HOME` or `PATH` in the `env` of the managed settings,
//!   a `--settings` the worker was launched with, the project's
//!   `.claude/settings.json` / `settings.local.json` (repository content an
//!   accept-edits worker writes), or the user's `settings.json`.
//!
//! **Decided (2026-09-26, under the owner's standing direction), two
//! refinements of the 2026-09-25 ruling:**
//!
//! * **`core.pager` / `pager.*` are exempt.** git starts a pager only when its
//!   stdout is a terminal, and the boxes this rule judges are Claude Code Bash
//!   commands, which have none to give it — measured 2026-09-27 from inside the
//!   Bash tool: stdin is `/dev/null`, stdout and stderr are pipes, and there is
//!   no controlling terminal (`/dev/tty` answers ENXIO), so not even a redirect
//!   or a descriptor copy can hand git one. Two reads start a program without
//!   a terminal, and the classifier refuses both before this runs: `git grep
//!   -O` / `--open-files-in-pager` in every spelling git's option parser
//!   accepts (`-nO`, `-iOless`, `--open`, `--op`), which runs the pager on the
//!   matching files; and `git <cmd> --help` (`git --help <cmd>`, `git -h
//!   <cmd>`), which is `git help` and runs the man or web viewer
//!   (`man.<tool>.cmd`, `browser.<tool>.cmd`) a repository's config may name.
//!   Escalating on a pager (`delta`, `less -R`, common in global configs)
//!   would be the blanket escalation the ruling refused. (`GIT_PAGER` /
//!   `PAGER` in the worker's environment likewise.)
//! * **`filter.<driver>.*` counts only where the repository's attributes
//!   select it** for a tracked path (`git check-attr filter`); an unreadable
//!   attribute set counts every configured filter.
//!
//! **What stays open.** A box that runs on another machine (`(runs on <m>)`)
//! escalates: its configuration is that machine's. And the Bash tool's
//! environment is more than the process's and the settings': Claude Code
//! sources its shell snapshot before each command (the login shell's `PATH`,
//! and its aliases and functions — a `git` alias or function runs instead of
//! the git probed), a SessionStart hook's `CLAUDE_ENV_FILE`, and zsh's
//! `~/.zshenv`, and sets variables of its own (`GIT_EDITOR`). Those come from
//! the person's own rc files and hooks, not from the repository — an
//! accept-edits worker writes them unasked only when it was launched in
//! `$HOME` itself, where it can plant code the person's next shell runs in
//! any case — and this check does not read them.

use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

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
fn normalise(p: &Path) -> PathBuf {
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
        Self { process }
    }

    /// A worker known only by its environment `vars` (`KEY=value`).
    #[cfg(test)]
    pub(crate) fn from_env(vars: Vec<String>) -> Self {
        Self::new(ProcArgs {
            env: vars,
            ..ProcArgs::default()
        })
    }

    /// The value of `key`, as `getenv` answers it: the first entry.
    #[must_use]
    pub fn var(&self, key: &str) -> Option<&str> {
        self.process.env_var(key)
    }

    /// The worker's Claude Code directory: its `CLAUDE_CONFIG_DIR`, else its
    /// `$HOME/.claude` ([`crate::harness::footer::claude_dir_of`]) — where its
    /// session files and transcripts say where its Bash tool stands.
    #[must_use]
    pub fn claude_dir(&self) -> Option<PathBuf> {
        crate::harness::footer::claude_dir_of(self.var("CLAUDE_CONFIG_DIR"), self.var("HOME"))
    }

    /// A test's worker: this process's `PATH`, no system configuration, and
    /// `global` (none: empty) as its global one — what the repository alone
    /// contributes, whatever the developer's own config says.
    #[cfg(test)]
    pub(crate) fn hermetic(global: Option<&Path>) -> Self {
        let global = global.unwrap_or(Path::new("/dev/null"));
        Self::from_env(vec![
            format!("PATH={}", std::env::var("PATH").unwrap_or_default()),
            "GIT_CONFIG_NOSYSTEM=1".to_string(),
            format!("GIT_CONFIG_GLOBAL={}", global.display()),
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
    /// have put a `git` in: a relative entry, which the shell resolves against
    /// whatever directory the command runs in, and an entry `worker_writes`
    /// (a directory the worker writes without anyone approving it: its
    /// project, a virtualenv's or `node_modules`' `bin` in it, a scratch
    /// directory). A git planted there would run — here, in the supervisor,
    /// before anything is approved, and again as the approved command.
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
    /// --list`, `rev-parse`, and — once the configuration names no
    /// `core.fsmonitor` program, which reading the index would run —
    /// `ls-files` and `check-attr`, for the populated submodules (each viewed
    /// in turn, recursively) and the filter drivers the attributes select.
    /// The git is the worker's ([`Self::git_program`]), found on a `PATH`
    /// with no entry `worker_writes` ahead of it.
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
    /// read loads or which git runs ([`git_relevant`]) with a value other
    /// than the worker process's own: an `env` block in the managed settings,
    /// a `--settings` the worker was launched with (a file, relative to
    /// `launch`, or inline JSON), the project's `.claude/settings.local.json`
    /// and `.claude/settings.json` in `launch` (repository content an
    /// accept-edits worker writes), or the user's `settings.json` in the
    /// worker's Claude directory. Claude Code applies those to the commands
    /// its Bash tool runs, not to the process environment this check reads,
    /// so where one would change a git read the read escalates instead — and
    /// so it does where a settings file is there but cannot be read whole or
    /// does not parse ([`read_settings`]), since its `env` could say
    /// anything. A file that is not there sets nothing.
    pub fn settings_env(&self, launch: &Path) -> Result<(), String> {
        let mut sources: Vec<(String, Result<Option<String>, String>)> = Vec::new();
        for managed in MANAGED_SETTINGS {
            sources.push((managed.to_string(), read_settings(Path::new(managed))));
        }
        let argv = &self.process.argv;
        for (i, arg) in argv.iter().enumerate() {
            let value = match arg.strip_prefix("--settings=") {
                Some(v) => v,
                None if arg == "--settings" => match argv.get(i + 1) {
                    Some(v) => v.as_str(),
                    None => continue,
                },
                None => continue,
            };
            if value.trim_start().starts_with('{') {
                sources.push(("--settings".to_string(), Ok(Some(value.to_string()))));
            } else {
                let file = launch.join(value);
                sources.push((file.display().to_string(), read_settings(&file)));
            }
        }
        let mut files = vec![
            launch.join(".claude/settings.local.json"),
            launch.join(".claude/settings.json"),
        ];
        files.extend(self.claude_dir().map(|dir| dir.join("settings.json")));
        for file in files {
            let text = read_settings(&file);
            sources.push((file.display().to_string(), text));
        }
        for (source, text) in sources {
            let unread = |why: &str| {
                format!(
                    "a git read whose Bash tool's settings ({source}) {why}: its `env` is unknown"
                )
            };
            let Some(text) = text.map_err(|why| unread(&why))? else {
                continue;
            };
            let settings =
                aterm_json::from_str::<Value>(&text).map_err(|_| unread("do not parse"))?;
            let Some(env) = settings.get("env").and_then(Value::as_object) else {
                continue;
            };
            for (key, value) in env {
                let value = value
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| value.to_string());
                if git_relevant(key) && self.var(key) != Some(value.as_str()) {
                    return Err(format!(
                        "a git read whose Bash tool gets `{key}` from Claude Code's settings \
                         ({source}), not the environment this check reads"
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Claude Code's managed (system-wide) settings file, where one is read.
#[cfg(target_os = "macos")]
const MANAGED_SETTINGS: &[&str] =
    &["/Library/Application Support/ClaudeCode/managed-settings.json"];
#[cfg(target_os = "linux")]
const MANAGED_SETTINGS: &[&str] = &["/etc/claude-code/managed-settings.json"];
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const MANAGED_SETTINGS: &[&str] = &[];

/// The most of a settings file this check reads; a larger one is not read.
const MAX_SETTINGS_BYTES: u64 = 1 << 20;

/// A settings file's text: `Ok(None)` where there is none, `Err` naming why
/// where one is there but cannot be read whole — not a regular file (a FIFO
/// planted there is never opened to block on), larger than
/// [`MAX_SETTINGS_BYTES`], or unreadable.
fn read_settings(path: &Path) -> Result<Option<String>, String> {
    if let Err(e) = std::fs::symlink_metadata(path)
        && e.kind() == std::io::ErrorKind::NotFound
    {
        return Ok(None);
    }
    let text = crate::harness::footer::read_small(path, MAX_SETTINGS_BYTES + 1)
        .ok_or("cannot be read as a regular file")?;
    if text.len() as u64 > MAX_SETTINGS_BYTES {
        return Err(format!("are larger than {MAX_SETTINGS_BYTES} bytes"));
    }
    Ok(Some(text))
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
    /// of this process's ([`withheld_from_probe`] aside), stdin closed.
    fn git(&self, dir: &Path) -> Command {
        let mut cmd = Command::new(&self.program);
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

/// Whether `view`'s configuration names an fsmonitor PROGRAM (a boolean is
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
