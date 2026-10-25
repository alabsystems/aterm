// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The retired gate's leftovers in a scratch home, laid out as the gate left them on this Mac:
//! a main checkout with a submodule, its snapshot worktree detached at a commit origin/main
//! contains with the submodule populated, the cells lane, and the machine lock. Git is the real
//! one. "Now" is a week and a day after the scratch was laid, so what the test writes is old
//! unless it says otherwise.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::{Git as _, GitCli, Kind, Roots, plan, retire, written_since};

/// A scratch home holding the gate's leftovers, removed when the test ends.
struct Gate {
    dir: aterm_tempfile::TempDir,
    roots: Roots,
    now: SystemTime,
}

impl Gate {
    fn home(&self) -> &Path {
        &self.roots.home
    }
    fn checkout(&self) -> PathBuf {
        self.home().join("aterm")
    }
    fn snapshot(&self) -> PathBuf {
        self.home().join("aterm-verify.noindex")
    }
    fn cells(&self) -> PathBuf {
        self.home().join("aterm-verify-cells.noindex")
    }
    fn lock(&self) -> PathBuf {
        self.roots.cache.join("aterm-verify")
    }
    /// What git lists as worktrees of the checkout.
    fn worktrees(&self) -> String {
        GitCli
            .run(&self.checkout(), &["worktree", "list", "--porcelain"])
            .out
    }
    /// Set `path`'s modification time to `ago` before this test's now.
    fn written(&self, path: &Path, ago: Duration) {
        std::fs::File::options()
            .read(true)
            .open(path)
            .and_then(|f| f.set_modified(self.now - ago))
            .expect("the time set");
    }
}

/// `git <args>` in `dir`, as a person set the scratch up: it must succeed.
#[track_caller]
fn git(dir: &Path, args: &[&str]) {
    let mut all = vec![
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "protocol.file.allow=always",
        "-c",
        "init.defaultBranch=main",
    ];
    all.extend_from_slice(args);
    let a = GitCli.run(dir, &all);
    assert_eq!(
        a.code,
        Some(0),
        "git {args:?} in {}: {}",
        dir.display(),
        a.err
    );
}

fn gate() -> Gate {
    let dir = aterm_tempfile::Builder::new()
        .prefix("retired-gate-")
        .tempdir()
        .expect("scratch");
    let home = std::fs::canonicalize(dir.path())
        .expect("resolved")
        .join("home");
    let sub = home.join("astream");
    let checkout = home.join("aterm");
    for repo in [&sub, &checkout] {
        std::fs::create_dir_all(repo).expect("a repo");
        git(repo, &["init", "-q"]);
    }
    std::fs::write(sub.join("lib.rs"), "// sub\n").expect("written");
    git(&sub, &["add", "lib.rs"]);
    git(&sub, &["commit", "-qm", "sub"]);
    std::fs::write(checkout.join("Cargo.toml"), "[workspace]\n").expect("written");
    std::fs::write(checkout.join(".gitignore"), "/target*\n/.aterm-verify\n").expect("written");
    git(&checkout, &["add", "Cargo.toml", ".gitignore"]);
    git(
        &checkout,
        &[
            "submodule",
            "add",
            "-q",
            &sub.to_string_lossy(),
            "vendor/astream",
        ],
    );
    git(&checkout, &["commit", "-qm", "main"]);
    git(
        &checkout,
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    let snapshot = home.join("aterm-verify.noindex");
    git(
        &checkout,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            &snapshot.to_string_lossy(),
            "HEAD",
        ],
    );
    git(&snapshot, &["submodule", "update", "-q", "--init"]);
    std::fs::create_dir_all(snapshot.join("target/debug")).expect("a build");
    std::fs::write(snapshot.join("target/debug/libx.rlib"), vec![1u8; 4096]).expect("written");
    let cells = home.join("aterm-verify-cells.noindex/win");
    std::fs::create_dir_all(&cells).expect("cells");
    std::fs::write(cells.join("x.rmeta"), "x").expect("written");
    let cache = home.join("cache");
    let lock = cache.join("aterm-verify");
    std::fs::create_dir_all(lock.join("queue")).expect("the lock's queue");
    std::fs::write(lock.join("lock"), "").expect("the lock");
    std::fs::write(lock.join("holder"), "pid 1\n").expect("the holder");
    Gate {
        dir,
        roots: Roots {
            home,
            cache,
            workspaces: Vec::new(),
        },
        now: SystemTime::now() + super::UNWRITTEN + Duration::from_secs(24 * 3600),
    }
}

