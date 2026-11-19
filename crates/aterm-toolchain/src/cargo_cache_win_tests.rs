// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows arm's actual `LockFileEx` locks in private homes, and genuine Cargo's cooperation
//! with them: the unix suite's cases that hold on Windows, with Windows' own redirections.

use super::*;
use aterm_dirfd::acl;
use std::{
    fs::{File, OpenOptions, TryLockError},
    io::Write as _,
    path::{Path, PathBuf},
};

const MUTATE: &str = ".package-cache-mutate";

#[test]
fn reclamation_excludes_build_readers_and_releases_partial_download_contention() {
    let home = aterm_tempfile::tempdir().unwrap();
    let download_path = home.path().join(".package-cache");
    write_owned(&download_path, b"");
    let reader = SourceCacheGuard::try_acquire(home.path()).unwrap();
    assert_eq!(
        SourceCacheReclaimGuard::try_acquire(home.path())
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    drop(reader);
    let download = exclusive(&download_path).unwrap();
    assert_eq!(
        SourceCacheReclaimGuard::try_acquire(home.path())
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    // Failure on the second lock must release the first. Downloads and
    // ordinary readers remain compatible under Cargo's actual protocol.
    drop(SourceCacheGuard::try_acquire(home.path()).unwrap());
    drop(download);
    let reclaim = SourceCacheReclaimGuard::try_acquire(home.path()).unwrap();
    reclaim.check_current().unwrap();
    assert!(SourceCacheGuard::try_acquire(home.path()).is_err());
    assert!(matches!(
        exclusive(&download_path),
        Err(TryLockError::WouldBlock)
    ));
    drop(reclaim);
    drop(SourceCacheGuard::try_acquire(home.path()).unwrap());
    exclusive(&download_path).unwrap();
}

#[test]
fn reclamation_missing_protocol_is_readonly_and_download_identity_stays_bound() {
    let home = aterm_tempfile::tempdir().unwrap();
    assert!(SourceCacheReclaimGuard::try_acquire(home.path()).is_err());
    assert!(std::fs::read_dir(home.path()).unwrap().next().is_none());
    write_owned(&home.path().join(MUTATE), b"");
    assert!(SourceCacheReclaimGuard::try_acquire(home.path()).is_err());
    assert!(!home.path().join(".package-cache").exists());
    let download = home.path().join(".package-cache");
    write_owned(&download, b"");
    let reclaim = SourceCacheReclaimGuard::try_acquire(home.path()).unwrap();
    std::fs::rename(&download, home.path().join("retained-download-lock")).unwrap();
    write_owned(&download, b"");
    assert!(reclaim.check_current().is_err());
    assert!(SourceCacheGuard::try_acquire(home.path()).is_err());
    drop(reclaim);
    drop(SourceCacheReclaimGuard::try_acquire(home.path()).unwrap());
}

#[test]
fn shared_guards_allow_download_and_exclude_mutation_until_last_close() {
    fn send_static<T: Send + 'static>() {}
    send_static::<SourceCacheGuard>();
    let home = aterm_tempfile::tempdir().unwrap();
    let path = home.path().join(MUTATE);
    write_owned(&path, b"existing Cargo rendezvous bytes");
    let before = identity(&path);
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
    // A Windows byte-range lock is mandatory: the bytes are read once it is let go.
    drop(mutation);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"existing Cargo rendezvous bytes"
    );
    assert_eq!(identity(&path), before);
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
fn lock_errors_never_become_acquired() {
    let home = aterm_tempfile::tempdir().unwrap();
    for error in [
        // ERROR_LOCK_VIOLATION.
        io::Error::from_raw_os_error(33),
        io::ErrorKind::Unsupported.into(),
        io::ErrorKind::PermissionDenied.into(),
    ] {
        let expected = (error.kind(), error.raw_os_error());
        let failed = imp::acquire_mode(home.path(), |_| Err(error), true).unwrap_err();
        assert_eq!((failed.kind(), failed.raw_os_error()), expected);
        drop(exclusive(&home.path().join(MUTATE)).unwrap());
    }
}

#[test]
fn lock_name_replacement_is_detected_without_releasing_the_original_guard() {
    let home = aterm_tempfile::tempdir().unwrap();
    let guard = SourceCacheGuard::try_acquire(home.path()).unwrap();
    let old = home.path().join("original-lock");
    std::fs::rename(home.path().join(MUTATE), &old).unwrap();
    write_owned(&home.path().join(MUTATE), b"replacement");
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

/// Windows refuses to rename a folder while a file under it is open, so a held guard keeps its
/// Cargo home's name; a home replaced while no guard holds it is a home of its own.
#[test]
fn a_held_cargo_home_keeps_its_name_and_a_replaced_one_is_another_home() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("cargo-home");
    std::fs::create_dir(&home).unwrap();
    let guard = SourceCacheGuard::try_acquire(&home).unwrap();
    let moved = root.path().join("original-home");
    let refused = std::fs::rename(&home, &moved).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied, "{refused}");
    guard.check_current().unwrap();
    drop(guard);
    std::fs::rename(&home, &moved).unwrap();
    std::fs::create_dir(&home).unwrap();
    let replaced = SourceCacheGuard::try_acquire(&home).unwrap();
    replaced.check_current().unwrap();
    drop(exclusive(&moved.join(MUTATE)).unwrap());
    assert!(matches!(
        exclusive(&home.join(MUTATE)),
        Err(TryLockError::WouldBlock)
    ));
}

#[test]
fn redirected_or_shared_write_lock_files_refuse_without_modifying_them() {
    let home = aterm_tempfile::tempdir().unwrap();
    let outside = home.path().join("original");
    std::fs::write(&outside, b"keep original bytes").unwrap();
    let path = home.path().join(MUTATE);
    // A reparse point at the lock's name is never followed: a junction to a folder.
    let folder = home.path().join("elsewhere");
    std::fs::create_dir(&folder).unwrap();
    junction(&path, &folder);
    assert!(SourceCacheGuard::try_acquire(home.path()).is_err());
    assert!(std::fs::read_dir(&folder).unwrap().next().is_none());
    std::fs::remove_dir(&path).unwrap();
    std::fs::hard_link(&outside, &path).unwrap();
    assert_eq!(
        SourceCacheGuard::try_acquire(home.path())
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(std::fs::read(&outside).unwrap(), b"keep original bytes");
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, b"do not re-permit or truncate").unwrap();
    // Everyone may read it: what others may read does not matter, as on unix.
    let user = acl::Sid::token_user().unwrap();
    permit(
        &path,
        &format!("D:P(A;;FA;;;{user})(A;;FA;;;SY)(A;;FR;;;WD)"),
    );
    SourceCacheGuard::try_acquire(home.path())
        .unwrap()
        .check_current()
        .unwrap();
    // Everyone may write it: unix's group/other write bits, refused.
    permit(
        &path,
        &format!("D:P(A;;FA;;;{user})(A;;FA;;;SY)(A;;FRFW;;;WD)"),
    );
    let access_list = || {
        acl::Descriptor::of_path(&path, acl::Parts::DACL)
            .unwrap()
            .to_sddl(acl::Parts::DACL)
            .unwrap()
    };
    let before = access_list();
    assert_eq!(
        SourceCacheGuard::try_acquire(home.path())
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(access_list(), before);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"do not re-permit or truncate"
    );
    // No access list at all (a NULL list, which this user may set): everyone may do everything,
    // unix's 0666, refused and left as it was.
    permit(&path, "D:NO_ACCESS_CONTROL");
    let before = access_list();
    assert_eq!(
        SourceCacheGuard::try_acquire(home.path())
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(access_list(), before);
}

/// The lock rule's clauses read off descriptors made in memory, where no file this user can make
/// reaches one: a lock another account owns (SYSTEM; re-owning a file takes SeRestorePrivilege,
/// which this user's token lacks), one with no access list at all (a NULL list lets everyone do
/// everything), and one holding an allow entry of a type the rule does not read (an object entry,
/// read rights only) are each refused; the user's own, under the user, SYSTEM and Administrators,
/// passes.
#[test]
fn another_owner_a_null_list_or_an_unread_entry_type_is_refused() {
    let user = acl::Sid::token_user().unwrap();
    let closed = |sddl: &str| {
        imp::closed_to_others(&acl::Descriptor::from_sddl(sddl).unwrap())
            .unwrap_or_else(|e| panic!("{sddl}: {e}"))
    };
    assert!(
        closed(&format!(
            "O:{user}D:P(A;;FA;;;{user})(A;;FA;;;SY)(A;;FA;;;BA)"
        )),
        "the user's own"
    );
    assert!(
        !closed(&format!("O:SYD:P(A;;FA;;;{user})(A;;FA;;;SY)")),
        "another account's"
    );
    assert!(
        !closed(&format!("O:{user}D:NO_ACCESS_CONTROL")),
        "no access list"
    );
    // An object entry with an object type: descriptor text with none makes a plain allow entry.
    let unread =
        format!("O:{user}D:P(A;;FA;;;{user})(OA;;FR;bf967aba-0de6-11d0-a285-00aa003049e2;;WD)");
    let kinds: Vec<u8> = acl::Descriptor::from_sddl(&unread)
        .unwrap()
        .entries()
        .unwrap()
        .expect("an access list")
        .iter()
        .map(|entry| entry.kind)
        .collect();
    assert!(
        kinds
            .iter()
            .any(|kind| ![acl::ACCESS_ALLOWED, acl::ACCESS_DENIED].contains(kind)),
        "premise: an entry of another type, in {kinds:?}"
    );
    assert!(
        !closed(&unread),
        "an entry of a type the rule does not read"
    );
}

#[test]
fn an_aliased_cargo_home_is_held_at_its_real_folder() {
    let world = aterm_tempfile::tempdir().unwrap();
    let home = world.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let alias = world.path().join("alias");
    junction(&alias, &home);
    let guard = SourceCacheGuard::try_acquire(&alias).unwrap();
    guard.check_current().unwrap();
    assert!(matches!(
        exclusive(&home.join(MUTATE)),
        Err(TryLockError::WouldBlock)
    ));
    drop(guard);
    drop(exclusive(&home.join(MUTATE)).unwrap());
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
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&path, permissions.clone()).unwrap();
    assert_eq!(
        SourceCacheGuard::try_acquire(root.path())
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"read only");
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "the scratch file is put back as it was made so its folder can go"
    )]
    permissions.set_readonly(false);
    std::fs::set_permissions(&path, permissions).unwrap();
}

