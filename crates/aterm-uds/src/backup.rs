// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Leaving a file or a folder out of backups: on macOS the attribute `tmutil addexclusion`
//! writes (`com.apple.metadata:com_apple_backup_excludeItem`), which Time Machine honours and
//! which needs no permission and no dialog. Elsewhere there is no such mark: [`Backups::exclude`]
//! says so rather than report a mark nothing wrote, and a backup tool there has to be told by its
//! own configuration.
//!
//! The attribute travels with the file, so a caller that marks a new file before it gets its name
//! never shows that name unmarked. [`Backups`] is the injected platform field: [`Backups::SYSTEM`]
//! is the platform's mark and [`Backups::INERT`] the arm a test takes when it is not about the
//! mark.

use std::fs::File;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arm {
    System,
    Inert,
    #[cfg(any(test, debug_assertions))]
    NoMark,
}

/// How a caller leaves a file out of backups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backups {
    arm: Arm,
}

impl Backups {
    /// The platform's mark, where it has one.
    pub const SYSTEM: Self = Self { arm: Arm::System };
    /// Marks nothing and reports nothing unmarked.
    pub const INERT: Self = Self { arm: Arm::Inert };
    /// Answers as a system with no mark does, on any system: what a test of a caller's words for
    /// one takes.
    #[cfg(any(test, debug_assertions))]
    pub const NO_MARK: Self = Self { arm: Arm::NoMark };

    /// Whether this system has a mark at all. Where it has none, [`Self::exclude`] is an error and
    /// nothing is left out of any backup.
    #[must_use]
    pub const fn system_has_a_mark() -> bool {
        cfg!(target_os = "macos")
    }

    /// Leave the open `file` out of backups. Marking one already marked writes nothing.
    /// # Errors
    /// The attribute could not be written (a volume that keeps no extended attributes, say), or
    /// this system has no mark ([`io::ErrorKind::Unsupported`]).
    pub fn exclude(self, file: &File) -> io::Result<()> {
        match self.arm {
            Arm::System => platform::exclude(file),
            Arm::Inert => Ok(()),
            #[cfg(any(test, debug_assertions))]
            Arm::NoMark => Err(no_mark()),
        }
    }

    /// [`Self::exclude`] by path, following a link.
    /// # Errors
    /// `path` does not open, or [`Self::exclude`]'s.
    pub fn exclude_path(self, path: &Path) -> io::Result<()> {
        if self.arm == Arm::Inert {
            return Ok(());
        }
        #[cfg(unix)]
        let file = {
            use std::os::unix::fs::OpenOptionsExt as _;
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(path)?
        };
        #[cfg(not(unix))]
        let file = File::open(path)?;
        self.exclude(&file)
    }

    /// Whether the open `file` is left out of backups: never on a system with no mark.
    #[must_use]
    pub fn excluded(self, file: &File) -> bool {
        match self.arm {
            Arm::System => platform::excluded(file),
            Arm::Inert => true,
            #[cfg(any(test, debug_assertions))]
            Arm::NoMark => false,
        }
    }
}

#[cfg(any(test, debug_assertions, not(target_os = "macos")))]
fn no_mark() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "this system has no mark a backup honours: a backup tool here has to be told to skip it",
    )
}

#[cfg(target_os = "macos")]
mod platform {
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd as _;

    const ATTRIBUTE: &std::ffi::CStr = c"com.apple.metadata:com_apple_backup_excludeItem";

    /// A binary property list of `true`, the value this writes. `tmutil addexclusion` writes
    /// another one, the string `com.apple.backupd` (measured on macOS 27.0), and `tmutil
    /// isexcluded` says `[Excluded]` for either.
    const TRUE_BPLIST: &[u8] = &[
        0x62, 0x70, 0x6c, 0x69, 0x73, 0x74, 0x30, 0x30, 0x09, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x09,
    ];

