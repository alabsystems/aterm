// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[cfg(unix)]
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

#[cfg(unix)]
fn file(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"x").unwrap();
}

/// Set `path`'s access and modification times to `at`, without following a link.
#[cfg(unix)]
fn stamp(path: &Path, at: SystemTime) {
    let file = std::fs::File::open(path).unwrap();
    file.set_times(std::fs::FileTimes::new().set_accessed(at).set_modified(at))
        .unwrap();
}

/// Every file and folder of `dir`, to [`DEPTH`] below it, last touched at `at`.
#[cfg(unix)]
fn age_tree(dir: &Path, at: SystemTime) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        if entry.file_type().unwrap().is_dir() {
            age_tree(&entry.path(), at);
        }
        stamp(&entry.path(), at);
    }
    stamp(dir, at);
}

/// The idle rule over the tooling cache: a row unused a month goes, one a run used
/// stays (an exec's access time is use), a completed row's stage is finished, and a
/// link is never followed. Scratches with no exec stamp stay even when old or already
/// staged: no age proves their copied image finished running. NEGATIVE CONTROL: the same completed
/// row with one file's access time set yesterday (as an exec sets it) stays.
#[cfg(unix)]
#[test]
fn a_tool_unused_a_month_goes_and_one_a_run_used_stays() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().join("tooling.noindex");
    let now = SystemTime::now();
    let month = now - RECLAIM_IDLE - DAY;
    for row in ["pwsh-7.6.7", "fish-4.9.3", "binaryen-133"] {
        file(&root.join(row).join("bin/tool"));
        file(&root.join(row).join("stamp.json"));
        age_tree(&root.join(row), month);
    }
    let fish = root.join("fish-4.9.3/bin/tool");
    std::fs::File::open(&fish)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_accessed(now - DAY))
        .unwrap();
    file(&root.join(".provision-pwsh-abc/download/x.tar.gz"));
    age_tree(&root.join(".provision-pwsh-abc"), now - 2 * DAY);
    let unknown = [
        ".provision-fish-unknown",
        ".provision-reclaim-.provision-fish-staged-unknown",
    ];
    for scratch in unknown {
        file(&root.join(scratch).join("tree/bin/fish"));
        age_tree(&root.join(scratch), month);
    }
    file(&root.join(".provision-fish-new/src/Cargo.toml"));
    file(&root.join(".provision-reclaim-old/bin/tool"));
    let outside = dir.path().join("elsewhere");
    file(&outside.join("bin/tool"));
    age_tree(&outside, month);
    std::os::unix::fs::symlink(&outside, root.join("linked-1.0")).unwrap();

    let planned: Vec<(String, Unused)> = plan(&root, now)
        .into_iter()
        .map(|p| {
            (
                p.path.file_name().unwrap().to_string_lossy().into_owned(),
                p.why,
            )
        })
        .collect();
    let names: Vec<&str> = planned.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [".provision-reclaim-old", "binaryen-133", "pwsh-7.6.7"],
        "{planned:#?}"
    );
    assert!(planned.iter().any(|(n, w)| n == "pwsh-7.6.7"
        && matches!(w, Unused::Tooling { after, .. } if *after == RECLAIM_IDLE)));
    let held_tool = root.join("pwsh-7.6.7/bin/tool");
    let processes = |dirs: &[PathBuf], _: &[u32]| {
        let users = dirs
            .iter()
            .any(|dir| held_tool.starts_with(dir))
            .then(|| aterm_uds::holders::PathUse {
                pid: 23,
                how: "exe",
                path: held_tool.clone(),
            })
            .into_iter()
            .collect();
        Ok(aterm_uds::holders::PathScan {
            users,
            ..aterm_uds::holders::PathScan::default()
        })
    };
    let mut asked_held = false;
    assert!(
        reclaim_on(
            &root,
            now,
            &mut |path, _| {
                asked_held |= path.ends_with("pwsh-7.6.7");
                false
            },
            &super::super::fence::Fence::new(&processes)
        )
        .is_empty()
    );
    assert!(
        !asked_held,
        "a running tool is kept before recording or staging"
    );
    assert!(held_tool.is_file());
    let mut asked = Vec::new();
    let removed = reclaim(&root, now, &mut |p, why| {
        asked.push((p.to_path_buf(), why));
        !p.ends_with("binaryen-133")
    });
    assert_eq!(asked.len(), 3, "{asked:#?}");
    assert!(
        asked.iter().all(|(path, _)| {
            !path.ends_with(".provision-pwsh-abc")
                && unknown.iter().all(|scratch| !path.ends_with(scratch))
        }),
        "scratch retention precedes the intent callback: {asked:#?}"
    );
    let gone: Vec<&Path> = removed.iter().map(|(p, _)| p.as_path()).collect();
    assert_eq!(gone.len(), 2, "{removed:#?}");
    for kept in [
        "fish-4.9.3/bin/tool",
        "binaryen-133/bin/tool",
        ".provision-fish-new",
        ".provision-pwsh-abc/download/x.tar.gz",
    ] {
        assert!(root.join(kept).exists(), "{kept} stays");
    }
    for scratch in unknown {
        assert_eq!(
            std::fs::read(root.join(scratch).join("tree/bin/fish")).unwrap(),
            b"x",
            "the actual uncertain image stays at its original path"
        );
        assert!(
            !root.join(format!("{STAGE}{scratch}")).exists(),
            "an uncertain image is not renamed either"
        );
    }
    for went in ["pwsh-7.6.7", ".provision-reclaim-old"] {
        assert!(!root.join(went).exists(), "{went} goes");
    }
    assert!(
        !root.join(".provision-reclaim-pwsh-7.6.7").exists(),
        "the stage went with it"
    );
    assert!(
        outside.join("bin/tool").exists(),
        "a link is never followed"
    );
    assert!(root.join("linked-1.0").exists(), "nor removed");
}

