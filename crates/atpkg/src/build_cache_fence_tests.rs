// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_uds::holders::PathUse;

#[test]
fn unavailable_ownership_keeps_a_cache_but_platform_services_do_not_poison_the_scan() {
    assert_eq!(
        unused(Err(std::io::Error::other("unavailable"))),
        Err(KeepReason::ProcessUnavailable)
    );
    let mut scan = PathScan {
        unreadable: vec![(17, Some(PathBuf::from("/System/Library/service")))],
        ..PathScan::default()
    };
    assert_eq!(unused(Ok(scan.clone())), Ok(()));
    scan.unreadable
        .push((18, Some(PathBuf::from("/Users//u/tool"))));
    assert_eq!(
        unused(Ok(scan.clone())),
        Err(KeepReason::ProcessUnreadable(18))
    );
    scan.users.push(PathUse {
        pid: 19,
        how: "exe",
        path: PathBuf::from("/cache/program"),
    });
    assert_eq!(
        unused(Ok(scan)),
        Err(KeepReason::ProcessUsed {
            pid: 19,
            how: "executable",
            observed: PathBuf::from("/cache/program")
        })
    );
}

#[test]
fn diagnostic_evidence_is_built_only_for_a_reporting_pass() {
    let calls = Cell::new(0);
    let reason = || {
        calls.set(calls.get() + 1);
        KeepReason::InventoryUnavailable
    };
    let path = Path::new("/cache");
    Fence::default().note_with(path, reason);
    assert_eq!(calls.get(), 0);
    let reporting = Fence::default().with_reporting();
    reporting.note_with(path, reason);
    assert_eq!(calls.get(), 1);
    assert!(matches!(
        reporting.retained().as_slice(),
        [RetainedCheck {
            reason: KeepReason::InventoryUnavailable,
            ..
        }]
    ));
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
fn cleanup_reports_the_observed_union_path_without_attributing_use_to_every_path() {
    let original = Path::new("/fixture/original");
    let current = Path::new("/fixture/current");
    for used in [original, current] {
        let observed = used.join("program");
        let processes = |_: &[PathBuf], _: &[u32]| {
            Ok(PathScan {
                users: vec![PathUse {
                    pid: 43,
                    how: "exe",
                    path: observed.clone(),
                }],
                ..PathScan::default()
            })
        };
        let fence = Fence::new(&processes).with_reporting();
        assert!(!fence.clear(original, current));
        let retained = fence.retained();
        assert_eq!(
            retained.len(),
            1,
            "an old image spelling is no second retained candidate"
        );
        let record = &retained[0];
        assert_eq!(record.path, current);
        assert_eq!(
            record.reason,
            KeepReason::ProcessUsed {
                pid: 43,
                how: "executable",
                observed: observed.clone()
            }
        );
        let words = record.words();
        assert!(words.contains(&format!("a checked removal path {}", observed.display())));
        let other = if used == original { current } else { original };
        assert!(
            !words.contains(&format!(
                "a checked removal path {}",
                other.join("program").display()
            )),
            "a union scan supplies no use observation for that other path: {words}"
        );
    }
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

#[test]
fn ignoring_own_descriptor_use_preserves_independent_uncertainty() {
    let me = std::process::id();
    let path = PathBuf::from("/cache/partial-probe");
    let scan = PathScan {
        users: vec![PathUse {
            pid: me,
            how: "fd",
            path: path.join("guard"),
        }],
        unreadable: vec![(me, Some(PathBuf::from("/private/test-remover")))],
        scanned: 1,
    };
    let processes = |_: &[PathBuf], _: &[u32]| Ok(scan.clone());
    let filtered = |dirs: &[PathBuf], skip: &[u32]| observed(dirs, skip, &processes);
    let fence = Fence::new(&filtered).with_reporting();
    assert!(!fence.clear_all(&path, &[&path]));
    assert_eq!(
        fence.retained().as_slice(),
        [RetainedCheck {
            path: path.clone(),
            reason: KeepReason::ProcessUnreadable(me),
        }]
    );

    let mut complete = scan.clone();
    complete.unreadable.clear();
    let processes = |_: &[PathBuf], _: &[u32]| Ok(complete.clone());
    let filtered = |dirs: &[PathBuf], skip: &[u32]| observed(dirs, skip, &processes);
    assert!(Fence::new(&filtered).clear_all(&path, &[&path]));
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

/// One mapped file inside the tree holds it through every automatic cache route. The test
/// process maps the file itself, so the production kernel reader sees a holder under the tree's
/// path with no child to exec: the kernel reports a mapping by its file's current path, which
/// follows the tree through each staged rename. That the same reader finds a running executable
/// by its path is aterm-uds' to prove (`holders_paths_tests.rs`).
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_mapped_cache_file_keeps_whole_registered_tooling_and_resumed_stages() {
    use std::os::fd::AsFd as _;
    use std::time::{Duration, SystemTime};

    let directory = aterm_tempfile::tempdir().unwrap();
    let home = directory.path().canonicalize().unwrap().join("home");
    let owner = home.join("workspace");
    // Private whatever the umask: a registered target is laid only through folders no
    // group or other user can write, from the home down.
    let private = |at: &Path| {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(at)
            .unwrap();
    };
    private(&owner);
    std::fs::write(owner.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    // The registrar deliberately excludes Library. Use an admitted private mirror
    // outside it so this same real image can exercise registered and mirrored targets.
    let root = home.join("cache.noindex");
    let leaf = super::super::build_dir(&root, &owner);
    private(leaf.parent().unwrap());
    assert!(super::super::registered::create_checked(&home, &leaf, &owner).unwrap());
    let tooling = leaf.join("debug/deps/tooling.noindex");
    let row = tooling.join("fish-7");
    let image = row.join("bin/mapped");
    std::fs::create_dir_all(image.parent().unwrap()).unwrap();
    std::fs::write(leaf.join("debug/.cargo-lock"), b"").unwrap();
    std::fs::write(&image, vec![0x71; aterm_uds::shm::PAGE]).unwrap();
    let file = std::fs::File::open(&image).unwrap();
    // SAFETY: this private file has exactly PAGE bytes and nothing writes, resizes or removes
    // it while the region lives: every route below is asserted to keep it, and the region is
    // dropped before the one reclaim that may remove it.
    let region =
        unsafe { aterm_uds::shm::Region::map_readonly(file.as_fd(), aterm_uds::shm::PAGE) }
            .unwrap();
    drop(file);
    let mut byte = [0];
    region.read_into(0, &mut byte).unwrap();
    assert_eq!(byte, [0x71]);
    let me = std::process::id();

    // Use the real production kernel reader, which ignores this process's own descriptors
    // but not its mappings, scoped to this process so unrelated processes outside the test
    // do not determine its expected verdict.
    let processes = |dirs: &[PathBuf], skip: &[u32]| {
        let mut scan = live(dirs, skip)?;
        scan.users.retain(|user| user.pid == me);
        scan.unreadable.retain(|(user, _)| *user == me);
        Ok(scan)
    };
    let fence = Fence::new(&processes);
    let now = SystemTime::now();
    let day = Duration::from_secs(24 * 60 * 60);
    let gone = super::super::ReclaimAfter::CALM;
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
        held.push(tools.is_empty() && tool_stage.join("bin/mapped").is_file());
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
        held.push(mirror.is_empty() && pending.is_empty() && stage.join("cache.noindex").is_dir());
    } else {
        held.push(false);
    }
    drop(region);
    assert!(
        held.iter().all(|kept| *kept),
        "a mapped file must retain every route: {held:?}"
    );

    // The same identity-bound pending payload goes once its only holder unmaps it.
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
