// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The retired gate's leftovers in a scratch home, laid out as the gate left them on this Mac:
//! a main checkout with a submodule, its snapshot worktree detached at a commit origin/main
//! contains with the submodule populated, the cells lane, and the machine lock. Git is the real
//! one. "Now" is a week and a day after the scratch was laid, so what the test writes is old
//! unless it says otherwise.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::{Git as _, GitCli, Kind, LOCKED, Roots, plan, retire, written_since};

#[path = "build_cache_retired_lazy_fetch_tests.rs"]
mod lazy_fetch;

#[path = "build_cache_retired_registry_tests.rs"]
mod registry;

#[path = "build_cache_retired_registry_edge_tests.rs"]
mod registry_edges;

#[path = "build_cache_retired_history_tests.rs"]
mod history;

#[path = "build_cache_retired_history_edges_tests.rs"]
mod history_edges;

#[path = "build_cache_retired_reflog_tests.rs"]
mod reflog;

#[path = "build_cache_retired_config_tests.rs"]
mod config;

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
    /// Whether git lists `wt` as a worktree of the checkout.
    fn has_worktree(&self, wt: &Path) -> bool {
        super::listed(&GitCli, &self.checkout(), wt)
            .expect("git worktree list")
            .is_some()
    }
    /// Set `path`'s modification time to `ago` before this test's now.
    fn written(&self, path: &Path, ago: Duration) {
        let mut options = std::fs::OpenOptions::new();
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
            const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
            options
                .access_mode(FILE_WRITE_ATTRIBUTES)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
        }
        #[cfg(not(windows))]
        {
            options.read(true);
        }
        options
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

/// `path` as git reads a local path: a Windows verbatim path's `\\?\` prefix is one git reads
/// as a host name, so it goes, naming the same place.
fn for_git(path: &Path) -> String {
    super::git_path(path).to_string_lossy().into_owned()
}

/// Write and retain an unpushed commit directly in metadata whose worktree is gone.
fn retained_module_commit(g: &Gate, module: &Path) -> String {
    let created = GitCli.run(
        &g.checkout(),
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "--git-dir",
            &module.to_string_lossy(),
            "--work-tree",
            &g.checkout().to_string_lossy(),
            "commit-tree",
            "HEAD^{tree}",
            "-m",
            "unpushed empty commit",
        ],
    );
    assert_eq!(created.code, Some(0), "{}", created.err);
    let object = created.out.trim().to_owned();
    git(
        &g.checkout(),
        &[
            "--git-dir",
            &module.to_string_lossy(),
            "--work-tree",
            &g.checkout().to_string_lossy(),
            "update-ref",
            "HEAD",
            &object,
        ],
    );
    object
}

fn gate() -> Gate {
    let dir = aterm_tempfile::Builder::new()
        .prefix("retired-gate-")
        .tempdir()
        .expect("scratch");
    let resolved = std::fs::canonicalize(dir.path()).expect("resolved");
    let home = super::git_path(&resolved).join("home");
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
        &["submodule", "add", "-q", &for_git(&sub), "vendor/astream"],
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
            &for_git(&snapshot),
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

/// Each leftover goes, with its reason, and a pass removes all three: the snapshot's tree goes
/// whole and git no longer lists it, though its submodule is populated (a plain `git worktree
/// remove` refuses that).
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
    assert!(g.has_worktree(&g.snapshot()), "{}", g.worktrees());
    let done = retire(&g.roots, g.now, &GitCli);
    assert!(done.iter().all(|(_, r)| r.is_ok()), "{done:#?}");
    for p in [g.snapshot(), g.cells(), g.lock()] {
        assert!(p.symlink_metadata().is_err(), "{} is gone", p.display());
    }
    assert!(!g.has_worktree(&g.snapshot()), "{}", g.worktrees());
    assert!(
        g.checkout().join("Cargo.toml").is_file(),
        "the checkout stays"
    );
    drop(g.dir);
}

/// An actually running image keeps a retired snapshot, from its tree or from git's entry for it.
/// The child proves execution before any removal can touch its image; after it is reaped, the same
/// leftover can go. The cells lane and the lock share one arm of a pass, and the kernel reader
/// matches an image by path prefix whichever root holds it, so
/// [`used_or_unknown_ownership_keeps_retired_leftovers`] holds those two with a stated holder. The
/// gap: a kernel read that misses an image only under those two path shapes.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn running_images_keep_a_retired_snapshot_until_reaped() {
    use aterm_uds::deadline::{KeptUntilRan, ad_hoc_copy, cat_echoes, kill_within};
    use aterm_uds::spawnfd::{Child, Stdio};

    struct Running(Child);
    impl Drop for Running {
        fn drop(&mut self) {
            kill_within(&mut self.0, Duration::from_secs(60));
        }
    }

    let mut outcomes = Vec::new();
    for route in [0, 3] {
        let g = gate();
        let checkout = g.checkout();
        let paths = [
            g.snapshot(),
            g.cells(),
            g.lock(),
            super::own_entry(
                &checkout,
                &g.snapshot(),
                &std::fs::read(g.snapshot().join(".git")).unwrap(),
            )
            .unwrap(),
        ];
        let image = paths[route].join("target/bin/cat");
        let Gate { dir, roots, now } = g;
        let mut custody = KeptUntilRan::new(dir.keep());
        std::fs::create_dir_all(image.parent().unwrap()).unwrap();
        ad_hoc_copy(Path::new("/bin/cat"), &image);
        let child = aterm_uds::spawnfd::command(&image)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut running = Running(child);
        let input = cat_echoes(&mut running.0);
        custody.ran();
        let pid = running.0.id();
        // Exercise the production kernel reader, with only this proven fixture child
        // determining the verdict rather than unrelated processes on the owner's machine.
        let processes = |dirs: &[PathBuf], skip: &[u32]| {
            let mut scan = super::super::fence::live(dirs, skip)?;
            scan.users.retain(|user| user.pid == pid);
            scan.unreadable.retain(|(user, _)| *user == pid);
            Ok(scan)
        };
        let fence = super::super::fence::Fence::new(&processes);
        let done = super::retire_on(&roots, now, &GitCli, &fence);
        let held = done
            .iter()
            .find(|(left, _)| left.path == paths[0])
            .is_some_and(|(_, result)| result.is_err())
            && image.is_file();
        let snapshot_unchanged = paths[0].join(".git").is_file()
            && paths[1].is_dir()
            && GitCli
                .run(&checkout, &["worktree", "list", "--porcelain"])
                .out
                .contains(&*paths[0].to_string_lossy());
        kill_within(&mut running.0, Duration::from_secs(60));
        assert!(
            running.0.try_wait().unwrap().is_some(),
            "the child was reaped"
        );
        drop(input);
        let clear = super::retire_on(&roots, now, &GitCli, &fence);
        let reclaimed = clear.iter().all(|(_, result)| result.is_ok())
            && paths.iter().all(|path| !path.exists());
        outcomes.push((route, held, snapshot_unchanged, reclaimed));
    }
    assert!(
        outcomes
            .iter()
            .all(|(_, held, unchanged, reclaimed)| *held && *unchanged && *reclaimed),
        "live images retain their route, and released images permit reclaim: {outcomes:?}"
    );
}

/// The retired paths share the fail-closed ownership policy for cwd, files and unknown
/// processes. Clearing that same observation permits the existing eligible cleanup. A holder of
/// the cells lane and the lock alone keeps both while the snapshot goes, and a later pass, once
/// they are free, takes what the snapshot left.
#[test]
fn used_or_unknown_ownership_keeps_retired_leftovers() {
    use aterm_uds::holders::{PathScan, PathUse};

    let g = gate();
    let paths = [g.snapshot(), g.cells(), g.lock()];
    for how in ["exe", "cwd", "fd", "unreadable", "unavailable"] {
        let processes = |dirs: &[PathBuf], _: &[u32]| {
            if how == "unavailable" {
                return Err(std::io::Error::other("private unavailable probe"));
            }
            let scan = if how == "unreadable" {
                PathScan {
                    unreadable: vec![(23, Some(PathBuf::from("/private/unknown-tool")))],
                    ..PathScan::default()
                }
            } else {
                PathScan {
                    users: paths
                        .iter()
                        .filter(|path| dirs.iter().any(|dir| path.starts_with(dir)))
                        .map(|path| PathUse {
                            pid: 23,
                            how,
                            path: path.join("held"),
                        })
                        .collect(),
                    ..PathScan::default()
                }
            };
            Ok(scan)
        };
        let done = super::retire_on(
            &g.roots,
            g.now,
            &GitCli,
            &super::super::fence::Fence::new(&processes),
        );
        assert!(
            done.iter().all(|(_, result)| result.is_err()),
            "{how}: {done:?}"
        );
        assert!(paths.iter().all(|path| path.is_dir()), "{how}");
        assert!(g.snapshot().join(".git").is_file(), "{how}");
    }
    let clear = |_: &[PathBuf], _: &[u32]| Ok(PathScan::default());
    let done = super::retire_on(
        &g.roots,
        g.now,
        &GitCli,
        &super::super::fence::Fence::new(&clear),
    );
    assert!(done.iter().all(|(_, result)| result.is_ok()), "{done:?}");
    assert!(paths.iter().all(|path| !path.exists()));

    let g = gate();
    let leftovers = [g.cells(), g.lock()];
    let holding = std::cell::Cell::new(true);
    let processes = |dirs: &[PathBuf], _: &[u32]| {
        let users = leftovers
            .iter()
            .filter(|path| holding.get() && dirs.iter().any(|dir| path.starts_with(dir)))
            .map(|path| PathUse {
                pid: 23,
                how: "exe",
                path: path.join("held"),
            })
            .collect();
        Ok(PathScan {
            users,
            ..PathScan::default()
        })
    };
    let fence = super::super::fence::Fence::new(&processes);
    let done = super::retire_on(&g.roots, g.now, &GitCli, &fence);
    for path in &leftovers {
        assert!(
            done.iter()
                .find(|(left, _)| left.path == *path)
                .is_some_and(|(_, result)| result.is_err()),
            "{}: {done:?}",
            path.display()
        );
        assert!(path.is_dir(), "{}", path.display());
    }
    assert!(!g.snapshot().exists(), "{done:?}");
    holding.set(false);
    let done = super::retire_on(&g.roots, g.now, &GitCli, &fence);
    assert!(done.iter().all(|(_, result)| result.is_ok()), "{done:?}");
    assert!(leftovers.iter().all(|path| !path.exists()));
}

/// A git entry acquired after the snapshot tree went stays, and the failed snapshot
/// removal keeps its cells lane. Only that snapshot's own entry is examined.
#[test]
fn a_git_entry_acquired_after_the_tree_went_is_kept() {
    use aterm_uds::holders::{PathScan, PathUse};

    let g = gate();
    let entry = super::own_entry(
        &g.checkout(),
        &g.snapshot(),
        &std::fs::read(g.snapshot().join(".git")).unwrap(),
    )
    .unwrap();
    let late = std::cell::Cell::new(false);
    let held = std::cell::Cell::new(true);
    let processes = |dirs: &[PathBuf], _: &[u32]| {
        let users = if held.get() && dirs.contains(&entry) && !g.snapshot().exists() {
            late.set(true);
            vec![PathUse {
                pid: 24,
                how: "fd",
                path: entry.join("modules/vendor/astream/objects/held"),
            }]
        } else {
            Vec::new()
        };
        Ok(PathScan {
            users,
            ..PathScan::default()
        })
    };
    let done = super::retire_on(
        &g.roots,
        g.now,
        &GitCli,
        &super::super::fence::Fence::new(&processes),
    );
    assert!(late.get(), "the entry was checked after the tree went");
    assert!(done[0].1.is_err() && done[1].1.is_err(), "{done:?}");
    assert!(!g.snapshot().exists());
    assert!(entry.join("gitdir").is_file() && g.cells().is_dir());
    assert!(g.has_worktree(&g.snapshot()), "{}", g.worktrees());
    assert!(
        super::pending::read(&g.checkout(), &g.snapshot())
            .unwrap()
            .is_some_and(|receipt| receipt.tree_gone)
    );
    held.set(false);
    let finished = super::retire_on(
        &g.roots,
        g.now,
        &GitCli,
        &super::super::fence::Fence::new(&processes),
    );
    assert!(
        finished.iter().all(|(_, result)| result.is_ok()),
        "{finished:?}"
    );
    assert!(!entry.exists() && !g.cells().exists());
    assert!(
        super::pending::read(&g.checkout(), &g.snapshot())
            .unwrap()
            .is_none()
    );
}

