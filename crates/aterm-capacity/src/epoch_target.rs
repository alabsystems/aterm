// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Original filesystem target exclusion through compilation and artifact acceptance.
//! This owns no CPU, cohort, source-generation or publication authority.

use aterm_dirfd::{self as fd, Mode, OFlags};
use std::{
    fmt,
    fs::File,
    io,
    os::unix::fs::MetadataExt as _,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

const LOCK: &str = ".aterm-epoch-target.lock";

// Preserve the legacy root API's parent aliases (notably macOS /tmp) without
// following a target-leaf symlink. Resolve the existing parent once, then use
// only the pinned canonical physical path for locking and the Spawn request.
pub(crate) fn resolve_legacy_target(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute()
        || path
            .components()
            .any(|p| matches!(p, Component::CurDir | Component::ParentDir))
    {
        return Err(refused(
            "root target must be absolute without parent traversal",
        ));
    }
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            return Err(refused("root target leaf must be an original directory"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut parts = vec![
        path.file_name()
            .ok_or_else(|| refused("root target has no leaf"))?
            .to_owned(),
    ];
    let mut parent = path
        .parent()
        .ok_or_else(|| refused("root target has no parent"))?;
    loop {
        match std::fs::symlink_metadata(parent) {
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                parts.push(
                    parent
                        .file_name()
                        .ok_or_else(|| refused("root target has no existing parent"))?
                        .to_owned(),
                );
                parent = parent
                    .parent()
                    .ok_or_else(|| refused("root target has no existing parent"))?;
            }
            Err(error) => return Err(error),
        }
    }
    let mut resolved = parent.canonicalize()?;
    for part in parts.into_iter().rev() {
        resolved.push(part);
    }
    Ok(resolved)
}

fn refused(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

fn directory(parent: &File, name: impl fd::Arg) -> io::Result<File> {
    aterm_uds::kept_out(|| {
        fd::openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(io::Error::from)
    })
}

fn owned(file: &File, is_directory: bool) -> io::Result<(u64, u64)> {
    let m = file.metadata()?;
    // SAFETY: geteuid reads this process's effective account identity.
    let uid = unsafe { libc::geteuid() };
    if m.uid() != uid
        || if is_directory {
            !m.is_dir() || m.mode() & 0o022 != 0
        } else {
            !m.is_file() || m.nlink() != 1 || m.mode() & 0o7777 != 0o600
        }
    {
        return Err(refused(
            "target directory or lock changed ownership, type, links or privacy",
        ));
    }
    Ok((m.dev(), m.ino()))
}

fn walk(path: &Path, create: bool) -> io::Result<File> {
    if !path.is_absolute()
        || path
            .components()
            .any(|p| matches!(p, Component::CurDir | Component::ParentDir))
    {
        return Err(refused(
            "target lease requires an absolute canonical directory",
        ));
    }
    let root = aterm_uds::kept_out(|| {
        fd::openat(
            fd::CWD,
            "/",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(io::Error::from)
    })?;
    let mut current = root;
    for part in path.components() {
        if let Component::Normal(name) = part {
            current = match directory(&current, name) {
                Ok(next) => next,
                Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                    owned(&current, true)?;
                    match fd::mkdirat(&current, name, Mode::from_bits(0o700)) {
                        Ok(()) => {}
                        Err(error) if error.raw_os_error() == libc::EEXIST => {}
                        Err(error) => return Err(error.into()),
                    }
                    let next = directory(&current, name)?;
                    owned(&next, true)?;
                    next
                }
                Err(error) => return Err(error),
            };
        }
    }
    owned(&current, true)?;
    Ok(current)
}

struct Inner {
    path: PathBuf,
    directory: File,
    directory_id: (u64, u64),
    lock: File,
    lock_id: (u64, u64),
    loaned: AtomicBool,
}

impl Inner {
    fn validate(&self) -> io::Result<()> {
        if owned(&self.directory, true)? != self.directory_id
            || owned(&walk(&self.path, false)?, true)? != self.directory_id
            || owned(&self.lock, false)? != self.lock_id
        {
            return Err(refused(
                "original target directory or lock changed identity",
            ));
        }
        let named = File::from(fd::openat(
            &self.directory,
            LOCK,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )?);
        if owned(&named, false)? != self.lock_id {
            return Err(refused("original target lock name was replaced"));
        }
        Ok(())
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let _ = self.lock.unlock();
    }
}

/// Opaque original target lock, retained through artifact acceptance.
pub struct TargetLease {
    inner: Arc<Inner>,
}

impl fmt::Debug for TargetLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TargetLease")
            .field("path", &self.inner.path)
            .finish_non_exhaustive()
    }
}

impl TargetLease {
    /// Acquire a canonical target, creating missing directories under owned ancestors.
    /// # Errors
    /// WouldBlock if another guard holds its original lock; unsafe paths, linked or
    /// replaced nodes, foreign ownership, and filesystem errors refuse.
    pub fn acquire(path: &Path) -> io::Result<Self> {
        let directory = walk(path, true)?;
        let directory_id = owned(&directory, true)?;
        let lock: std::fs::File = aterm_uds::kept_out(|| {
            match fd::openat(
                &directory,
                LOCK,
                OFlags::RDWR
                    | OFlags::CREATE
                    | OFlags::EXCL
                    | OFlags::NOFOLLOW
                    | OFlags::CLOEXEC
                    | OFlags::NONBLOCK,
                Mode::from_bits(0o600),
            ) {
                Ok(file) => {
                    let file = File::from(file);
                    use std::os::unix::fs::PermissionsExt as _;
                    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                    Ok(file)
                }
                Err(error) if error.raw_os_error() == libc::EEXIST => Ok(File::from(fd::openat(
                    &directory,
                    LOCK,
                    OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                    Mode::empty(),
                )?)),
                Err(error) => Err(io::Error::from(error)),
            }
        })?;
        let lock_id = owned(&lock, false)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "nonblocking admission on this exact original target lock"
        )]
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Err(io::ErrorKind::WouldBlock.into()),
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }
        let lease = Self {
            inner: Arc::new(Inner {
                path: path.to_owned(),
                directory,
                directory_id,
                lock,
                lock_id,
                loaned: AtomicBool::new(false),
            }),
        };
        lease.check()?;
        Ok(lease)
    }

    /// Original canonical physical target path guarded by this lease.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Revalidate original physical directory and named lock without adopting replacements.
    /// # Errors
    /// Changed/missing directory or lock, ownership/type/privacy failure, or I/O error.
    pub fn check(&self) -> io::Result<()> {
        self.inner.validate()
    }

    /// Lend the actual held lock to one root, keeping exclusion even if this lease is dropped.
    /// # Errors
    /// WouldBlock for an existing active loan, or changed original physical custody.
    pub fn loan(&mut self) -> io::Result<TargetLoan> {
        self.check()?;
        self.inner
            .loaned
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| io::Error::from(io::ErrorKind::WouldBlock))?;
        Ok(TargetLoan {
            inner: Arc::clone(&self.inner),
        })
    }
}