/// Only an actual run proof admits a given-up scratch. Old, malformed and pre-reboot
/// exec stamps without proof stay; so do live scratches and non-file/link markers.
#[cfg(unix)]
#[test]
fn a_given_up_scratch_goes_only_after_its_run_is_proven() {
    use aterm_uds::deadline::{EXEC_STAMP, FIRST_EXEC_HANG, RAN};
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().join("tooling.noindex");
    let now = SystemTime::now();
    let kept = [
        (".provision-fish-spent", Some("")),
        (".provision-fish-rebooted", Some("18446744073709551615")),
        (".provision-fish-fresh", Some("0")),
        (".provision-fish-live", None),
        (".provision-fish-proof-live", None),
        (".provision-fish-proof-dir", Some("0")),
        (".provision-fish-proof-link", Some("0")),
        (".provision-fish-stamp-link", None),
        (".provision-fish-vetoed", Some("0")),
    ];
    let proven = ".provision-fish-proven";
    for (name, stamp) in kept.into_iter().chain([(proven, Some("0"))]) {
        file(
            &root
                .join(name)
                .join("target.noindex/release/build/x/build-script-build"),
        );
        if let Some(at) = stamp {
            std::fs::write(root.join(name).join(EXEC_STAMP), at).unwrap();
        }
        age_tree(&root.join(name), now - RECLAIM_IDLE - DAY);
    }
    std::fs::write(
        root.join(".provision-fish-fresh").join(EXEC_STAMP),
        aterm_uds::process::awake_ms()
            .map(|ms| ms.to_string())
            .unwrap_or_default(),
    )
    .unwrap();
    for name in [
        proven,
        ".provision-fish-proof-live",
        ".provision-fish-stamp-link",
        ".provision-fish-vetoed",
    ] {
        std::fs::write(root.join(name).join(RAN), b"").unwrap();
    }
    std::fs::create_dir(root.join(".provision-fish-proof-dir").join(RAN)).unwrap();
    let outside = dir.path().join("outside-proof");
    std::fs::write(&outside, b"outside").unwrap();
    std::os::unix::fs::symlink(&outside, root.join(".provision-fish-proof-link").join(RAN))
        .unwrap();
    std::os::unix::fs::symlink(
        &outside,
        root.join(".provision-fish-stamp-link").join(EXEC_STAMP),
    )
    .unwrap();

    let names = |planned: Vec<Planned>| {
        planned
            .into_iter()
            .map(|p| {
                assert_eq!(p.why, Unused::GivenUp);
                p.path.file_name().unwrap().to_string_lossy().into_owned()
            })
            .collect::<Vec<_>>()
    };
    let expected = vec![proven, ".provision-fish-vetoed"];
    assert_eq!(names(plan(&root, now)), expected);
    assert_eq!(
        names(plan(&root, now + RECLAIM_IDLE + FIRST_EXEC_HANG)),
        expected,
        "even another month and 600 seconds cannot replace a run proof"
    );
    let mut asked = Vec::new();
    let removed = reclaim(&root, now, &mut |path, why| {
        assert_eq!(why, Unused::GivenUp);
        asked.push(path.file_name().unwrap().to_string_lossy().into_owned());
        !path.ends_with(".provision-fish-vetoed")
    });
    assert_eq!(asked, expected, "unproved images never reach the callback");
    let gone: Vec<String> = removed
        .iter()
        .map(|(p, why)| {
            assert_eq!(*why, Unused::GivenUp, "{}", p.display());
            p.file_name().unwrap().to_string_lossy().into_owned()
        })
        .collect();
    assert_eq!(gone, [proven], "{removed:#?}");
    assert!(!root.join(proven).exists());
    for (name, _) in kept {
        assert_eq!(
            std::fs::read(
                root.join(name)
                    .join("target.noindex/release/build/x/build-script-build")
            )
            .unwrap(),
            b"x",
            "{name} stays at its original path"
        );
        assert!(!root.join(format!("{STAGE}{name}")).exists());
    }
    assert_eq!(std::fs::read(outside).unwrap(), b"outside");
}

