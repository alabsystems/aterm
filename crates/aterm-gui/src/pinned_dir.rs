// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Directory capabilities retained across asynchronous artifact work.
//!
//! A canonical `PathBuf` is only a statement about one instant. Another
//! same-uid process can rename an ancestor and put a symlink or replacement
//! directory at the old pathname before an encode worker writes. `PinnedDir`
//! instead opens every absolute-path component without following links and
//! retains the resulting handles. Mutations are relative to the retained leaf
//! on Unix; Windows retains deny-delete handles for every ancestor and validates
//! their file identities around path-based operations.
//!
//! POSIX has no portable unlink-by-file-descriptor operation. Recursive cleanup
//! therefore traverses only retained, no-follow directory handles and checks the
//! retained child's link count after the final `unlinkat`. A same-uid process can
//! still swap an empty direct child during that syscall window; the replacement
//! may be removed inside this server-owned directory, but the post-check returns
//! an error and never falsely certifies that the retained inode was removed.
//! The same POSIX limitation applies to `PinnedFile::remove_exact`: “exact”
//! describes the success verdict, not an impossible side-effect guarantee. A
//! same-uid process can swap a direct file entry after the identity check and
//! before `unlinkat`; that replacement entry may be unlinked inside the pinned,
//! server-owned `0700` directory, but no operation follows/traverses it, nothing
//! outside that directory is mutated, and the retained-inode post-check prevents
//! a false `Ok`.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Component, Path, PathBuf};

fn validate_component(name: &OsStr) -> io::Result<()> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
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

#[cfg(test)]
fn reset_pinned_chain_open_count() {
    PINNED_CHAIN_OPEN_COUNT.set(0);
}

#[cfg(test)]
fn pinned_chain_open_count() -> usize {
    PINNED_CHAIN_OPEN_COUNT.get()
}