/// Each leftover goes, with its reason, and a pass removes all three: git no longer lists the
/// snapshot, though its submodule is populated (a plain `git worktree remove` refuses that).
#[test]
fn the_gates_leftovers_go_once_unused() {
    let g = gate();
    let plain = GitCli.run(
        &g.checkout(),
        &["worktree", "remove", &g.snapshot().to_string_lossy()],
    );
    assert_ne!(
        plain.code,
        Some(0),
        "git alone keeps a worktree with a submodule"
    );
    let planned = plan(&g.roots, g.now, &GitCli);
    let kinds: Vec<(&Path, bool)> = planned
        .iter()
        .map(|l| (l.path.as_path(), l.goes()))
        .collect();
    assert_eq!(
        kinds,
        [
            (g.snapshot().as_path(), true),
            (g.cells().as_path(), true),
            (g.lock().as_path(), true)
        ],
        "{planned:#?}"
    );
    assert_eq!(
        planned[0].kind,
        Kind::Snapshot {
            checkout: g.checkout()
        }
    );
    let words = planned[0].words();
    assert!(
        words.contains("clean") && words.contains("origin/main"),
        "{words}"
    );
    assert!(g.worktrees().contains(&*g.snapshot().to_string_lossy()));
    let done = retire(&g.roots, g.now, &GitCli);
    assert!(done.iter().all(|(_, r)| r.is_ok()), "{done:#?}");
    for p in [g.snapshot(), g.cells(), g.lock()] {
        assert!(p.symlink_metadata().is_err(), "{} is gone", p.display());
    }
    assert!(
        !g.worktrees().contains(&*g.snapshot().to_string_lossy()),
        "{}",
        g.worktrees()
    );
    assert!(
        g.checkout().join("Cargo.toml").is_file(),
        "the checkout stays"
    );
    drop(g.dir);
}

/// One negative control: its name, what it does to a fresh scratch, and the words its verdict names.
type Case<'a> = (&'a str, &'a dyn Fn(&Gate), &'a str);

/// Negative controls, each in a fresh scratch: a file written today, an uncommitted change, an
/// untracked file, a commit origin/main does not contain, and a lock keep the snapshot, each
/// naming why, and the cells lane stays with it; a pass removes neither.
#[test]
fn a_used_snapshot_stays_and_says_why() {
    let cases: [Case<'_>; 5] = [
        (
            "touched today",
            &|g| {
                let f = g.snapshot().join("target/debug/libx.rlib");
                g.written(&f, Duration::from_secs(3600));
            },
            "libx.rlib was written 1h ago",
        ),
        (
            "an uncommitted change",
            &|g| std::fs::write(g.snapshot().join("Cargo.toml"), "[workspace]\n# x\n").expect("w"),
            "uncommitted change(s) (M Cargo.toml",
        ),
        (
            "an untracked file",
            &|g| std::fs::write(g.snapshot().join("notes.md"), "mine\n").expect("w"),
            "?? notes.md",
        ),
        (
            "a commit not in origin/main",
            &|g| {
                std::fs::write(g.snapshot().join("new.rs"), "\n").expect("w");
                git(&g.snapshot(), &["add", "new.rs"]);
                git(&g.snapshot(), &["commit", "-qm", "local"]);
            },
            "is not in origin/main",
        ),
        (
            "a locked worktree",
            &|g| {
                git(
                    &g.checkout(),
                    &["worktree", "lock", &g.snapshot().to_string_lossy()],
                )
            },
            "locked",
        ),
    ];
    for (case, make, why) in cases {
        let g = gate();
        make(&g);
        let planned = plan(&g.roots, g.now, &GitCli);
        let snapshot = &planned[0];
        assert_eq!(snapshot.path, g.snapshot(), "{case}");
        assert!(!snapshot.goes(), "{case}: {planned:#?}");
        assert!(
            snapshot.words().contains(why),
            "{case}: {}",
            snapshot.words()
        );
        assert!(!planned[1].goes(), "{case}: the cells lane stays with it");
        let done = retire(&g.roots, g.now, &GitCli);
        assert!(
            done[0].1.is_err() && done[1].1.is_err(),
            "{case}: {done:#?}"
        );
        assert!(g.snapshot().is_dir() && g.cells().is_dir(), "{case}");
        assert!(
            g.worktrees().contains(&*g.snapshot().to_string_lossy()),
            "{case}"
        );
    }
}