/// A TOOL WHOSE RENAME IS REFUSED ONCE ITS INTENT IS ON RECORD CLOSES THAT INTENT: a pinned
/// tool unused a month passes the check and its intent is put on record; only then, before
/// its rename aside ([`super::super::seams::on_staging`]), is the rename made to fail (the
/// tooling root set to take no new name, every Unix, not as root; on macOS, the tool's folder
/// flagged `uchg`). The pass returns it refused by the rename's error kind, nothing of it
/// gone, whole and unstaged; a later pass with the refusal cleared takes it. NEGATIVE
/// CONTROL: without the outcome, the pass returns nothing for the intent it put on record.
#[cfg(unix)]
#[test]
fn a_tool_whose_rename_is_refused_after_its_intent_closes_that_intent() {
    use super::super::seams;
    struct Undo(PathBuf, fn(&Path));
    impl Drop for Undo {
        fn drop(&mut self) {
            seams::clear();
            (self.1)(&self.0);
        }
    }
    type Refuse = (&'static str, fn(&Path), fn(&Path));
    let mut refusals: Vec<Refuse> = Vec::new();
    if crate::platform::our_uid() != 0 {
        refusals.push((
            "a root that takes no new name",
            |tool| {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(
                    tool.parent().unwrap(),
                    std::fs::Permissions::from_mode(0o555),
                )
                .unwrap();
            },
            |tool| {
                use std::os::unix::fs::PermissionsExt as _;
                let _ = std::fs::set_permissions(
                    tool.parent().unwrap(),
                    std::fs::Permissions::from_mode(0o755),
                );
            },
        ));
    }
    #[cfg(target_os = "macos")]
    refusals.push((
        "uchg",
        |tool| crate::platform::set_file_flags(tool, libc::UF_IMMUTABLE).unwrap(),
        |tool| {
            let _ = crate::platform::set_file_flags(tool, 0);
        },
    ));
    assert!(!refusals.is_empty(), "no refusal this user can set");
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().join("tooling.noindex");
    let now = SystemTime::now();
    let refused = Unused::Refused {
        by: super::super::Refusal::Rename(std::io::ErrorKind::PermissionDenied),
    };
    for (n, (what, refuse, undo)) in refusals.into_iter().enumerate() {
        let tool = root.join(format!("pwsh-7.6.{n}"));
        file(&tool.join("bin/tool"));
        age_tree(&tool, now - RECLAIM_IDLE - DAY);
        let undone = Undo(tool.clone(), undo);
        seams::on_staging(refuse);
        let mut asked = Vec::new();
        let went = reclaim(&root, now, &mut |p, _| {
            asked.push(p.to_path_buf());
            true
        });
        assert_eq!(asked, std::slice::from_ref(&tool), "{what}: its intent");
        assert_eq!(went, [(tool.clone(), refused)], "{what}: its outcome");
        assert!(tool.join("bin/tool").is_file(), "{what}: whole");
        assert!(
            entries(&root).iter().all(|e| e == &tool),
            "{what}: never staged"
        );
        drop(undone);
        let went = reclaim(&root, now, &mut |_, _| true);
        assert!(
            matches!(went.as_slice(), [(p, Unused::Tooling { .. })] if p == &tool),
            "{what}: the next pass takes it: {went:?}"
        );
        assert!(!tool.exists(), "{what}");
    }
}

/// A TOOL STOPPED ONCE STAGED CLOSES AS ITS STAGE: a pinned tool unused a month whose rename
/// aside went through, and that a process comes to use in its stage once its intent is on
/// record, is returned as that stage with nothing of it gone ([`super::super::Refusal::Staged`]),
/// never as the tool's own path, which no longer holds it; the stage holds it all. That stage,
/// stopped the same way by the next pass, is returned as itself, `Staged`; and the pass after,
/// the process gone, finishes it ([`Unused::Unfinished`]). NEGATIVE CONTROL: the move not
/// marked on the open intent, the pass returns the tool's gone path as `Stopped`.
#[cfg(unix)]
#[test]
fn a_tool_stopped_once_staged_closes_as_its_stage() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().join("tooling.noindex");
    let now = SystemTime::now();
    let tool = root.join("pwsh-7.6.9");
    file(&tool.join("bin/tool"));
    age_tree(&tool, now - RECLAIM_IDLE - DAY);
    let (held, asked) = (std::cell::Cell::new(true), std::cell::Cell::new(false));
    let processes = |dirs: &[PathBuf], _: &[u32]| {
        let users = dirs
            .iter()
            .filter(|d| {
                held.get()
                    && asked.get()
                    && d.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with(STAGE))
            })
            .map(|stage| aterm_uds::holders::PathUse {
                pid: 35,
                how: "cwd",
                path: stage.join("bin"),
            })
            .collect();
        Ok(aterm_uds::holders::PathScan {
            users,
            ..aterm_uds::holders::PathScan::default()
        })
    };
    let fence = super::super::fence::Fence::new(&processes);
    let pass = || {
        asked.set(false);
        reclaim_on(
            &root,
            now,
            &mut |_, _| {
                asked.set(true);
                true
            },
            &fence,
        )
    };
    let staged = Unused::Refused {
        by: super::super::Refusal::Staged,
    };
    let went = pass();
    let stages: Vec<PathBuf> = entries(&root);
    assert_eq!(stages.len(), 1, "moved aside: {went:?}");
    let stage = stages[0].clone();
    assert_ne!(stage, tool, "moved aside");
    assert_eq!(
        went,
        [(stage.clone(), staged)],
        "its stage, nothing of it gone"
    );
    assert!(stage.join("bin/tool").is_file(), "the stage holds it all");
    assert_eq!(
        pass(),
        [(stage.clone(), staged)],
        "the stage, stopped after its intent, is itself"
    );
    held.set(false);
    assert_eq!(
        pass(),
        [(stage.clone(), Unused::Unfinished)],
        "the next pass finishes it"
    );
    assert!(!stage.exists() && !tool.exists());
}

