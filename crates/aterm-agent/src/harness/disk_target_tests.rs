// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for build-directory reclaim: profile discovery, idle clocks, lock discipline, and
//! removing only idle `incremental/` caches in scratch directories.

use super::*;
use crate::harness::disk::DEFAULT_TARGET_STALE_DAYS as STALE;
use crate::harness::disk::tests::{
    Tmp, lay_profile, lay_target, now_after_write, synthetic_target,
};

const DAY: i64 = 86_400;

/// Timestamp an hour before the stale window closes at `now`.
fn inside_window(now: i64) -> i64 {
    now - (STALE * DAY - 3_600)
}

/// Default reclaim judge at `now`.
fn judge(now: i64) -> Judge {
    Judge {
        now,
        threshold_days: STALE,
        device: None,
        volume: None,
    }
}

/// Assess one build directory with the shipped bounds.
fn assessed(path: &Path, now: i64) -> TargetDir {
    assess(path, now, STALE, WalkBudget::SHIPPED).expect("a directory")
}

#[test]
fn registered_custody_cannot_fall_back_to_an_ordinary_cargo_target() {
    for contents in [
        br#"{"schema":3}"#.as_slice(),
        b"unknown future policy".as_slice(),
    ] {
        let tmp = Tmp::new("managed-target");
        let target = synthetic_target(&tmp, "target");
        let now = now_after_write(STALE + 1);
        assert!(assessed(&target, now).fences().is_ok());
        std::fs::write(
            target.join(atpkg::build_cache::registered::MARKER),
            contents,
        )
        .unwrap();
        let observed = assessed(&target, now);
        assert!(observed.holds_source.is_none());
        assert!(observed.fences().is_err());
        reclaim(&target, &judge(now)).expect_err("registered authority is required");
        assert!(target.join("debug/incremental").exists());
    }
    let tmp = Tmp::new("unreadable-managed-marker");
    let target = synthetic_target(&tmp, "target");
    std::fs::create_dir(target.join(atpkg::build_cache::registered::MARKER)).unwrap();
    reclaim(&target, &judge(now_after_write(STALE + 1)))
        .expect_err("a nonregular policy is uncertainty, not ordinary authority");
    assert!(target.join("debug/incremental").exists());
}

#[cfg(unix)]
#[test]
fn supported_retention_revokes_ordinary_paths_and_keeps_unretained_siblings_eligible() {
    for method in 0..4 {
        let tmp = Tmp::new("supported-cache-retention");
        let home = tmp.path().canonicalize().unwrap();
        let target = synthetic_target(&tmp, "target").canonicalize().unwrap();
        aterm_uds::private_file::ensure_private_dir(&target).unwrap();
        let sibling = synthetic_target(&tmp, "sibling").canonicalize().unwrap();
        let now = now_after_write(STALE + 1);
        let debug = target.join("debug");
        let seen = assessed(&target, now).profiles[0].last_used.unwrap();
        let mut called = false;
        let mut retain = |stage, _: &Path| {
            if stage == Stage::Staging && !called {
                called = true;
                atpkg::build_cache::registered::retain(&home, &target).unwrap();
            }
        };
        let result = match method {
            0 => reclaim_with(&target, &judge(now), &mut retain),
            1 => reclaim_lru_with(&target, &debug, &judge(now), seen, &mut retain),
            2 => reclaim_whole_with(&target, &judge(now), seen, DAY, &mut || Ok(()), &mut retain),
            _ => reclaim_profiles_with(
                &target,
                &judge(now),
                seen,
                DAY,
                &mut || Ok(()),
                &mut || Ok(()),
                &mut retain,
            ),
        };
        assert!(called && result.is_err());
        assert!(target.join("debug/incremental").exists());
        assert!(assessed(&target, now).fences().is_err());
        let unretained = reclaim(&sibling, &judge(now)).unwrap();
        assert!(!unretained.units.is_empty());
    }
}

#[test]
fn a_late_registered_marker_revokes_each_ordinary_removal_path() {
    #[cfg(unix)]
    let whens = [Stage::Locked, Stage::Staging, Stage::Staged];
    #[cfg(windows)]
    let whens = [Stage::Locked, Stage::Staging];
    for method in 0..4 {
        for when in whens {
            let tmp = Tmp::new("late-managed-target");
            let target = synthetic_target(&tmp, "target");
            let now = now_after_write(STALE + 1);
            let debug = target.join("debug");
            let seen = assessed(&target, now).profiles[0].last_used.unwrap();
            let mut called = false;
            let mut marker = |stage, _: &Path| {
                if stage == when {
                    called = true;
                    std::fs::write(
                        target.join(atpkg::build_cache::registered::MARKER),
                        br#"{"schema":3}"#,
                    )
                    .unwrap();
                }
            };
            let result = match method {
                0 => reclaim_with(&target, &judge(now), &mut marker),
                1 => reclaim_lru_with(&target, &debug, &judge(now), seen, &mut marker),
                2 => {
                    reclaim_whole_with(&target, &judge(now), seen, DAY, &mut || Ok(()), &mut marker)
                }
                _ => reclaim_profiles_with(
                    &target,
                    &judge(now),
                    seen,
                    DAY,
                    &mut || Ok(()),
                    &mut || Ok(()),
                    &mut marker,
                ),
            };
            assert!(called, "method {method}, stage {when:?}");
            assert!(result.is_err(), "method {method}, stage {when:?}");
            assert!(debug.join(INCREMENTAL).exists());
            assert!(target.join(atpkg::build_cache::registered::MARKER).exists());
        }
    }
}

#[test]
fn custody_revoked_before_removal_stops_later_profiles_until_a_fresh_pass() {
    for custody_marker in atpkg::build_cache::registered::CUSTODY_MARKERS {
        let tmp = Tmp::new("predelete-managed-target");
        let target = synthetic_target(&tmp, "target");
        lay_profile(&target.join("release"));
        let before = snapshot(&target, &|_| false);
        let now = now_after_write(STALE + 1);
        let marker = target.join(custody_marker);
        let mut staging = 0;
        let mut released = 0;
        let mut removing = 0;
        let error = reclaim_with(&target, &judge(now), &mut |stage, _| match stage {
            Stage::Staging => {
                staging += 1;
                if staging == 1 {
                    std::fs::write(&marker, b"held").unwrap();
                } else if marker.exists() {
                    std::fs::remove_file(&marker).unwrap();
                }
            }
            Stage::Released => {
                released += 1;
                // Clearing custody between profiles cannot revive this pass's permission.
                if marker.exists() {
                    std::fs::remove_file(&marker).unwrap();
                }
            }
            Stage::Removing => removing += 1,
            Stage::Locked | Stage::Staged | Stage::Deleting => {}
        })
        .expect_err("a pre-removal custody refusal stops the whole pass");
        assert!(error.to_string().contains(MANAGED_CACHE), "{error}");
        assert_eq!(staging, 1, "the next profile never reaches deletion");
        assert_eq!(released, 1, "the refused profile releases its locks");
        assert_eq!(removing, 0, "no removal boundary was entered");
        assert!(!marker.exists());
        assert_eq!(
            snapshot(&target, &|_| false),
            before,
            "no payload was removed"
        );

        let done = reclaim(&target, &judge(now)).expect("a fresh pass may reclaim both profiles");
        assert_eq!(done.units.len(), 2);
        assert!(done.trouble.is_empty(), "{done:?}");
        for profile in ["debug", "release"] {
            assert!(done.units.contains(&target.join(profile).join(INCREMENTAL)));
            assert!(!target.join(profile).join(INCREMENTAL).exists());
        }
    }
}

#[cfg(unix)]
#[test]
fn a_renamed_pinned_target_keeps_its_registered_custody() {
    let tmp = Tmp::new("renamed-managed-target");
    let target = synthetic_target(&tmp, "target");
    let moved = tmp.path().join("moved");
    let now = now_after_write(STALE + 1);
    let mut called = false;
    reclaim_with(&target, &judge(now), &mut |stage, _| {
        if stage == Stage::Staging {
            called = true;
            std::fs::rename(&target, &moved).unwrap();
            std::fs::create_dir(&target).unwrap();
            std::fs::write(moved.join(atpkg::build_cache::registered::MARKER), b"held").unwrap();
        }
    })
    .expect_err("the pinned original retains its policy after a rename");
    assert!(called);
    assert!(moved.join("debug/incremental").exists());
}

fn ordinary_method(
    method: usize,
    target: &Path,
    now: i64,
    seen: i64,
    hook: &mut dyn FnMut(Stage, &Path),
) -> io::Result<Removed> {
    match method {
        0 => reclaim_with(target, &judge(now), hook),
        1 => reclaim_lru_with(target, &target.join("debug"), &judge(now), seen, hook),
        2 => reclaim_whole_with(target, &judge(now), seen, DAY, &mut || Ok(()), hook),
        _ => reclaim_profiles_with(
            target,
            &judge(now),
            seen,
            DAY,
            &mut || Ok(()),
            &mut || Ok(()),
            hook,
        ),
    }
}

#[test]
fn a_managed_ancestor_keeps_an_explicitly_named_nested_target() {
    for method in 0..4 {
        let tmp = Tmp::new("nested-managed-target");
        let target = synthetic_target(&tmp, "generation/nested-target");
        let now = now_after_write(STALE + 1);
        let observed = assessed(&target, now);
        let seen = observed.profiles[0].last_used.unwrap();
        assert!(observed.fences().is_ok());
        let before = snapshot(&target, &|_| false);
        std::fs::write(
            target
                .parent()
                .unwrap()
                .join(atpkg::build_cache::registered::MARKER),
            br#"{"schema":3}"#,
        )
        .unwrap();
        assert!(assessed(&target, now).fences().is_err());
        ordinary_method(method, &target, now, seen, &mut |_, _| {})
            .expect_err("an explicit nested target cannot escape its ancestor's custody");
        assert_eq!(snapshot(&target, &|_| false), before, "method {method}");
    }
}

/// A managed build directory inside what an ordinary removal takes is never removed, and it
/// ends where it was. A removal that takes more than `incremental/` (the whole one, tier 3)
/// finds its marker by the scan before the locks, stages nothing, and its delete in place stops
/// at it; one that takes `incremental/` unread stops at it in its stage, and what that delete
/// left goes back.
///
/// Negative control: a pass that staged it and left what its delete kept in the stage would
/// leave `managed` there, where the last snapshot finds nothing at its path and a stage stands.
#[test]
fn an_ordinary_target_cannot_remove_a_managed_descendant() {
    for method in 0..4 {
        let tmp = Tmp::new("managed-descendant");
        let target = synthetic_target(&tmp, "target");
        let nested = lay_target(&target.join("debug/incremental/managed"));
        std::fs::write(nested.join(atpkg::build_cache::registered::MARKER), b"held").unwrap();
        let before = snapshot(&nested, &|_| false);
        let now = now_after_write(STALE + 1);
        let observed = assessed(&target, now);
        assert!(observed.fences().is_ok());
        let seen = observed.profiles[0].last_used.unwrap();
        let why = match ordinary_method(method, &target, now, seen, &mut |_, _| {}) {
            Ok(done) => done.trouble.join("; "),
            Err(error) => error.to_string(),
        };
        assert!(why.contains(MANAGED_CACHE), "method {method}: {why}");
        assert_eq!(snapshot(&nested, &|_| false), before, "method {method}");
        #[cfg(unix)]
        assert!(stages_in(&target).is_empty(), "method {method}: staged");
    }
}

#[test]
fn a_custody_scan_never_follows_a_nested_directory_link() {
    let tmp = Tmp::new("custody-scan-link");
    let target = synthetic_target(&tmp, "target");
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(
        outside.join(atpkg::build_cache::registered::MARKER),
        b"held",
    )
    .unwrap();
    std::fs::write(outside.join("precious"), b"not a cache descendant").unwrap();
    let before = snapshot(&outside, &|_| false);
    let link = target.join("linked");
    link_dir(&outside, &link);
    unmanaged_custody(&target, WalkBudget::SHIPPED)
        .expect("a link's protected target is not traversed");
    assert!(unmanaged_custody(&link, WalkBudget::SHIPPED).is_err());
    assert_eq!(snapshot(&outside, &|_| false), before);
    let nested = target.join("real-child");
    std::fs::create_dir(&nested).unwrap();
    std::fs::write(nested.join(atpkg::build_cache::registered::MARKER), b"held").unwrap();
    assert!(
        unmanaged_custody(&target, WalkBudget::SHIPPED).is_err(),
        "CONTROL: a reachable real directory's custody is inspected"
    );
}

#[test]
fn a_custody_scan_refuses_incomplete_bounds_and_checks_a_files_ancestors() {
    let tmp = Tmp::new("custody-scan-bounds");
    let target = synthetic_target(&tmp, "target");
    for budget in [
        WalkBudget {
            entries: 0,
            ..WalkBudget::SHIPPED
        },
        WalkBudget {
            depth: 0,
            ..WalkBudget::SHIPPED
        },
        WalkBudget {
            wall: Duration::ZERO,
            ..WalkBudget::SHIPPED
        },
    ] {
        assert!(unmanaged_custody(&target, budget).is_err());
    }
    let file = target.join("CACHEDIR.TAG");
    unmanaged_custody(&file, WalkBudget::SHIPPED).expect("unmarked ordinary file");
    std::fs::write(
        target.join(atpkg::build_cache::registered::package::COORDINATOR),
        b"held",
    )
    .unwrap();
    assert!(unmanaged_custody(&file, WalkBudget::SHIPPED).is_err());
}

fn mid_walk_custody(ancestor: bool, rename: bool, custody_marker: &str) {
    for method in 0..4 {
        let tmp = Tmp::new("mid-walk-managed-target");
        let target = synthetic_target(&tmp, "generation/target");
        let moved = target.with_file_name("moved");
        lay_profile(&target.join("release"));
        let payloads = [
            PathBuf::from("debug/incremental/lay-1x2y3z/s-abc-def/query-cache.bin"),
            PathBuf::from("debug/incremental/remaining/session/query-cache.bin"),
            PathBuf::from("release/incremental/lay-1x2y3z/s-abc-def/query-cache.bin"),
            PathBuf::from("release/incremental/remaining/session/query-cache.bin"),
        ];
        for rel in &payloads {
            use std::io::Write as _;
            let payload = target.join(rel);
            std::fs::create_dir_all(payload.parent().unwrap()).unwrap();
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&payload)
                .unwrap();
            file.write_all(&[17u8; 8192]).unwrap();
            // These scratch payloads may be recreated after a power loss.
            aterm_durable::ordered(&file).unwrap();
            assert!(
                blocks(&payload) > 0,
                "method {method}: {rel:?} must release allocated bytes when removed first"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                assert_eq!(
                    file.metadata().unwrap().nlink(),
                    1,
                    "method {method}: {rel:?} must have no retained hard link"
                );
            }
        }
        // Each file is known by its identity and length, not its path: a staged pass moves the
        // payloads before it deletes them.
        let payloads: Vec<_> = payloads
            .iter()
            .map(|rel| identity(&target.join(rel)))
            .collect();
        let allocated: Vec<_> = snapshot(&target, &|_| false)
            .into_iter()
            .filter(|(_, kind, _)| kind == "file")
            .map(|(rel, _, _)| {
                let file = target.join(&rel);
                (identity(&file), blocks(&file))
            })
            .collect();
        let now = now_after_write(STALE + 1);
        let seen = assessed(&target, now)
            .profiles
            .iter()
            .filter_map(|profile| profile.last_used)
            .max()
            .unwrap();
        let mut injected = false;
        let mut locked_after = 0;
        let mut remaining = Vec::new();
        let mut marker: Option<PathBuf> = None;
        let done = ordinary_method(method, &target, now, seen, &mut |stage, _| {
            if injected && stage == Stage::Locked {
                locked_after += 1;
            }
            if stage != Stage::Removing || injected {
                return;
            }
            let live = live_files(&target);
            let alive: Vec<_> = payloads.iter().filter(|p| live.contains(p)).collect();
            if alive.len() != payloads.len() - 1 {
                return;
            }
            injected = true;
            remaining = alive.into_iter().copied().collect();
            #[cfg(unix)]
            if rename {
                std::fs::rename(&target, &moved).unwrap();
                std::fs::create_dir(&target).unwrap();
            }
            let active = if rename { &moved } else { &target };
            let owner = if ancestor {
                active.parent().unwrap()
            } else {
                active
            };
            let policy = owner.join(custody_marker);
            std::fs::write(&policy, br#"{"schema":3}"#).unwrap();
            marker = Some(policy);
        })
        .expect("the files deleted before revocation are reported");
        assert!(
            injected,
            "method {method}: mutation must already have started"
        );
        let active = if rename { &moved } else { &target };
        let live = live_files(active);
        assert!(
            remaining.iter().all(|p| live.contains(p)),
            "method {method}: remaining payloads"
        );
        let freed: u64 = allocated
            .iter()
            .filter(|(p, _)| !live.contains(p))
            .map(|(_, bytes)| bytes)
            .sum();
        assert!(freed > 0, "method {method}: actual partial deletion");
        assert_eq!(
            done.bytes, freed,
            "method {method}: only actual released bytes"
        );
        assert!(
            done.trouble.iter().any(|why| why.contains(MANAGED_CACHE)),
            "{done:?}"
        );
        assert_eq!(
            locked_after, 0,
            "method {method}: no profile is locked once custody is revoked"
        );
        assert!(marker.unwrap().exists());
    }
}