/// Interrupted Prepared custody never authorizes a moved or replaced source, changed
/// metadata or new work. This is distinct from the proved TreeGone recovery above.
#[cfg(unix)]
#[test]
fn interrupted_snapshot_receipts_keep_moved_replaced_and_new_work() {
    for case in [
        "prepared-moved",
        "snapshot-replaced",
        "metadata-replaced",
        "snapshot-symlink",
        "metadata-symlink",
        "source-written",
        "git-binding-changed",
        "head-changed",
        "no-receipt-moved",
    ] {
        let g = gate();
        let text = std::fs::read(g.snapshot().join(".git")).unwrap();
        let entry = super::own_entry(&g.checkout(), &g.snapshot(), &text).unwrap();
        let head = super::listed(&GitCli, &g.checkout(), &g.snapshot())
            .unwrap()
            .unwrap()
            .head;
        if case != "no-receipt-moved" {
            super::pending::begin(&g.checkout(), &g.snapshot(), &entry, &head, &text).unwrap();
        }
        let away = g.home().join("moved-source");
        let moved_entry = g.home().join("moved-entry");
        match case {
            "prepared-moved" | "no-receipt-moved" => {
                std::fs::rename(g.snapshot(), &away).unwrap();
            }
            "snapshot-replaced" | "snapshot-symlink" => {
                std::fs::rename(g.snapshot(), &away).unwrap();
                if case == "snapshot-symlink" {
                    std::os::unix::fs::symlink(&away, g.snapshot()).unwrap();
                } else {
                    std::fs::create_dir(g.snapshot()).unwrap();
                    std::fs::write(g.snapshot().join("new-work"), b"keep").unwrap();
                }
            }
            "metadata-replaced" | "metadata-symlink" => {
                std::fs::rename(&entry, &moved_entry).unwrap();
                if case == "metadata-symlink" {
                    std::os::unix::fs::symlink(&moved_entry, &entry).unwrap();
                } else {
                    std::fs::create_dir(&entry).unwrap();
                    std::fs::write(entry.join("new-work"), b"keep").unwrap();
                }
            }
            "source-written" => {
                std::fs::write(g.snapshot().join("new-work"), b"keep").unwrap();
            }
            "git-binding-changed" => {
                std::fs::write(g.snapshot().join(".git"), b"gitdir: /unrelated\n").unwrap();
            }
            "head-changed" => {
                std::fs::write(g.snapshot().join("new-work"), b"keep").unwrap();
                git(&g.snapshot(), &["add", "new-work"]);
                git(&g.snapshot(), &["commit", "-qm", "new work"]);
            }
            _ => unreachable!(),
        }
        let done = retire(&g.roots, g.now + 30 * super::UNWRITTEN, &GitCli);
        assert!(entry.symlink_metadata().is_ok(), "{case}: {done:?}");
        if case != "no-receipt-moved" {
            assert!(done[0].1.is_err() && done[1].1.is_err(), "{case}: {done:?}");
        }
        if away.exists() {
            assert!(away.join("Cargo.toml").is_file(), "{case}");
        }
        if moved_entry.exists() {
            assert!(moved_entry.join("gitdir").is_file(), "{case}");
        }
        if matches!(
            case,
            "snapshot-replaced" | "source-written" | "head-changed"
        ) {
            assert_eq!(
                std::fs::read(g.snapshot().join("new-work")).unwrap(),
                b"keep",
                "{case}"
            );
        }
    }
}

/// A crash after the Prepared receipt was committed leaves the exact original tree
/// resumable; unrelated bytes at the receipt path never authorize deleting that tree.
#[test]
fn prepared_snapshot_custody_resumes_and_receipt_collisions_stay() {
    for collision in [false, true] {
        let g = gate();
        let receipt = super::beside(&g.checkout(), super::pending::SUFFIX).unwrap();
        if collision {
            std::fs::write(&receipt, b"owner data").unwrap();
        } else {
            let text = std::fs::read(g.snapshot().join(".git")).unwrap();
            let entry = super::own_entry(&g.checkout(), &g.snapshot(), &text).unwrap();
            let head = super::listed(&GitCli, &g.checkout(), &g.snapshot())
                .unwrap()
                .unwrap()
                .head;
            super::pending::begin(&g.checkout(), &g.snapshot(), &entry, &head, &text).unwrap();
        }
        let done = retire(&g.roots, g.now, &GitCli);
        if collision {
            assert!(
                done[0].1.is_err() && g.snapshot().join("Cargo.toml").is_file(),
                "{done:?}"
            );
            assert_eq!(std::fs::read(&receipt).unwrap(), b"owner data");
        } else {
            assert!(done.iter().all(|(_, result)| result.is_ok()), "{done:?}");
            assert!(!g.snapshot().exists() && !receipt.exists());
        }
    }
}

/// Correcting a clock backward cannot make new work look older than a receipt's
/// permanent cutoff. A future receipt keeps both source and metadata untouched.
#[test]
fn a_receipt_from_a_future_clock_keeps_new_work() {
    let g = gate();
    let text = std::fs::read(g.snapshot().join(".git")).unwrap();
    let entry = super::own_entry(&g.checkout(), &g.snapshot(), &text).unwrap();
    let head = super::listed(&GitCli, &g.checkout(), &g.snapshot())
        .unwrap()
        .unwrap()
        .head;
    let mut pending =
        super::pending::begin(&g.checkout(), &g.snapshot(), &entry, &head, &text).unwrap();
    let future = (g.now + super::UNWRITTEN)
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    pending.started = [future.as_secs(), u64::from(future.subsec_nanos())];
    let receipt = super::beside(&g.checkout(), super::pending::SUFFIX).unwrap();
    std::fs::write(&receipt, aterm_json::to_vec(&pending).unwrap()).unwrap();
    let work = g.snapshot().join("new-work");
    std::fs::write(&work, b"written under the corrected clock").unwrap();
    let done = retire(&g.roots, g.now, &GitCli);
    assert!(done[0].1.is_err() && done[1].1.is_err(), "{done:?}");
    assert_eq!(
        std::fs::read(&work).unwrap(),
        b"written under the corrected clock"
    );
    assert!(entry.join("gitdir").is_file() && receipt.is_file());
}

/// An empty commit changes no source-file timestamp or parent snapshot HEAD. New
/// submodule Git objects and refs must survive both Prepared and TreeGone custody.
#[test]
fn new_submodule_objects_keep_prepared_and_tree_gone_metadata() {
    use aterm_uds::holders::{PathScan, PathUse};

    for tree_gone in [false, true] {
        let g = gate();
        let text = std::fs::read(g.snapshot().join(".git")).unwrap();
        let entry = super::own_entry(&g.checkout(), &g.snapshot(), &text).unwrap();
        if tree_gone {
            let processes = |dirs: &[PathBuf], _: &[u32]| {
                Ok(PathScan {
                    users: (dirs.contains(&entry) && !g.snapshot().exists())
                        .then(|| PathUse {
                            pid: 25,
                            how: "fd",
                            path: entry.join("held"),
                        })
                        .into_iter()
                        .collect(),
                    ..PathScan::default()
                })
            };
            let done = super::retire_on(
                &g.roots,
                g.now,
                &GitCli,
                &super::super::fence::Fence::new(&processes),
            );
            assert!(done[0].1.is_err() && !g.snapshot().exists(), "{done:?}");
        } else {
            let head = super::listed(&GitCli, &g.checkout(), &g.snapshot())
                .unwrap()
                .unwrap()
                .head;
            super::pending::begin(&g.checkout(), &g.snapshot(), &entry, &head, &text).unwrap();
        }
        let module = entry.join("modules/vendor/astream");
        let object = if tree_gone {
            retained_module_commit(&g, &module)
        } else {
            git(
                &g.snapshot().join("vendor/astream"),
                &["commit", "--allow-empty", "-qm", "unpushed empty commit"],
            );
            GitCli
                .run(&g.snapshot().join("vendor/astream"), &["rev-parse", "HEAD"])
                .out
                .trim()
                .to_owned()
        };
        let before = GitCli.run(
            &g.checkout(),
            &[
                "--git-dir",
                &module.to_string_lossy(),
                "--work-tree",
                &g.checkout().to_string_lossy(),
                "cat-file",
                "-t",
                &object,
            ],
        );
        assert_eq!((before.code, before.out.as_str()), (Some(0), "commit\n"));
        let done = retire(&g.roots, g.now + 30 * super::UNWRITTEN, &GitCli);
        let after = GitCli.run(
            &g.checkout(),
            &[
                "--git-dir",
                &module.to_string_lossy(),
                "--work-tree",
                &g.checkout().to_string_lossy(),
                "cat-file",
                "-t",
                &object,
            ],
        );
        assert_eq!(
            (after.code, after.out.as_str()),
            (Some(0), "commit\n"),
            "unpushed object stays; TreeGone={tree_gone}, cleanup={done:?}, stderr={}",
            after.err
        );
        assert!(
            done[0].1.is_err() && done[1].1.is_err(),
            "TreeGone={tree_gone}: {done:?}"
        );
    }
}

/// A process scan can take long enough for work to arrive after eligibility. The
/// receipt's cutoff must precede that probe, with a strict check before destruction.
#[test]
fn work_arriving_in_process_probes_stays() {
    use aterm_uds::holders::PathScan;

    let mut outcomes = Vec::new();
    for (commit, late) in [(false, false), (true, false), (false, true), (true, true)] {
        let g = gate();
        let text = std::fs::read(g.snapshot().join(".git")).unwrap();
        let entry = super::own_entry(&g.checkout(), &g.snapshot(), &text).unwrap();
        let injected = std::cell::Cell::new(false);
        let object = std::cell::RefCell::new(None);
        let work = g.snapshot().join("new-work");
        let processes = |_: &[PathBuf], _: &[u32]| {
            if (!late || !g.snapshot().exists()) && !injected.replace(true) {
                if commit && late {
                    *object.borrow_mut() = Some(retained_module_commit(
                        &g,
                        &entry.join("modules/vendor/astream"),
                    ));
                } else if commit {
                    let sub = g.snapshot().join("vendor/astream");
                    git(
                        &sub,
                        &["commit", "--allow-empty", "-qm", "arrived during scan"],
                    );
                    *object.borrow_mut() = Some(
                        GitCli
                            .run(&sub, &["rev-parse", "HEAD"])
                            .out
                            .trim()
                            .to_owned(),
                    );
                } else {
                    if late {
                        std::fs::create_dir(g.snapshot()).unwrap();
                    }
                    std::fs::write(&work, b"arrived during scan").unwrap();
                }
            }
            Ok(PathScan::default())
        };
        let done = super::retire_on(
            &g.roots,
            g.now,
            &GitCli,
            &super::super::fence::Fence::new(&processes),
        );
        let survives = if let Some(object) = object.borrow().as_ref() {
            GitCli
                .run(
                    &g.checkout(),
                    &[
                        "--git-dir",
                        &entry.join("modules/vendor/astream").to_string_lossy(),
                        "--work-tree",
                        &g.checkout().to_string_lossy(),
                        "cat-file",
                        "-t",
                        object,
                    ],
                )
                .code
                == Some(0)
        } else {
            std::fs::read(&work).is_ok_and(|bytes| bytes == b"arrived during scan")
        };
        outcomes.push((
            (commit, late),
            injected.get(),
            survives,
            done[0].1.is_err() && done[1].1.is_err(),
        ));
    }
    assert!(
        outcomes
            .iter()
            .all(|(_, injected, survives, kept)| *injected && *survives && *kept),
        "{outcomes:?}"
    );
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
        assert!(g.has_worktree(&g.snapshot()), "{case}: {}", g.worktrees());
    }
}

