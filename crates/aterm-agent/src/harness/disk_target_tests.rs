// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the build-directory reclaim: what a profile is, when it is idle, and that only an
//! idle profile's `incremental/` is removed, under every one of cargo's locks.
//!
//! Fixtures are laid out as cargo lays a build directory ([`lay_target`]: locks in each
//! profile, none at the root), each in its own scratch directory; no test touches the owner's
//! files or reads the process table.

use super::*;
use crate::harness::disk::DEFAULT_TARGET_STALE_DAYS as STALE;
use crate::harness::disk::tests::{
    Tmp, lay_profile, lay_target, now_after_write, synthetic_target,
};

const DAY: i64 = 86_400;

/// An hour before the stale window closes at `now`: a profile last written then is still in
/// use.
fn inside_window(now: i64) -> i64 {
    now - (STALE * DAY - 3_600)
}

/// The judge every test reclaims under: `now`, the default stale window, any
/// device.
fn judge(now: i64) -> Judge {
    Judge {
        now,
        threshold_days: STALE,
        device: None,
        volume: None,
    }
}

/// One build directory assessed with the shipped bounds.
fn assessed(path: &Path, now: i64) -> TargetDir {
    assess(path, now, STALE, WalkBudget::SHIPPED).expect("a directory")
}

/// `rel` (`/`-separated) under `base`, one component at a time: under a verbatim `\\?\` base
/// (what `canonicalize` answers on Windows) a `/` is a character of a name, not a separator.
fn at(base: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(base.to_path_buf(), |p, c| p.join(c))
}

/// The bytes the blocks of the file at `p` take.
#[cfg(unix)]
fn blocks(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::symlink_metadata(p).expect("stat").blocks() * 512
}

/// The bytes the file at `p` holds allocated (its `FILE_STANDARD_INFO` allocation, read
/// through its handle): what deleting its last name releases.
#[cfg(windows)]
fn blocks(p: &Path) -> u64 {
    let f = std::fs::File::open(p).expect("open");
    aterm_dirfd::fstat(&f).expect("stat").allocation
}

