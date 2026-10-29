// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host-side `aterm machines agent` entry points and layout.

use crate::port::System;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub mod apply;
pub mod chain;
pub mod dispatch;
pub mod hello;
pub mod inbox;
pub mod install;
pub mod job;
pub mod lock;
pub mod platform;
pub mod reclaim;
pub mod relay;
pub mod state;
pub mod watch;

/// Where everything the agent keeps lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The agent's root (root:wheel 0755).
    pub root: PathBuf,
}

impl Layout {
    /// The installed layout under [`crate::AGENT_ROOT`].
    #[must_use]
    pub fn installed() -> Self {
        Self {
            root: PathBuf::from(crate::AGENT_ROOT),
        }
    }

    /// `agent/`: the expanded agent (an aterm.app, or a dev binary).
    #[must_use]
    pub fn agent(&self) -> PathBuf {
        self.root.join("agent")
    }

    /// `agent.prev/`: the agent before the last upgrade.
    #[must_use]
    pub fn previous(&self) -> PathBuf {
        self.root.join("agent.prev")
    }

    /// `bin/`.
    #[must_use]
    pub fn bin(&self) -> PathBuf {
        self.root.join("bin")
    }

    /// `bin/aterm`: root-owned symlink to the agent's executable.
    #[must_use]
    pub fn exe_link(&self) -> PathBuf {
        self.bin().join("aterm")
    }

    /// `allowed_signers` (0644): who may sign envelopes for this host.
    #[must_use]
    pub fn allowed_signers(&self) -> PathBuf {
        self.root.join("allowed_signers")
    }

    /// `managed.toml` (0644): who enrolled this host and what the agent manages.
    #[must_use]
    pub fn managed(&self) -> PathBuf {
        self.root.join("managed.toml")
    }

    /// `.metadata_never_index`: keeps Spotlight and LaunchServices away from the second aterm.app.
    #[must_use]
    pub fn never_index(&self) -> PathBuf {
        self.root.join(".metadata_never_index")
    }

    /// `.staging/`: where enrollment expands the agent archive before `install`.
    #[must_use]
    pub fn staging(&self) -> PathBuf {
        self.root.join(".staging")
    }

    /// `state/` (0755): floors, the run lock, the journal and the runs.
    #[must_use]
    pub fn state(&self) -> PathBuf {
        self.root.join("state")
    }

    /// `state/floors.toml` (0644, so `hello` reads it as the admin).
    #[must_use]
    pub fn floors(&self) -> PathBuf {
        self.state().join("floors.toml")
    }

    /// `state/run.lock` (0644): `pid=<n> run=<id>` of the run in progress.
    #[must_use]
    pub fn lock(&self) -> PathBuf {
        self.state().join("run.lock")
    }

    /// `state/run.lock.guard` (0600): the `flock` every change of `run.lock` is made under.
    #[must_use]
    pub fn lock_guard(&self) -> PathBuf {
        self.state().join("run.lock.guard")
    }

    /// `state/aterm.app-first-run/` (0755): whether the aterm.app the `aterm.app` item
    /// installed showed it ran ([`crate::first_exec`]); nothing replaces that bundle while its
    /// hold lasts, and uninstall keeps it until then.
    #[must_use]
    pub fn app_first_run(&self) -> PathBuf {
        self.state().join("aterm.app-first-run")
    }

    /// `state/last_run` (0644): the newest finished run's id.
    #[must_use]
    pub fn last_run(&self) -> PathBuf {
        self.state().join("last_run")
    }

    /// `state/last_busy` (0644): newest signed request refused as `busy` ([`watch`]).
    #[must_use]
    pub fn last_busy(&self) -> PathBuf {
        self.state().join("last_busy")
    }

    /// `state/screen-watch/` (0700): `screen`'s armed watchers and what each did ([`watch`]).
    #[must_use]
    pub fn screen_watch(&self) -> PathBuf {
        self.state().join("screen-watch")
    }

    /// `state/journal.ndjson` (0600): one line per change, written before it is applied.
    #[must_use]
    pub fn journal(&self) -> PathBuf {
        self.state().join("journal.ndjson")
    }

    /// `state/runs/`.
    #[must_use]
    pub fn runs(&self) -> PathBuf {
        self.state().join("runs")
    }

    /// `state/runs/<id>/`.
    #[must_use]
    pub fn run_dir(&self, id: &str) -> PathBuf {
        self.runs().join(id)
    }

    /// `cache/` (0755): verified artifacts, one `<sha256>` file each.
    #[must_use]
    pub fn cache(&self) -> PathBuf {
        self.root.join("cache")
    }