/// A STUCK STAGE KEEPS NO TOOL OF ITS NAME: beside `.provision-reclaim-pwsh-7.6.7`, a stage an
/// earlier pass left that keeps one entry (read as another user's: the remover leaves it as it
/// is), the same tool unused a month again is staged under the next free name and goes whole
/// in the same pass; the stuck stage is asked for and reported once, and the plan lists each
/// once.
#[cfg(unix)]
#[test]
fn a_stuck_stage_keeps_no_tool_of_its_name() {
    use std::os::unix::fs::MetadataExt as _;
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            crate::build_cache::remove::seams::clear();
        }
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().join("tooling.noindex");
    let now = SystemTime::now();
    let tool = root.join("pwsh-7.6.7");
    file(&tool.join("bin/tool"));
    age_tree(&tool, now - RECLAIM_IDLE - DAY);
    let stage = root.join(".provision-reclaim-pwsh-7.6.7");
    let held = stage.join("bin/held");
    file(&held);
    let _clear = Clear;
    crate::build_cache::remove::seams::other_user(std::fs::symlink_metadata(&held).unwrap().ino());

    let planned: Vec<PathBuf> = plan(&root, now).into_iter().map(|p| p.path).collect();
    assert_eq!(planned, [stage.clone(), tool.clone()]);
    let mut asked = Vec::new();
    let removed = reclaim(&root, now, &mut |p, why| {
        asked.push((p.to_path_buf(), why));
        true
    });
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert_eq!(asked[0], (stage.clone(), Unused::Unfinished));
    assert_eq!(asked[1].0, tool);
    assert_eq!(removed.len(), 2, "each once: {removed:?}");
    assert_eq!(
        removed[0],
        (
            stage.clone(),
            Unused::Stayed {
                stayed: 1,
                freed: 0
            }
        )
    );
    assert!(
        removed[1].0 == tool && matches!(removed[1].1, Unused::Tooling { .. }),
        "{removed:?}"
    );
    assert!(!tool.exists() && !root.join(".provision-reclaim-pwsh-7.6.7-1").exists());
    assert_eq!(std::fs::read(&held).unwrap(), b"x");
}

