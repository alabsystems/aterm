// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Directory capabilities retained across asynchronous artifact work.
//!
//! A canonical `PathBuf` is only a statement about one instant. Another
//! same-uid process can rename an ancestor and put a symlink or replacement
//! directory at the old pathname before an encode worker writes. `PinnedDir`
//! instead opens every absolute-path component without following links and
//! retains the resulting handles. Mutations are relative to the retained leaf
//! on Unix. Windows retains a handle on every component, validates their file
//! identities around path-based operations, proves every entry it opens by
//! pathname a direct child of the retained leaf (its stand-in for `openat`),
//! and anchors every removal and rename to a handle whose identity it has
//! proven. NTFS refuses to rename any directory that has an open handle
//! anywhere beneath it (`windows_child_pins_share_delete_but_pin_every_ancestor`),
//! so a retained leaf pins its whole ancestor chain; the leaf's OWN share mode
//! decides whether another opener may rename the leaf itself. A chain's root
//! denies that; a directory reached through [`PinnedDir::child`] shares it, as
//! a Unix descriptor does, so any number of pins of one directory coexist.
//!
//! POSIX has no unlink-by-descriptor, so removal (recursive cleanup,
//! `PinnedFile::remove_exact`) checks identity, `unlinkat`s by name, then checks the retained
//! inode's link count. A same-uid swap inside that window can remove the replacement entry, but
//! only inside the pinned, server-owned `0700` directory; nothing is followed, and the
//! post-check returns an error rather than a false `Ok`.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

use crate::durable::Flush;

/// How a new private file is published: the flush that finishes its bytes, and
/// whether its parent's entries are synced at once or left to the caller's batch
/// barrier (a video recording's [`PinnedDir::sync`] before its marker).
#[derive(Clone, Copy)]
struct Publication {
    flush: Flush,
    sync_parent: bool,
}

/// Whether `name` is one ordinary path component that every backend may hand a handle-relative
/// call: the rule [`validate_component`] enforces, for callers that judge a name before it
/// reaches a [`PinnedDir`].
pub(crate) fn is_one_component(name: &OsStr) -> bool {
    validate_component(name).is_ok()
}

/// Refuse any name that is not one ordinary path component: the empty name, `.`, `..`, any
/// separator (`/`, and `\` on Windows) and NUL.
///
/// Decided on the raw name, never on `Path::components()`, which normalizes a trailing `/`, `/.`
/// and `//` away: `"link/"` would pass as `link`, and the `*at` call would resolve the symlink
/// (`O_NOFOLLOW`, `unlinkat` and `renameat` govern only the final component).
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

fn identity_changed() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "confined artifact directory identity changed",
    )
}

const REMOVE_MAX_ENTRIES: usize = 16_384;
/// Descriptor allowance left beside an initial pinned chain by artifact
/// admission. Keep this shared with `control` so cleanup cannot silently grow
/// beyond the capacity it is charged for.
pub(crate) const PINNED_DIR_OPERATION_DESCRIPTOR_UNITS: usize = 8;
/// A wire-published video prune can already own the fresh recording, index,
/// marker, and candidate recording (four handles). Recursive cleanup opens one
/// additional child before checking its depth, so a limit of three consumes the
/// remaining four units exactly. Shipping artifact layouts are flat; deeper
/// owner-namespace trees are treated as interference and fail closed.
const REMOVE_LIVE_FIXED_UNITS: usize = 4;
const REMOVE_MAX_DEPTH: usize = PINNED_DIR_OPERATION_DESCRIPTOR_UNITS - REMOVE_LIVE_FIXED_UNITS - 1;

pub(crate) struct ClearContentsOutcome {
    pub(crate) result: io::Result<()>,
    pub(crate) removed: usize,
}