#[cfg(unix)]
mod imp {
    use std::io::{Read as _, Write as _};
    use std::sync::Arc;

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
        Component, OsStr, OsString, Path, PathBuf, identity_changed, io, validate_component,
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
            test,
            aterm_spec::refines(
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

        /// Reopen the exact retained directory inode for a cross-process
        /// advisory lock. Unlike a replaceable child lockfile, every process
        /// writing through this pinned directory necessarily locks the same
        /// authority even if a same-uid actor renames child entries.
        ///
        /// This must be a new open-file description, not `dup`: `flock` locks
        /// are attached to the open-file description on Unix, so a duplicated
        /// descriptor would keep the lease locked until the longer-lived
        /// [`PinnedDir`] descriptor also closed.
        pub(crate) fn open_directory_lock(&self) -> io::Result<std::fs::File> {
            Ok(std::fs::File::from(open_directory_at(self.leaf(), ".")?))
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
                file.file.sync_all()
            })();
            if let Err(error) = result {
                let _ = file.remove_exact();
                return Err(error);
            }
            Ok(file)
        }

        fn write_private_inner(&self, name: &OsStr, bytes: &[u8]) -> io::Result<PinnedFile> {
            let mut after_create = Some(|| {});
            self.write_private_inner_with_hook(name, bytes, &mut after_create)
        }

        fn temporary_name(sequence: u64) -> OsString {
            OsString::from(format!(
                ".aterm-write-p{}-{sequence:020}",
                std::process::id()
            ))
        }

        #[cfg_attr(
            test,
            aterm_spec::refines(
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
                true,
            )
        }

        /// Publish one durable file as part of a private batch whose caller
        /// supplies the parent-directory sync barrier before any visibility
        /// marker. The file itself is still fsynced and identity-checked.
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
                false,
            )
        }

        /// Create and fsync an invisible temporary file, then atomically
        /// publish it without replacing any existing final component.
        pub(crate) fn write_new_private_with_hooks(
            &self,
            name: &OsStr,
            bytes: &[u8],
            after_temp_create: impl FnOnce(),
            after_publish: impl FnOnce(),
        ) -> io::Result<PinnedFile> {
            self.write_new_private_with_authorizer_and_hooks(
                name,
                bytes,
                || true,
                after_temp_create,
                after_publish,
                true,
            )
        }

        fn write_new_private_with_authorizer_and_hooks(
            &self,
            name: &OsStr,
            bytes: &[u8],
            authorize: impl FnOnce() -> bool,
            after_temp_create: impl FnOnce(),
            after_publish: impl FnOnce(),
            sync_parent: bool,
        ) -> io::Result<PinnedFile> {
            validate_component(name)?;
            static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let mut authorize = Some(authorize);
            let mut after_temp_create = Some(after_temp_create);
            let mut after_publish = Some(after_publish);
            for _ in 0..32 {
                let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let temporary = Self::temporary_name(sequence);
                match self.write_private_inner_with_hook(&temporary, bytes, &mut after_temp_create)
                {
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
        /// Unlike [`Self::read_private`], an ancestor rename is allowed: this is
        /// reserved for cleanup that must stay on an already-authorized inode.
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

        #[cfg_attr(
            test,
            aterm_spec::refines(
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
            let (bytes, guard) = self.read_private_at_retained(name, limit)?;
            guard.validate_path_identity()?;
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

        /// Hard-bounded enumeration: overrunning `limit` is an ERROR, not a
        /// truncation. Every retention caller moved to `names_up_to` (see its
        /// note: a directory that outgrew the bound must not become a permanent
        /// failure), so this stricter variant is compiled for the TEST build
        /// only — it is where the fail-closed bound is still stated and
        /// exercised, and shipping it would be unreachable code.
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

        fn remove_contents(&self, budget: &mut usize, depth: usize) -> io::Result<()> {
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
                    Ok(child) => child.remove_contents(budget, depth + 1).and_then(|()| {
                        if !self.current_child_matches(&name, &child) {
                            return Err(identity_changed());
                        }
                        unlinkat(self.leaf(), &name, AtFlags::REMOVEDIR)
                            .map_err(io::Error::from)?;
                        if !directory_was_unlinked(child.leaf()) {
                            return Err(identity_changed());
                        }
                        Ok(())
                    }),
                    Err(_) => {
                        unlinkat(self.leaf(), &name, AtFlags::empty()).map_err(io::Error::from)
                    }
                };
                if let Err(error) = result
                    && first_error.is_none()
                {
                    first_error = Some(error);
                }
            }
            first_error.map_or(Ok(()), Err)
        }

        pub(crate) fn clear_contents_with_budget(&self, budget: &mut usize) -> io::Result<()> {
            self.remove_contents(budget, 0)
        }

        /// Remove a child tree named only BY NAME, resolving it through the
        /// retained parent. Every production cleanup already holds the child's
        /// own handle and therefore goes through `remove_child_tree_exact`, so
        /// this variant is compiled for the TEST build only: it is what lets a
        /// test hand the parent nothing but a name AFTER the parent's pathname
        /// was swapped, proving the removal still lands on the retained inode
        /// and never on the replacement. Shipping it would be unreachable code.
        #[cfg(test)]
        pub(crate) fn remove_child_tree(&self, name: &OsStr) -> io::Result<()> {
            validate_component(name)?;
            let child = match self.child(name) {
                Ok(child) => child,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error),
            };
            let mut budget = super::REMOVE_MAX_ENTRIES;
            child.remove_contents(&mut budget, 0)?;
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
            child.remove_contents(&mut budget, 0)?;
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
    }
}

#[cfg(windows)]
mod imp {
    use std::io::{Read as _, Write as _};
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    use std::os::windows::io::AsRawHandle as _;
    use std::sync::Arc;

    use super::{
        OsStr, OsString, Path, PathBuf, identity_changed, io, validate_component,
        validate_open_path,
    };

    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    const DELETE_ACCESS: u32 = 0x0001_0000;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    fn windows_io_error(error: windows::core::Error) -> io::Error {
        let raw = error.code().0 as u32;
        if raw & 0xFFFF_0000 == 0x8007_0000 {
            io::Error::from_raw_os_error((raw & 0xFFFF) as i32)
        } else {
            io::Error::other(error.to_string())
        }
    }

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