/// Git's own bookkeeping inside the snapshot (a submodule's git directory, which a read like
/// `git status` writes) is no write; a file beside it is. A held machine lock keeps its
/// directory.
#[test]
fn gits_bookkeeping_is_no_write_and_a_held_lock_stays() {
    let g = gate();
    let modules = g.snapshot().join(".aterm-verify/modules/vendor/astream");
    let sub = g.snapshot().join("vendor/astream");
    let original = GitCli.run(&sub, &["rev-parse", "--absolute-git-dir"]);
    assert_eq!(original.code, Some(0), "{}", original.err);
    std::fs::create_dir_all(modules.parent().unwrap()).expect("legacy module namespace");
    std::fs::rename(original.out.trim(), &modules).expect("actual private module store relocated");
    std::fs::write(sub.join(".git"), format!("gitdir: {}\n", for_git(&modules)))
        .expect("module rebound");
    git(
        &g.checkout(),
        &[
            "config",
            "--file",
            &for_git(&modules.join("config")),
            "core.worktree",
            &for_git(&sub),
        ],
    );
    std::fs::write(modules.join("index.lock"), "").expect("w");
    g.written(&modules, Duration::ZERO);
    let since = g.now - super::UNWRITTEN;
    assert_eq!(written_since(&g.snapshot(), since, None), Ok(None));
    let beside = g.snapshot().join(".aterm-verify/receipt");
    std::fs::write(&beside, "x").expect("w");
    g.written(&beside, Duration::ZERO);
    assert_eq!(
        written_since(&g.snapshot(), since, None),
        Ok(Some(beside.clone()))
    );
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

/// A lock taken after the plan judged the snapshot unlocked, as its removal starts, keeps it:
/// git is asked again just before the tree is touched, and the cells lane stays with the
/// snapshot. Another checkout's cells lane, whose snapshot is gone already, still goes: a
/// snapshot kept keeps its own cells lane alone. Negative control: the same removal with no
/// lock taken removes both ([`the_gates_leftovers_go_once_unused`]).
#[test]
fn a_lock_taken_just_before_the_removal_keeps_the_snapshot() {
    /// Git, but a person locks the snapshot after the plan read it, as its removal starts (the
    /// second time git is asked for the checkout's worktrees).
    struct LockedAtRemoval {
        snapshot: PathBuf,
        asked: std::cell::Cell<usize>,
    }
    impl super::Git for LockedAtRemoval {
        fn run(&self, dir: &Path, args: &[&str]) -> super::Answer {
            if args == ["worktree", "list", "--porcelain"] {
                self.asked.set(self.asked.get() + 1);
                if self.asked.get() == 2 {
                    git(dir, &["worktree", "lock", &self.snapshot.to_string_lossy()]);
                }
            }
            GitCli.run(dir, args)
        }

        fn read_history(
            &self,
            dir: &Path,
            args: &[&str],
            input: &[u8],
            objects: Option<&Path>,
            deadline: std::time::Instant,
        ) -> super::Answer {
            GitCli.read_history(dir, args, input, objects, deadline)
        }
    }
    let g = gate();
    let other = g.home().join("other");
    std::fs::create_dir_all(&other).expect("a checkout");
    git(&other, &["init", "-q"]);
    let other_cells = g.home().join("other-verify-cells.noindex/win");
    std::fs::create_dir_all(&other_cells).expect("its cells lane");
    std::fs::write(other_cells.join("x.rmeta"), "x").expect("written");
    let locking = LockedAtRemoval {
        snapshot: g.snapshot(),
        asked: std::cell::Cell::new(0),
    };
    let done = retire(&g.roots, g.now, &locking);
    let (snapshot, removed) = &done[0];
    assert_eq!(snapshot.path, g.snapshot());
    assert_eq!(removed, &Err(LOCKED.to_owned()), "{done:#?}");
    assert!(
        g.snapshot().join("Cargo.toml").is_file(),
        "the snapshot stays"
    );
    assert!(done[1].1.is_err() && g.cells().is_dir(), "{done:#?}");
    assert!(g.has_worktree(&g.snapshot()), "{}", g.worktrees());
    let other_lane = g.home().join("other-verify-cells.noindex");
    let went = done.iter().find(|(l, _)| l.path == other_lane);
    assert_eq!(went.map(|(_, r)| r.clone()), Some(Ok(())), "{done:#?}");
    assert!(
        other_lane.symlink_metadata().is_err(),
        "its cells lane went"
    );
}

/// A LEFTOVER NO ONE ENTRY KEEPS GOES, AND ONE KEEPING AN ENTRY IS KEPT, as the build cache's
/// stages are (`build_cache::remove::tree`): the cells lane holding a folder without its owner's
/// write bit, which stops a plain removal at its first entry, goes whole; the machine lock keeping
/// one entry the remover leaves as it is (read as another user's, the remover's seam) is kept,
/// every other entry of it gone, its `Err` the count and the first of what stayed
/// (`remove::named`). NEGATIVE CONTROL: the same folder laid outside the leftovers refuses
/// `std::fs::remove_dir_all`, which leaves its file.
#[cfg(unix)]
#[test]
fn a_leftover_no_one_entry_keeps_goes_and_one_keeping_an_entry_is_kept() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    use crate::build_cache::remove;

    /// Gives each folder its owner's bits back and clears the remover's seams when dropped, so
    /// a failed test leaves its scratch removable.
    struct Unlock(Vec<PathBuf>);
    impl Drop for Unlock {
        fn drop(&mut self) {
            remove::seams::clear();
            for at in &self.0 {
                let _ = std::fs::set_permissions(at, std::fs::Permissions::from_mode(0o700));
            }
        }
    }
    let g = gate();
    let lay = |at: &Path| {
        std::fs::create_dir_all(at).expect("a folder");
        std::fs::write(at.join("x.rmeta"), "x").expect("written");
        std::fs::set_permissions(at, std::fs::Permissions::from_mode(0o500)).expect("its mode");
    };
    let held = g.cells().join("win/held");
    let control = g.dir.path().join("control/held");
    let _unlock = Unlock(vec![held.clone(), control.clone()]);
    lay(&held);
    lay(&control);
    assert!(std::fs::remove_dir_all(control.parent().expect("its folder")).is_err());
    assert!(
        control.join("x.rmeta").is_file(),
        "a plain removal leaves it"
    );
    let holder = std::fs::symlink_metadata(g.lock().join("holder")).expect("the holder");
    remove::seams::other_user(holder.ino());

    let done = retire(&g.roots, g.now, &GitCli);
    let of = |kind: Kind| {
        done.iter()
            .find(|(l, _)| l.kind == kind)
            .map(|(_, r)| r.clone())
    };
    assert_eq!(of(Kind::Cells), Some(Ok(())), "{done:#?}");
    assert!(g.cells().symlink_metadata().is_err(), "the cells lane went");
    assert_eq!(
        remove::stayed(&g.lock(), 10),
        [g.lock().join("holder")],
        "every other entry of the lock went"
    );
    let kept = remove::named(&g.lock(), 1);
    assert_eq!(
        kept.first,
        [(PathBuf::from("holder"), remove::Why::AnotherUser)]
    );
    assert_eq!(
        of(Kind::Lock),
        Some(Err(kept.words(&g.lock()))),
        "{done:#?}"
    );
}

/// Gives back what a test took from its scratch, so a failed test leaves it removable: the
/// `uchg` flag off each file, and each folder's extended ACL cleared, when dropped.
#[cfg(target_os = "macos")]
struct Release {
    flagged: Vec<PathBuf>,
    denied: Vec<PathBuf>,
}

#[cfg(target_os = "macos")]
impl Drop for Release {
    fn drop(&mut self) {
        for at in &self.flagged {
            let _ = crate::platform::set_file_flags(at, 0);
        }
        for at in &self.denied {
            let _ = aterm_uds::spawnfd::command("/bin/chmod")
                .arg("-N")
                .arg(at)
                .stderr(aterm_uds::spawnfd::Stdio::null())
                .status();
        }
    }
}

/// A folder of three files under `at`, its own ACL denying its deletion as a test scratch's run
/// leaves it (the incident's class: it stops `git worktree remove`, `rm -rf` and
/// `std::fs::remove_dir_all` at it).
#[cfg(target_os = "macos")]
fn denying_delete(at: &Path) {
    std::fs::create_dir_all(at).expect("a folder");
    for f in ["g1", "g2", "g3"] {
        std::fs::write(at.join(f), "x").expect("written");
    }
    crate::build_cache::tests::acl(at, &[crate::build_cache::tests::DENY_DELETE]);
}

/// THE SNAPSHOT GOES THROUGH THE REMOVER, AND WHAT STAYS IS KNOWN AGAIN: a clean snapshot whose
/// ignored build folder holds a folder denying its deletion and a file its owner flagged `uchg`
/// (which the remover never clears) loses every entry but that file, git's entry for it is
/// dropped, and the pass keeps it with the count and the first of what stayed
/// (`remove::named`), its cells lane with it. The `.git` the remover took is back as it was,
/// so the next plan knows the rest as a snapshot half removed and judges it by its files: a
/// plan an hour after the removal (no file in it written for a month) has it go, though the
/// removal rewrote its folders; once the flag is gone, the next pass finishes it and its cells
/// lane. NEGATIVE CONTROLS: the flag refuses `std::fs::remove_file`, and the folder refuses
/// `std::fs::remove_dir_all`.
#[cfg(target_os = "macos")]
#[test]
fn a_snapshot_one_entry_keeps_loses_the_rest_and_is_finished_once_it_goes() {
    use crate::build_cache::remove;

    let g = gate();
    let denied = g.snapshot().join("target/tmp/scratch/d");
    let stuck = g.snapshot().join("target/held/stuck");
    let _release = Release {
        flagged: vec![stuck.clone()],
        denied: vec![denied.clone()],
    };
    denying_delete(&denied);
    std::fs::create_dir_all(stuck.parent().expect("its folder")).expect("a folder");
    std::fs::write(&stuck, "held").expect("written");
    let month = Duration::from_secs(30 * 86_400);
    g.written(&stuck, month);
    for lane in [
        g.cells().join("win/x.rmeta"),
        g.cells().join("win"),
        g.cells(),
    ] {
        g.written(&lane, month);
    }
    crate::platform::set_file_flags(&stuck, libc::UF_IMMUTABLE).expect("uchg");
    assert!(std::fs::remove_file(&stuck).is_err(), "the flag refuses it");
    assert!(
        std::fs::remove_dir_all(&denied).is_err() && denied.is_dir(),
        "the ACL refuses a plain removal of the folder"
    );
    let gitfile = std::fs::read(g.snapshot().join(".git")).expect("its .git");
    let entry = g.checkout().join(".git/worktrees/aterm-verify.noindex");
    assert!(entry.is_dir(), "git's entry for it");
    let planned = plan(&g.roots, g.now, &GitCli);
    assert!(planned[0].goes() && planned[1].goes(), "{planned:#?}");

    let done = retire(&g.roots, g.now, &GitCli);
    let of = |done: &[(super::Leftover, Result<(), String>)], at: &Path| {
        done.iter()
            .find(|(l, _)| l.path == at)
            .map(|(_, r)| r.clone())
    };
    let kept = remove::Named {
        count: 1,
        first: vec![(
            PathBuf::from("target/held/stuck"),
            remove::Why::StillRefusing,
        )],
    };
    assert_eq!(
        of(&done, &g.snapshot()),
        Some(Err(kept.words(&g.snapshot()))),
        "{done:#?}"
    );
    assert_eq!(
        remove::stayed(&g.snapshot(), 10),
        [g.snapshot().join(".git"), stuck.clone()],
        "every other entry went, the denying folder too"
    );
    assert_eq!(
        std::fs::read(g.snapshot().join(".git")).expect("its .git"),
        gitfile,
        "its .git is back as it was"
    );
    assert!(entry.symlink_metadata().is_err(), "git's entry was dropped");
    assert!(
        !g.worktrees().contains(&*g.snapshot().to_string_lossy()),
        "{}",
        g.worktrees()
    );
    assert!(
        of(&done, &g.cells()).is_some_and(|r| r.is_err()) && g.cells().is_dir(),
        "the cells lane stays with it: {done:#?}"
    );
    assert_eq!(of(&done, &g.lock()), Some(Ok(())), "{done:#?}");

    assert_eq!(
        super::half_removed(&g.checkout(), &g.snapshot()),
        Some(entry)
    );
    let soon = SystemTime::now() + Duration::from_secs(3600);
    let again = plan(&g.roots, soon, &GitCli);
    assert!(again[0].goes() && again[1].goes(), "{again:#?}");
    crate::platform::set_file_flags(&stuck, 0).expect("the flag off");
    let finished = retire(&g.roots, soon, &GitCli);
    assert!(finished.iter().all(|(_, r)| r.is_ok()), "{finished:#?}");
    for p in [g.snapshot(), g.cells()] {
        assert!(p.symlink_metadata().is_err(), "{} is gone", p.display());
    }
    assert!(
        g.checkout().join("Cargo.toml").is_file(),
        "the checkout stays"
    );
}

