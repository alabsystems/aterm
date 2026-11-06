// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual kernel locks and genuine Targo source-cache cooperation in private homes.

use super::*;
use std::{
    fs::{File, OpenOptions, TryLockError},
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

const MUTATE: &str = ".package-cache-mutate";

/// An independent native exclusive contender on a Cargo lock file, let go by `LOCK_UN` when
/// dropped (clippy.toml's flock release rule), so no sibling test's fork keeps it held.
struct Exclusive(File);

impl Drop for Exclusive {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn exclusive(path: &Path) -> Result<Exclusive, TryLockError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(TryLockError::Error)?;
    #[expect(
        clippy::disallowed_methods,
        reason = "independent native exclusive contender tests the exact Cargo lock protocol; `Exclusive`'s Drop releases it by `LOCK_UN`"
    )]
    file.try_lock()?;
    Ok(Exclusive(file))
}

/// Each holder lets go at once, even while a child of another thread's spawn sits between fork
/// and exec holding a copy of every descriptor (an flock lives on the open file description): the
/// guard and the contender release by `LOCK_UN`, not by a close that copy outlives.
/// NEGATIVE CONTROL (by hand): with `Held`'s Drop removed the exclusive take reads `WouldBlock`;
/// with `Exclusive`'s, the guard's acquisition does.
#[test]
fn dropped_holders_let_go_while_a_forked_child_holds_their_descriptors() {
    let home = aterm_tempfile::tempdir().unwrap();
    let path = home.path().join(MUTATE);
    let guard = SourceCacheGuard::try_acquire(home.path()).unwrap();
    let guards_fork = aterm_uds::fork_window::Parked::park();
    drop(guard);
    let mutation = exclusive(&path).map_err(|error| format!("{error:?}"));
    let mutations_fork = aterm_uds::fork_window::Parked::park();
    let refused = mutation.as_ref().err().cloned();
    drop(mutation);
    let after = SourceCacheGuard::try_acquire(home.path()).map_err(|error| error.kind());
    for fork in [guards_fork, mutations_fork] {
        let status = fork.release();
        assert!(status.success(), "{status}");
    }
    assert_eq!(
        refused, None,
        "the dropped guard's lock stayed held in the forked child"
    );
    assert_eq!(
        after.err(),
        None,
        "the dropped contender's lock stayed held in the forked child"
    );
}

#[test]
fn shared_guards_allow_download_and_exclude_mutation_until_last_close() {
    fn send_static<T: Send + 'static>() {}
    send_static::<SourceCacheGuard>();
    let home = aterm_tempfile::tempdir().unwrap();
    let path = home.path().join(MUTATE);
    std::fs::write(&path, b"existing Cargo rendezvous bytes").unwrap();
    let before = std::fs::metadata(&path).unwrap();
    let first = SourceCacheGuard::try_acquire(home.path()).unwrap();
    let second = SourceCacheGuard::try_acquire(home.path()).unwrap();
    first.check_current().unwrap();
    second.check_current().unwrap();
    let download = exclusive(&home.path().join(".package-cache")).unwrap();
    assert!(matches!(exclusive(&path), Err(TryLockError::WouldBlock)));
    drop(first);
    assert!(matches!(exclusive(&path), Err(TryLockError::WouldBlock)));
    drop(second);
    let mutation = exclusive(&path).unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"existing Cargo rendezvous bytes"
    );
    assert_eq!(std::fs::metadata(&path).unwrap().ino(), before.ino());
    drop(mutation);
    drop(download);
}

