// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Process ownership at the build-cache removal boundary.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use aterm_uds::holders::PathScan;

pub type Processes<'a> = &'a dyn Fn(&[PathBuf], &[u32]) -> std::io::Result<PathScan>;

/// One bounded reason a pass keeps a cache, without retaining a process table or its paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Blocked {
    Used(u32, &'static str),
    Unreadable(u32),
    Unavailable,
}

impl std::fmt::Display for Blocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Used(pid, how) => write!(f, "process {pid} uses it ({how})"),
            Self::Unreadable(pid) => write!(f, "process {pid} could not be ruled out"),
            Self::Unavailable => f.write_str("the process ownership scan is unavailable"),
        }
    }
}

fn unused(scan: std::io::Result<PathScan>) -> Result<(), Blocked> {
    let scan = scan.map_err(|_| Blocked::Unavailable)?;
    if let Some(user) = scan.users.first() {
        let how = match user.how {
            "cwd" => "working directory",
            "exe" => "executable",
            "map" => "mapped file",
            _ => "open file",
        };
        return Err(Blocked::Used(user.pid, how));
    }
    if let Some((pid, _)) = scan
        .unreadable
        .iter()
        .find(|(_, exe)| !aterm_uds::holders::unix_platform_service(exe.as_deref()))
    {
        return Err(Blocked::Unreadable(*pid));
    }
    Ok(())
}

/// A pass's injected process probe; fixtures use an inert arm, never the owner's processes.
pub(super) struct Fence<'a> {
    processes: Processes<'a>,
    told: Cell<bool>,
}

impl<'a> Fence<'a> {
    pub(super) fn new(processes: Processes<'a>) -> Self {
        Self {
            processes,
            told: Cell::new(false),
        }
    }

    /// Both spellings matter after a rename: a running image's recorded path may still name
    /// its original cache. Neither a resumed stage nor its replacement may hide that holder.
    pub(super) fn clear(&self, original: &Path, current: &Path) -> bool {
        self.clear_all(&[original, current])
    }

    pub(super) fn clear_all(&self, paths: &[&Path]) -> bool {
        let mut dirs = Vec::new();
        for &path in paths {
            if !dirs.iter().any(|dir: &PathBuf| dir == path) {
                dirs.push(path.to_path_buf());
            }
            if let Ok(real) = path.canonicalize()
                && !dirs.contains(&real)
            {
                dirs.push(real);
            }
            // The original entry may already have moved. Resolve its surviving parent
            // so a relative path or a symlinked parent still names the kernel's old image.
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            if let Some(name) = path.file_name()
                && let Ok(parent) = parent.canonicalize()
            {
                let original = parent.join(name);
                if !dirs.contains(&original) {
                    dirs.push(original);
                }
            }
        }
        match unused((self.processes)(&dirs, &[std::process::id()])) {
            Ok(()) => true,
            Err(reason) => {
                // One bounded explanation per pass, including a failed or incomplete scan.
                if !self.told.replace(true) {
                    aterm_log::warn!("build cache kept: {reason}");
                }
                false
            }
        }
    }
}

impl Default for Fence<'static> {
    fn default() -> Self {
        #[cfg(not(test))]
        let processes: Processes<'static> = &live;
        #[cfg(test)]
        let processes: Processes<'static> = &|_, _| Ok(PathScan::default());
        Self::new(processes)
    }
}

/// The regular disk pass's kernel reader, retaining this remover's executable and cwd while
/// ignoring only its own lock/directory descriptors. Including self in the reader also keeps
/// legitimate same-image children from being mistaken for a skipped parent's pre-exec fork.
#[cfg(any(not(test), target_os = "macos", target_os = "linux"))]
pub(super) fn live(dirs: &[PathBuf], skip: &[u32]) -> std::io::Result<PathScan> {
    observed(dirs, skip, &kernel)
}

#[cfg(any(not(test), target_os = "macos", target_os = "linux"))]
fn kernel(dirs: &[PathBuf], skip: &[u32]) -> std::io::Result<PathScan> {
    #[cfg(all(debug_assertions, target_vendor = "apple"))]
    {
        match aterm_uds::holders::lab_jail_pids() {
            Some(pids) => aterm_uds::holders::scan_path_users_of(&pids?, dirs, skip),
            None => aterm_uds::holders::scan_path_users(dirs, skip),
        }
    }
    #[cfg(not(all(debug_assertions, target_vendor = "apple")))]
    aterm_uds::holders::scan_path_users(dirs, skip)
}