/// A SNAPSHOT HALF REMOVED BY GIT GOES: git's `worktree remove --force` deletes the worktree's
/// entry even when it stops at an entry of the tree it cannot delete (DISK4-2's probe: a folder
/// denying its deletion), so git no longer lists what is left. Laid so here: the entry gone,
/// the tree's tracked files partly gone, its `.git` naming the entry. The plan judges it half
/// removed and it goes, its cells lane with it, the denying folder included; git's
/// `worktree remove` could not finish it. A tree whose `.git` names another repository's entry,
/// or that has none, is not taken for one and stays, git not listing it.
#[cfg(target_os = "macos")]
#[test]
fn a_snapshot_half_removed_by_git_is_judged_and_goes() {
    let g = gate();
    let denied = g.snapshot().join("target/tmp/scratch/d");
    let _release = Release {
        flagged: Vec::new(),
        denied: vec![denied.clone()],
    };
    denying_delete(&denied);
    let entry = g.checkout().join(".git/worktrees/aterm-verify.noindex");
    std::fs::remove_dir_all(&entry).expect("git's entry gone, as its remove leaves it");
    std::fs::remove_file(g.snapshot().join("Cargo.toml")).expect("a tracked file gone");
    assert!(!g.worktrees().contains(&*g.snapshot().to_string_lossy()));
    let gitfile = g.snapshot().join(".git");
    let named = std::fs::read(&gitfile).expect("its .git");

    let foreign = format!(
        "gitdir: {}\n",
        g.home().join("astream/.git/worktrees/x").display()
    );
    for (case, lay) in [
        ("another repository's entry", Some(foreign)),
        ("no .git", None),
    ] {
        let _ = std::fs::remove_file(&gitfile);
        if let Some(text) = lay {
            std::fs::write(&gitfile, text).expect("written");
        }
        assert_eq!(
            super::half_removed(&g.checkout(), &g.snapshot()),
            None,
            "{case}"
        );
        assert!(
            super::listed(&GitCli, &g.checkout(), &g.snapshot()).is_ok_and(|l| l.is_none()),
            "{case}"
        );
        let planned = plan(&g.roots, g.now, &GitCli);
        assert_eq!(
            planned[0].verdict,
            Err(super::unlisted(&g.checkout())),
            "{case}: {planned:#?}"
        );
        assert!(!planned[1].goes(), "{case}: the cells lane stays with it");
    }
    let _ = std::fs::remove_file(&gitfile);
    std::fs::write(&gitfile, &named).expect("its .git back");

    assert_eq!(
        super::half_removed(&g.checkout(), &g.snapshot()),
        Some(entry.clone())
    );
    let planned = plan(&g.roots, g.now, &GitCli);
    assert!(planned[0].goes() && planned[1].goes(), "{planned:#?}");
    assert!(
        planned[0].words().contains(&*entry.to_string_lossy()),
        "{}",
        planned[0].words()
    );
    let done = retire(&g.roots, g.now, &GitCli);
    assert!(done.iter().all(|(_, r)| r.is_ok()), "{done:#?}");
    for p in [g.snapshot(), g.cells(), g.lock()] {
        assert!(p.symlink_metadata().is_err(), "{} is gone", p.display());
    }
    assert!(
        g.checkout().join("Cargo.toml").is_file(),
        "the checkout stays"
    );
}

/// A SNAPSHOT GIT CANNOT READ EVEN THROUGH THE CHECKOUT'S MODULE STORE STAYS AND NAMES THE REMEDY:
/// its submodule holds an unpushed commit whose objects lived only in a store an
/// `objects/info/alternates` entry names, and that store is gone: `git status` fails even with the
/// main checkout's `.git/modules` store, so the snapshot stays with the submodule, the missing
/// store and the remedy named, and its cells lane with it. NEGATIVE CONTROL: while the store is
/// there, or when the snapshot's submodule is at a commit the main checkout's `.git/modules` store
/// holds, the snapshot is proved clean (or dirty if changed) without asking a person.
#[test]
fn a_snapshot_git_cannot_read_stays_and_names_the_remedy() {
    let g = gate();
    let sub = g.snapshot().join("vendor/astream");
    let sub_git = GitCli.run(&sub, &["rev-parse", "--absolute-git-dir"]);
    assert_eq!(sub_git.code, Some(0), "{}", sub_git.err);
    let sub_git = PathBuf::from(sub_git.out.trim());
    let keeper = g.home().join("aterm-r17-keeper");
    let store = keeper.join("modules/vendor/astream/objects");
    std::fs::create_dir_all(store.parent().expect("its folder")).expect("a folder");
    std::fs::rename(sub_git.join("objects"), &store).expect("its objects moved");
    std::fs::create_dir_all(sub_git.join("objects/info")).expect("a folder");
    std::fs::write(
        sub_git.join("objects/info/alternates"),
        format!("{}\n", store.display()),
    )
    .expect("written");
    let planned = plan(&g.roots, g.now, &GitCli);
    assert!(
        planned[0].goes(),
        "read through its alternates: {planned:#?}"
    );

    assert!(
        crate::build_cache::remove::tree(&keeper).whole(),
        "the store gone"
    );
    let status = GitCli.run(
        &g.snapshot(),
        &[
            "status",
            "--porcelain",
            "--ignore-submodules=none",
            "--untracked-files=normal",
        ],
    );
    assert_ne!(status.code, Some(0), "git cannot read it directly");
    let why = super::Unreadable::read(&status.err);
    assert_eq!(
        (why.submodule.as_deref(), why.missing_store.as_deref()),
        (Some("vendor/astream"), Some(store.as_path())),
        "{status:?}"
    );

    // While the main checkout's own `.git/modules/vendor/astream/objects` still holds the
    // submodule's commit and tree (as when the snapshot borrowed from a removed worktree of the
    // same checkout), `plan` proves a dirty submodule dirty and a clean one eligible read-only.
    let dirty = sub.join("untracked.rs");
    std::fs::write(&dirty, "// local work\n").expect("written");
    g.written(&dirty, Duration::from_secs(30 * 24 * 3600));
    g.written(&sub, Duration::from_secs(30 * 24 * 3600));
    let dirty_plan = plan(&g.roots, g.now, &GitCli);
    assert!(
        dirty_plan[0]
            .verdict
            .as_ref()
            .is_err_and(|e| e.contains("uncommitted change(s)")),
        "dirty submodule kept: {dirty_plan:#?}"
    );
    std::fs::remove_file(&dirty).expect("clean again");
    g.written(&sub, Duration::from_secs(30 * 24 * 3600));
    let rescued = plan(&g.roots, g.now, &GitCli);
    assert!(
        rescued[0].goes() && rescued[1].goes(),
        "clean snapshot rescued through checkout module store: {rescued:#?}"
    );

    // When the main checkout's module store also lacks the objects, nothing can prove the
    // submodule clean, so the snapshot and its cells lane stay with `Unreadable`.
    let checkout_mod_objs = g.checkout().join(".git/modules/vendor/astream/objects");
    assert!(
        crate::build_cache::remove::tree(&checkout_mod_objs).whole(),
        "checkout module objects removed"
    );
    let planned = plan(&g.roots, g.now, &GitCli);
    assert_eq!(planned[0].verdict, Err(why.words()), "{planned:#?}");
    assert!(!planned[1].goes(), "the cells lane stays with it");
    let done = retire(&g.roots, g.now, &GitCli);
    assert!(done[0].1.is_err() && done[1].1.is_err(), "{done:#?}");
    assert!(g.snapshot().join("Cargo.toml").is_file() && g.cells().is_dir());
}

#[test]
fn a_snapshot_whose_submodule_borrowed_from_a_removed_worktree_retires_cleanly() {
    let g = gate();
    let sub = g.snapshot().join("vendor/astream");
    let sub_git = GitCli.run(&sub, &["rev-parse", "--absolute-git-dir"]);
    assert_eq!(sub_git.code, Some(0), "{}", sub_git.err);
    let sub_git = PathBuf::from(sub_git.out.trim());
    let gone = g
        .checkout()
        .join(".git/worktrees/removed-wt/modules/vendor/astream/objects");
    std::fs::create_dir_all(&gone).expect("a folder");
    assert!(crate::build_cache::remove::tree(&sub_git.join("objects")).whole());
    std::fs::create_dir_all(sub_git.join("objects/info")).expect("a folder");
    std::fs::write(
        sub_git.join("objects/info/alternates"),
        format!("{}\n", gone.display()),
    )
    .expect("written");
    assert!(crate::build_cache::remove::tree(gone.parent().unwrap()).whole());

    let planned = plan(&g.roots, g.now, &GitCli);
    assert!(planned[0].goes() && planned[1].goes(), "{planned:#?}");
    let done = retire(&g.roots, g.now, &GitCli);
    assert!(done.iter().all(|(_, r)| r.is_ok()), "{done:#?}");
    for p in [g.snapshot(), g.cells()] {
        assert!(p.symlink_metadata().is_err(), "{} is gone", p.display());
    }
    assert!(g.checkout().join("Cargo.toml").is_file());
}

/// A SNAPSHOT WHOSE `.git` WILL NOT GO IS LEFT WHOLE: its `.git` goes first, and while that
/// stays (read as another user's, the remover's seam; a `uchg` flag does the same) nothing else
/// of it is touched: git still lists a clean worktree, not a gutted one, its cells lane stays
/// with it, and once the `.git` lets go the next pass removes both. NEGATIVE CONTROL: with the
/// tree removed first, every other entry goes and git is left listing a worktree whose tracked
/// files are gone, judged `uncommitted change(s)` by every later plan.
#[cfg(unix)]
#[test]
fn a_snapshot_whose_git_file_stays_is_left_whole() {
    use std::os::unix::fs::MetadataExt as _;

    use crate::build_cache::remove;

    /// Clears the remover's seams when dropped, so a failed test leaves none set.
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            remove::seams::clear();
        }
    }
    let g = gate();
    let _clear = Clear;
    let gitfile = g.snapshot().join(".git");
    let named = std::fs::read(&gitfile).expect("its .git");
    remove::seams::other_user(std::fs::symlink_metadata(&gitfile).expect("its .git").ino());

    let done = retire(&g.roots, g.now, &GitCli);
    assert!(done[0].1.is_err() && done[1].1.is_err(), "{done:#?}");
    for kept in [
        "Cargo.toml",
        ".gitignore",
        "vendor/astream/lib.rs",
        "target/debug/libx.rlib",
    ] {
        assert!(g.snapshot().join(kept).is_file(), "{kept} is untouched");
    }
    assert_eq!(std::fs::read(&gitfile).expect("its .git"), named);
    assert!(g.cells().is_dir(), "the cells lane stays with it");
    let status = GitCli.run(&g.snapshot(), &["status", "--porcelain"]);
    assert_eq!(
        (status.code, status.out.as_str()),
        (Some(0), ""),
        "a clean worktree still: {}",
        status.err
    );
    assert!(g.worktrees().contains(&*g.snapshot().to_string_lossy()));

    remove::seams::clear();
    assert!(plan(&g.roots, g.now, &GitCli)[0].goes());
    let finished = retire(&g.roots, g.now, &GitCli);
    assert!(finished.iter().all(|(_, r)| r.is_ok()), "{finished:#?}");
    for p in [g.snapshot(), g.cells()] {
        assert!(p.symlink_metadata().is_err(), "{} is gone", p.display());
    }
    assert!(!g.worktrees().contains(&*g.snapshot().to_string_lossy()));
}