/// The identity and length of the file at `path`: what it is wherever a pass moves it.
fn identity(path: &Path) -> (aterm_sysprobe::volumes::FileId, u64) {
    let id = aterm_sysprobe::volumes::file_id(path, false).expect("an identity");
    (id, std::fs::symlink_metadata(path).expect("stat").len())
}

/// The [`identity`] of every file under `root`, a stage's included: read by metadata alone, so
/// no file a remover holds open is read.
fn live_files(root: &Path) -> Vec<(aterm_sysprobe::volumes::FileId, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read").flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file()
                && let Some(id) = aterm_sysprobe::volumes::file_id(&path, false)
            {
                out.push((id, meta.len()));
            }
        }
    }
    out
}

#[test]
fn mid_walk_registered_custody_stops_each_ordinary_removal_path() {
    mid_walk_custody(false, false, atpkg::build_cache::registered::MARKER);
    mid_walk_custody(true, false, atpkg::build_cache::registered::MARKER);
}

#[test]
fn mid_walk_package_coordination_stops_each_ordinary_removal_path() {
    mid_walk_custody(
        false,
        false,
        atpkg::build_cache::registered::package::COORDINATOR,
    );
    mid_walk_custody(
        true,
        false,
        atpkg::build_cache::registered::package::COORDINATOR,
    );
}

#[cfg(unix)]
#[test]
fn mid_walk_custody_follows_a_renamed_root_and_its_ancestors() {
    for marker in atpkg::build_cache::registered::CUSTODY_MARKERS {
        mid_walk_custody(false, true, marker);
        mid_walk_custody(true, true, marker);
    }
}

/// Join `/`-separated `rel` under `base` one component at a time (safe under verbatim `\\?\`).
fn at(base: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(base.to_path_buf(), |p, c| p.join(c))
}

/// Allocated byte count of the file at `p`.
#[cfg(unix)]
fn blocks(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::symlink_metadata(p).expect("stat").blocks() * 512
}

/// Allocated byte count (`FILE_STANDARD_INFO`) of the file at `p`.
#[cfg(windows)]
fn blocks(p: &Path) -> u64 {
    let f = std::fs::File::open(p).expect("open");
    aterm_dirfd::fstat(&f).expect("stat").allocation
}

/// Set the mtime of `path` to `at` (unix seconds).
fn set_mtime(path: &Path, at: i64) {
    let when = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(u64::try_from(at).expect("after the epoch"));
    #[cfg(unix)]
    let file = std::fs::File::open(path);
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        std::fs::OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
    };
    file.and_then(|f| f.set_modified(when)).expect("set mtime");
}

/// Strip any verbatim `\\?\` prefix from `p` for `cmd` / `mklink`.
#[cfg(windows)]
fn plain(p: &Path) -> String {
    let text = p.to_str().expect("test paths are UTF-8");
    text.strip_prefix(r"\\?\").unwrap_or(text).to_owned()
}

/// Create a directory symlink (unix) or junction (Windows) at `link` pointing to `target`.
fn link_dir(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).expect("link");
    #[cfg(windows)]
    {
        let out = aterm_uds::spawnfd::command("cmd")
            .args(["/C", "mklink", "/J", &plain(link), &plain(target)])
            .output()
            .expect("cmd runs");
        assert!(
            out.status.success(),
            "mklink /J {link:?} {target:?}: {out:?}"
        );
    }
}

/// Undeletable 8 KiB file under `dir` (`0555` parent on unix; held without delete sharing on Windows).
struct Stuck {
    #[cfg(unix)]
    dir: PathBuf,
    #[cfg(windows)]
    _open: std::fs::File,
}

impl Stuck {
    fn at(dir: &Path) -> Stuck {
        std::fs::create_dir_all(dir).expect("stuck dir");
        let file = dir.join("file");
        std::fs::write(&file, vec![1u8; 8192]).expect("stuck file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).expect("chmod");
            Stuck {
                dir: dir.to_path_buf(),
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            const FILE_SHARE_READ: u32 = 1;
            let open = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(&file)
                .expect("held");
            Stuck { _open: open }
        }
    }
}

impl Drop for Stuck {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o755));
        }
    }
}

/// Acquire `lock` exclusively via [`HeldLock::try_take`].
fn take(lock: &Path) -> HeldLock {
    let f = std::fs::File::open(lock).expect("the lock exists");
    HeldLock::try_take(f)
        .expect("the lock can be taken")
        .expect("the test takes the lock")
}

/// Probe whether `lock` is currently held, releasing via `LOCK_UN`.
fn held(lock: &Path) -> bool {
    let f = std::fs::File::open(lock).expect("lock");
    !matches!(HeldLock::try_take(f), Ok(Some(_)))
}

/// Forked child parked before exit, holding inherited file descriptors until [`Parked::exec`].
#[cfg(unix)]
struct Parked {
    pid: libc::pid_t,
    go: Option<std::io::PipeWriter>,
    reaped: std::cell::Cell<Option<std::process::ExitStatus>>,
}

#[cfg(unix)]
impl Parked {
    fn fork() -> Parked {
        use std::os::fd::AsRawFd as _;
        let (go_r, go_w) = aterm_uds::pipe().expect("pipe");
        let (go_fd, go_w_fd) = (go_r.as_raw_fd(), go_w.as_raw_fd());
        // The go pipe's read end is the child's: unmarked for this fork alone.
        let handing = aterm_uds::spawn_lock::handing(&[go_fd]);
        // SAFETY: the child calls only async-signal-safe functions (`close`, `read`, `_exit`,
        // and errno) on descriptors opened before `fork`, then exits without unwinding.
        let pid = unsafe { libc::fork() };
        if pid != 0 {
            // The child's copy of the hold is never released: it `_exit`s.
            drop(handing);
        }
        assert!(pid >= 0, "fork: {}", std::io::Error::last_os_error());
        if pid == 0 {
            let interrupted =
                || std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted;
            unsafe {
                libc::close(go_w_fd);
            }
            let mut b = 0u8;
            loop {
                let n = unsafe { libc::read(go_fd, (&raw mut b).cast(), 1) };
                if n >= 0 {
                    break;
                }
                if !interrupted() {
                    unsafe { libc::_exit(1) };
                }
            }
            unsafe { libc::_exit(0) };
        }
        drop(go_r);
        Parked {
            pid,
            go: Some(go_w),
            reaped: std::cell::Cell::new(None),
        }
    }

    /// Returns `true` while the parked child has not yet exited.
    fn pending(&self) -> bool {
        use std::os::unix::process::ExitStatusExt as _;
        if self.reaped.get().is_some() {
            return false;
        }
        let mut status: libc::c_int = 0;
        // SAFETY: `WNOHANG` polls the child owned by `self.pid` without blocking.
        let r = unsafe { libc::waitpid(self.pid, &raw mut status, libc::WNOHANG) };
        if r == self.pid {
            self.reaped
                .set(Some(std::process::ExitStatus::from_raw(status)));
            return false;
        }
        r == 0
    }

    /// Release the child and return its exit status.
    fn exec(mut self) -> std::process::ExitStatus {
        self.reap().expect("the child ran")
    }

    fn reap(&mut self) -> std::io::Result<std::process::ExitStatus> {
        use std::io::Write as _;
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(mut go) = self.go.take() {
            let _ = go.write_all(b"g");
        }
        if let Some(status) = self.reaped.get() {
            return Ok(status);
        }
        let mut status: libc::c_int = 0;
        loop {
            // SAFETY: `self.pid` is the live child forked in `Parked::fork`.
            let r = unsafe { libc::waitpid(self.pid, &raw mut status, 0) };
            if r >= 0 {
                let status = std::process::ExitStatus::from_raw(status);
                self.reaped.set(Some(status));
                return Ok(status);
            }
            let err = std::io::Error::last_os_error();
            if err.kind() != std::io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    }
}

#[cfg(unix)]
impl Drop for Parked {
    fn drop(&mut self) {
        if self.go.is_some() || self.reaped.get().is_none() {
            let _ = self.reap();
        }
    }
}

/// Build directory populated with uplifted binaries, `.d`, `examples/`, `criterion/`, `doc/`, and `.dSYM`.
fn furnished(tmp: &Tmp, name: &str) -> PathBuf {
    let t = synthetic_target(tmp, name);
    let debug = t.join("debug");
    let twin = debug.join("deps").join("lay-2bcb77ac5af7c69c");
    std::fs::write(&twin, b"#!/bin/sh\necho lay-ok\n").expect("bin");
    std::fs::hard_link(&twin, debug.join("lay")).expect("uplift");
    std::fs::write(debug.join("lay.d"), b"lay: src/main.rs\n").expect(".d");
    std::fs::create_dir_all(debug.join("examples")).expect("examples");
    std::fs::write(debug.join("examples").join("ex"), b"example").expect("ex");
    #[cfg(unix)]
    std::os::unix::fs::symlink("deps/lay-2bcb77ac5af7c69c", debug.join("lay.dSYM"))
        .expect("dsym link");
    #[cfg(windows)]
    link_dir(&debug.join("deps"), &debug.join("lay.dSYM"));
    let baseline = t.join("criterion").join("b").join("before");
    std::fs::create_dir_all(&baseline).expect("criterion");
    std::fs::write(baseline.join("estimates.json"), b"{\"mean\":1}").expect("baseline");
    std::fs::create_dir_all(t.join("doc").join("lay")).expect("doc");
    std::fs::write(t.join("doc").join("lay").join("index.html"), b"<html>").expect("doc page");
    t
}

/// TIER 3 SAYS IT IS STILL GOING WHILE IT JUDGES: before it removes anything it compares a copied
/// artifact with its twin byte by byte, and matches each entry of the profile against every name in
/// `deps/`, either of which can run for minutes on a swapping machine.
#[test]
fn tier_3_ticks_while_it_compares_artifacts_and_judges_entries() {
    const KEPT: usize = 400;
    const BLOCKS: usize = 512;
    let tmp = Tmp::new("tier-3-ticks");
    let t = synthetic_target(&tmp, "target");
    let debug = t.join("debug");
    let bytes = vec![7u8; BLOCKS * 8192];
    std::fs::write(debug.join("deps").join("lay-2bcb77ac5af7c69c"), &bytes).expect("twin");
    std::fs::write(debug.join("lay"), &bytes).expect("a copy, so its bytes are compared");
    for i in 0..KEPT {
        std::fs::write(debug.join(format!("kept-{i}")), b"x").expect("kept");
    }
    let now = now_after_write(STALE + 1);
    let seen = assessed(&t, now).profiles[0].last_used.unwrap();
    let before = aterm_uds::progress::ticks_here();
    reclaim_profiles(&t, &judge(now), seen, DAY, &mut || Ok(()), &mut || Ok(())).expect("emptied");
    let ticks = aterm_uds::progress::ticks_here() - before;
    assert!(
        std::fs::symlink_metadata(debug.join("lay")).is_err(),
        "the copy was its twin's bytes, so it went"
    );
    assert!(debug.join("kept-0").exists(), "no output of cargo's stays");
    assert!(ticks >= KEPT + BLOCKS, "{ticks} ticks");
}

/// Snapshot every entry under `root` except paths matching `skip`.
fn snapshot(root: &Path, skip: &dyn Fn(&Path) -> bool) -> Vec<(PathBuf, String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).expect("read").flatten() {
            let p = e.path();
            let rel = p.strip_prefix(root).expect("under").to_path_buf();
            if skip(&rel) {
                continue;
            }
            let meta = std::fs::symlink_metadata(&p).expect("stat");
            if meta.is_symlink() {
                let text = std::fs::read_link(&p).expect("link");
                out.push((
                    rel,
                    "link".to_owned(),
                    text.as_os_str().as_encoded_bytes().to_vec(),
                ));
            } else if meta.is_dir() {
                out.push((rel, "dir".to_owned(), Vec::new()));
                stack.push(p);
            } else if cfg!(windows) && CARGO_LOCKS.iter().any(|l| e.file_name() == *l) {
                out.push((rel, "lock".to_owned(), meta.len().to_le_bytes().to_vec()));
            } else {
                out.push((rel, "file".to_owned(), std::fs::read(&p).expect("read")));
            }
        }
    }
    out.sort();
    out
}

/// Under `profile/incremental`?
fn in_incremental(rel: &Path) -> bool {
    rel.components()
        .any(|c| c.as_os_str() == std::ffi::OsStr::new(INCREMENTAL))
}

/// Targo's supported build-unit layout: the same profile locks, with
/// build/<package>/<unit>/{fingerprint,out,run} instead of deps/.fingerprint.
fn unit_target(tmp: &Tmp) -> PathBuf {
    let t = synthetic_target(tmp, "target");
    let p = t.join("debug");
    std::fs::remove_dir_all(p.join("deps")).expect("no legacy deps");
    std::fs::remove_dir_all(p.join(".fingerprint")).expect("no legacy fingerprints");
    let unit = at(&p, "build/lay/2bcb77ac5af7c69c");
    for d in ["out", "fingerprint", "run"] {
        std::fs::create_dir_all(unit.join(d)).expect("unit directory");
    }
    std::fs::write(at(&unit, "out/liblay.rlib"), vec![7u8; 4096]).expect("unit artifact");
    for d in ["fingerprint", "run"] {
        std::fs::write(unit.join(d).join("invoked.timestamp"), b"built").expect("invoked");
    }
    std::fs::write(at(&unit, "fingerprint/lib-lay"), b"0123456789abcdef").expect("completed hash");
    std::fs::write(at(&unit, "run/stdout"), b"completed output").expect("run output");
    std::fs::write(at(&p, "build/sibling-file"), b"not a package").expect("sibling file");
    std::fs::write(at(&p, "build/lay/sibling-file"), b"not a unit").expect("sibling file");
    t
}

#[test]
fn unit_layout_reclaim_keeps_compiled_outputs_and_removes_only_incremental() {
    let tmp = Tmp::new("unit-layout");
    let t = unit_target(&tmp);
    let now = now_after_write(STALE + 1);
    let before = snapshot(&t, &in_incremental);
    let td = assessed(&t, now);
    assert_eq!(td.profiles.len(), 1, "{td:?}");
    assert_eq!(td.profiles[0].path, t.join("debug"));
    assert_eq!(td.candidates(now, STALE).len(), 1);
    let done = reclaim(&t, &judge(now)).expect("idle unit layout");
    assert_eq!(done.units, vec![at(&t, "debug/incremental")]);
    assert_eq!(snapshot(&t, &in_incremental), before);
}

#[test]
fn unit_layout_requires_real_directories_and_any_held_cargo_lock_refuses() {
    let now = now_after_write(STALE + 1);
    for lock in CARGO_LOCKS {
        let tmp = Tmp::new("unit-lock");
        let t = unit_target(&tmp);
        let before = snapshot(&t, &|_| false);
        let hold = take(&t.join("debug").join(lock));
        let td = assessed(&t, now);
        assert_eq!(td.profiles.len(), 1);
        assert!(td.profiles[0].in_use.is_some());
        assert!(td.candidates(now, STALE).is_empty());
        reclaim(&t, &judge(now)).expect_err("held unit-layout build");
        assert_eq!(snapshot(&t, &|_| false), before);
        drop(hold);
    }
    for missing in ["locks", "build", "incremental"] {
        let tmp = Tmp::new("unit-incomplete");
        let t = unit_target(&tmp);
        let p = t.join("debug");
        if missing == "locks" {
            for lock in CARGO_LOCKS {
                std::fs::remove_file(p.join(lock)).expect("remove lock");
            }
        } else {
            std::fs::remove_dir_all(p.join(missing)).expect("remove layout directory");
            std::fs::write(p.join(missing), b"not a directory").expect("plain file");
        }
        let td = assessed(&t, now);
        assert!(td.profiles.is_empty(), "{missing}: {td:?}");
        reclaim(&t, &judge(now)).expect_err("not a supported locked profile");
    }
}

#[test]
fn unit_clocks_keep_recent_compilations_even_when_profile_parents_stay_old() {
    let now = now_after_write(STALE + 1);
    for channel in [
        "out",
        "fingerprint",
        "run",
        "fingerprint/invoked.timestamp",
        "run/invoked.timestamp",
        "fingerprint/lib-lay",
        "run/stdout",
    ] {
        let tmp = Tmp::new("unit-clock");
        let t = unit_target(&tmp);
        let debug = t.join("debug");
        let unit = at(&debug, "build/lay/2bcb77ac5af7c69c");
        let old = now - (STALE + 1) * DAY;
        last_used(&debug, old);
        set_mtime(&at(&debug, "build/lay"), old);
        set_mtime(&unit, old);
        let recent = inside_window(now);
        set_mtime(&at(&unit, channel), recent);
        let td = assessed(&t, now);
        assert_eq!(td.profiles.len(), 1);
        assert_eq!(td.profiles[0].last_used, Some(recent), "{channel}");
        assert!(td.candidates(now, STALE).is_empty(), "{channel}");
        reclaim(&t, &judge(now)).expect_err("recent unit activity");
        assert!(debug.join(INCREMENTAL).exists());
    }
}