/// Set the mtime of the directory or file at `path` to `at` (unix seconds).
fn set_mtime(path: &Path, at: i64) {
    let when = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(u64::try_from(at).expect("after the epoch"));
    #[cfg(unix)]
    let file = std::fs::File::open(path);
    // A directory opens only with backup semantics, and a time is written with
    // `FILE_WRITE_ATTRIBUTES` alone.
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

/// `p` without its verbatim `\\?\` prefix: what `cmd` takes as a working directory or a
/// `mklink` operand.
#[cfg(windows)]
fn plain(p: &Path) -> String {
    let text = p.to_str().expect("test paths are UTF-8");
    text.strip_prefix(r"\\?\").unwrap_or(text).to_owned()
}

/// A directory link at `link` to the directory `target`: a symlink on unix, a junction on
/// Windows (`cmd /C mklink /J`, which needs no privilege; it refuses the verbatim spelling
/// `canonicalize` answers, so the plain one is passed).
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

/// A file under `dir` (made here, 8 KiB) that no delete can take until this is dropped: on
/// unix its directory made read-only (`0555`); on Windows the file held open without delete
/// sharing, which refuses a delete of it with a sharing violation.
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

/// Take `lock` exclusively, as a build would, through [`HeldLock::try_take`] so the release
/// is by `LOCK_UN` too.
fn take(lock: &Path) -> HeldLock {
    let f = std::fs::File::open(lock).expect("the lock exists");
    HeldLock::try_take(f)
        .expect("the lock can be taken")
        .expect("the test takes the lock")
}

/// Is `lock` held by someone right now? The probe releases by `LOCK_UN` ([`HeldLock`]): by
/// close alone it would stay held in a child a sibling test is spawning.
fn held(lock: &Path) -> bool {
    let f = std::fs::File::open(lock).expect("lock");
    !matches!(HeldLock::try_take(f), Ok(Some(_)))
}

/// A child forked from another thread and kept short of its exec, holding a copy of every
/// descriptor open at the fork, as every child does until its exec. [`Parked::fork`] returns
/// once the child exists; it execs only at [`Parked::exec`] (a handshake over two pipes).
#[cfg(unix)]
struct Parked {
    spawner: std::thread::JoinHandle<std::io::Result<std::process::ExitStatus>>,
    go: std::io::PipeWriter,
}

#[cfg(unix)]
impl Parked {
    fn fork() -> Parked {
        use std::io::Read as _;
        use std::os::fd::AsRawFd as _;
        use std::os::unix::process::CommandExt as _;
        // `forked`: the child says it exists. `go`: the test lets it exec.
        let (mut forked_r, forked_w) = std::io::pipe().expect("pipe");
        let (go_r, go_w) = std::io::pipe().expect("pipe");
        let (forked_fd, go_fd, go_w_fd) =
            (forked_w.as_raw_fd(), go_r.as_raw_fd(), go_w.as_raw_fd());
        let spawner = std::thread::spawn(move || {
            let mut cmd = std::process::Command::new("/bin/sh");
            cmd.args(["-c", "exit 0"]);
            // SAFETY: the hook runs in the forked child before exec and calls
            // only async-signal-safe functions (close, write, read, and errno)
            // on descriptors opened before the spawn. It drops its own copy of
            // `go`'s write end first, so the test's end closing — a panic
            // included — lets it exec rather than wait.
            unsafe {
                cmd.pre_exec(move || {
                    let interrupted = || {
                        std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                    };
                    libc::close(go_w_fd);
                    if libc::write(forked_fd, b"f".as_ptr().cast(), 1) != 1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    let mut b = 0u8;
                    while libc::read(go_fd, (&raw mut b).cast(), 1) < 0 {
                        if !interrupted() {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                    Ok(())
                });
            }
            let status = cmd.status();
            // A child that failed before its byte: the read below ends.
            drop((forked_w, go_r));
            status
        });
        let mut b = [0u8; 1];
        forked_r.read_exact(&mut b).expect("the child forked");
        Parked { spawner, go: go_w }
    }

    /// Has the child yet to exec? The spawn returns only at its exec, which
    /// waits for `go`.
    fn pending(&self) -> bool {
        !self.spawner.is_finished()
    }

    /// Let the child exec; its exit status.
    fn exec(mut self) -> std::process::ExitStatus {
        use std::io::Write as _;
        self.go.write_all(b"g").expect("let the child exec");
        self.spawner
            .join()
            .expect("the spawner")
            .expect("the child ran")
    }
}

/// A build directory with everything a real one also holds and must keep:
/// the uplifted binary `debug/lay` (a hard link of its `deps/` twin, as cargo
/// uplifts), `debug/lay.d`, `debug/examples/ex`, a criterion baseline, a
/// `doc/` page, and a link an uplift left (`debug/lay.dSYM`: on unix a symlink
/// to the twin; on Windows, where a file symlink needs a privilege, a junction
/// to `deps/`).
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

/// Every entry under `root` — its path relative to `root`, its kind, and its
/// bytes (a file's content, a link's text) — except what lies under a path
/// `skip` names. What "byte-identical" is checked against.
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
                // A Windows lock is mandatory: a lock file a test's build holds
                // cannot be read through another handle, so its length stands
                // for its bytes (cargo's are empty).
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

/// An idle profile loses only its `incremental/`: every other entry of a furnished build
/// directory is byte-identical afterwards, and the bytes counted are the blocks released.
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

    // Reclaimed, it is no longer a candidate: nothing is left to take.
    let again = assessed(&t, now);
    let why = target_witness(&again, now, STALE).expect_err("nothing to reclaim");
    assert!(
        why.contains("no idle profile holds an incremental/"),
        "{why}"
    );
    let err = reclaim(&t, &judge(now)).expect_err("nothing to reclaim");
    assert!(err.to_string().contains("nothing reclaimed"), "{err}");
}

/// Profiles are found at any depth (`debug`, `<triple>/release`,
/// `conformance-release/release`) and each is judged alone: one written inside the window
/// keeps its `incremental/`.
#[test]
fn profiles_are_found_at_any_depth_and_each_is_judged_alone() {
    let tmp = Tmp::new("nested");
    let t = synthetic_target(&tmp, "target");
    lay_profile(&at(&t, "aarch64-apple-darwin/release"));
    lay_profile(&at(&t, "conformance-release/release"));
    let now = now_after_write(STALE + 1);
    // The triple's profile was compiled into inside the window (as `now`
    // reads it).
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
    // Control: the shipped bounds read it all.
    assert!(target_witness(&assessed(&t, now), now, STALE).is_ok());
}

/// A size bounded by a deadline that has passed reads nothing more and says it was cut
/// short; one whose deadline is far off is the whole size, as [`total_bytes`] gives it.
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

/// The idle clock is [`CLOCK_DIRS`], never a lock's or the profile directory's: a fresh lock
/// or profile directory is not a use; a fresh `deps/` or `examples/` is.
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
    // A fresh lock and a fresh profile directory: not a use.
    set_mtime(&debug.join(".cargo-lock"), now);
    set_mtime(&debug, now);
    assert!(target_witness(&assessed(&t, now), now, STALE).is_ok());
    // A fresh deps/ (a compile): in use, whatever the lock says.
    set_mtime(&debug.join(".cargo-lock"), old);
    set_mtime(&debug.join("deps"), inside_window(now));
    let why = target_witness(&assessed(&t, now), now, STALE).expect_err("in use");
    assert!(
        why.contains(&format!("written into {}d ago", STALE - 1)),
        "{why}"
    );
    // A fresh examples/ alone: in use too.
    set_mtime(&debug.join("deps"), old);
    set_mtime(&debug.join("examples"), inside_window(now));
    assert!(target_witness(&assessed(&t, now), now, STALE).is_err());
}

/// A build holding any one of cargo's locks keeps its profile, in the report and in the
/// reclaim, while an idle sibling profile is still reclaimed.
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

/// The reclaim's locks are free once it is done, though a child forked under them has yet to
/// exec ([`Parked`]). Negative control: with [`HeldLock`]'s drop reduced to a plain close,
/// every lock reads held at `Released`.
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

/// The report's probe ([`locks_held`]) releases each lock by `LOCK_UN`, so a child forked
/// under it ([`Parked`]) does not keep it held. Negative control: with the probe releasing by
/// close alone, each lock reads held after the probe and the reclaim fails.
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
        // The hook runs while the probe holds `lock`.
        under.push(held(lock));
        parked.push(Parked::fork());
    });
    // Everything read while every child is parked short of its exec.
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

