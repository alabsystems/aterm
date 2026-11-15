// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fresh namespace/cache lifetime, independent of input meaning or dispatch.
//! A workspace is neither source/config custody nor a seed, SDK or admission.
//! Native settlement and any original input owners remain separate obligations.

use super::Producer;
use crate::cargo_cache::SourceCacheGuard;
use aterm_uds::private_file::PrivateDirectory;
use std::{
    io,
    os::fd::{AsFd as _, AsRawFd as _, BorrowedFd},
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

/// One actual newly created producer, private Cargo cache and original endpoint.
/// No path, prior record, arbitrary guard or input-use stand-in constructs it.
/// Drop quarantines its namespace and closes its cache guard; that is not native
/// child/cohort settlement, cleanup, reclamation or permission to adopt the cache.
#[must_use = "retain this same workspace beside original inputs through native settlement"]
pub struct MaterializationWorkspace {
    // Declaration order keeps producer quarantine before the cache guard closes.
    producer: Producer,
    cache: SourceCacheGuard,
    directory: PrivateDirectory,
    cargo_home: PathBuf,
    deadline: Instant,
    revoked: AtomicBool,
}

impl MaterializationWorkspace {
    /// Create only a fresh namespace and empty private Cargo cache. Existing
    /// namespaces are never adopted. An established producer is quarantined on
    /// failure; any partial namespace is retained and remains nonadoptable even
    /// when failure preceded producer construction. No child is dispatched.
    ///
    /// # Errors
    /// Expired endpoint, invalid/nonprivate parent, namespace/cache creation,
    /// original cache locking or final original-identity check refuses.
    pub fn create(parent: &Path, name: &str, deadline: Instant) -> io::Result<Self> {
        Self::create_with(parent, name, deadline, |_| Ok(()))
    }

    /// Add the actual readonly original-parent restriction before any mkdir.
    /// This cannot adopt an existing namespace or confer input meaning.
    /// # Errors
    /// As [`Self::create`], or the parent differs from the retained original.
    pub fn create_original_parent(
        parent: &Path,
        name: &str,
        original: BorrowedFd<'_>,
        deadline: Instant,
    ) -> io::Result<Self> {
        Self::create_with_parent(parent, name, Some(original), deadline, |_| Ok(()))
    }

    fn create_with(
        parent: &Path,
        name: &str,
        deadline: Instant,
        after_cache: impl FnOnce(&Path) -> io::Result<()>,
    ) -> io::Result<Self> {
        Self::create_with_parent(parent, name, None, deadline, after_cache)
    }

    fn create_with_parent(
        parent: &Path,
        name: &str,
        original: Option<BorrowedFd<'_>>,
        deadline: Instant,
        after_cache: impl FnOnce(&Path) -> io::Result<()>,
    ) -> io::Result<Self> {
        before(deadline)?;
        let producer = match original {
            Some(original) => Producer::create_original_parent(parent, name, original)?,
            None => Producer::create(parent, name)?,
        };
        let cargo_home = producer.path().join("cargo-home");
        let contents = &producer
            .inner
            .as_ref()
            .expect("same original producer")
            .contents;
        aterm_dirfd::mkdirat(contents, "cargo-home", aterm_dirfd::Mode::from_bits(0o700))?;
        let original = super::directory(contents, "cargo-home")?;
        let directory = PrivateDirectory::from_file(aterm_uds::kept_out(|| original.try_clone())?)?;
        producer.check_current()?;
        after_cache(&cargo_home)?;
        before(deadline)?;
        if !directory.is_at(&cargo_home)? {
            return Err(io::Error::other(
                "fresh Cargo home changed before acquisition",
            ));
        }
        let cache =
            SourceCacheGuard::try_acquire_original_directory(&cargo_home, original.as_fd())?;
        let workspace = Self {
            producer,
            cache,
            directory,
            cargo_home,
            deadline,
            revoked: AtomicBool::new(false),
        };
        workspace.check_current()?;
        Ok(workspace)
    }

    /// Fresh output location only; neither its name nor its contents are a seed.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.producer.path()
    }

    /// The private cache held by this same fresh producer.
    #[must_use]
    pub fn cargo_home(&self) -> &Path {
        &self.cargo_home
    }

    /// Original finite endpoint; rechecking cannot extend it.
    #[must_use]
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Borrow the actual producer's original readonly contents-directory FD.
    /// This does not reopen a path, create an input use, or grant execution,
    /// source, seed, SDK, completion or availability authority. A native caller
    /// must retain this workspace and its separate original resource owners
    /// through every child/descendant closure before interpreting output.
    ///
    /// Callback error or unwind quarantines this same workspace and retains its
    /// guards. Original namespace/cache/descriptor checks bookend the callback.
    /// # Errors
    /// Original ownership, descriptor-role or endpoint drift, callback failure,
    /// or earlier quarantine refuses. The same cache lock remains retained.
    pub fn with_contents_directory<T>(
        &self,
        action: impl FnOnce(BorrowedFd<'_>) -> io::Result<T>,
    ) -> io::Result<T> {
        let original = self
            .producer
            .inner
            .as_ref()
            .expect("same original workspace producer")
            .contents
            .as_fd();
        let check = || {
            self.check_current()?;
            contents_role(original)
        };
        if let Err(error) = check() {
            self.quarantine();
            return Err(error);
        }
        let result = catch_unwind(AssertUnwindSafe(|| action(original)));
        let final_check = check();
        if final_check.is_err() {
            self.quarantine();
        }
        match result {
            Ok(Ok(value)) => final_check.map(|()| value),
            Ok(Err(error)) => {
                self.quarantine();
                Err(error)
            }
            Err(panic) => {
                self.quarantine();
                resume_unwind(panic)
            }
        }
    }

    /// Observe the same original namespace/cache under the same endpoint.
    /// Failure permanently quarantines this workspace while retaining its guards.
    ///
    /// # Errors
    /// Original identity drift, lost owner, expiration or earlier quarantine.
    pub fn check_current(&self) -> io::Result<()> {
        if self.revoked.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "original materialization workspace is quarantined",
            ));
        }
        let current = (|| {
            before(self.deadline)?;
            self.producer.check_current()?;
            if !self.directory.is_at(&self.cargo_home)? {
                return Err(io::Error::other("original fresh Cargo directory changed"));
            }
            self.cache.check_current()?;
            if !self.directory.is_at(&self.cargo_home)? {
                return Err(io::Error::other("original fresh Cargo directory changed"));
            }
            self.producer.check_current()?;
            before(self.deadline)
        })();
        if current.is_err() {
            self.quarantine();
        }
        current
    }

    /// Revoke this same workspace without stopping a child or releasing guards.
    /// The original cache lock remains held until this actual bundle is dropped.
    pub fn quarantine(&self) {
        self.revoked.store(true, Ordering::Release);
        self.producer
            .inner
            .as_ref()
            .expect("same original workspace producer")
            .quarantine();
    }
}

fn contents_role(directory: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: fcntl observes only this borrowed actual open descriptor.
    let flags = unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: this observes the same borrowed descriptor without changing it.
    let descriptor_flags = unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_GETFD) };
    if descriptor_flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if flags & libc::O_ACCMODE != libc::O_RDONLY || descriptor_flags & libc::FD_CLOEXEC == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "original contents description is not readonly and close-on-exec",
        ));
    }
    Ok(())
}

fn before(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(io::ErrorKind::TimedOut.into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "workspace/contents_tests.rs"]
mod contents_tests;