#[test]
fn unit_stamp_changes_are_rechecked_under_locks_for_idle_and_pressure_reclaim() {
    for pressure in [false, true] {
        for channel in [
            "fingerprint/invoked.timestamp",
            "fingerprint/lib-lay",
            "run/stdout",
        ] {
            let tmp = Tmp::new("unit-clock-race");
            let t = unit_target(&tmp);
            let debug = t.join("debug");
            let stamp = at(&at(&debug, "build/lay/2bcb77ac5af7c69c"), channel);
            let now = now_after_write(STALE + 1);
            let seen = assessed(&t, now).profiles[0]
                .last_used
                .expect("known clock");
            let mut update = |stage, _: &Path| {
                if stage == Stage::Locked {
                    set_mtime(&stamp, if pressure { seen + 60 } else { now });
                }
            };
            let result = if pressure {
                reclaim_lru_with(&t, &debug, &judge(now), seen, &mut update)
            } else {
                reclaim_with(&t, &judge(now), &mut update)
            };
            result.expect_err("unit rebuilt after the survey");
            assert!(debug.join(INCREMENTAL).exists());
        }
    }
}

#[test]
fn unit_clocks_refuse_truncation_symlinks_and_nonfile_invocation_stamps() {
    let tmp = Tmp::new("unit-clock-bounds");
    let t = unit_target(&tmp);
    let debug = t.join("debug");
    assert!(fd::clock(&debug, WalkBudget::SHIPPED).is_some());
    assert_eq!(
        fd::clock(
            &debug,
            WalkBudget {
                entries: 0,
                ..WalkBudget::SHIPPED
            }
        ),
        None
    );
    for name in ["fingerprint", "out", "run"] {
        let tmp = Tmp::new("unit-clock-shape");
        let t = unit_target(&tmp);
        let debug = t.join("debug");
        let fixed = at(&debug, "build/lay/2bcb77ac5af7c69c").join(name);
        std::fs::remove_dir_all(&fixed).expect("replace fixed clock directory");
        std::fs::write(&fixed, b"unsupported recently written shape").expect("regular file");
        let now = now_after_write(STALE + 1);
        let td = assessed(&t, now);
        assert_eq!(td.profiles.len(), 1);
        assert_eq!(td.profiles[0].last_used, None, "{name}");
        reclaim(&t, &judge(now)).expect_err("fixed clock path is not a directory");
        assert!(debug.join(INCREMENTAL).exists());
    }
    for linked in [false, true] {
        let tmp = Tmp::new("unit-clock-link");
        let t = unit_target(&tmp);
        let debug = t.join("debug");
        let fingerprint = at(&debug, "build/lay/2bcb77ac5af7c69c/fingerprint");
        if linked {
            let outside = tmp.path().join("outside");
            std::fs::create_dir_all(&outside).expect("outside directory");
            std::fs::write(outside.join("invoked.timestamp"), b"outside").expect("outside stamp");
            std::fs::remove_dir_all(&fingerprint).expect("replace fingerprint");
            link_dir(&outside, &fingerprint);
        } else {
            let stamp = fingerprint.join("invoked.timestamp");
            std::fs::remove_file(&stamp).expect("replace stamp");
            std::fs::create_dir(&stamp).expect("a directory is not an invocation stamp");
        }
        let now = now_after_write(STALE + 1);
        let td = assessed(&t, now);
        assert_eq!(td.profiles.len(), 1);
        assert_eq!(td.profiles[0].last_used, None);
        reclaim(&t, &judge(now)).expect_err("unknown unit clock");
        assert!(debug.join(INCREMENTAL).exists());
    }
}

#[test]
fn reclaimable_profile_totals_preserve_exact_maximum_overflow_and_partial_counts() {
    let tmp = Tmp::new("profile-total");
    let t = synthetic_target(&tmp, "target");
    let now = now_after_write(STALE + 1);
    let mut td = assessed(&t, now);
    assert_eq!(td.profiles.len(), 1);
    td.profiles[0].bytes = u64::MAX;
    td.profiles[0].bytes_partial = false;
    assert_eq!(td.reclaimable(now, STALE), (u64::MAX, false));

    let mut extra = td.profiles[0].clone();
    extra.path = t.join("another-profile");
    extra.bytes = 1;
    td.profiles.push(extra);
    assert_eq!(td.reclaimable(now, STALE), (u64::MAX, true));

    td.profiles[1].bytes = 0;
    assert_eq!(td.reclaimable(now, STALE), (u64::MAX, false));
    td.profiles[1].bytes_partial = true;
    assert_eq!(td.reclaimable(now, STALE), (u64::MAX, true));
    td.profiles.clear();
    assert_eq!(td.reclaimable(now, STALE), (0, false));
}

#[test]
fn reclaimable_totals_use_only_the_same_eligible_profiles_as_the_candidate_list() {
    let tmp = Tmp::new("profile-selection-total");
    let t = synthetic_target(&tmp, "target");
    let now = now_after_write(STALE + 1);
    let mut td = assessed(&t, now);
    assert_eq!(td.profiles.len(), 1);
    td.profiles[0].bytes = 7;
    td.profiles[0].bytes_partial = false;
    let base = td.profiles[0].clone();
    for (name, last_used, incremental, in_use) in [
        ("held", base.last_used, true, Some("locked".to_owned())),
        ("recent", Some(now), true, None),
        ("unknown-age", None, true, None),
        ("empty", base.last_used, false, None),
    ] {
        td.profiles.push(Profile {
            path: t.join(name),
            last_used,
            incremental,
            in_use,
            bytes: u64::MAX,
            bytes_partial: true,
            deps_entries: None,
            died: Died::default(),
        });
    }
    assert_eq!(td.candidates(now, STALE), vec![&base]);
    assert_eq!(td.reclaimable(now, STALE), (7, false));
    td.profiles[0].bytes_partial = true;
    assert_eq!(td.reclaimable(now, STALE), (7, true));
}

/// An idle profile loses only its `incremental/` cache and releases its locks.
#[test]
fn an_idle_profile_loses_only_its_incremental_cache_and_everything_else_is_byte_identical() {
    let tmp = Tmp::new("only-incremental");
    let t = furnished(&tmp, "target");
    let now = now_after_write(STALE + 1);
    let cache = at(&t, "debug/incremental/lay-1x2y3z/s-abc-def/query-cache.bin");
    let expect = blocks(&cache);
    let before = snapshot(&t, &in_incremental);

    let td = assessed(&t, now);
    assert_eq!(td.profiles.len(), 1, "{td:?}");
    let w = target_witness(&td, now, STALE).expect("a witness");
    assert!(
        matches!(w, Witness::StaleBuildDir { profiles: 1, .. }),
        "{w:?}"
    );
    assert_eq!(td.reclaimable(now, STALE), (expect, false));

    let done = reclaim(&t, &judge(now)).expect("reclaimed");
    assert_eq!(done.units, vec![at(&t, "debug/incremental")], "{done:?}");
    assert!(
        done.skipped.is_empty() && done.trouble.is_empty(),
        "{done:?}"
    );
    assert_eq!(done.bytes, expect, "the blocks the query cache released");
    assert!(!at(&t, "debug/incremental").exists());
    assert_eq!(
        snapshot(&t, &in_incremental),
        before,
        "every entry but incremental/ is byte-identical"
    );
    assert!(
        CARGO_LOCKS.iter().all(|l| !held(&t.join("debug").join(l))),
        "every lock released"
    );

    let again = assessed(&t, now);
    let why = target_witness(&again, now, STALE).expect_err("nothing to reclaim");
    assert!(
        why.contains("no idle profile holds an incremental/"),
        "{why}"
    );
    let err = reclaim(&t, &judge(now)).expect_err("nothing to reclaim");
    assert!(err.to_string().contains("nothing reclaimed"), "{err}");
}

/// Profiles are discovered at any depth and judged independently.
#[test]
fn profiles_are_found_at_any_depth_and_each_is_judged_alone() {
    let tmp = Tmp::new("nested");
    let t = synthetic_target(&tmp, "target");
    lay_profile(&at(&t, "aarch64-apple-darwin/release"));
    lay_profile(&at(&t, "conformance-release/release"));
    let now = now_after_write(STALE + 1);
    set_mtime(
        &at(&t, "aarch64-apple-darwin/release/deps"),
        inside_window(now),
    );

    let td = assessed(&t, now);
    let mut found: Vec<PathBuf> = td.profiles.iter().map(|p| p.path.clone()).collect();
    found.sort();
    assert_eq!(
        found,
        vec![
            at(&t, "aarch64-apple-darwin/release"),
            at(&t, "conformance-release/release"),
            t.join("debug"),
        ]
    );
    assert!(matches!(
        target_witness(&td, now, STALE),
        Ok(Witness::StaleBuildDir { profiles: 2, .. })
    ));

    let done = reclaim(&t, &judge(now)).expect("reclaimed");
    let mut went = done.units.clone();
    went.sort();
    assert_eq!(
        went,
        vec![
            at(&t, "conformance-release/release/incremental"),
            at(&t, "debug/incremental"),
        ]
    );
    assert!(
        at(&t, "aarch64-apple-darwin/release/incremental").exists(),
        "the profile used inside the window keeps its cache"
    );
}

/// A walk cut short (entries, depth or time) makes the whole directory not a candidate.
#[test]
fn a_walk_cut_short_makes_the_whole_directory_not_a_candidate() {
    let tmp = Tmp::new("cut");
    let t = synthetic_target(&tmp, "target");
    let now = now_after_write(STALE + 1);
    for budget in [
        WalkBudget {
            entries: 2,
            ..WalkBudget::SHIPPED
        },
        WalkBudget {
            depth: 0,
            ..WalkBudget::SHIPPED
        },
        WalkBudget {
            wall: Duration::ZERO,
            ..WalkBudget::SHIPPED
        },
    ] {
        let td = assess(&t, now, STALE, budget).expect("a directory");
        assert!(td.walk_cut_short.is_some(), "{budget:?}: {td:?}");
        let why = target_witness(&td, now, STALE).expect_err("not a candidate");
        assert!(why.contains("not all known"), "{why}");
    }
    assert!(target_witness(&assessed(&t, now), now, STALE).is_ok());
}

/// A size walk past its deadline stops immediately and reports truncation.
#[test]
fn a_size_past_its_deadline_is_cut_short() {
    let tmp = Tmp::new("size-deadline");
    let t = furnished(&tmp, "target");
    let now = std::time::Instant::now();
    assert_eq!(total_bytes_until(&t, now), (0, true));
    let whole = total_bytes(&t);
    assert!(whole.0 > 0 && !whole.1, "{whole:?}");
    assert_eq!(
        total_bytes_until(&t, now + Duration::from_secs(3600)),
        whole
    );
}

/// The idle clock is [`CLOCK_DIRS`], never a lock or profile directory mtime.
#[test]
fn the_idle_clock_is_the_cache_directories_only() {
    let tmp = Tmp::new("clock");
    let t = synthetic_target(&tmp, "target");
    let debug = t.join("debug");
    let now = now_after_write(STALE + 1);
    let old = now - (STALE + 5) * DAY;
    for c in CACHE_DIRS {
        set_mtime(&debug.join(c), old);
    }
    std::fs::create_dir_all(debug.join("examples")).expect("examples");
    set_mtime(&debug.join("examples"), old);
    set_mtime(&debug.join(".cargo-lock"), now);
    set_mtime(&debug, now);
    assert!(target_witness(&assessed(&t, now), now, STALE).is_ok());
    set_mtime(&debug.join(".cargo-lock"), old);
    set_mtime(&debug.join("deps"), inside_window(now));
    let why = target_witness(&assessed(&t, now), now, STALE).expect_err("in use");
    assert!(
        why.contains(&format!("written into {}d ago", STALE - 1)),
        "{why}"
    );
    set_mtime(&debug.join("deps"), old);
    set_mtime(&debug.join("examples"), inside_window(now));
    assert!(target_witness(&assessed(&t, now), now, STALE).is_err());
}

/// A build holding any cargo lock preserves its profile while idle siblings are reclaimed.
#[test]
fn a_build_holding_any_lock_keeps_its_profile() {
    let now = now_after_write(STALE + 1);
    for lock in CARGO_LOCKS {
        let tmp = Tmp::new("held");
        let t = synthetic_target(&tmp, "target");
        lay_profile(&t.join("release"));
        let busy = t.join("debug");
        let before = snapshot(&busy, &|_| false);
        let hold = take(&busy.join(lock));

        let td = assessed(&t, now);
        let debug = td
            .profiles
            .iter()
            .find(|p| p.path == busy)
            .expect("the debug profile");
        assert!(
            debug
                .in_use
                .as_deref()
                .is_some_and(|w| w.contains("a build holds") && w.contains(lock)),
            "{lock}: {debug:?}"
        );
        assert!(
            matches!(
                target_witness(&td, now, STALE),
                Ok(Witness::StaleBuildDir { profiles: 1, .. })
            ),
            "{lock}: only the release profile is a candidate"
        );

        let done = reclaim(&t, &judge(now)).expect("the sibling is reclaimed");
        assert_eq!(done.units, vec![at(&t, "release/incremental")], "{lock}");
        assert!(
            done.skipped
                .iter()
                .any(|(p, why)| p == &busy && why.contains("a build holds")),
            "{lock}: {done:?}"
        );
        assert_eq!(snapshot(&busy, &|_| false), before, "{lock}: untouched");
        drop(hold);
    }
}

/// Every lock is held from before anything is taken until what was taken is staged, and let go
/// before the stage is deleted (Windows, which stages nothing: until after the delete).
///
/// Negative control: a pass that deleted in place on unix would show `incremental/` still in its
/// place at `Deleting` with every lock held.
#[test]
fn every_lock_is_held_until_what_it_took_is_staged() {
    let tmp = Tmp::new("locks");
    let t = synthetic_target(&tmp, "target");
    let debug = t.join("debug");
    let now = now_after_write(STALE + 1);
    let mut seen = Vec::new();
    reclaim_with(&t, &judge(now), &mut |stage, _| {
        if stage == Stage::Removing {
            return;
        }
        let all = CARGO_LOCKS.iter().all(|l| held(&debug.join(l)));
        let inc = debug.join(INCREMENTAL).exists();
        seen.push((stage, all, inc));
    })
    .expect("reclaimed");
    #[cfg(unix)]
    let expected = vec![
        (Stage::Locked, true, true),
        (Stage::Staging, true, true),
        (Stage::Staged, true, false),
        (Stage::Released, false, false),
        (Stage::Deleting, false, false),
    ];
    #[cfg(windows)]
    let expected = vec![
        (Stage::Locked, true, true),
        (Stage::Staging, true, true),
        (Stage::Deleting, true, true),
        (Stage::Released, false, false),
    ];
    assert_eq!(seen, expected, "(stage, locks held, in place)");
    assert!(!debug.join(INCREMENTAL).exists());
    assert!(!debug.join(STAGE).exists(), "no stage is left");
}

/// Reclaim locks are released via `LOCK_UN` even while a forked child has not yet exec'd.
#[test]
#[cfg(unix)]
fn a_lock_is_released_while_a_child_forked_under_it_has_yet_to_exec() {
    let tmp = Tmp::new("fork-window");
    let t = synthetic_target(&tmp, "target");
    let debug = t.join("debug");
    let now = now_after_write(STALE + 1);
    let mut child: Option<Parked> = None;
    let mut seen = Vec::new();
    reclaim_with(&t, &judge(now), &mut |stage, _| {
        if stage == Stage::Removing {
            return;
        }
        if stage == Stage::Locked {
            child = Some(Parked::fork());
        }
        let pending = child.as_ref().is_some_and(Parked::pending);
        let all = CARGO_LOCKS.iter().all(|l| held(&debug.join(l)));
        seen.push((stage, all, pending));
    })
    .expect("reclaimed");
    let status = child.take().expect("forked").exec();
    assert!(status.success(), "{status:?}");
    assert_eq!(
        seen,
        vec![
            (Stage::Locked, true, true),
            (Stage::Staging, true, true),
            (Stage::Staged, true, true),
            (Stage::Released, false, true),
            (Stage::Deleting, false, true),
        ],
        "(stage, every lock held, the child not yet exec'd)"
    );
    assert!(!debug.join(INCREMENTAL).exists());
}

