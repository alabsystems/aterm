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
#[path = "build_cache_retirement_tests.rs"]
mod tests;