#[test]
fn exclusive_mutator_refuses_shared_acquisition_without_faking_a_guard() {
    let home = aterm_tempfile::tempdir().unwrap();
    let mutation = exclusive(&home.path().join(MUTATE)).unwrap();
    assert_eq!(
        SourceCacheGuard::try_acquire(home.path())
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    drop(mutation);
    SourceCacheGuard::try_acquire(home.path())
        .unwrap()
        .check_current()
        .unwrap();
}

#[test]
fn unsupported_filesystem_and_lock_errors_never_become_acquired() {
    let home = aterm_tempfile::tempdir().unwrap();
    let called = AtomicBool::new(false);
    let error = imp::acquire_with(
        home.path(),
        |_| Err(io::ErrorKind::Unsupported.into()),
        |_| {
            called.store(true, Ordering::SeqCst);
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert!(!called.load(Ordering::SeqCst));
    assert!(
        !home.path().join(MUTATE).exists(),
        "refuse known unsupported FS before creating or locking"
    );
    for error in [
        io::Error::from_raw_os_error(libc::ENOLCK),
        io::ErrorKind::Unsupported.into(),
        io::ErrorKind::PermissionDenied.into(),
    ] {
        let expected = (error.kind(), error.raw_os_error());
        let failed = imp::acquire_with(home.path(), |_| Ok(()), |_| Err(error)).unwrap_err();
        assert_eq!((failed.kind(), failed.raw_os_error()), expected);
        drop(exclusive(&home.path().join(MUTATE)).unwrap());
    }
}

#[test]
fn lock_files_own_unsupported_filesystem_refuses_before_any_lock() {
    let home = aterm_tempfile::tempdir().unwrap();
    let path = home.path().join(MUTATE);
    std::fs::write(&path, b"keep original mount file bytes").unwrap();
    let mut probes = 0;
    let called = AtomicBool::new(false);
    let refused = imp::acquire_with(
        home.path(),
        |_| {
            probes += 1;
            if probes == 1 {
                Ok(())
            } else {
                Err(io::ErrorKind::Unsupported.into())
            }
        },
        |_| {
            called.store(true, Ordering::SeqCst);
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::Unsupported);
    assert_eq!(probes, 2);
    assert!(!called.load(Ordering::SeqCst));
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"keep original mount file bytes"
    );
}

#[test]
fn lock_name_replacement_is_detected_without_releasing_the_original_guard() {
    let home = aterm_tempfile::tempdir().unwrap();
    let guard = SourceCacheGuard::try_acquire(home.path()).unwrap();
    let old = home.path().join("original-lock");
    std::fs::rename(home.path().join(MUTATE), &old).unwrap();
    std::fs::write(home.path().join(MUTATE), b"replacement").unwrap();
    assert!(guard.check_current().is_err());
    assert!(matches!(exclusive(&old), Err(TryLockError::WouldBlock)));
    drop(exclusive(&home.path().join(MUTATE)).unwrap());
    drop(guard);
    drop(exclusive(&old).unwrap());
    assert_eq!(
        std::fs::read(home.path().join(MUTATE)).unwrap(),
        b"replacement"
    );
}

#[test]
fn home_replacement_is_detected_while_original_lock_remains_owned() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("cargo-home");
    std::fs::create_dir(&home).unwrap();
    let guard = SourceCacheGuard::try_acquire(&home).unwrap();
    let moved = root.path().join("original-home");
    std::fs::rename(&home, &moved).unwrap();
    std::fs::create_dir(&home).unwrap();
    assert!(guard.check_current().is_err());
    assert!(matches!(
        exclusive(&moved.join(MUTATE)),
        Err(TryLockError::WouldBlock)
    ));
    drop(guard);
    drop(exclusive(&moved.join(MUTATE)).unwrap());
}

#[test]
fn redirected_or_shared_write_lock_files_refuse_without_modifying_them() {
    use std::os::unix::fs::symlink;
    let home = aterm_tempfile::tempdir().unwrap();
    let outside = home.path().join("original");
    std::fs::write(&outside, b"keep original bytes").unwrap();
    let path = home.path().join(MUTATE);
    symlink(&outside, &path).unwrap();
    assert!(SourceCacheGuard::try_acquire(home.path()).is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), b"keep original bytes");
    std::fs::remove_file(&path).unwrap();
    std::fs::hard_link(&outside, &path).unwrap();
    assert_eq!(
        SourceCacheGuard::try_acquire(home.path())
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, b"do not chmod or truncate").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert_eq!(
        SourceCacheGuard::try_acquire(home.path())
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o666);
    assert_eq!(std::fs::read(&path).unwrap(), b"do not chmod or truncate");
}

#[test]
fn missing_relative_or_read_only_home_has_no_best_effort_success() {
    assert_eq!(
        SourceCacheGuard::try_acquire(Path::new("relative"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    let root = aterm_tempfile::tempdir().unwrap();
    assert_eq!(
        SourceCacheGuard::try_acquire(&root.path().join("missing"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    let path = root.path().join(MUTATE);
    std::fs::write(&path, b"read only").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    // Root may open a read-only-mode file successfully; that is then a real lock.
    // This ordinary user control requires the actual open error, never a fallback.
    // SAFETY: geteuid reads this process's effective uid and has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        assert_eq!(
            SourceCacheGuard::try_acquire(root.path())
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }
    assert_eq!(std::fs::read(&path).unwrap(), b"read only");
}

#[test]
fn identity_drift_after_actual_lock_acquisition_refuses_and_closes_only_its_file() {
    let home = aterm_tempfile::tempdir().unwrap();
    let old = home.path().join("old-lock");
    let error = imp::acquire_with(
        home.path(),
        |_| Ok(()),
        |file| {
            #[expect(
                clippy::disallowed_methods,
                reason = "actual shared acquisition before the deterministic name-replacement seam"
            )]
            file.try_lock_shared().map_err(io::Error::from)?;
            std::fs::rename(home.path().join(MUTATE), &old)?;
            std::fs::write(home.path().join(MUTATE), b"replacement")
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    drop(exclusive(&old).unwrap());
    assert_eq!(
        std::fs::read(home.path().join(MUTATE)).unwrap(),
        b"replacement"
    );
}

fn targo() -> PathBuf {
    let targo = PathBuf::from(
        std::env::var_os("CARGO").expect("the actual test producer supplies its Cargo/Targo path"),
    );
    let mut version = aterm_uds::spawnfd::command(&targo);
    version.args(["--unverified", "--version"]);
    let output =
        aterm_uds::deadline::output_within(&mut version, aterm_spec::wait::MUST_HAPPEN).unwrap();
    assert!(
        output.status.success() && String::from_utf8_lossy(&output.stdout).starts_with("targo "),
        "genuine Targo discriminator: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    targo
}

fn fetch(targo: &Path, project: &Path, home: &Path, offline: bool) -> String {
    let mut command = aterm_uds::spawnfd::command(targo);
    command
        .args(["fetch", "--locked", "--manifest-path"])
        .arg(project.join("Cargo.toml"));
    if offline {
        command.arg("--offline");
    }
    command
        .current_dir(project)
        .env("CARGO_HOME", home)
        // The fixture has no registry/git dependencies. This explicit local policy
        // enables entry into AutoGC in its dedicated case; no network source exists.
        .env("CARGO_NET_OFFLINE", if offline { "true" } else { "false" })
        .env("CARGO_CACHE_AUTO_CLEAN_FREQUENCY", "always")
        .env("CARGO_LOG", "gc=trace");
    let output =
        aterm_uds::deadline::output_within(&mut command, aterm_spec::wait::MUST_HAPPEN).unwrap();
    let text = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "actual local fetch failed: {text}");
    eprintln!("actual dependency-free Targo fetch offline={offline}:\n{text}");
    text
}

#[test]
fn genuine_targo_fetch_coexists_and_actual_auto_gc_cannot_mutate_while_held() {
    let targo = targo();
    let root = aterm_tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    let home = root.path().join("cargo-home");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::create_dir(&home).unwrap();
    std::fs::write(project.join("Cargo.toml"), "[package]\nname='source_cache_guard_probe'\nversion='0.0.0'\nedition='2024'\n[workspace]\n").unwrap();
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(
        project.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"source_cache_guard_probe\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    let before = std::fs::read(project.join("Cargo.lock")).unwrap();
    let guard = SourceCacheGuard::try_acquire(&home).unwrap();
    let offline = fetch(&targo, &project, &home, true);
    assert!(
        offline.contains("running offline, auto gc disabled"),
        "offline fetch never tests GC mutation: {offline}"
    );
    let contended = fetch(&targo, &project, &home, false);
    assert!(
        contended.contains("unable to acquire mutate lock, auto gc disabled"),
        "actual AutoGC must observe the owned shared lock: {contended}"
    );
    assert!(!contended.contains("running offline, auto gc disabled"));
    guard.check_current().unwrap();
    drop(guard);
    let admitted = fetch(&targo, &project, &home, false);
    assert!(
        admitted.contains("should_run_auto_gc"),
        "after release the actual GC reaches its admitted policy: {admitted}"
    );
    assert!(!admitted.contains("unable to acquire mutate lock"));
    assert_eq!(std::fs::read(project.join("Cargo.lock")).unwrap(), before);
    // A new cache may initialize last_auto_gc in this same second, so this is
    // GC admission evidence, not a claim that any source was selected for deletion.
}