/// [`locks_held`] releases each probed lock via `LOCK_UN` even across a concurrent fork.
#[test]
#[cfg(unix)]
fn a_probed_lock_is_released_while_a_child_forked_under_the_probe_has_yet_to_exec() {
    let tmp = Tmp::new("probe-fork-window");
    let t = synthetic_target(&tmp, "target");
    let debug = t.join("debug");
    let now = now_after_write(STALE + 1);
    let mut parked: Vec<Parked> = Vec::new();
    let mut under = Vec::new();
    let verdict = locks_held_with(&debug, &mut |lock| {
        under.push(held(lock));
        parked.push(Parked::fork());
    });
    let after: Vec<bool> = CARGO_LOCKS.iter().map(|l| held(&debug.join(l))).collect();
    let again = locks_held(&debug);
    let done = reclaim(&t, &judge(now));
    let pending = parked.iter().all(Parked::pending);
    let forked = parked.len();
    for child in parked {
        let status = child.exec();
        assert!(status.success(), "{status:?}");
    }
    assert_eq!(verdict, None, "no build holds any lock");
    assert_eq!(forked, CARGO_LOCKS.len(), "a child forked under each lock");
    assert!(
        under.iter().all(|h| *h),
        "the hook ran under the probe's lock: {under:?}"
    );
    assert!(pending, "every child was parked throughout");
    assert_eq!(
        after,
        vec![false; CARGO_LOCKS.len()],
        "every lock free once the probe returned"
    );
    assert_eq!(again, None, "the next probe reads no build");
    let done = done.expect("reclaimed");
    assert_eq!(done.units, vec![debug.join(INCREMENTAL)], "{done:?}");
}

/// A compile since the survey keeps the profile: the verdict is derived again under the locks.
#[test]
fn a_compile_since_the_survey_keeps_the_profile() {
    let tmp = Tmp::new("since");
    let t = synthetic_target(&tmp, "target");
    let debug = t.join("debug");
    let now = now_after_write(STALE + 1);
    let err = reclaim_with(&t, &judge(now), &mut |stage, _| {
        if stage == Stage::Locked {
            set_mtime(&debug.join("deps"), now);
        }
    })
    .expect_err("no longer idle");
    assert!(err.to_string().contains("no longer idle"), "{err}");
    assert!(debug.join(INCREMENTAL).exists());
}

/// An unreadable clock is refused as unreadable, never dated `0d ago`.
#[test]
fn an_unread_clock_is_said_never_dated_zero_days() {
    let now = 2_000_000_000;
    let unread = "its caches' clock could not be read";
    let idle = Select::Idle { leave: &[] };
    assert_eq!(idle.refuses(None, &judge(now)).as_deref(), Some(unread));
    let lru = Select::Lru {
        profile: Path::new("target/debug"),
        seen: now,
    };
    assert_eq!(lru.refuses(None, &judge(now)).as_deref(), Some(unread));
    assert_eq!(
        idle.refuses(Some(now - (STALE + 1) * DAY), &judge(now)),
        None
    );
    let recent = idle
        .refuses(Some(inside_window(now)), &judge(now))
        .expect("inside the window");
    assert_eq!(
        recent,
        format!("no longer idle: written into 0d ago, under the {STALE}d threshold")
    );
    let zero = Judge {
        threshold_days: 0,
        ..judge(now)
    };
    assert_eq!(
        idle.refuses(Some(now - 5 * 60), &zero).as_deref(),
        Some(
            "no longer idle: written into 5 min ago; no reclaim takes a profile built in the last 10 min"
        )
    );
}

/// Missing locks are created only when removal proceeds, and creation failure skips the profile.
#[test]
fn a_missing_lock_is_created_only_when_the_removal_proceeds_and_its_failure_skips() {
    let now = now_after_write(STALE + 1);
    let old_cargo = |tmp: &Tmp, name: &str| {
        let t = synthetic_target(tmp, name);
        for lock in &CARGO_LOCKS[1..] {
            std::fs::remove_file(t.join("debug").join(lock)).expect("an older cargo's");
        }
        t
    };
    let tmp = Tmp::new("create");
    let only = |t: &Path| {
        CARGO_LOCKS
            .iter()
            .filter(|l| t.join("debug").join(l).exists())
            .count()
    };

    let t = old_cargo(&tmp, "report");
    assert!(target_witness(&assessed(&t, now), now, STALE).is_ok());
    assert_eq!(only(&t), 1, "the report wrote no lock");
    let mut all_held = false;
    reclaim_with(&t, &judge(now), &mut |stage, _| {
        if stage == Stage::Staging {
            all_held = CARGO_LOCKS.iter().all(|l| held(&t.join("debug").join(l)));
        }
    })
    .expect("reclaimed");
    assert!(all_held);
    assert_eq!(only(&t), 3, "created now that the removal proceeds");

    let t = old_cargo(&tmp, "held");
    let hold = take(&at(&t, "debug/.cargo-lock"));
    let err = reclaim(&t, &judge(now)).expect_err("held");
    assert!(err.to_string().contains("a build holds"), "{err}");
    assert_eq!(only(&t), 1, "nothing created while a build holds one");
    drop(hold);

    let t = old_cargo(&tmp, "full");
    inject_create_lock_fault(true);
    let err = reclaim(&t, &judge(now)).expect_err("cannot lock");
    inject_create_lock_fault(false);
    assert!(err.to_string().contains("could not be created"), "{err}");
    assert!(t.join("debug").join(INCREMENTAL).exists(), "cache intact");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let t = old_cargo(&tmp, "readonly");
        let debug = t.join("debug");
        std::fs::set_permissions(&debug, std::fs::Permissions::from_mode(0o555)).expect("chmod");
        let err = reclaim(&t, &judge(now)).expect_err("cannot lock");
        std::fs::set_permissions(&debug, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert!(err.to_string().contains("could not be created"), "{err}");
        assert!(debug.join(INCREMENTAL).exists(), "cache intact");
        assert_eq!(only(&t), 1);
    }
}

/// Symlinks in path components, `incremental`, or swapped profiles are never followed.
#[test]
fn nothing_is_reached_through_a_symlink() {
    let tmp = Tmp::new("links");
    let now = now_after_write(STALE + 1);
    let victim = tmp.path().join("victim");
    lay_target(&victim);
    let victim_before = snapshot(&victim, &|_| false);

    let parent = tmp.path().join("repo");
    let t = lay_target(&parent.join("target"));
    let t = std::fs::canonicalize(&t).expect("canonical");
    assert!(target_witness(&assessed(&t, now), now, STALE).is_ok());
    let parent = std::fs::canonicalize(&parent).expect("canonical");
    std::fs::rename(&parent, tmp.path().join("repo.moved")).expect("move");
    link_dir(&victim, &parent);
    std::fs::create_dir_all(victim.join("target")).expect("decoy");
    lay_target(&victim.join("target"));
    let decoy_before = snapshot(&victim.join("target"), &|_| false);
    let err = reclaim(&t, &judge(now)).expect_err("a link in the path");
    assert!(err.to_string().contains("symlink"), "{err}");
    assert!(!err.to_string().contains(IN_SYNC_ROOT), "{err}");
    assert_eq!(snapshot(&victim.join("target"), &|_| false), decoy_before);
    std::fs::remove_dir_all(victim.join("target")).expect("decoy away");

    let t = synthetic_target(&tmp, "linked-inc");
    std::fs::remove_dir_all(at(&t, "debug/incremental")).expect("real one away");
    link_dir(
        &at(&victim, "debug/incremental"),
        &at(&t, "debug/incremental"),
    );
    let why = target_witness(&assessed(&t, now), now, STALE).expect_err("no real cache");
    assert!(
        why.contains("no idle profile holds an incremental/"),
        "{why}"
    );
    assert!(reclaim(&t, &judge(now)).is_err());
    assert!(std::fs::symlink_metadata(at(&t, "debug/incremental")).is_ok());

    let t = synthetic_target(&tmp, "inner-link");
    link_dir(&victim, &at(&t, "debug/incremental/escape"));
    reclaim(&t, &judge(now)).expect("reclaimed");
    assert!(!at(&t, "debug/incremental").exists());

    let t = synthetic_target(&tmp, "swap");
    let debug = t.join("debug");
    let moved = t.join("debug.moved");
    let mut swapped = None;
    let done = reclaim_with(&t, &judge(now), &mut |stage, _| {
        if stage == Stage::Locked {
            let renamed = std::fs::rename(&debug, &moved);
            if renamed.is_ok() {
                link_dir(&victim.join("debug"), &debug);
            }
            swapped = Some(renamed);
        }
    })
    .expect("reclaimed on the checked handle");
    let swapped = swapped.expect("the hook ran");
    #[cfg(unix)]
    let checked = {
        swapped.expect("swap");
        moved
    };
    #[cfg(windows)]
    let checked = {
        swapped.expect_err("NTFS refuses the rename while the reclaim holds the profile");
        debug
    };
    assert!(done.bytes > 0, "{done:?}");
    assert!(
        !checked.join(INCREMENTAL).exists(),
        "the checked directory's cache went"
    );
    assert_eq!(
        snapshot(&victim, &|_| false),
        victim_before,
        "the victim is byte-identical throughout"
    );
}

/// Deletion never crosses onto another device or volume, and a directory on another device
/// ends where it was: the delete of the stage never enters it, and what that delete left goes
/// back.
#[test]
fn the_delete_never_enters_another_device() {
    let tmp = Tmp::new("device");
    let t = synthetic_target(&tmp, "target");
    let inc = t.join("debug").join(INCREMENTAL);
    let counted = blocks(&at(&inc, "lay-1x2y3z/s-abc-def/query-cache.bin"));
    let mount = inc.join("mounted");
    std::fs::create_dir_all(&mount).expect("mount");
    std::fs::write(mount.join("theirs"), vec![3u8; 4096]).expect("theirs");
    mark_foreign(&mount);

    let done = reclaim(&t, &judge(now_after_write(STALE + 1))).expect("reclaimed");
    assert!(mount.join("theirs").exists(), "not entered");
    assert!(!inc.join("lay-1x2y3z").exists(), "the rest went");
    assert!(
        done.trouble.iter().any(|t| t.contains("another device")),
        "{done:?}"
    );
    assert!(done.units.is_empty(), "incremental/ itself stays: {done:?}");
    assert_eq!(
        done.bytes, counted,
        "nothing on the other device is counted"
    );
    // What the stage's delete left went back where it was: no stage stands.
    #[cfg(unix)]
    assert!(stages_in(&t).is_empty(), "nothing stays staged");

    let t = synthetic_target(&tmp, "volume");
    let dev = dev_of(&t).expect("dev");
    let elsewhere = Judge {
        device: Some(dev.wrapping_add(1)),
        ..judge(now_after_write(STALE + 1))
    };
    let err = reclaim(&t, &elsewhere).expect_err("another volume");
    assert!(err.to_string().contains("another volume"), "{err}");
    assert!(t.join("debug").join(INCREMENTAL).exists());
}

/// [`VolumeGuid`] parses case-insensitively, rejects malformed strings, and [`same_volume`] requires matching GUIDs.
#[test]
fn a_volume_guid_reads_in_any_case_and_the_fence_holds_to_it() {
    use aterm_sysprobe::volumes::VolumeKey;
    const C: &str = "{6140fc3b-044f-43d7-b060-cb98170c7a63}";
    let c = VolumeGuid::parse(C).expect("a GUID");
    assert_eq!(VolumeGuid::parse(&C.to_ascii_uppercase()), Some(c));
    assert_eq!(c.to_string(), C);
    assert_eq!(
        VolumeGuid::of_key(&VolumeKey::Volume(C.to_owned())),
        Some(c)
    );
    assert_eq!(VolumeGuid::of_key(&VolumeKey::Fsid(7)), None);
    assert_eq!(
        VolumeGuid::of_key(&VolumeKey::Container("disk3".to_owned())),
        None
    );
    for not_one in [
        "",
        "6140fc3b-044f-43d7-b060-cb98170c7a63",
        "{6140fc3b-044f-43d7-b060-cb98170c7a6}",
        "{6140fc3b-044f-43d7-b060-cb98170c7a634}",
        "{6140fc3b044f-43d7-b060-cb98170c7a63-}",
        "{6140fc3g-044f-43d7-b060-cb98170c7a63}",
        "{+140fc3b-044f-43d7-b060-cb98170c7a63}",
        "{6140fc3b-044f-43d7-b060-cb98170c7a63",
    ] {
        assert_eq!(VolumeGuid::parse(not_one), None, "{not_one}");
    }
    let d = VolumeGuid::parse("{6140fc3b-044f-43d7-b060-cb98170c7a62}").expect("a GUID");
    assert_ne!(c, d);
    assert_eq!(same_volume(Some(c), Some(c)), Ok(()));
    for (measured, here, why) in [
        (Some(c), Some(d), "GUIDs differ"),
        (None, Some(c), "measured volume's own name"),
        (Some(c), None, "could not be read"),
        (None, None, "measured volume's own name"),
    ] {
        let err = same_volume(measured, here).expect_err("refused");
        assert!(err.contains(why), "{measured:?} {here:?}: {err}");
    }
}

/// Counted bytes match the blocks actually released by unlinking (excluding hard-linked files and unallocated sparse ranges).
#[test]
fn the_bytes_counted_are_the_blocks_the_unlinks_released() {
    let tmp = Tmp::new("bytes");
    let t = synthetic_target(&tmp, "target");
    let debug = t.join("debug");
    let session = at(&debug, "incremental/lay-1x2y3z/s-abc-def");
    std::fs::write(session.join("lay.o"), vec![5u8; 64 * 1024]).expect("object");
    std::fs::hard_link(session.join("lay.o"), at(&debug, "deps/lay.o")).expect("linked twin");
    let now = now_after_write(STALE + 1);
    #[cfg_attr(windows, expect(unused_mut, reason = "unix adds the sparse file"))]
    let mut expect = blocks(&session.join("query-cache.bin"));
    #[cfg(unix)]
    {
        let sparse = session.join("sparse");
        std::fs::File::create(&sparse)
            .and_then(|f| f.set_len(64 * 1024 * 1024))
            .expect("sparse");
        expect += blocks(&sparse);
        assert!(expect < 1024 * 1024, "a sparse file holds few blocks");
    }
    assert!(expect > 0, "the query cache holds blocks");

    assert_eq!(assessed(&t, now).reclaimable(now, STALE), (expect, false));
    let done = reclaim(&t, &judge(now)).expect("reclaimed");
    assert_eq!(done.bytes, expect, "{done:?}");
    assert_eq!(
        std::fs::read(at(&debug, "deps/lay.o")).expect("kept"),
        vec![5u8; 64 * 1024],
        "the deps/ twin keeps its data"
    );
}

/// An undeletable entry is reported and uncounted while its siblings are still removed, and it
/// ends where it was: what the delete of a stage leaves goes back, and its line names it there.
///
/// Negative control (unix): without the move back the stuck folder would sit in the pass's
/// stage, where `ro` names nothing and a stage stands.
#[test]
fn an_undeletable_entry_is_reported_and_its_siblings_still_go() {
    let tmp = Tmp::new("undeletable");
    let t = synthetic_target(&tmp, "target");
    let inc = t.join("debug").join(INCREMENTAL);
    let ro = inc.join("stuck");
    let stuck = Stuck::at(&ro);
    let mut counted = blocks(&at(&inc, "lay-1x2y3z/s-abc-def/query-cache.bin"));
    for i in 0..10 {
        let f = inc.join(format!("f{i:02}"));
        std::fs::write(&f, vec![2u8; 8192]).expect("sibling");
        counted += blocks(&f);
    }

    let done = reclaim(&t, &judge(now_after_write(STALE + 1))).expect("partly reclaimed");
    drop(stuck);
    assert!(ro.join("file").exists(), "the stuck file stays");
    let left: Vec<String> = std::fs::read_dir(&inc)
        .expect("incremental/ stays")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, ["stuck"], "every sibling went");
    assert_eq!(done.bytes, counted, "only what went: {done:?}");
    assert!(done.units.is_empty(), "not claimed as gone: {done:?}");
    assert!(
        done.trouble.iter().any(|t| t.contains("stuck")),
        "the error is surfaced: {done:?}"
    );
    #[cfg(unix)]
    {
        assert!(stages_in(&t).is_empty(), "no stage stands: {done:?}");
        let said = ro.join("file").display().to_string();
        assert!(
            done.trouble.iter().any(|t| t.starts_with(&said)),
            "its line names it where it is: {done:?}"
        );
    }
}

/// A directory holding `Cargo.toml` or `src/` at its root is never a reclaim candidate.
#[test]
fn a_directory_holding_source_is_never_a_candidate() {
    let tmp = Tmp::new("source");
    let t = synthetic_target(&tmp, "target");
    std::fs::write(t.join("Cargo.toml"), b"[package]\n").expect("manifest");
    let now = now_after_write(STALE + 1);
    let why = target_witness(&assessed(&t, now), now, STALE).expect_err("source");
    assert!(why.contains("Cargo.toml"), "{why}");
    let err = reclaim(&t, &judge(now)).expect_err("source");
    assert!(err.to_string().contains("source"), "{err}");
    assert!(t.join("debug").join(INCREMENTAL).exists());
}

