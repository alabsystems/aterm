// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
        if self.at(path) == Some(when) && !self.ran.swap(true, std::sync::atomic::Ordering::SeqCst)
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
    fn run_tty(
        &self,
        c: &crate::port::Cmd,
        before: &mut dyn FnMut(&mut crate::port::Cmd) -> Result<(), crate::port::RunFailed>,
    ) -> Result<Option<i32>, crate::port::RunFailed> {
        self.live.run_tty(c, before)
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