/// A profile whose clock could not be read is refused by both passes with that reason, never
/// dated "written into 0d ago".
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
    // The dated arms: past the window it is idle, inside it the days are said, and at a
    // 0-day window the ten minutes are.
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

/// A missing lock is created only when the removal proceeds (an older cargo's profile holds
/// only `.cargo-lock`), never while a build holds one, and a failure to create one (ENOSPC,
/// a read-only profile) skips the profile whole.
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

    // The report creates nothing.
    let t = old_cargo(&tmp, "report");
    assert!(target_witness(&assessed(&t, now), now, STALE).is_ok());
    assert_eq!(only(&t), 1, "the report wrote no lock");
    // The reclaim creates the missing two, and holds all three.
    let mut all_held = false;
    reclaim_with(&t, &judge(now), &mut |stage, _| {
        if stage == Stage::Deleting {
            all_held = CARGO_LOCKS.iter().all(|l| held(&t.join("debug").join(l)));
        }
    })
    .expect("reclaimed");
    assert!(all_held);
    assert_eq!(only(&t), 3, "created now that the removal proceeds");

    // `.cargo-lock` held: skipped, and nothing created.
    let t = old_cargo(&tmp, "held");
    let hold = take(&at(&t, "debug/.cargo-lock"));
    let err = reclaim(&t, &judge(now)).expect_err("held");
    assert!(err.to_string().contains("a build holds"), "{err}");
    assert_eq!(only(&t), 1, "nothing created while a build holds one");
    drop(hold);

    // A full disk: the missing lock cannot be created — skipped.
    let t = old_cargo(&tmp, "full");
    inject_create_lock_fault(true);
    let err = reclaim(&t, &judge(now)).expect_err("cannot lock");
    inject_create_lock_fault(false);
    assert!(err.to_string().contains("could not be created"), "{err}");
    assert!(t.join("debug").join(INCREMENTAL).exists(), "cache intact");

    // A read-only profile: the same (unix: a Windows directory's read-only
    // attribute does not keep a file from being created in it).
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