/// Git's own bookkeeping inside the snapshot (a submodule's git directory, which a read like
/// `git status` writes) is no write; a file beside it is. A held machine lock keeps its
/// directory.
#[test]
fn gits_bookkeeping_is_no_write_and_a_held_lock_stays() {
    let g = gate();
    let modules = g.snapshot().join(".aterm-verify/modules/vendor/astream");
    for d in ["objects", "refs"] {
        std::fs::create_dir_all(modules.join(d)).expect("a git dir");
    }
    std::fs::write(modules.join("HEAD"), "ref: refs/heads/main\n").expect("w");
    std::fs::write(modules.join("index.lock"), "").expect("w");
    g.written(&modules, Duration::ZERO);
    let since = g.now - super::UNWRITTEN;
    assert_eq!(written_since(&g.snapshot(), since), None);
    let beside = g.snapshot().join(".aterm-verify/receipt");
    std::fs::write(&beside, "x").expect("w");
    g.written(&beside, Duration::ZERO);
    assert_eq!(written_since(&g.snapshot(), since), Some(beside.clone()));
    std::fs::remove_file(&beside).expect("removed");
    g.written(
        &g.snapshot().join(".aterm-verify"),
        Duration::from_secs(9 * 86_400),
    );

    let held = std::fs::File::open(g.lock().join("lock")).expect("the lock");
    #[expect(
        clippy::disallowed_methods,
        reason = "the test holds the gate's lock as a live run would, and lets it go on drop"
    )]
    held.try_lock().expect("held");
    let planned = plan(&g.roots, g.now, &GitCli);
    assert!(planned[0].goes(), "{planned:#?}");
    let lock = planned
        .iter()
        .find(|l| l.kind == Kind::Lock)
        .expect("the lock");
    assert!(
        !lock.goes() && lock.words().contains("holds its lock"),
        "{lock:?}"
    );
    drop(held);
}

/// A lock taken after the plan judged the snapshot unlocked, just before its removal, keeps
/// it: one `--force` removes a clean worktree with a populated submodule but never a locked
/// one, and the cells lane stays with the snapshot. Negative control: the same removal with no
/// lock taken removes both ([`the_gates_leftovers_go_once_unused`]).
#[test]
fn a_lock_taken_just_before_the_removal_keeps_the_snapshot() {
    /// Git, but a person locks the snapshot as its removal starts.
    struct LockedFirst;
    impl super::Git for LockedFirst {
        fn run(&self, dir: &Path, args: &[&str]) -> super::Answer {
            if let ["worktree", "remove", .., path] = args {
                git(dir, &["worktree", "lock", path]);
            }
            GitCli.run(dir, args)
        }
    }
    let g = gate();
    let done = retire(&g.roots, g.now, &LockedFirst);
    let (snapshot, removed) = &done[0];
    assert_eq!(snapshot.path, g.snapshot());
    assert!(
        removed.as_ref().is_err_and(|e| e.contains("locked")),
        "{done:#?}"
    );
    assert!(
        g.snapshot().join("Cargo.toml").is_file(),
        "the snapshot stays"
    );
    assert!(done[1].1.is_err() && g.cells().is_dir(), "{done:#?}");
    assert!(g.worktrees().contains(&*g.snapshot().to_string_lossy()));
}
