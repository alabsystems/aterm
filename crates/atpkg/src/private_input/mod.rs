// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Lifetime custody for a newly produced private input, separate from its meaning.
//!
//! Only creating a fresh container constructs an owner. An existing path, record,
//! digest or inherited descriptor cannot reopen it as a live owner. The contents
//! still need their producer's ordinary source/seed validation: an owned directory
//! is neither a canonical seed nor a qualified SDK. No reclaim or adoption path is
//! provided. Lost or uncertain ownership leaves the directory in place.

use aterm_dirfd::{self as fd, Mode, OFlags};
use aterm_uds::private_file::PrivateDirectory;
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

const CONTENTS: &str = "contents";

mod materialization;
pub use materialization::MaterializationAttempt;
mod preparation_inputs;
pub use preparation_inputs::PreparationInputs;

const LOCK: &str = ".input-owner.lock";
const STATE: &str = ".input-owner";
const BUILDING: &[u8] = b"aterm-private-input-v1\nmaterializing\n";
const READY: &[u8] = b"aterm-private-input-v1\nlive-original-owner\n";
const RETIRED: &[u8] = b"aterm-private-input-v1\nretired-by-original-owner\n";
const QUARANTINED: &[u8] = b"aterm-private-input-v1\nquarantined\n";

fn refused(reason: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, reason)
}

