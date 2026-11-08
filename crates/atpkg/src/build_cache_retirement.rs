// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! One-time target retirement: certify every lock, rename, then drain outside the cut.

use std::path::{Path, PathBuf};

use super::{fence::Fence, inventory, registered::retention, remove};

fn names(dir: &Path) -> std::io::Result<(PathBuf, std::ffi::OsString)> {
    let parent = dir
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut base = dir
        .file_name()
        .ok_or_else(|| std::io::Error::other("target has no name"))?
        .to_os_string();
    base.push(".removing");
    Ok((parent.join(".ubd"), base))
}

/// Every aside, including those left by the old synchronous remover. A linked `.ubd`
/// is never used as a retirement destination or searched for deletion candidates.
fn pending(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let (holder, base) = names(dir)?;
    let mut stages = super::stages_in(holder.parent().unwrap(), &base);
    if std::fs::symlink_metadata(&holder).is_ok_and(|m| m.is_dir()) {
        stages.extend(super::stages_in(&holder, &base));
    }
    Ok(stages)
}

fn locks(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    inventory::retiring(dir).map(|seen| seen.locks)
}

fn certify(
    dir: &Path,
    original: &Path,
    fence: &Fence<'_>,
) -> std::io::Result<Vec<crate::lock::Flock>> {
    if !fence.clear(original, dir) || !retention::allows_tree(dir) {
        return Err(std::io::Error::other(
            "a process or custody keeps the retired target",
        ));
    }
    let locks = locks(dir)?;
    let held = super::hold(&locks)
        .ok_or_else(|| std::io::Error::other("a Cargo lock keeps the retired target"))?;
    if self::locks(dir)? != locks || !fence.clear(original, dir) || !retention::allows_tree(dir) {
        return Err(std::io::Error::other(
            "the retired target changed or is in use",
        ));
    }
    Ok(held)
}

/// Certify an obsolete target's complete lock inventory and process/custody fences, then
/// atomically rename it to a unique aside in its parent's `.ubd`. No artifact is deleted
/// here. Returns all asides a background call to [`finish`] should drain, even when the
/// original is already absent. The caller must have stopped using this target permanently.
///
/// # Errors
/// An inventory, lock, process, custody, destination or rename cannot be certified. A live
/// target stays at its original name; no caller should remove its links on an error.
pub fn stage(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    stage_on(dir, &Fence::default())
}

/// [`stage`] with an inert or controlled process census for headless fixtures.
/// # Errors
/// The same inventory, lock, custody and staging errors as [`stage`].
#[cfg(any(test, debug_assertions))]
pub fn stage_with_processes(
    dir: &Path,
    processes: super::fence::Processes<'_>,
) -> std::io::Result<Vec<PathBuf>> {
    stage_on(dir, &Fence::new(processes))
}

fn stage_on(dir: &Path, fence: &Fence<'_>) -> std::io::Result<Vec<PathBuf>> {
    match std::fs::symlink_metadata(dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return pending(dir),
        Err(error) => return Err(error),
        Ok(meta) if !meta.is_dir() => {
            return Err(std::io::Error::other(
                "the retired target is not a physical directory",
            ));
        }
        Ok(_) => {}
    }
    let _held = certify(dir, dir, fence)?;
    let (holder, base) = names(dir)?;
    match std::fs::create_dir(&holder) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    if !std::fs::symlink_metadata(&holder)?.is_dir() || !retention::allows_tree(&holder) {
        return Err(std::io::Error::other(
            "the retirement holder is linked or kept",
        ));
    }
    if !fence.clear(dir, dir) || !retention::allows_tree(dir) {
        return Err(std::io::Error::other(
            "the retired target is now in use or kept",
        ));
    }
    super::stage_unkept(dir, &holder, &base)?
        .ok_or_else(|| std::io::Error::other("custody kept the staged target"))?;
    pending(dir)
}

/// Drain every aside this retirement left, re-certifying locks, both process spellings and
/// custody before deletion. A partial removal stays staged for the next call. Never removes
/// the original target, which a later build may have recreated.
///
/// # Errors
/// A fence, inventory or lock refuses, or an aside could not be completely removed.
pub fn finish(dir: &Path) -> std::io::Result<()> {
    finish_on(dir, &Fence::default())
}

/// [`finish`] with a controlled process census for headless fixtures.
/// # Errors
/// The same certification and incomplete-removal errors as [`finish`].
#[cfg(any(test, debug_assertions))]
pub fn finish_with_processes(
    dir: &Path,
    processes: super::fence::Processes<'_>,
) -> std::io::Result<()> {
    finish_on(dir, &Fence::new(processes))
}