    fn open_mutable_directory(path: &Path, delete: bool) -> io::Result<std::fs::File> {
        let access = GENERIC_READ | GENERIC_WRITE | if delete { DELETE_ACCESS } else { 0 };
        let file = std::fs::OpenOptions::new()
            .access_mode(access)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
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

    /// Open the retained READ guard for an artifact.
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
    /// Retained WRITE guards and retained DIRECTORY guards keep their denials
    /// unchanged; only this read path relaxed.
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

        pub(crate) fn validate_entry_identity_at_retained(&self) -> io::Result<()> {
            let current = probe_regular_file(&self.dir.path.join(&self.name))?;
            if !same_identity(&current, &self.file) {
                return Err(identity_changed());
            }
            Ok(())
        }

        pub(crate) fn validate_path_identity(&self) -> io::Result<()> {
            self.dir.validate_path_identity()?;
            self.validate_entry_identity_at_retained()
        }

        pub(crate) fn remove_exact(self) -> io::Result<()> {
            dispose_file_handle(&self.file)
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
        /// the temporary/final entry. Retained Windows directory handles deny
        /// ancestor replacement; the batch caller performs the full path checks.
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
            test,
            aterm_spec::refines(
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

        pub(crate) fn sync(&self) -> io::Result<()> {
            self.leaf().sync_all()
        }

        pub(crate) fn child(&self, name: &OsStr) -> io::Result<Self> {
            validate_component(name)?;
            self.validate_path_identity()?;
            let path = self.path.join(name);
            let child = open_mutable_directory(&path, true)?;
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

        pub(crate) fn open_namespace_lock(&self, name: &OsStr) -> io::Result<std::fs::File> {
            validate_component(name)?;
            self.validate_path_identity()?;
            let path = self.path.join(name);
            let mut options = std::fs::OpenOptions::new();
            options
                .read(true)
                .write(true)
                .create(true)
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
            Ok(file)
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
            // REQUIRED even though `access_mode` above is what actually reaches
            // `CreateFileW`. std validates the PORTABLE flags first, and a creation
            // disposition without `write`/`append` is rejected there —
            // "creating or truncating a file requires write or append access" —
            // before the Windows-specific access mask is ever consulted. Without
            // this line every private artifact write failed on Windows, which is
            // how `aterm-ctl window` came to answer with a write error on a
            // directory it could plainly write to. `get_access_mode` still returns
            // the explicit mask (an explicit `access_mode` outranks these), so this
            // changes the validation outcome and nothing about the actual open.
            options.write(true);
            options.create_new(true);
            let file = options.open(path)?;
            let file = PinnedFile {
                dir: self.clone(),
                name: name.to_os_string(),
                file,
            };
            let prepare = (|| {
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
                file.file.sync_all()
            })();
            if let Err(error) = result {
                let _ = file.remove_exact();
                return Err(error);
            }
            Ok(file)
        }

        fn write_private_inner(&self, name: &OsStr, bytes: &[u8]) -> io::Result<PinnedFile> {
            let mut after_create = Some(|| {});
            self.write_private_inner_with_hook(name, bytes, &mut after_create, true)
        }

        fn temporary_name(sequence: u64) -> OsString {
            OsString::from(format!(
                ".aterm-write-p{}-{sequence:020}",
                std::process::id()
            ))
        }

        #[cfg_attr(
            test,
            aterm_spec::refines(
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
                true,
            )
        }

        /// Publish one durable file as part of a private batch whose caller
        /// supplies the parent-directory sync barrier before any visibility
        /// marker. The file itself is still fsynced and identity-checked.
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
                false,
            )
        }

        pub(crate) fn write_new_private_with_hooks(
            &self,
            name: &OsStr,
            bytes: &[u8],
            after_temp_create: impl FnOnce(),
            after_publish: impl FnOnce(),
        ) -> io::Result<PinnedFile> {
            self.write_new_private_with_authorizer_and_hooks(
                name,
                bytes,
                || true,
                after_temp_create,
                after_publish,
                true,
            )
        }

        fn write_new_private_with_authorizer_and_hooks(
            &self,
            name: &OsStr,
            bytes: &[u8],
            authorize: impl FnOnce() -> bool,
            after_temp_create: impl FnOnce(),
            after_publish: impl FnOnce(),
            sync_parent: bool,
        ) -> io::Result<PinnedFile> {
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
            test,
            aterm_spec::refines(
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

        /// Windows directory handles deny ancestor rename for their lifetime, so
        /// the retained-capability operation is the ordinary path-validated one.
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
            let (_file, identity) = open_regular_file_with_identity(&self.path.join(name))?;
            PinnedFileIdentity::from_raw(identity)
        }

        pub(crate) fn remove_file_if_exists(&self, name: &OsStr) -> io::Result<()> {
            validate_component(name)?;
            self.validate_path_identity()?;
            match open_disposable(&self.path.join(name)) {
                Ok(file) => {
                    // `open_disposable` carries FILE_FLAG_BACKUP_SEMANTICS so a
                    // reparse point can be unlinked WITHOUT being followed —
                    // which also lets a real directory open, and
                    // `FileDispositionInfo` will happily delete an empty one.
                    // Unix removes with `unlinkat` and NO `AT_REMOVEDIR`, so a
                    // directory there answers EISDIR and survives. Two backends
                    // of one abstraction stating different contracts is itself
                    // the defect: a file-scoped cleanup must never destroy a
                    // directory a same-uid actor parked on an artifact name, and
                    // these callers are exactly the best-effort rollback paths
                    // (`write_snapshot_artifacts`, video/image publication) that
                    // run on names outside this process's control. A directory
                    // SYMLINK/junction stays removable — that is a link, and
                    // `unlinkat` removes it too.
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
            probe_regular_file(&self.path.join(name)).is_ok()
        }

        #[cfg(test)]
        pub(crate) fn names(&self, limit: usize) -> io::Result<Vec<OsString>> {
            self.validate_path_identity()?;
            let mut names = Vec::with_capacity(limit.min(256));
            for entry in std::fs::read_dir(&self.path)? {
                if names.len() == limit {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "artifact directory exceeded its entry bound",
                    ));
                }
                names.push(entry?.file_name());
            }
            Ok(names)
        }

        pub(crate) fn names_up_to(&self, limit: usize) -> io::Result<Vec<OsString>> {
            self.validate_path_identity()?;
            let mut names = Vec::with_capacity(limit.min(256));
            for entry in std::fs::read_dir(&self.path)? {
                if names.len() == limit {
                    break;
                }
                names.push(entry?.file_name());
            }
            Ok(names)
        }

        fn remove_contents(&self, budget: &mut usize, depth: usize) -> io::Result<()> {
            if depth > super::REMOVE_MAX_DEPTH {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "artifact cleanup exceeded its directory-depth bound",
                ));
            }
            let mut first_error = None;
            for name in self.names_up_to((*budget).saturating_add(1))? {
                if *budget == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "artifact cleanup exceeded its entry bound",
                    ));
                }
                *budget -= 1;
                let path = self.path.join(&name);
                let result = match std::fs::symlink_metadata(&path) {
                    Ok(metadata)
                        if metadata.is_dir()
                            && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 =>
                    {
                        self.child(&name).and_then(|child| {
                            child.remove_contents(budget, depth + 1)?;
                            if !self.current_child_matches(&name, &child) {
                                return Err(identity_changed());
                            }
                            dispose_file_handle(child.leaf())
                        })
                    }
                    Ok(metadata)
                        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 =>
                    {
                        open_disposable(&path).and_then(|file| dispose_file_handle(&file))
                    }
                    Ok(_) => open_disposable(&path).and_then(|file| dispose_file_handle(&file)),
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

        pub(crate) fn clear_contents_with_budget(&self, budget: &mut usize) -> io::Result<()> {
            self.remove_contents(budget, 0)
        }

        #[cfg(test)]
        pub(crate) fn remove_child_tree(&self, name: &OsStr) -> io::Result<()> {
            validate_component(name)?;
            let child = match self.child(name) {
                Ok(child) => child,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error),
            };
            let mut budget = super::REMOVE_MAX_ENTRIES;
            child.remove_contents(&mut budget, 0)?;
            if !self.current_child_matches(name, &child) {
                return Err(identity_changed());
            }
            dispose_file_handle(child.leaf())
        }

        pub(crate) fn remove_child_tree_exact(&self, name: &OsStr, child: &Self) -> io::Result<()> {
            validate_component(name)?;
            let mut budget = super::REMOVE_MAX_ENTRIES;
            child.remove_contents(&mut budget, 0)?;
            if !self.current_child_matches(name, child) {
                return Err(identity_changed());
            }
            dispose_file_handle(child.leaf())
        }

        pub(crate) fn remove_empty_child_exact(
            &self,
            name: &OsStr,
            child: &Self,
        ) -> io::Result<()> {
            validate_component(name)?;
            if !self.current_child_matches(name, child) {
                return Err(identity_changed());
            }
            dispose_file_handle(child.leaf())
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
            if !self.current_child_matches(from, expected) {
                return Err(identity_changed());
            }
            rename_file_handle(expected.leaf(), &self.path, to, false)?;
            let current = probe_directory(&self.path.join(to))?;
            if !same_identity(&current, expected.leaf()) {
                return Err(identity_changed());
            }
            Ok(())
        }
    }

    fn open_disposable(path: &Path) -> io::Result<std::fs::File> {
        std::fs::OpenOptions::new()
            .access_mode(GENERIC_READ | DELETE_ACCESS)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }

    fn dispose_file_handle(file: &std::fs::File) -> io::Result<()> {
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Storage::FileSystem::{
            FILE_DISPOSITION_INFO, FileDispositionInfo, SetFileInformationByHandle,
        };

        let information = FILE_DISPOSITION_INFO { DeleteFile: true };
        // SAFETY: `file` owns a live handle opened with DELETE access; the
        // information pointer and exact structure size match FileDispositionInfo.
        unsafe {
            SetFileInformationByHandle(
                HANDLE(file.as_raw_handle()),
                FileDispositionInfo,
                (&raw const information).cast(),
                u32::try_from(std::mem::size_of_val(&information))
                    .expect("FILE_DISPOSITION_INFO size fits u32"),
            )
            .map_err(windows_io_error)
        }
    }

    /// The exact `FILE_RENAME_INFO` request bytes for one destination, split out
    /// so the payload's shape is directly assertable. `bytes` is the size handed
    /// to `SetFileInformationByHandle`; `buffer` is `usize`-aligned backing store
    /// at least that large.
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
            use windows::Win32::Storage::FileSystem::FILE_RENAME_INFO;
            let header = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
            let mut raw = [0_u8; 4];
            raw.copy_from_slice(&self.request_bytes()[header - 4..header]);
            u32::from_ne_bytes(raw)
        }

        /// The UTF-16 units the kernel finds at `FileName`, terminator included.
        #[cfg(test)]
        #[must_use]
        pub(super) fn file_name_units(&self) -> Vec<u16> {
            use windows::Win32::Storage::FileSystem::FILE_RENAME_INFO;
            let header = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
            self.request_bytes()[header..]
                .chunks_exact(2)
                .map(|pair| u16::from_ne_bytes([pair[0], pair[1]]))
                .collect()
        }
    }

    /// Build the rename request for `destination`. Split out of
    /// [`rename_file_handle`] so `windows_tests` can assert the payload's shape
    /// without a live handle; see that function's doc comment for why the
    /// terminator is load-bearing.
    pub(super) fn rename_payload(destination: &Path, replace: bool) -> io::Result<RenamePayload> {
        use std::os::windows::ffi::OsStrExt as _;
        use windows::Win32::Storage::FileSystem::FILE_RENAME_INFO;

        // NUL-TERMINATE the destination, and let the buffer size cover the
        // terminator. `FileNameLength` alone does NOT bound what
        // `SetFileInformationByHandle` reads: the Win32 wrapper also walks the
        // name as a wide C string. Without the terminator it ran off the end of
        // this allocation, and the only thing that ever stopped it was the
        // padding `vec![0_usize; words]` happens to leave — `bytes` is
        // `offset_of(FileName) + 2 * len`, which is a multiple of
        // `size_of::<usize>()` exactly when the destination's UTF-16 length is
        // 2 mod 4, and in that case there is NO slack and no zero byte.
        // Measured on Windows 11 (2026-08-27): every failing publication had
        // zero slack, and the same path succeeded or answered os error 123
        // (ERROR_INVALID_NAME) / os error 2 (ERROR_FILE_NOT_FOUND) from one run
        // to the next purely on what followed the allocation. That made EVERY
        // confined artifact write — snapshot, `image`/`window` capture, video
        // frame — fail nondeterministically on a path-length parity nobody
        // chose. `FileNameLength` stays the length of the NAME, excluding the
        // terminator, which is what the structure documents.
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
        // SAFETY: `Vec<usize>` provides at least pointer alignment and enough
        // bytes for the header plus the UTF-16 payload INCLUDING its NUL
        // terminator, so both a length-driven and a terminator-driven reader
        // stay inside this allocation.
        unsafe {
            std::ptr::write(information, FILE_RENAME_INFO::default());
            (*information).Anonymous.ReplaceIfExists = replace;
            // RootDirectory stays NULL (from `default()`) — see the doc comment:
            // `SetFileInformationByHandle` rejects any other value.
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

    /// Rename the file behind `file`'s HANDLE to `name` inside `parent_path`.
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
    /// this with `validate_path_identity()` before and after — and `PinnedDir::path`
    /// is the canonicalized path of the directory those pinned handles refer to.
    fn rename_file_handle(
        file: &std::fs::File,
        parent_path: &Path,
        name: &OsStr,
        replace: bool,
    ) -> io::Result<()> {
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Storage::FileSystem::{FileRenameInfo, SetFileInformationByHandle};

        validate_component(name)?;
        let mut payload = rename_payload(&parent_path.join(name), replace)?;
        let bytes = u32::try_from(payload.bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "rename too large"))?;
        // SAFETY: `payload` owns `bytes` initialized, `usize`-aligned bytes
        // holding one FILE_RENAME_INFO plus its NUL-terminated destination.
        unsafe {
            SetFileInformationByHandle(
                HANDLE(file.as_raw_handle()),
                FileRenameInfo,
                payload.as_mut_ptr().cast(),
                bytes,
            )
            .map_err(windows_io_error)
        }
    }
}