/// Partial deletion updating `incremental/`'s mtime leaves the profile idle for the next
/// reclaim, whether the pass staged what it took and moved back what its delete left, or (unix
/// too: a full disk makes no stage) deleted it in place.
#[test]
fn a_reclaim_cut_short_leaves_the_profile_idle_for_the_next() {
    #[cfg(unix)]
    let ways = [false, true];
    #[cfg(windows)]
    let ways = [false];
    for in_place in ways {
        let tmp = Tmp::new(["cut-short", "cut-short-in-place"][usize::from(in_place)]);
        let t = synthetic_target(&tmp, "target");
        let debug = t.join("debug");
        let inc = debug.join(INCREMENTAL);
        let stuck = inc.join("stuck");
        let held = Stuck::at(&stuck);
        let now = now_after_write(0);
        let old = now - 30 * DAY;
        for d in [
            ".fingerprint",
            "deps",
            "build",
            "incremental",
            "incremental/lay-1x2y3z",
        ] {
            set_mtime(&at(&debug, d), old);
        }
        assert!(target_witness(&assessed(&t, now), now, STALE).is_ok());

        #[cfg(unix)]
        fd::inject_stage_fault(in_place.then_some(libc::ENOSPC));
        let done = reclaim(&t, &judge(now));
        #[cfg(unix)]
        fd::inject_stage_fault(None);
        let done = done.expect("partly reclaimed");
        assert!(done.units.is_empty(), "{done:?}");
        assert!(stuck.join("file").exists());
        drop(held);

        let again = assessed(&t, now);
        assert!(
            target_witness(&again, now, STALE).is_ok(),
            "still idle after its own partial delete: {again:?}"
        );
        let done = reclaim(&t, &judge(now)).expect("the rest");
        assert_eq!(done.units, vec![inc.clone()], "{done:?}");
        assert!(!inc.exists());
    }
}

/// Created locks inherit the standard `OpenOptions` mode under the process umask.
#[test]
#[cfg(unix)]
fn a_created_lock_takes_the_mode_cargo_gives_it() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = Tmp::new("lock-mode");
    let t = synthetic_target(&tmp, "target");
    let debug = t.join("debug");
    for lock in &CARGO_LOCKS[1..] {
        std::fs::remove_file(debug.join(lock)).expect("an older cargo's");
    }
    reclaim(&t, &judge(now_after_write(STALE + 1))).expect("reclaimed");
    let reference = tmp.path().join("as-cargo-makes-it");
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&reference)
        .expect("reference");
    let mode = |p: &Path| std::fs::metadata(p).expect("stat").permissions().mode() & 0o777;
    for lock in &CARGO_LOCKS[1..] {
        assert_eq!(
            format!("{:o}", mode(&debug.join(lock))),
            format!("{:o}", mode(&reference)),
            "{lock}"
        );
    }
}

/// An idle profile with an empty `incremental/` is skipped without creating locks.
#[test]
fn an_empty_incremental_is_not_a_candidate_and_gets_no_lock() {
    let tmp = Tmp::new("empty-inc");
    let t = synthetic_target(&tmp, "target");
    let release = t.join("release");
    lay_profile(&release);
    std::fs::remove_dir_all(release.join(INCREMENTAL)).expect("emptied");
    std::fs::create_dir(release.join(INCREMENTAL)).expect("empty");
    std::fs::remove_file(release.join(".cargo-artifact-lock")).expect("a check-only profile");
    let now = now_after_write(STALE + 1);

    let td = assessed(&t, now);
    assert_eq!(td.profiles.len(), 2, "{td:?}");
    let cands: Vec<&Path> = td
        .candidates(now, STALE)
        .iter()
        .map(|p| p.path.as_path())
        .collect();
    assert_eq!(cands, vec![t.join("debug").as_path()], "{td:?}");
    assert!(matches!(
        target_witness(&td, now, STALE),
        Ok(Witness::StaleBuildDir { profiles: 1, .. })
    ));

    let done = reclaim(&t, &judge(now)).expect("the debug profile");
    assert_eq!(done.units, vec![at(&t, "debug/incremental")], "{done:?}");
    assert!(release.join(INCREMENTAL).is_dir(), "the empty one stays");
    assert!(
        !release.join(".cargo-artifact-lock").exists(),
        "no lock created for nothing"
    );

    std::fs::remove_dir_all(t.join("debug")).expect("only release left");
    let why = target_witness(&assessed(&t, now), now, STALE).expect_err("nothing to take");
    assert!(
        why.contains("no idle profile holds an incremental/"),
        "{why}"
    );
    assert!(reclaim(&t, &judge(now)).is_err());
    assert!(!release.join(".cargo-artifact-lock").exists());
}

/// `dev_of` follows symlinks so a linked home fences reclaim to its target volume.
#[test]
#[cfg(unix)]
fn the_device_of_a_link_is_the_one_it_resolves_to() {
    use std::os::unix::fs::MetadataExt as _;
    let tmp = Tmp::new("dev-link");
    let link = tmp.path().join("home");
    std::os::unix::fs::symlink("/dev", &link).expect("link");
    let devfs = std::fs::metadata("/dev").expect("/dev").dev();
    assert_ne!(
        devfs,
        std::fs::metadata(tmp.path()).expect("tmp").dev(),
        "the fixture needs /dev on its own volume"
    );
    assert_eq!(dev_of(&link), Some(devfs));
}

/// Date `profile` last used at `at` across [`CLOCK_DIRS`].
fn last_used(profile: &Path, at: i64) {
    for c in CLOCK_DIRS {
        let d = profile.join(c);
        if d.exists() {
            set_mtime(&d, at);
        }
    }
}

/// [`reclaim_lru`] removes only the named profile's `incremental/` under all cargo locks.
#[test]
fn reclaim_lru_takes_only_the_named_profile_under_every_lock() {
    let tmp = Tmp::new("lru-one");
    let t = synthetic_target(&tmp, "target");
    lay_profile(&t.join("release"));
    let now = now_after_write(0);
    let (debug, release) = (t.join("debug"), t.join("release"));
    last_used(&debug, now - 3 * 3_600);
    last_used(&release, now - 3_600);
    assert!(
        reclaim(&t, &judge(now)).is_err(),
        "the idle reclaim takes neither"
    );
    let before = snapshot(&t, &|rel| rel.starts_with("debug/incremental"));

    let mut seen = Vec::new();
    // The profile's own locks: a stage's delete names the stage, not the profile.
    let done = reclaim_lru_with(&t, &debug, &judge(now), now - 3 * 3_600, &mut |stage, _| {
        if stage == Stage::Removing {
            return;
        }
        let all = CARGO_LOCKS.iter().all(|l| held(&debug.join(l)));
        seen.push((stage, all));
    })
    .expect("reclaimed");
    assert_eq!(done.units, vec![debug.join(INCREMENTAL)], "{done:?}");
    assert!(
        done.skipped.is_empty() && done.trouble.is_empty(),
        "{done:?}"
    );
    #[cfg(unix)]
    let expected = vec![
        (Stage::Locked, true),
        (Stage::Staging, true),
        (Stage::Staged, true),
        (Stage::Released, false),
        (Stage::Deleting, false),
    ];
    #[cfg(windows)]
    let expected = vec![
        (Stage::Locked, true),
        (Stage::Staging, true),
        (Stage::Deleting, true),
        (Stage::Released, false),
    ];
    assert_eq!(seen, expected);
    assert!(!debug.join(INCREMENTAL).exists());
    assert_eq!(
        snapshot(&t, &|rel| rel.starts_with("debug/incremental")),
        before,
        "the sibling's cache and everything else are byte-identical"
    );
}

/// [`reclaim_lru`] refuses profiles modified since the survey, within [`PRESSURE_MIN_IDLE_S`], or unknown.
#[test]
fn reclaim_lru_refuses_a_profile_used_since_the_survey_or_just_built() {
    let tmp = Tmp::new("lru-refuse");
    let t = synthetic_target(&tmp, "target");
    let debug = t.join("debug");
    let now = now_after_write(0);
    let then = now - 3 * 3_600;
    last_used(&debug, then);

    let err = reclaim_lru_with(&t, &debug, &judge(now), then, &mut |stage, _| {
        if stage == Stage::Locked {
            set_mtime(&debug.join("deps"), then + 60);
        }
    })
    .expect_err("used since the survey");
    assert!(err.to_string().contains("since the survey"), "{err}");
    assert!(debug.join(INCREMENTAL).exists());
    let err = reclaim_lru(&t, &debug, &judge(now), then).expect_err("used since");
    assert!(err.to_string().contains("since the survey"), "{err}");

    last_used(&debug, now - 5 * 60);
    reclaim_lru(&t, &debug, &judge(now), now - 5 * 60).expect_err("just built");
    assert!(debug.join(INCREMENTAL).exists());

    reclaim_lru(&t, &t.join("nope"), &judge(now), then).expect_err("no such profile");
    assert!(debug.join(INCREMENTAL).exists());
}

/// Whole-directory removal refuses unlocked newcomer profiles and deletes only through its pinned handle.
#[test]
fn a_whole_removal_takes_no_profile_it_did_not_lock_and_deletes_through_its_handle() {
    let now = now_after_write(2);
    let ok = &mut || Ok(());

    let tmp = Tmp::new("whole-new-profile");
    let t = std::fs::canonicalize(lay_target(&tmp.path().join("target"))).expect("canonical");
    let err = reclaim_whole_with(&t, &judge(now), now, DAY, ok, &mut |stage, _| {
        if stage == Stage::Locked {
            lay_profile(&at(&t, "aarch64-apple-darwin/debug"));
        }
    })
    .expect_err("a profile it never locked");
    assert!(err.to_string().contains("profiles changed"), "{err}");
    assert!(at(&t, "debug/deps/liblay.rlib").exists());

    let tmp = Tmp::new("whole-swap");
    let t = std::fs::canonicalize(lay_target(&tmp.path().join("target"))).expect("canonical");
    let moved = tmp.path().join("target.moved");
    let mut swapped = None;
    let done = reclaim_whole_with(&t, &judge(now), now, DAY, ok, &mut |stage, _| {
        if stage == Stage::Staging {
            let renamed = std::fs::rename(&t, &moved);
            if renamed.is_ok() {
                std::fs::create_dir(&t).expect("swap in");
                std::fs::write(t.join("keep.txt"), b"not a build's").expect("newcomer");
            }
            swapped = Some(renamed);
        }
    })
    .expect("the pinned directory's contents went");
    let swapped = swapped.expect("the hook ran");
    #[cfg(unix)]
    {
        swapped.expect("swap out");
        assert!(t.join("keep.txt").exists(), "the newcomer is never entered");
        assert!(
            !moved.join("debug").exists(),
            "the pinned directory was emptied through its handle"
        );
        assert!(done.units.is_empty(), "{done:?}");
        assert!(
            done.trouble
                .iter()
                .any(|t| t.contains("names another directory")),
            "{done:?}"
        );
    }
    #[cfg(windows)]
    {
        swapped.expect_err("NTFS refuses the rename while the reclaim holds the directory");
        assert!(!moved.exists(), "nothing was moved");
        assert_eq!(done.units, std::slice::from_ref(&t), "{done:?}");
        assert!(!t.exists());
    }

    let tmp = Tmp::new("whole-alone");
    let t = std::fs::canonicalize(lay_target(&tmp.path().join("target"))).expect("canonical");
    let done =
        reclaim_whole_with(&t, &judge(now), now, DAY, ok, &mut |_, _| {}).expect("goes whole");
    assert_eq!(done.units, std::slice::from_ref(&t));
    assert!(!t.exists());

    let tmp = Tmp::new("whole-check");
    let t = std::fs::canonicalize(lay_target(&tmp.path().join("target"))).expect("canonical");
    let err = reclaim_whole_with(
        &t,
        &judge(now),
        now,
        DAY,
        &mut || Err("its session resumed".to_owned()),
        &mut |_, _| {},
    )
    .expect_err("the check refuses");
    assert!(err.to_string().contains("its session resumed"), "{err}");
    assert!(at(&t, "debug/deps/liblay.rlib").exists());
}

#[cfg(unix)]
#[test]
fn whole_reclaim_keeps_freed_bytes_when_its_final_name_cannot_be_inspected() {
    use std::os::unix::fs::PermissionsExt as _;

    struct RestoreSearch<'a>(&'a Path);
    impl Drop for RestoreSearch<'_> {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o700));
        }
    }

    let tmp = Tmp::new("whole-final-name-error");
    let t = std::fs::canonicalize(lay_target(&tmp.path().join("target"))).expect("canonical");
    let _restore = RestoreSearch(&t);
    let moved = tmp.path().join("target.emptied");
    let payload = at(&t, "debug/deps/liblay.rlib");
    let allocated = blocks(&payload);
    assert!(allocated > 0, "the removed file has counted allocation");
    let now = now_after_write(2);
    let replacement = b"the replacement is not a build directory";
    let mut swapped = false;
    let mut released = false;
    let done = reclaim_whole_with(
        &t,
        &judge(now),
        now,
        DAY,
        &mut || Ok(()),
        &mut |stage, at| {
            if stage == Stage::Removing && at == t {
                assert_eq!(names_in(&t), ["CACHEDIR.TAG"], "all else went first");
                std::fs::rename(&t, &moved).expect("move the emptied directory");
                std::fs::create_dir(&t).expect("replace its name with a directory");
                std::fs::write(t.join("keep.txt"), replacement).expect("the replacement payload");
                std::fs::set_permissions(&t, std::fs::Permissions::from_mode(0o000))
                    .expect("deny the final directory open");
                std::fs::read_dir(&t).expect_err("the replacement cannot be opened by this user");
                swapped = true;
            }
            if stage == Stage::Released {
                released = true;
            }
        },
    )
    .expect("a final name lookup error retains the already-counted removal");
    assert!(
        swapped && released,
        "the final boundary and release were observed"
    );
    assert!(done.bytes >= allocated, "{done:?}");
    assert!(done.units.is_empty(), "the named root did not go: {done:?}");
    assert!(
        !done.trouble.is_empty(),
        "the retained root is reported: {done:?}"
    );
    std::fs::set_permissions(&t, std::fs::Permissions::from_mode(0o700))
        .expect("restore the replacement for inspection and cleanup");
    assert_eq!(std::fs::read(t.join("keep.txt")).unwrap(), replacement);
    assert_eq!(
        names_in(&moved),
        ["CACHEDIR.TAG"],
        "the root that stays keeps its marker"
    );
}

/// The names of the entries of the directory at `dir`, sorted.
#[cfg(unix)]
fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("read")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// [`unhashed_twin`] matches only the unhashed stem (`-` as `_`) with the same extension.
#[test]
fn an_unhashed_twin_is_the_same_name_with_an_extension() {
    for (name, dep) in [
        ("aterm.exe", "aterm.exe"),
        ("aterm.pdb", "aterm.pdb"),
        ("aterm-agent.exe", "aterm_agent.exe"),
        ("aterm_agent.pdb", "aterm_agent.pdb"),
    ] {
        assert!(unhashed_twin(name, dep), "{name} {dep}");
    }
    for (name, dep) in [
        ("aterm.exe", "aterm-2bcb77ac5af7c69c.exe"),
        ("aterm.exe", "aterm.pdb"),
        ("aterm", "aterm"),
        (".exe", ".exe"),
        ("aterm.", "aterm."),
        ("aterm.exe", "aterm.exe.old"),
    ] {
        assert!(!unhashed_twin(name, dep), "{name} {dep}");
    }
}

/// [`fixed_verdict`] accepts only a local fixed disk volume.
#[test]
fn only_a_local_fixed_disk_passes_the_volume_fence() {
    assert_eq!(fixed_verdict(Some((true, Some(true)))), Ok(()));
    for (volume, why) in [
        (None, "no volume"),
        (Some((false, Some(true))), "not on a local disk volume"),
        (Some((false, None)), "not on a local disk volume"),
        (Some((true, Some(false))), "removable"),
        (Some((true, None)), "could not be read"),
    ] {
        let err = fixed_verdict(volume).expect_err("refused");
        assert!(err.contains(why), "{volume:?}: {err}");
    }
}

/// Lay a stage an earlier pass left at `folder/name` ([`STAGE`]): its tag and one 8 KiB file of
/// what it took. The stage's path.
#[cfg(unix)]
fn lay_stage(folder: &Path, name: &str) -> PathBuf {
    let stage = folder.join(name);
    std::fs::create_dir_all(&stage).expect("stage");
    std::fs::write(stage.join(STAGE_TAG), b"").expect("tag");
    std::fs::write(stage.join("taken.bin"), vec![5u8; 8192]).expect("taken");
    stage
}