    /// `cache/<sha256>`.
    #[must_use]
    pub fn cached(&self, sha256: &str) -> PathBuf {
        self.cache().join(sha256.to_ascii_lowercase())
    }

    /// `stage/` (root-only work space): envelope scratch files, expanded archives.
    #[must_use]
    pub fn stage(&self) -> PathBuf {
        self.root.join("stage")
    }

    /// `inbox/`: root-owned staging directory before hash verification into `cache/`.
    #[must_use]
    pub fn inbox(&self) -> PathBuf {
        self.root.join("inbox")
    }
}

/// The admin's upload inbox: `put` writes here as the admin, `apply` copies from here.
#[must_use]
pub fn user_inbox(home: &str) -> PathBuf {
    Path::new(home).join("Library/Caches/aterm-machines/inbox")
}

/// The item families this agent knows, as the baseline's tables name them.
pub const FAMILIES: &[&str] = &[
    "names",
    "sharing",
    "firewall",
    "time",
    "power",
    "updates",
    "keyboard",
    "consumer",
    "setup",
    "defaults",
    "developer",
    "homebrew",
    "app",
    "aterm",
    "github",
    "person",
];

/// Exit status returned when an agent command is refused (`refused: <why>`).
pub const REFUSED: u8 = 2;

/// `aterm machines agent <args>`.
#[must_use]
pub fn main(args: &[String], sys: &dyn System) -> ExitCode {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("refused: the agent cannot name its own executable: {e}");
            return ExitCode::from(REFUSED);
        }
    };
    let layout = Layout::installed();
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    run(sys, &layout, &exe, &words, &mut stdin, &mut stdout)
}

