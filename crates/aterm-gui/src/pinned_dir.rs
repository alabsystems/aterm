// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Directory handle capabilities (`PinnedDir`) retained across asynchronous artifact writes.
//!
//! Opens each ancestor component without following symlinks ([`PinnedDir::child`]) and anchors
//! file creation, rename, and identity-checked removal to retained directory handles.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

use crate::durable::Flush;

/// How a new private file is published: the flush that finishes its bytes, and
/// when its parent's entries are synced ([`ParentSync`]).
#[derive(Clone, Copy)]
struct Publication {
    flush: Flush,
    parent: ParentSync,
}

/// When a new private file's parent entries are synced, against the rename that shows it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ParentSync {
    /// After the rename, and the published name is then revalidated against
    /// the retained path (and removed again if that fails).
    AfterPublish,
    /// Left to the caller's batch barrier (a video recording's [`PinnedDir::sync`] before its marker).
    Deferred,
    /// Before the rename, the path checked on each side of the sync while
    /// the file is invisible: the rename that shows it is the last call that
    /// reaches the directory by path, so a completion marker's consumer may
    /// remove the directory the moment the marker appears (where no no-replace
    /// rename exists, the link that shows it is followed by the unlink of its
    /// temporary name). A best-effort sync of the retained handle then attempts
    /// to persist the entry; it names no path, and its failure leaves the marker
    /// in place and the visible publication successful.
    BeforePublish,
}

/// Whether `name` is one ordinary path component that every backend may hand a handle-relative call.
pub(crate) fn is_one_component(name: &OsStr) -> bool {
    validate_component(name).is_ok()
}

/// Refuse any name that is not one ordinary path component: the empty name, `.`, `..`, any
/// separator (`/`, and `\` on Windows) and NUL.
fn validate_component(name: &OsStr) -> io::Result<()> {
    let bytes = name.as_encoded_bytes();
    let separator = |byte: u8| byte == b'/' || (cfg!(windows) && byte == b'\\');
    if bytes.is_empty()
        || bytes == b"."
        || bytes == b".."
        || bytes.iter().any(|&byte| byte == 0 || separator(byte))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact name must be one ordinary path component",
        ));
    }
    #[cfg(windows)]
    if !windows_component_is_safe(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact name is an unsafe Windows device/stream alias",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn windows_component_is_safe(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    if name.contains(':') || name.ends_with('.') || name.ends_with(' ') {
        return false;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(&['.', ' '][..]);
    let upper = stem.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
    ) {
        return false;
    }
    let suffix = upper
        .strip_prefix("COM")
        .or_else(|| upper.strip_prefix("LPT"));
    !suffix.is_some_and(|suffix| {
        matches!(
            suffix,
            "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
        )
    })
}

/// A retained entry's name now reaches a different object: a real device/inode
/// (volume/file-index) mismatch, never a failure to look.
fn identity_changed() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "confined artifact directory identity changed",
    )
}

/// A failed reopen of a retained entry's name to compare its identity: a name
/// that reaches nothing is the entry having vanished, a case of its own beside
/// [`identity_changed`], and every other failure (`EMFILE`, `EINTR`, ...) is
/// returned as it came, errno and all.
fn reopen_failed(error: io::Error) -> io::Error {
    if error.kind() == io::ErrorKind::NotFound {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("confined artifact entry vanished: {error}"),
        )
    } else {
        error
    }
}

/// A failed `O_NOFOLLOW` reopen (with `O_DIRECTORY` for a directory) of a
/// retained entry's name: `ELOOP` (a symlink) and `ENOTDIR` (not a directory)
/// prove the name now reaches another kind of object, a real mismatch
/// (`Ok(false)`); any other failure is [`reopen_failed`].
#[cfg(unix)]
fn reopen_mismatch(error: io::Error) -> io::Result<bool> {
    match error.raw_os_error() {
        Some(libc::ELOOP | libc::ENOTDIR) => Ok(false),
        _ => Err(reopen_failed(error)),
    }
}

#[cfg(test)]
std::thread_local! {
    /// The errno the next identity comparison on this thread fails with ([`fail_next_identity_probe`]).
    static IDENTITY_PROBE_FAULT: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
}

/// Fail the next identity comparison on this thread with `errno`, as a kernel
/// out of descriptors or interrupted mid-call would.
#[cfg(test)]
fn fail_next_identity_probe(errno: i32) {
    IDENTITY_PROBE_FAULT.set(Some(errno));
}

/// The fault [`fail_next_identity_probe`] armed, taken once; `Ok` outside tests.
fn identity_probe_fault() -> io::Result<()> {
    #[cfg(test)]
    if let Some(errno) = IDENTITY_PROBE_FAULT.take() {
        return Err(io::Error::from_raw_os_error(errno));
    }
    Ok(())
}

const REMOVE_MAX_ENTRIES: usize = 16_384;
/// Descriptor allowance left beside an initial pinned chain by artifact admission.
pub(crate) const PINNED_DIR_OPERATION_DESCRIPTOR_UNITS: usize = 8;
/// A wire-published video prune can already own the fresh recording, index,
/// marker, and candidate recording (four handles).
const REMOVE_LIVE_FIXED_UNITS: usize = 4;
const REMOVE_MAX_DEPTH: usize = PINNED_DIR_OPERATION_DESCRIPTOR_UNITS - REMOVE_LIVE_FIXED_UNITS - 1;

pub(crate) struct ClearContentsOutcome {
    pub(crate) result: io::Result<()>,
    pub(crate) removed: usize,
}

/// Maximum absolute-path depth an initial pin may retain.
pub(crate) const PINNED_DIR_OPEN_COMPONENT_LIMIT: usize =
    64 - PINNED_DIR_OPERATION_DESCRIPTOR_UNITS;

fn validate_open_path(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pinned directory must be absolute",
        ));
    }
    if path.components().count() > PINNED_DIR_OPEN_COMPONENT_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pinned directory exceeds the artifact descriptor budget",
        ));
    }
    Ok(())
}

fn admission_refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "pinned directory descriptor admission refused",
    )
}