/// A stage an earlier pass left goes only while its custody allows: a managed ancestor keeps it
/// whole, and a marker that lands in the build directory once its delete began stops that delete.
///
/// Negative control: with no marker anywhere the same stage is gone after one reclaim, and
/// without the custody read at the stage's own removal, phase 2's file would go.
#[cfg(unix)]
#[test]
fn a_stage_is_finished_only_while_custody_allows() {
    let now = now_after_write(STALE + 1);
    let marker = atpkg::build_cache::registered::MARKER;
    let tmp = Tmp::new("stage-custody-ancestor");
    let t = synthetic_target(&tmp, "target");
    let stage = lay_stage(&t.join("debug"), STAGE);
    std::fs::write(t.parent().unwrap().join(marker), br#"{"schema":3}"#).unwrap();
    reclaim(&t, &judge(now)).expect_err("a managed ancestor keeps the stage");
    assert_eq!(std::fs::read(stage.join("taken.bin")).unwrap(), [5u8; 8192]);
    assert!(stage.join(STAGE_TAG).exists());

    let tmp = Tmp::new("stage-custody-late");
    let t = synthetic_target(&tmp, "target");
    let stage = lay_stage(&t.join("debug"), STAGE);
    let mut landed = false;
    let error = reclaim_with(&t, &judge(now), &mut |step, at| {
        if step == Stage::Removing && at == stage.as_path() && !landed {
            landed = true;
            std::fs::write(t.join(marker), br#"{"schema":3}"#).unwrap();
        }
    })
    .expect_err("custody read again at the stage stops its delete");
    assert!(landed, "the stage's delete began");
    assert!(error.to_string().contains(MANAGED_CACHE), "{error}");
    assert_eq!(std::fs::read(stage.join("taken.bin")).unwrap(), [5u8; 8192]);

    let tmp = Tmp::new("stage-custody-none");
    let t = synthetic_target(&tmp, "target");
    let stage = lay_stage(&t.join("debug"), STAGE);
    let freed = blocks(&stage.join("taken.bin"));
    let done = reclaim(&t, &judge(now)).expect("an ordinary stage goes");
    assert!(!stage.exists(), "{done:?}");
    assert!(done.units.contains(&stage), "{done:?}");
    assert!(done.bytes >= freed, "{done:?}");
}

/// A stage an earlier pass left is never walked: a profile moved into it is no profile of the
/// build directory's, and its entries count against no walk's bound.
///
/// Negative control: the same tree in a folder of another name is found as a profile and cuts a
/// walk bounded at 20 entries.
#[cfg(unix)]
#[test]
fn a_stage_is_never_walked_as_a_profile_nor_kept_by_a_bound() {
    let now = now_after_write(STALE + 1);
    let bounded = WalkBudget {
        entries: 20,
        ..WalkBudget::SHIPPED
    };
    for (name, staged) in [(STAGE, true), ("aside", false)] {
        let tmp = Tmp::new("stage-walk");
        let t = synthetic_target(&tmp, "target");
        let folder = t.join(name);
        lay_profile(&folder.join("debug"));
        std::fs::write(folder.join(STAGE_TAG), b"").unwrap();
        for n in 0..50 {
            std::fs::write(folder.join(format!("taken-{n}")), b"x").unwrap();
        }
        let td = assessed(&t, now);
        let mut profiles: Vec<PathBuf> = td.profiles.iter().map(|p| p.path.clone()).collect();
        profiles.sort();
        let stages: Vec<PathBuf> = td.stages.iter().map(|s| s.path.clone()).collect();
        let cut = fd::walk(&t, bounded).cut;
        if staged {
            assert_eq!(profiles, vec![t.join("debug")]);
            assert_eq!(stages, vec![folder.clone()]);
            assert!(td.stages[0].bytes > 0, "{td:?}");
            assert!(cut.is_none(), "{cut:?}");
        } else {
            assert_eq!(profiles, vec![at(&t, "aside/debug"), t.join("debug")]);
            assert!(stages.is_empty(), "{td:?}");
            assert!(cut.is_some(), "the control walk is bounded");
        }
    }
}

/// The hook order of a pass that stages: what it takes is staged under the locks and deleted
/// once they are let go.
#[cfg(unix)]
const STAGED_ORDER: [Stage; 5] = [
    Stage::Locked,
    Stage::Staging,
    Stage::Staged,
    Stage::Released,
    Stage::Deleting,
];

/// The hook order of a pass that deletes in place, under the locks.
#[cfg(unix)]
const IN_PLACE_ORDER: [Stage; 4] = [
    Stage::Locked,
    Stage::Staging,
    Stage::Deleting,
    Stage::Released,
];

/// Every folder under `root` named as a stage ([`STAGE`]): none where `root` itself is gone.
#[cfg(unix)]
fn stages_in(root: &Path) -> Vec<PathBuf> {
    if !root.exists() {
        return Vec::new();
    }
    snapshot(root, &|_| false)
        .into_iter()
        .filter(|(rel, kind, _)| kind == "dir" && is_stage_path(rel))
        .map(|(rel, _, _)| root.join(rel))
        .collect()
}

/// The blocks of every file under `dir`.
#[cfg(unix)]
fn blocks_under(dir: &Path) -> u64 {
    snapshot(dir, &|_| false)
        .into_iter()
        .filter(|(_, kind, _)| kind == "file")
        .map(|(rel, _, _)| blocks(&dir.join(rel)))
        .sum()
}

/// The blocks of the files of the synthetic target `t` that `method` ([`ordinary_method`])
/// takes: `debug/incremental/` for the idle and the pressure reclaim, every file but the
/// `CACHEDIR.TAG` for the whole removal, and the four cargo outputs for tier 3.
#[cfg(unix)]
fn taken_by(method: usize, t: &Path) -> u64 {
    let outputs = [
        "debug/.fingerprint",
        "debug/build",
        "debug/deps",
        "debug/incremental",
    ];
    snapshot(t, &|_| false)
        .into_iter()
        .filter(|(rel, kind, _)| {
            kind == "file"
                && match method {
                    0 | 1 => rel.starts_with("debug/incremental"),
                    2 => rel != Path::new("CACHEDIR.TAG"),
                    _ => outputs.iter().any(|o| rel.starts_with(o)),
                }
        })
        .map(|(rel, _, _)| blocks(&t.join(rel)))
        .sum()
}

/// Whether the lock at `lock` exists and is held.
#[cfg(unix)]
fn held_if_there(lock: &Path) -> bool {
    lock.exists() && held(lock)
}

/// The exit test of DC-20: each of the four removers takes what it removes under cargo's
/// locks, moves it into a stage of its own, lets the locks go, and only then deletes the stage,
/// so a build that starts in that profile meanwhile takes its lock at once.
///
/// Negative controls: the probe at `Staged` finds the locks held, and the same probe at the
/// first removal, which here finds the lock let go wherever the pass moved it, finds it held in
/// `a_stage_that_cannot_be_made_deletes_in_place_under_the_locks`; a pass that deleted in place
/// would come to its first removal with `debug/.cargo-lock` held, where `Build::holds` panics.
#[cfg(unix)]
#[test]
fn a_disk_pass_lets_go_of_the_cargo_locks_before_it_deletes() {
    for method in 0..4 {
        let tmp = Tmp::new("lets-go");
        let t = synthetic_target(&tmp, "target");
        let debug = t.join("debug");
        let now = now_after_write(STALE + 1);
        let seen = assessed(&t, now).profiles[0].last_used.unwrap();
        let taken = taken_by(method, &t);
        let probe = if method == 2 {
            at(&t, ".aterm-removing/debug/.cargo-lock")
        } else {
            debug.join(".cargo-lock")
        };
        let mut order = Vec::new();
        let mut held_when_staged = None;
        let mut free_at_first_removal = None;
        let mut build: Option<Build> = None;
        let done = ordinary_method(method, &t, now, seen, &mut |stage, _| {
            if stage != Stage::Removing {
                if stage == Stage::Staged {
                    held_when_staged = Some(held(&probe));
                }
                order.push(stage);
                return;
            }
            if build.is_none() {
                free_at_first_removal = Some(!held(&probe));
                std::fs::create_dir_all(&debug).expect("a build's profile");
                let lock = debug.join(".cargo-lock");
                if !lock.exists() {
                    std::fs::write(&lock, b"").expect("its lock");
                }
                build = Some(Build::holds(&lock));
            }
        })
        .expect("reclaimed");
        assert_eq!(order, STAGED_ORDER, "method {method}");
        assert_eq!(held_when_staged, Some(true), "method {method}");
        assert_eq!(free_at_first_removal, Some(true), "method {method}");
        assert!(build.is_some(), "method {method}: a build took the lock");
        assert_eq!(done.bytes, taken, "method {method}: {done:?}");
        assert!(stages_in(&t).is_empty(), "method {method}: stage left");
        if method == 2 {
            assert!(t.join("CACHEDIR.TAG").exists());
            assert!(debug.join(".cargo-lock").exists());
            assert!(done.units.is_empty(), "{done:?}");
            assert!(
                done.skipped
                    .iter()
                    .any(|(_, why)| why.contains("a build started in it")),
                "{done:?}"
            );
        }
    }
}

/// A pass stopped once it staged and let go (killed; here a panic at its first removal) leaves
/// its stage tagged and let go, and the next pass finishes it and counts what it held.
///
/// Negative control: the same pass without the panic leaves no stage.
#[cfg(unix)]
#[test]
fn a_left_stage_is_finished_by_the_next_pass() {
    for method in 0..4 {
        for stop in [true, false] {
            let tmp = Tmp::new("left-stage");
            let t = synthetic_target(&tmp, "target");
            let now = now_after_write(STALE + 1);
            let seen = assessed(&t, now).profiles[0].last_used.unwrap();
            let first = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                ordinary_method(method, &t, now, seen, &mut |stage, _| {
                    assert!(
                        !(stop && stage == Stage::Removing),
                        "the pass stops at its first removal"
                    );
                })
            }));
            let stages = stages_in(&t);
            if !stop {
                first.expect("no stop").expect("reclaimed");
                assert!(stages.is_empty(), "method {method}: {stages:?}");
                continue;
            }
            assert!(first.is_err(), "method {method}: the pass stopped");
            assert_eq!(stages.len(), 1, "method {method}: {stages:?}");
            let stage = &stages[0];
            assert!(stage.join(STAGE_TAG).exists(), "method {method}");
            assert!(!held(&stage.join(STAGE_TAG)), "method {method}");
            let td = assessed(&t, now);
            assert_eq!(td.stages.len(), 1, "method {method}: {td:?}");
            assert!(
                matches!(
                    target_witness(&td, now, STALE),
                    Ok(Witness::StaleBuildDir { profiles: 0, .. })
                ),
                "method {method}: {td:?}"
            );
            let left = blocks_under(stage);
            let done = reclaim(&t, &judge(now)).expect("the next pass finishes it");
            assert_eq!(done.bytes, left, "method {method}: {done:?}");
            assert!(done.units.contains(stage), "method {method}: {done:?}");
            assert!(stages_in(&t).is_empty(), "method {method}");
        }
    }
}

/// Custody is read again once all that is taken sits in the stage: a marker that lands then
/// sends everything back where it was, and nothing goes.
///
/// Negative control: a plain file that lands then (`note.txt`) changes nothing, and what was
/// taken goes.
#[cfg(unix)]
#[test]
fn custody_read_again_at_the_stage_moves_everything_back() {
    let marker = atpkg::build_cache::registered::MARKER;
    for method in 0..4 {
        for (name, refuses) in [(marker, true), ("note.txt", false)] {
            let tmp = Tmp::new("stage-custody-again");
            let t = synthetic_target(&tmp, "target");
            let now = now_after_write(STALE + 1);
            let seen = assessed(&t, now).profiles[0].last_used.unwrap();
            let before = snapshot(&t, &|rel| rel == Path::new(name));
            // Where the lock is once the locks are let go: back in its profile, or (the whole
            // removal that goes on) in the stage with the profile it took.
            let lock = if method == 2 && !refuses {
                at(&t, ".aterm-removing/debug/.cargo-lock")
            } else {
                t.join("debug/.cargo-lock")
            };
            let mut landed = false;
            let mut let_go = None;
            let result = ordinary_method(method, &t, now, seen, &mut |stage, _| {
                if stage == Stage::Staged {
                    landed = true;
                    std::fs::write(t.join(name), br#"{"schema":3}"#).unwrap();
                }
                if stage == Stage::Released && let_go.is_none() {
                    let_go = Some(!held(&lock));
                }
            });
            assert!(landed, "method {method}");
            assert!(stages_in(&t).is_empty(), "method {method}: stage left");
            assert_eq!(let_go, Some(true), "method {method}: the locks are let go");
            if refuses {
                let text = match &result {
                    Ok(done) => done.trouble.join("; "),
                    Err(error) => error.to_string(),
                };
                assert!(text.contains(MANAGED_CACHE), "method {method}: {text}");
                assert_eq!(
                    snapshot(&t, &|rel| rel == Path::new(name)),
                    before,
                    "method {method}: everything is back"
                );
            } else {
                let done = result.expect("reclaimed");
                assert!(done.bytes > 0, "method {method}: {done:?}");
                assert!(!t.join("debug/incremental").exists(), "method {method}");
            }
        }
    }
}

/// A move into the stage that a mount point refuses (`EBUSY`: it would cross a volume) leaves
/// that entry where it was, deletes nothing of it in place, and the pass refuses.
///
/// Negative control: with the fault cleared the same pass reclaims.
#[cfg(unix)]
#[test]
fn a_move_that_would_cross_a_volume_leaves_the_tree_and_refuses() {
    for method in 0..4 {
        for fault in [Some(libc::EBUSY), None] {
            let tmp = Tmp::new("stage-xdev");
            let t = synthetic_target(&tmp, "target");
            let now = now_after_write(STALE + 1);
            let seen = assessed(&t, now).profiles[0].last_used.unwrap();
            let before = snapshot(&t, &|_| false);
            let case = format!("method {method}, {fault:?}");
            fd::inject_move_fault(fault.map(|errno| (0, errno)));
            let result = ordinary_method(method, &t, now, seen, &mut |_, _| {});
            fd::inject_move_fault(None);
            assert!(stages_in(&t).is_empty(), "{case}");
            if fault.is_some() {
                let error = result.expect_err("nothing is reclaimed");
                assert!(
                    error.to_string().contains("would cross a volume"),
                    "{case}: {error}"
                );
                assert_eq!(snapshot(&t, &|_| false), before, "{case}");
            } else {
                assert!(result.expect("reclaimed").bytes > 0, "{case}");
            }
        }
    }
}

/// Where no stage can be made (a full disk), a rename says none can be made here (`ENOSYS`, or
/// `EXDEV` between two names of one folder: an overlayfs lower directory), or a removal that
/// takes more than `incremental/` did not read what it would stage whole before the locks (its
/// scan stops at a bound), what is taken is deleted in place under the locks, as before stages:
/// the probe that finds the locks let go at a staged pass's first removal finds them held here.
///
/// Negative control: with no fault the order is the staged one and the locks are let go, as
/// they are where the scan's bound meets a removal that makes no scan (`incremental/` alone).
#[cfg(unix)]
#[test]
fn a_stage_that_cannot_be_made_deletes_in_place_under_the_locks() {
    let unread = WalkBudget {
        entries: 0,
        ..WalkBudget::SHIPPED
    };
    let faults = [
        (Some(libc::ENOSPC), None, None),
        (None, Some(libc::ENOSYS), None),
        (None, Some(libc::EXDEV), None),
        (None, None, Some(unread)),
        (None, None, None),
    ];
    for method in 0..4 {
        for (stage_fault, move_fault, scan) in faults {
            let tmp = Tmp::new("stage-cannot");
            let t = synthetic_target(&tmp, "target");
            let now = now_after_write(STALE + 1);
            let seen = assessed(&t, now).profiles[0].last_used.unwrap();
            let mut taken = taken_by(method, &t);
            if method == 2 {
                taken += blocks(&t.join("CACHEDIR.TAG"));
            }
            let lock = t.join("debug/.cargo-lock");
            let mut order = Vec::new();
            let mut held_at_removal = None;
            let mut staged_at_deleting = None;
            fd::inject_stage_fault(stage_fault);
            fd::inject_move_fault(move_fault.map(|errno| (0, errno)));
            fd::inject_scan_budget(scan);
            let result = ordinary_method(method, &t, now, seen, &mut |stage, _| {
                if stage == Stage::Removing {
                    if held_at_removal.is_none() {
                        held_at_removal = Some(held_if_there(&lock));
                    }
                    return;
                }
                if stage == Stage::Deleting && staged_at_deleting.is_none() {
                    staged_at_deleting = Some(!stages_in(&t).is_empty());
                }
                order.push(stage);
            });
            fd::inject_stage_fault(None);
            fd::inject_move_fault(None);
            fd::inject_scan_budget(None);
            let done = result.expect("reclaimed");
            let case = format!("method {method}, {stage_fault:?}, {move_fault:?}, {scan:?}");
            let unread = scan.is_some() && method >= 2;
            if stage_fault.is_none() && move_fault.is_none() && !unread {
                assert_eq!(order, STAGED_ORDER, "{case}");
                assert_eq!(held_at_removal, Some(false), "{case}");
                continue;
            }
            assert_eq!(order, IN_PLACE_ORDER, "{case}");
            assert_eq!(held_at_removal, Some(true), "{case}: held");
            assert_eq!(staged_at_deleting, Some(false), "{case}: no stage stands");
            assert_eq!(done.bytes, taken, "{case}: {done:?}");
            assert!(stages_in(&t).is_empty(), "{case}");
        }
    }
}

/// A folder with a volume mounted at or below it, by the mount table read before the locks, is
/// never moved: it is deleted in place under the locks, which never enters another device. So
/// is one whose mount table was not read whole, which rules no mount out.
///
/// Negative control: with a table read whole that holds no mount there (what a test's pass
/// reads unless it is given another) the same pass stages.
#[cfg(unix)]
#[test]
fn a_folder_with_a_volume_mounted_below_it_is_deleted_in_place_never_moved() {
    let cut = aterm_sysprobe::volumes::Table::default();
    assert!(!cut.complete, "a table not read whole");
    for (mounted, table) in [(true, None), (false, Some(cut)), (false, None)] {
        let tmp = Tmp::new("stage-mounted");
        let t = synthetic_target(&tmp, "target");
        let now = now_after_write(STALE + 1);
        let inc = t.join("debug").join(INCREMENTAL);
        let in_place = mounted || table.is_some();
        fd::pretend_mounted(mounted.then(|| inc.join("lay-1x2y3z")));
        fd::pretend_table(table);
        let mut order = Vec::new();
        let mut made = false;
        let result = reclaim_with(&t, &judge(now), &mut |stage, _| {
            made |= !stages_in(&t).is_empty();
            if stage != Stage::Removing {
                order.push(stage);
            }
        });
        fd::pretend_mounted(None);
        fd::pretend_table(None);
        result.expect("reclaimed");
        if in_place {
            assert_eq!(order, IN_PLACE_ORDER);
            assert!(!made, "no stage was made");
        } else {
            assert_eq!(order, STAGED_ORDER);
            assert!(made, "the control stages");
        }
        assert!(!inc.exists());
    }
}

/// A folder named as a stage that holds no tag but something else is no stage: no pass removes
/// it or what it holds, and a profile in it is walked (and so locked) as any other.
///
/// Negative control: an empty folder named as a stage (a pass killed before it made its tag)
/// is gone after one reclaim.
#[cfg(unix)]
#[test]
fn a_folder_named_as_a_stage_without_its_tag_is_never_removed() {
    let now = now_after_write(STALE + 1);
    let work = b"someone's work";
    for method in 0..4 {
        let tmp = Tmp::new("tagless-stage");
        let t = synthetic_target(&tmp, "target");
        let seen = assessed(&t, now).profiles[0].last_used.unwrap();
        let folder = if method == 2 {
            t.join(STAGE)
        } else {
            t.join("debug").join(STAGE)
        };
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("work.txt"), work).unwrap();
        assert!(assessed(&t, now).stages.is_empty(), "method {method}");
        let result = ordinary_method(method, &t, now, seen, &mut |_, _| {});
        assert_eq!(
            std::fs::read(folder.join("work.txt")).unwrap(),
            work,
            "method {method}"
        );
        assert!(assessed(&t, now).stages.is_empty(), "method {method}");
        if method == 2 {
            let done = result.expect("the rest went");
            assert!(t.join("CACHEDIR.TAG").exists());
            assert!(
                done.skipped
                    .iter()
                    .any(|(p, why)| *p == t && why.contains("more than its CACHEDIR.TAG")),
                "{done:?}"
            );
        } else {
            let done = result.expect("the idle cache goes");
            assert!(done.bytes > 0, "method {method}: {done:?}");
        }
    }

    let tmp = Tmp::new("tagless-profile");
    let t = synthetic_target(&tmp, "target");
    lay_profile(&at(&t, ".aterm-removing/x"));
    let found: Vec<PathBuf> = assessed(&t, now)
        .profiles
        .iter()
        .map(|p| p.path.clone())
        .collect();
    assert!(found.contains(&at(&t, ".aterm-removing/x")), "{found:?}");

    let tmp = Tmp::new("tagless-empty");
    let t = synthetic_target(&tmp, "target");
    let empty = at(&t, "debug/.aterm-removing-1");
    std::fs::create_dir(&empty).unwrap();
    assert_eq!(assessed(&t, now).stages.len(), 1);
    let done = reclaim(&t, &judge(now)).expect("reclaimed");
    assert!(!empty.exists());
    assert!(done.units.contains(&empty), "{done:?}");
}