/// [`main`] with its process inputs explicit.
pub fn run(
    sys: &dyn System,
    layout: &Layout,
    exe: &Path,
    args: &[&str],
    stdin: &mut dyn Read,
    out: &mut dyn Write,
) -> ExitCode {
    match args {
        ["ssh"] => {
            let original = std::env::var("SSH_ORIGINAL_COMMAND").ok();
            dispatch::ssh(sys, layout, original.as_deref(), stdin, out)
        }
        ["hello"] => hello::print(sys, layout, out),
        ["put", sha] => inbox::put(sys, sha, stdin, out),
        ["apply"] => apply::apply(sys, layout, exe, stdin, out),
        ["run-job", id] => job::run_job(sys, layout, exe, id),
        ["attach", id] => apply::attach(sys, layout, id, out),
        ["put-file", path, mode] => inbox::put_file(sys, Path::new(path), mode, stdin, out),
        ["install", rest @ ..] => install::main(sys, layout, exe, rest, out),
        ["lock-screen"] => lock::lock_screen(sys, out, lock::LOCK_HANG),
        ["screen-watch", rest @ ..] => watch::main(sys, layout, exe, rest, out),
        ["tcc", clients @ ..] => crate::items::person::agent_tcc(sys, clients, out),
        ["version"] => {
            let _ = writeln!(out, "{}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => refuse(
            out,
            &format!(
                "`aterm machines agent {}` is not an agent verb (ssh, hello, put, apply, \
                 run-job, attach, put-file, install, lock-screen, screen-watch, tcc, version)",
                args.join(" ")
            ),
        ),
    }
}

/// Print `refused: <why>` and answer status 2.
pub fn refuse(out: &mut dyn Write, why: &str) -> ExitCode {
    let _ = writeln!(out, "refused: {why}");
    let _ = out.flush();
    ExitCode::from(REFUSED)
}

/// Whether `s` is a lowercase SHA-256 hex digest.
#[must_use]
pub fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether `s` is a run id this agent makes: `<unix seconds>-<serial>`.
#[must_use]
pub fn is_run_id(s: &str) -> bool {
    let Some((a, b)) = s.split_once('-') else {
        return false;
    };
    let digits = |p: &str| !p.is_empty() && p.len() <= 20 && p.bytes().all(|b| b.is_ascii_digit());
    digits(a) && digits(b)
}

/// The account this process runs as, resolved through the directory (never `HOME`).
/// # Errors
/// One line.
pub fn me(sys: &dyn System) -> Result<crate::model::User, String> {
    let ran = sys
        .run(&crate::port::Cmd::new("/usr/bin/id").arg("-un"))
        .map_err(|e| format!("id -un: {e}"))?;
    if !ran.success() {
        return Err(format!("id -un: {}", ran.describe()));
    }
    let name = ran.out().trim().to_owned();
    crate::facts::user(sys, &name)
}

/// `done`, with a path already gone ([`std::io::ErrorKind::NotFound`]) as nothing left to do.
fn gone<T: Default>(done: std::io::Result<T>) -> std::io::Result<T> {
    match done {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        done => done,
    }
}

/// Remove `path` and everything below it, never following a link (a link is removed, not
/// entered). What is already gone counts as removed.
/// # Errors
/// The first I/O error.
pub fn remove_tree(sys: &dyn System, path: &Path) -> std::io::Result<()> {
    let st = match sys.stat(path) {
        Ok(st) => st,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if st.dir && !st.symlink {
        for name in gone(sys.list_dir(path))? {
            remove_tree(sys, &path.join(name))?;
        }
    }
    gone(sys.remove(path))
}

/// [`remove_tree`] of `path`, leaving each of `keep` and the folders that still hold one; a
/// folder that held one is removed once empty. The [`reclaim`] job may be removing the kept
/// folders, `stage/`, `state/` and the root meanwhile, so what is already gone counts as
/// removed.
/// # Errors
/// Each path that could not be removed (not the folders left holding it), once everything else
/// that could be was.
pub fn remove_tree_keeping(sys: &dyn System, path: &Path, keep: &[PathBuf]) -> std::io::Result<()> {
    let mut failed = Vec::new();
    remove_all_but(sys, path, keep, &mut failed);
    let Some(kind) = failed.first().map(|(_, e)| e.kind()) else {
        return Ok(());
    };
    let each: Vec<String> = failed
        .iter()
        .map(|(path, e)| format!("{}: {e}", path.display()))
        .collect();
    Err(std::io::Error::new(kind, each.join("; ")))
}

/// [`remove_tree_keeping`], adding each path that could not be removed to `failed` and going on
/// with the rest. A folder that holds a failed path is left, unnamed: that path names it.
fn remove_all_but(
    sys: &dyn System,
    path: &Path,
    keep: &[PathBuf],
    failed: &mut Vec<(PathBuf, std::io::Error)>,
) {
    if keep.iter().any(|k| k == path) {
        return;
    }
    let holds = keep.iter().any(|k| k.starts_with(path));
    // A kept descendant never grants traversal through a link at an ancestor.
    let done = match gone(sys.stat(path).map(Some)) {
        Ok(Some(st)) if st.dir && !st.symlink => None,
        Ok(Some(_)) => Some(gone(sys.remove(path))),
        Ok(None) => Some(Ok(())),
        Err(e) => Some(Err(e)),
    };
    if let Some(done) = done {
        if let Err(e) = done {
            failed.push((path.to_owned(), e));
        }
        return;
    }
    let names = match gone(sys.list_dir(path)) {
        Ok(names) => names,
        Err(e) => {
            failed.push((path.to_owned(), e));
            return;
        }
    };
    let before = failed.len();
    for name in names {
        remove_all_but(sys, &path.join(name), keep, failed);
    }
    if failed.len() > before {
        return;
    }
    let done = if holds {
        gone(sys.list_dir(path)).and_then(|left| {
            if left.is_empty() {
                gone(sys.remove(path))
            } else {
                Ok(())
            }
        })
    } else {
        gone(sys.remove(path))
    };
    if let Err(e) = done {
        failed.push((path.to_owned(), e));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_ids_and_digests_are_checked_before_they_name_a_path() {
        assert!(is_run_id("1800000000-12"));
        assert!(!is_run_id("../../etc"));
        assert!(!is_run_id("1800000000-"));
        assert!(!is_run_id("1-2-3"));
        assert!(is_sha256(&"a1".repeat(32)));
        assert!(!is_sha256(&"A1".repeat(32)));
        assert!(!is_sha256("../x"));
    }

    #[test]
    fn a_folder_that_held_a_kept_one_goes_once_nothing_is_left_in_it() {
        let live = crate::port::Live::new();
        let tmp = aterm_tempfile::tempdir().unwrap();
        let root = tmp.path().join("aterm");
        let (held, gone) = (
            root.join("stage/upgrade-1-5"),
            root.join("stage/upgrade-1-6"),
        );
        std::fs::create_dir_all(&held).unwrap();
        std::fs::create_dir_all(root.join("agent")).unwrap();
        std::fs::write(root.join("agent/aterm"), b"agent").unwrap();
        remove_tree_keeping(&live, &root, &[held.clone(), gone.clone()]).unwrap();
        assert!(held.is_dir() && !root.join("agent").exists());
        std::fs::remove_dir(&held).unwrap();
        remove_tree_keeping(&live, &root, &[held, gone]).unwrap();
        assert!(
            !root.exists(),
            "the negative control: the job took what was kept before this ran"
        );
    }

    #[test]
    fn a_kept_descendant_does_not_admit_a_symlink_to_another_tree() {
        let live = crate::port::Live::new();
        let tmp = aterm_tempfile::tempdir().unwrap();
        let root = tmp.path().join("aterm");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(outside.join("held")).unwrap();
        std::fs::write(outside.join("held/aterm"), b"held program").unwrap();
        std::fs::write(outside.join("unrelated"), b"another program").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("stage")).unwrap();

        remove_tree_keeping(&live, &root, &[root.join("stage/held")]).unwrap();

        assert_eq!(
            std::fs::read(outside.join("held/aterm")).unwrap(),
            b"held program"
        );
        assert_eq!(
            std::fs::read(outside.join("unrelated")).unwrap(),
            b"another program"
        );
        assert!(!root.exists(), "only the link and its own empty parent go");
    }

    /// When the [`reclaim`] job does the rest of its work, on the path [`Job::at`] names.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum When {
        /// Once the `n`th listing of it returns.
        Listed(usize),
        /// Once it is removed.
        Removed,
        /// Inside its removal, between [`crate::port::Live`]'s `lstat` of it and its unlink.
        Removing,
    }

    /// [`crate::port::Live`], on which the [`reclaim`] job does the rest of its work once, at
    /// `at`: it empties and removes `held`, then `stage/` and the root if they are empty, as its
    /// `/bin/rmdir` does. A removal of one of `refused` fails; one at or under `held` panics.
    struct Job {
        live: crate::port::Live,
        root: PathBuf,
        held: PathBuf,
        at: Option<(When, PathBuf)>,
        listed: std::sync::atomic::AtomicUsize,
        ran: std::sync::atomic::AtomicBool,
        refused: Vec<PathBuf>,
    }

    impl Job {
        fn new(root: &Path, held: &Path, at: Option<(When, PathBuf)>) -> Self {
            Self {
                live: crate::port::Live::new(),
                root: root.to_owned(),
                held: held.to_owned(),
                at,
                listed: std::sync::atomic::AtomicUsize::new(0),
                ran: std::sync::atomic::AtomicBool::new(false),
                refused: Vec::new(),
            }
        }

        fn at(&self, path: &Path) -> Option<When> {
            self.at
                .as_ref()
                .filter(|(_, at)| at == path)
                .map(|(when, _)| *when)
        }

        fn reached(&self, when: When, path: &Path) {
            if self.at(path) == Some(when)
                && !self.ran.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                let _ = std::fs::remove_dir_all(&self.held);
                let _ = std::fs::remove_dir(self.root.join("stage"));
                let _ = std::fs::remove_dir(self.root.join("state"));
                let _ = std::fs::remove_dir(&self.root);
            }
        }
    }

    impl System for Job {
        fn run(&self, c: &crate::port::Cmd) -> Result<crate::port::Ran, crate::port::RunFailed> {
            self.live.run(c)
        }
        fn run_tty(&self, c: &crate::port::Cmd) -> Result<Option<i32>, crate::port::RunFailed> {
            self.live.run_tty(c)
        }
        fn read_file(&self, p: &Path) -> std::io::Result<Vec<u8>> {
            self.live.read_file(p)
        }
        fn write_file(&self, p: &Path, b: &[u8], m: u32) -> std::io::Result<()> {
            self.live.write_file(p, b, m)
        }
        fn write_user_file(
            &self,
            u: &crate::model::User,
            p: &Path,
            b: &[u8],
            m: u32,
        ) -> std::io::Result<()> {
            self.live.write_user_file(u, p, b, m)
        }
        fn remove(&self, p: &Path) -> std::io::Result<()> {
            assert!(!p.starts_with(&self.held), "{} is held", p.display());
            if self.refused.iter().any(|r| r == p) {
                return Err(std::io::ErrorKind::PermissionDenied.into());
            }
            if self.at(p) == Some(When::Removing) {
                let dir = std::fs::symlink_metadata(p)?.is_dir();
                self.reached(When::Removing, p);
                return if dir {
                    std::fs::remove_dir(p)
                } else {
                    std::fs::remove_file(p)
                };
            }
            let done = self.live.remove(p);
            self.reached(When::Removed, p);
            done
        }
        fn create_dir_all(&self, p: &Path, m: u32) -> std::io::Result<()> {
            self.live.create_dir_all(p, m)
        }
        fn chown(&self, p: &Path, u: u32, g: u32) -> std::io::Result<()> {
            self.live.chown(p, u, g)
        }
        fn stat(&self, p: &Path) -> std::io::Result<crate::port::Stat> {
            self.live.stat(p)
        }
        fn list_dir(&self, p: &Path) -> std::io::Result<Vec<String>> {
            let names = self.live.list_dir(p);
            if self.at(p).is_some() {
                let n = self
                    .listed
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.reached(When::Listed(n + 1), p);
            }
            names
        }
        fn sha256(&self, p: &Path) -> std::io::Result<String> {
            self.live.sha256(p)
        }
        fn copy_file(&self, f: &Path, t: &Path, m: u32) -> std::io::Result<()> {
            self.live.copy_file(f, t, m)
        }
        fn rename(&self, f: &Path, t: &Path) -> std::io::Result<()> {
            self.live.rename(f, t)
        }
        fn euid(&self) -> u32 {
            self.live.euid()
        }
        fn now(&self) -> u64 {
            self.live.now()
        }
    }

    /// An agent root under `tmp` with `.staging/`, `agent/`, `stage/upgrade-1-5/` and `state/`,
    /// each holding a file.
    fn agent_root(tmp: &Path) -> PathBuf {
        let root = tmp.join("aterm");
        for file in [
            ".staging/payload/aterm",
            "agent/aterm",
            "stage/upgrade-1-5/payload/aterm",
            "state/aterm.app-first-run/exec",
            "state/journal",
        ] {
            let file = root.join(file);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, b"agent").unwrap();
        }
        root
    }

    #[test]
    fn what_the_reclaim_job_removes_meanwhile_counts_as_removed() {
        // The folder kept, whether the job has removed it already, and where the rest runs.
        let cases = [
            ("stage/upgrade-1-5", false, When::Removed, "agent"),
            ("stage/upgrade-1-5", false, When::Listed(1), "stage"),
            ("stage/upgrade-1-5", true, When::Removing, "stage"),
            ("stage/upgrade-1-5", false, When::Removed, "state"),
            (".staging", false, When::Removing, "stage"),
            ("state/aterm.app-first-run", false, When::Listed(1), "state"),
            ("state/aterm.app-first-run", true, When::Removing, "state"),
        ];
        for (held, gone, when, at) in cases {
            let tmp = aterm_tempfile::tempdir().unwrap();
            let root = agent_root(tmp.path());
            let held = root.join(held);
            if gone {
                std::fs::remove_dir_all(&held).unwrap();
            }
            let job = Job::new(&root, &held, Some((when, root.join(at))));
            let case = format!("the job's rest at {when:?} {at}");
            remove_tree_keeping(&job, &root, std::slice::from_ref(&held)).expect(&case);
            assert!(job.ran.load(std::sync::atomic::Ordering::SeqCst), "{case}");
            assert!(!root.exists(), "{case}: the root and state/ go");
            remove_tree_keeping(&job, &root, &[held]).expect(&case);
        }
    }

    #[test]
    fn a_path_that_cannot_be_removed_leaves_the_rest_removed_and_is_named() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let root = agent_root(tmp.path());
        let held = root.join("stage/upgrade-1-5");
        // Siblings listed after the refused `state/journal`, in a folder that keeps nothing.
        let after = [root.join("state/lock"), root.join("state/runs/1-1")];
        for file in &after {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, b"agent").unwrap();
        }
        let mut job = Job::new(&root, &held, None);
        job.refused = vec![
            root.join(".staging/payload/aterm"),
            root.join("state/journal"),
        ];
        let why = remove_tree_keeping(&job, &root, std::slice::from_ref(&held)).unwrap_err();
        assert_eq!(why.kind(), std::io::ErrorKind::PermissionDenied);
        let why = why.to_string();
        for file in &after {
            assert!(!file.exists(), "{} stays: {why}", file.display());
        }
        assert!(!root.join("state/runs").exists(), "{why}");
        assert!(!root.join("agent").exists() && held.is_dir(), "{why}");
        let named: Vec<&str> = why.split("; ").collect();
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let each = [".staging/payload/aterm", "state/journal"]
            .map(|failed| format!("{}: {denied}", root.join(failed).display()));
        assert_eq!(named, each, "each refused path, and only it, is named");
    }

    #[test]
    fn an_unknown_verb_is_refused_with_status_two() {
        let fake = crate::port::Fake::root();
        let mut out = Vec::new();
        let code = run(
            &fake,
            &Layout::installed(),
            Path::new("/x"),
            &["shell"],
            &mut std::io::empty(),
            &mut out,
        );
        assert_eq!(code, ExitCode::from(REFUSED));
        assert!(String::from_utf8_lossy(&out).starts_with("refused: "));
    }
}