#[cfg(test)]
std::thread_local! {
    static PINNED_CHAIN_OPEN_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn note_pinned_chain_open() {
    PINNED_CHAIN_OPEN_COUNT.set(PINNED_CHAIN_OPEN_COUNT.get() + 1);
}

#[cfg(all(test, unix))]
fn reset_pinned_chain_open_count() {
    PINNED_CHAIN_OPEN_COUNT.set(0);
}

#[cfg(all(test, unix))]
fn pinned_chain_open_count() -> usize {
    PINNED_CHAIN_OPEN_COUNT.get()
}

#[cfg(unix)]
mod imp {
    use std::io::{Read as _, Write as _};
    use std::path::Component;
    use std::sync::Arc;

    use crate::durable::Flush;

    use aterm_dirfd::OwnedFd;
    #[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
    use aterm_dirfd::linkat;
    use aterm_dirfd::{
        AtFlags, CWD, Dir, FileType, Mode, OFlags, fchmod, fstat, mkdirat, openat, renameat,
        unlinkat,
    };
    #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
    use aterm_dirfd::{RenameFlags, renameat_with};

    use super::{
        OsStr, OsString, ParentSync, Path, PathBuf, Publication, identity_changed,
        identity_probe_fault, io, reopen_mismatch, validate_component, validate_open_path,
    };

    fn directory_flags() -> OFlags {
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC
    }

    fn file_read_flags() -> OFlags {
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC
    }

    fn open_directory_at<Fd: aterm_dirfd::AsFd>(
        parent: Fd,
        name: impl aterm_dirfd::Arg,
    ) -> io::Result<OwnedFd> {
        aterm_uds::kept_out(|| {
            openat(parent, name, directory_flags(), Mode::empty()).map_err(io::Error::from)
        })
    }

    fn is_directory(fd: &impl aterm_dirfd::AsFd) -> io::Result<bool> {
        Ok(FileType::from_raw_mode(fstat(fd)?.st_mode).is_dir())
    }

    fn is_regular_file(fd: &impl aterm_dirfd::AsFd) -> io::Result<bool> {
        Ok(FileType::from_raw_mode(fstat(fd)?.st_mode).is_file())
    }

    /// Whether two descriptors reach one inode: `Ok(false)` is a real
    /// device/inode mismatch, and a failed `fstat` returns its errno.
    fn same_identity(
        left: &impl aterm_dirfd::AsFd,
        right: &impl aterm_dirfd::AsFd,
    ) -> io::Result<bool> {
        identity_probe_fault()?;
        let (left, right) = (fstat(left)?, fstat(right)?);
        Ok(left.st_dev == right.st_dev && left.st_ino == right.st_ino)
    }

    #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
    fn rename_no_replace_with_hook(
        directory: &impl aterm_dirfd::AsFd,
        from: &OsStr,
        to: &OsStr,
        after_publish: impl FnOnce(),
    ) -> io::Result<()> {
        renameat_with(directory, from, directory, to, RenameFlags::NOREPLACE)
            .map_err(io::Error::from)?;
        after_publish();
        Ok(())
    }

    #[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
    fn rename_no_replace_with_hook(
        directory: &impl aterm_dirfd::AsFd,
        from: &OsStr,
        to: &OsStr,
        after_publish: impl FnOnce(),
    ) -> io::Result<()> {
        linkat(directory, from, directory, to, AtFlags::empty()).map_err(io::Error::from)?;
        after_publish();
        unlinkat(directory, from, AtFlags::empty()).map_err(io::Error::from)
    }

    /// Whether the retained directory lost its last link; a failure to tell
    /// returns its errno.
    #[cfg(not(target_vendor = "apple"))]
    fn directory_was_unlinked(directory: &impl aterm_dirfd::AsFd) -> io::Result<bool> {
        Ok(fstat(directory)?.st_nlink == 0)
    }

    /// Whether the retained directory is no longer reachable at the path the
    /// kernel last knew it by; a failure to tell returns its errno.
    #[cfg(target_vendor = "apple")]
    fn directory_was_unlinked(directory: &impl aterm_dirfd::AsFd) -> io::Result<bool> {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::ffi::OsStrExt as _;

        let mut path = [0_i8; libc::PATH_MAX as usize];
        // SAFETY: the retained descriptor is live and `path` is writable
        // storage large enough for Darwin's F_GETPATH contract.
        if unsafe {
            libc::fcntl(
                directory.as_fd().as_raw_fd(),
                libc::F_GETPATH,
                path.as_mut_ptr(),
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: Successful F_GETPATH writes a NUL-terminated path.
        let path = unsafe { std::ffi::CStr::from_ptr(path.as_ptr()) };
        let path = OsStr::from_bytes(path.to_bytes());
        match open_directory_at(CWD, path) {
            Ok(current) => Ok(!same_identity(&current, directory)?),
            // Nothing there, a non-directory, or a symlink: no directory at
            // the old path, so the retained one is not reachable through it.
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(libc::ENOENT | libc::ENOTDIR | libc::ELOOP)
                ) =>
            {
                Ok(true)
            }
            Err(error) => Err(error),
        }
    }

    #[derive(Debug)]
    struct UnixPinned {
        root: Arc<OwnedFd>,
        components: Vec<(OsString, Arc<OwnedFd>)>,
    }

    /// An absolute directory and every no-follow ancestor used to reach it.
    #[derive(Clone, Debug)]
    pub(crate) struct PinnedDir {
        path: PathBuf,
        pinned: Arc<UnixPinned>,
    }

    #[derive(Debug)]
    pub(crate) struct PinnedFile {
        dir: PinnedDir,
        name: OsString,
        file: std::fs::File,
    }

    /// Copyable directory identity used by registries without retaining an extra descriptor.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct PinnedDirIdentity {
        dev: u64,
        ino: u64,
    }

    /// Recorded kernel identity for checkpoint revalidation after a descriptor closes.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct PinnedFileIdentity {
        dev: u64,
        ino: u64,
    }

    impl PinnedFileIdentity {
        fn from_file(file: &std::fs::File) -> io::Result<Self> {
            let stat = fstat(file).map_err(io::Error::from)?;
            if !FileType::from_raw_mode(stat.st_mode).is_file() || stat.st_nlink != 1 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "artifact source is not a singly-linked regular file",
                ));
            }
            Ok(Self {
                dev: stat.st_dev,
                ino: stat.st_ino,
            })
        }

        /// Reopen one name through an already-validated retained directory and
        /// compare its current device/inode.
        pub(crate) fn validate_at_retained(self, dir: &PinnedDir, name: &OsStr) -> io::Result<()> {
            if dir.private_file_identity_at_retained(name)? != self {
                return Err(identity_changed());
            }
            Ok(())
        }
    }

    impl PinnedFile {
        fn identity(&self) -> io::Result<PinnedFileIdentity> {
            PinnedFileIdentity::from_file(&self.file)
        }

        /// Record this open file's identity and close it.
        pub(crate) fn into_identity(self) -> io::Result<PinnedFileIdentity> {
            self.identity()
        }

        /// Whether this entry's name still reaches the retained file:
        /// `Ok(false)` is a real device/inode mismatch (a symlink in its place
        /// included, [`reopen_mismatch`]), a name that reaches nothing is the
        /// entry having vanished ([`super::reopen_failed`]), and any other failure
        /// returns its errno.
        fn current_name_matches(&self) -> io::Result<bool> {
            let current = match openat(
                self.dir.leaf(),
                &self.name,
                file_read_flags(),
                Mode::empty(),
            ) {
                Ok(current) => current,
                Err(errno) => return reopen_mismatch(errno.into()),
            };
            same_identity(&current, &self.file)
        }

        /// Revalidate this entry against its already-retained parent directory.
        pub(crate) fn validate_entry_identity_at_retained(&self) -> io::Result<()> {
            let fd = openat(
                self.dir.leaf(),
                &self.name,
                file_read_flags(),
                Mode::empty(),
            )
            .map_err(io::Error::from)?;
            let stat = fstat(&fd).map_err(io::Error::from)?;
            let expected = fstat(&self.file).map_err(io::Error::from)?;
            if !FileType::from_raw_mode(stat.st_mode).is_file()
                || stat.st_nlink != 1
                || expected.st_nlink != 1
                || stat.st_dev != expected.st_dev
                || stat.st_ino != expected.st_ino
            {
                return Err(identity_changed());
            }
            Ok(())
        }

        pub(crate) fn validate_path_identity(&self) -> io::Result<()> {
            self.dir.validate_path_identity()?;
            self.validate_entry_identity_at_retained()
        }

        /// Remove through the retained parent and certify success only when the
        /// retained inode lost its final link.
        pub(crate) fn remove_exact(self) -> io::Result<()> {
            if !self.current_name_matches()? {
                return Err(identity_changed());
            }
            unlinkat(self.dir.leaf(), &self.name, AtFlags::empty()).map_err(io::Error::from)?;
            let stat = fstat(&self.file).map_err(io::Error::from)?;
            if stat.st_nlink != 0 {
                return Err(identity_changed());
            }
            Ok(())
        }

        /// Move into an exclusively owned recovery directory without replacing any destination.
        pub(crate) fn move_to_recovery(self, target: &PinnedDir, name: &OsStr) -> io::Result<()> {
            validate_component(name)?;
            self.validate_path_identity()?;
            target.validate_path_identity()?;
            #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
            {
                let identity = self.identity()?;
                renameat_with(
                    self.dir.leaf(),
                    &self.name,
                    target.leaf(),
                    name,
                    RenameFlags::NOREPLACE,
                )
                .map_err(io::Error::from)?;
                identity.validate_at_retained(target, name)?;
                self.dir.sync()?;
                target.sync()?;
                target.validate_path_identity()
            }
            #[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
            {
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "exclusive recovery move is unavailable",
                ))
            }
        }

        fn replace_as(mut self, name: &OsStr) -> io::Result<Self> {
            validate_component(name)?;
            if let Err(error) = self.validate_path_identity() {
                let _ = self.remove_exact();
                return Err(error);
            }
            if let Err(error) = renameat(self.dir.leaf(), &self.name, self.dir.leaf(), name)
                .map_err(io::Error::from)
            {
                let _ = self.remove_exact();
                return Err(error);
            }
            self.name = name.to_os_string();
            if let Err(error) = self.validate_path_identity() {
                let _ = self.remove_exact();
                return Err(error);
            }
            Ok(self)
        }

        /// Atomically publish a fully written temporary file at a previously
        /// absent final name.
        fn publish_as_new_with_hook(
            self,
            name: &OsStr,
            after_publish: impl FnOnce(),
        ) -> io::Result<Self> {
            self.publish_as_new_with_validation(name, after_publish, true)
        }

        /// Publish through the retained parent capability while checking only
        /// the temporary/final entry.
        fn publish_as_new_at_retained_with_hook(
            self,
            name: &OsStr,
            after_publish: impl FnOnce(),
        ) -> io::Result<Self> {
            self.publish_as_new_with_validation(name, after_publish, false)
        }

        fn publish_as_new_with_validation(
            mut self,
            name: &OsStr,
            after_publish: impl FnOnce(),
            validate_path: bool,
        ) -> io::Result<Self> {
            validate_component(name)?;
            let validate = |file: &Self| {
                if validate_path {
                    file.validate_path_identity()
                } else {
                    file.validate_entry_identity_at_retained()
                }
            };
            if let Err(error) = validate(&self) {
                let _ = self.remove_exact();
                return Err(error);
            }
            if let Err(error) =
                rename_no_replace_with_hook(self.dir.leaf(), &self.name, name, after_publish)
            {
                let _ = self.remove_exact();
                return Err(error);
            }
            self.name = name.to_os_string();
            if let Err(error) = validate(&self) {
                let _ = self.remove_exact();
                return Err(error);
            }
            Ok(self)
        }

        /// Publish a fully written temporary file at a previously absent final
        /// name as the last call on its directory ([`ParentSync::BeforePublish`]):
        /// the no-replace rename runs through the retained parent, so it is
        /// confined without a check after it.
        fn publish_as_new_last_with_hook(
            mut self,
            name: &OsStr,
            after_publish: impl FnOnce(),
        ) -> io::Result<Self> {
            validate_component(name)?;
            if let Err(error) = self
                .validate_path_identity()
                .and_then(|()| self.dir.sync())
                .and_then(|()| self.validate_path_identity())
            {
                let _ = self.remove_exact();
                return Err(error);
            }
            if let Err(error) =
                rename_no_replace_with_hook(self.dir.leaf(), &self.name, name, after_publish)
            {
                let _ = self.remove_exact();
                return Err(error);
            }
            self.name = name.to_os_string();
            // Attempt to persist the entry through the retained descriptor alone: an
            // fsync of a directory its consumer already removed still succeeds,
            // and a failure here must not take back a marker someone has seen.
            let _ = self.dir.sync();
            Ok(self)
        }
    }

    impl PinnedDir {
        /// Resolve a caller path once while holding the exact resulting directory.
        pub(crate) fn open_resolved(path: &Path) -> io::Result<Self> {
            Self::open_resolved_with_admission(path, |_| true)
        }

        /// Resolve the exact target.
        pub(crate) fn open_resolved_with_admission(
            path: &Path,
            admit: impl FnOnce(&Path) -> bool,
        ) -> io::Result<Self> {
            let expected = open_directory_at(CWD, path)?;
            if !is_directory(&expected)? {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "resolved path is not a directory",
                ));
            }
            let canonical = std::fs::canonicalize(path)?;
            let pinned = Self::open_with_admission(&canonical, admit)?;
            if !same_identity(&expected, pinned.leaf())? {
                return Err(identity_changed());
            }
            Ok(pinned)
        }

        #[cfg_attr(
            any(test, debug_assertions),
            aterm_spec_anchor::refines(
                machine = "AnchoredArtifactTransaction",
                action = "ConfinePin",
                project = "aterm_gui::artifact_transaction_conformance::project_anchored"
            )
        )]
        pub(crate) fn open(path: &Path) -> io::Result<Self> {
            validate_open_path(path)?;
            let path = path.to_path_buf();
            #[cfg(test)]
            super::note_pinned_chain_open();
            let root = open_directory_at(CWD, Path::new("/"))?;
            if !is_directory(&root)? {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "filesystem root is not a directory",
                ));
            }
            let root = Arc::new(root);
            let mut components: Vec<(OsString, Arc<OwnedFd>)> = Vec::new();
            for component in path.components() {
                let name = match component {
                    Component::RootDir => continue,
                    Component::Normal(name) => name,
                    _ => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "canonical directory contains a non-normal component",
                        ));
                    }
                };
                let parent = components
                    .last()
                    .map_or(root.as_ref(), |(_, directory)| directory.as_ref());
                #[cfg(test)]
                super::note_pinned_chain_open();
                let directory = open_directory_at(parent, name)?;
                if !is_directory(&directory)? {
                    return Err(io::Error::new(
                        io::ErrorKind::NotADirectory,
                        "pinned component is not a directory",
                    ));
                }
                components.push((name.to_os_string(), Arc::new(directory)));
            }
            let result = Self {
                path,
                pinned: Arc::new(UnixPinned { root, components }),
            };
            result.validate_path_identity()?;
            Ok(result)
        }

        fn leaf(&self) -> &OwnedFd {
            self.pinned
                .components
                .last()
                .map_or(self.pinned.root.as_ref(), |(_, directory)| {
                    directory.as_ref()
                })
        }

        #[must_use]
        pub(crate) fn path(&self) -> &Path {
            &self.path
        }

        pub(crate) fn retained_identity(&self) -> io::Result<PinnedDirIdentity> {
            let stat = fstat(self.leaf()).map_err(io::Error::from)?;
            Ok(PinnedDirIdentity {
                dev: stat.st_dev,
                ino: stat.st_ino,
            })
        }

        /// Reopen the exact retained directory inode for a cross-process advisory lock.
        pub(crate) fn open_directory_lock(&self) -> io::Result<std::fs::File> {
            Ok(std::fs::File::from(open_directory_at(self.leaf(), ".")?))
        }

        /// Open an existing direct-child advisory lease through this retained directory authority.
        pub(crate) fn open_existing_namespace_lock_at_retained(
            &self,
            name: &OsStr,
        ) -> io::Result<std::fs::File> {
            validate_component(name)?;
            let fd = aterm_uds::kept_out(|| {
                openat(
                    self.leaf(),
                    name,
                    OFlags::RDWR | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(io::Error::from)
            })?;
            let stat = fstat(&fd).map_err(io::Error::from)?;
            if !FileType::from_raw_mode(stat.st_mode).is_file() || stat.st_nlink != 1 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "namespace lock is not a singly-linked regular file",
                ));
            }
            Ok(std::fs::File::from(fd))
        }

        pub(crate) fn validate_path_identity(&self) -> io::Result<()> {
            let mut current = open_directory_at(CWD, Path::new("/"))?;
            if !same_identity(&current, self.pinned.root.as_ref())? {
                return Err(identity_changed());
            }
            for (name, expected) in &self.pinned.components {
                let next = open_directory_at(&current, name.as_os_str())?;
                if !is_directory(&next)? || !same_identity(&next, expected.as_ref())? {
                    return Err(identity_changed());
                }
                current = next;
            }
            Ok(())
        }

        /// The directory's entries, handed to the drive by a plain `fsync(2)` of the held
        /// descriptor: aterm-durable's, which astream-sys counts `plain`.
        pub(crate) fn sync(&self) -> io::Result<()> {
            aterm_durable::primitive::fsync(self.leaf())
        }

        /// Flush the directory's entries ordered ahead of every later write on the device.
        pub(crate) fn sync_ordered(&self) -> io::Result<()> {
            aterm_durable::ordered(&std::fs::File::from(self.leaf().try_clone()?))
        }

        pub(crate) fn child(&self, name: &OsStr) -> io::Result<Self> {
            validate_component(name)?;
            let child = open_directory_at(self.leaf(), name)?;
            if !is_directory(&child)? {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "pinned child is not a directory",
                ));
            }
            let mut components = self.pinned.components.clone();
            components.push((name.to_os_string(), Arc::new(child)));
            Ok(Self {
                path: self.path.join(name),
                pinned: Arc::new(UnixPinned {
                    root: Arc::clone(&self.pinned.root),
                    components,
                }),
            })
        }

        pub(crate) fn create_child(&self, name: &OsStr) -> io::Result<Self> {
            validate_component(name)?;
            mkdirat(self.leaf(), name, Mode::RWXU).map_err(io::Error::from)?;
            let child = self.child(name)?;
            fchmod(child.leaf(), Mode::RWXU).map_err(io::Error::from)?;
            self.sync()?;
            child.validate_path_identity()?;
            Ok(child)
        }

        fn write_private_inner_with_hook<F: FnOnce()>(
            &self,
            name: &OsStr,
            bytes: &[u8],
            after_create: &mut Option<F>,
            flush: Flush,
        ) -> io::Result<PinnedFile> {
            validate_component(name)?;
            let flags = OFlags::WRONLY
                | OFlags::CREATE
                | OFlags::EXCL
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::CLOEXEC;
            let fd = openat(self.leaf(), name, flags, Mode::RUSR | Mode::WUSR)
                .map_err(io::Error::from)?;
            let file = PinnedFile {
                dir: self.clone(),
                name: name.to_os_string(),
                file: std::fs::File::from(fd),
            };
            let prepare = (|| {
                let stat = fstat(&file.file).map_err(io::Error::from)?;
                if !FileType::from_raw_mode(stat.st_mode).is_file() || stat.st_nlink != 1 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "artifact target is not a singly-linked regular file",
                    ));
                }
                fchmod(&file.file, Mode::RUSR | Mode::WUSR).map_err(io::Error::from)
            })();
            if let Err(error) = prepare {
                let _ = file.remove_exact();
                return Err(error);
            }
            if let Some(after_create) = after_create.take() {
                after_create();
            }
            let result = (|| {
                (&file.file).write_all(bytes)?;
                crate::durable::finish(&file.file, flush)
            })();
            if let Err(error) = result {
                let _ = file.remove_exact();
                return Err(error);
            }
            Ok(file)
        }

        fn write_private_inner(&self, name: &OsStr, bytes: &[u8]) -> io::Result<PinnedFile> {
            let mut after_create = Some(|| {});
            self.write_private_inner_with_hook(name, bytes, &mut after_create, Flush::Ordered)
        }

        fn temporary_name(sequence: u64) -> OsString {
            OsString::from(format!(
                ".aterm-write-p{}-{sequence:020}",
                std::process::id()
            ))
        }

        #[cfg_attr(
            any(test, debug_assertions),
            aterm_spec_anchor::refines(
                machine = "AnchoredArtifactTransaction",
                action = "WritePinned",
                project = "aterm_gui::artifact_transaction_conformance::project_anchored"
            )
        )]
        pub(crate) fn write_private(&self, name: &OsStr, bytes: &[u8]) -> io::Result<PinnedFile> {
            self.write_private_authorized(name, bytes, || true)
        }

        pub(crate) fn write_private_authorized(
            &self,
            name: &OsStr,
            bytes: &[u8],
            authorize: impl FnOnce() -> bool,
        ) -> io::Result<PinnedFile> {
            validate_component(name)?;
            match self.pin_private_file(name) {
                Ok(existing) => drop(existing),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let mut authorize = Some(authorize);
            for _ in 0..32 {
                let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let temporary = Self::temporary_name(sequence);
                match self.write_private_inner(&temporary, bytes) {
                    Ok(file) => {
                        if !authorize
                            .take()
                            .expect("publication authorizer runs exactly once")(
                        ) {
                            let _ = file.remove_exact();
                            return Err(io::Error::new(
                                io::ErrorKind::Interrupted,
                                "artifact publication cancelled",
                            ));
                        }
                        let file = file.replace_as(name)?;
                        if let Err(error) = self.sync().and_then(|()| file.validate_path_identity())
                        {
                            let _ = file.remove_exact();
                            return Err(error);
                        }
                        return Ok(file);
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(error),
                }
            }
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "could not allocate a private replacement name",
            ))
        }

        pub(crate) fn write_new_private(
            &self,
            name: &OsStr,
            bytes: &[u8],
        ) -> io::Result<PinnedFile> {
            self.write_new_private_authorized(name, bytes, || true)
        }

        pub(crate) fn write_new_private_authorized(
            &self,
            name: &OsStr,
            bytes: &[u8],
            authorize: impl FnOnce() -> bool,
        ) -> io::Result<PinnedFile> {
            self.write_new_private_with_authorizer_and_hooks(
                name,
                bytes,
                authorize,
                || {},
                || {},
                Publication {
                    flush: Flush::Ordered,
                    parent: ParentSync::AfterPublish,
                },
            )
        }

        /// Publish a new private completion marker as the last call on this
        /// directory ([`ParentSync::BeforePublish`]): `after_publish` runs the
        /// moment the marker is visible.
        pub(crate) fn write_new_private_marker(
            &self,
            name: &OsStr,
            bytes: &[u8],
            after_publish: impl FnOnce(),
        ) -> io::Result<PinnedFile> {
            self.write_new_private_with_authorizer_and_hooks(
                name,
                bytes,
                || true,
                || {},
                after_publish,
                Publication {
                    flush: Flush::Ordered,
                    parent: ParentSync::BeforePublish,
                },
            )
        }

        /// Publish one file as part of a private batch whose caller supplies the
        /// parent-directory sync barrier before any visibility marker.
        pub(crate) fn write_new_private_deferred_dir_sync_authorized(
            &self,
            name: &OsStr,
            bytes: &[u8],
            authorize: impl FnOnce() -> bool,
        ) -> io::Result<PinnedFile> {
            self.write_new_private_with_authorizer_and_hooks(
                name,
                bytes,
                authorize,
                || {},
                || {},
                Publication {
                    flush: Flush::Ordered,
                    parent: ParentSync::Deferred,
                },
            )
        }

        /// Create an invisible temporary file finished with `flush`.
        pub(crate) fn write_new_private_with_hooks(
            &self,
            name: &OsStr,
            bytes: &[u8],
            after_temp_create: impl FnOnce(),
            after_publish: impl FnOnce(),
            flush: Flush,
        ) -> io::Result<PinnedFile> {
            self.write_new_private_with_authorizer_and_hooks(
                name,
                bytes,
                || true,
                after_temp_create,
                after_publish,
                Publication {
                    flush,
                    parent: ParentSync::AfterPublish,
                },
            )
        }

        fn write_new_private_with_authorizer_and_hooks(
            &self,
            name: &OsStr,
            bytes: &[u8],
            authorize: impl FnOnce() -> bool,
            after_temp_create: impl FnOnce(),
            after_publish: impl FnOnce(),
            publication: Publication,
        ) -> io::Result<PinnedFile> {
            let Publication { flush, parent } = publication;
            validate_component(name)?;
            static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let mut authorize = Some(authorize);
            let mut after_temp_create = Some(after_temp_create);
            let mut after_publish = Some(after_publish);
            for _ in 0..32 {
                let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let temporary = Self::temporary_name(sequence);
                match self.write_private_inner_with_hook(
                    &temporary,
                    bytes,
                    &mut after_temp_create,
                    flush,
                ) {
                    Ok(file) => {
                        if !authorize
                            .take()
                            .expect("publication authorizer runs exactly once")(
                        ) {
                            let _ = file.remove_exact();
                            return Err(io::Error::new(
                                io::ErrorKind::Interrupted,
                                "artifact publication cancelled",
                            ));
                        }
                        let after_publish = after_publish
                            .take()
                            .expect("publication hook runs exactly once");
                        return match parent {
                            ParentSync::AfterPublish => {
                                let file = file.publish_as_new_with_hook(name, after_publish)?;
                                if let Err(error) =
                                    self.sync().and_then(|()| file.validate_path_identity())
                                {
                                    let _ = file.remove_exact();
                                    return Err(error);
                                }
                                Ok(file)
                            }
                            ParentSync::Deferred => {
                                file.publish_as_new_at_retained_with_hook(name, after_publish)
                            }
                            ParentSync::BeforePublish => {
                                file.publish_as_new_last_with_hook(name, after_publish)
                            }
                        };
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
            }
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "could not allocate a private publication name",
            ))
        }

        /// Read a child through the retained directory capability and revalidate the child entry.
        #[cfg_attr(
            any(test, debug_assertions),
            aterm_spec_anchor::refines(
                machine = "AnchoredArtifactTransaction",
                action = "ReadPinned",
                project = "aterm_gui::artifact_transaction_conformance::project_anchored"
            )
        )]
        pub(crate) fn read_private_at_retained(
            &self,
            name: &OsStr,
            limit: usize,
        ) -> io::Result<(Vec<u8>, PinnedFile)> {
            validate_component(name)?;
            let fd = openat(self.leaf(), name, file_read_flags(), Mode::empty())
                .map_err(io::Error::from)?;
            let stat = fstat(&fd).map_err(io::Error::from)?;
            if !FileType::from_raw_mode(stat.st_mode).is_file() || stat.st_nlink != 1 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "artifact source is not a singly-linked regular file",
                ));
            }
            let mut file = std::fs::File::from(fd);
            let cap = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
            let mut bytes = Vec::with_capacity(limit.min(64 << 10).saturating_add(1));
            (&mut file).take(cap).read_to_end(&mut bytes)?;
            if bytes.len() > limit {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "artifact source exceeded its size bound",
                ));
            }
            let guard = PinnedFile {
                dir: self.clone(),
                name: name.to_os_string(),
                file,
            };
            guard.validate_entry_identity_at_retained()?;
            Ok((bytes, guard))
        }

        /// Pin a child through the retained directory capability without
        /// requiring the directory's former lexical path to remain current.
        pub(crate) fn pin_private_file_at_retained(&self, name: &OsStr) -> io::Result<PinnedFile> {
            validate_component(name)?;
            let fd = openat(self.leaf(), name, file_read_flags(), Mode::empty())
                .map_err(io::Error::from)?;
            let stat = fstat(&fd).map_err(io::Error::from)?;
            if !FileType::from_raw_mode(stat.st_mode).is_file() || stat.st_nlink != 1 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "artifact source is not a singly-linked regular file",
                ));
            }
            let guard = PinnedFile {
                dir: self.clone(),
                name: name.to_os_string(),
                file: std::fs::File::from(fd),
            };
            guard.validate_entry_identity_at_retained()?;
            Ok(guard)
        }

        /// Open one direct child through this retained directory, record its identity.
        pub(crate) fn private_file_identity_at_retained(
            &self,
            name: &OsStr,
        ) -> io::Result<PinnedFileIdentity> {
            validate_component(name)?;
            let fd = openat(self.leaf(), name, file_read_flags(), Mode::empty())
                .map_err(io::Error::from)?;
            PinnedFileIdentity::from_file(&std::fs::File::from(fd))
        }

        pub(crate) fn pin_private_file(&self, name: &OsStr) -> io::Result<PinnedFile> {
            let guard = self.pin_private_file_at_retained(name)?;
            guard.validate_path_identity()?;
            Ok(guard)
        }

        pub(crate) fn remove_file_if_exists(&self, name: &OsStr) -> io::Result<()> {
            validate_component(name)?;
            match unlinkat(self.leaf(), name, AtFlags::empty()) {
                Ok(()) => Ok(()),
                Err(aterm_dirfd::Errno::NOENT) => Ok(()),
                Err(error) => Err(error.into()),
            }
        }

        pub(crate) fn rename(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
            validate_component(from)?;
            validate_component(to)?;
            renameat(self.leaf(), from, self.leaf(), to).map_err(Into::into)
        }

        pub(crate) fn is_regular_file(&self, name: &OsStr) -> bool {
            validate_component(name).is_ok()
                && openat(self.leaf(), name, file_read_flags(), Mode::empty())
                    .ok()
                    .is_some_and(|fd| matches!(is_regular_file(&fd), Ok(true)))
        }

        /// Hard-bounded enumeration.
        #[cfg(test)]
        pub(crate) fn names(&self, limit: usize) -> io::Result<Vec<OsString>> {
            let mut directory = Dir::read_from(self.leaf()).map_err(io::Error::from)?;
            let mut names = Vec::with_capacity(limit.min(256));
            for entry in &mut directory {
                let entry = entry.map_err(io::Error::from)?;
                let bytes = entry.file_name().to_bytes();
                if bytes == b"." || bytes == b".." {
                    continue;
                }
                if names.len() == limit {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "artifact directory exceeded its entry bound",
                    ));
                }
                use std::os::unix::ffi::OsStrExt as _;
                names.push(OsStr::from_bytes(bytes).to_os_string());
            }
            Ok(names)
        }

        /// Visit at most `limit` entries' names in directory order, holding none of them.
        pub(crate) fn visit_names(
            &self,
            limit: usize,
            visit: &mut dyn FnMut(&OsStr),
        ) -> io::Result<bool> {
            let mut directory = Dir::read_from(self.leaf()).map_err(io::Error::from)?;
            let mut seen = 0usize;
            for entry in &mut directory {
                let entry = entry.map_err(io::Error::from)?;
                let bytes = entry.file_name().to_bytes();
                if bytes == b"." || bytes == b".." {
                    continue;
                }
                if seen == limit {
                    return Ok(true);
                }
                seen += 1;
                use std::os::unix::ffi::OsStrExt as _;
                visit(OsStr::from_bytes(bytes));
            }
            Ok(false)
        }

        /// Return at most `limit` entries without turning a larger directory
        /// into a permanent retention failure.
        pub(crate) fn names_up_to(&self, limit: usize) -> io::Result<Vec<OsString>> {
            let mut directory = Dir::read_from(self.leaf()).map_err(io::Error::from)?;
            let mut names = Vec::with_capacity(limit.min(256));
            for entry in &mut directory {
                let entry = entry.map_err(io::Error::from)?;
                let bytes = entry.file_name().to_bytes();
                if bytes == b"." || bytes == b".." {
                    continue;
                }
                if names.len() == limit {
                    break;
                }
                use std::os::unix::ffi::OsStrExt as _;
                names.push(OsStr::from_bytes(bytes).to_os_string());
            }
            Ok(names)
        }

        fn remove_contents(
            &self,
            budget: &mut usize,
            depth: usize,
            removed: &mut usize,
        ) -> io::Result<()> {
            if depth > super::REMOVE_MAX_DEPTH {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "artifact cleanup exceeded its directory-depth bound",
                ));
            }
            let names = self.names_up_to((*budget).saturating_add(1))?;
            let mut first_error = None;
            for name in names {
                if *budget == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "artifact cleanup exceeded its entry bound",
                    ));
                }
                *budget -= 1;
                let result = match self.child(&name) {
                    Ok(child) => child
                        .remove_contents(budget, depth + 1, removed)
                        .and_then(|()| {
                            if !self.current_child_matches(&name, &child)? {
                                return Err(identity_changed());
                            }
                            unlinkat(self.leaf(), &name, AtFlags::REMOVEDIR)
                                .map_err(io::Error::from)?;
                            *removed = removed.saturating_add(1);
                            if !directory_was_unlinked(child.leaf())? {
                                return Err(identity_changed());
                            }
                            Ok(())
                        }),
                    Err(_) => unlinkat(self.leaf(), &name, AtFlags::empty())
                        .map_err(io::Error::from)
                        .map(|()| *removed = removed.saturating_add(1)),
                };
                if let Err(error) = result
                    && first_error.is_none()
                {
                    first_error = Some(error);
                }
            }
            first_error.map_or(Ok(()), Err)
        }

        pub(crate) fn clear_contents_with_budget(
            &self,
            budget: &mut usize,
        ) -> super::ClearContentsOutcome {
            let mut removed = 0;
            let result = self.remove_contents(budget, 0, &mut removed);
            super::ClearContentsOutcome { result, removed }
        }

        /// Test-only: remove a child tree by name through the retained parent.
        #[cfg(test)]
        pub(crate) fn remove_child_tree(&self, name: &OsStr) -> io::Result<()> {
            validate_component(name)?;
            let child = match self.child(name) {
                Ok(child) => child,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error),
            };
            let mut budget = super::REMOVE_MAX_ENTRIES;
            let mut removed = 0;
            child.remove_contents(&mut budget, 0, &mut removed)?;
            if !self.current_child_matches(name, &child)? {
                return Err(identity_changed());
            }
            unlinkat(self.leaf(), name, AtFlags::REMOVEDIR).map_err(io::Error::from)?;
            if !directory_was_unlinked(child.leaf())? {
                return Err(identity_changed());
            }
            Ok(())
        }

        pub(crate) fn remove_child_tree_exact(&self, name: &OsStr, child: &Self) -> io::Result<()> {
            validate_component(name)?;
            let mut budget = super::REMOVE_MAX_ENTRIES;
            let mut removed = 0;
            child.remove_contents(&mut budget, 0, &mut removed)?;
            if !self.current_child_matches(name, child)? {
                return Err(identity_changed());
            }
            unlinkat(self.leaf(), name, AtFlags::REMOVEDIR).map_err(io::Error::from)?;
            if !directory_was_unlinked(child.leaf())? {
                return Err(identity_changed());
            }
            Ok(())
        }

        pub(crate) fn remove_empty_child_exact(
            &self,
            name: &OsStr,
            child: &Self,
        ) -> io::Result<()> {
            self.remove_empty_child_exact_with_hook_inner(name, child, || {})
        }

        fn remove_empty_child_exact_with_hook_inner(
            &self,
            name: &OsStr,
            child: &Self,
            after_check: impl FnOnce(),
        ) -> io::Result<()> {
            validate_component(name)?;
            if !self.current_child_matches(name, child)? {
                return Err(identity_changed());
            }
            after_check();
            unlinkat(self.leaf(), name, AtFlags::REMOVEDIR).map_err(io::Error::from)?;
            if !directory_was_unlinked(child.leaf())? {
                return Err(identity_changed());
            }
            Ok(())
        }

        #[cfg(test)]
        pub(crate) fn remove_empty_child_exact_with_hook(
            &self,
            name: &OsStr,
            child: &Self,
            after_check: impl FnOnce(),
        ) -> io::Result<()> {
            self.remove_empty_child_exact_with_hook_inner(name, child, after_check)
        }

        /// Whether `name` here still reaches the directory `expected` retains:
        /// `Ok(false)` is a real device/inode mismatch (a symlink or a
        /// non-directory in its place included, [`reopen_mismatch`]), a name
        /// that reaches nothing is the child having vanished
        /// ([`super::reopen_failed`]), and any other failure returns its errno.
        pub(crate) fn current_child_matches(
            &self,
            name: &OsStr,
            expected: &Self,
        ) -> io::Result<bool> {
            validate_component(name)?;
            let current = match open_directory_at(self.leaf(), name) {
                Ok(current) => current,
                Err(error) => return reopen_mismatch(error),
            };
            same_identity(&current, expected.leaf())
        }

        pub(crate) fn rename_child_exact(
            &self,
            from: &OsStr,
            expected: &Self,
            to: &OsStr,
        ) -> io::Result<()> {
            validate_component(from)?;
            validate_component(to)?;
            if !self.current_child_matches(from, expected)? {
                return Err(identity_changed());
            }
            self.rename(from, to)?;
            if !self.current_child_matches(to, expected)? {
                return Err(identity_changed());
            }
            Ok(())
        }

        /// A new private append-only file (the session journal's segment, `session_log_journal_fs`).
        pub(crate) fn create_new_append_private(&self, name: &OsStr) -> io::Result<std::fs::File> {
            self.create_new_private_with(name, OFlags::WRONLY | OFlags::APPEND)
        }

        /// A new private read-write file (the session journal's pending instance lock).
        pub(crate) fn create_new_private_rw(&self, name: &OsStr) -> io::Result<std::fs::File> {
            self.create_new_private_with(name, OFlags::RDWR)
        }

        fn create_new_private_with(
            &self,
            name: &OsStr,
            access: OFlags,
        ) -> io::Result<std::fs::File> {
            validate_component(name)?;
            let flags = access
                | OFlags::CREATE
                | OFlags::EXCL
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::CLOEXEC;
            let fd = aterm_uds::kept_out(|| {
                openat(self.leaf(), name, flags, Mode::RUSR | Mode::WUSR).map_err(io::Error::from)
            })?;
            let file = PinnedFile {
                dir: self.clone(),
                name: name.to_os_string(),
                file: std::fs::File::from(fd),
            };
            let prepare = (|| {
                let stat = fstat(&file.file).map_err(io::Error::from)?;
                if !FileType::from_raw_mode(stat.st_mode).is_file() || stat.st_nlink != 1 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "a new private file is not a singly-linked regular file",
                    ));
                }
                fchmod(&file.file, Mode::RUSR | Mode::WUSR).map_err(io::Error::from)
            })();
            if let Err(error) = prepare {
                let _ = file.remove_exact();
                return Err(error);
            }
            Ok(file.file)
        }

        /// An existing entry, or a new one, opened read-write with no follow.
        pub(crate) fn open_or_create_private_rw(&self, name: &OsStr) -> io::Result<std::fs::File> {
            validate_component(name)?;
            let flags = OFlags::RDWR
                | OFlags::CREATE
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::CLOEXEC;
            let fd = aterm_uds::kept_out(|| {
                openat(self.leaf(), name, flags, Mode::RUSR | Mode::WUSR).map_err(io::Error::from)
            })?;
            let file = std::fs::File::from(fd);
            if !is_regular_file(&file)? {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "a private file is not a regular file",
                ));
            }
            Ok(file)
        }

        /// An existing entry opened read-only with no follow and no wait, with its descriptor metadata.
        pub(crate) fn open_regular_at_retained(
            &self,
            name: &OsStr,
        ) -> io::Result<(std::fs::File, std::fs::Metadata)> {
            validate_component(name)?;
            let fd = openat(self.leaf(), name, file_read_flags(), Mode::empty())
                .map_err(io::Error::from)?;
            let file = std::fs::File::from(fd);
            let meta = file.metadata()?;
            if !meta.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "not a regular file",
                ));
            }
            Ok((file, meta))
        }

        /// Rename `from` to `to` within this directory, never over an existing `to`.
        pub(crate) fn rename_no_replace(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
            validate_component(from)?;
            validate_component(to)?;
            rename_no_replace_with_hook(self.leaf(), from, to, || {})
        }

        /// The retained directory's own metadata (its owner and mode).
        pub(crate) fn leaf_metadata(&self) -> io::Result<std::fs::Metadata> {
            std::fs::File::from(open_directory_at(self.leaf(), ".")?).metadata()
        }

        /// Set the retained directory's mode to 0700.
        pub(crate) fn restrict_to_owner(&self) -> io::Result<()> {
            fchmod(self.leaf(), Mode::RWXU).map_err(io::Error::from)
        }
    }

    impl PinnedFile {
        /// The pinned file's metadata (through its own descriptor).
        pub(crate) fn metadata(&self) -> io::Result<std::fs::Metadata> {
            self.file.metadata()
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::io::{Read as _, Write as _};
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    use std::os::windows::io::AsRawHandle as _;
    use std::sync::Arc;

    use crate::durable::Flush;

    use super::{
        OsStr, OsString, ParentSync, Path, PathBuf, Publication, identity_changed,
        identity_probe_fault, io, reopen_failed, validate_component, validate_open_path,
    };

    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
    const DELETE_ACCESS: u32 = 0x0001_0000;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    #[repr(C)]
    struct ByHandleFileInformation {
        file_attributes: u32,
        creation_time: [u32; 2],
        last_access_time: [u32; 2],
        last_write_time: [u32; 2],
        volume_serial_number: u32,
        file_size_high: u32,
        file_size_low: u32,
        number_of_links: u32,
        file_index_high: u32,
        file_index_low: u32,
    }

    unsafe extern "system" {
        fn GetFileInformationByHandle(
            file: *mut std::ffi::c_void,
            information: *mut ByHandleFileInformation,
        ) -> i32;
        fn GetFinalPathNameByHandleW(
            file: *mut std::ffi::c_void,
            path: *mut u16,
            path_len: u32,
            flags: u32,
        ) -> u32;
        fn FlushFileBuffers(file: *mut std::ffi::c_void) -> i32;
    }

    /// Where the object behind `file` lives NOW, as a `\\?\` path — the name
    /// it was opened by, so a hard link reports the link actually opened.
    fn final_path(file: &std::fs::File) -> io::Result<PathBuf> {
        use std::os::windows::ffi::OsStringExt as _;
        let mut buffer = vec![0_u16; 512];
        loop {
            let capacity = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
            // SAFETY: `file` owns a live handle and `buffer` has `capacity`
            // writable UTF-16 units; the return value is checked below.
            let written = unsafe {
                GetFinalPathNameByHandleW(
                    file.as_raw_handle().cast(),
                    buffer.as_mut_ptr(),
                    capacity,
                    0,
                )
            } as usize;
            if written == 0 {
                return Err(io::Error::last_os_error());
            }
            if written < buffer.len() {
                buffer.truncate(written);
                return Ok(PathBuf::from(OsString::from_wide(&buffer)));
            }
            buffer.resize(written, 0);
        }
    }

    /// Prove that `entry` — just opened by the path `<parent>\<name>` — is a
    /// direct child of the directory `parent` retains.
    fn ensure_direct_child(parent: &std::fs::File, entry: &std::fs::File) -> io::Result<()> {
        let entry_path = final_path(entry)?;
        if entry_path.parent() != Some(final_path(parent)?.as_path()) {
            return Err(identity_changed());
        }
        Ok(())
    }

    /// Flush a retained directory's entries: `FlushFileBuffers`, Windows' only
    /// flush, on the handle itself.
    fn flush_directory(directory: &std::fs::File) -> io::Result<()> {
        // SAFETY: `directory` owns a live handle for the duration of the call;
        // the call dereferences no pointer of ours.
        if unsafe { FlushFileBuffers(directory.as_raw_handle().cast()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct FileIdentity {
        volume: u32,
        index: u64,
        links: u32,
    }

    fn file_identity(file: &std::fs::File) -> io::Result<FileIdentity> {
        let mut information = std::mem::MaybeUninit::<ByHandleFileInformation>::uninit();
        // SAFETY: `file` owns a live kernel handle and the out pointer addresses
        // writable storage for exactly one BY_HANDLE_FILE_INFORMATION value.
        let ok = unsafe {
            GetFileInformationByHandle(file.as_raw_handle().cast(), information.as_mut_ptr())
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: A nonzero return initializes the full output structure.
        let information = unsafe { information.assume_init() };
        Ok(FileIdentity {
            volume: information.volume_serial_number,
            index: (u64::from(information.file_index_high) << 32)
                | u64::from(information.file_index_low),
            links: information.number_of_links,
        })
    }

    #[cfg(test)]
    pub(super) fn test_link_count(path: &Path) -> io::Result<u32> {
        let file = std::fs::OpenOptions::new()
            .access_mode(GENERIC_READ)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        Ok(file_identity(&file)?.links)
    }

    /// [`ensure_direct_child`] over two pathnames, opened the way the removal paths open them.
    #[cfg(test)]
    pub(super) fn test_is_direct_child(parent: &Path, entry: &Path) -> io::Result<()> {
        ensure_direct_child(&probe_directory(parent)?, &open_disposable(entry)?)
    }

    fn open_directory(path: &Path) -> io::Result<std::fs::File> {
        let file = std::fs::OpenOptions::new()
            .access_mode(GENERIC_READ)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "pinned path is a reparse point or not a directory",
            ));
        }
        Ok(file)
    }

    /// Open a directory this process will write into (and flush: the
    /// `GENERIC_WRITE` is what `FlushFileBuffers` needs).
    fn open_mutable_directory(path: &Path, share_delete: bool) -> io::Result<std::fs::File> {
        let share =
            FILE_SHARE_READ | FILE_SHARE_WRITE | if share_delete { FILE_SHARE_DELETE } else { 0 };
        let file = std::fs::OpenOptions::new()
            .access_mode(GENERIC_READ | GENERIC_WRITE)
            .share_mode(share)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "mutable path is a reparse point or not a directory",
            ));
        }
        Ok(file)
    }

    fn probe_directory(path: &Path) -> io::Result<std::fs::File> {
        probe_directory_or_other(path)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "probed path is a reparse point or not a directory",
            )
        })
    }

    /// Open `path` to compare its identity: `None` when it now reaches a
    /// reparse point or a non-directory, another kind of object.
    fn probe_directory_or_other(path: &Path) -> io::Result<Option<std::fs::File>> {
        let file = std::fs::OpenOptions::new()
            .access_mode(GENERIC_READ)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Ok(None);
        }
        Ok(Some(file))
    }

    /// Whether two handles reach one file: `Ok(false)` is a real
    /// volume/file-index mismatch, and a failed query returns its OS error.
    fn same_identity(left: &std::fs::File, right: &std::fs::File) -> io::Result<bool> {
        identity_probe_fault()?;
        let (left, right) = (file_identity(left)?, file_identity(right)?);
        Ok((left.volume, left.index) == (right.volume, right.index))
    }

    /// Open `<parent>\<name>` with DELETE for one rename or removal, and prove
    /// it is BOTH the directory `expected` retains and, right now, a direct
    /// child of the directory `parent` retains: the Unix `renameat`/`unlinkat`
    /// relative to the parent's descriptor.
    fn open_child_for_mutation(
        parent: &std::fs::File,
        path: &Path,
        expected: &std::fs::File,
    ) -> io::Result<std::fs::File> {
        let file = std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES | DELETE_ACCESS)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .map_err(reopen_failed)?;
        if !same_identity(&file, expected)? {
            return Err(identity_changed());
        }
        ensure_direct_child(parent, &file)?;
        Ok(file)
    }

    /// Open a retained read guard for an artifact.
    fn open_regular_file_with_identity(path: &Path) -> io::Result<(std::fs::File, FileIdentity)> {
        let file = std::fs::OpenOptions::new()
            .access_mode(GENERIC_READ)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let metadata = file.metadata()?;
        let identity = file_identity(&file)?;
        if !metadata.is_file()
            || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || identity.links != 1
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "artifact is not a singly-linked regular file",
            ));
        }
        Ok((file, identity))
    }

    fn open_regular_file(path: &Path) -> io::Result<std::fs::File> {
        open_regular_file_with_identity(path).map(|(file, _)| file)
    }

    fn probe_regular_file(path: &Path) -> io::Result<std::fs::File> {
        open_regular_file(path)
    }

    #[derive(Debug)]
    struct WindowsPinned {
        components: Vec<(PathBuf, Arc<std::fs::File>)>,
    }

    #[derive(Clone, Debug)]
    pub(crate) struct PinnedDir {
        path: PathBuf,
        pinned: Arc<WindowsPinned>,
    }

    #[derive(Debug)]
    pub(crate) struct PinnedFile {
        dir: PinnedDir,
        name: OsString,
        file: std::fs::File,
    }

    /// Copyable directory identity used by registries without retaining an extra handle.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct PinnedDirIdentity {
        volume: u32,
        index: u64,
    }

    /// Recorded kernel identity for checkpoint revalidation after a handle closes.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct PinnedFileIdentity {
        volume: u32,
        index: u64,
    }

    impl PinnedFileIdentity {
        fn from_raw(identity: FileIdentity) -> io::Result<Self> {
            if identity.links != 1 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "artifact source is not a singly-linked regular file",
                ));
            }
            Ok(Self {
                volume: identity.volume,
                index: identity.index,
            })
        }

        /// Reopen one name through an already-validated retained directory and
        /// compare its current volume/file-index.
        pub(crate) fn validate_at_retained(self, dir: &PinnedDir, name: &OsStr) -> io::Result<()> {
            if dir.private_file_identity_at_retained(name)? != self {
                return Err(identity_changed());
            }
            Ok(())
        }
    }

    impl PinnedFile {
        fn identity(&self) -> io::Result<PinnedFileIdentity> {
            PinnedFileIdentity::from_raw(file_identity(&self.file)?)
        }

        /// Record this open file's identity and close it.
        pub(crate) fn into_identity(self) -> io::Result<PinnedFileIdentity> {
            self.identity()
        }

        /// The name still reaches this exact file.
        pub(crate) fn validate_entry_identity_at_retained(&self) -> io::Result<()> {
            let current = probe_regular_file(&self.dir.path.join(&self.name))?;
            if !same_identity(&current, &self.file)? {
                return Err(identity_changed());
            }
            ensure_direct_child(self.dir.leaf(), &self.file)
        }

        pub(crate) fn validate_path_identity(&self) -> io::Result<()> {
            self.dir.validate_path_identity()?;
            self.validate_entry_identity_at_retained()
        }

        pub(crate) fn remove_exact(self) -> io::Result<()> {
            dispose_file_handle(&self.file)
        }

        /// Handle-based exclusive recovery move.
        pub(crate) fn move_to_recovery(self, target: &PinnedDir, name: &OsStr) -> io::Result<()> {
            validate_component(name)?;
            self.validate_path_identity()?;
            target.validate_path_identity()?;
            let outgoing = open_disposable(&self.dir.path.join(&self.name))?;
            if !same_identity(&outgoing, &self.file)? {
                return Err(identity_changed());
            }
            ensure_direct_child(self.dir.leaf(), &outgoing)?;
            rename_file_handle(&outgoing, &target.path, name, false)?;
            self.identity()?.validate_at_retained(target, name)?;
            self.dir.sync()?;
            target.sync()?;
            target.validate_path_identity()
        }

        fn replace_as(mut self, name: &OsStr) -> io::Result<Self> {
            validate_component(name)?;
            if let Err(error) = self.validate_path_identity() {
                let _ = self.remove_exact();
                return Err(error);
            }
            if let Err(error) = rename_file_handle(&self.file, &self.dir.path, name, true) {
                let _ = self.remove_exact();
                return Err(error);
            }
            self.name = name.to_os_string();
            if let Err(error) = self.validate_path_identity() {
                let _ = self.remove_exact();
                return Err(error);
            }
            Ok(self)
        }

        fn publish_as_new_with_hook(
            self,
            name: &OsStr,
            after_publish: impl FnOnce(),
        ) -> io::Result<Self> {
            self.publish_as_new_with_validation(name, after_publish, true)
        }

        /// Publish through the retained parent capability while checking only
        /// the temporary/final entry, which includes proving it a direct child
        /// of the retained directory ([`Self::validate_entry_identity_at_retained`]).
        fn publish_as_new_at_retained_with_hook(
            self,
            name: &OsStr,
            after_publish: impl FnOnce(),
        ) -> io::Result<Self> {
            self.publish_as_new_with_validation(name, after_publish, false)
        }

        fn publish_as_new_with_validation(
            mut self,
            name: &OsStr,
            after_publish: impl FnOnce(),
            validate_path: bool,
        ) -> io::Result<Self> {
            validate_component(name)?;
            let validate = |file: &Self| {
                if validate_path {
                    file.validate_path_identity()
                } else {
                    file.validate_entry_identity_at_retained()
                }
            };
            if let Err(error) = validate(&self) {
                let _ = self.remove_exact();
                return Err(error);
            }
            if let Err(error) = rename_file_handle(&self.file, &self.dir.path, name, false) {
                let _ = self.remove_exact();
                return Err(error);
            }
            self.name = name.to_os_string();
            after_publish();
            if let Err(error) = validate(&self) {
                let _ = self.remove_exact();
                return Err(error);
            }
            Ok(self)
        }

        /// Publish a fully written temporary file at a previously absent final
        /// name as the last call on its directory ([`ParentSync::BeforePublish`]).
        /// The rename resolves its destination by path, so a handle-only proof
        /// that the file landed in the retained directory precedes `after_publish`;
        /// nothing reopens the directory by path once the file is visible.
        fn publish_as_new_last_with_hook(
            mut self,
            name: &OsStr,
            after_publish: impl FnOnce(),
        ) -> io::Result<Self> {
            validate_component(name)?;
            if let Err(error) = self
                .validate_path_identity()
                .and_then(|()| self.dir.sync())
                .and_then(|()| self.validate_path_identity())
            {
                let _ = self.remove_exact();
                return Err(error);
            }
            if let Err(error) = rename_file_handle(&self.file, &self.dir.path, name, false)
                .and_then(|()| ensure_direct_child(self.dir.leaf(), &self.file))
            {
                let _ = self.remove_exact();
                return Err(error);
            }
            self.name = name.to_os_string();
            after_publish();
            // Attempt to persist the entry through the retained handle alone; a
            // failure here must not take back a marker someone has seen.
            let _ = self.dir.sync();
            Ok(self)
        }
    }

    impl PinnedDir {
        /// Resolve a caller path while retaining the exact final directory.
        pub(crate) fn open_resolved(path: &Path) -> io::Result<Self> {
            Self::open_resolved_with_admission(path, |_| true)
        }

        /// Resolve the exact target.
        pub(crate) fn open_resolved_with_admission(
            path: &Path,
            admit: impl FnOnce(&Path) -> bool,
        ) -> io::Result<Self> {
            let expected = open_directory(path)?;
            let canonical = std::fs::canonicalize(path)?;
            let pinned = Self::open_with_admission(&canonical, admit)?;
            if !same_identity(&expected, pinned.leaf())? {
                return Err(identity_changed());
            }
            Ok(pinned)
        }

        #[cfg_attr(
            any(test, debug_assertions),
            aterm_spec_anchor::refines(
                machine = "AnchoredArtifactTransaction",
                action = "ConfinePin",
                project = "aterm_gui::artifact_transaction_conformance::project_anchored"
            )
        )]
        pub(crate) fn open(path: &Path) -> io::Result<Self> {
            validate_open_path(path)?;
            let path = path.to_path_buf();
            let mut prefixes = path.ancestors().map(Path::to_path_buf).collect::<Vec<_>>();
            prefixes.reverse();
            let mut components = Vec::with_capacity(prefixes.len());
            let last = prefixes.len().saturating_sub(1);
            for (index, prefix) in prefixes.into_iter().enumerate() {
                #[cfg(test)]
                super::note_pinned_chain_open();
                let file = if index == last {
                    open_mutable_directory(&prefix, false)?
                } else {
                    open_directory(&prefix)?
                };
                components.push((prefix, Arc::new(file)));
            }
            let result = Self {
                path,
                pinned: Arc::new(WindowsPinned { components }),
            };
            result.validate_path_identity()?;
            Ok(result)
        }

        #[must_use]
        pub(crate) fn path(&self) -> &Path {
            &self.path
        }

        pub(crate) fn retained_identity(&self) -> io::Result<PinnedDirIdentity> {
            let identity = file_identity(self.leaf())?;
            Ok(PinnedDirIdentity {
                volume: identity.volume,
                index: identity.index,
            })
        }

        fn leaf(&self) -> &std::fs::File {
            self.pinned
                .components
                .last()
                .expect("an absolute Windows directory has a root component")
                .1
                .as_ref()
        }

        pub(crate) fn validate_path_identity(&self) -> io::Result<()> {
            for (path, expected) in &self.pinned.components {
                let current = probe_directory(path)?;
                if !same_identity(&current, expected.as_ref())? {
                    return Err(identity_changed());
                }
            }
            Ok(())
        }

        /// The directory's entries, flushed as the unix arm's plain `fsync(2)` flushes them.
        pub(crate) fn sync(&self) -> io::Result<()> {
            flush_directory(self.leaf())
        }

        pub(crate) fn child(&self, name: &OsStr) -> io::Result<Self> {
            validate_component(name)?;
            self.validate_path_identity()?;
            self.open_child_directory(self.path.join(name))
        }

        /// Open the directory at `path` — a name inside this one — and extend
        /// the chain with it, once its handle is proven to sit in the
        /// directory this chain retains.
        fn open_child_directory(&self, path: PathBuf) -> io::Result<Self> {
            let child = open_mutable_directory(&path, true)?;
            ensure_direct_child(self.leaf(), &child)?;
            let mut components = self.pinned.components.clone();
            components.push((path.clone(), Arc::new(child)));
            Ok(Self {
                path,
                pinned: Arc::new(WindowsPinned { components }),
            })
        }

        pub(crate) fn ensure_child(&self, name: &OsStr) -> io::Result<Self> {
            match self.child(name) {
                Ok(child) => Ok(child),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    match self.create_child(name) {
                        Ok(child) => Ok(child),
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                            self.child(name)
                        }
                        Err(error) => Err(error),
                    }
                }
                Err(error) => Err(error),
            }
        }

        fn open_namespace_lock_with_create(
            &self,
            name: &OsStr,
            create: bool,
        ) -> io::Result<std::fs::File> {
            validate_component(name)?;
            self.validate_path_identity()?;
            let path = self.path.join(name);
            let mut options = std::fs::OpenOptions::new();
            options
                .read(true)
                .write(true)
                .create(create)
                .truncate(false)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
            let file = options.open(&path)?;
            let metadata = file.metadata()?;
            if !metadata.is_file()
                || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
                || file_identity(&file)?.links != 1
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "namespace lock is a reparse point or not a singly-linked regular file",
                ));
            }
            let current = probe_regular_file(&path)?;
            if !same_identity(&file, &current)? {
                return Err(identity_changed());
            }
            ensure_direct_child(self.leaf(), &file)?;
            Ok(file)
        }

        pub(crate) fn open_namespace_lock(&self, name: &OsStr) -> io::Result<std::fs::File> {
            self.open_namespace_lock_with_create(name, true)
        }

        pub(crate) fn open_existing_namespace_lock_at_retained(
            &self,
            name: &OsStr,
        ) -> io::Result<std::fs::File> {
            self.open_namespace_lock_with_create(name, false)
        }

        pub(crate) fn create_child(&self, name: &OsStr) -> io::Result<Self> {
            validate_component(name)?;
            self.validate_path_identity()?;
            std::fs::create_dir(self.path.join(name))?;
            let child = self.child(name)?;
            self.sync()?;
            child.validate_path_identity()?;
            Ok(child)
        }

        fn write_private_inner_with_hook<F: FnOnce()>(
            &self,
            name: &OsStr,
            bytes: &[u8],
            after_create: &mut Option<F>,
            validate_path: bool,
            flush: Flush,
        ) -> io::Result<PinnedFile> {
            validate_component(name)?;
            if validate_path {
                self.validate_path_identity()?;
            }
            let path = self.path.join(name);
            let mut options = std::fs::OpenOptions::new();
            options
                .access_mode(GENERIC_WRITE | DELETE_ACCESS)
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
            options.write(true);
            options.create_new(true);
            let file = options.open(path)?;
            let file = PinnedFile {
                dir: self.clone(),
                name: name.to_os_string(),
                file,
            };
            let prepare = (|| {
                ensure_direct_child(self.leaf(), &file.file)?;
                let metadata = file.file.metadata()?;
                if !metadata.is_file()
                    || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
                    || file_identity(&file.file)?.links != 1
                {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "artifact target is a reparse point or not a singly-linked regular file",
                    ));
                }
                Ok(())
            })();
            if let Err(error) = prepare {
                let _ = file.remove_exact();
                return Err(error);
            }
            if let Some(after_create) = after_create.take() {
                after_create();
            }
            let result = (|| {
                (&file.file).write_all(bytes)?;
                crate::durable::finish(&file.file, flush)
            })();
            if let Err(error) = result {
                let _ = file.remove_exact();
                return Err(error);
            }
            Ok(file)
        }

        fn write_private_inner(&self, name: &OsStr, bytes: &[u8]) -> io::Result<PinnedFile> {
            let mut after_create = Some(|| {});
            self.write_private_inner_with_hook(name, bytes, &mut after_create, true, Flush::Ordered)
        }

        fn temporary_name(sequence: u64) -> OsString {
            OsString::from(format!(
                ".aterm-write-p{}-{sequence:020}",
                std::process::id()
            ))
        }

        #[cfg_attr(
            any(test, debug_assertions),
            aterm_spec_anchor::refines(
                machine = "AnchoredArtifactTransaction",
                action = "WritePinned",
                project = "aterm_gui::artifact_transaction_conformance::project_anchored"
            )
        )]
        pub(crate) fn write_private(&self, name: &OsStr, bytes: &[u8]) -> io::Result<PinnedFile> {
            self.write_private_authorized(name, bytes, || true)
        }

        pub(crate) fn write_private_authorized(
            &self,
            name: &OsStr,
            bytes: &[u8],
            authorize: impl FnOnce() -> bool,
        ) -> io::Result<PinnedFile> {
            validate_component(name)?;
            match self.pin_private_file(name) {
                Ok(existing) => drop(existing),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let mut authorize = Some(authorize);
            for _ in 0..32 {
                let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let temporary = Self::temporary_name(sequence);
                match self.write_private_inner(&temporary, bytes) {
                    Ok(file) => {
                        if !authorize
                            .take()
                            .expect("publication authorizer runs exactly once")(
                        ) {
                            let _ = file.remove_exact();
                            return Err(io::Error::new(
                                io::ErrorKind::Interrupted,
                                "artifact publication cancelled",
                            ));
                        }
                        let file = file.replace_as(name)?;
                        if let Err(error) = self.sync().and_then(|()| file.validate_path_identity())
                        {
                            let _ = file.remove_exact();
                            return Err(error);
                        }
                        return Ok(file);
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(error),
                }
            }
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "could not allocate a private replacement name",
            ))
        }

        pub(crate) fn write_new_private(
            &self,
            name: &OsStr,
            bytes: &[u8],
        ) -> io::Result<PinnedFile> {
            self.write_new_private_authorized(name, bytes, || true)
        }

        pub(crate) fn write_new_private_authorized(
            &self,
            name: &OsStr,
            bytes: &[u8],
            authorize: impl FnOnce() -> bool,
        ) -> io::Result<PinnedFile> {
            self.write_new_private_with_authorizer_and_hooks(
                name,
                bytes,
                authorize,
                || {},
                || {},
                Publication {
                    flush: Flush::Ordered,
                    parent: ParentSync::AfterPublish,
                },
            )
        }

        /// Publish a new private completion marker as the last call on this
        /// directory ([`ParentSync::BeforePublish`]): `after_publish` runs the
        /// moment the marker is visible.
        pub(crate) fn write_new_private_marker(
            &self,
            name: &OsStr,
            bytes: &[u8],
            after_publish: impl FnOnce(),
        ) -> io::Result<PinnedFile> {
            self.write_new_private_with_authorizer_and_hooks(
                name,
                bytes,
                || true,
                || {},
                after_publish,
                Publication {
                    flush: Flush::Ordered,
                    parent: ParentSync::BeforePublish,
                },
            )
        }

        /// Publish one file as part of a private batch whose caller supplies the parent-directory.
        pub(crate) fn write_new_private_deferred_dir_sync_authorized(
            &self,
            name: &OsStr,
            bytes: &[u8],
            authorize: impl FnOnce() -> bool,
        ) -> io::Result<PinnedFile> {
            self.write_new_private_with_authorizer_and_hooks(
                name,
                bytes,
                authorize,
                || {},
                || {},
                Publication {
                    flush: Flush::Ordered,
                    parent: ParentSync::Deferred,
                },
            )
        }

        pub(crate) fn write_new_private_with_hooks(
            &self,
            name: &OsStr,
            bytes: &[u8],
            after_temp_create: impl FnOnce(),
            after_publish: impl FnOnce(),
            flush: Flush,
        ) -> io::Result<PinnedFile> {
            self.write_new_private_with_authorizer_and_hooks(
                name,
                bytes,
                || true,
                after_temp_create,
                after_publish,
                Publication {
                    flush,
                    parent: ParentSync::AfterPublish,
                },
            )
        }

        fn write_new_private_with_authorizer_and_hooks(
            &self,
            name: &OsStr,
            bytes: &[u8],
            authorize: impl FnOnce() -> bool,
            after_temp_create: impl FnOnce(),
            after_publish: impl FnOnce(),
            publication: Publication,
        ) -> io::Result<PinnedFile> {
            let Publication { flush, parent } = publication;
            validate_component(name)?;
            static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let mut authorize = Some(authorize);
            let mut after_temp_create = Some(after_temp_create);
            let mut after_publish = Some(after_publish);
            for _ in 0..32 {
                let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let temporary = Self::temporary_name(sequence);
                match self.write_private_inner_with_hook(
                    &temporary,
                    bytes,
                    &mut after_temp_create,
                    parent != ParentSync::Deferred,
                    flush,
                ) {
                    Ok(file) => {
                        if !authorize
                            .take()
                            .expect("publication authorizer runs exactly once")(
                        ) {
                            let _ = file.remove_exact();
                            return Err(io::Error::new(
                                io::ErrorKind::Interrupted,
                                "artifact publication cancelled",
                            ));
                        }
                        let after_publish = after_publish
                            .take()
                            .expect("publication hook runs exactly once");
                        return match parent {
                            ParentSync::AfterPublish => {
                                let file = file.publish_as_new_with_hook(name, after_publish)?;
                                if let Err(error) =
                                    self.sync().and_then(|()| file.validate_path_identity())
                                {
                                    let _ = file.remove_exact();
                                    return Err(error);
                                }
                                Ok(file)
                            }
                            ParentSync::Deferred => {
                                file.publish_as_new_at_retained_with_hook(name, after_publish)
                            }
                            ParentSync::BeforePublish => {
                                file.publish_as_new_last_with_hook(name, after_publish)
                            }
                        };
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
            }
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "could not allocate a private publication name",
            ))
        }

        #[cfg_attr(
            any(test, debug_assertions),
            aterm_spec_anchor::refines(
                machine = "AnchoredArtifactTransaction",
                action = "ReadPinned",
                project = "aterm_gui::artifact_transaction_conformance::project_anchored"
            )
        )]
        pub(crate) fn read_private(
            &self,
            name: &OsStr,
            limit: usize,
        ) -> io::Result<(Vec<u8>, PinnedFile)> {
            validate_component(name)?;
            self.validate_path_identity()?;
            let mut file = open_regular_file(&self.path.join(name))?;
            let cap = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
            let mut bytes = Vec::with_capacity(limit.min(4096));
            (&mut file).take(cap).read_to_end(&mut bytes)?;
            if bytes.len() > limit {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "artifact source exceeded its size bound",
                ));
            }
            let guard = PinnedFile {
                dir: self.clone(),
                name: name.to_os_string(),
                file,
            };
            guard.validate_path_identity()?;
            Ok((bytes, guard))
        }

        /// The ordinary path-validated read.
        pub(crate) fn read_private_at_retained(
            &self,
            name: &OsStr,
            limit: usize,
        ) -> io::Result<(Vec<u8>, PinnedFile)> {
            self.read_private(name, limit)
        }

        pub(crate) fn pin_private_file(&self, name: &OsStr) -> io::Result<PinnedFile> {
            self.validate_path_identity()?;
            let guard = self.pin_private_file_at_retained(name)?;
            guard.validate_path_identity()?;
            Ok(guard)
        }

        pub(crate) fn pin_private_file_at_retained(&self, name: &OsStr) -> io::Result<PinnedFile> {
            validate_component(name)?;
            let file = open_regular_file(&self.path.join(name))?;
            let guard = PinnedFile {
                dir: self.clone(),
                name: name.to_os_string(),
                file,
            };
            guard.validate_entry_identity_at_retained()?;
            Ok(guard)
        }

        /// Open one direct child through this retained directory, record its identity.
        pub(crate) fn private_file_identity_at_retained(
            &self,
            name: &OsStr,
        ) -> io::Result<PinnedFileIdentity> {
            validate_component(name)?;
            let (file, identity) = open_regular_file_with_identity(&self.path.join(name))?;
            ensure_direct_child(self.leaf(), &file)?;
            PinnedFileIdentity::from_raw(identity)
        }

        pub(crate) fn remove_file_if_exists(&self, name: &OsStr) -> io::Result<()> {
            validate_component(name)?;
            self.validate_path_identity()?;
            match open_disposable(&self.path.join(name)) {
                Ok(file) => {
                    ensure_direct_child(self.leaf(), &file)?;
                    let metadata = file.metadata()?;
                    if metadata.is_dir()
                        && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::IsADirectory,
                            "artifact name is a directory, not a file",
                        ));
                    }
                    dispose_file_handle(&file)
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            }
        }

        pub(crate) fn is_regular_file(&self, name: &OsStr) -> bool {
            if validate_component(name).is_err() || self.validate_path_identity().is_err() {
                return false;
            }
            probe_regular_file(&self.path.join(name))
                .is_ok_and(|file| ensure_direct_child(self.leaf(), &file).is_ok())
        }

        pub(crate) fn names_up_to(&self, limit: usize) -> io::Result<Vec<OsString>> {
            self.validate_path_identity()?;
            names_in(&self.path, limit)
        }

        fn remove_contents(
            &self,
            budget: &mut usize,
            depth: usize,
            removed: &mut usize,
        ) -> io::Result<()> {
            if depth > super::REMOVE_MAX_DEPTH {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "artifact cleanup exceeded its directory-depth bound",
                ));
            }
            let here = final_path(self.leaf())?;
            let mut first_error = None;
            for name in names_in(&here, (*budget).saturating_add(1))? {
                if *budget == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "artifact cleanup exceeded its entry bound",
                    ));
                }
                *budget -= 1;
                let path = here.join(&name);
                let result = match std::fs::symlink_metadata(&path) {
                    Ok(metadata)
                        if metadata.is_dir()
                            && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 =>
                    {
                        self.open_child_directory(path.clone()).and_then(|child| {
                            child.remove_contents(budget, depth + 1, removed)?;
                            dispose_file_handle(&open_child_for_mutation(
                                self.leaf(),
                                &path,
                                child.leaf(),
                            )?)?;
                            *removed = removed.saturating_add(1);
                            Ok(())
                        })
                    }
                    Ok(_) => open_disposable(&path).and_then(|file| {
                        ensure_direct_child(self.leaf(), &file)?;
                        dispose_file_handle(&file)?;
                        *removed = removed.saturating_add(1);
                        Ok(())
                    }),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(error),
                };
                if let Err(error) = result
                    && first_error.is_none()
                {
                    first_error = Some(error);
                }
            }
            first_error.map_or(Ok(()), Err)
        }

        pub(crate) fn clear_contents_with_budget(
            &self,
            budget: &mut usize,
        ) -> super::ClearContentsOutcome {
            let mut removed = 0;
            let result = self.remove_contents(budget, 0, &mut removed);
            super::ClearContentsOutcome { result, removed }
        }

        #[cfg(test)]
        pub(crate) fn remove_child_tree(&self, name: &OsStr) -> io::Result<()> {
            validate_component(name)?;
            let child = match self.child(name) {
                Ok(child) => child,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error),
            };
            self.remove_child_tree_exact(name, &child)
        }

        pub(crate) fn remove_child_tree_exact(&self, name: &OsStr, child: &Self) -> io::Result<()> {
            validate_component(name)?;
            let mut budget = super::REMOVE_MAX_ENTRIES;
            let mut removed = 0;
            child.remove_contents(&mut budget, 0, &mut removed)?;
            self.remove_empty_child_exact(name, child)
        }

        /// Remove `name` only while it is the exact directory `child` retains
        /// and a direct child of this one.
        pub(crate) fn remove_empty_child_exact(
            &self,
            name: &OsStr,
            child: &Self,
        ) -> io::Result<()> {
            validate_component(name)?;
            dispose_file_handle(&open_child_for_mutation(
                self.leaf(),
                &self.path.join(name),
                child.leaf(),
            )?)
        }

        /// Whether `name` here still reaches the directory `expected` retains:
        /// `Ok(false)` is a real volume/file-index mismatch (a reparse point or
        /// a non-directory in its place included), a name that reaches
        /// nothing is the child having vanished ([`reopen_failed`]), and any
        /// other failure returns its OS error.
        pub(crate) fn current_child_matches(
            &self,
            name: &OsStr,
            expected: &Self,
        ) -> io::Result<bool> {
            validate_component(name)?;
            let Some(current) =
                probe_directory_or_other(&self.path.join(name)).map_err(reopen_failed)?
            else {
                return Ok(false);
            };
            same_identity(&current, expected.leaf())
        }

        pub(crate) fn rename_child_exact(
            &self,
            from: &OsStr,
            expected: &Self,
            to: &OsStr,
        ) -> io::Result<()> {
            validate_component(from)?;
            validate_component(to)?;
            let handle =
                open_child_for_mutation(self.leaf(), &self.path.join(from), expected.leaf())?;
            rename_file_handle(&handle, &self.path, to, false)?;
            let landed = ensure_direct_child(self.leaf(), &handle);
            drop(handle);
            landed?;
            if !self.current_child_matches(to, expected)? {
                return Err(identity_changed());
            }
            Ok(())
        }
    }

    fn names_in(directory: &Path, limit: usize) -> io::Result<Vec<OsString>> {
        let mut names = Vec::with_capacity(limit.min(256));
        for entry in std::fs::read_dir(directory)? {
            if names.len() == limit {
                break;
            }
            names.push(entry?.file_name());
        }
        Ok(names)
    }

    fn open_disposable(path: &Path) -> io::Result<std::fs::File> {
        std::fs::OpenOptions::new()
            .access_mode(GENERIC_READ | DELETE_ACCESS)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }

    /// `SetFileInformationByHandle`'s answer: `Ok` for TRUE, else this thread's
    /// last OS error, which the call set.
    fn set_information(succeeded: aterm_win32::core::BOOL) -> io::Result<()> {
        if succeeded == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn dispose_file_handle(file: &std::fs::File) -> io::Result<()> {
        use aterm_win32::Win32::Storage::FileSystem::{
            FILE_DISPOSITION_INFO, FileDispositionInfo, SetFileInformationByHandle,
        };

        let information = FILE_DISPOSITION_INFO { DeleteFile: true };
        // SAFETY: `file` owns a live handle opened with DELETE access; the
        // information pointer and exact structure size match FileDispositionInfo.
        set_information(unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileDispositionInfo,
                (&raw const information).cast(),
                u32::try_from(std::mem::size_of_val(&information))
                    .expect("FILE_DISPOSITION_INFO size fits u32"),
            )
        })
    }

    /// The `FILE_RENAME_INFO` request bytes for one destination.
    pub(super) struct RenamePayload {
        buffer: Vec<usize>,
        bytes: usize,
    }

    impl RenamePayload {
        fn as_mut_ptr(&mut self) -> *mut u8 {
            self.buffer.as_mut_ptr().cast()
        }

        /// The request bytes exactly as the kernel will read them.
        #[cfg(test)]
        #[must_use]
        pub(super) fn request_bytes(&self) -> &[u8] {
            // SAFETY: `buffer` owns at least `bytes` initialized bytes and
            // `usize` may be read as its constituent bytes.
            unsafe { std::slice::from_raw_parts(self.buffer.as_ptr().cast::<u8>(), self.bytes) }
        }

        #[cfg(test)]
        #[must_use]
        pub(super) fn file_name_length(&self) -> u32 {
            use aterm_win32::Win32::Storage::FileSystem::FILE_RENAME_INFO;
            let header = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
            let mut raw = [0_u8; 4];
            raw.copy_from_slice(&self.request_bytes()[header - 4..header]);
            u32::from_ne_bytes(raw)
        }

        /// The UTF-16 units the kernel finds at `FileName`, terminator included.
        #[cfg(test)]
        #[must_use]
        pub(super) fn file_name_units(&self) -> Vec<u16> {
            use aterm_win32::Win32::Storage::FileSystem::FILE_RENAME_INFO;
            let header = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
            self.request_bytes()[header..]
                .chunks_exact(2)
                .map(|pair| u16::from_ne_bytes([pair[0], pair[1]]))
                .collect()
        }
    }

    /// Build the rename request for `destination`.
    pub(super) fn rename_payload(destination: &Path, replace: bool) -> io::Result<RenamePayload> {
        use aterm_win32::Win32::Storage::FileSystem::FILE_RENAME_INFO;
        use std::os::windows::ffi::OsStrExt as _;

        let name_bytes = destination
            .as_os_str()
            .encode_wide()
            .count()
            .checked_mul(2)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "rename name is too long")
            })?;
        let wide = destination
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let header = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
        let bytes = header
            .checked_add(wide.len().checked_mul(2).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "rename name is too long")
            })?)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "rename size overflow"))?;
        let words = bytes
            .checked_add(std::mem::size_of::<usize>() - 1)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "rename size overflow"))?
            / std::mem::size_of::<usize>();
        let mut buffer = vec![0_usize; words];
        let information = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        // SAFETY: `Vec<usize>` provides at least pointer alignment and enough bytes for the
        // header plus the UTF-16 payload including its NUL terminator, so both a length-driven
        // and a terminator-driven reader stay inside this allocation.
        unsafe {
            std::ptr::write(information, FILE_RENAME_INFO::default());
            (*information).Anonymous.ReplaceIfExists = replace;
            (*information).FileNameLength = u32::try_from(name_bytes)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "rename name too long"))?;
            std::ptr::copy_nonoverlapping(
                wide.as_ptr(),
                std::ptr::addr_of_mut!((*information).FileName).cast(),
                wide.len(),
            );
        }
        Ok(RenamePayload { buffer, bytes })
    }

    /// Rename the file behind `file`'s handle to `name` inside `parent_path`.
    fn rename_file_handle(
        file: &std::fs::File,
        parent_path: &Path,
        name: &OsStr,
        replace: bool,
    ) -> io::Result<()> {
        use aterm_win32::Win32::Storage::FileSystem::{FileRenameInfo, SetFileInformationByHandle};

        validate_component(name)?;
        let mut payload = rename_payload(&parent_path.join(name), replace)?;
        let bytes = u32::try_from(payload.bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "rename too large"))?;
        // SAFETY: `payload` owns `bytes` initialized, `usize`-aligned bytes
        // holding one FILE_RENAME_INFO plus its NUL-terminated destination.
        set_information(unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileRenameInfo,
                payload.as_mut_ptr().cast(),
                bytes,
            )
        })
    }
}

pub(crate) use imp::{PinnedDir, PinnedDirIdentity, PinnedFile, PinnedFileIdentity};

impl PinnedDir {
    /// Admit one already-resolved path, then [`Self::open`] it.
    pub(crate) fn open_with_admission(
        path: &Path,
        admit: impl FnOnce(&Path) -> bool,
    ) -> io::Result<Self> {
        validate_open_path(path)?;
        if !admit(path) {
            return Err(admission_refused());
        }
        Self::open(path)
    }
}

/// Contracts both backends owe, asserted with one body so they cannot drift.
#[cfg(test)]
#[path = "pinned_dir_shared_backend_contract_tests.rs"]
mod shared_backend_contract_tests;

#[cfg(all(test, windows))]
#[path = "pinned_dir_windows_tests.rs"]
pub(crate) mod windows_tests;

#[cfg(all(test, unix))]
#[path = "pinned_dir_tests.rs"]
mod tests;