/// Non-cloneable borrowed exclusion for one original root and its output receipt.
pub struct TargetLoan {
    inner: Arc<Inner>,
}

impl fmt::Debug for TargetLoan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TargetLoan").finish_non_exhaustive()
    }
}

impl TargetLoan {
    /// Original canonical physical target path guarded by this loan.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Recheck the original target and named lock without changing custody.
    /// # Errors
    /// Original physical directory/lock was replaced, removed or made unsafe.
    pub fn check(&self) -> io::Result<()> {
        self.inner.validate()
    }

    pub(crate) fn validate(&self, target: &Path) -> io::Result<()> {
        if target != self.inner.path {
            return Err(refused("target loan belongs to another path"));
        }
        self.check()
    }
}

impl Drop for TargetLoan {
    fn drop(&mut self) {
        self.inner.loaned.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn legacy_parent_alias_resolves_once_but_target_leaf_symlinks_refuse() {
        let world = aterm_tempfile::Builder::new()
            .prefix("epoch-target-alias-")
            .tempdir()
            .unwrap();
        let physical = world.path().canonicalize().unwrap();
        let alias = physical.join("parent-alias");
        std::os::unix::fs::symlink(&physical, &alias).unwrap();
        let requested = alias.join("nested/target");
        assert!(TargetLease::acquire(&requested).is_err());
        let resolved = resolve_legacy_target(&requested).unwrap();
        assert_eq!(resolved, physical.join("nested/target"));
        let lease = TargetLease::acquire(&resolved).unwrap();
        assert!(
            matches!(TargetLease::acquire(&resolve_legacy_target(&requested).unwrap()),
            Err(error) if error.kind()==io::ErrorKind::WouldBlock)
        );
        // Retargeting the caller's alias cannot change the pinned physical path.
        std::fs::remove_file(&alias).unwrap();
        let other = physical.join("other");
        std::fs::create_dir(&other).unwrap();
        std::os::unix::fs::symlink(&other, &alias).unwrap();
        lease.check().unwrap();
        let leaf = physical.join("target-link");
        std::os::unix::fs::symlink(&resolved, &leaf).unwrap();
        assert!(resolve_legacy_target(&leaf).is_err());
    }

    #[test]
    fn original_loan_keeps_target_exclusive_and_can_be_reused_after_drop() {
        let world = aterm_tempfile::Builder::new()
            .prefix("epoch-target-")
            .tempdir()
            .unwrap();
        let target = world.path().canonicalize().unwrap().join("nested/target");
        let mut lease = TargetLease::acquire(&target).unwrap();
        assert!(
            matches!(TargetLease::acquire(&target),Err(e) if e.kind()==io::ErrorKind::WouldBlock)
        );
        let loan = lease.loan().unwrap();
        assert!(matches!(lease.loan(),Err(e) if e.kind()==io::ErrorKind::WouldBlock));
        loan.validate(&target).unwrap();
        drop(lease);
        assert!(
            matches!(TargetLease::acquire(&target),Err(e) if e.kind()==io::ErrorKind::WouldBlock)
        );
        drop(loan);
        let mut next = TargetLease::acquire(&target).unwrap();
        let first = next.loan().unwrap();
        drop(first);
        next.loan().unwrap().validate(&target).unwrap();
    }

    #[test]
    fn original_directory_or_lock_replacement_refuses_without_adopting() {
        let world = aterm_tempfile::Builder::new()
            .prefix("epoch-target-identity-")
            .tempdir()
            .unwrap();
        let target = world.path().canonicalize().unwrap().join("target");
        let mut lease = TargetLease::acquire(&target).unwrap();
        let loan = lease.loan().unwrap();
        std::fs::rename(target.join(LOCK), target.join("original-lock")).unwrap();
        std::fs::write(target.join(LOCK), b"foreign").unwrap();
        std::fs::set_permissions(target.join(LOCK), std::fs::Permissions::from_mode(0o600))
            .unwrap();
        assert!(lease.check().is_err());
        assert!(loan.check().is_err());
        drop(loan);
        drop(lease);
        let lease = TargetLease::acquire(&target).unwrap();
        std::fs::rename(&target, world.path().join("original-target")).unwrap();
        std::fs::create_dir(&target).unwrap();
        assert!(lease.check().is_err());
    }
}