/// A TOOL THAT CANNOT BE STAGED IS NEITHER PLANNED NOR ASKED FOR: a tool unused a month, in a
/// tooling cache this user may add no name to (0o555), or (macOS) flagged `uchg` itself, which
/// refuses its rename, is not listed by the plan, its intent is never asked, and it stays
/// whole. NEGATIVE CONTROL: the root's write bit back and the flag off, the plan lists it and
/// a pass takes it.
#[cfg(unix)]
#[test]
fn a_tool_that_cannot_be_staged_is_neither_planned_nor_asked_for() {
    use std::os::unix::fs::PermissionsExt as _;
    /// Gives the root and the tool their owner's write bit back, and (macOS) the tool its
    /// flags off, when dropped, so a failed test leaves its scratch removable.
    struct Let(PathBuf, PathBuf);
    impl Drop for Let {
        fn drop(&mut self) {
            for dir in [&self.0, &self.1] {
                let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755));
            }
            #[cfg(target_os = "macos")]
            let _ = crate::platform::set_file_flags(&self.1, 0);
        }
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().join("tooling.noindex");
    let now = SystemTime::now();
    let tool = root.join("pwsh-7.6.7");
    file(&tool.join("bin/tool"));
    age_tree(&tool, now - RECLAIM_IDLE - DAY);
    let fixed = Let(root.clone(), tool.clone());
    let unstaged = |why: &str| {
        assert!(plan(&root, now).is_empty(), "{why}");
        let mut asked = Vec::new();
        let removed = reclaim(&root, now, &mut |p, why| {
            asked.push((p.to_path_buf(), why));
            true
        });
        assert!(
            asked.is_empty() && removed.is_empty(),
            "{why}: {asked:?} {removed:?}"
        );
        assert!(tool.join("bin/tool").is_file(), "{why}");
    };
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();
    unstaged("a root that takes no name");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    #[cfg(target_os = "macos")]
    {
        crate::platform::set_file_flags(&tool, libc::UF_IMMUTABLE).unwrap();
        assert!(std::fs::rename(&tool, root.join("moved")).is_err());
        unstaged("a flagged tool");
        crate::platform::set_file_flags(&tool, 0).unwrap();
    }

    let planned: Vec<PathBuf> = plan(&root, now).into_iter().map(|p| p.path).collect();
    assert_eq!(planned, std::slice::from_ref(&tool));
    let removed = reclaim(&root, now, &mut |_, _| true);
    assert!(
        removed.len() == 1 && removed[0].0 == tool && !tool.exists(),
        "{removed:?}"
    );
    drop(fixed);
}

#[test]
fn the_root_is_the_resolvers_tooling_cache() {
    let home = Path::new("/Users//someone");
    assert_eq!(
        root(home),
        Path::new("/Users//someone/Library/Caches/aterm/tooling.noindex")
    );
    assert!(plan(&home.join("nowhere-at-all"), SystemTime::now()).is_empty());
}
