// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CHANGE A REVIEW SEES (design §9.1): by default the branch's change, the diff from
//! `merge-base(HEAD, @{upstream} or origin/HEAD)` to the working tree, plus untracked,
//! non-ignored files as new-file hunks; `--uncommitted` narrows the base to `HEAD`.
//!
//! Git runs hardened, as a reviewer's input must not run anything the repository
//! configures: no system config, no lazy fetch, no prompt, `core.fsmonitor` and hooks off,
//! submodules ignored, no external diff, no textconv, no colour, and the person's own filter
//! drivers (git-lfs, for one) switched off. A repository whose own configuration names a
//! filter driver (which a diff of the working tree would run) is refused, naming it, and so
//! is one whose configuration cannot be read; and so is one whose `git config aterm.aiReview`
//! is false. Files whose names look like secrets (`approval::default_secrets`) are held back;
//! links are named and never read through.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::facts::{Job, Runner};

/// The largest change a review sends.
pub const CHANGE_MAX: usize = 200 * 1024;
/// The largest untracked file read into the change.
pub const UNTRACKED_FILE_MAX: u64 = 64 * 1024;
/// Each git run's time limit.
pub const GIT_BUDGET: Duration = Duration::from_secs(20);

/// Which base the change is taken from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Base {
    /// The branch's: the merge base with its upstream, else with `origin/HEAD`, else `HEAD`.
    Branch,
    /// `HEAD`: only what is not committed.
    Uncommitted,
    /// The merge base with this ref.
    Ref(String),
}

/// The change, ready to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The repository's top directory (as git resolves it).
    pub root: PathBuf,
    /// What it is, in a person's words.
    pub describes: String,
    /// The diff text, untracked files included.
    pub diff: String,
    /// How many files it touches.
    pub files: usize,
    /// Paths held back because their names look like secrets, repository-relative.
    pub held_back: Vec<String>,
    /// Paths that are symbolic links, repository-relative: never read through, and denied
    /// to a reviewer that reads the repository.
    pub links: Vec<String>,
}

/// Why no change could be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused(pub String);

