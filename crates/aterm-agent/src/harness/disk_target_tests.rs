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
        let (go_r, go_w) = std::io::pipe().expect("pipe");
        let (go_fd, go_w_fd) = (go_r.as_raw_fd(), go_w.as_raw_fd());
        // SAFETY: the child calls only async-signal-safe functions (`close`, `read`, `_exit`,
        // and errno) on descriptors opened before `fork`, then exits without unwinding.
        let pid = unsafe { libc::fork() };
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

/// Every lock is held from before the delete until after it, then released.
#[test]
fn every_lock_is_held_for_the_whole_delete_and_released_after() {
    let tmp = Tmp::new("locks");
    let t = synthetic_target(&tmp, "target");
    let debug = t.join("debug");
    let now = now_after_write(STALE + 1);
    let mut seen = Vec::new();
    reclaim_with(&t, &judge(now), &mut |stage, _| {
        let all = CARGO_LOCKS.iter().all(|l| held(&debug.join(l)));
        let inc = debug.join(INCREMENTAL).exists();
        seen.push((stage, all, inc));
    })
    .expect("reclaimed");
    assert_eq!(
        seen,
        vec![
            (Stage::Locked, true, true),
            (Stage::Deleting, true, true),
            (Stage::Released, false, false),
        ]
    );
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
            (Stage::Deleting, true, true),
            (Stage::Released, false, true),
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
        if stage == Stage::Deleting {
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

/// Deletion never crosses onto another device or volume.
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

/// An undeletable entry is reported and uncounted while its siblings are still removed.
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

/// Partial deletion updating `incremental/`'s mtime leaves the profile idle for the next reclaim.
#[test]
fn a_reclaim_cut_short_leaves_the_profile_idle_for_the_next() {
    let tmp = Tmp::new("cut-short");
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

    let done = reclaim(&t, &judge(now)).expect("partly reclaimed");
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
    let done = reclaim_lru_with(&t, &debug, &judge(now), now - 3 * 3_600, &mut |stage, p| {
        let all = CARGO_LOCKS.iter().all(|l| held(&p.join(l)));
        seen.push((stage, all));
    })
    .expect("reclaimed");
    assert_eq!(done.units, vec![debug.join(INCREMENTAL)], "{done:?}");
    assert!(
        done.skipped.is_empty() && done.trouble.is_empty(),
        "{done:?}"
    );
    assert_eq!(
        seen,
        vec![
            (Stage::Locked, true),
            (Stage::Deleting, true),
            (Stage::Released, false)
        ]
    );
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
        if stage == Stage::Deleting {
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

#[cfg(windows)]
#[path = "disk_target_win_tests.rs"]
mod win;