#[test]
fn identity_drift_after_actual_lock_acquisition_refuses_and_closes_only_its_file() {
    let home = aterm_tempfile::tempdir().unwrap();
    let old = home.path().join("old-lock");
    let error = imp::acquire_mode(
        home.path(),
        |file| {
            #[expect(
                clippy::disallowed_methods,
                reason = "actual shared acquisition before the deterministic name-replacement seam"
            )]
            file.try_lock_shared().map_err(io::Error::from)?;
            std::fs::rename(home.path().join(MUTATE), &old)?;
            write_owned(&home.path().join(MUTATE), b"replacement");
            Ok(())
        },
        true,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    drop(exclusive(&old).unwrap());
    assert_eq!(
        std::fs::read(home.path().join(MUTATE)).unwrap(),
        b"replacement"
    );
}

/// Genuine Cargo, the one running this test, honours the guard's Windows lock as its own: its
/// automatic cache cleaning cannot take the mutate lock while a shared guard holds it, and
/// reaches its own policy once the guard lets go. The fixture has no dependency, so nothing is
/// fetched from any network.
#[test]
fn genuine_cargo_fetch_coexists_and_actual_auto_gc_cannot_mutate_while_held() {
    let cargo = PathBuf::from(
        std::env::var_os("CARGO").expect("the actual test producer supplies its Cargo path"),
    );
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
    let offline = fetch(&cargo, &project, &home, true);
    assert!(
        offline.contains("running offline, auto gc disabled"),
        "offline fetch never tests GC mutation: {offline}"
    );
    let contended = fetch(&cargo, &project, &home, false);
    assert!(
        contended.contains("unable to acquire mutate lock, auto gc disabled"),
        "actual AutoGC must observe the owned shared lock: {contended}"
    );
    assert!(!contended.contains("running offline, auto gc disabled"));
    guard.check_current().unwrap();
    drop(guard);
    let admitted = fetch(&cargo, &project, &home, false);
    assert!(
        admitted.contains("should_run_auto_gc"),
        "after release the actual GC reaches its admitted policy: {admitted}"
    );
    assert!(!admitted.contains("unable to acquire mutate lock"));
    assert_eq!(std::fs::read(project.join("Cargo.lock")).unwrap(), before);
}