/// A stage whose tag another pass holds is that pass's: a pass leaves it, and stages what it
/// takes beside it under the next free name.
///
/// Negative control: once the hold is let go, the next pass finishes the stage.
#[cfg(unix)]
#[test]
fn a_stage_its_pass_still_holds_is_left_to_it() {
    let tmp = Tmp::new("stage-held");
    let t = synthetic_target(&tmp, "target");
    let now = now_after_write(STALE + 1);
    let stage = lay_stage(&t.join("debug"), STAGE);
    let hold = take(&stage.join(STAGE_TAG));
    let mut beside = Vec::new();
    let done = reclaim_with(&t, &judge(now), &mut |step, _| {
        if step == Stage::Staged {
            beside = stages_in(&t);
        }
    })
    .expect("the idle cache still goes");
    assert_eq!(std::fs::read(stage.join("taken.bin")).unwrap(), [5u8; 8192]);
    assert!(
        done.skipped
            .iter()
            .any(|(p, why)| *p == stage && why.contains("its pass is still deleting it")),
        "{done:?}"
    );
    assert!(
        beside.contains(&at(&t, "debug/.aterm-removing-1")),
        "{beside:?}"
    );
    assert!(
        done.units.contains(&t.join("debug").join(INCREMENTAL)),
        "{done:?}"
    );
    assert!(!t.join("debug").join(INCREMENTAL).exists());

    drop(hold);
    let again = reclaim(&t, &judge(now)).expect("the stage goes once its pass lets go");
    assert!(!stage.exists());
    assert!(again.units.contains(&stage), "{again:?}");
}

/// Only the idle row finishes what an earlier pass left staged with no cargo lock of its own.
/// A pressure row, a whole removal and a tier-3 removal that refuse (here: a build holds a
/// lock) answer with the refusal alone and leave the stage as it was, and a whole removal that
/// goes finishes the stage at its root only once it let its own locks go.
///
/// Negative control: under the same held lock the idle reclaim finishes the stage and answers
/// with it. A remover that finished stages before its own checks would answer "removed" for a
/// row it refused, with no lock taken.
#[cfg(unix)]
#[test]
fn a_removal_that_refuses_finishes_no_stage_unless_it_is_the_idle_row() {
    let now = now_after_write(STALE + 1);
    for method in 0..4 {
        let tmp = Tmp::new("stage-refused");
        let t = synthetic_target(&tmp, "target");
        let seen = assessed(&t, now).profiles[0].last_used.unwrap();
        let stage = if method == 2 {
            lay_stage(&t, STAGE)
        } else {
            lay_stage(&t.join("debug"), STAGE)
        };
        let build = Build::holds(&t.join("debug/.cargo-lock"));
        let result = ordinary_method(method, &t, now, seen, &mut |_, _| {});
        drop(build);
        if method == 0 {
            let done = result.expect("the idle row finishes the stage");
            assert_eq!(done.units, vec![stage.clone()], "{done:?}");
            assert!(!stage.exists());
            assert!(
                done.skipped
                    .iter()
                    .any(|(_, why)| why.contains("a build holds")),
                "{done:?}"
            );
            continue;
        }
        let error = result.expect_err("refused");
        assert!(
            error.to_string().contains("a build holds"),
            "method {method}: {error}"
        );
        assert_eq!(
            std::fs::read(stage.join("taken.bin")).unwrap(),
            [5u8; 8192],
            "method {method}"
        );
        assert!(stage.join(STAGE_TAG).exists(), "method {method}");
    }

    let tmp = Tmp::new("stage-whole-after");
    let t = synthetic_target(&tmp, "target");
    let seen = assessed(&t, now).profiles[0].last_used.unwrap();
    let stage = lay_stage(&t, STAGE);
    let mut released = false;
    let mut finished_after = None;
    let done = reclaim_whole_with(
        &t,
        &judge(now),
        seen,
        DAY,
        &mut || Ok(()),
        &mut |step, at| {
            released |= step == Stage::Released;
            if step == Stage::Removing && at == stage.as_path() && finished_after.is_none() {
                finished_after = Some(released);
            }
        },
    )
    .expect("goes whole");
    assert_eq!(finished_after, Some(true), "after its locks: {done:?}");
    assert!(done.units.contains(&stage), "{done:?}");
    assert!(done.units.contains(&t), "{done:?}");
    assert!(!t.exists());
}

/// Where a rename says partway that nothing more can be staged here, what was not staged is
/// deleted in place while the locks are still held, and only the stage once they are let go:
/// each `Deleting` names what it deletes, the profile under its locks or the stage after them.
///
/// Negative control: with no fault nothing is removed under the lock. A pass that let go
/// before it deleted the rest in place would show a removal outside the stage with the lock
/// let go.
#[cfg(unix)]
#[test]
fn what_cannot_be_staged_partway_is_deleted_in_place_before_the_locks_go() {
    for fault in [Some((1, libc::ENOSYS)), None] {
        let tmp = Tmp::new("stage-partway");
        let t = synthetic_target(&tmp, "target");
        let now = now_after_write(STALE + 1);
        let seen = assessed(&t, now).profiles[0].last_used.unwrap();
        let taken = taken_by(3, &t);
        let lock = t.join("debug/.cargo-lock");
        let mut order = Vec::new();
        let mut removals = Vec::new();
        fd::inject_move_fault(fault);
        let result = ordinary_method(3, &t, now, seen, &mut |stage, at| {
            if stage == Stage::Removing {
                removals.push((at.ancestors().any(is_stage_path), held(&lock)));
            } else {
                order.push((stage, is_stage_path(at)));
            }
        });
        fd::inject_move_fault(None);
        let done = result.expect("reclaimed");
        assert_eq!(done.bytes, taken, "{fault:?}: {done:?}");
        assert!(stages_in(&t).is_empty(), "{fault:?}");
        let mut expected = vec![
            (Stage::Locked, false),
            (Stage::Staging, false),
            (Stage::Staged, false),
            (Stage::Released, false),
            (Stage::Deleting, true),
        ];
        if fault.is_none() {
            assert_eq!(order, expected, "(stage, names a stage)");
            assert!(
                removals.iter().all(|r| *r == (true, false)),
                "(in the stage, lock held): {removals:?}"
            );
            continue;
        }
        expected.insert(3, (Stage::Deleting, false));
        assert_eq!(order, expected, "(stage, names a stage)");
        assert!(removals.contains(&(false, true)), "{removals:?}");
        assert!(removals.contains(&(true, false)), "{removals:?}");
        assert!(
            removals.iter().all(|(staged, locked)| staged != locked),
            "in place only under the lock, the stage only after: {removals:?}"
        );
    }
}

/// A whole removal reads what it carried again at its stage, under the locks: a managed build
/// cache that landed in it after the scan made before the locks moves back with everything
/// else, and all of it then goes in place under the locks, a delete that stops at the cache.
///
/// Negative control: with no cache the same removal deletes its stage once the locks are let
/// go. Without the second read the stage would be deleted around the cache after the locks,
/// the cache out of its place the while.
#[cfg(unix)]
#[test]
fn a_whole_removal_reads_what_it_carried_again_at_the_stage() {
    let marker = atpkg::build_cache::registered::MARKER;
    for lands in [true, false] {
        let tmp = Tmp::new("stage-reread");
        let t = synthetic_target(&tmp, "target");
        let now = now_after_write(STALE + 1);
        let seen = assessed(&t, now).profiles[0].last_used.unwrap();
        let mut order = Vec::new();
        let result = reclaim_whole_with(
            &t,
            &judge(now),
            seen,
            DAY,
            &mut || Ok(()),
            &mut |stage, _| {
                if stage == Stage::Removing {
                    return;
                }
                if stage == Stage::Staged && lands {
                    let carried = at(&t, ".aterm-removing/debug/build/nested");
                    std::fs::create_dir(&carried).expect("a cache");
                    std::fs::write(carried.join(marker), b"held").expect("its marker");
                }
                order.push(stage);
            },
        );
        assert!(stages_in(&t).is_empty(), "lands {lands}");
        if !lands {
            result.expect("goes whole");
            assert_eq!(order, STAGED_ORDER);
            assert!(!t.exists());
            continue;
        }
        let text = match result {
            Ok(done) => done.trouble.join("; "),
            Err(error) => error.to_string(),
        };
        assert!(text.contains(MANAGED_CACHE), "{text}");
        assert_eq!(
            order,
            [
                Stage::Locked,
                Stage::Staging,
                Stage::Staged,
                Stage::Deleting,
                Stage::Released,
            ]
        );
        assert_eq!(
            std::fs::read(at(&t, "debug/build/nested").join(marker)).unwrap(),
            b"held",
            "the cache is back where it was, whole"
        );
    }
}

/// What moves back out of a stage never replaces an entry: where a fresh `incremental/` stands
/// in the profile by the time custody refuses what was staged, the staged one stays in its
/// stage, under its tag, and the fresh one is untouched.
///
/// Negative control: a move back by a plain rename would put the staged folder over the empty
/// one, which would then hold the query cache. With nothing standing there everything is back
/// and no stage is left (`custody_read_again_at_the_stage_moves_everything_back`).
#[cfg(unix)]
#[test]
fn a_move_back_never_replaces_what_stands_there() {
    let tmp = Tmp::new("stage-no-replace");
    let t = synthetic_target(&tmp, "target");
    let now = now_after_write(STALE + 1);
    let inc = t.join("debug").join(INCREMENTAL);
    let result = reclaim_with(&t, &judge(now), &mut |stage, _| {
        if stage == Stage::Staged {
            std::fs::create_dir(&inc).expect("a fresh incremental/");
            std::fs::write(
                t.join(atpkg::build_cache::registered::MARKER),
                br#"{"schema":3}"#,
            )
            .unwrap();
        }
    });
    let text = match result {
        Ok(done) => done.trouble.join("; "),
        Err(error) => error.to_string(),
    };
    assert!(text.contains("it stays staged"), "{text}");
    assert!(names_in(&inc).is_empty(), "the fresh folder is as it was");
    let stages = stages_in(&t);
    assert_eq!(stages.len(), 1, "{stages:?}");
    assert!(stages[0].join(STAGE_TAG).exists(), "it keeps its tag");
    let cache = at(
        &stages[0],
        "incremental/lay-1x2y3z/s-abc-def/query-cache.bin",
    );
    assert_eq!(std::fs::read(cache).unwrap(), [9u8; 8192]);
}

/// What the delete of a stage leaves goes back only under the profile's cargo locks: while a
/// build that started there holds one, what could not be deleted stays staged under its tag,
/// and a later pass finishes it.
///
/// Negative control: with no build the same stuck entry is back where it was and no stage
/// stands (`an_undeletable_entry_is_reported_and_its_siblings_still_go`).
#[cfg(unix)]
#[test]
fn what_a_delete_leaves_stays_staged_while_a_build_holds_the_profile() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = Tmp::new("stage-left-held");
    let t = synthetic_target(&tmp, "target");
    let debug = t.join("debug");
    let stuck = Stuck::at(&debug.join(INCREMENTAL).join("stuck"));
    let now = now_after_write(STALE + 1);
    let mut build: Option<Build> = None;
    let done = reclaim_with(&t, &judge(now), &mut |stage, _| {
        if stage == Stage::Removing && build.is_none() {
            build = Some(Build::holds(&debug.join(".cargo-lock")));
        }
    })
    .expect("partly reclaimed");
    drop(stuck);
    let stage = debug.join(STAGE);
    let staged = at(&stage, "incremental/stuck");
    assert!(staged.join("file").exists(), "{done:?}");
    assert!(
        !debug.join(INCREMENTAL).exists(),
        "nothing went back beside the build"
    );
    assert!(
        done.trouble.iter().any(|t| t.contains("stays staged")),
        "{done:?}"
    );
    assert!(stage.join(STAGE_TAG).exists());
    drop(build);
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let again = reclaim(&t, &judge(now)).expect("the next pass finishes the stage");
    assert_eq!(again.units, vec![stage.clone()], "{again:?}");
    assert!(!stage.exists());
}

/// A stage inside a build directory named inside this one is that directory's to finish: the
/// outer directory's reclaim leaves it, as it leaves that directory's profiles.
///
/// Negative control: the same reclaim told of no directory inside finishes both stages.
#[cfg(unix)]
#[test]
fn a_stage_in_a_build_directory_named_inside_is_left_to_it() {
    let now = now_after_write(STALE + 1);
    for named in [true, false] {
        let tmp = Tmp::new("stage-leave");
        let outer = synthetic_target(&tmp, "target");
        let inner = lay_target(&outer.join("nested/target"));
        let mine = lay_stage(&outer.join("debug"), STAGE);
        let theirs = lay_stage(&inner.join("debug"), STAGE);
        let leave = if named {
            vec![inner.clone()]
        } else {
            Vec::new()
        };
        let done = reclaim_leaving(&outer, &judge(now), &leave).expect("reclaimed");
        assert!(!mine.exists(), "named {named}: {done:?}");
        assert!(done.units.contains(&mine), "named {named}: {done:?}");
        assert_eq!(theirs.exists(), named, "{done:?}");
        assert_eq!(done.units.contains(&theirs), !named, "{done:?}");
    }
}