fn observed(dirs: &[PathBuf], skip: &[u32], processes: Processes<'_>) -> std::io::Result<PathScan> {
    let me = std::process::id();
    let skipped: Vec<u32> = skip.iter().copied().filter(|pid| *pid != me).collect();
    let mut scan = processes(dirs, &skipped)?;
    scan.users.retain(|user| user.pid != me || user.how != "fd");
    Ok(scan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_uds::holders::PathUse;

    #[test]
    fn unavailable_ownership_keeps_a_cache_but_platform_services_do_not_poison_the_scan() {
        assert_eq!(
            unused(Err(std::io::Error::other("unavailable"))),
            Err(Blocked::Unavailable)
        );
        let mut scan = PathScan {
            unreadable: vec![(17, Some(PathBuf::from("/System/Library/service")))],
            ..PathScan::default()
        };
        assert_eq!(unused(Ok(scan.clone())), Ok(()));
        scan.unreadable
            .push((18, Some(PathBuf::from("/Users//u/tool"))));
        assert_eq!(unused(Ok(scan.clone())), Err(Blocked::Unreadable(18)));
        scan.users.push(PathUse {
            pid: 19,
            how: "exe",
            path: PathBuf::from("/cache/program"),
        });
        assert_eq!(unused(Ok(scan)), Err(Blocked::Used(19, "executable")));
    }

    #[test]
    fn an_old_image_spelling_and_the_stage_spelling_are_both_checked() {
        let original = Path::new("/cache/build");
        let stage = Path::new("/cache/retiring");
        for reported in [original, stage] {
            let processes = |dirs: &[PathBuf], _: &[u32]| {
                assert!(dirs.contains(&original.to_path_buf()));
                assert!(dirs.contains(&stage.to_path_buf()));
                Ok(PathScan {
                    users: vec![PathUse {
                        pid: 20,
                        how: "exe",
                        path: reported.join("program"),
                    }],
                    ..PathScan::default()
                })
            };
            assert!(!Fence::new(&processes).clear(original, stage));
        }
        assert!(Fence::new(&|_, _| Ok(PathScan::default())).clear(original, stage));
    }

    #[test]
    fn only_the_removers_own_descriptors_are_ignored() {
        let me = std::process::id();
        for how in ["exe", "cwd", "fd", "future-use"] {
            let processes = |_: &[PathBuf], skip: &[u32]| {
                assert!(
                    !skip.contains(&me),
                    "a same-image child must not inherit fork suppression"
                );
                Ok(PathScan {
                    users: vec![
                        PathUse {
                            pid: me,
                            how,
                            path: PathBuf::from("/cache/self"),
                        },
                        PathUse {
                            pid: me + 1,
                            how: "fd",
                            path: PathBuf::from("/cache/child"),
                        },
                    ],
                    ..PathScan::default()
                })
            };
            let scan = observed(&[], &[me], &processes).unwrap();
            assert_eq!(scan.users.iter().any(|user| user.pid == me), how != "fd");
            assert!(scan.users.iter().any(|user| user.pid == me + 1));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_relative_cache_keeps_its_absolute_original_after_staging_through_a_symlink() {
        let world = aterm_tempfile::tempdir().unwrap();
        let real = world.path().canonicalize().unwrap().join("real");
        let alias = world.path().join("alias");
        let original = real.join("cache.noindex");
        let stage = real.join("cache.noindex.removing");
        std::fs::create_dir_all(&original).unwrap();
        std::fs::write(original.join("program"), b"inert ownership fixture").unwrap();
        std::os::unix::fs::symlink(&real, &alias).unwrap();

        // Reach the private fixture from the actual cwd without changing global cwd.
        let mut relative = PathBuf::new();
        for component in std::env::current_dir().unwrap().components() {
            if matches!(component, std::path::Component::Normal(_)) {
                relative.push("..");
            }
        }
        relative.push(alias.strip_prefix("/").unwrap());
        relative.push("cache.noindex");
        let staged_probe = Cell::new(false);
        let processes = |dirs: &[PathBuf], _: &[u32]| {
            let reported = original.join("program");
            let users = if stage.is_dir() {
                staged_probe.set(true);
                dirs.iter()
                    .any(|dir| reported.starts_with(dir))
                    .then_some(PathUse {
                        pid: 21,
                        how: "exe",
                        path: reported,
                    })
                    .into_iter()
                    .collect()
            } else {
                Vec::new()
            };
            Ok(PathScan {
                users,
                ..PathScan::default()
            })
        };
        let removed = super::super::remove_unbuilt_on(&relative, &Fence::new(&processes)).unwrap();
        assert!(
            staged_probe.get(),
            "the late holder is observed after the rename"
        );
        assert!(
            !removed,
            "the recorded absolute original must keep its stage"
        );
        assert!(stage.join("program").is_file());
    }

    /// One exact image exercises every automatic cache route. No image or ancestor is
    /// renamed or removed until its child has echoed a line from inside that actual image.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_running_cache_image_keeps_whole_registered_tooling_and_resumed_stages() {
        use aterm_uds::deadline::{KeptUntilRan, ad_hoc_copy, cat_echoes, kill_within};
        use aterm_uds::spawnfd::{Child, Stdio};
        use std::time::{Duration, SystemTime};

        struct Running {
            child: Child,
            stdin: Option<std::process::ChildStdin>,
        }
        impl Drop for Running {
            fn drop(&mut self) {
                kill_within(&mut self.child, Duration::from_secs(60));
            }
        }

        let directory = aterm_tempfile::tempdir().unwrap().keep();
        let mut custody = KeptUntilRan::new(directory);
        let home = custody.path().canonicalize().unwrap().join("home");
        let owner = home.join("workspace");
        std::fs::create_dir_all(&owner).unwrap();
        std::fs::write(owner.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        // The registrar deliberately excludes Library. Use an admitted private mirror
        // outside it so this same real image can exercise registered and mirrored targets.
        let root = home.join("cache.noindex");
        let leaf = super::super::build_dir(&root, &owner);
        std::fs::create_dir_all(leaf.parent().unwrap()).unwrap();
        assert!(super::super::registered::create_checked(&home, &leaf, &owner).unwrap());
        let tooling = leaf.join("debug/deps/tooling.noindex");
        let row = tooling.join("fish-7");
        let image = row.join("bin/cat");
        std::fs::create_dir_all(image.parent().unwrap()).unwrap();
        std::fs::write(leaf.join("debug/.cargo-lock"), b"").unwrap();
        ad_hoc_copy(Path::new("/bin/cat"), &image);
        let child = aterm_uds::spawnfd::command(&image)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut running = Running { child, stdin: None };
        running.stdin = Some(cat_echoes(&mut running.child));
        custody.ran();
        let pid = running.child.id();

        // Use the real production kernel reader, scoped to this proven fixture child so
        // unrelated processes outside the test do not determine its expected verdict.
        let processes = |dirs: &[PathBuf], skip: &[u32]| {
            let mut scan = live(dirs, skip)?;
            scan.users.retain(|user| user.pid == pid);
            scan.unreadable.retain(|(user, _)| *user == pid);
            Ok(scan)
        };
        let fence = Fence::new(&processes);
        let now = SystemTime::now();
        let day = Duration::from_secs(24 * 60 * 60);
        let gone = super::super::RECLAIM_GONE;
        let mut held = Vec::new();
        let idle = super::super::reclaim_watched_on(
            &root,
            Some(&home),
            now + super::super::RECLAIM_IDLE + day,
            gone,
            None,
            &mut |_, _| true,
            &fence,
        );
        held.push(idle.is_empty() && image.is_file());
        std::fs::remove_dir_all(&owner).unwrap();
        let absent = super::super::reclaim_watched_on(
            &root,
            Some(&home),
            now + 2 * day,
            gone,
            None,
            &mut |_, _| true,
            &fence,
        );
        held.push(absent.is_empty() && image.is_file());
        let registered = super::super::registered::reclaim_on(
            &home,
            now + 2 * day,
            gone,
            None,
            &mut |_, _| true,
            &fence,
        );
        held.push(registered.is_empty() && image.is_file());
        held.push(!super::super::remove_unbuilt_on(&leaf, &fence).unwrap() && image.is_file());
        let tools = super::super::tooling::reclaim_on(
            &tooling,
            now + super::super::RECLAIM_IDLE + day,
            &mut |_, _| true,
            &fence,
        );
        held.push(tools.is_empty() && image.is_file());

        // Resume a tooling stage whose name could mean `fish-7` or stage7 of `fish`.
        let tool_stage = tooling.join(".provision-reclaim-fish-7");
        if row.is_dir() {
            std::fs::rename(&row, &tool_stage).unwrap();
            let tools = super::super::tooling::reclaim_on(&tooling, now, &mut |_, _| true, &fence);
            held.push(tools.is_empty() && tool_stage.join("bin/cat").is_file());
        } else {
            held.push(false);
        }
        let stage = leaf.parent().unwrap().join(super::super::DOOMED);
        if leaf.is_dir() {
            super::super::registered::stage_for_process_test(&home, &leaf, &stage);
            let mirror = super::super::reclaim_watched_on(
                &root,
                Some(&home),
                now + 2 * day,
                gone,
                None,
                &mut |_, _| true,
                &fence,
            );
            let pending = super::super::registered::reclaim_on(
                &home,
                now + 2 * day,
                gone,
                None,
                &mut |_, _| true,
                &fence,
            );
            held.push(
                mirror.is_empty() && pending.is_empty() && stage.join("cache.noindex").is_dir(),
            );
        } else {
            held.push(false);
        }
        kill_within(&mut running.child, Duration::from_secs(60));
        assert!(
            running.child.try_wait().unwrap().is_some(),
            "the proven child was reaped"
        );
        drop(running);
        assert!(
            held.iter().all(|kept| *kept),
            "a live exact image must retain every route: {held:?}"
        );

        // The same identity-bound pending payload goes after its only holder exits.
        let removed = super::super::registered::reclaim_on(
            &home,
            now + 2 * day,
            gone,
            None,
            &mut |_, _| true,
            &fence,
        );
        assert_eq!(
            removed.len(),
            1,
            "the clear kernel observation must allow reclamation"
        );
        assert!(!stage.exists());
    }
}