fn fetch(cargo: &Path, project: &Path, home: &Path, offline: bool) -> String {
    let mut command = aterm_uds::spawnfd::command(cargo);
    command
        .args(["fetch", "--locked", "--manifest-path"])
        .arg(project.join("Cargo.toml"));
    if offline {
        command.arg("--offline");
    }
    command
        .current_dir(project)
        .env("CARGO_HOME", home)
        .env("CARGO_NET_OFFLINE", if offline { "true" } else { "false" })
        .env("CARGO_CACHE_AUTO_CLEAN_FREQUENCY", "always")
        .env("CARGO_LOG", "gc=trace");
    let output = aterm_uds::deadline::output_while_process_progressing(
        &mut command,
        aterm_spec::wait::MUST_HAPPEN,
        "a dependency-free cargo fetch",
    )
    .unwrap();
    let text = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "actual local fetch failed: {text}");
    eprintln!("actual dependency-free Cargo fetch offline={offline}:\n{text}");
    text
}

/// A lock file this user made, taking its folder's entries as Cargo's own do.
fn write_owned(path: &Path, bytes: &[u8]) {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .and_then(|mut file| file.write_all(bytes))
        .unwrap();
}

/// The identity of the file at `path`.
fn identity(path: &Path) -> aterm_dirfd::FileId {
    aterm_dirfd::file_id(File::open(path).unwrap()).unwrap()
}

