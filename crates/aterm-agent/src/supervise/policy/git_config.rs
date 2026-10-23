// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Git configuration hazard check for read-only command approval.

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
    /// A read that displays signatures on its own.
    pub signatures: bool,
    /// A `log`/`show`/`whatchanged`/`reflog`.
    pub log_like: bool,
    /// The first read that contacts a remote, as a reason.
    pub remote: Option<String>,
}

/// The git reads on `line`, run from any of `cwds`.
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
            || (sub == "remote" && crate::supervise::classify::git_remote_show_contacts(args));
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

/// Resolve dot and parent components lexically.
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
    /// `(scope, key, value)` as `git config --list --show-scope` reports them.
    pub config: Vec<(String, String, Option<String>)>,
    /// The executable `post-index-change` hook of the repository.
    pub index_hook: Option<PathBuf>,
    /// The filter drivers the repository's attributes select for a tracked path.
    pub filters_selected: Option<Vec<String>>,
    /// Each populated submodule, recursively, with what git loads there.
    pub submodules: Vec<(PathBuf, GitView)>,
    /// The worker's `GIT_EXTERNAL_DIFF` environment value.
    pub external_diff: Option<String>,
}

const MAX_SUBMODULE_DEPTH: usize = 8;
const MAX_SUBMODULES: usize = 64;

/// How long one git the check runs may take before it is killed and the read
/// is refused (the line escalates, as for any read that cannot be made).
#[cfg(not(test))]
const GIT_READ_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
#[cfg(test)]
const GIT_READ_WAIT: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;

/// The worker process as the kernel recorded it at exec ([`atpkg::caller_shell::process_args`]).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct WorkerEnv {
    process: ProcArgs,
    /// The worker's pid.
    pid: Option<u32>,
    /// The digest a harness tape knows this worker by.
    digest: Option<u64>,
}

impl std::fmt::Debug for WorkerEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerEnv")
            .field("pid", &self.pid)
            .field("vars", &self.process.env.len())
            .field("argv", &self.process.argv.len())
            .finish_non_exhaustive()
    }
}

fn withheld_from_probe(key: &str) -> bool {
    key == "GIT_CONFIG" || key.starts_with("GIT_TRACE")
}

impl WorkerEnv {
    /// The worker process `process`.
    #[must_use]
    pub const fn new(process: ProcArgs) -> Self {
        Self {
            process,
            pid: None,
            digest: None,
        }
    }

    /// The digest a harness tape names this worker by in each probe's request (`super::probe`).
    #[must_use]
    pub fn digest(&self, seed: u64) -> u64 {
        self.digest.unwrap_or_else(|| {
            let mut h = aterm_zstd::xxh64::Xxh64::new(seed);
            for (part, words) in [(0u8, &self.process.env), (1, &self.process.argv)] {
                h.update(&[part]);
                for word in words {
                    h.update(&(word.len() as u64).to_le_bytes());
                    h.update(word.as_bytes());
                }
            }
            h.digest()
        })
    }

    /// This worker with its [`Self::digest`] under `seed` fixed.
    #[must_use]
    pub fn digested(mut self, seed: u64) -> Self {
        self.digest = Some(self.digest(seed));
        self
    }

    /// A worker known only by what a harness tape holds of it.
    #[must_use]
    pub fn recorded(digest: u64, pid: Option<u32>) -> Self {
        Self {
            process: ProcArgs::default(),
            pid,
            digest: Some(digest),
        }
    }

    /// The same worker, known as process `pid`.
    #[must_use]
    pub const fn with_pid(mut self, pid: u32) -> Self {
        self.pid = Some(pid);
        self
    }