/// A path as the file system resolves it, or as given where it cannot be resolved.
#[must_use]
pub fn real(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The repositories a directory is in, found without running anything: every ancestor
/// (itself included) holding a `.git` entry, nearest first, each resolved.
#[must_use]
pub fn repo_roots_of(cwd: &Path) -> Vec<PathBuf> {
    real(cwd)
        .ancestors()
        .filter(|dir| dir.join(".git").symlink_metadata().is_ok())
        .map(Path::to_path_buf)
        .collect()
}

/// The git a review runs: the first `PATH` entry with a `git` file whose resolved directory
/// and executable target are inside neither `cwd` nor any repository `cwd` is in. A
/// repository's git would run before any hardening applies, even when an outside directory
/// links to it. This checks the selected paths; it does not pin identities against swaps.
#[must_use]
pub fn git_program(path_var: Option<&str>, cwd: &Path) -> Option<PathBuf> {
    let mut fenced = repo_roots_of(cwd);
    fenced.push(real(cwd));
    std::env::split_paths(path_var?)
        .filter(|dir| dir.is_absolute())
        .filter_map(|dir| std::fs::canonicalize(dir).ok())
        .filter(|dir| !fenced.iter().any(|f| dir.starts_with(f)))
        .map(|dir| dir.join("git"))
        .filter_map(|path| std::fs::canonicalize(path).ok())
        .filter(|path| !fenced.iter().any(|f| path.starts_with(f)))
        .find(|path| {
            let Ok(meta) = path.metadata() else {
                return false;
            };
            if !meta.is_file() {
                return false;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                meta.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            true
        })
}

fn git_env(var: &dyn Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = ["HOME", "USER", "LOGNAME"]
        .into_iter()
        .filter_map(|k| var(k).map(|v| (k.to_string(), v)))
        .collect();
    for (k, v) in [
        ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
        ("LANG", "C"),
        ("LC_ALL", "C"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_OPTIONAL_LOCKS", "0"),
    ] {
        env.push((k.to_string(), v.to_string()));
    }
    env
}

/// Runs git hardened in `cwd`; its stdout on exit 0 (or on `ok_exits`).
struct Git<'a> {
    runner: &'a dyn Runner,
    program: PathBuf,
    env: Vec<(String, String)>,
    cwd: PathBuf,
    /// `-c` settings that switch the person's own filter drivers off.
    filters_off: Vec<String>,
}

impl Git<'_> {
    fn run(&self, args: &[&str], ok_exits: &[i32]) -> Result<String, String> {
        let mut all: Vec<String> = [
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "diff.external=",
            "-c",
            "diff.ignoreSubmodules=all",
            "-c",
            "color.ui=false",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        for setting in &self.filters_off {
            all.push("-c".to_string());
            all.push(setting.clone());
        }
        all.push("--no-pager".to_string());
        all.extend(args.iter().map(|s| (*s).to_string()));
        let ran = self
            .runner
            .run(&Job {
                program: &self.program,
                args: &all,
                env: &self.env,
                stdin: None,
                cwd: Some(&self.cwd),
                budget: GIT_BUDGET,
            })
            .map_err(|e| format!("git {}: {e:?}", args.first().copied().unwrap_or_default()))?;
        // Runner keeps a bounded tail. A full capture may have lost paths or filter
        // settings at its front, so it cannot authorize or describe a complete change.
        if ran.stdout.len() >= super::facts::OUTPUT_KEEP {
            return Err("git output reached the capture limit".to_string());
        }
        match ran.exit {
            Some(0) => Ok(ran.stdout),
            Some(code) if ok_exits.contains(&code) => Ok(ran.stdout),
            other => Err(format!(
                "git {} exited {other:?}",
                args.first().copied().unwrap_or_default()
            )),
        }
    }
}

/// The secret rules a review applies: the approval policy's, for `$HOME` as given and as
/// resolved (a home reached through a link is the same home).
#[must_use]
pub fn secret_rules(home: Option<&Path>) -> Vec<crate::supervise::policy::approval::SecretRule> {
    let mut rules = crate::supervise::policy::approval::default_secrets(home);
    if let Some(h) = home {
        let resolved = real(h);
        if resolved != h {
            rules.extend(crate::supervise::policy::approval::default_secrets(Some(
                &resolved,
            )));
        }
    }
    rules
}

/// Whether a path in the repository at `root` looks like a secret: by a component or
/// basename of the secret rules, or by lying under one of their secret places that the
/// repository CONTAINS (a dotfiles repository rooted at `$HOME` holds `.config/gh` and
/// `.claude.json`). A repository that itself sits inside such a place (one in a cloud folder
/// under `~/Library`) is not secret for that.
#[must_use]
pub fn looks_secret(
    root: &Path,
    rel: &str,
    rules: &[crate::supervise::policy::approval::SecretRule],
) -> bool {
    use crate::supervise::policy::approval::SecretRule;
    let comps: Vec<String> = rel.split('/').map(str::to_ascii_lowercase).collect();
    rules.iter().any(|rule| match rule {
        SecretRule::Component(name) => comps.iter().any(|c| c.eq_ignore_ascii_case(name)),
        SecretRule::Basename(glob) => comps
            .last()
            .is_some_and(|c| crate::supervise::classify::glob_match(&glob.to_ascii_lowercase(), c)),
        SecretRule::Under(place) => place.starts_with(root) && root.join(rel).starts_with(place),
    })
}

/// Where an open file really is, as the kernel names it.
#[cfg(target_os = "macos")]
fn path_of_open(file: &std::fs::File) -> Option<PathBuf> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;
    let mut buf = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: F_GETPATH writes at most PATH_MAX bytes, NUL-terminated, into `buf`.
    let got = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buf.as_mut_ptr()) };
    if got == -1 {
        return None;
    }
    let len = buf.iter().position(|b| *b == 0)?;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..len])))
}

/// Where an open file really is, as the kernel names it.
#[cfg(target_os = "linux")]
fn path_of_open(file: &std::fs::File) -> Option<PathBuf> {
    use std::os::fd::AsRawFd as _;
    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).ok()
}

/// Where an open file really is: unknown on this platform, so no untracked file is read.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn path_of_open(_file: &std::fs::File) -> Option<PathBuf> {
    None
}