/// NO OTHER WORKTREE'S ENTRY GOES WITH THE SNAPSHOT: another worktree of the checkout, moved
/// away (so git finds its entry stale) with a commit nobody pushed in its submodule, whose
/// repository git keeps under that entry, keeps its entry, the submodule's repository and the
/// commit through the pass that removes the snapshot, and moved back, git works in it.
/// NEGATIVE CONTROL: `git worktree prune`, which the removal once ran after the tree, would drop
/// that entry (its dry run names it), and the commit with it.
#[test]
fn no_other_worktrees_entry_goes_with_the_snapshot() {
    let g = gate();
    let wip = g.home().join("aterm-wip");
    git(
        &g.checkout(),
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            &wip.to_string_lossy(),
            "HEAD",
        ],
    );
    git(&wip, &["submodule", "update", "-q", "--init"]);
    let sub = wip.join("vendor/astream");
    std::fs::write(sub.join("mine.rs"), "\n").expect("written");
    git(&sub, &["add", "mine.rs"]);
    git(&sub, &["commit", "-qm", "unpushed"]);
    let commit = GitCli
        .run(&sub, &["rev-parse", "HEAD"])
        .out
        .trim()
        .to_owned();
    let entry = g.checkout().join(".git/worktrees/aterm-wip");
    assert!(entry.join("modules").is_dir(), "its submodule's repository");
    let away = g.home().join("aterm-wip.away");
    std::fs::rename(&wip, &away).expect("moved away");
    let dry = GitCli.run(
        &g.checkout(),
        &["worktree", "prune", "--dry-run", "--verbose"],
    );
    assert!(
        format!("{}{}", dry.out, dry.err).contains("worktrees/aterm-wip"),
        "git would prune it: {dry:?}"
    );

    let done = retire(&g.roots, g.now, &GitCli);
    assert!(done.iter().all(|(_, r)| r.is_ok()), "{done:#?}");
    assert!(
        g.snapshot().symlink_metadata().is_err(),
        "the snapshot went"
    );
    assert!(entry.join("modules").is_dir(), "the other entry stays");
    std::fs::rename(&away, &wip).expect("moved back");
    let status = GitCli.run(&wip, &["status", "--porcelain"]);
    assert_eq!(status.code, Some(0), "{}", status.err);
    let kept = GitCli.run(&sub, &["cat-file", "-e", &format!("{commit}^{{commit}}")]);
    assert_eq!(kept.code, Some(0), "the unpushed commit: {}", kept.err);
}

/// GIT MUST STOP LISTING THE SNAPSHOT FOR ITS REMOVAL TO COUNT: a lock taken after its tree went,
/// as git is asked to drop its entry, makes git refuse, and the pass says the snapshot was not
/// removed (its cells lane kept with it), git still listing it, locked: the person's lock keeps
/// git's entry.
#[test]
fn a_lock_taken_as_its_entry_goes_keeps_the_snapshot_listed() {
    /// Git, but a person locks the snapshot as git is asked to drop its entry.
    struct LockedAtDrop;
    impl super::Git for LockedAtDrop {
        fn run(&self, dir: &Path, args: &[&str]) -> super::Answer {
            if let ["worktree", "remove", "--force", wt] = args {
                git(dir, &["worktree", "lock", wt]);
            }
            GitCli.run(dir, args)
        }

        fn read_history(
            &self,
            dir: &Path,
            args: &[&str],
            input: &[u8],
            objects: Option<&Path>,
            deadline: std::time::Instant,
        ) -> super::Answer {
            GitCli.read_history(dir, args, input, objects, deadline)
        }
    }
    let g = gate();
    let done = retire(&g.roots, g.now, &LockedAtDrop);
    assert!(done[0].1.is_err() && done[1].1.is_err(), "{done:#?}");
    assert!(g.snapshot().symlink_metadata().is_err(), "its tree went");
    assert!(g.cells().is_dir(), "the cells lane stays with it");
    assert!(
        super::listed(&GitCli, &g.checkout(), &g.snapshot())
            .is_ok_and(|l| l.is_some_and(|entry| entry.locked)),
        "{}",
        g.worktrees()
    );
    git(
        &g.checkout(),
        &["worktree", "unlock", &g.snapshot().to_string_lossy()],
    );
    let finished = retire(&g.roots, g.now, &GitCli);
    assert!(
        finished.iter().all(|(_, result)| result.is_ok()),
        "{finished:?}"
    );
    assert!(!g.cells().exists());
    assert!(
        super::pending::read(&g.checkout(), &g.snapshot())
            .unwrap()
            .is_none()
    );
}

/// A git that records each directory it is asked about and answers nothing.
#[derive(Default)]
struct Asked(std::sync::Mutex<Vec<PathBuf>>);

impl super::Git for Asked {
    fn run(&self, dir: &Path, _: &[&str]) -> super::Answer {
        self.0.lock().expect("unpoisoned").push(dir.to_path_buf());
        super::Answer {
            code: Some(1),
            out: String::new(),
            err: "a recording git".to_owned(),
        }
    }
}

/// A scratch home with a main checkout at `<home>/<folder>/aterm` the build cache names as a
/// workspace, the retired gate's snapshot beside it: `(scratch, roots, checkout)`.
fn leftover_in(folder: &str) -> (aterm_tempfile::TempDir, Roots, PathBuf) {
    let dir = aterm_tempfile::Builder::new()
        .prefix("retired-fence-")
        .tempdir()
        .expect("scratch");
    let home = std::fs::canonicalize(dir.path())
        .expect("resolved")
        .join("home");
    let checkout = home.join(folder).join("aterm");
    std::fs::create_dir_all(checkout.join(".git")).expect("a main checkout");
    std::fs::create_dir_all(home.join(folder).join("aterm-verify.noindex")).expect("a snapshot");
    let roots = Roots {
        cache: home.join("cache"),
        workspaces: vec![checkout.clone()],
        home,
    };
    (dir, roots, checkout)
}

/// THE DAILY PASS NEVER LOOKS INTO A PROTECTED FOLDER: a workspace in a folder of `$HOME` that
/// is never looked into (Documents, Desktop, Downloads on macOS) is no checkout of the pass,
/// though the retired gate's snapshot stands beside it, so no `git` child and no walk reads
/// there and macOS asks the person nothing. NEGATIVE CONTROL: the same leftover in an ordinary
/// folder is planned, and git is asked about its checkout.
#[test]
fn a_workspace_in_a_folder_never_looked_into_is_no_checkout_of_the_pass() {
    let now = SystemTime::now();
    let skipped = crate::build_cache::checkout::HOME_SKIP[0].trim_end_matches('*');
    let (_scratch, roots, checkout) = leftover_in(skipped);
    assert!(
        super::main_checkout(&checkout),
        "PRECONDITION: only the fence leaves it out"
    );
    let git = Asked::default();
    let planned = plan(&roots, now, &git);
    assert!(
        planned
            .iter()
            .all(|l| !l.path.starts_with(roots.home.join(skipped))),
        "{planned:#?}"
    );
    assert_eq!(*git.0.lock().expect("unpoisoned"), Vec::<PathBuf>::new());

    let (_scratch, roots, checkout) = leftover_in("src");
    let git = Asked::default();
    let planned = plan(&roots, now, &git);
    assert!(
        planned.iter().any(|l| l.kind
            == Kind::Snapshot {
                checkout: checkout.clone()
            }),
        "control: {planned:#?}"
    );
    assert!(
        git.0.lock().expect("unpoisoned").contains(&checkout),
        "control: git is asked about an ordinary checkout"
    );
}

/// AN INTERRUPTED SNAPSHOT REMOVAL RECORDED IN AN EARLIER BOOT RESUMES BY ITS VOLUME'S UUID, AND
/// ANOTHER VOLUME'S STAYS REFUSED: both `Prepared` (`tree_gone == false`) and `TreeGone`
/// (`tree_gone == true`) receipts record the volume's UUID beside `[dev, ino]`; when a restart
/// renumbers the volume (the same UUIDs and inodes, another device number), the next pass finishes
/// the removal. NEGATIVE CONTROL: when `snapshot_volume` or `entry_volume` is another volume's
/// UUID (though `[dev, ino]` matches this boot's), the receipt refuses and the leftover stays.
#[cfg(target_os = "macos")]
#[test]
fn an_interrupted_snapshot_removal_from_an_earlier_boot_resumes_by_volume_uuid_and_another_volumes_stays()
 {
    for tree_gone in [false, true] {
        for elsewhere in [false, true] {
            let g = gate();
            let text = std::fs::read(g.snapshot().join(".git")).unwrap();
            let entry = super::own_entry(&g.checkout(), &g.snapshot(), &text).unwrap();
            let head = super::listed(&GitCli, &g.checkout(), &g.snapshot())
                .unwrap()
                .unwrap()
                .head;
            let mut pending =
                super::pending::begin(&g.checkout(), &g.snapshot(), &entry, &head, &text).unwrap();
            assert!(pending.snapshot_volume.is_some());
            assert!(pending.entry_volume.is_some());
            if tree_gone {
                assert!(super::super::remove::tree(&g.snapshot().join(".git")).whole());
                assert!(super::super::remove::tree(&g.snapshot()).whole());
                super::pending::tree_gone(&g.checkout(), &g.snapshot(), &mut pending).unwrap();
            }
            if elsewhere {
                if tree_gone {
                    pending.entry_volume.as_mut().unwrap()[0] ^= 0xff;
                } else {
                    pending.snapshot_volume.as_mut().unwrap()[0] ^= 0xff;
                }
            } else {
                pending.snapshot_id[0] = pending.snapshot_id[0].wrapping_add(1);
                pending.entry_id[0] = pending.entry_id[0].wrapping_add(1);
            }
            let receipt = super::beside(&g.checkout(), super::pending::SUFFIX).unwrap();
            std::fs::write(&receipt, aterm_json::to_vec(&pending).unwrap()).unwrap();
            let done = retire(&g.roots, g.now, &GitCli);
            if elsewhere {
                assert!(
                    done[0].1.is_err() && done[1].1.is_err(),
                    "tree_gone={tree_gone}: {done:?}"
                );
                assert!(entry.join("gitdir").is_file() && receipt.is_file());
                if !tree_gone {
                    assert!(g.snapshot().join("Cargo.toml").is_file());
                }
            } else {
                assert!(
                    done.iter().all(|(_, result)| result.is_ok()),
                    "tree_gone={tree_gone}: {done:?}"
                );
                assert!(!g.snapshot().exists() && !entry.exists() && !receipt.exists());
            }
        }
    }
}

/// A linked worktree of the gate's checkout where an agent makes one,
/// `<home>/Developer/.ubd/aterm-<name>.noindex`, on a new branch `agent/<name>` at the
/// checkout's HEAD (which origin/main holds), its submodule populated.
fn linked(g: &Gate, name: &str) -> PathBuf {
    linked_at(
        g,
        name,
        &agents_folder(g).join(format!("aterm-{name}.noindex")),
    )
}

/// Where agents put their worktrees in the scratch home: `<home>/Developer/.ubd`.
fn agents_folder(g: &Gate) -> PathBuf {
    let at = g.home().join("Developer").join(".ubd");
    std::fs::create_dir_all(&at).expect("the agents' folder");
    at
}

/// [`linked`] at `wt`, wherever that is.
fn linked_at(g: &Gate, name: &str, wt: &Path) -> PathBuf {
    let wt = wt.to_path_buf();
    git(
        &g.checkout(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &format!("agent/{name}"),
            &for_git(&wt),
            "HEAD",
        ],
    );
    git(&wt, &["submodule", "update", "-q", "--init"]);
    wt
}

/// Make `folder` a build folder as Cargo makes one: a `CACHEDIR.TAG` in it.
fn cache_tag(folder: &Path) {
    std::fs::create_dir_all(folder).expect("a build folder");
    std::fs::write(
        folder.join("CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n# a cache directory tag\n",
    )
    .expect("tagged");
}

/// AN IGNORED ENTRY THAT IS NO TAGGED CACHE KEEPS ITS WORKTREE: git holds no copy of an
/// ignored file, so a linked worktree with one (`.aterm-verify`, which the checkout ignores)
/// stays, and so does one whose ignored `target/` carries no cache tag; the same worktree with
/// only a tagged `target/` goes. NEGATIVE CONTROL: the tagged `target/` alone is admitted.
#[test]
fn a_linked_worktree_holding_an_ignored_file_stays() {
    let g = gate();
    let wt = linked(&g, "notes");
    std::fs::create_dir_all(wt.join("target/debug")).expect("a build");
    std::fs::write(wt.join("target/debug/libx.rlib"), "x").expect("written");
    let untagged = super::judge_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now);
    assert!(
        untagged
            .as_ref()
            .is_err_and(|why| why.contains("target/") && why.contains("no tagged cache")),
        "{untagged:?}"
    );
    cache_tag(&wt.join("target"));
    super::judge_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now)
        .expect("control: a tagged target/ alone is admitted");
    std::fs::write(wt.join(".aterm-verify"), "notes nobody else holds").expect("written");
    let judged = super::judge_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now);
    assert!(
        judged
            .as_ref()
            .is_err_and(|why| why.contains(".aterm-verify") && why.contains("no tagged cache")),
        "{judged:?}"
    );
    let removed = super::remove_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now);
    assert!(removed.is_err(), "{removed:?}");
    assert!(wt.join(".aterm-verify").is_file(), "the file stays");
    assert!(g.has_worktree(&wt), "{}", g.worktrees());
    assert!(linked_receipts(&g).is_empty(), "no receipt is left");
}