/// Maximum absolute-path depth an initial pin may retain. Artifact admission
/// reserves additional units for children, files, validation, and bounded
/// cleanup handles; together they fit the artifact handoff descriptor budget.
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
        OsStr, OsString, Path, PathBuf, Publication, identity_changed, io, validate_component,
        validate_open_path,
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
        openat(parent, name, directory_flags(), Mode::empty()).map_err(Into::into)
    }

    fn is_directory(fd: &impl aterm_dirfd::AsFd) -> bool {
        fstat(fd)
            .ok()
            .is_some_and(|stat| FileType::from_raw_mode(stat.st_mode).is_dir())
    }

    fn is_regular_file(fd: &impl aterm_dirfd::AsFd) -> bool {
        fstat(fd)
            .ok()
            .is_some_and(|stat| FileType::from_raw_mode(stat.st_mode).is_file())
    }

    fn same_identity(left: &impl aterm_dirfd::AsFd, right: &impl aterm_dirfd::AsFd) -> bool {
        let (Ok(left), Ok(right)) = (fstat(left), fstat(right)) else {
            return false;
        };
        left.st_dev == right.st_dev && left.st_ino == right.st_ino
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
        // `linkat` is the first observable final-name boundary on the portable
        // fallback. Fire ownership before the fallible temporary-name unlink:
        // a reader can enter now, and an unlink failure leaves the final hard
        // link visible even though publication returns an error.
        after_publish();
        unlinkat(directory, from, AtFlags::empty()).map_err(io::Error::from)
    }

    #[cfg(not(target_vendor = "apple"))]
    fn directory_was_unlinked(directory: &impl aterm_dirfd::AsFd) -> bool {
        fstat(directory).ok().is_some_and(|stat| stat.st_nlink == 0)
    }

    #[cfg(target_vendor = "apple")]
    fn directory_was_unlinked(directory: &impl aterm_dirfd::AsFd) -> bool {
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
            return false;
        }
        // SAFETY: successful F_GETPATH writes a NUL-terminated path.
        let path = unsafe { std::ffi::CStr::from_ptr(path.as_ptr()) };
        let path = OsStr::from_bytes(path.to_bytes());
        open_directory_at(CWD, path)
            .ok()
            .is_none_or(|current| !same_identity(&current, directory))
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

    /// Copyable directory identity used by registries without retaining an
    /// extra descriptor beyond the admitted owner that supplied it.
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
        /// compare its current device/inode. The temporary descriptor closes on
        /// return.
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

        /// Record this open file's identity and close it. A later checkpoint can
        /// detect an ordinary replacement without retaining one descriptor per
        /// entry; it is not continuous exclusion and kernel IDs may be reused
        /// after the file is deleted.
        pub(crate) fn into_identity(self) -> io::Result<PinnedFileIdentity> {
            self.identity()
        }

        fn current_name_matches(&self) -> bool {
            openat(
                self.dir.leaf(),
                &self.name,
                file_read_flags(),
                Mode::empty(),
            )
            .ok()
            .is_some_and(|current| same_identity(&current, &self.file))
        }

        /// Revalidate this entry against its already-retained parent directory.
        /// This intentionally says nothing about whether the parent's original
        /// lexical ancestor chain still names that directory.
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
        /// retained inode lost its final link. POSIX cannot make the check and
        /// `unlinkat` indivisible: a swapped replacement direct entry inside the
        /// pinned server-owned directory can be removed, but never an outside
        /// target, and that race is reported as an error rather than false success.
        pub(crate) fn remove_exact(self) -> io::Result<()> {
            if !self.current_name_matches() {
                return Err(identity_changed());
            }
            unlinkat(self.dir.leaf(), &self.name, AtFlags::empty()).map_err(io::Error::from)?;
            let stat = fstat(&self.file).map_err(io::Error::from)?;
            if stat.st_nlink != 0 {
                return Err(identity_changed());
            }
            Ok(())
        }

        /// Move into an exclusively owned recovery directory without replacing
        /// any destination or unlinking a subsequently replaced source path.
        /// An identity change around rename is reported; moved bytes remain in
        /// the recovery directory even when its final validation/sync fails.
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
        /// absent final name. Darwin `RENAME_EXCL` and Linux
        /// `RENAME_NOREPLACE` never expose either partial bytes or a transient
        /// second hard link at the final component.
        fn publish_as_new_with_hook(
            self,
            name: &OsStr,
            after_publish: impl FnOnce(),
        ) -> io::Result<Self> {
            self.publish_as_new_with_validation(name, after_publish, true)
        }

        /// Publish through the retained parent capability while checking only
        /// the temporary/final entry. The batch caller validates the lexical
        /// ancestor chain around its one directory durability barrier.
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
    }

    impl PinnedDir {
        /// Resolve a caller path once while holding the exact resulting
        /// directory, then no-follow pin its canonical ancestor chain and
        /// require both opens to identify the same inode.
        pub(crate) fn open_resolved(path: &Path) -> io::Result<Self> {
            Self::open_resolved_with_admission(path, |_| true)
        }

        /// Resolve the exact target, then let descriptor admission run against
        /// that canonical depth before any retained ancestor chain is opened.
        pub(crate) fn open_resolved_with_admission(
            path: &Path,
            admit: impl FnOnce(&Path) -> bool,
        ) -> io::Result<Self> {
            let expected = open_directory_at(CWD, path)?;
            if !is_directory(&expected) {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "resolved path is not a directory",
                ));
            }
            let canonical = std::fs::canonicalize(path)?;
            let pinned = Self::open_with_admission(&canonical, admit)?;
            if !same_identity(&expected, pinned.leaf()) {
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
            if !is_directory(&root) {
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
                if !is_directory(&directory) {
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

        /// Reopen the exact retained directory inode for a cross-process advisory lock: unlike
        /// a replaceable child lockfile, every process writing through this pin locks the same
        /// inode even if a same-uid actor renames entries.
        ///
        /// A new open-file description, not a `dup`: `flock` locks belong to the description,
        /// so a dup would stay locked until the [`PinnedDir`] descriptor also closed.
        pub(crate) fn open_directory_lock(&self) -> io::Result<std::fs::File> {
            Ok(std::fs::File::from(open_directory_at(self.leaf(), ".")?))
        }

        /// Open an existing direct-child advisory lease through this retained
        /// directory authority. No lexical ancestor is re-resolved between the
        /// lock decision and a later handle-rooted mutation.
        pub(crate) fn open_existing_namespace_lock_at_retained(
            &self,
            name: &OsStr,
        ) -> io::Result<std::fs::File> {
            validate_component(name)?;
            let fd = openat(
                self.leaf(),
                name,
                OFlags::RDWR | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(io::Error::from)?;
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
            if !same_identity(&current, self.pinned.root.as_ref()) {
                return Err(identity_changed());
            }
            for (name, expected) in &self.pinned.components {
                let next = open_directory_at(&current, name.as_os_str())?;
                if !is_directory(&next) || !same_identity(&next, expected.as_ref()) {
                    return Err(identity_changed());
                }
                current = next;
            }
            Ok(())
        }

        pub(crate) fn sync(&self) -> io::Result<()> {
            aterm_dirfd::fsync(self.leaf()).map_err(Into::into)
        }

        /// Flush the directory's entries ORDERED ahead of every later write on the
        /// device ([`aterm_durable::ordered`] on a duplicate of the retained
        /// descriptor), for a caller whose entries only a later step on this device
        /// depends on (the session log's segments, instance lock and retention).
        /// [`Self::sync`] stays the plain `fsync(2)` its other callers take (a video
        /// recording's batch barrier, a new artifact's entry).
        pub(crate) fn sync_ordered(&self) -> io::Result<()> {
            aterm_durable::ordered(&std::fs::File::from(self.leaf().try_clone()?))
        }

        pub(crate) fn child(&self, name: &OsStr) -> io::Result<Self> {
            validate_component(name)?;
            let child = open_directory_at(self.leaf(), name)?;
            if !is_directory(&child) {
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
                // The caller's tier: ordered for an artifact, whose bytes land before its
                // marker (`.done`, `.published`) or its rename (the requester is local
                // and reads the page cache, no recovery reads these files, and a torn PNG
                // fails its CRC). A caller that names `Flush::Durable` gets the bytes
                // fully flushed, but the entry that names them still takes
                // [`PinnedDir::sync`]'s plain `fsync(2)`, which on Apple promises
                // neither order nor durability: no such caller is left.
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
            // Ordered: every caller of the replacing writers is an artifact (a snapshot's
            // png and txt, a capture's PNG reply).
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
                    sync_parent: true,
                },
            )
        }

        /// Publish one file as part of a private batch whose caller supplies the
        /// parent-directory sync barrier before any visibility marker. The file
        /// itself is still flushed (ordered: a video frame or index lands before the
        /// marker that publishes it) and identity-checked.
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
                    sync_parent: false,
                },
            )
        }

        /// Create an invisible temporary file finished with `flush`, then
        /// atomically publish it without replacing any existing final component.
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
                    sync_parent: true,
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
            let Publication { flush, sync_parent } = publication;
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
                        let file = if sync_parent {
                            file.publish_as_new_with_hook(name, after_publish)?
                        } else {
                            file.publish_as_new_at_retained_with_hook(name, after_publish)?
                        };
                        if sync_parent
                            && let Err(error) =
                                self.sync().and_then(|()| file.validate_path_identity())
                        {
                            let _ = file.remove_exact();
                            return Err(error);
                        }
                        return Ok(file);
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

        /// Read a child through the retained directory capability and revalidate
        /// the child entry against that same capability after the bounded read.
        /// An ancestor rename is allowed: a caller that needs the lexical path to
        /// still name this directory follows with
        /// [`PinnedFile::validate_path_identity`].
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

        /// Open one direct child through this retained directory, record its
        /// identity, and close it. This intentionally performs one entry open
        /// and no lexical ancestor walk.
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
                    .is_some_and(|fd| is_regular_file(&fd))
        }

        /// Hard-bounded enumeration: overrunning `limit` is an error, not a truncation.
        /// Test-only: shipping callers use [`Self::names_up_to`], so a directory that outgrew
        /// the bound is not a permanent failure.
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

        /// Visit at most `limit` entries' names in directory order, holding
        /// none of them (bounded memory however large the directory): `true`
        /// when an entry was left unvisited.
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
        /// into a permanent retention failure. Callers repeat bounded sweeps.
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
                            if !self.current_child_matches(&name, &child) {
                                return Err(identity_changed());
                            }
                            unlinkat(self.leaf(), &name, AtFlags::REMOVEDIR)
                                .map_err(io::Error::from)?;
                            *removed = removed.saturating_add(1);
                            if !directory_was_unlinked(child.leaf()) {
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

        /// Test-only: remove a child tree by name through the retained parent, so a test can
        /// swap the parent's pathname and see the removal land on the retained inode. Production
        /// cleanup holds the child's handle and uses [`Self::remove_child_tree_exact`].
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
            if !self.current_child_matches(name, &child) {
                return Err(identity_changed());
            }
            unlinkat(self.leaf(), name, AtFlags::REMOVEDIR).map_err(io::Error::from)?;
            if !directory_was_unlinked(child.leaf()) {
                return Err(identity_changed());
            }
            Ok(())
        }

        pub(crate) fn remove_child_tree_exact(&self, name: &OsStr, child: &Self) -> io::Result<()> {
            validate_component(name)?;
            let mut budget = super::REMOVE_MAX_ENTRIES;
            let mut removed = 0;
            child.remove_contents(&mut budget, 0, &mut removed)?;
            if !self.current_child_matches(name, child) {
                return Err(identity_changed());
            }
            unlinkat(self.leaf(), name, AtFlags::REMOVEDIR).map_err(io::Error::from)?;
            if !directory_was_unlinked(child.leaf()) {
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
            if !self.current_child_matches(name, child) {
                return Err(identity_changed());
            }
            after_check();
            unlinkat(self.leaf(), name, AtFlags::REMOVEDIR).map_err(io::Error::from)?;
            if !directory_was_unlinked(child.leaf()) {
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

        pub(crate) fn current_child_matches(&self, name: &OsStr, expected: &Self) -> bool {
            validate_component(name).is_ok()
                && open_directory_at(self.leaf(), name)
                    .ok()
                    .is_some_and(|current| same_identity(&current, expected.leaf()))
        }

        pub(crate) fn rename_child_exact(
            &self,
            from: &OsStr,
            expected: &Self,
            to: &OsStr,
        ) -> io::Result<()> {
            validate_component(from)?;
            validate_component(to)?;
            if !self.current_child_matches(from, expected) {
                return Err(identity_changed());
            }
            self.rename(from, to)?;
            if !self.current_child_matches(to, expected) {
                return Err(identity_changed());
            }
            Ok(())
        }

        /// A NEW private append-only file (the session journal's segment,
        /// `session_log_journal_fs`): `O_WRONLY|O_APPEND|O_CREAT|O_EXCL|
        /// O_NOFOLLOW|O_CLOEXEC` at mode 0600 through the retained directory,
        /// then checked a singly linked regular file and set to 0600 whatever
        /// the umask. An existing name (a symlink, a FIFO, a file) is refused
        /// by the exclusive create; a new file that fails the check is removed
        /// exactly.
        pub(crate) fn create_new_append_private(&self, name: &OsStr) -> io::Result<std::fs::File> {
            self.create_new_private_with(name, OFlags::WRONLY | OFlags::APPEND)
        }

        /// A NEW private read-write file (the session journal's pending
        /// instance lock): as [`Self::create_new_append_private`], opened
        /// `O_RDWR`.
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

        /// An existing entry, or a new one, opened read-write with no follow
        /// (the session journal's retention lock): `O_RDWR|O_CREAT|
        /// O_NOFOLLOW|O_NONBLOCK|O_CLOEXEC`, mode 0600 when created; a
        /// symlink is refused, and anything but a regular file too.
        pub(crate) fn open_or_create_private_rw(&self, name: &OsStr) -> io::Result<std::fs::File> {
            validate_component(name)?;
            let flags = OFlags::RDWR
                | OFlags::CREATE
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::CLOEXEC;
            let fd = openat(self.leaf(), name, flags, Mode::RUSR | Mode::WUSR)
                .map_err(io::Error::from)?;
            let file = std::fs::File::from(fd);
            if !is_regular_file(&file) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "a private file is not a regular file",
                ));
            }
            Ok(file)
        }

        /// An existing entry opened read-only with no follow and no wait (a
        /// symlink refused, a FIFO opened without blocking and then refused,
        /// as anything but a regular file is): what the session journal
        /// reads, stats and probes.
        pub(crate) fn open_regular_at_retained(&self, name: &OsStr) -> io::Result<std::fs::File> {
            validate_component(name)?;
            let fd = openat(self.leaf(), name, file_read_flags(), Mode::empty())
                .map_err(io::Error::from)?;
            let file = std::fs::File::from(fd);
            if !is_regular_file(&file) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "not a regular file",
                ));
            }
            Ok(file)
        }

        /// Rename `from` to `to` within this directory, never over an
        /// existing `to` (Darwin `RENAME_EXCL`, Linux `RENAME_NOREPLACE`).
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
        OsStr, OsString, Path, PathBuf, Publication, identity_changed, io, validate_component,
        validate_open_path,
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
    /// `FILE_NAME_NORMALIZED | VOLUME_NAME_DOS` (flags 0): the same spelling
    /// for the same directory on every call, which is what the comparison in
    /// [`ensure_direct_child`] relies on.
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
            // On success the count excludes the terminator; when the buffer is
            // too small it is the size REQUIRED, terminator included.
            if written < buffer.len() {
                buffer.truncate(written);
                return Ok(PathBuf::from(OsString::from_wide(&buffer)));
            }
            buffer.resize(written, 0);
        }
    }

    /// Prove that `entry` — just opened by the path `<parent>\<name>` — is a
    /// direct child of the directory `parent` retains. This is the Windows
    /// stand-in for Unix `openat`: a pathname open lands somewhere else if the
    /// parent's name was swapped (renamed away and a junction put in its
    /// place, or the emptied directory turned into a junction in place)
    /// between the last validation and the open. Once `entry` is open, NTFS
    /// refuses to rename any directory above it, so the directory it sits in
    /// cannot move, and two different directories never hold one path at the
    /// same instant: an equal parent path IS the retained directory. For an
    /// entry whose handle denies delete (a created file, a lock, a removal
    /// handle) that holds for the handle's whole life; one that shares delete
    /// (a read guard, a `child` directory) can itself be renamed later, which
    /// its callers' revalidation turns into a refusal. A junction swapped in
    /// and back out again cannot fool this the way a path-identity re-probe
    /// can, and a hard link reports the link actually opened.
    fn ensure_direct_child(parent: &std::fs::File, entry: &std::fs::File) -> io::Result<()> {
        let entry_path = final_path(entry)?;
        if entry_path.parent() != Some(final_path(parent)?.as_path()) {
            return Err(identity_changed());
        }
        Ok(())
    }

    /// Flush a retained directory's entries: `FlushFileBuffers`, Windows' only
    /// flush, on the handle itself. It is the counterpart of the unix arm's plain
    /// `fsync(2)` and, like it, goes around aterm-durable's two tiers and records
    /// nothing in its counting seam: the seam records the tier a SITE asks for
    /// (Linux's `ordered` is the full `fsync(2)` and still records `Ordered`), and
    /// [`PinnedDir::sync`]'s callers — a recording's batch barrier, a new
    /// artifact's entry — ask for neither tier on either backend. What a power
    /// loss leaves is the stronger answer here: this flush is a full one.
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
        // SAFETY: a nonzero return initializes the full output structure.
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

    /// [`ensure_direct_child`] over two pathnames, opened the way the
    /// removal paths open them.
    #[cfg(test)]
    pub(super) fn test_is_direct_child(parent: &Path, entry: &Path) -> io::Result<()> {
        ensure_direct_child(&probe_directory(parent)?, &open_disposable(entry)?)
    }

    fn open_directory(path: &Path) -> io::Result<std::fs::File> {
        let file = std::fs::OpenOptions::new()
            .access_mode(GENERIC_READ)
            // Omitting FILE_SHARE_DELETE pins this exact directory against
            // rename/deletion while the asynchronous artifact operation lives.
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
    ///
    /// `share_delete` is the difference between the two ways a chain is
    /// built. A chain's ROOT, reached by [`PinnedDir::open`] (the control
    /// directory, a launch's `video/<instance>` or image namespace, a
    /// snapshot's parent), denies delete: nothing renames or removes one of
    /// those while it is pinned, so the denial costs nothing and keeps another
    /// opener from renaming it away.
    ///
    /// A directory reached by [`PinnedDir::child`] — `video/`, a recording, a
    /// retention tombstone, `.capture-locks` — SHARES delete and holds no
    /// DELETE access of its own. Windows checks sharing in both directions on
    /// every open, so while `child` asked for DELETE and denied it to everyone
    /// else, two chains could never hold one directory: a `video frames`
    /// reply's `child("video")` refused the next reply and the next
    /// recording's chain, and a reader could never pin a recording its
    /// producer still held (os error 32, ERROR_SHARING_VIOLATION; four
    /// aterm-gui tests, 2026-10-17). A Unix descriptor stops nothing either.
    /// Renames and removals take a short-lived DELETE handle instead
    /// ([`open_child_for_mutation`]), proven to be the same directory.
    ///
    /// The price: another same-user process CAN rename such a directory
    /// whenever nothing is open inside it — a recording between two frame
    /// writes, a tombstone — and put a junction in its place, or turn the
    /// emptied directory into a junction in place. Its ancestors cannot move:
    /// NTFS answers ERROR_ACCESS_DENIED to renaming a directory with a handle
    /// open beneath it, whatever that handle's share mode. So every entry
    /// opened by pathname through a retained directory — a created, pinned or
    /// read file, a child directory, a lock, a removal — is proven to sit in
    /// the directory the handle retains ([`ensure_direct_child`]): what landed
    /// elsewhere is refused, and what this process created there is disposed
    /// by its own handle before a byte is written. `create_new` and no-replace
    /// renames never touch an entry that was already there, and a replacing
    /// rename runs only after the full path check, with the leaf held in place
    /// by the temporary open inside it. Two residues stay, both creations and
    /// neither a write into or a removal of anything already there: a refused
    /// `create_new` leaves nothing behind (its own handle disposes it), and
    /// `create_child` names its new directory by pathname before `child` proves
    /// it, so on a swapped leaf it can leave one EMPTY directory in the swap's
    /// target and answer an error.
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
        let file = std::fs::OpenOptions::new()
            .access_mode(GENERIC_READ)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "probed path is a reparse point or not a directory",
            ));
        }
        Ok(file)
    }

    fn same_identity(left: &std::fs::File, right: &std::fs::File) -> bool {
        file_identity(left)
            .ok()
            .zip(file_identity(right).ok())
            .is_some_and(|(left, right)| (left.volume, left.index) == (right.volume, right.index))
    }

    /// Open `<parent>\<name>` with DELETE for one rename or removal, and prove
    /// it is BOTH the directory `expected` retains and, right now, a direct
    /// child of the directory `parent` retains: the Unix `renameat`/`unlinkat`
    /// relative to the parent's descriptor. Every retained `child` handle
    /// shares delete, so this succeeds while other pins in this process (a
    /// reader's, a producer's) still hold the directory, as on Unix; it fails
    /// with a sharing violation only against a handle that denies delete — a
    /// chain root (see [`open_mutable_directory`]) or another process's guard.
    /// It denies delete to others itself, so nothing moves the directory
    /// between this proof and the mutation, and its handle open inside
    /// `parent` keeps `parent` in place as well.
    fn open_child_for_mutation(
        parent: &std::fs::File,
        path: &Path,
        expected: &std::fs::File,
    ) -> io::Result<std::fs::File> {
        let file = std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES | DELETE_ACCESS)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        if !same_identity(&file, expected) {
            return Err(identity_changed());
        }
        ensure_direct_child(parent, &file)?;
        Ok(file)
    }

    /// Open a retained read guard for an artifact. Unlike the write and directory guards it
    /// shares `FILE_SHARE_DELETE`: Windows checks sharing both ways, and a live write guard holds
    /// `DELETE_ACCESS` for its whole life (it publishes and rolls back through it), so a mask
    /// without DELETE fails with ERROR_SHARING_VIOLATION on an artifact this process still holds.
    /// Dropping DELETE from the write guard after publication is no fix: `remove_exact` rolls back
    /// through that retained guard.
    ///
    /// This share mask used to omit `FILE_SHARE_DELETE`, so a live read guard
    /// ALSO denied delete/rename of that exact file to every other opener. That
    /// extra denial is deliberately gone. What it cost to keep, and why nothing
    /// load-bearing went with it:
    ///
    /// Windows evaluates sharing in BOTH directions on every open. The new
    /// open's desired access must be inside every live handle's share mask, AND
    /// the new open's share mask must contain every live handle's already
    /// granted access. A retained write guard from
    /// `write_private_inner_with_hook` is granted `GENERIC_WRITE |
    /// DELETE_ACCESS`, and it needs DELETE for its whole retained life:
    /// `SetFileInformationByHandle(FileRenameInfo)` publishes with it and
    /// `remove_exact` rolls back with it. The old read mask permitted that
    /// writer's WRITE but not its DELETE, so reading an artifact this very
    /// process had written and was still holding failed with
    /// ERROR_SHARING_VIOLATION (os error 32). That is not a test-only corner:
    /// the video prune probe originally exposed it while reading artifacts whose
    /// publisher still retained write guards.
    ///
    /// Re-opening the write handle without DELETE once publication finished was
    /// tried on paper and rejected. `remove_exact` on a still-retained guard is
    /// the entire point of handle-anchored rollback (`VideoPublication::abort`
    /// and the snapshot commit paths depend on it), and re-acquiring DELETE
    /// afterwards requires the retained handle to have shared DELETE all along —
    /// which surrenders the WRITE guard's denial, the one
    /// `windows_guards_deny_delete_until_exact_handles_drop` asserts, instead of
    /// this one, for strictly more code and an extra open.
    ///
    /// The read guard never protected a read by exclusion; it protects it by
    /// revalidation. `validate_path_identity` re-probes the name and compares
    /// volume + file index, so a same-uid delete or rename during the read
    /// becomes a fail-closed verdict instead of a false certification — and the
    /// bytes already in hand came from this handle, which Windows keeps valid
    /// even after the name is unlinked. That is precisely the `#[cfg(unix)]`
    /// backend's contract: an open fd there stops neither `unlink` nor
    /// `rename`. Two backends of one abstraction were stating different
    /// contracts, which is itself the defect. The `AnchoredArtifactTransaction`
    /// machine these functions refine agrees — it has no mutual-exclusion
    /// concept at all, and `ReadPinned` is enabled whenever the directory is
    /// pinned. Reader protection that IS specified lives in the refcounted lease
    /// registry (`ArtifactReaderLease` / `mutate_unleased_artifact`), never in a
    /// share mask.
    ///
    /// Retained WRITE guards keep their denial unchanged. Retained DIRECTORY
    /// guards reached through `child` share delete too, for the same reason
    /// (see [`open_mutable_directory`]); chain roots still deny it.
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

    /// Copyable directory identity used by registries without retaining an
    /// extra handle beyond the admitted owner that supplied it.
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
        /// compare its current volume/file-index. The temporary handle closes
        /// on return.
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

        /// Record this open file's identity and close it. A later checkpoint can
        /// detect an ordinary replacement without retaining one handle per
        /// entry; it is not continuous exclusion and kernel IDs may be reused
        /// after the file is deleted.
        pub(crate) fn into_identity(self) -> io::Result<PinnedFileIdentity> {
            self.identity()
        }

        /// The name still reaches this exact file, AND the file sits in the
        /// directory the chain retains. The re-probe alone cannot tell the
        /// second: it walks the same pathname, so through a junction swapped
        /// in for a `child` leaf it finds the same misplaced file and agrees.
        /// The publish paths run this before and after their handle rename,
        /// whose destination is a pathname too, so an entry that a swap moved
        /// out is disposed by its handle rather than published there.
        pub(crate) fn validate_entry_identity_at_retained(&self) -> io::Result<()> {
            let current = probe_regular_file(&self.dir.path.join(&self.name))?;
            if !same_identity(&current, &self.file) {
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

        /// Handle-based exclusive recovery move; no source pathname is ever
        /// conditionally unlinked after another file may have replaced it.
        pub(crate) fn move_to_recovery(self, target: &PinnedDir, name: &OsStr) -> io::Result<()> {
            validate_component(name)?;
            self.validate_path_identity()?;
            target.validate_path_identity()?;
            let outgoing = open_disposable(&self.dir.path.join(&self.name))?;
            if !same_identity(&outgoing, &self.file) {
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
        /// NTFS refuses to rename any directory above an open handle, so while
        /// the temporary is open neither its directory nor any ancestor can
        /// move; the leaf's NAME can still have been swapped for a junction
        /// before the temporary was created, and the rename's pathname
        /// destination follows it — the post-rename check then disposes the
        /// entry by its handle. The batch caller's full path check is what
        /// refuses the swapped chain itself.
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
    }

    impl PinnedDir {
        /// Resolve a caller path while retaining the exact final directory,
        /// then pin and compare the canonical no-reparse chain.
        pub(crate) fn open_resolved(path: &Path) -> io::Result<Self> {
            Self::open_resolved_with_admission(path, |_| true)
        }

        /// Resolve the exact target, then let descriptor admission run against
        /// that canonical depth before any retained ancestor chain is opened.
        pub(crate) fn open_resolved_with_admission(
            path: &Path,
            admit: impl FnOnce(&Path) -> bool,
        ) -> io::Result<Self> {
            let expected = open_directory(path)?;
            let canonical = std::fs::canonicalize(path)?;
            let pinned = Self::open_with_admission(&canonical, admit)?;
            if !same_identity(&expected, pinned.leaf()) {
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
                if !same_identity(&current, expected.as_ref()) {
                    return Err(identity_changed());
                }
            }
            Ok(())
        }

        /// The directory's entries, flushed as the unix arm's plain `fsync(2)`
        /// flushes them, unrecorded ([`flush_directory`]).
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
        /// directory this chain retains. The validation before it is a
        /// statement about one instant, and a `child` leaf shares delete: had
        /// its name been swapped in between, the open would land elsewhere and
        /// every later at-retained operation would anchor to THAT directory.
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
                // Keep the exact lock entry pinned for the advisory-lock lifetime.
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
            if !same_identity(&file, &current) {
                return Err(identity_changed());
            }
            // Two lockers that opened two DIFFERENT files would each win: a
            // `child` directory shares delete, so its name could be swapped
            // between the validation above and the open. The lock must be the
            // one inside the retained directory.
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
            // NTFS refuses to rename a directory above a retained handle and
            // the open proves its lock sits inside the retained directory, so
            // the path-validated open is already capability-stable.
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
                // Excluding FILE_SHARE_DELETE keeps the exact opened file from
                // being swapped between validation and truncation/publication.
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
            // std refuses `create_new` without `write`/`append` before it consults
            // `access_mode`, which still decides the actual open.
            options.write(true);
            options.create_new(true);
            let file = options.open(path)?;
            let file = PinnedFile {
                dir: self.clone(),
                name: name.to_os_string(),
                file,
            };
            let prepare = (|| {
                // Batch members (`validate_path == false`) skip the chain walk
                // above, and even a walked chain only speaks for one instant:
                // a recording shares delete and has nothing open inside it
                // between two frames, so another process can rename it aside
                // and put a junction in its place, and this create then lands
                // in the junction's target (measured on the 2026-09-28
                // snapshot, where a `video 4` wrote 36 frames and `index.json`
                // there and left them). `create_new` guarantees the entry is
                // the one this call made, so a misplaced one is disposed by its
                // own handle below before a byte is written into it.
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
                // The caller's tier, as the unix arm's.
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
            // Ordered, as the unix arm's: every caller is an artifact.
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
                    sync_parent: true,
                },
            )
        }

        /// Publish one file as part of a private batch whose caller supplies the
        /// parent-directory sync barrier before any visibility marker. The file
        /// itself is still flushed (ordered: a video frame or index lands before the
        /// marker that publishes it) and identity-checked.
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
                    sync_parent: false,
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
                    sync_parent: true,
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
            let Publication { flush, sync_parent } = publication;
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
                    sync_parent,
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
                        let file = if sync_parent {
                            file.publish_as_new_with_hook(name, after_publish)?
                        } else {
                            file.publish_as_new_at_retained_with_hook(name, after_publish)?
                        };
                        if sync_parent
                            && let Err(error) =
                                self.sync().and_then(|()| file.validate_path_identity())
                        {
                            let _ = file.remove_exact();
                            return Err(error);
                        }
                        return Ok(file);
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

        /// The ordinary path-validated read: NTFS refuses to rename any
        /// directory above a retained handle, and the guard's entry check
        /// proves the file sits in the retained directory itself.
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

        /// Open one direct child through this retained directory, record its
        /// identity, and close it. This intentionally performs one entry open
        /// and no lexical ancestor walk.
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
                    // The validation above is a statement about one instant: a
                    // `child` directory shares delete, so its name can be
                    // swapped before this open. Delete only what provably
                    // sits inside the retained directory.
                    ensure_direct_child(self.leaf(), &file)?;
                    // `open_disposable` carries FILE_FLAG_BACKUP_SEMANTICS so a reparse point is
                    // unlinked without being followed, which also opens a real directory that
                    // `FileDispositionInfo` would delete if empty. Refuse it, as Unix `unlinkat`
                    // without `AT_REMOVEDIR` does: these rollback paths run on names this process
                    // does not control. A directory symlink or junction is a link and stays
                    // removable.
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
            // Enumerate and open through where the retained handle IS, never
            // through `self.path`: the Unix backend clears by descriptor
            // (`Dir::read_from` + `unlinkat`), and a retention tombstone is
            // cleared right after `rename_child_exact` moved it, when
            // `self.path` still spells the name it had BEFORE the rename.
            // Clearing by that stale path answered os error 2 for every
            // tombstone, so pruned recordings kept their files until the next
            // sweep. Each entry is proven a direct child of the retained
            // directory before anything is removed, so a `here` that a swap
            // made stale between the lookup and an open removes nothing.
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
                    // A reparse point is opened (and removed) as itself, never
                    // followed; files and links alike must be proven to sit in
                    // the retained directory first.
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
        /// and a direct child of this one, by a DELETE handle proven so
        /// ([`open_child_for_mutation`]). Other pins of `child` in this
        /// process share delete, so they never block the removal; Windows
        /// deletes the directory when the last of them closes.
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

        pub(crate) fn current_child_matches(&self, name: &OsStr, expected: &Self) -> bool {
            validate_component(name).is_ok()
                && probe_directory(&self.path.join(name)).is_ok_and(|current| {
                    expected
                        .pinned
                        .components
                        .last()
                        .is_some_and(|(_, expected)| same_identity(&current, expected))
                })
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
            // The destination is a pathname (see `rename_file_handle`): prove
            // the moved directory still sits in this one, under `to`, while
            // its handle keeps this directory from moving.
            let landed = ensure_direct_child(self.leaf(), &handle);
            drop(handle);
            landed?;
            if !self.current_child_matches(to, expected) {
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

    /// The `FILE_RENAME_INFO` request bytes for one destination. `bytes` is the size handed to
    /// `SetFileInformationByHandle`; `buffer` is `usize`-aligned backing store at least that large.
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

    /// Build the rename request for `destination`; split out of [`rename_file_handle`] so
    /// `windows_tests` can assert its shape without a live handle.
    pub(super) fn rename_payload(destination: &Path, replace: bool) -> io::Result<RenamePayload> {
        use aterm_win32::Win32::Storage::FileSystem::FILE_RENAME_INFO;
        use std::os::windows::ffi::OsStrExt as _;

        // NUL-terminate the destination inside the buffer: `SetFileInformationByHandle` also
        // walks `FileName` as a wide C string, and the `usize`-granular buffer has no zero slack
        // when the UTF-16 length is 2 mod 4. `FileNameLength` stays the name's length without
        // the terminator, as the structure documents.
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
            // `RootDirectory` stays NULL from `default()` (see `rename_file_handle`).
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
    ///
    /// `FILE_RENAME_INFO::RootDirectory` — the natural way to anchor the destination
    /// to a directory HANDLE this process already retains — is NOT honored by
    /// `SetFileInformationByHandle`. That field belongs to the NT-native
    /// `NtSetInformationFile` contract; the Win32 wrapper answers a non-NULL
    /// `RootDirectory` with `ERROR_INVALID_PARAMETER`, and its contract is
    /// `RootDirectory = NULL` plus a FULLY-QUALIFIED destination path. Measured
    /// both ways on Windows 11 (2026-08-20): handle+relative → os error 87,
    /// NULL+full path → success. Until it was fixed, this made EVERY private
    /// artifact publication fail on Windows — `aterm-ctl window` reported a write
    /// error on a directory it could plainly write to.
    ///
    /// The SOURCE side keeps its handle anchoring either way: the rename acts on
    /// `file`'s open handle, never on a path re-lookup that could land on a
    /// substitute. The destination stays honest through the callers, which bracket
    /// this with an entry check before and after — `validate_path_identity()`, or
    /// at least `validate_entry_identity_at_retained()` — that proves the renamed
    /// entry a direct child of the retained directory, and dispose it by its
    /// handle (or refuse) if the pathname destination led anywhere else.
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
    /// Admit one already-resolved path, then [`Self::open`] it; a refusal opens nothing.
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
mod shared_backend_contract_tests {
    use super::PinnedDir;

    fn unique_dir(stem: &str) -> std::path::PathBuf {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "aterm-pinned-shared-{stem}-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ))
    }

    /// `remove_file_if_exists` removes a file; a directory on an artifact name survives it on
    /// every backend.
    #[test]
    fn remove_file_if_exists_never_removes_a_directory() {
        let root = unique_dir("remove-file-is-file-only");
        std::fs::create_dir_all(&root).unwrap();
        let pinned = PinnedDir::open(&root).unwrap();

        let occupied = std::ffi::OsStr::new("snapshot.png.txt");
        std::fs::create_dir(root.join(occupied)).unwrap();
        let error = pinned
            .remove_file_if_exists(occupied)
            .expect_err("a directory is not a file this may remove");
        assert!(
            root.join(occupied).is_dir(),
            "the directory on the artifact name must survive a file-scoped removal: {error}"
        );

        // The same call still removes an ordinary file, and still treats an
        // absent name as success.
        let file = std::ffi::OsStr::new("snapshot.png");
        std::fs::write(root.join(file), b"payload").unwrap();
        pinned.remove_file_if_exists(file).unwrap();
        assert!(!root.join(file).exists());
        pinned.remove_file_if_exists(file).unwrap();

        // A Windows chain root denies delete while pinned, so the cleanup below
        // must come after the pin drops or it silently leaves `root` behind.
        drop(pinned);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn closed_file_identity_revalidates_and_rejects_a_replacement() {
        let root = unique_dir("closed-file-identity");
        std::fs::create_dir_all(&root).unwrap();
        let pinned = PinnedDir::open(&root).unwrap();
        let name = std::ffi::OsStr::new("frame.png");
        let identity = pinned
            .write_new_private(name, b"original")
            .unwrap()
            .into_identity()
            .unwrap();
        assert_eq!(
            pinned.private_file_identity_at_retained(name).unwrap(),
            identity,
            "the retained-entry probe returns the open file's identity"
        );
        identity.validate_at_retained(&pinned, name).unwrap();

        // Keep the original allocated so a reused inode/file index cannot pose as the
        // replacement.
        std::fs::rename(root.join(name), root.join("original.png")).unwrap();
        let replacement = pinned.write_new_private(name, b"replacement").unwrap();
        assert!(
            identity.validate_at_retained(&pinned, name).is_err(),
            "the checkpoint must reject a different object at the same name"
        );

        drop(replacement);
        drop(pinned);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A recording is pinned by its producer and, at the same time, by every
    /// `video frames` reader; retention then renames it to a tombstone and
    /// clears it through the capability it already holds.
    ///
    /// On Windows each of those steps failed. A second `child` pin of one
    /// directory answered ERROR_SHARING_VIOLATION (os error 32) while the
    /// first lived, so a reader could never pin a recording its producer still
    /// held. And the clear went through the retained capability's pathname,
    /// which the rename had just made stale, so it answered os error 2 and
    /// every tombstone kept its files. Unix clears by descriptor and never had
    /// either failure.
    #[test]
    fn two_pins_of_one_directory_coexist_and_a_renamed_one_still_clears() {
        let root = unique_dir("two-pins-rename-clear");
        let name = std::ffi::OsStr::new("recording");
        let tombstone = std::ffi::OsStr::new(".prune-recording");
        std::fs::create_dir_all(root.join(name)).unwrap();
        std::fs::write(root.join(name).join("index.json"), b"{}").unwrap();
        let pinned = PinnedDir::open(&root).unwrap();
        let producer = pinned.child(name).unwrap();

        let reader_root = PinnedDir::open_resolved(&root).unwrap();
        let reader = reader_root
            .child(name)
            .expect("a second pin of the same directory in one process");
        let (bytes, index) = reader
            .read_private_at_retained(std::ffi::OsStr::new("index.json"), 16)
            .unwrap();
        assert_eq!(bytes, b"{}");
        drop(index);

        pinned
            .rename_child_exact(name, &producer, tombstone)
            .expect("a live reader pin does not block the exact retention rename");
        assert!(
            reader.validate_path_identity().is_err(),
            "the reader sees its recording renamed away and fails closed"
        );
        let mut budget = 16;
        let cleared = producer.clear_contents_with_budget(&mut budget);
        cleared
            .result
            .expect("the renamed capability clears the directory it retains");
        assert_eq!(cleared.removed, 1);
        assert!(!root.join(tombstone).join("index.json").exists());

        drop(reader);
        drop(reader_root);
        pinned
            .remove_empty_child_exact(tombstone, &producer)
            .unwrap();
        drop(producer);
        assert!(!root.join(tombstone).exists());
        assert!(!root.join(name).exists());
        drop(pinned);
        let _ = std::fs::remove_dir_all(root);
    }

    /// "One ordinary component" is decided on the raw name, so `"link/"` and its kin are refused.
    #[test]
    fn one_component_is_decided_on_the_raw_name() {
        use std::ffi::OsStr;
        let refused = [
            "", ".", "..", "a/b", "/a", "./a", "a/..", "link/", "link/.", "link//", "link/./.",
            "x\0y",
        ];
        for name in refused {
            assert!(
                super::validate_component(OsStr::new(name)).is_err(),
                "{name:?} is not one ordinary component"
            );
        }
        #[cfg(windows)]
        for name in ["a\\b", "link\\", "link\\."] {
            assert!(
                super::validate_component(OsStr::new(name)).is_err(),
                "{name:?} is not one ordinary component"
            );
        }
        // Every name shape production hands a pinned directory still passes.
        let produced = [
            "rec-00000000000000000001-001",
            ".prune-p4242-0123456789abcdef01234567-00000000000000000007",
            ".instance-prune-p4242-x",
            ".aterm-write-p4242-00000000000000000003",
            "p4242-0123456789abcdef01234567",
            ".instance.lease",
            "index.json",
            "index.json.tmp",
            "frame_0001.png",
            "snapshot.png",
            "snapshot.png.txt",
            "snapshot.png.done",
            "images",
            "auto",
            "video",
            ".capture-locks",
        ];
        for name in produced {
            assert!(
                super::validate_component(OsStr::new(name)).is_ok(),
                "{name:?} is one ordinary component"
            );
        }
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::{
        PinnedDir, imp::rename_payload, imp::test_is_direct_child, imp::test_link_count,
        windows_component_is_safe,
    };

    /// The `FILE_RENAME_INFO` payload carries a NUL terminator inside the request bytes at every
    /// destination length. Four consecutive lengths always include the zero-slack case (UTF-16
    /// length 2 mod 4), where allocation padding supplies no zero byte.
    #[test]
    fn rename_payload_nul_terminates_its_destination_at_every_length() {
        let mut zero_slack_seen = 0usize;
        for extra in 0..4usize {
            let destination = std::path::Path::new(r"\\?\C:\aterm-rename-payload")
                .join("x".repeat(extra + 1))
                .join("artifact.png");
            let payload = rename_payload(&destination, true).expect("payload builds");
            let expected = {
                use std::os::windows::ffi::OsStrExt as _;
                destination.as_os_str().encode_wide().collect::<Vec<_>>()
            };

            assert_eq!(
                payload.file_name_length() as usize,
                expected.len() * 2,
                "FileNameLength states the NAME's bytes, excluding the terminator"
            );

            let units = payload.file_name_units();
            assert!(
                units.len() > expected.len(),
                "the request bytes must hold one more unit than the name itself \
                 (destination {} units, request {} units)",
                expected.len(),
                units.len()
            );
            assert_eq!(&units[..expected.len()], &expected[..], "name round-trips");
            assert_eq!(
                units[expected.len()],
                0,
                "the unit after the name must be the NUL the kernel stops on"
            );

            if (std::mem::offset_of!(
                aterm_win32::Win32::Storage::FileSystem::FILE_RENAME_INFO,
                FileName
            ) + expected.len() * 2)
                % std::mem::size_of::<usize>()
                == 0
            {
                zero_slack_seen += 1;
            }
        }
        assert!(
            zero_slack_seen > 0,
            "the sweep must cover the length where allocation padding supplies no zero byte"
        );
    }

    fn unique_dir(stem: &str) -> std::path::PathBuf {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "aterm-pinned-{stem}-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ))
    }

    #[test]
    fn retained_windows_handles_validate_publish_and_delete_exact_children() {
        let root = unique_dir("windows-lifecycle");
        std::fs::create_dir_all(&root).unwrap();
        let pinned = PinnedDir::open(&root).unwrap();
        let index = pinned
            .write_new_private(std::ffi::OsStr::new("index.json"), b"whole")
            .unwrap();
        pinned.sync().unwrap();
        pinned.validate_path_identity().unwrap();
        index.validate_path_identity().unwrap();
        // Reading an artifact whose write guard this process still holds works: the read
        // mask shares DELETE, which the write guard holds for its whole life.
        let (bytes, read) = pinned
            .read_private(std::ffi::OsStr::new("index.json"), 32)
            .unwrap();
        assert_eq!(bytes, b"whole");
        read.validate_path_identity().unwrap();
        // A retained read guard does not deny deletion on Windows, as on Unix: it detects a
        // same-uid mutation and fails closed.
        drop(index);
        std::fs::remove_file(root.join("index.json"))
            .expect("a retained read guard permits deletion, as an open Unix fd does");
        assert!(
            read.validate_path_identity().is_err(),
            "a removed artifact must fail the read guard's identity revalidation"
        );
        drop(read);

        let child = pinned
            .create_child(std::ffi::OsStr::new("recording"))
            .unwrap();
        let frame = child
            .write_new_private(std::ffi::OsStr::new("frame.png"), b"png")
            .unwrap();
        frame.validate_path_identity().unwrap();
        drop(frame);
        pinned
            .remove_child_tree_exact(std::ffi::OsStr::new("recording"), &child)
            .unwrap();
        // The `unwrap` above is the removal verdict. `dispose_file_handle` marks
        // delete-on-close, so the name stays enumerable until `child`'s handle closes (Unix
        // drops it at once): assert after the drop.
        drop(child);
        assert!(!root.join("recording").exists());
        drop(pinned);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn windows_device_stream_and_normalization_aliases_are_rejected() {
        for unsafe_name in [
            "CON",
            "con.txt",
            "CONIN$",
            "conout$.log",
            "COM0",
            "COM1",
            "COM¹",
            "LPT².txt",
            "name:stream",
            "trailing.",
            "trailing ",
        ] {
            assert!(
                !windows_component_is_safe(std::ffi::OsStr::new(unsafe_name)),
                "{unsafe_name:?} must be rejected before CreateFileW"
            );
        }
        for safe_name in ["capture.png", "complete.json", "company.txt", "lptx"] {
            assert!(
                windows_component_is_safe(std::ffi::OsStr::new(safe_name)),
                "{safe_name:?} is an ordinary filename"
            );
        }
    }

    #[test]
    fn windows_hardlink_target_is_rejected_without_mutating_victim() {
        let root = unique_dir("windows-hardlink");
        std::fs::create_dir_all(&root).unwrap();
        let victim = root.join("victim");
        std::fs::write(&victim, b"keep-me").unwrap();
        std::fs::hard_link(&victim, root.join("shot.png")).unwrap();
        let links_before = test_link_count(&victim).unwrap();
        let pinned = PinnedDir::open(&root).unwrap();

        assert!(
            pinned
                .write_private(std::ffi::OsStr::new("shot.png"), b"replace")
                .is_err(),
            "a multiply-linked final component must fail closed"
        );
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep-me");
        assert_eq!(
            test_link_count(&victim).unwrap(),
            links_before,
            "neither hardlink name is removed or replaced"
        );

        drop(pinned);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn windows_guards_deny_delete_until_exact_handles_drop() {
        let root = unique_dir("windows-deny-delete");
        let moved = root.with_extension("moved");
        std::fs::create_dir_all(&root).unwrap();
        let pinned = PinnedDir::open(&root).unwrap();
        let file_path = root.join("shot.png");
        let renamed_path = root.join("renamed.png");
        let file = pinned
            .write_new_private(std::ffi::OsStr::new("shot.png"), b"png")
            .unwrap();

        assert!(
            std::fs::remove_file(&file_path).is_err(),
            "a live PinnedFile denies ordinary deletion"
        );
        assert!(
            std::fs::rename(&file_path, &renamed_path).is_err(),
            "a live PinnedFile denies ordinary rename"
        );
        drop(file);
        std::fs::rename(&file_path, &renamed_path)
            .expect("ordinary file rename succeeds after guard drop");
        std::fs::rename(&renamed_path, &file_path).unwrap();

        assert!(
            std::fs::rename(&root, &moved).is_err(),
            "a live PinnedDir denies ancestor rename"
        );
        drop(pinned);
        std::fs::rename(&root, &moved)
            .expect("ancestor rename succeeds after every directory guard drops");

        let _ = std::fs::remove_dir_all(moved);
    }

    /// The Windows posture of a directory reached through `child`, stated in
    /// code: it SHARES delete, so another opener may rename it (and every
    /// retained chain through it then fails closed), but NTFS refuses to
    /// rename any directory above a live handle, so its ancestors stay put.
    #[test]
    fn windows_child_pins_share_delete_but_pin_every_ancestor() {
        let root = unique_dir("windows-child-posture");
        std::fs::create_dir_all(root.join("video").join("recording")).unwrap();
        let pinned = PinnedDir::open(&root).unwrap();
        let video = pinned.child(std::ffi::OsStr::new("video")).unwrap();
        let recording = video.child(std::ffi::OsStr::new("recording")).unwrap();

        assert!(
            std::fs::rename(root.join("video"), root.join("video-moved")).is_err(),
            "a live pin beneath `video` keeps it in place, though `video` shares delete"
        );
        std::fs::rename(
            root.join("video").join("recording"),
            root.join("video").join("recording-moved"),
        )
        .expect("a `child` pin shares delete, so the pinned leaf itself may be renamed");
        assert!(
            recording.validate_path_identity().is_err(),
            "the chain notices its leaf moved and fails closed"
        );
        video
            .validate_path_identity()
            .expect("the renamed leaf's parent chain is intact");

        drop(recording);
        drop(video);
        drop(pinned);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A pathname open inside a `child` directory is trusted only once its
    /// handle is proven to sit in the retained directory — by the path the
    /// file was actually opened through, so a hard link placed inside cannot
    /// vouch for the same file reached from outside.
    #[test]
    fn windows_direct_child_proof_follows_the_link_that_was_opened() {
        let root = unique_dir("windows-direct-child");
        let inside = root.join("inside");
        let outside = root.join("outside");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("victim"), b"keep-me").unwrap();
        std::fs::hard_link(outside.join("victim"), inside.join("victim")).unwrap();

        test_is_direct_child(&inside, &inside.join("victim"))
            .expect("the link inside the retained directory is its direct child");
        assert!(
            test_is_direct_child(&inside, &outside.join("victim")).is_err(),
            "the same file opened through the outside link is not"
        );
        assert!(
            test_is_direct_child(&inside, &root).is_err(),
            "nor is the retained directory's own parent"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    /// A directory junction at `link`: unlike `symlink_dir` it needs no
    /// privilege or Developer Mode, so the tests that use it run on every
    /// host. `mklink` refuses the `\\?\` spelling `canonicalize` returns ("The
    /// filename, directory name, or volume label syntax is incorrect"), so the
    /// plain spelling is passed. `cmd` is spawned through
    /// `aterm_uds::spawnfd::command`, which decides its console, like every
    /// child this crate starts.
    fn junction(link: &std::path::Path, target: &std::path::Path) {
        let plain = |path: &std::path::Path| {
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

    fn sorted_names(directory: &std::path::Path) -> Vec<String> {
        let mut names = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    /// REGRESSION: a recording is a `child` directory, which shares delete,
    /// and between two frame writes nothing is open inside it — so another
    /// process can rename it aside and put a junction in its place. Measured
    /// on the 2026-09-28 snapshot against `video 4`: every later frame and
    /// `index.json` went into the junction's target and stayed there, because
    /// a batch member skips the chain walk, created its file by pathname, and
    /// its entry check re-probed through the same junction and agreed. Every
    /// pathname open through the retained recording must land in the
    /// directory it retains, or be refused — and disposed by its own handle
    /// when this process created it — which is what Unix `openat` gives by
    /// construction.
    #[test]
    fn windows_swapped_recording_is_neither_written_nor_read_through_the_swap() {
        use std::ffi::OsStr;

        let root = unique_dir("windows-swapped-recording");
        let outside = unique_dir("windows-swapped-recording-outside");
        let named = root.join("rec");
        let moved = root.join("rec-moved");
        std::fs::create_dir_all(&named).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let pinned = PinnedDir::open(&root).unwrap();
        let recording = pinned.child(OsStr::new("rec")).unwrap();
        // One frame the way `write_sealed_frame` writes it: a batch member
        // whose handle is closed straight after, leaving nothing open inside.
        let member = |name: &str, bytes: &[u8]| {
            recording
                .write_new_private_deferred_dir_sync_authorized(OsStr::new(name), bytes, || true)
                .and_then(super::PinnedFile::into_identity)
        };
        member("frame_0001.png", b"one").expect("an unswapped recording takes its frame");

        std::fs::rename(&named, &moved)
            .expect("nothing is open inside the recording between two frames");
        junction(&named, &outside);
        std::fs::write(outside.join("planted.png"), b"planted").unwrap();

        assert!(
            member("frame_0002.png", b"two").is_err(),
            "a batch member refuses a directory that is not the one it retains"
        );
        assert!(
            recording
                .write_new_private(OsStr::new("index.json"), b"{}")
                .is_err(),
            "a fully validated publication refuses the swapped chain"
        );
        assert!(
            recording
                .write_private(OsStr::new("frame_0001.png"), b"replaced")
                .is_err(),
            "so does a replacing one"
        );
        assert!(
            recording
                .pin_private_file_at_retained(OsStr::new("planted.png"))
                .is_err(),
            "a file planted in the junction's target is not the recording's"
        );
        assert!(
            recording
                .private_file_identity_at_retained(OsStr::new("planted.png"))
                .is_err(),
            "nor may it vouch for a frame seal"
        );
        assert!(
            recording
                .remove_file_if_exists(OsStr::new("planted.png"))
                .is_err(),
            "nor be removed through the swap"
        );
        assert_eq!(
            sorted_names(&outside),
            ["planted.png"],
            "nothing was written into, removed from, or left in the junction's target"
        );
        assert_eq!(
            std::fs::read(outside.join("planted.png")).unwrap(),
            b"planted"
        );

        // A junction that leads back to the recording reaches the directory
        // it retains, so a member lands there — the write follows the
        // directory, not its name, as `openat` does. The full path check the
        // batch barrier runs still refuses the swapped chain.
        std::fs::remove_dir(&named).unwrap();
        junction(&named, &moved);
        member("frame_0003.png", b"three").expect("the retained directory takes its frame");
        assert!(recording.validate_path_identity().is_err());
        assert_eq!(sorted_names(&moved), ["frame_0001.png", "frame_0003.png"]);
        assert_eq!(sorted_names(&outside), ["planted.png"]);

        std::fs::remove_dir(&named).unwrap();
        drop(recording);
        drop(pinned);
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    /// A retention tombstone renamed under a junction'd parent name is never
    /// cleared through the junction: the clear opens every entry where the
    /// retained handle IS and proves it a direct child before removing it.
    #[test]
    fn windows_renamed_recording_clears_only_what_it_retains() {
        use std::ffi::OsStr;

        let root = unique_dir("windows-renamed-clear");
        let outside = unique_dir("windows-renamed-clear-outside");
        std::fs::create_dir_all(root.join("rec")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("rec").join("frame_0001.png"), b"one").unwrap();
        std::fs::write(outside.join("keep.txt"), b"keep").unwrap();
        let pinned = PinnedDir::open(&root).unwrap();
        let recording = pinned.child(OsStr::new("rec")).unwrap();

        // Another opener renames the pinned recording (its `child` pin shares
        // delete) and parks a junction to an outside directory on its name.
        std::fs::rename(root.join("rec"), root.join("rec-moved")).unwrap();
        junction(&root.join("rec"), &outside);

        let mut budget = 16;
        let cleared = recording.clear_contents_with_budget(&mut budget);
        cleared
            .result
            .expect("the clear follows the retained handle to its new name");
        assert_eq!(cleared.removed, 1);
        assert!(sorted_names(&root.join("rec-moved")).is_empty());
        assert_eq!(std::fs::read(outside.join("keep.txt")).unwrap(), b"keep");
        assert!(
            pinned
                .remove_empty_child_exact(OsStr::new("rec"), &recording)
                .is_err(),
            "the junction on the old name is not the retained recording"
        );
        assert!(root.join("rec-moved").is_dir());
        assert_eq!(sorted_names(&outside), ["keep.txt"]);

        std::fs::remove_dir(root.join("rec")).unwrap();
        drop(recording);
        drop(pinned);
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn windows_reparse_directory_is_never_followed_during_cleanup() {
        let root = unique_dir("windows-reparse");
        let outside = unique_dir("windows-reparse-outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), b"keep").unwrap();
        junction(&root.join("recording"), &outside);
        let pinned = PinnedDir::open(&root).unwrap();
        assert!(
            pinned
                .remove_child_tree(std::ffi::OsStr::new("recording"))
                .is_err(),
            "a junction is not opened as a recording directory"
        );
        assert_eq!(std::fs::read(outside.join("keep.txt")).unwrap(), b"keep");
        drop(pinned);
        let _ = std::fs::remove_dir(root.join("recording"));
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    use super::{
        PINNED_DIR_OPEN_COMPONENT_LIMIT, PINNED_DIR_OPERATION_DESCRIPTOR_UNITS, PinnedDir,
        REMOVE_LIVE_FIXED_UNITS, REMOVE_MAX_DEPTH, pinned_chain_open_count,
        reset_pinned_chain_open_count,
    };

    fn unique_dir(stem: &str) -> std::path::PathBuf {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "aterm-pinned-{stem}-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ))
    }

    /// An artifact's bytes are only ordered ahead of what follows (aterm-durable's
    /// counting seam), never fully flushed: the requester is local and reads the
    /// page cache, and no recovery reads them. A write that asks for
    /// [`Flush::Durable`] still gets it (no caller asks now that the network drive
    /// is gone, but the tier is plumbed), and the directory flush,
    /// [`PinnedDir::sync`], is the untouched plain `fsync(2)`, which goes through no
    /// aterm-durable arm and so records nothing.
    #[test]
    fn an_artifacts_bytes_are_ordered_and_a_durable_write_stays_durable() {
        use aterm_durable::{Flush, seam::Recording};
        use std::ffi::OsStr;
        let root = unique_dir("flush-tiers");
        std::fs::create_dir_all(&root).unwrap();
        let dir = PinnedDir::open_resolved(&root).unwrap();
        let recording = Recording::start();
        let shot = dir.write_private(OsStr::new("shot.png"), b"png").unwrap();
        assert_eq!(
            recording.take(),
            [Flush::Ordered],
            "a replaced artifact's bytes; its entry takes the plain fsync(2)"
        );
        let done = dir
            .write_new_private(OsStr::new("shot.done"), b"done")
            .unwrap();
        assert_eq!(recording.take(), [Flush::Ordered], "a new artifact's bytes");
        let frame = dir
            .write_new_private_deferred_dir_sync_authorized(OsStr::new("frame.png"), b"f", || true)
            .unwrap();
        assert_eq!(
            recording.take(),
            [Flush::Ordered],
            "a video batch member's bytes"
        );
        let token = dir
            .write_new_private_with_hooks(
                OsStr::new("token"),
                b"secret",
                || {},
                || {},
                Flush::Durable,
            )
            .unwrap();
        assert_eq!(recording.take(), [Flush::Durable], "a durable write");
        dir.sync().unwrap();
        assert_eq!(recording.take(), [], "the directory's plain fsync(2)");
        drop((shot, done, frame, token));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `O_NOFOLLOW`, `unlinkat` and `renameat` govern only the final component, so `"link/"`,
    /// `"link/."` and `"link//"` must be refused as names, never resolved through the symlink.
    #[test]
    fn a_trailing_separator_never_reaches_through_a_symlink() {
        use std::ffi::OsStr;
        let root = unique_dir("trailing-separator");
        let confined = root.join("confined");
        let outside = root.join("outside");
        std::fs::create_dir_all(&confined).unwrap();
        std::fs::create_dir_all(outside.join("victim")).unwrap();
        std::fs::create_dir_all(outside.join("tree")).unwrap();
        std::fs::write(outside.join("tree").join("keep"), b"outside").unwrap();
        symlink(&outside, confined.join("link")).unwrap();
        symlink(outside.join("victim"), confined.join("vlink")).unwrap();
        symlink(outside.join("tree"), confined.join("tlink")).unwrap();
        symlink(outside.join("absent"), confined.join("dangle")).unwrap();
        let pinned = PinnedDir::open(&confined).unwrap();

        let mut escapes = Vec::new();
        for name in ["link/", "link/.", "link//", "link/./."] {
            match pinned.child(OsStr::new(name)) {
                Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {}
                other => escapes.push(format!(
                    "child({name:?}) was not refused: {:?}",
                    other.map(|child| child.path().to_path_buf())
                )),
            }
        }
        let _ = pinned.remove_child_tree(OsStr::new("tlink/"));
        if !outside.join("tree").join("keep").exists() {
            escapes.push("remove_child_tree(\"tlink/\") emptied an outside directory".into());
        }
        let _ = pinned.rename(OsStr::new("vlink/"), OsStr::new("moved"));
        if !outside.join("victim").is_dir() {
            escapes.push("rename(\"vlink/\", ..) moved an outside directory in".into());
        }
        let _ = pinned.create_child(OsStr::new("dangle/"));
        if outside.join("absent").exists() {
            escapes.push("create_child(\"dangle/\") made a directory outside".into());
        }
        // Control: the bare name is refused too; the link is never followed.
        assert!(pinned.child(OsStr::new("link")).is_err());
        drop(pinned);
        let _ = std::fs::remove_dir_all(&root);
        assert!(escapes.is_empty(), "{escapes:#?}");
    }

    /// A same-uid actor can retarget a symlink between admission and confinement, so admission
    /// runs against the exact resolved target, and the hard depth ceiling fires too, both before
    /// any ancestor handle opens.
    #[test]
    fn deep_resolved_path_is_rejected_before_ancestor_chain_open() {
        let root = unique_dir("deep-resolved-budget");
        let mut deep_parent = root.join("target");
        while deep_parent.components().count() + 1 < PINNED_DIR_OPEN_COMPONENT_LIMIT {
            deep_parent.push("d");
        }
        let deep = deep_parent.join("leaf");
        std::fs::create_dir_all(&deep).unwrap();
        let alias = root.join("alias");
        symlink(&deep_parent, &alias).unwrap();
        let resolved_alias = alias.join("leaf");

        reset_pinned_chain_open_count();
        let mut admission_ran = false;
        let error = PinnedDir::open_resolved_with_admission(&resolved_alias, |canonical| {
            admission_ran = true;
            assert_eq!(
                canonical.components().count(),
                PINNED_DIR_OPEN_COMPONENT_LIMIT
            );
            assert_eq!(pinned_chain_open_count(), 0);
            false
        })
        .expect_err("aggregate admission must be able to refuse the exact resolved depth");
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        assert!(admission_ran);
        assert_eq!(
            pinned_chain_open_count(),
            0,
            "admission refusal must occur before any retained ancestor handle opens"
        );

        let too_deep = deep.join("d");
        std::fs::create_dir(&too_deep).unwrap();
        reset_pinned_chain_open_count();
        let error = PinnedDir::open_resolved(&resolved_alias.join("d"))
            .expect_err("a resolved path deeper than the descriptor budget must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(
            pinned_chain_open_count(),
            0,
            "the depth check must run before any retained ancestor handle opens"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn recursive_cleanup_depth_fits_the_charged_suffix() {
        assert_eq!(
            REMOVE_LIVE_FIXED_UNITS + REMOVE_MAX_DEPTH + 1,
            PINNED_DIR_OPERATION_DESCRIPTOR_UNITS,
            "fixed publication handles plus the deepest opened child must fit admission"
        );

        let root = unique_dir("cleanup-depth-budget");
        let recording = root.join("instance/recording");
        let mut nested = recording.clone();
        for depth in 0..=REMOVE_MAX_DEPTH {
            nested.push(format!("d{depth}"));
        }
        std::fs::create_dir_all(&nested).unwrap();
        let instance = PinnedDir::open(&root.join("instance")).unwrap();
        let error = instance
            .remove_child_tree(std::ffi::OsStr::new("recording"))
            .expect_err("a deeper owner-namespace tree must fail before exceeding its charge");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            recording.is_dir(),
            "fail-closed cleanup leaves the interfered exact tree for a later bounded sweep"
        );

        drop(instance);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn clear_contents_reports_only_successful_mutations() {
        let root = unique_dir("clear-mutation-count");
        std::fs::create_dir_all(root.join("nested")).unwrap();
        let pinned = PinnedDir::open(&root).unwrap();

        let mut budget = 8;
        let empty = pinned.clear_contents_with_budget(&mut budget);
        empty.result.unwrap();
        assert_eq!(empty.removed, 1, "the empty nested directory was removed");

        std::fs::write(root.join("first"), b"one").unwrap();
        std::fs::create_dir(root.join("nested")).unwrap();
        std::fs::write(root.join("nested/second"), b"two").unwrap();
        let mut budget = 8;
        let cleared = pinned.clear_contents_with_budget(&mut budget);
        cleared.result.unwrap();
        assert_eq!(
            cleared.removed, 3,
            "two files plus their containing directory are real mutations"
        );
        assert_eq!(budget, 5, "the scan budget tracks the same three entries");

        let mut budget = 8;
        let no_op = pinned.clear_contents_with_budget(&mut budget);
        no_op.result.unwrap();
        assert_eq!(no_op.removed, 0, "an empty sweep must not request fsync");
        assert_eq!(budget, 8);

        drop(pinned);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A directory lock is an open file description of its own: two locks through one pin
    /// exclude each other, and a lease released by `LOCK_UN` ([`HeldAdvisoryLock`]) is free at
    /// once, even while a child forked under it has yet to exec. Negative control: a lock let go
    /// by the close alone stays taken in that child's copy.
    ///
    /// [`HeldAdvisoryLock`]: crate::native_document_host::HeldAdvisoryLock
    #[test]
    fn directory_lock_has_an_independent_unlock_lifetime() {
        use crate::native_document_host::HeldAdvisoryLock;
        use crate::parked_fork::ParkedChild;
        use std::fs::TryLockError;

        let root = unique_dir("directory-lock");
        std::fs::create_dir_all(&root).unwrap();
        let pin = PinnedDir::open(&root).unwrap();
        let contender = pin.open_directory_lock().unwrap();
        #[expect(
            clippy::disallowed_methods,
            reason = "the contender's probe; the one take it makes is let go by `unlock` below"
        )]
        let try_contender = || contender.try_lock();
        let refused = || matches!(try_contender(), Err(TryLockError::WouldBlock));

        let lease = HeldAdvisoryLock::try_lock(pin.open_directory_lock().unwrap())
            .expect("the lease takes the directory lock");
        assert!(
            refused(),
            "a second lock through one pin is its own description"
        );
        let child = ParkedChild::fork();
        drop(lease);
        try_contender().expect("the dropped lease's lock is free at once, its pin still open");
        assert!(
            child.still_parked(),
            "the child forked under it had yet to exec"
        );
        child.release();
        contender.unlock().unwrap();

        // Negative control: released by the close alone, the lock lives on in the parked
        // child's copy of the description until that child execs.
        let lease = pin.open_directory_lock().unwrap();
        #[expect(
            clippy::disallowed_methods,
            reason = "the negative control: a lock let go by the close alone"
        )]
        lease.try_lock().unwrap();
        let child = ParkedChild::fork();
        drop(lease);
        assert!(
            refused(),
            "control: closed, not unlocked, the child's copy holds it"
        );
        child.release();

        drop((contender, pin));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn retained_leaf_never_redirects_and_fails_closed_after_path_replacement() {
        let root = unique_dir("ancestor-swap");
        let outside = unique_dir("outside");
        std::fs::create_dir_all(root.join("images")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let pinned = PinnedDir::open(&root.join("images")).unwrap();

        let moved = root.join("images-moved");
        std::fs::rename(root.join("images"), &moved).unwrap();
        symlink(&outside, root.join("images")).unwrap();

        assert!(
            pinned
                .write_private(std::ffi::OsStr::new("shot.png"), b"safe")
                .is_err()
        );
        assert!(
            !moved.join("shot.png").exists(),
            "the exact temporary/published artifact is removed on final validation failure"
        );
        assert!(
            !outside.join("shot.png").exists(),
            "the replacement ancestor receives no artifact bytes"
        );
        assert!(pinned.validate_path_identity().is_err());

        let _ = std::fs::remove_file(root.join("images"));
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn deferred_batch_write_uses_only_the_retained_directory_capability() {
        let root = unique_dir("deferred-batch-swap");
        let outside = unique_dir("deferred-batch-outside");
        let original = root.join("recording");
        let moved = root.join("recording-moved");
        std::fs::create_dir_all(&original).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let pinned = PinnedDir::open(&original).unwrap();

        std::fs::rename(&original, &moved).unwrap();
        symlink(&outside, &original).unwrap();

        let frame = pinned
            .write_new_private_deferred_dir_sync_authorized(
                std::ffi::OsStr::new("frame.png"),
                b"retained",
                || true,
            )
            .expect("batch members use the already-authorized retained directory");
        frame.validate_entry_identity_at_retained().unwrap();
        assert!(
            frame.validate_path_identity().is_err(),
            "ordinary exact-path validation remains fail-closed after replacement"
        );
        assert_eq!(std::fs::read(moved.join("frame.png")).unwrap(), b"retained");
        assert!(
            !outside.join("frame.png").exists(),
            "the replacement namespace never receives batch bytes"
        );

        drop(frame);
        drop(pinned);
        let _ = std::fs::remove_file(original);
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn retained_parent_removes_only_its_original_child_after_path_replacement() {
        let root = unique_dir("cleanup-swap");
        let replacement = unique_dir("cleanup-replacement");
        std::fs::create_dir_all(root.join("instance/rec-1")).unwrap();
        std::fs::write(root.join("instance/rec-1/frame.png"), b"old").unwrap();
        std::fs::create_dir_all(replacement.join("rec-1")).unwrap();
        std::fs::write(replacement.join("rec-1/keep.txt"), b"keep").unwrap();
        let instance = PinnedDir::open(&root.join("instance")).unwrap();

        let moved = root.join("instance-moved");
        std::fs::rename(root.join("instance"), &moved).unwrap();
        symlink(&replacement, root.join("instance")).unwrap();
        instance
            .remove_child_tree(std::ffi::OsStr::new("rec-1"))
            .unwrap();

        assert!(!moved.join("rec-1").exists());
        assert_eq!(
            std::fs::read(replacement.join("rec-1/keep.txt")).unwrap(),
            b"keep"
        );
        assert!(instance.validate_path_identity().is_err());

        let _ = std::fs::remove_file(root.join("instance"));
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(replacement);
    }

    #[test]
    fn retained_file_probes_stay_on_original_directory_after_path_replacement() {
        let root = unique_dir("retained-read-swap");
        let original = root.join("instance");
        let moved = root.join("instance-moved");
        std::fs::create_dir_all(&original).unwrap();
        std::fs::write(original.join("index.json"), b"original-index").unwrap();
        std::fs::write(original.join("frame.png"), b"original-frame").unwrap();
        let pinned = PinnedDir::open(&original).unwrap();

        std::fs::rename(&original, &moved).unwrap();
        std::fs::create_dir(&original).unwrap();
        std::fs::write(original.join("index.json"), b"replacement-index").unwrap();
        std::fs::write(original.join("frame.png"), b"replacement-frame").unwrap();

        assert!(
            pinned.validate_path_identity().is_err(),
            "the ordinary path authority must reject the replaced namespace"
        );
        let (index, index_guard) = pinned
            .read_private_at_retained(std::ffi::OsStr::new("index.json"), 64)
            .expect("capability-bound read stays on the retained directory");
        let frame_guard = pinned
            .pin_private_file_at_retained(std::ffi::OsStr::new("frame.png"))
            .expect("capability-bound pin stays on the retained directory");
        assert_eq!(index, b"original-index");
        drop(frame_guard);
        drop(index_guard);
        assert_eq!(
            std::fs::read(original.join("index.json")).unwrap(),
            b"replacement-index",
            "the lexical replacement is never read or modified"
        );
        assert_eq!(
            std::fs::read(original.join("frame.png")).unwrap(),
            b"replacement-frame"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn hardlink_target_is_rejected_before_bytes_or_mode_change() {
        let root = unique_dir("hardlink");
        std::fs::create_dir_all(&root).unwrap();
        let victim = root.join("victim");
        std::fs::write(&victim, b"keep-me").unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o640)).unwrap();
        std::fs::hard_link(&victim, root.join("shot.png")).unwrap();
        let pinned = PinnedDir::open(&root).unwrap();

        assert!(
            pinned
                .write_private(std::ffi::OsStr::new("shot.png"), b"replace")
                .is_err()
        );
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep-me");
        assert_eq!(
            std::fs::metadata(&victim).unwrap().permissions().mode() & 0o777,
            0o640
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn fifo_target_fails_without_blocking() {
        use std::os::unix::ffi::OsStrExt as _;

        let root = unique_dir("fifo");
        std::fs::create_dir_all(&root).unwrap();
        let fifo = root.join("shot.png");
        let path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: `path` is NUL-terminated and points to an absent path.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let pinned = PinnedDir::open(&root).unwrap();
        // A liveness check, not a stopwatch: without O_NONBLOCK this open blocks until a
        // writer appears (never), so the call runs on a helper and is awaited for a minute.
        let (answered, answer) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = answered.send(
                pinned
                    .write_private(std::ffi::OsStr::new("shot.png"), b"replace")
                    .is_err(),
            );
        });
        let refused = answer
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("O_NONBLOCK must make a writerless FIFO fail rather than block");
        assert!(refused, "a FIFO is not a regular file to replace");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn exact_file_guard_rejects_replacement_and_create_new_does_not_modify_it() {
        let root = unique_dir("file-swap");
        std::fs::create_dir_all(&root).unwrap();
        let pinned = PinnedDir::open(&root).unwrap();
        let file = pinned
            .write_new_private(std::ffi::OsStr::new("index.json.tmp"), b"ours")
            .unwrap();
        pinned
            .remove_file_if_exists(std::ffi::OsStr::new("index.json.tmp"))
            .unwrap();
        std::fs::write(root.join("index.json.tmp"), b"replacement").unwrap();

        assert!(file.validate_path_identity().is_err());
        std::fs::write(root.join("index.json"), b"planted-final").unwrap();
        assert!(
            pinned
                .write_new_private(std::ffi::OsStr::new("index.json"), b"ours")
                .is_err()
        );
        assert_eq!(
            std::fs::read(root.join("index.json.tmp")).unwrap(),
            b"replacement"
        );
        assert_eq!(
            std::fs::read(root.join("index.json")).unwrap(),
            b"planted-final"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn failed_atomic_publication_leaves_no_private_temporary_file() {
        let root = unique_dir("publication-cleanup");
        std::fs::create_dir_all(root.join("index.json")).unwrap();
        let pinned = PinnedDir::open(&root).unwrap();

        assert!(
            pinned
                .write_new_private(std::ffi::OsStr::new("index.json"), b"complete")
                .is_err()
        );
        assert!(root.join("index.json").is_dir());
        assert!(
            std::fs::read_dir(&root).unwrap().flatten().all(|entry| {
                !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".aterm-write-")
            }),
            "the exact temporary file is removed when no-replace publication fails"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn enumeration_stops_at_the_requested_bound() {
        let root = unique_dir("bounded-enumeration");
        std::fs::create_dir_all(&root).unwrap();
        for index in 0..5 {
            std::fs::write(root.join(format!("{index}.txt")), b"x").unwrap();
        }
        let pinned = PinnedDir::open(&root).unwrap();
        assert!(pinned.names(4).is_err());
        assert_eq!(pinned.names(5).unwrap().len(), 5);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unlink_postcheck_never_certifies_a_swapped_replacement() {
        let root = unique_dir("unlink-barrier");
        std::fs::create_dir_all(root.join("recording")).unwrap();
        let parent = PinnedDir::open(&root).unwrap();
        let child = parent.child(std::ffi::OsStr::new("recording")).unwrap();
        let moved = root.join("recording-moved");
        let root_for_hook = root.clone();
        let moved_for_hook = moved.clone();

        let error = parent
            .remove_empty_child_exact_with_hook(
                std::ffi::OsStr::new("recording"),
                &child,
                move || {
                    std::fs::rename(root_for_hook.join("recording"), &moved_for_hook).unwrap();
                    std::fs::create_dir(root_for_hook.join("recording")).unwrap();
                },
            )
            .expect_err("unlinking a swapped direct child must not report exact cleanup");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(
            moved.is_dir(),
            "the retained inode remains and the post-check detects that fact"
        );
        assert!(
            !root.join("recording").exists(),
            "the only residual side effect is confined to the owned parent"
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