    /// Whether the attribute is on `file`, whatever value it holds: `tmutil`'s counts, and is left
    /// as it is.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(super) fn excluded(file: &File) -> bool {
        // SAFETY: `file` keeps its descriptor open across the call and the attribute name is a
        // NUL-terminated string live for it; a null buffer of length 0 asks `fgetxattr` only for
        // the value's length, or -1, and it writes nothing.
        let n = unsafe {
            libc::fgetxattr(
                file.as_raw_fd(),
                ATTRIBUTE.as_ptr(),
                std::ptr::null_mut(),
                0,
                0,
                0,
            )
        };
        n > 0
    }

    #[cfg_attr(trust_verify, trust::skip)]
    pub(super) fn exclude(file: &File) -> io::Result<()> {
        if excluded(file) {
            return Ok(());
        }
        set(file, TRUE_BPLIST)
    }

    #[cfg_attr(trust_verify, trust::skip)]
    pub(super) fn set(file: &File, value: &[u8]) -> io::Result<()> {
        // SAFETY: `file` keeps its descriptor open across the call, the attribute name is a
        // NUL-terminated `'static` string, and `value` is a slice live for the call of the length
        // passed.
        let wrote = unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                ATTRIBUTE.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
                0,
            )
        };
        if wrote == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// The attribute's value, for a test to compare.
    #[cfg(test)]
    pub(super) fn value(file: &File) -> Vec<u8> {
        let mut have = [0u8; 128];
        // SAFETY: as [`excluded`], and `have` is a writable buffer of the length passed; `fgetxattr`
        // writes at most that many bytes and answers how many, or -1.
        let n = unsafe {
            libc::fgetxattr(
                file.as_raw_fd(),
                ATTRIBUTE.as_ptr(),
                have.as_mut_ptr().cast(),
                have.len(),
                0,
                0,
            )
        };
        have.get(..usize::try_from(n).expect("a value"))
            .expect("within the buffer")
            .to_vec()
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::fs::File;
    use std::io;

    pub(super) fn excluded(_file: &File) -> bool {
        false
    }

    pub(super) fn exclude(_file: &File) -> io::Result<()> {
        Err(super::no_mark())
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;

    fn scratch_file() -> (aterm_tempfile::TempDir, std::path::PathBuf, File) {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let path = dir.path().join("kept");
        let file = File::create(&path).expect("create");
        (dir, path, file)
    }

    /// The inert arm writes nothing, whatever it answers: a control that the attribute below is
    /// the system arm's own doing.
    #[test]
    #[cfg(target_os = "macos")]
    fn the_system_arm_marks_a_file_and_the_inert_arm_leaves_it_alone() {
        let (_dir, path, file) = scratch_file();
        Backups::INERT.exclude(&file).expect("inert");
        assert!(!Backups::SYSTEM.excluded(&file), "the inert arm wrote it");
        assert!(Backups::INERT.excluded(&file));
        Backups::SYSTEM.exclude(&file).expect("marked");
        assert!(Backups::SYSTEM.excluded(&file));
        Backups::SYSTEM.exclude(&file).expect("marked twice");
        let again = File::open(&path).expect("reopen");
        assert!(Backups::SYSTEM.excluded(&again), "it is on the file");
    }

    /// By path reaches the same mark, and a folder takes it.
    #[test]
    #[cfg(target_os = "macos")]
    fn a_folder_is_marked_by_its_path() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        Backups::SYSTEM.exclude_path(dir.path()).expect("marked");
        let folder = File::open(dir.path()).expect("open");
        assert!(Backups::SYSTEM.excluded(&folder));
    }

    /// The value `tmutil addexclusion` wrote (`xattr -px` on a file it excluded, macOS 27.0) reads
    /// as excluded, and marking over it leaves it: compared by bytes, a file `tmutil` excluded
    /// read as unmarked, and the next mark overwrote its value.
    #[test]
    #[cfg(target_os = "macos")]
    fn the_value_tmutil_writes_counts_and_is_left_alone() {
        const TMUTIL: &[u8] = &[
            0x62, 0x70, 0x6C, 0x69, 0x73, 0x74, 0x30, 0x30, 0x5F, 0x10, 0x11, 0x63, 0x6F, 0x6D,
            0x2E, 0x61, 0x70, 0x70, 0x6C, 0x65, 0x2E, 0x62, 0x61, 0x63, 0x6B, 0x75, 0x70, 0x64,
            0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x1C,
        ];
        let (_dir, _path, file) = scratch_file();
        platform::set(&file, TMUTIL).expect("the attribute");
        assert!(Backups::SYSTEM.excluded(&file), "tmutil's own value");
        Backups::SYSTEM.exclude(&file).expect("marked");
        assert_eq!(platform::value(&file), TMUTIL, "and it is left as it was");
    }

    /// A system with no mark says so at every step: an `Ok` here is what let a caller report a key
    /// left out of backups that a backup copies.
    #[test]
    fn a_system_with_no_mark_says_so() {
        let (_dir, path, file) = scratch_file();
        assert_eq!(
            Backups::NO_MARK.exclude(&file).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            Backups::NO_MARK.exclude_path(&path).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert!(!Backups::NO_MARK.excluded(&file));
    }

    /// This system's own answer agrees with [`Backups::system_has_a_mark`], so the arm above stands
    /// for the real one wherever there is no mark.
    #[test]
    fn the_system_arm_answers_as_the_system_does() {
        let (_dir, _path, file) = scratch_file();
        let marked = Backups::SYSTEM.exclude(&file);
        assert_eq!(marked.is_ok(), Backups::system_has_a_mark(), "{marked:?}");
        assert_eq!(
            Backups::SYSTEM.excluded(&file),
            Backups::system_has_a_mark()
        );
    }

    /// A path that names nothing is an error, not a silent success: the mark is what a caller
    /// relies on.
    #[test]
    fn a_missing_path_says_so() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let err = Backups::SYSTEM
            .exclude_path(&dir.path().join("none"))
            .expect_err("no such file");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