/// A build that starts in a build directory between a whole removal's last look at it and its
/// `rmdir` keeps the directory, with its `CACHEDIR.TAG` written back as it was.
///
/// Negative control: with no build starting there the directory goes whole.
#[cfg(unix)]
#[test]
fn a_build_that_starts_as_the_root_goes_gets_its_tag_back() {
    for starts in [true, false] {
        let tmp = Tmp::new("whole-tag-back");
        let t = synthetic_target(&tmp, "target");
        let now = now_after_write(STALE + 1);
        let seen = assessed(&t, now).profiles[0].last_used.unwrap();
        let tag = std::fs::read(t.join("CACHEDIR.TAG")).expect("tag");
        let done = reclaim_whole_with(
            &t,
            &judge(now),
            seen,
            DAY,
            &mut || Ok(()),
            &mut |stage, at| {
                if starts && stage == Stage::Removing && at == t {
                    std::fs::create_dir(t.join("debug")).expect("a build's profile");
                }
            },
        )
        .expect("the rest went");
        if !starts {
            assert_eq!(done.units, std::slice::from_ref(&t), "{done:?}");
            assert!(!t.exists());
            continue;
        }
        assert!(done.units.is_empty(), "{done:?}");
        assert!(
            done.skipped
                .iter()
                .any(|(p, why)| *p == t && why.contains("is written back")),
            "{done:?}"
        );
        assert_eq!(std::fs::read(t.join("CACHEDIR.TAG")).expect("tag"), tag);
        assert!(t.join("debug").is_dir());
    }
}

/// The folder or file at `path` dated `at` (unix seconds).
#[cfg(unix)]
fn dated(path: &Path, at: i64) {
    let when = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(u64::try_from(at).expect("after the epoch"));
    std::fs::File::open(path)
        .and_then(|f| f.set_modified(when))
        .expect("dated");
}

/// The compiler's own names say what a build that died left: an unfinished session
/// (`s-{stamp}-{random}-working`, base 36) and its lock, and a metadata temporary folder
/// (`rmeta` and six letters or digits); nothing else, a finished session included.
/// NEGATIVE CONTROL: a reader that took any `s-` folder hands back a finished session's lock
/// (`s-abc-def-0123abcd`), and one that took any `rmeta` prefix takes `rmeta1234567`; each
/// fails its `None`/`false` line.
#[test]
fn the_compilers_own_names_say_what_a_died_build_left() {
    assert_eq!(
        unfinished_session_lock("s-ab12-cd34-working").as_deref(),
        Some("s-ab12-cd34.lock")
    );
    for name in [
        "s-abc-def-0123abcd",
        "s-abc-def",
        "s--def-working",
        "s-abc--working",
        "s-ABC-def-working",
        "s-abc-de_f-working",
        "x-abc-def-working",
    ] {
        assert_eq!(unfinished_session_lock(name), None, "{name}");
    }
    assert!(metadata_temp("rmetaAb12Cd"));
    for name in [
        "rmeta1234567",
        "rmetaAb12C",
        "rmetaAb12C-",
        "Rmeta123456",
        "rmeta",
    ] {
        assert!(!metadata_temp(name), "{name}");
    }
}

/// A died build's output is read by its age and its lock: an old unfinished session whose lock
/// is free and an old metadata folder are listed, in `deps/` and in a build script's output
/// folder laid out by unit (`build/<unit>/out/`, where a probe such as autocfg compiles); a young
/// one of each ripens; a session whose lock a rustc holds is waited for and reads released once
/// it is let go; a finished session, an unfamiliar session name, a session with no lock file
/// and an `rmeta` file (no folder) are left.
/// NEGATIVE CONTROL: a reader without the lock probe lists the held session as a piece to take,
/// one without the age gate lists the young ones, and one that reads only
/// `build/<unit>/<run>/out/` misses the build script's; each fails the `partials` line.
#[cfg(unix)]
#[test]
fn a_died_build_is_read_by_age_and_lock_and_a_held_session_is_waited_for() {
    use std::os::unix::fs::MetadataExt as _;
    let tmp = Tmp::new("died-read");
    let target = synthetic_target(&tmp, "t");
    let profile = target.join("debug");
    let now = now_after_write(0) + 3_600;
    let krate = profile.join("incremental/lay-1x2y3z");
    let session = |name: &str, lock: bool, at: i64| {
        let dir = krate.join(format!("s-{name}-working"));
        std::fs::create_dir_all(&dir).expect("session");
        std::fs::write(dir.join("query-cache.bin"), vec![1u8; 4096]).expect("cache");
        if lock {
            std::fs::write(krate.join(format!("s-{name}.lock")), b"").expect("lock");
        }
        dated(&dir, at);
        dir
    };
    let old = session("old1-a1", true, now - 600);
    let young = session("young1-b2", true, now - 5);
    let held = session("held1-c3", true, now - 600);
    session("nolock1-d4", false, now - 600);
    let unknown = session("old1", true, now - 600);
    let finished = krate.join("s-abc-def-0123abcd");
    std::fs::create_dir_all(&finished).expect("finished");
    let metadata = |folder: &str, name: &str, at: i64| {
        let dir = profile.join(folder).join(name);
        std::fs::create_dir_all(&dir).expect("rmeta");
        std::fs::write(dir.join("lib.rmeta"), vec![2u8; 4096]).expect("rmeta file");
        dated(&dir, at);
        dir
    };
    let old_meta = metadata("deps", "rmetaAb12Cd", now - 600);
    let probe_meta = metadata("build/lay-0123456789abcdef/out", "rmetaPr0b3s", now - 600);
    metadata("deps", "rmetaYo12Ng", now - 30);
    std::fs::write(profile.join("deps/rmetaFi1E00"), b"a file").expect("rmeta file");
    let build = Build::holds(&krate.join("s-held1-c3.lock"));
    let dev = std::fs::symlink_metadata(&profile).expect("stat").dev();

    let died = died_output(&profile, dev, now);
    let mut partials: Vec<&Path> = died.partials.iter().map(|p| p.path.as_path()).collect();
    partials.sort();
    let mut want = vec![old.as_path(), old_meta.as_path(), probe_meta.as_path()];
    want.sort();
    assert_eq!(partials, want, "{died:?}");
    let piece = died.partials.iter().find(|p| p.path == old).expect("old");
    assert_eq!(piece.session_lock, Some(krate.join("s-old1-a1.lock")));
    assert_eq!(piece.modified, now - 600);
    assert!(piece.bytes >= 4096, "{piece:?}");
    assert_eq!(
        died.ripens,
        Some(now - 5 + DIED_SESSION_MIN_AGE_S),
        "{died:?}"
    );
    assert_eq!(
        died.held,
        vec![Held::Session(krate.join("s-held1-c3.lock"))]
    );
    assert!(!died.cut);
    assert!(!died.held[0].released(), "a rustc still holds it");
    drop(build);
    assert!(died.held[0].released(), "let go");
    assert!(young.exists() && held.exists() && finished.exists() && unknown.exists());
}

/// TIER 0's remover takes a died build's piece under the profile's locks and the session's own,
/// and puts its folder's times back, so the profile's clock reads what it read before: a held
/// session lock keeps the session; let go, the session goes and its lock file stays.
/// NEGATIVE CONTROL: a remover that skipped the session lock removes the held session (the
/// first `expect_err` fails), and one that did not put the times back leaves `deps/` dated
/// the removal (the `modified` line fails).
#[cfg(unix)]
#[test]
fn a_died_builds_piece_goes_under_its_locks_and_its_folders_times_stay() {
    let tmp = Tmp::new("died-reclaim");
    let target = synthetic_target(&tmp, "t");
    let profile = target.join("debug");
    let now = now_after_write(0) + 3_600;
    let judge = Judge {
        now,
        threshold_days: 7,
        device: None,
        volume: None,
    };
    let meta = profile.join("deps/rmetaAb12Cd");
    std::fs::create_dir_all(&meta).expect("rmeta");
    std::fs::write(meta.join("lib.rmeta"), vec![2u8; 8192]).expect("rmeta file");
    dated(&meta, now - 600);
    let krate = profile.join("incremental/lay-1x2y3z");
    let session = krate.join("s-old1-died1-working");
    std::fs::create_dir_all(&session).expect("session");
    std::fs::write(session.join("query-cache.bin"), vec![1u8; 8192]).expect("cache");
    let lock = krate.join("s-old1-died1.lock");
    std::fs::write(&lock, b"").expect("lock");
    dated(&session, now - 600);
    for dir in [profile.join("deps"), krate.clone()] {
        dated(&dir, now - 7_200);
    }
    let modified = |p: &Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .expect("mtime")
    };
    let (deps_at, krate_at) = (modified(&profile.join("deps")), modified(&krate));

    let mut stages = Vec::new();
    let went = reclaim_died_with(
        &target,
        &profile,
        &meta,
        &judge,
        now - 600,
        &mut || Ok(()),
        &mut |stage, at| stages.push((stage, at.to_path_buf())),
    )
    .expect("the metadata folder goes");
    assert_eq!(went.units, vec![meta.clone()]);
    assert!(went.bytes >= 8192, "{went:?}");
    assert!(went.trouble.is_empty(), "{went:?}");
    assert!(!meta.exists());
    let told: Vec<Stage> = stages
        .iter()
        .map(|(s, _)| *s)
        .filter(|s| *s != Stage::Removing)
        .collect();
    assert_eq!(
        told,
        [Stage::Locked, Stage::Deleting, Stage::Released],
        "{stages:?}"
    );
    assert_eq!(
        modified(&profile.join("deps")),
        deps_at,
        "deps/ dated as before"
    );

    let build = Build::holds(&lock);
    let why = reclaim_died(&target, &profile, &session, &judge, now - 600, &mut || {
        Ok(())
    })
    .expect_err("a rustc holds its session");
    assert!(why.to_string().contains("session lock"), "{why}");
    assert!(session.exists());
    drop(build);
    let refused = reclaim_died(&target, &profile, &session, &judge, now - 601, &mut || {
        Ok(())
    })
    .expect_err("written into since the survey");
    assert!(
        refused.to_string().contains("since the survey"),
        "{refused}"
    );
    let went = reclaim_died(&target, &profile, &session, &judge, now - 600, &mut || {
        Ok(())
    })
    .expect("let go, the session goes");
    assert_eq!(went.units, vec![session.clone()]);
    assert!(!session.exists());
    assert!(
        lock.exists(),
        "rustc's own collection removes a lock with no session"
    );
    assert_eq!(
        modified(&krate),
        krate_at,
        "its crate folder dated as before"
    );
    let finished = krate.join("s-abc-def-0123abcd");
    std::fs::create_dir_all(&finished).expect("finished");
    reclaim_died(&target, &profile, &finished, &judge, now, &mut || Ok(()))
        .expect_err("a finished session is no piece of a died build");
    assert!(finished.exists());
}

/// A whole removal walks what it staged, under the locks, for a profile it did not lock: one
/// a build started after the walk made under the locks (here, between that walk and the
/// stage) moves back with everything else, and nothing goes.
///
/// Negative control: with no new profile the same removal goes whole. Without the walk at the
/// stage, `release/` would be deleted with the rest once the locks were let go, its build's
/// lock held the while.
#[cfg(unix)]
#[test]
fn a_profile_a_build_starts_after_the_locks_moves_back_and_nothing_goes() {
    let now = now_after_write(2);
    for starts in [true, false] {
        let tmp = Tmp::new("whole-late-profile");
        let t = std::fs::canonicalize(lay_target(&tmp.path().join("target"))).expect("canonical");
        let mut build: Option<Build> = None;
        let result = reclaim_whole_with(
            &t,
            &judge(now),
            now,
            DAY,
            &mut || Ok(()),
            &mut |stage, _| {
                if stage == Stage::Staging && starts {
                    lay_profile(&t.join("release"));
                    build = Some(Build::holds(&at(&t, "release/.cargo-lock")));
                }
            },
        );
        if !starts {
            result.expect("goes whole");
            assert!(!t.exists());
            continue;
        }
        let error = result.expect_err("nothing goes");
        assert!(error.to_string().contains("did not lock"), "{error}");
        assert!(at(&t, "release/deps/liblay.rlib").exists(), "{error}");
        assert!(at(&t, "debug/deps/liblay.rlib").exists(), "{error}");
        assert!(stages_in(&t).is_empty(), "{error}");
        drop(build);
    }
}

/// What a stage's finish cannot delete goes back where it was once its folder is quiet, and
/// the stage goes; while a build holds the profile it stays staged, under its tag, for a later
/// pass; where a build laid its own in its place, it stays in the stage, which loses its tag,
/// so no pass counts or finishes it again.
///
/// Negative control: a finish that only deletes keeps, in every case, a tagged stage that each
/// idle pass counts and fails on again.
#[cfg(unix)]
#[test]
fn what_a_stage_finish_leaves_goes_back_or_stops_being_a_stage() {
    use std::os::unix::fs::PermissionsExt as _;
    let now = now_after_write(STALE + 1);
    for case in ["quiet", "held", "taken"] {
        let tmp = Tmp::new("stage-settle");
        let t = synthetic_target(&tmp, "target");
        let debug = t.join("debug");
        if case != "taken" {
            std::fs::remove_dir_all(debug.join(INCREMENTAL)).expect("its name free");
        }
        let stage = lay_stage(&debug, STAGE);
        let stuck = Stuck::at(&stage.join(INCREMENTAL).join("stuck"));
        let build = (case == "held").then(|| Build::holds(&debug.join(".cargo-lock")));
        let result = reclaim(&t, &judge(now));
        drop(build);
        drop(stuck);
        let back = debug.join(INCREMENTAL).join("stuck");
        let _ = std::fs::set_permissions(&back, std::fs::Permissions::from_mode(0o755));
        let text = match &result {
            Ok(done) => done.trouble.join("; "),
            Err(error) => error.to_string(),
        };
        let staged = stage.join(INCREMENTAL).join("stuck/file");
        match case {
            "quiet" => {
                assert!(back.join("file").exists(), "{case}: {text}");
                assert!(stages_in(&t).is_empty(), "{case}: {text}");
            }
            "held" => {
                assert!(staged.exists(), "{case}: {text}");
                assert!(stage.join(STAGE_TAG).exists(), "{case}: {text}");
                assert!(text.contains("a later pass finishes it"), "{case}: {text}");
                assert_eq!(assessed(&t, now).stages.len(), 1, "{case}: {text}");
            }
            _ => {
                assert!(staged.exists(), "{case}: {text}");
                assert!(!stage.join(STAGE_TAG).exists(), "{case}: {text}");
                assert!(text.contains("no longer a stage"), "{case}: {text}");
                assert!(assessed(&t, now).stages.is_empty(), "{case}: {text}");
            }
        }
    }
}

/// The scan a removal makes of what it would stage passes over a profile's `incremental/`,
/// which rustc alone writes: a cache past the scan's bound still stages, so the largest
/// removals are the ones that let the locks go first.
///
/// Negative control: the same entries in `deps/`, which the scan reads, run it into its bound,
/// and what is taken is deleted in place under the locks.
#[cfg(unix)]
#[test]
fn a_profiles_incremental_is_never_scanned_before_it_is_staged() {
    let bounded = WalkBudget {
        entries: 40,
        ..WalkBudget::SHIPPED
    };
    for method in [2, 3] {
        for folder in [INCREMENTAL, "deps"] {
            let tmp = Tmp::new("stage-scan-inc");
            let t = synthetic_target(&tmp, "target");
            let now = now_after_write(STALE + 1);
            let seen = assessed(&t, now).profiles[0].last_used.unwrap();
            let many = t.join("debug").join(folder).join("many");
            std::fs::create_dir_all(&many).expect("many");
            for n in 0..100 {
                std::fs::write(many.join(format!("e{n}")), b"x").expect("entry");
            }
            let mut order = Vec::new();
            fd::inject_scan_budget(Some(bounded));
            let result = ordinary_method(method, &t, now, seen, &mut |stage, _| {
                if stage != Stage::Removing {
                    order.push(stage);
                }
            });
            fd::inject_scan_budget(None);
            result.expect("reclaimed");
            let case = format!("method {method}, {folder}");
            if folder == INCREMENTAL {
                assert_eq!(order, STAGED_ORDER, "{case}");
            } else {
                assert_eq!(order, IN_PLACE_ORDER, "{case}");
            }
        }
    }
}

#[cfg(windows)]
#[path = "disk_target_win_tests.rs"]
mod win;