pub(crate) use imp::{PinnedDir, PinnedDirIdentity, PinnedFile, PinnedFileIdentity};

impl PinnedDir {
    /// Admit one already-resolved path before constructing its retained
    /// ancestor chain. The ordinary [`Self::open`] remains the common,
    /// refinement-anchored implementation after admission succeeds.
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

/// Contracts BOTH backends owe, asserted with one body so they cannot drift.
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

    /// REGRESSION: `remove_file_if_exists` removes a FILE. A directory sitting on
    /// an artifact name must survive it, on every backend.
    ///
    /// Unix removes with `unlinkat` and no `AT_REMOVEDIR`, so a directory there
    /// answers EISDIR. Windows opens with `FILE_FLAG_BACKUP_SEMANTICS` (needed to
    /// unlink a reparse point without following it), which also opens a real
    /// directory, and `FileDispositionInfo` then deleted an empty one outright.
    /// The callers are best-effort rollback paths — `write_snapshot_artifacts`
    /// cleaning up after a failed payload write, video/image publication abort —
    /// running on names this process does not control, so the Windows backend was
    /// silently destroying a directory a same-uid actor had parked there while
    /// the Unix backend left it alone.
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

        // Keep the original object allocated so this test cannot accidentally
        // pass an immediately reused inode/file-index as the replacement.
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
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::{PinnedDir, imp::rename_payload, imp::test_link_count, windows_component_is_safe};