/// An untracked file as a new-file hunk; `None` for one that is binary, too large,
/// unreadable, or not where its name says: it is opened without following a link at its
/// last component, and kept only when the kernel's path of the open file is the repository's
/// own path for it, so a directory swapped for a link between listing and reading is never
/// read through.
fn new_file_hunk(root: &Path, rel: &str) -> Option<String> {
    use std::io::Read as _;
    let path = root.join(rel);
    let mut open = std::fs::OpenOptions::new();
    open.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = open.open(&path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > UNTRACKED_FILE_MAX {
        return None;
    }
    if path_of_open(&file)? != real(root).join(rel) {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(UNTRACKED_FILE_MAX + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let text = String::from_utf8(bytes).ok()?;
    if text.contains('\0') {
        return None;
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut out = format!(
        "diff --git a/{rel} b/{rel}\nnew file (untracked)\n--- /dev/null\n+++ b/{rel}\n@@ -0,0 +1,{} @@\n",
        lines.len()
    );
    for l in lines {
        out.push('+');
        out.push_str(l);
        out.push('\n');
    }
    Some(out)
}

/// Reads the filter drivers git would load, by scope. The person's own (global) ones are
/// switched off for the review's git runs; one the repository configures refuses. Read with
/// `--show-origin`, known to every git this runs on; a configuration that cannot be read
/// refuses too, so the guard fails closed.
fn filters(git: &Git<'_>) -> Result<Vec<String>, Refused> {
    let listed = git
        .run(
            &["config", "--show-origin", "--get-regexp", r"^filter\."],
            &[1],
        )
        .map_err(|_| Refused("this repository's filter drivers could not be read".to_string()))?;
    // The person's own files: where `--global` finds its settings. A line from any other
    // file (the repository's `.git/config`, a file it includes) or from the command line is
    // the repository's, even under a key the global file uses too.
    let global_origins: Vec<String> = git
        .run(
            &[
                "config",
                "--global",
                "--show-origin",
                "--get-regexp",
                r"^filter\.",
            ],
            &[1],
        )
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('\t').map(|(o, _)| o.to_string()))
        .collect();
    let mut off = Vec::new();
    for line in listed.lines() {
        // `file:<path>\t<key> <value>` (or `command line:` for `-c`).
        let Some((origin, rest)) = line.split_once('\t') else {
            return Err(Refused(
                "this repository's filter drivers could not be read".to_string(),
            ));
        };
        let key = rest.split_whitespace().next().unwrap_or_default();
        if !global_origins.iter().any(|g| g == origin) {
            return Err(Refused(format!(
                "this repository configures a filter driver ({}), which a diff would run",
                crate::harness::one_line(key, 60)
            )));
        }
        if let Some(name) = key
            .strip_prefix("filter.")
            .and_then(|k| k.rsplit_once('.'))
            .map(|(name, _)| name)
        {
            for leaf in ["clean", "smudge", "process"] {
                let setting = format!("filter.{name}.{leaf}=");
                if !off.contains(&setting) {
                    off.push(setting);
                }
            }
            let required = format!("filter.{name}.required=false");
            if !off.contains(&required) {
                off.push(required);
            }
        }
    }
    Ok(off)
}

/// Tracked paths that are links in the index or the working tree, repository-relative.
fn tracked_links(git: &Git<'_>) -> Result<Vec<String>, Refused> {
    Ok(git
        .run(&["ls-files", "-s", "-z"], &[])
        .map_err(Refused)?
        .split('\0')
        .filter_map(|entry| {
            let (mode, path) = entry.split_once('\t')?;
            let link = mode.starts_with("120000 ")
                || git
                    .cwd
                    .join(path)
                    .symlink_metadata()
                    .is_ok_and(|m| m.file_type().is_symlink());
            link.then(|| path.to_string())
        })
        .collect())
}

/// Collects the change in `cwd`.
///
/// # Errors
///
/// Why nothing can be sent: not a repository, a refusal the repository asks for, a filter
/// driver, no change, or a change over [`CHANGE_MAX`].
pub fn collect(
    runner: &dyn Runner,
    var: &dyn Fn(&str) -> Option<String>,
    cwd: &Path,
    base: &Base,
    home: Option<&Path>,
) -> Result<Change, Refused> {
    let program = git_program(var("PATH").as_deref(), cwd)
        .ok_or_else(|| Refused("no git on PATH outside this repository".to_string()))?;
    let mut git = Git {
        runner,
        program,
        env: git_env(var),
        cwd: cwd.to_path_buf(),
        filters_off: Vec::new(),
    };
    let root = git
        .run(&["rev-parse", "--show-toplevel"], &[])
        .map_err(|_| Refused("not inside a git repository".to_string()))?;
    // Git writes the raw path and one line terminator. Other whitespace belongs to the
    // directory name, including any preceding newlines on Unix.
    let root = root
        .strip_suffix('\n')
        .ok_or_else(|| Refused("git did not terminate its repository path".to_string()))?;
    #[cfg(windows)]
    let root = root.strip_suffix('\r').unwrap_or(root);
    let root_path = PathBuf::from(root);
    git.cwd = root_path.clone();
    // Read as git reads a boolean (`no`, `off`, `0` are false too); a value git cannot read
    // as one, or a configuration that cannot be read, refuses: the opt-out fails closed.
    match git.run(&["config", "--bool", "--get", "aterm.aiReview"], &[1]) {
        Ok(v) if v.trim() == "false" => {
            return Err(Refused(
                "this repository says no (git config aterm.aiReview is false)".to_string(),
            ));
        }
        Ok(_) => {}
        Err(_) => {
            return Err(Refused(
                "this repository's git config aterm.aiReview could not be read as a boolean"
                    .to_string(),
            ));
        }
    }
    git.filters_off = filters(&git)?;
    let upstream = || {
        git.run(&["rev-parse", "--verify", "--quiet", "@{upstream}"], &[1])
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                git.run(&["rev-parse", "--verify", "--quiet", "origin/HEAD"], &[1])
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
    };
    let merge_base = |with: &str| {
        git.run(&["merge-base", "HEAD", with], &[])
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let has_head = git
        .run(&["rev-parse", "--verify", "--quiet", "HEAD"], &[1])
        .is_ok_and(|s| !s.trim().is_empty());
    let (base_rev, describes) = match base {
        Base::Uncommitted => (
            has_head.then(|| "HEAD".to_string()),
            "the uncommitted change and untracked files".to_string(),
        ),
        Base::Ref(r) => {
            if r.starts_with('-') {
                return Err(Refused(format!(
                    "`{}` is not a ref",
                    crate::harness::one_line(r, 60)
                )));
            }
            let mb = merge_base(r).ok_or_else(|| {
                Refused(format!(
                    "no merge base with `{}`",
                    crate::harness::one_line(r, 60)
                ))
            })?;
            (
                Some(mb),
                format!(
                    "the change since `{}`, untracked files included",
                    crate::harness::one_line(r, 60)
                ),
            )
        }
        Base::Branch => match upstream().and_then(|u| merge_base(&u)) {
            Some(mb) => (
                Some(mb),
                "the branch's change since its upstream, untracked files included".to_string(),
            ),
            None => (
                has_head.then(|| "HEAD".to_string()),
                "the uncommitted change and untracked files (no upstream)".to_string(),
            ),
        },
    };
    let rules = secret_rules(home);
    let mut held_back = Vec::new();
    let mut links = tracked_links(&git)?;
    let mut tracked = Vec::new();
    if let Some(b) = &base_rev {
        let names = git
            .run(
                &[
                    "diff",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--ignore-submodules=all",
                    "--name-only",
                    "-z",
                    b,
                    "--",
                ],
                &[],
            )
            .map_err(Refused)?;
        for name in names.split('\0').filter(|n| !n.is_empty()) {
            if looks_secret(&root_path, name, &rules) {
                held_back.push(name.to_string());
            } else {
                tracked.push(name.to_string());
            }
        }
    }
    let untracked_args: &[&str] = if base_rev.is_some() {
        &["ls-files", "--others", "--exclude-standard", "-z"]
    } else {
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ]
    };
    let untracked_list = git.run(untracked_args, &[]).map_err(Refused)?;
    let mut untracked = Vec::new();
    for name in untracked_list.split('\0').filter(|n| !n.is_empty()) {
        if untracked.iter().any(|u| u == name)
            || held_back.iter().any(|h| h == name)
            || links.iter().any(|l| l == name)
        {
            continue;
        }
        let link = root_path
            .join(name)
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink());
        if link {
            links.push(name.to_string());
        } else if looks_secret(&root_path, name, &rules) {
            held_back.push(name.to_string());
        } else {
            untracked.push(name.to_string());
        }
    }
    let mut diff = String::new();
    if let Some(b) = &base_rev
        && !tracked.is_empty()
    {
        let mut args: Vec<String> = [
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--ignore-submodules=all",
            "--no-color",
            "-M",
            b.as_str(),
            "--",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        args.extend(tracked.iter().map(|n| format!(":(literal){n}")));
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        diff = git.run(&refs, &[]).map_err(Refused)?;
    }
    let mut untracked_files = 0usize;
    for name in &untracked {
        if let Some(hunk) = new_file_hunk(&root_path, name) {
            diff.push_str(&hunk);
            untracked_files += 1;
        }
        if diff.len() > CHANGE_MAX {
            break;
        }
    }
    if diff.trim().is_empty() {
        return Err(Refused(format!("no change to review ({describes})")));
    }
    if diff.len() > CHANGE_MAX {
        return Err(Refused(format!(
            "the change is over {} KiB; narrow it with --base <ref> or --uncommitted",
            CHANGE_MAX / 1024
        )));
    }
    links.sort();
    links.dedup();
    Ok(Change {
        root: root_path,
        describes,
        diff,
        files: tracked.len() + untracked_files,
        held_back,
        links,
    })
}

#[cfg(test)]
#[path = "change_tests.rs"]
mod tests;