/// Nothing is reached through a symlink: a path component swapped for a link refuses the
/// reclaim; a linked `incremental` is no candidate; a link inside `incremental/` is unlinked,
/// its target kept; a profile swapped for a link under the locks redirects nothing.
#[test]
fn nothing_is_reached_through_a_symlink() {
    let tmp = Tmp::new("links");
    let now = now_after_write(STALE + 1);
    let victim = tmp.path().join("victim");
    lay_target(&victim);
    let victim_before = snapshot(&victim, &|_| false);

    // 1. A path component swapped for a link after the survey.
    let parent = tmp.path().join("repo");
    let t = lay_target(&parent.join("target"));
    let t = std::fs::canonicalize(&t).expect("canonical");
    assert!(target_witness(&assessed(&t, now), now, STALE).is_ok());
    let parent = std::fs::canonicalize(&parent).expect("canonical");
    std::fs::rename(&parent, tmp.path().join("repo.moved")).expect("move");
    link_dir(&victim, &parent);
    // `victim` holds a `target/`? It does not; give the link one to find.
    std::fs::create_dir_all(victim.join("target")).expect("decoy");
    lay_target(&victim.join("target"));
    let decoy_before = snapshot(&victim.join("target"), &|_| false);
    let err = reclaim(&t, &judge(now)).expect_err("a link in the path");
    assert!(err.to_string().contains("symlink"), "{err}");
    // A link is said as one, never as a cloud sync root (Windows says that of a
    // reparse point that is no link alone).
    assert!(!err.to_string().contains(IN_SYNC_ROOT), "{err}");
    assert_eq!(snapshot(&victim.join("target"), &|_| false), decoy_before);
    std::fs::remove_dir_all(victim.join("target")).expect("decoy away");

    // 2. `incremental` itself a link: never followed, never a candidate.
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

    // 3. A link inside incremental/: the link goes, its target stays.
    let t = synthetic_target(&tmp, "inner-link");
    link_dir(&victim, &at(&t, "debug/incremental/escape"));
    reclaim(&t, &judge(now)).expect("reclaimed");
    assert!(!at(&t, "debug/incremental").exists());

    // 4. The profile swapped for a link under the locks. On Windows the swap
    // itself is refused: NTFS renames no directory with a handle open beneath
    // it, and the reclaim holds the profile and its locks open.
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

/// The delete never enters a directory on another device (reported, uncounted), and a build
/// directory on another volume than the one measured is refused.
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

    // A build directory on another volume than the one measured is refused.
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

/// A VOLUME'S GUID IS ITS OWN NAME, AND THE FENCE'S SECOND READING HOLDS TO IT
/// ([`same_volume`], pure): a GUID reads in any case and is spelled back as the mount manager
/// spells it; only a Windows volume's key carries one; the measured GUID and the one read
/// through the build directory's handle must be one, and either unread refuses. NEGATIVE
/// CONTROLS: every malformed spelling reads as none, and two GUIDs one bit apart differ.
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

/// The bytes counted are what the unlinks released: a file hard-linked into `deps/` counts
/// nothing, a sparse file its blocks (unix; on Windows a file extended without the sparse flag
/// is allocated whole); the report's estimate and the reclaim's count agree.
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

/// An undeletable entry is reported and uncounted, its siblings deleted and counted, and the
/// non-empty `incremental/` not claimed as gone.
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

/// A source tree is never a candidate: a directory holding a `Cargo.toml`
/// or `src/` at its root is not build output, whatever it holds.
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

/// A reclaim cut short leaves the profile idle: its unlinks write `incremental/`'s mtime,
/// which is not part of the idle clock, so the next reclaim takes what is left.
#[test]
fn a_reclaim_cut_short_leaves_the_profile_idle_for_the_next() {
    let tmp = Tmp::new("cut-short");
    let t = synthetic_target(&tmp, "target");
    let debug = t.join("debug");
    let inc = debug.join(INCREMENTAL);
    let stuck = inc.join("stuck");
    let held = Stuck::at(&stuck);
    // Judged at the real time, with every directory last written a month ago.
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

/// A lock the reclaim creates gets the mode cargo's `OpenOptions` gives it (`0666` under the
/// umask), never a private `0600` that locks out a shared build directory's group.
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
    // What cargo's `OpenOptions::new().read(true).write(true).create(true)`
    // makes under this process's umask.
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

/// An idle profile with an empty `incremental/` (a release or `CARGO_INCREMENTAL=0` build) is
/// not a candidate and gets no lock, while an idle sibling is reclaimed.
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

    // Alone, it makes the directory no candidate at all.
    std::fs::remove_dir_all(t.join("debug")).expect("only release left");
    let why = target_witness(&assessed(&t, now), now, STALE).expect_err("nothing to take");
    assert!(
        why.contains("no idle profile holds an incremental/"),
        "{why}"
    );
    assert!(reclaim(&t, &judge(now)).is_err());
    assert!(!release.join(".cargo-artifact-lock").exists());
}

/// `dev_of` follows a link, as the free-space figure does, so a home linked to another volume
/// fences the reclaim to that volume. The fixture links to `/dev` (devfs, its own volume).
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

/// Date `profile` last used at `at`: what a compile writes, written then.
fn last_used(profile: &Path, at: i64) {
    for c in CLOCK_DIRS {
        let d = profile.join(c);
        if d.exists() {
            set_mtime(&d, at);
        }
    }
}

/// Under pressure [`reclaim_lru`] takes only the named recent profile's `incremental/`, under
/// all of its locks, and leaves everything else byte-identical.
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

/// Under the locks the pressure pick is derived again, and refused when written since the
/// survey, written within [`PRESSURE_MIN_IDLE_S`], or not one of the directory's profiles.
#[test]
fn reclaim_lru_refuses_a_profile_used_since_the_survey_or_just_built() {
    let tmp = Tmp::new("lru-refuse");
    let t = synthetic_target(&tmp, "target");
    let debug = t.join("debug");
    let now = now_after_write(0);
    let then = now - 3 * 3_600;
    last_used(&debug, then);

    // A compile lands once the locks are taken: refused under them.
    let err = reclaim_lru_with(&t, &debug, &judge(now), then, &mut |stage, _| {
        if stage == Stage::Locked {
            set_mtime(&debug.join("deps"), then + 60);
        }
    })
    .expect_err("used since the survey");
    assert!(err.to_string().contains("since the survey"), "{err}");
    assert!(debug.join(INCREMENTAL).exists());
    // Or before them: refused without a lock taken.
    let err = reclaim_lru(&t, &debug, &judge(now), then).expect_err("used since");
    assert!(err.to_string().contains("since the survey"), "{err}");

    // Built five minutes ago: no reclaim takes a profile that recent.
    last_used(&debug, now - 5 * 60);
    reclaim_lru(&t, &debug, &judge(now), now - 5 * 60).expect_err("just built");
    assert!(debug.join(INCREMENTAL).exists());

    // Not a profile of this build directory.
    reclaim_lru(&t, &t.join("nope"), &judge(now), then).expect_err("no such profile");
    assert!(debug.join(INCREMENTAL).exists());
}

/// A whole removal refuses when a profile appears after it locked the rest, and deletes only
/// through the handle it pinned: a directory swapped in under the name is never entered.
/// Control: left alone, the directory goes whole.
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
    // NTFS renames no directory with a handle open beneath it: the reclaim
    // holds the pinned directory and every lock open, so no swap happens and
    // the directory goes whole.
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

    // The caller's check, under the locks, refuses with its own reason.
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

/// Windows' unhashed twin: the same name with `-` read as `_` and an extension, never a
/// hashed name, a bare name or another extension.
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

/// The Windows volume fence: only a local fixed disk passes; a volume no mount holds, one
/// off the local device allowlist (a share, an unmeasured device), a removable one and one
/// whose kind could not be read each refuse, saying why.
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