    /// REGRESSION: the `FILE_RENAME_INFO` payload must carry a NUL terminator
    /// INSIDE the request bytes, at every destination length.
    ///
    /// `FileNameLength` does not bound what `SetFileInformationByHandle` reads —
    /// it also walks `FileName` as a wide C string. The payload buffer is
    /// `usize`-granular, so `offset_of(FileName) + 2 * len` leaves zero slack
    /// exactly when the destination's UTF-16 length is 2 mod 4. Without an
    /// explicit terminator those lengths shipped an unterminated name and the
    /// kernel read past the allocation, so a confined artifact publication
    /// answered os error 123 / os error 2 / success depending only on the heap
    /// byte that happened to follow. Sweeping four consecutive lengths always
    /// covers the zero-slack case, so this fails without the terminator no
    /// matter where the sweep starts.
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
                windows::Win32::Storage::FileSystem::FILE_RENAME_INFO,
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
        // Reading an artifact whose WRITE guard this process still retains must
        // work. The write guard holds DELETE_ACCESS for its whole life (rename
        // publication and `remove_exact` both need it), so a read mask without
        // FILE_SHARE_DELETE answered ERROR_SHARING_VIOLATION here — which is
        // what the video prune probe originally hit against a live publisher.
        let (bytes, read) = pinned
            .read_private(std::ffi::OsStr::new("index.json"), 32)
            .unwrap();
        assert_eq!(bytes, b"whole");
        read.validate_path_identity().unwrap();
        // The price, stated in code rather than left implicit: a retained READ
        // guard no longer denies deletion on Windows. Its contract is now the
        // Unix one — it does not EXCLUDE a same-uid mutation, it DETECTS one and
        // fails closed. Both halves are asserted so neither can rot silently.
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
        // The `unwrap` above is the removal verdict; this `exists` check is only
        // about WHEN the name stops being enumerable. `dispose_file_handle` is
        // Windows' handle-anchored removal and it marks delete-on-close, so the
        // committed entry survives until the last handle to that exact inode
        // closes — `child` is still holding one right here. (`std`'s `metadata`
        // falls back to a directory-entry query when the open is refused, so a
        // delete-pending name still reports as existing.) Unix `unlinkat` drops
        // the name at once, so the two backends differ in observation point, not
        // in guarantee. Assert once the handle this call was anchored to is gone.
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