/// The receipts in the checkout's git directory (`.git/aterm-retiring/`).
fn linked_receipts(g: &Gate) -> Vec<PathBuf> {
    std::fs::read_dir(g.checkout().join(".git/aterm-retiring"))
        .map(|d| {
            d.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect()
        })
        .unwrap_or_default()
}

/// A LINKED WORKTREE ON A MERGED BRANCH GOES, ITS POPULATED SUBMODULE WITH IT: the judge admits
/// it on its branch, the remover takes its tree and git's entry for it, the checkout's own
/// submodule store stays, no receipt is left, and the gate's snapshot is judged as before.
/// NEGATIVE CONTROL: the snapshot's rule (detached only) refuses it, `not detached`.
#[test]
fn a_linked_worktree_on_a_merged_branch_goes_with_its_submodule() {
    let g = gate();
    let wt = linked(&g, "done");
    let snapshot_rule = super::judge_snapshot(&GitCli, &g.checkout(), &wt, g.now);
    assert!(
        snapshot_rule
            .as_ref()
            .is_err_and(|why| why.contains("not detached")),
        "control: {snapshot_rule:?}"
    );
    let admitted = super::judge_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now)
        .expect("a merged, clean linked worktree is admitted");
    assert_eq!(admitted.branch.as_deref(), Some("refs/heads/agent/done"));
    assert!(admitted.why.contains("linked worktree"), "{}", admitted.why);
    super::remove_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now).expect("removed");
    assert!(wt.symlink_metadata().is_err(), "its tree went");
    assert!(!g.has_worktree(&wt), "{}", g.worktrees());
    assert!(
        g.checkout().join(".git/modules/vendor/astream").is_dir(),
        "the checkout's submodule store stays"
    );
    assert_eq!(linked_receipts(&g), Vec::<PathBuf>::new());
    assert_eq!(super::retiring(&g.checkout()), Vec::<PathBuf>::new());
    let planned = plan(&g.roots, g.now, &GitCli);
    assert!(planned[0].goes(), "{planned:#?}");
}

/// A LINKED WORKTREE'S RECEIPT LIES IN ITS CHECKOUT'S GIT DIRECTORY: the snapshot's slot beside
/// the checkout stays free, so the snapshot is still judged, and [`super::retiring`] names the
/// worktree. NEGATIVE CONTROL: one slot per checkout would put it beside the checkout, and the
/// snapshot would read as `no longer names this snapshot`.
#[test]
fn a_linked_receipt_lies_in_the_checkouts_git_dir() {
    let g = gate();
    let wt = linked(&g, "done");
    let text = std::fs::read(wt.join(".git")).expect("its .git");
    let entry = super::own_entry(&g.checkout(), &wt, &text).expect("its entry");
    let head = super::listed(&GitCli, &g.checkout(), &wt)
        .expect("git worktree list")
        .expect("listed")
        .head;
    super::pending::begin(&g.checkout(), &wt, &entry, &head, &text).expect("a receipt");
    assert_eq!(
        linked_receipts(&g).len(),
        1,
        "one receipt in .git/aterm-retiring"
    );
    assert!(
        super::beside(&g.checkout(), super::pending::SUFFIX)
            .expect("the snapshot's slot")
            .symlink_metadata()
            .is_err(),
        "the snapshot's slot stays free"
    );
    let planned = plan(&g.roots, g.now, &GitCli);
    assert!(planned[0].goes(), "{planned:#?}");
    assert_eq!(super::retiring(&g.checkout()), std::slice::from_ref(&wt));
}

/// A COMMIT ONLY A LINKED WORKTREE'S REFLOG HOLDS KEEPS IT: a commit made in it and then reset
/// away is in no branch, and removing git's entry and the branch would drop the reflogs that are
/// its only root. NEGATIVE CONTROL: the same worktree without the reset is judged on its HEAD,
/// `is not in origin/main` ([`a_linked_worktree_on_a_merged_branch_goes_with_its_submodule`]
/// admits one with neither).
#[test]
fn a_linked_worktree_whose_reflog_holds_a_commit_main_lacks_stays() {
    let g = gate();
    let wt = linked(&g, "lost");
    std::fs::write(wt.join("new.rs"), "\n").expect("written");
    git(&wt, &["add", "new.rs"]);
    git(&wt, &["commit", "-qm", "local"]);
    git(&wt, &["reset", "-q", "--hard", "refs/remotes/origin/main"]);
    let judged = super::judge_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now);
    assert!(
        judged.as_ref().is_err_and(|why| why.contains("reflog")),
        "{judged:?}"
    );
    let removed = super::remove_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now);
    assert!(removed.is_err(), "{removed:?}");
    assert!(wt.join("Cargo.toml").is_file(), "its tree stands");
    assert!(g.has_worktree(&wt), "{}", g.worktrees());
}

/// A LINKED WORKTREE HOLDING ANOTHER WORKTREE STAYS: a worktree under an ignored folder of it
/// (`.claude/worktrees/y`) is invisible to `git status` in it and to the private-store walk,
/// which passes a `.git` file over, so the judge names it. NEGATIVE CONTROL: without the check
/// the removal would take `y`'s tree with its parent's.
#[test]
fn a_linked_worktree_holding_another_worktree_stays() {
    let g = gate();
    std::fs::create_dir_all(g.checkout().join(".git/info")).expect("git's info folder");
    std::fs::write(g.checkout().join(".git/info/exclude"), "/.claude/\n").expect("written");
    let wt = linked(&g, "done");
    let y = wt.join(".claude/worktrees/y");
    git(
        &g.checkout(),
        &["worktree", "add", "-q", "--detach", &for_git(&y), "HEAD"],
    );
    let judged = super::judge_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now);
    let inside = y.to_string_lossy().into_owned();
    assert!(
        judged
            .as_ref()
            .is_err_and(|why| why.contains("lies inside it") && why.contains(&inside)),
        "{judged:?}"
    );
    let removed = super::remove_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now);
    assert!(removed.is_err(), "{removed:?}");
    assert!(
        y.join("Cargo.toml").is_file() && g.has_worktree(&y),
        "y stands"
    );
    assert!(g.has_worktree(&wt), "{}", g.worktrees());
}

/// A LINKED WORKTREE PART OF WHICH STAYS KEEPS GIT'S ENTRY: an ignored file its owner flagged
/// `uchg` (which the remover never clears) stays, every other entry goes, git still lists the
/// worktree, its `.git` is back as it was and its receipt names it, so the next look finds it;
/// once the flag is gone the next removal finishes it from the receipt. NEGATIVE CONTROL: the
/// snapshot's arm removes git's entry by the remover, and git then lists it no more.
#[cfg(target_os = "macos")]
#[test]
fn a_linked_worktree_half_removed_keeps_its_entry() {
    let g = gate();
    let wt = linked(&g, "done");
    cache_tag(&wt.join("target"));
    let stuck = wt.join("target/held/stuck");
    let _release = Release {
        flagged: vec![stuck.clone()],
        denied: Vec::new(),
    };
    std::fs::create_dir_all(stuck.parent().expect("its folder")).expect("a folder");
    std::fs::write(&stuck, "held").expect("written");
    crate::platform::set_file_flags(&stuck, libc::UF_IMMUTABLE).expect("uchg");
    let gitfile = std::fs::read(wt.join(".git")).expect("its .git");

    let first = super::remove_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now);
    assert!(first.is_err(), "{first:?}");
    assert!(stuck.is_file(), "the flagged file stays");
    assert!(
        wt.join("Cargo.toml").symlink_metadata().is_err(),
        "the rest went"
    );
    assert!(g.has_worktree(&wt), "git still lists it: {}", g.worktrees());
    assert_eq!(
        std::fs::read(wt.join(".git")).expect("its .git"),
        gitfile,
        "its .git is back as it was"
    );
    assert_eq!(super::retiring(&g.checkout()), std::slice::from_ref(&wt));

    crate::platform::set_file_flags(&stuck, 0).expect("the flag off");
    super::remove_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now).expect("finished");
    assert!(wt.symlink_metadata().is_err(), "its tree went");
    assert!(!g.has_worktree(&wt), "{}", g.worktrees());
    assert_eq!(super::retiring(&g.checkout()), Vec::<PathBuf>::new());
}

/// A LINKED REMOVAL STOPPED AFTER ITS TREE RESUMES FROM ITS RECEIPT: a process holding git's
/// entry for it once its tree went keeps the entry; discovery, which finds a worktree by its
/// folder, can no longer find it, and [`super::retiring`] names it; the next removal finishes it
/// and its receipt. NEGATIVE CONTROL: without the receipt listing nothing names the worktree.
#[test]
fn a_linked_removal_stopped_after_its_tree_resumes_from_its_receipt() {
    use aterm_uds::holders::{PathScan, PathUse};

    let g = gate();
    let wt = linked(&g, "done");
    let entry = super::own_entry(
        &g.checkout(),
        &wt,
        &std::fs::read(wt.join(".git")).expect("its .git"),
    )
    .expect("its entry");
    let held = std::cell::Cell::new(true);
    let processes = |dirs: &[PathBuf], _: &[u32]| -> std::io::Result<PathScan> {
        let users = if held.get() && dirs.contains(&entry) && !wt.exists() {
            vec![PathUse {
                pid: 24,
                how: "fd",
                path: entry.join("held"),
            }]
        } else {
            Vec::new()
        };
        Ok(PathScan {
            users,
            ..PathScan::default()
        })
    };
    let first = super::remove_worktree_with_processes(
        &GitCli,
        g.home(),
        &g.checkout(),
        &wt,
        g.now,
        &processes,
    );
    assert!(first.is_err(), "{first:?}");
    assert!(!wt.exists(), "its tree went");
    assert!(g.has_worktree(&wt), "{}", g.worktrees());
    assert_eq!(super::retiring(&g.checkout()), std::slice::from_ref(&wt));
    held.set(false);
    super::remove_worktree_with_processes(&GitCli, g.home(), &g.checkout(), &wt, g.now, &processes)
        .expect("resumed");
    assert!(!g.has_worktree(&wt) && !entry.exists(), "{}", g.worktrees());
    assert_eq!(super::retiring(&g.checkout()), Vec::<PathBuf>::new());
}

/// A RECEIPT WHOSE REMOVAL STOPS BEFORE ITS FIRST ENTRY GOES IS WITHDRAWN: a process that takes
/// the worktree up between the receipt's commit and the first deletion keeps it whole, and no
/// receipt is left to admit it at a later look, after a person may have put in it a file whose
/// time a move kept. NEGATIVE CONTROL: without the withdrawal the receipt stays, and
/// [`super::retiring`] names the worktree.
#[test]
fn a_linked_removal_stopped_before_its_first_deletion_leaves_no_receipt() {
    use aterm_uds::holders::{PathScan, PathUse};

    let g = gate();
    let wt = linked(&g, "done");
    let probes = std::cell::Cell::new(0u32);
    let processes = |_: &[PathBuf], _: &[u32]| -> std::io::Result<PathScan> {
        probes.set(probes.get() + 1);
        let users = if probes.get() > 1 {
            vec![PathUse {
                pid: 24,
                how: "cwd",
                path: wt.clone(),
            }]
        } else {
            Vec::new()
        };
        Ok(PathScan {
            users,
            ..PathScan::default()
        })
    };
    let removed = super::remove_worktree_with_processes(
        &GitCli,
        g.home(),
        &g.checkout(),
        &wt,
        g.now,
        &processes,
    );
    assert_eq!(removed, Err(super::USED.to_owned()));
    assert_eq!(
        probes.get(),
        2,
        "the second probe follows the receipt's commit"
    );
    assert!(wt.join("Cargo.toml").is_file(), "its tree stands");
    assert!(g.has_worktree(&wt), "{}", g.worktrees());
    assert_eq!(linked_receipts(&g), Vec::<PathBuf>::new());
    assert_eq!(super::retiring(&g.checkout()), Vec::<PathBuf>::new());
}