fn inode(file: &File) -> io::Result<(u64, u64)> {
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

fn directory(parent: &File, name: &str) -> io::Result<File> {
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

struct Inner {
    parent: PrivateDirectory,
    parent_path: PathBuf,
    root: PrivateDirectory,
    root_file: File,
    root_path: PathBuf,
    contents: File,
    contents_path: PathBuf,
    lock: Option<aterm_toolchain::record_lock::RecordLock>,
    process: u32,
    live: AtomicBool,
}

impl Inner {
    fn check(&self, expected: &[u8]) -> io::Result<()> {
        let result = self.check_inner(expected);
        if result.is_err() {
            self.quarantine();
        }
        result
    }

    fn check_inner(&self, expected: &[u8]) -> io::Result<()> {
        if self.process != std::process::id() {
            return Err(refused("private input owner belongs to another process"));
        }
        if !self.live.load(Ordering::Acquire) {
            return Err(refused("original private input owner was lost"));
        }
        if !self.parent.is_at(&self.parent_path)? || !self.root.is_at(&self.root_path)? {
            return Err(refused("original private input namespace changed"));
        }
        let named_root = File::from(fd::openat(
            fd::CWD,
            &self.root_path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        if inode(&named_root)? != inode(&self.root_file)?
            || inode(&directory(&self.root_file, CONTENTS)?)? != inode(&self.contents)?
        {
            return Err(refused("original private input directory changed"));
        }
        let named_lock = File::from(fd::openat(
            &self.root_file,
            LOCK,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        let metadata = named_lock.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || inode(&named_lock)? != inode(self.lock.as_ref().expect("original held lock"))?
        {
            return Err(refused("original private input lock changed"));
        }
        let view = self.root.view()?;
        // Private-file admission checks ownership, ACL, mode and hard links
        // independently of the original kernel lock's identity.
        if !view.read(LOCK, 0).map_err(io::Error::other)?.is_empty()
            || view.read(STATE, 128).map_err(io::Error::other)? != expected
        {
            return Err(refused("original private input lifecycle state changed"));
        }
        if !self.live.load(Ordering::Acquire) {
            return Err(refused("original private input owner was lost"));
        }
        Ok(())
    }

    fn quarantine(&self) {
        if self.process != std::process::id() {
            return;
        }
        self.live.store(false, Ordering::Release);
        // The original descriptor-relative directory is used even after a path
        // replacement. A failed durable write never restores the live owner;
        // no API adopts even an unchanged old record as a successful owner.
        if let Ok(view) = self.root.view() {
            let _ = view.write(STATE, QUARANTINED);
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        if self.process != std::process::id() {
            // A fork copy cannot run LOCK_UN on the original shared open-file
            // description. Its kernel copies close at process exit; neither
            // foreign Drop nor an inherited record can retire the parent owner.
            std::mem::forget(self.lock.take());
        }
    }
}

/// Actual materialization custody for one fresh container. Drop quarantines it.
/// This value supplies no source, signature, SDK or execution authority.
#[must_use = "preserve the original producer through materialization"]
pub struct Producer {
    inner: Option<Arc<Inner>>,
}

/// A failed consuming transition retains its exact original input.
#[must_use = "an error does not release or replace original custody"]
pub struct Refused<T> {
    error: io::Error,
    original: T,
}

impl<T> Refused<T> {
    pub(crate) fn new(error: io::Error, original: T) -> Self {
        Self { error, original }
    }

    /// The original transition error.
    #[must_use]
    pub const fn error(&self) -> &io::Error {
        &self.error
    }

    /// Recover the same original owner; no pathname recovery is attempted.
    #[must_use]
    pub fn into_parts(self) -> (io::Error, T) {
        (self.error, self.original)
    }
}

impl<T> fmt::Debug for Refused<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Refused")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl Producer {
    /// Create a new private container under an existing private absolute parent.
    /// Partial creation is retained on failure. Existing entries are never adopted.
    ///
    /// # Errors
    /// The name/path/privacy is invalid, the name exists, or anchored creation,
    /// locking, durable publication or original-identity checks fail.
    pub fn create(parent_path: &Path, name: &str) -> io::Result<Self> {
        Self::create_with(parent_path, name, |_| Ok(()))
    }

    fn create_with(
        parent_path: &Path,
        name: &str,
        after_create: impl FnOnce(&Path) -> io::Result<()>,
    ) -> io::Result<Self> {
        if !parent_path.is_absolute()
            || name.contains('/')
            || name.contains('\0')
            || !matches!(
                (
                    Path::new(name).components().next(),
                    Path::new(name).components().count()
                ),
                (Some(Component::Normal(_)), 1)
            )
            || name.len() > 128
        {
            return Err(refused(
                "private input requires an absolute parent and one bounded name",
            ));
        }
        let parent_file = File::from(fd::openat(
            fd::CWD,
            parent_path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        let parent = PrivateDirectory::from_file(aterm_uds::kept_out(|| parent_file.try_clone())?)?;
        if !parent.is_at(parent_path)? {
            return Err(refused("private input parent changed before creation"));
        }
        fd::mkdirat(&parent_file, name, Mode::from_bits(0o700))?;
        let root_path = parent_path.join(name);
        let root_file = directory(&parent_file, name)?;
        after_create(&root_path)?;
        let root = PrivateDirectory::from_file(aterm_uds::kept_out(|| root_file.try_clone())?)?;
        fd::mkdirat(&root_file, CONTENTS, Mode::from_bits(0o700))?;
        let contents = directory(&root_file, CONTENTS)?;
        let lock_file = File::from(fd::openat(
            &root_file,
            LOCK,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_bits(0o600),
        )?);
        let lock = aterm_toolchain::record_lock::RecordLock::try_lock(lock_file, 0)
            .map_err(|error| io::Error::other(format!("private input ownership lock: {error}")))?;
        root.view()?.write(STATE, BUILDING)?;
        let inner = Arc::new(Inner {
            parent,
            parent_path: parent_path.to_owned(),
            root,
            root_file,
            contents_path: root_path.join(CONTENTS),
            root_path,
            contents,
            lock: Some(lock),
            process: std::process::id(),
            live: AtomicBool::new(true),
        });
        inner.check(BUILDING)?;
        Ok(Self { inner: Some(inner) })
    }

    /// The new materialization destination, not a validated SDK or source root.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.inner.as_ref().expect("live producer").contents_path
    }

    /// Recheck the actual original materializing namespace and lifecycle lock.
    /// This permits a retained materialization operation to check its producer
    /// before dispatch and after native completion, without publishing READY.
    /// It does not inspect or authenticate the contents.
    /// # Errors
    /// Original identity/state drift or loss of the creating owner refuses.
    pub fn check_current(&self) -> io::Result<()> {
        self.inner.as_ref().expect("live producer").check(BUILDING)
    }

    /// Transfer this same owner after the caller finishes materialization.
    /// This records only the lifecycle transition, never content validation.
    ///
    /// # Errors
    /// Namespace/state drift or durable publication failure; the original
    /// producer is returned and remains responsible for its partial materialization.
    pub fn finish(mut self) -> Result<Owner, Refused<Self>> {
        let inner = self.inner.as_ref().expect("live producer");
        let result = (|| {
            inner.check(BUILDING)?;
            inner.root.view()?.write(STATE, READY)?;
            inner.check(READY)
        })();
        match result {
            Ok(()) => Ok(Owner {
                inner: self.inner.take(),
            }),
            Err(error) => {
                inner.quarantine();
                Err(Refused::new(error, self))
            }
        }
    }
}

impl Drop for Producer {
    fn drop(&mut self) {
        if let Some(inner) = &self.inner {
            inner.quarantine();
        }
    }
}

/// The original live lifecycle owner. No path/record/FD constructor exists.
#[must_use = "keep the original owner beside its users"]
pub struct Owner {
    inner: Option<Arc<Inner>>,
}

impl Owner {
    /// Recheck the original private namespace, lock and lifecycle state.
    /// # Errors
    /// Any drift, unreadable state or lost original owner refuses.
    pub fn check_current(&self) -> io::Result<()> {
        self.inner.as_ref().expect("live owner").check(READY)
    }

    /// Borrow this actual owner into an independently admitted native operation.
    /// # Errors
    /// As [`Self::check_current`]. This grants no scheduling or SDK authority.
    pub fn lend(&self) -> io::Result<InputUse> {
        self.check_current()?;
        let held = InputUse {
            inner: Arc::clone(self.inner.as_ref().expect("live owner")),
        };
        held.check_current()?;
        Ok(held)
    }

    /// Retire the same original lifecycle only after every actual use was dropped.
    /// This is a local ownership bookend, not a child/cohort settlement receipt.
    /// No files are removed, and the retained directory cannot be reopened as an owner.
    ///
    /// # Errors
    /// The original state changed, any use is outstanding, or durable publication
    /// fails. The original owner is returned; no failed transition enables reuse.
    pub fn retire(mut self) -> Result<Retired, Refused<Self>> {
        let inner = self.inner.as_ref().expect("live owner");
        let result = (|| {
            inner.check(READY)?;
            // This method consumes the non-Clone Owner, so no concurrent lend
            // can start. Existing InputUse values can only drop their own Arcs.
            if Arc::strong_count(inner) != 1 {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "private input still has actual users",
                ));
            }
            inner.root.view()?.write(STATE, RETIRED)?;
            inner.check(RETIRED)
        })();
        match result {
            Ok(()) => Ok(Retired {
                inner: self.inner.take().expect("same original owner"),
            }),
            Err(error) => {
                if error.kind() != io::ErrorKind::WouldBlock {
                    inner.quarantine();
                }
                Err(Refused::new(error, self))
            }
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        if let Some(inner) = &self.inner {
            inner.quarantine();
        }
    }
}

/// Original local lifecycle retirement with no outstanding use guards.
/// This grants no recursive deletion, SDK validation or foreign-cohort authority.
pub struct Retired {
    inner: Arc<Inner>,
}

impl Retired {
    /// Original retained container; no path lookup constructs this bookend.
    #[must_use]
    pub fn container(&self) -> &Path {
        &self.inner.root_path
    }

    /// Recheck the original retired namespace and its same held lock.
    /// # Errors
    /// Drift, unreadable state or loss of the original namespace refuses.
    pub fn check_current(&self) -> io::Result<()> {
        self.inner.check(RETIRED)
    }
}

/// A non-serialized use guard over the same original lifecycle lock.
/// A retained native operation keeps this value until its actual settlement.
#[must_use = "retain this actual guard through native settlement"]
pub struct InputUse {
    inner: Arc<Inner>,
}

impl InputUse {
    /// Original contents path; it is not canonical-source or SDK evidence.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.inner.contents_path
    }

    /// Recheck namespace/state custody, not recursive content equality.
    /// # Errors
    /// As [`Owner::check_current`].
    pub fn check_current(&self) -> io::Result<()> {
        self.inner.check(READY)
    }
}

#[cfg(test)]
mod tests;