fn finish_on(dir: &Path, fence: &Fence<'_>) -> std::io::Result<()> {
    let mut errors = Vec::new();
    for aside in pending(dir)? {
        let result = (|| {
            let _held = certify(&aside, dir, fence)?;
            match remove::tree_while(&aside, &|| retention::allows_walk(&aside)) {
                Ok(left) if left.whole() => Ok(()),
                Ok(left) => Err(std::io::Error::other(remove::stayed_words(
                    &aside,
                    left.stayed,
                ))),
                Err(_) => Err(std::io::Error::other(super::revoked_words(&aside))),
            }
        })();
        if let Err(error) = result {
            errors.push(format!("{}: {error}", aside.display()));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(std::io::Error::other(errors.join("; ")))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// The bounded survey is the negative control: its small budget refuses exactly the
    /// fixture retirement must handle, including a Cargo lock beyond the usual profile depth.
    #[test]
    fn oversized_retirement_keeps_every_lock_and_resumes_its_aside() {
        inventory::with_entry_budget(8, oversized_retirement);
    }

    fn oversized_retirement() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let dir = scratch.path().join("target.noindex");
        let profile = dir.join("debug/tmp/nested/target/debug");
        std::fs::create_dir_all(&profile).unwrap();
        let deps = dir.join("debug/deps");
        std::fs::create_dir(&deps).unwrap();
        for n in 0..32 {
            std::fs::write(deps.join(format!("artifact-{n}")), b"artifact").unwrap();
        }
        let lock_path = profile.join(".cargo-lock");
        std::fs::write(&lock_path, b"").unwrap();
        assert!(inventory::scan(&dir).is_err());
        let fence = Fence::new(&|_, _| Ok(Default::default()));
        let held = super::super::hold(&[lock_path]).unwrap();
        assert!(
            stage_on(&dir, &fence).is_err(),
            "even a deep held lock keeps this oversized tree"
        );
        assert!(dir.is_dir());
        drop(held);

        let stages = stage_on(&dir, &fence).unwrap();
        assert!(!dir.exists());
        assert_eq!(stages.len(), 1);
        assert_eq!(stages[0].parent().unwrap(), scratch.path().join(".ubd"));
        assert!(
            stages[0].join("debug/deps/artifact-0").is_file(),
            "staging does not delete synchronously"
        );
        assert_eq!(
            stage_on(&dir, &fence).unwrap(),
            stages,
            "an interrupted drain is rediscovered"
        );
        finish_on(&dir, &fence).unwrap();
        assert!(!stages[0].exists());
        assert!(stage_on(&dir, &fence).unwrap().is_empty());
        println!(
            "retirement fixture: bounded inventory=refused, deps artifacts before=32 after=0; aside drained"
        );
    }

    #[test]
    fn resumed_asides_recheck_locks_processes_and_custody() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let dir = scratch.path().join("target.noindex");
        std::fs::create_dir_all(dir.join("debug")).unwrap();
        std::fs::write(dir.join("debug/.cargo-lock"), b"").unwrap();
        let clear = Fence::new(&|_, _| Ok(Default::default()));
        let aside = stage_on(&dir, &clear).unwrap().remove(0);
        let held = super::super::hold(&[aside.join("debug/.cargo-lock")]).unwrap();
        assert!(finish_on(&dir, &clear).is_err());
        assert!(aside.is_dir());
        drop(held);
        let unavailable = Fence::new(&|_, _| Err(std::io::Error::other("unavailable")));
        assert!(finish_on(&dir, &unavailable).is_err());
        std::fs::write(aside.join(super::super::registered::MARKER), b"kept").unwrap();
        assert!(finish_on(&dir, &clear).is_err());
        assert!(aside.join("debug/.cargo-lock").is_file());
        std::fs::remove_file(aside.join(super::super::registered::MARKER)).unwrap();
        // A fresh target belongs to the next build and must survive finishing the old aside.
        std::fs::create_dir(&dir).unwrap();
        finish_on(&dir, &clear).unwrap();
        assert!(dir.is_dir());
        assert!(!aside.exists());
    }

    /// Old adjacent asides remain recoverable; a linked holder never extends deletion
    /// authority into an unrelated folder.
    #[test]
    fn old_asides_resume_but_a_linked_holder_is_never_used() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let dir = scratch.path().join("target.noindex");
        let old = scratch.path().join("target.noindex.removing-2");
        std::fs::create_dir(&old).unwrap();
        std::fs::write(old.join("old-artifact"), b"old").unwrap();
        let external = scratch.path().join("external");
        std::fs::create_dir(&external).unwrap();
        let foreign = external.join("target.noindex.removing");
        std::fs::create_dir(&foreign).unwrap();
        std::os::unix::fs::symlink(&external, scratch.path().join(".ubd")).unwrap();
        let clear = Fence::new(&|_, _| Ok(Default::default()));
        assert_eq!(
            stage_on(&dir, &clear).unwrap().as_slice(),
            std::slice::from_ref(&old)
        );
        finish_on(&dir, &clear).unwrap();
        assert!(!old.exists());
        assert!(foreign.is_dir());
        std::fs::create_dir(&dir).unwrap();
        assert!(stage_on(&dir, &clear).is_err());
        assert!(dir.is_dir());
        assert!(foreign.is_dir());
    }
}