/// A FILE PUT IN A LINKED WORKTREE WHOSE REMOVAL STOPPED KEEPS IT, WHATEVER ITS TIME: a receipt
/// stands and its removal took a tracked file; a person then moves in a month-old note, the move
/// keeping its time, which is older than the receipt's start. The receipt admits only what git
/// sees the removal do. NEGATIVE CONTROLS: the same tree before the note is admitted from its
/// receipt; without the check the note, older than the receipt, goes with the tree.
#[test]
fn a_file_put_in_a_stopped_linked_removal_keeps_it() {
    let g = gate();
    let wt = linked(&g, "done");
    let text = std::fs::read(wt.join(".git")).expect("its .git");
    let entry = super::own_entry(&g.checkout(), &wt, &text).expect("its entry");
    let head = super::listed(&GitCli, &g.checkout(), &wt)
        .expect("git worktree list")
        .expect("listed")
        .head;
    super::pending::begin(&g.checkout(), &wt, &entry, &head, &text).expect("a receipt");
    std::fs::remove_file(wt.join("Cargo.toml")).expect("a tracked file gone");
    let control = super::judge_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now);
    assert!(control.is_ok(), "control: {control:?}");

    let note = wt.join("notes.md");
    std::fs::write(&note, "mine\n").expect("written");
    let month = SystemTime::now() - Duration::from_secs(30 * 86_400);
    std::fs::File::options()
        .write(true)
        .open(&note)
        .and_then(|f| f.set_modified(month))
        .expect("its old time");
    let judged = super::judge_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now);
    assert!(
        judged
            .as_ref()
            .is_err_and(|why| why.contains("?? notes.md")),
        "{judged:?}"
    );
    let removed = super::remove_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now);
    assert!(removed.is_err(), "{removed:?}");
    assert_eq!(std::fs::read_to_string(&note).expect("the note"), "mine\n");
    assert!(g.has_worktree(&wt), "{}", g.worktrees());
}

/// A LINKED WORKTREE WITH A FILE GIT IS TOLD NOT TO LOOK AT STAYS: an edit to a tracked file
/// marked skip-worktree or assume-unchanged does not show in `git status`, and would go with the
/// tree once nothing in it was written for a week. NEGATIVE CONTROL: without the check each is
/// admitted, its edit unseen.
#[test]
fn a_linked_worktree_with_a_file_git_is_told_not_to_look_at_stays() {
    let g = gate();
    for (name, flag) in [
        ("skip", "--skip-worktree"),
        ("assume", "--assume-unchanged"),
    ] {
        let wt = linked(&g, name);
        std::fs::write(wt.join("Cargo.toml"), "[workspace]\n# mine\n").expect("written");
        git(&wt, &["update-index", flag, "Cargo.toml"]);
        let judged = super::judge_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now);
        assert!(
            judged
                .as_ref()
                .is_err_and(|why| why.contains("told not to look at its file Cargo.toml")),
            "{name}: {judged:?}"
        );
        let removed = super::remove_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now);
        assert!(removed.is_err(), "{name}: {removed:?}");
        assert_eq!(
            std::fs::read_to_string(wt.join("Cargo.toml")).expect("its edit"),
            "[workspace]\n# mine\n",
            "{name}"
        );
    }
}

/// ONLY A WORKTREE AN AGENT MAKES IS A CANDIDATE: a merged, clean worktree beside the checkout
/// (`~/aterm-develop`, a person's own) stays, judged and removed alike, with its files and git's
/// entry; one in the checkout's `.claude/worktrees/` (Claude Code's isolation worktree) and one
/// named `*.noindex` in a `.ubd` folder are admitted; and a worktree of a bare repository (the
/// `git clone --bare … proj/.git` layout) stays, though it has the agent's shape. NEGATIVE
/// CONTROLS: without the shape rule the person's worktree is admitted and goes; without the bare
/// rule the bare repository's is admitted.
#[test]
fn only_a_worktree_an_agent_makes_is_a_candidate() {
    let g = gate();
    let person = linked_at(&g, "develop", &g.home().join("aterm-develop"));
    let judged = super::judge_worktree(&GitCli, g.home(), &g.checkout(), &person, g.now);
    assert!(
        judged
            .as_ref()
            .is_err_and(|why| why.contains("no worktree an agent makes")),
        "{judged:?}"
    );
    let removed = super::remove_worktree(&GitCli, g.home(), &g.checkout(), &person, g.now);
    assert!(removed.is_err(), "{removed:?}");
    assert!(person.join("Cargo.toml").is_file(), "its tree stands");
    assert!(g.has_worktree(&person), "{}", g.worktrees());

    let isolation = linked_at(&g, "iso", &g.checkout().join(".claude/worktrees/iso"));
    super::judge_worktree(&GitCli, g.home(), &g.checkout(), &isolation, g.now)
        .expect("control: Claude Code's isolation worktree is admitted");
    let shaped = linked(&g, "done");
    super::judge_worktree(&GitCli, g.home(), &g.checkout(), &shaped, g.now)
        .expect("control: an agent's .ubd/*.noindex worktree is admitted");

    let bare = g.home().join("proj");
    git(
        g.home(),
        &[
            "clone",
            "-q",
            "--bare",
            &for_git(&g.checkout()),
            &for_git(&bare.join(".git")),
        ],
    );
    git(&bare, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let in_bare = agents_folder(&g).join("proj-main.noindex");
    git(
        &bare,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/bare",
            &for_git(&in_bare),
            "HEAD",
        ],
    );
    let judged = super::judge_worktree(&GitCli, g.home(), &bare, &in_bare, g.now);
    assert!(
        judged
            .as_ref()
            .is_err_and(|why| why.contains("bare repository")),
        "{judged:?}"
    );
    let removed = super::remove_worktree(&GitCli, g.home(), &bare, &in_bare, g.now);
    assert!(removed.is_err(), "{removed:?}");
    assert!(in_bare.join("Cargo.toml").is_file(), "its tree stands");
}

/// AN AGENT'S WORKTREE IS READ OFF ITS PATH: a `*.noindex` name needs a `.ubd` folder above it,
/// at any depth, and a `.ubd` folder needs the name; an isolation worktree is a folder of its
/// own checkout's `.claude/worktrees/`, not of another's and not deeper. NEGATIVE CONTROL: a
/// rule reading the name alone admits `/nowhere/x/a.noindex`.
#[test]
fn an_agents_worktree_is_read_off_its_path() {
    let c = Path::new("/nowhere/c");
    for (wt, made) in [
        ("/nowhere/x/.ubd/a.noindex", true),
        ("/nowhere/.ubd/deep/a.noindex", true),
        ("/nowhere/x/a.noindex", false),
        ("/nowhere/.ubd/a", false),
        ("/nowhere/.ubd/.noindex", false),
        ("/nowhere/c/.claude/worktrees/x", true),
        ("/nowhere/d/.claude/worktrees/x", false),
        ("/nowhere/c/.claude/worktrees/x/y", false),
    ] {
        assert_eq!(super::agent_made(c, Path::new(wt)), made, "{wt}");
    }
}

/// Make git in `dir` (a worktree or a submodule checked out in one) ignore `pattern`, through its
/// own git directory's `info/exclude`, so no file in the tree says so.
fn exclude_in(dir: &Path, pattern: &str) {
    let at = GitCli.run(
        dir,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "info/exclude",
        ],
    );
    assert_eq!(at.code, Some(0), "{}", at.err);
    let exclude = PathBuf::from(at.out.trim());
    std::fs::create_dir_all(exclude.parent().expect("its folder")).expect("git's info folder");
    let mut rules = std::fs::read_to_string(&exclude).unwrap_or_default();
    rules.push_str(pattern);
    rules.push('\n');
    std::fs::write(&exclude, rules).expect("written");
}

/// A FILE A SUBMODULE IGNORES KEEPS ITS WORKTREE: the superproject's `git status` reports no
/// file git ignores inside a submodule, under any `--ignore-submodules`, so a submodule's own
/// `.claude/` notes (a session that worked in it) would go with the tree. Each submodule checked
/// out in a worktree is read on its own: an untagged ignored `target/` in it keeps the worktree,
/// a tagged one is admitted and recorded by its path in the worktree, and an ignored notes file
/// beside it keeps it, judged and removed alike. NEGATIVE CONTROL: reading the worktree's own
/// level alone admits the notes, and the removal takes them.
#[test]
fn a_file_a_submodule_ignores_keeps_its_worktree() {
    let g = gate();
    let wt = linked(&g, "subign");
    let sub = wt.join("vendor/astream");
    exclude_in(&sub, "/target/");
    exclude_in(&sub, "/.claude/");
    std::fs::create_dir_all(sub.join("target/debug")).expect("a build");
    std::fs::write(sub.join("target/debug/libx.rlib"), "x").expect("written");
    let untagged = super::judge_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now);
    assert!(
        untagged.as_ref().is_err_and(
            |why| why.contains("vendor/astream/target/") && why.contains("no tagged cache")
        ),
        "{untagged:?}"
    );
    cache_tag(&sub.join("target"));
    super::judge_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now)
        .expect("control: a tagged target/ in the submodule is admitted");
    assert_eq!(
        super::ignored_entries(&GitCli, &wt),
        Ok(vec!["vendor/astream/target/".to_owned()]),
        "recorded by its path in the worktree"
    );

    let notes = sub.join(".claude/notes.md");
    std::fs::create_dir_all(notes.parent().expect("its folder")).expect("a folder");
    std::fs::write(&notes, "a plan nobody else holds\n").expect("written");
    let judged = super::judge_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now);
    assert!(
        judged.as_ref().is_err_and(
            |why| why.contains("vendor/astream/.claude/") && why.contains("no tagged cache")
        ),
        "{judged:?}"
    );
    let removed = super::remove_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now);
    assert!(removed.is_err(), "{removed:?}");
    assert!(notes.is_file(), "the notes stay");
    assert!(g.has_worktree(&wt), "{}", g.worktrees());
    assert!(linked_receipts(&g).is_empty(), "no receipt is left");
}

/// AN EDIT A SUBMODULE'S OWN INDEX HIDES KEEPS ITS WORKTREE: a tracked file of the submodule
/// marked assume-unchanged and then edited shows in no status of the superproject or the
/// submodule, so the judge reads the submodule's own `ls-files -v`. NEGATIVE CONTROL: the same
/// worktree before the mark is admitted.
#[test]
fn an_edit_a_submodules_index_hides_keeps_its_worktree() {
    let g = gate();
    let wt = linked(&g, "subhide");
    let sub = wt.join("vendor/astream");
    super::judge_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now)
        .expect("control: admitted before the mark");
    let listed = GitCli.run(&sub, &["ls-files", "-z"]);
    let rel = listed
        .out
        .split('\0')
        .next()
        .expect("a tracked file")
        .to_owned();
    assert!(!rel.is_empty(), "the submodule tracks a file: {listed:?}");
    git(&sub, &["update-index", "--assume-unchanged", &rel]);
    std::fs::write(sub.join(&rel), "an edit nobody else holds\n").expect("written");
    let judged = super::judge_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now);
    assert!(
        judged
            .as_ref()
            .is_err_and(|why| why.contains("in vendor/astream/") && why.contains(&rel)),
        "{judged:?}"
    );
}

/// A linked worktree of the gate's checkout where an agent makes one, on a new branch
/// `agent/<name>`, its submodule never checked out: git leaves its folder empty.
fn linked_bare_submodule(g: &Gate, name: &str) -> PathBuf {
    let wt = agents_folder(g).join(format!("aterm-{name}.noindex"));
    git(
        &g.checkout(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &format!("agent/{name}"),
            &for_git(&wt),
            "HEAD",
        ],
    );
    wt
}