/// Replace the access list of `path` with the one `sddl` spells, taking nothing from its folder.
fn permit(path: &Path, sddl: &str) {
    acl::Descriptor::from_sddl(sddl)
        .unwrap()
        .apply_to_path(path, acl::Parts::DACL | acl::Parts::PROTECTED_DACL)
        .unwrap();
}

/// A directory junction at `link` to `target`: unlike a symbolic link it needs no privilege or
/// Developer Mode. Under sshd's Redirection Guard, which follows no junction this user made, the
/// test ends COULD NOT RUN.
fn junction(link: &Path, target: &Path) {
    aterm_uds::host::could_not_run_under_redirection_guard(
        "the test plants a junction this user makes where the guard looks",
    );
    let plain = |path: &Path| {
        let text = path.to_str().expect("test paths are UTF-8");
        text.strip_prefix(r"\\?\").unwrap_or(text).to_owned()
    };
    let status = aterm_uds::spawnfd::command("cmd")
        .args(["/C", "mklink", "/J", &plain(link), &plain(target)])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "mklink /J {link:?} {target:?}");
}

/// An independent native exclusive contender on a Cargo lock file, let go by `UnlockFile` when
/// dropped (clippy.toml's lock release rule).
struct Exclusive(File);

impl Drop for Exclusive {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// [`Exclusive`] on `path`, making a missing file as [`write_owned`] does.
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
        reason = "independent native exclusive contender tests the exact Cargo lock protocol; `Exclusive`'s Drop releases it by `UnlockFile`"
    )]
    file.try_lock()?;
    Ok(Exclusive(file))
}
