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

/// An actually running image keeps every retired gate route. The child proves execution
/// before any removal can touch its image; after it is reaped, the same leftover can go.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn running_images_keep_retired_snapshots_cells_and_locks_until_reaped() {
    use aterm_uds::deadline::{KeptUntilRan, ad_hoc_copy, cat_echoes, kill_within};
    use aterm_uds::spawnfd::{Child, Stdio};

    struct Running(Child);
    impl Drop for Running {
        fn drop(&mut self) {
            kill_within(&mut self.0, Duration::from_secs(60));
        }
    }

    let mut outcomes = Vec::new();
    for route in 0..4 {
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
        let retired_root = if route == 3 { &paths[0] } else { &paths[route] };
        let held = done
            .iter()
            .find(|(left, _)| left.path == *retired_root)
            .is_some_and(|(_, result)| result.is_err())
            && image.is_file();
        let snapshot_unchanged = (route != 0 && route != 3)
            || (paths[0].join(".git").is_file()
                && paths[1].is_dir()
                && GitCli
                    .run(&checkout, &["worktree", "list", "--porcelain"])
                    .out
                    .contains(&*paths[0].to_string_lossy()));
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
/// processes. Clearing that same observation permits the existing eligible cleanup.
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
    assert!(g.worktrees().contains(&*g.snapshot().to_string_lossy()));
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
    assert!(g.worktrees().contains(&*g.snapshot().to_string_lossy()));
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
    assert_eq!(kept.first, [PathBuf::from("holder")]);
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
        first: vec![PathBuf::from("target/held/stuck")],
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

/// A SNAPSHOT GIT CANNOT READ STAYS AND SAYS WHAT A PERSON DOES: its submodule borrowed its
/// objects from a store an `objects/info/alternates` entry names (as this Mac's snapshot
/// borrowed from a worktree since removed), and that store is gone: `git status` fails, the
/// snapshot stays with the submodule and the missing store named and the remedy (repair those
/// alternates or remove it), and its cells lane with it. NEGATIVE CONTROL: while the store is
/// there, git reads it through the alternates and the snapshot goes.
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

    std::fs::remove_dir_all(&keeper).expect("the store gone");
    let status = GitCli.run(
        &g.snapshot(),
        &[
            "status",
            "--porcelain",
            "--ignore-submodules=none",
            "--untracked-files=normal",
        ],
    );
    assert_ne!(status.code, Some(0), "git cannot read it");
    let why = super::Unreadable::read(&status.err);
    assert_eq!(
        (why.submodule.as_deref(), why.missing_store.as_deref()),
        (Some("vendor/astream"), Some(store.as_path())),
        "{status:?}"
    );
    let planned = plan(&g.roots, g.now, &GitCli);
    assert_eq!(planned[0].verdict, Err(why.words()), "{planned:#?}");
    assert!(!planned[1].goes(), "the cells lane stays with it");
    let done = retire(&g.roots, g.now, &GitCli);
    assert!(done[0].1.is_err() && done[1].1.is_err(), "{done:#?}");
    assert!(g.snapshot().join("Cargo.toml").is_file() && g.cells().is_dir());
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