    /// The worker's pid, where it was read.
    #[must_use]
    pub const fn pid(&self) -> Option<u32> {
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

    /// The value of `key`, as `getenv` answers it.
    #[must_use]
    pub fn var(&self, key: &str) -> Option<&str> {
        self.process.env_var(key)
    }

    /// The worker's Claude Code directory, for a worker standing in `cwd`.
    pub fn claude_dir(&self, cwd: &Path) -> Result<PathBuf, String> {
        use crate::harness::accounts::{HOME_ENV, HOMEDIR_ENV};
        claude_dir_at(
            self.var(HOME_ENV),
            self.var(HOMEDIR_ENV),
            cwd,
            atpkg::platform::account_home,
        )
    }

    /// A test's worker: this process's `PATH`, no system configuration.
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

    /// What a git command run in `dir` by the worker would load.
    pub fn raw_git_view(
        &self,
        dir: &Path,
        worker_writes: &dyn Fn(&Path) -> bool,
    ) -> Result<GitView, String> {
        // A `/`-rooted directory names no drive on Windows: the Bash tool's
        // shell there (Git Bash) reads `/c/x` as `C:\x` and its `/tmp` as a
        // folder of its own, where this process would read `\c\x` and `\tmp`
        // on its current drive — another directory's configuration.
        if dir.has_root() && !dir.is_absolute() {
            return Err(format!(
                "a git read in {} (a `/` path names no drive on Windows: where it runs is \
                 unknown)",
                dir.display()
            ));
        }
        let git = Probe {
            env: self,
            program: self.git_program(worker_writes)?,
        };
        git.view_at(dir, 0, &mut 0)
    }

    /// `Err` naming the file and variable when Claude Code settings inject a git variable.
    pub fn git_settings_env(&self, launch: &Path) -> Result<(), String> {
        self.settings_env_where(launch, git_relevant, "a git read")
    }

    /// [`Self::git_settings_env`] for the variables `relevant` names.
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

fn git_failed(what: &str, at: &Path, status: std::process::ExitStatus, command: &str) -> String {
    format!(
        "{what} {} (git {status}; `{command}` there says why)",
        at.display()
    )
}

/// The Claude Code directory of a process whose `CLAUDE_CONFIG_DIR` is `config_dir`.
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

fn git_relevant(key: &str) -> bool {
    matches!(key, "PATH" | "HOME" | "XDG_CONFIG_HOME")
        || (key.starts_with("GIT_") && !matches!(key, "GIT_PAGER" | "GIT_EDITOR"))
}

struct Probe<'a> {
    env: &'a WorkerEnv,
    program: PathBuf,
}

impl Probe<'_> {
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
            if key.is_empty() || withheld_from_probe(key) || set.contains(&key) {
                continue;
            }
            set.push(key);
            cmd.env(key, value);
        }
        cmd
    }

    fn view_at(&self, dir: &Path, depth: usize, count: &mut usize) -> Result<GitView, String> {
        let dir = dir
            .ancestors()
            .find(|a| a.is_dir())
            .unwrap_or_else(|| Path::new("/"));
        let run = |args: &[&str]| {
            let mut cmd = self.git(dir);
            cmd.args(args);
            aterm_uds::deadline::output_within(&mut cmd, GIT_READ_WAIT)
        };
        let listed = run(&["config", "--list", "-z", "--show-scope"])
            .map_err(|e| format!("cannot read the git config in {}: {e}", dir.display()))?;
        if !listed.status.success() {
            return Err(git_failed(
                "cannot read the git config in",
                dir,
                listed.status,
                "git config --list",
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
        if let Ok(hook) = run(&["rev-parse", "--git-path", "hooks/post-index-change"])
            && hook.status.success()
        {
            let rel = String::from_utf8_lossy(&hook.stdout).trim().to_string();
            let path = dir.join(rel);
            if is_executable(&path) {
                view.index_hook = Some(path);
            }
        }
        if fsmonitor_program(&view) {
            return Ok(view);
        }
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

    fn gitlinks(&self, top: &Path) -> Result<Vec<String>, String> {
        let mut cmd = self.git(top);
        cmd.args(["ls-files", "-z", "--stage"]);
        let out = aterm_uds::deadline::output_within(&mut cmd, GIT_READ_WAIT)
            .map_err(|e| format!("cannot list the index in {}: {e}", top.display()))?;
        if !out.status.success() {
            return Err(git_failed(
                "cannot list the index in",
                top,
                out.status,
                "git ls-files --stage",
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

    fn filters_selected(&self, top: &Path) -> Option<Vec<String>> {
        let mut cmd = self.git(top);
        cmd.args(["ls-files", "-z"]);
        let paths = aterm_uds::deadline::output_within(&mut cmd, GIT_READ_WAIT)
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

fn fsmonitor_program(view: &GitView) -> bool {
    view.config
        .iter()
        .any(|(_, k, v)| k == "core.fsmonitor" && !is_bool(v.as_deref()))
}

fn filter_driver(key: &str) -> Option<&str> {
    let rest = key.strip_prefix("filter.")?;
    let (driver, var) = rest.rsplit_once('.')?;
    matches!(var, "clean" | "smudge" | "process").then_some(driver)
}

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

fn is_bool(v: Option<&str>) -> bool {
    match v {
        None => true,
        Some(v) => matches!(
            v.to_ascii_lowercase().as_str(),
            "" | "true" | "yes" | "on" | "1" | "false" | "no" | "off" | "0"
        ),
    }
}

const PARTIAL_CLONE: &str = "a partial clone: a read may fetch a missing object from its \
                             promisor remote, and credential helpers, ssh and the remote's \
                             programs run there";

fn is_false(v: Option<&str>) -> bool {
    v.is_some_and(|v| {
        matches!(
            v.to_ascii_lowercase().as_str(),
            "false" | "no" | "off" | "0" | ""
        )
    })
}

fn is_true(v: Option<&str>) -> bool {
    v.is_none_or(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "yes" | "on" | "1"))
}

/// What a configuration key is to [`hazard`]: the ONE place a key's name is
/// tested, read by [`hazard`] to judge a view and by [`GitView::project`] to
/// decide which entries a harness tape keeps ([`consulted`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Consulted<'a> {
    /// `core.fsmonitor`: a program unless its value is a boolean.
    Fsmonitor,
    /// `diff.external`.
    DiffExternal,
    /// `extensions.partialclone`.
    PartialClone,
    /// `remote.<name>.promisor`.
    Promisor,
    /// `diff.<driver>.textconv` / `.command`.
    DiffDriver,
    /// `filter.<driver>.clean|smudge|process`, with its driver.
    Filter(&'a str),
    /// `gpg.program`, `gpg.<format>.program`.
    Signer,
    /// `log.showsignature`: makes a plain `log` display signatures.
    ShowSignature,
}

fn consult(key: &str) -> Option<Consulted<'_>> {
    Some(match key {
        "core.fsmonitor" => Consulted::Fsmonitor,
        "diff.external" => Consulted::DiffExternal,
        "extensions.partialclone" => Consulted::PartialClone,
        "log.showsignature" => Consulted::ShowSignature,
        k if k.starts_with("remote.") && k.ends_with(".promisor") => Consulted::Promisor,
        k if k.starts_with("diff.") && (k.ends_with(".textconv") || k.ends_with(".command")) => {
            Consulted::DiffDriver
        }
        k if k.starts_with("gpg.") && k.ends_with("program") => Consulted::Signer,
        k => Consulted::Filter(filter_driver(k)?),
    })
}

/// Whether [`hazard`] reads a configuration key named `key` at all.
#[must_use]
pub(crate) fn consulted(key: &str) -> bool {
    consult(key).is_some()
}

/// The version of [`GitView::project`]'s rule.
pub const PROJECTION: u32 = 1;

/// The one value a projected entry keeps in place of anything but a boolean ([`GitView::project`]).
pub const VALUE_SET: &str = "<set>";

fn value_class(v: Option<&str>) -> Option<String> {
    let v = v?;
    Some(
        if is_true(Some(v)) {
            "true"
        } else if is_false(Some(v)) {
            "false"
        } else {
            VALUE_SET
        }
        .to_string(),
    )
}

impl GitView {
    /// This view as a harness tape keeps it.
    #[must_use]
    pub fn project(&self) -> GitView {
        GitView {
            config: self
                .config
                .iter()
                .filter(|(_, key, _)| consulted(key))
                .map(|(scope, key, value)| {
                    (scope.clone(), key.clone(), value_class(value.as_deref()))
                })
                .collect(),
            index_hook: self.index_hook.clone(),
            filters_selected: self.filters_selected.clone(),
            submodules: self
                .submodules
                .iter()
                .map(|(path, sub)| (path.clone(), sub.project()))
                .collect(),
            external_diff: self.external_diff.as_ref().map(|_| VALUE_SET.to_string()),
        }
    }
}

/// The first thing in `view` that `reads` would run, as a reason naming the key (or the hook).
pub(crate) fn hazard(view: &GitView, reads: &GitReads) -> Option<String> {
    let shows = view
        .config
        .iter()
        .rev()
        .find(|(_, k, _)| consult(k) == Some(Consulted::ShowSignature))
        .is_some_and(|(_, _, v)| is_true(v.as_deref()));
    let signatures = reads.signatures || (reads.log_like && shows);
    if view.external_diff.is_some() {
        return Some(
            "git read: `GIT_EXTERNAL_DIFF` is set in the worker's environment: git runs it \
             for every diff"
                .to_string(),
        );
    }
    for (scope, key, value) in &view.config {
        let runs = |what: &str| Some(format!("git read: `{key}` is set ({scope} config): {what}"));
        let value = value.as_deref();
        match consult(key) {
            Some(Consulted::Fsmonitor) if !is_bool(value) => {
                return runs("git runs it on every read of the index");
            }
            Some(Consulted::DiffExternal) => return runs("git runs it for every diff"),
            Some(Consulted::PartialClone) => return runs(PARTIAL_CLONE),
            Some(Consulted::Promisor) if !is_false(value) => return runs(PARTIAL_CLONE),
            Some(Consulted::DiffDriver) => {
                return runs("git runs it on a diff of a file the attributes select");
            }
            Some(Consulted::Filter(driver))
                if view
                    .filters_selected
                    .as_ref()
                    .is_none_or(|selected| selected.iter().any(|d| d == driver)) =>
            {
                return runs("git runs it on a status or diff of a file the attributes select");
            }
            Some(Consulted::Signer) if signatures => {
                return runs("git runs it to verify the signatures this read displays");
            }
            _ => {}
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