/// A SUBMODULE FOLDER HOLDING FILES BUT NO `.git` KEEPS ITS WORKTREE: git status, under any
/// `--ignore-submodules`, sees nothing in a gitlink folder that is not checked out, so files
/// copied into it would go with the tree; so it stays, on a fresh judgment and on a stopped
/// removal's resume, whatever the files' times. NEGATIVE CONTROLS: the empty folder git leaves
/// is admitted, fresh and from the receipt; without the check the files are admitted both ways.
#[test]
fn a_submodule_folder_holding_files_but_no_git_keeps_its_worktree() {
    let g = gate();
    let wt = linked_bare_submodule(&g, "copied");
    let folder = wt.join("vendor/astream");
    assert!(folder.is_dir(), "git leaves the gitlink's folder");
    super::judge_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now)
        .expect("control: the empty folder git leaves is admitted");
    std::fs::write(folder.join("lib.rs"), "// copied in, then edited\n").expect("written");
    let judged = super::judge_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now);
    assert!(
        judged
            .as_ref()
            .is_err_and(|why| why.contains("vendor/astream/ holds files but no .git")),
        "{judged:?}"
    );
    let removed = super::remove_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now);
    assert!(removed.is_err(), "{removed:?}");
    assert!(folder.join("lib.rs").is_file(), "the copy stays");

    let resumed = linked_bare_submodule(&g, "resumed");
    let text = std::fs::read(resumed.join(".git")).expect("its .git");
    let entry = super::own_entry(&g.checkout(), &resumed, &text).expect("its entry");
    let head = super::listed(&GitCli, &g.checkout(), &resumed)
        .expect("git worktree list")
        .expect("listed")
        .head;
    super::pending::begin(&g.checkout(), &resumed, &entry, &head, &text).expect("a receipt");
    std::fs::remove_file(resumed.join("Cargo.toml")).expect("a tracked file gone");
    super::judge_worktree(&GitCli, g.home(), &g.checkout(), &resumed, g.now)
        .expect("control: the stopped removal's tree is admitted from its receipt");
    let copy = resumed.join("vendor/astream/lib.rs");
    std::fs::write(&copy, "// copied in with its old time\n").expect("written");
    let month = SystemTime::now() - Duration::from_secs(30 * 86_400);
    std::fs::File::options()
        .write(true)
        .open(&copy)
        .and_then(|f| f.set_modified(month))
        .expect("its old time");
    let judged = super::judge_worktree(&GitCli, g.home(), &g.checkout(), &resumed, g.now);
    assert!(
        judged
            .as_ref()
            .is_err_and(|why| why.contains("vendor/astream/ holds files but no .git")),
        "{judged:?}"
    );
    let removed = super::remove_worktree(&GitCli, g.home(), &g.checkout(), &resumed, g.now);
    assert!(removed.is_err(), "{removed:?}");
    assert!(copy.is_file(), "the copy stays");
}

/// Give `path` a modification time a month before the clock: what a move, a `cp -p` or a restore
/// from a backup keeps, older than any receipt's start.
fn a_month_old(path: &Path) {
    let month = SystemTime::now() - Duration::from_secs(30 * 86_400);
    std::fs::File::options()
        .read(true)
        .open(path)
        .and_then(|f| f.set_modified(month))
        .expect("its old time");
}

/// A TREE THAT STANDS ONCE GIT'S ENTRY FOR IT WENT STAYS: a removal that took a linked
/// worktree's `.git` stopped; `git worktree prune` then drops git's entry for it, whose `.git` is
/// gone; a person restores a month-old note into what is left. Its receipt still names it, and
/// with no entry nothing can say what was put in it since, so it stays with the note. NEGATIVE
/// CONTROLS: the same tree before the prune is admitted from its receipt; without the rule the
/// note, older than the receipt, goes with the tree.
#[test]
fn a_tree_standing_once_its_git_entry_went_stays() {
    let g = gate();
    let wt = linked(&g, "pruned");
    let text = std::fs::read(wt.join(".git")).expect("its .git");
    let entry = super::own_entry(&g.checkout(), &wt, &text).expect("its entry");
    let head = super::listed(&GitCli, &g.checkout(), &wt)
        .expect("git worktree list")
        .expect("listed")
        .head;
    super::pending::begin(&g.checkout(), &wt, &entry, &head, &text).expect("a receipt");
    std::fs::remove_file(wt.join(".git")).expect("its .git gone, as the removal takes it first");
    super::judge_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now)
        .expect("control: with git's entry, the stopped removal is admitted from its receipt");

    git(&g.checkout(), &["worktree", "prune"]);
    assert!(entry.symlink_metadata().is_err(), "git dropped its entry");
    let note = wt.join("notes.md");
    std::fs::write(&note, "restored from a backup\n").expect("written");
    a_month_old(&note);
    let judged = super::judge_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now);
    assert!(
        judged
            .as_ref()
            .is_err_and(|why| why.contains("went while its tree stands")),
        "{judged:?}"
    );
    let removed = super::remove_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now);
    assert!(removed.is_err(), "{removed:?}");
    assert_eq!(
        std::fs::read_to_string(&note).expect("the note"),
        "restored from a backup\n"
    );
    assert!(wt.join("Cargo.toml").is_file(), "what is left stays");
}

/// WHAT A STOPPED REMOVAL DID NOT DO KEEPS ITS TREE, IGNORED OR IN A SUBMODULE: a receipt stands
/// for a linked worktree that recorded its submodule's tagged `target/`, and the removal took a
/// tracked file of the worktree and one of its submodule. Then, each with a month-old time, an
/// ignored file put in the worktree, an ignored note put in the submodule, and an edit to the
/// submodule's tracked file each keep it, judged and removed alike. NEGATIVE CONTROLS: what the
/// removal did, the recorded `target/` included, is admitted from the receipt; a status read
/// without `--ignored`, or of the worktree alone with its submodules ignored, admits the first
/// two and the third.
#[test]
fn what_a_stopped_removal_did_not_do_keeps_its_tree() {
    let g = gate();
    let wt = linked(&g, "stopped");
    let sub = wt.join("vendor/astream");
    exclude_in(&sub, "/target/");
    exclude_in(&sub, "/.claude/");
    cache_tag(&sub.join("target"));
    let ignored = super::ignored_entries(&GitCli, &wt).expect("its ignored entries");
    assert_eq!(ignored, ["vendor/astream/target/"]);
    let text = std::fs::read(wt.join(".git")).expect("its .git");
    let entry = super::own_entry(&g.checkout(), &wt, &text).expect("its entry");
    let head = super::listed(&GitCli, &g.checkout(), &wt)
        .expect("git worktree list")
        .expect("listed")
        .head;
    let receipt = super::pending::prepare(
        &wt,
        &entry,
        &head,
        &text,
        super::pending::file_clock_now(),
        ignored,
    )
    .expect("a receipt");
    super::pending::commit(&g.checkout(), &receipt).expect("committed");
    std::fs::remove_file(wt.join("Cargo.toml")).expect("a tracked file gone");
    std::fs::remove_file(sub.join("lib.rs")).expect("a submodule's tracked file gone");
    super::judge_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now)
        .expect("control: what the removal did is admitted from its receipt");

    let put = wt.join(".aterm-verify");
    let note = sub.join(".claude/notes.md");
    let edit = sub.join("lib.rs");
    for (path, said) in [
        (&put, "!! .aterm-verify"),
        (&note, "!! .claude/ in vendor/astream/"),
        (&edit, " M lib.rs in vendor/astream/"),
    ] {
        std::fs::create_dir_all(path.parent().expect("its folder")).expect("a folder");
        std::fs::write(path, "mine\n").expect("written");
        a_month_old(path);
        let judged = super::judge_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now);
        assert!(
            judged.as_ref().is_err_and(|why| why.contains(said)),
            "{said}: {judged:?}"
        );
        let removed = super::remove_worktree(&GitCli, g.home(), &g.checkout(), &wt, g.now);
        assert!(removed.is_err(), "{said}: {removed:?}");
        assert_eq!(std::fs::read_to_string(path).expect("it stays"), "mine\n");
        std::fs::remove_file(path).expect("taken away for the next case");
    }
}

/// Git as [`GitCli`] answers it, with `after` called once each run has answered (its folder and
/// arguments) and `history` before each bounded history read: a writer racing a removal.
struct Racing<'a> {
    after: &'a dyn Fn(&Path, &[&str]),
    history: &'a dyn Fn(),
}

impl super::Git for Racing<'_> {
    fn run(&self, dir: &Path, args: &[&str]) -> super::Answer {
        self.run_with_alt(dir, args, None)
    }

    fn run_with_alt(&self, dir: &Path, args: &[&str], alt: Option<&Path>) -> super::Answer {
        let answer = GitCli.run_with_alt(dir, args, alt);
        (self.after)(dir, args);
        answer
    }

    fn read_history(
        &self,
        dir: &Path,
        args: &[&str],
        input: &[u8],
        objects: Option<&Path>,
        deadline: std::time::Instant,
    ) -> super::Answer {
        (self.history)();
        GitCli.read_history(dir, args, input, objects, deadline)
    }
}

/// An edit a person saves into the worktree `wt` while its removal runs.
const LATE: &str = "[workspace]\n# saved while the removal ran\n";

/// A WRITE GIT'S LAST READ MISSED KEEPS THE WORKTREE: an edit saved just after the removal's own
/// ignored-entry read (the last `git status` before its receipt) is one no git read sees, and
/// the receipt's start, taken before the removal's first git read, dates it as new, so custody
/// keeps the tree and withdraws the receipt. NEGATIVE CONTROL: with the start taken after that
/// read, the edit is older than it and goes with the tree.
#[test]
fn a_write_the_last_git_read_missed_keeps_the_worktree() {
    let g = gate();
    let wt = linked(&g, "raced");
    let reads = std::cell::Cell::new(0usize);
    let count = |dir: &Path, args: &[&str]| {
        if dir == wt && args.contains(&"--ignored") {
            reads.set(reads.get() + 1);
        }
    };
    let quiet = || {};
    let counting = Racing {
        after: &count,
        history: &quiet,
    };
    super::judge_worktree(&counting, g.home(), &g.checkout(), &wt, g.now)
        .expect("control: it is admitted");
    let judged = reads.replace(0);
    let save = |dir: &Path, args: &[&str]| {
        count(dir, args);
        if dir == wt && args.contains(&"--ignored") && reads.get() == judged + 1 {
            std::fs::write(wt.join("Cargo.toml"), LATE).expect("saved");
        }
    };
    let racing = Racing {
        after: &save,
        history: &quiet,
    };
    let removed = super::remove_worktree(&racing, g.home(), &g.checkout(), &wt, g.now);
    assert!(
        removed
            .as_ref()
            .is_err_and(|why| why.contains("written after")),
        "{removed:?}"
    );
    assert_eq!(
        std::fs::read_to_string(wt.join("Cargo.toml")).expect("the edit"),
        LATE
    );
    assert!(g.has_worktree(&wt), "{}", g.worktrees());
    assert!(linked_receipts(&g).is_empty(), "the receipt is withdrawn");
}

/// A WRITE WHILE THE HISTORY CHECK READS KEEPS THE WORKTREE: the private-history check of its
/// submodule's store (up to thirty seconds) runs before custody's last time check, so an edit
/// saved while it reads is seen before the first deletion. NEGATIVE CONTROL: with the history
/// check after the time check, the edit goes with the tree.
#[test]
fn a_write_while_the_history_check_reads_keeps_the_worktree() {
    let g = gate();
    let wt = linked(&g, "slow");
    let saved = std::cell::Cell::new(false);
    let none = |_: &Path, _: &[&str]| {};
    let save = || {
        if !saved.get() && !linked_receipts(&g).is_empty() && wt.join("Cargo.toml").is_file() {
            std::fs::write(wt.join("Cargo.toml"), LATE).expect("saved");
            saved.set(true);
        }
    };
    let racing = Racing {
        after: &none,
        history: &save,
    };
    let removed = super::remove_worktree(&racing, g.home(), &g.checkout(), &wt, g.now);
    assert!(saved.get(), "the history check read once its receipt stood");
    assert!(
        removed
            .as_ref()
            .is_err_and(|why| why.contains("written after")),
        "{removed:?}"
    );
    assert_eq!(
        std::fs::read_to_string(wt.join("Cargo.toml")).expect("the edit"),
        LATE
    );
    assert!(g.has_worktree(&wt), "{}", g.worktrees());
    assert!(linked_receipts(&g).is_empty(), "the receipt is withdrawn");
}