    #[test]
    fn windows_reparse_directory_is_never_followed_during_cleanup() {
        use std::os::windows::fs::symlink_dir;

        let root = unique_dir("windows-reparse");
        let outside = unique_dir("windows-reparse-outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), b"keep").unwrap();
        let link = root.join("recording");
        if symlink_dir(&outside, &link).is_err() {
            // Developer Mode / symlink privilege is not guaranteed on every
            // Windows CI host; lifecycle coverage above remains mandatory.
            let _ = std::fs::remove_dir_all(root);
            let _ = std::fs::remove_dir_all(outside);
            return;
        }
        let pinned = PinnedDir::open(&root).unwrap();
        assert!(
            pinned
                .remove_child_tree(std::ffi::OsStr::new("recording"))
                .is_err(),
            "a junction/symlink is not opened as a recording directory"
        );
        assert_eq!(std::fs::read(outside.join("keep.txt")).unwrap(), b"keep");
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

    /// REGRESSION: admission charges the caller's canonical path before an
    /// artifact job starts, but a same-uid actor can retarget a short symlink
    /// before confinement resolves it again. Reconcile against that exact
    /// target before constructing its retained ancestor chain. The hard depth
    /// ceiling is the second backstop and must also fire before chain opening.
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
    fn directory_lock_has_an_independent_unlock_lifetime() {
        let root = unique_dir("directory-lock");
        std::fs::create_dir_all(&root).unwrap();
        let first_pin = PinnedDir::open(&root).unwrap();
        let second_pin = PinnedDir::open(&root).unwrap();
        let first_lock = first_pin.open_directory_lock().unwrap();
        let second_lock = second_pin.open_directory_lock().unwrap();

        first_lock.try_lock().unwrap();
        assert!(matches!(
            second_lock.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(first_lock);
        // The property under test IS drop-release, so the drop stays — but
        // `flock` rides the open-file-description, and a concurrent test's
        // `fork()`ed child (any `pre_exec` Command or raw fork) that arrived
        // between our open and this drop holds a duplicate of `first_lock`'s
        // descriptor and keeps the lock alive until its execve/_exit —
        // measured up to ~523 ms under pathological load. Wait that transient
        // out. A real regression — a duplicate tied to the still-open
        // `first_pin`, or a descriptor stashed in a static — never releases
        // and still fails the budget.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match second_lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => {
                    panic!("dropping the lease handle must release its directory lock: {error:?}")
                }
            }
        }

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
        assert!(
            pinned
                .read_private(std::ffi::OsStr::new("index.json"), 64)
                .is_err(),
            "ordinary reads retain their lexical fail-closed contract"
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
        let started = std::time::Instant::now();
        assert!(
            pinned
                .write_private(std::ffi::OsStr::new("shot.png"), b"replace")
                .is_err()
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "O_NONBLOCK must make a writerless FIFO fail promptly"
        );

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
