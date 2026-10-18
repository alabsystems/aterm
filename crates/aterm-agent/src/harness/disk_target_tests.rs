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
    }
}

/// One build directory assessed with the shipped bounds.
fn assessed(path: &Path, now: i64) -> TargetDir {
    assess(path, now, STALE, WalkBudget::SHIPPED).expect("a directory")
}

/// The bytes the blocks of the file at `p` take.
fn blocks(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::symlink_metadata(p).expect("stat").blocks() * 512
}

/// Set the mtime of the directory or file at `path` to `at` (unix seconds).
fn set_mtime(path: &Path, at: i64) {
    let when = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(u64::try_from(at).expect("after the epoch"));
    std::fs::File::open(path)
        .and_then(|f| f.set_modified(when))
        .expect("set mtime");
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
struct Parked {
    spawner: std::thread::JoinHandle<std::io::Result<std::process::ExitStatus>>,
    go: std::io::PipeWriter,
}

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
/// `doc/` page, and a symlink an uplift left (`debug/lay.dSYM`).
fn furnished(tmp: &Tmp, name: &str) -> PathBuf {
    let t = synthetic_target(tmp, name);
    let debug = t.join("debug");
    let twin = debug.join("deps").join("lay-2bcb77ac5af7c69c");
    std::fs::write(&twin, b"#!/bin/sh\necho lay-ok\n").expect("bin");
    std::fs::hard_link(&twin, debug.join("lay")).expect("uplift");
    std::fs::write(debug.join("lay.d"), b"lay: src/main.rs\n").expect(".d");
    std::fs::create_dir_all(debug.join("examples")).expect("examples");
    std::fs::write(debug.join("examples").join("ex"), b"example").expect("ex");
    std::os::unix::fs::symlink("deps/lay-2bcb77ac5af7c69c", debug.join("lay.dSYM"))
        .expect("dsym link");
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
    let cache = t.join("debug/incremental/lay-1x2y3z/s-abc-def/query-cache.bin");
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
    assert_eq!(done.units, vec![t.join("debug/incremental")], "{done:?}");
    assert!(
        done.skipped.is_empty() && done.trouble.is_empty(),
        "{done:?}"
    );
    assert_eq!(done.bytes, expect, "the blocks the query cache released");
    assert!(!t.join("debug/incremental").exists());
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
    lay_profile(&t.join("aarch64-apple-darwin/release"));
    lay_profile(&t.join("conformance-release/release"));
    let now = now_after_write(STALE + 1);
    // The triple's profile was compiled into inside the window (as `now`
    // reads it).
    set_mtime(
        &t.join("aarch64-apple-darwin/release/deps"),
        inside_window(now),
    );

    let td = assessed(&t, now);
    let mut found: Vec<PathBuf> = td.profiles.iter().map(|p| p.path.clone()).collect();
    found.sort();
    assert_eq!(
        found,
        vec![
            t.join("aarch64-apple-darwin/release"),
            t.join("conformance-release/release"),
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
            t.join("conformance-release/release/incremental"),
            t.join("debug/incremental"),
        ]
    );
    assert!(
        t.join("aarch64-apple-darwin/release/incremental").exists(),
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
        assert_eq!(done.units, vec![t.join("release/incremental")], "{lock}");
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
    use std::os::unix::fs::PermissionsExt as _;
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
    let hold = take(&t.join("debug/.cargo-lock"));
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

    // A read-only profile: the same.
    let t = old_cargo(&tmp, "readonly");
    let debug = t.join("debug");
    std::fs::set_permissions(&debug, std::fs::Permissions::from_mode(0o555)).expect("chmod");
    let err = reclaim(&t, &judge(now)).expect_err("cannot lock");
    std::fs::set_permissions(&debug, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    assert!(err.to_string().contains("could not be created"), "{err}");
    assert!(debug.join(INCREMENTAL).exists(), "cache intact");
    assert_eq!(only(&t), 1);
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
    std::os::unix::fs::symlink(&victim, &parent).expect("swap in a link");
    // `victim` holds a `target/`? It does not; give the link one to find.
    std::fs::create_dir_all(victim.join("target")).expect("decoy");
    lay_target(&victim.join("target"));
    let decoy_before = snapshot(&victim.join("target"), &|_| false);
    let err = reclaim(&t, &judge(now)).expect_err("a link in the path");
    assert!(err.to_string().contains("symlink"), "{err}");
    assert_eq!(snapshot(&victim.join("target"), &|_| false), decoy_before);
    std::fs::remove_dir_all(victim.join("target")).expect("decoy away");

    // 2. `incremental` itself a link: never followed, never a candidate.
    let t = synthetic_target(&tmp, "linked-inc");
    std::fs::remove_dir_all(t.join("debug/incremental")).expect("real one away");
    std::os::unix::fs::symlink(
        victim.join("debug/incremental"),
        t.join("debug/incremental"),
    )
    .expect("link");
    let why = target_witness(&assessed(&t, now), now, STALE).expect_err("no real cache");
    assert!(
        why.contains("no idle profile holds an incremental/"),
        "{why}"
    );
    assert!(reclaim(&t, &judge(now)).is_err());
    assert!(std::fs::symlink_metadata(t.join("debug/incremental")).is_ok());

    // 3. A link inside incremental/: the link goes, its target stays.
    let t = synthetic_target(&tmp, "inner-link");
    std::os::unix::fs::symlink(&victim, t.join("debug/incremental/escape")).expect("link");
    reclaim(&t, &judge(now)).expect("reclaimed");
    assert!(!t.join("debug/incremental").exists());

    // 4. The profile swapped for a link under the locks.
    let t = synthetic_target(&tmp, "swap");
    let debug = t.join("debug");
    let moved = t.join("debug.moved");
    let done = reclaim_with(&t, &judge(now), &mut |stage, _| {
        if stage == Stage::Locked {
            std::fs::rename(&debug, &moved).expect("swap");
            std::os::unix::fs::symlink(victim.join("debug"), &debug).expect("link");
        }
    })
    .expect("reclaimed on the checked handle");
    assert!(done.bytes > 0, "{done:?}");
    assert!(
        !moved.join(INCREMENTAL).exists(),
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
    let counted = blocks(&inc.join("lay-1x2y3z/s-abc-def/query-cache.bin"));
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

/// The bytes counted are what the unlinks released: a file hard-linked into `deps/` counts
/// nothing, a sparse file its blocks; the report's estimate and the reclaim's count agree.
#[test]
fn the_bytes_counted_are_the_blocks_the_unlinks_released() {
    let tmp = Tmp::new("bytes");
    let t = synthetic_target(&tmp, "target");
    let debug = t.join("debug");
    let session = debug.join("incremental/lay-1x2y3z/s-abc-def");
    std::fs::write(session.join("lay.o"), vec![5u8; 64 * 1024]).expect("object");
    std::fs::hard_link(session.join("lay.o"), debug.join("deps/lay.o")).expect("linked twin");
    let sparse = session.join("sparse");
    std::fs::File::create(&sparse)
        .and_then(|f| f.set_len(64 * 1024 * 1024))
        .expect("sparse");
    let now = now_after_write(STALE + 1);
    let expect = blocks(&session.join("query-cache.bin")) + blocks(&sparse);
    assert!(expect < 1024 * 1024, "a sparse file holds few blocks");

    assert_eq!(assessed(&t, now).reclaimable(now, STALE), (expect, false));
    let done = reclaim(&t, &judge(now)).expect("reclaimed");
    assert_eq!(done.bytes, expect, "{done:?}");
    assert_eq!(
        std::fs::read(debug.join("deps/lay.o")).expect("kept"),
        vec![5u8; 64 * 1024],
        "the deps/ twin keeps its data"
    );
}

/// An undeletable entry is reported and uncounted, its siblings deleted and counted, and the
/// non-empty `incremental/` not claimed as gone.
#[test]
fn an_undeletable_entry_is_reported_and_its_siblings_still_go() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = Tmp::new("undeletable");
    let t = synthetic_target(&tmp, "target");
    let inc = t.join("debug").join(INCREMENTAL);
    let ro = inc.join("stuck");
    std::fs::create_dir_all(&ro).expect("ro");
    std::fs::write(ro.join("file"), vec![1u8; 8192]).expect("stuck");
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).expect("chmod");
    let mut counted = blocks(&inc.join("lay-1x2y3z/s-abc-def/query-cache.bin"));
    for i in 0..10 {
        let f = inc.join(format!("f{i:02}"));
        std::fs::write(&f, vec![2u8; 8192]).expect("sibling");
        counted += blocks(&f);
    }

    let done = reclaim(&t, &judge(now_after_write(STALE + 1))).expect("partly reclaimed");
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).expect("chmod");
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
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = Tmp::new("cut-short");
    let t = synthetic_target(&tmp, "target");
    let debug = t.join("debug");
    let inc = debug.join(INCREMENTAL);
    let stuck = inc.join("stuck");
    std::fs::create_dir_all(&stuck).expect("stuck");
    std::fs::write(stuck.join("file"), vec![1u8; 8192]).expect("stuck file");
    std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o555)).expect("chmod");
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
        set_mtime(&debug.join(d), old);
    }
    assert!(target_witness(&assessed(&t, now), now, STALE).is_ok());

    let done = reclaim(&t, &judge(now)).expect("partly reclaimed");
    assert!(done.units.is_empty(), "{done:?}");
    assert!(stuck.join("file").exists());
    std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o755)).expect("chmod");

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
    assert_eq!(done.units, vec![t.join("debug/incremental")], "{done:?}");
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
            lay_profile(&t.join("aarch64-apple-darwin/debug"));
        }
    })
    .expect_err("a profile it never locked");
    assert!(err.to_string().contains("profiles changed"), "{err}");
    assert!(t.join("debug/deps/liblay.rlib").exists());

    let tmp = Tmp::new("whole-swap");
    let t = std::fs::canonicalize(lay_target(&tmp.path().join("target"))).expect("canonical");
    let moved = tmp.path().join("target.moved");
    let done = reclaim_whole_with(&t, &judge(now), now, DAY, ok, &mut |stage, _| {
        if stage == Stage::Deleting {
            std::fs::rename(&t, &moved).expect("swap out");
            std::fs::create_dir(&t).expect("swap in");
            std::fs::write(t.join("keep.txt"), b"not a build's").expect("newcomer");
        }
    })
    .expect("the pinned directory's contents went");
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
    assert!(t.join("debug/deps/liblay.rlib").exists());
}
