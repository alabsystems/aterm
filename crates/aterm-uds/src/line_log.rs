// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Newline-framed append logs whose writers cooperate with atomic file replacement.
//! The sibling lock remains at its path; callers must not unlink it. Paths aliased through
//! hard links, and external writers that do not take this lock, are outside this protocol.

use std::fs::{File, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::Path;
use std::time::{Duration, Instant};

/// Serialize appends and replacement by a stable sibling (`<filename>.lock`). A stalled
/// holder cannot block a supervisor indefinitely: actual contention waits at most 250 ms.
/// Hold the returned guard across the entire read/transform/atomic-replace operation.
///
/// # Errors
/// An invalid filename or [`crate::file_lock::LockFile::take`]'s path, admission or lock error.
pub fn lock(path: &Path) -> io::Result<crate::file_lock::LockFile> {
    let mut name = path
        .file_name()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?
        .to_os_string();
    name.push(".lock");
    let now = Instant::now();
    let deadline = now.checked_add(Duration::from_millis(250)).unwrap_or(now);
    crate::file_lock::LockFile::take(
        &path.with_file_name(name),
        deadline,
        "journal is being updated by another process",
    )
}

/// Create or validate the regular log, without retaining an open descriptor across replacement.
/// A new Unix log is created with mode 0600, subject to the caller's umask.
///
/// # Errors
/// The path, lock, open or metadata error; final links, reparse points and non-regular files
/// are refused. The parent directory must already exist.
pub fn ensure(path: &Path) -> io::Result<()> {
    let _lock = lock(path)?;
    open_append(path).map(drop)
}

/// Append one line and its newline, reopening the current named file under [`lock`]. Preserve
/// all prior bytes, adding a delimiter if the old final row was interrupted. This checks the
/// writes and flush, without promising power-loss durability or repairing malformed old rows.
///
/// # Errors
/// A newline in `line`, [`ensure`]'s admission errors, or any read, seek, write or flush error.
pub fn append(path: &Path, line: &str) -> io::Result<()> {
    if line.contains('\n') {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    hold(path)?.append(&[line]).map(drop)
}

/// The log at a path, open under its [`lock`] until dropped: a writer that must read what other
/// writers appended before it decides what to append (a counter folded from the rows) reads and
/// appends under one hold.
pub struct Held {
    _lock: crate::file_lock::LockFile,
    file: File,
}

/// Which file a [`Held`] log is and how long: a reader that kept an earlier identity reads only
/// the bytes past its `len`, or the whole file again where `dev` or `ino` changed (another
/// process replaced it) or it shrank.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The device the file is on (on Windows its volume's serial number; 0 elsewhere off Unix).
    pub dev: u64,
    /// Its inode (on Windows its 128-bit file id on that volume; 0 elsewhere off Unix).
    pub ino: u128,
    /// Its length in bytes.
    pub len: u64,
}

/// [`Identity`] of the open file `file`.
///
/// Windows' is read through the handle ([`by_handle`]): no `Metadata` read by stable std names a
/// file's id there, and its creation time is no identity, since a file renamed over another
/// within seconds takes the replaced file's creation time (file system tunneling), so a log
/// replaced by its compaction would read as the same file.
///
/// # Errors
/// Its metadata's (on Windows, `GetFileInformationByHandle`'s).
pub fn identity_of(file: &File) -> io::Result<Identity> {
    #[cfg(windows)]
    {
        by_handle(file)
    }
    #[cfg(not(windows))]
    {
        Ok(of_metadata(&file.metadata()?))
    }
}

/// [`Identity`] of whatever is at `path` itself, a final link not followed: a reader's look
/// before it opens the file. On Windows the look opens the path for no access, sharing every
/// mode, so no writer, reader or replacement of the file is refused while it is open.
///
/// # Errors
/// The look's, `NotFound` where nothing is there.
pub fn identity_at(path: &Path) -> io::Result<Identity> {
    #[cfg(windows)]
    {
        by_handle(&look(path)?)
    }
    #[cfg(not(windows))]
    {
        Ok(of_metadata(&std::fs::symlink_metadata(path)?))
    }
}

/// [`identity_at`] and the modified time of the same look: one `lstat` on unix, one handle on
/// Windows, so both name the one file that was there. The time is `None` where the platform reads
/// none.
///
/// # Errors
/// [`identity_at`]'s, and on Windows the handle's metadata read's.
pub fn identity_and_modified_at(
    path: &Path,
) -> io::Result<(Identity, Option<std::time::SystemTime>)> {
    #[cfg(windows)]
    {
        let file = look(path)?;
        Ok((by_handle(&file)?, file.metadata()?.modified().ok()))
    }
    #[cfg(not(windows))]
    {
        let meta = std::fs::symlink_metadata(path)?;
        Ok((of_metadata(&meta), meta.modified().ok()))
    }
}

/// A handle on whatever is at `path` itself, as [`identity_at`] looks: no access asked, every
/// sharing allowed (so no writer, reader or replacement of the file is refused while it is
/// open), a folder, link or other reparse point opened as itself. Its identity
/// ([`identity_of`]), metadata and change-journal entry name the one entry that was there.
///
/// # Errors
/// The open's: nothing at `path`, or a folder on the way that may not be traversed.
#[cfg(windows)]
pub fn look(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    const FILE_SHARE_ALL: u32 = 0x1 | 0x2 | 0x4; // FILE_SHARE_READ | _WRITE | _DELETE
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    OpenOptions::new()
        .access_mode(0)
        .share_mode(FILE_SHARE_ALL)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(windows))]
fn of_metadata(meta: &std::fs::Metadata) -> Identity {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Identity {
            dev: meta.dev(),
            ino: u128::from(meta.ino()),
            len: meta.len(),
        }
    }
    #[cfg(not(unix))]
    {
        Identity {
            dev: 0,
            ino: 0,
            len: meta.len(),
        }
    }
}

/// The volume serial number, file id and length of `file`'s handle: the id and serial from
/// `FILE_ID_INFO` (`GetFileInformationByHandleEx`), whose 128-bit id names a file uniquely on
/// ReFS (a Dev Drive) as on NTFS, where `BY_HANDLE_FILE_INFORMATION`'s 64-bit index may repeat
/// on ReFS; that index and serial where the file system answers no `FILE_ID_INFO`, or an id of
/// zero (FAT, exFAT); the length from `GetFileInformationByHandle`.
#[cfg(windows)]
fn by_handle(file: &File) -> io::Result<Identity> {
    use crate::win::ffi::RawHandle;
    use std::os::windows::io::AsRawHandle as _;
    /// `FILE_INFO_BY_HANDLE_CLASS::FileIdInfo`.
    const FILE_ID_INFO_CLASS: i32 = 18;
    /// `FILE_ID_INFO`: the volume's 64-bit serial number, then the file's 128-bit id.
    #[repr(C)]
    struct FileIdInfo {
        volume_serial_number: u64,
        file_id: [u8; 16],
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        /// `BY_HANDLE_FILE_INFORMATION`, thirteen `DWORD`s, into `lpFileInformation`; declared
        /// as `crate::file_watch`'s Windows arm declares it, so the two agree.
        fn GetFileInformationByHandle(hFile: RawHandle, lpFileInformation: *mut u32) -> i32;
        fn GetFileInformationByHandleEx(
            hFile: RawHandle,
            FileInformationClass: i32,
            lpFileInformation: *mut core::ffi::c_void,
            dwBufferSize: u32,
        ) -> i32;
    }
    let handle = file.as_raw_handle() as RawHandle;
    let mut facts = [0u32; 13];
    // SAFETY: `file` keeps its handle open across the call, and `facts` is the thirteen
    // writable `DWORD`s of a `BY_HANDLE_FILE_INFORMATION`, all the call writes.
    if unsafe { GetFileInformationByHandle(handle, facts.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut id = FileIdInfo {
        volume_serial_number: 0,
        file_id: [0; 16],
    };
    // SAFETY: `file` keeps its handle open across the call, and `id` is a writable
    // `FILE_ID_INFO` of the size passed that outlives the call.
    let answered = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FILE_ID_INFO_CLASS,
            (&raw mut id).cast(),
            size_of::<FileIdInfo>() as u32,
        )
    } != 0
        && id.file_id != [0; 16];
    // dwVolumeSerialNumber, nFileSizeHigh and Low, nFileIndexHigh and Low.
    let (dev, ino) = if answered {
        (id.volume_serial_number, u128::from_le_bytes(id.file_id))
    } else {
        (
            u64::from(facts[7]),
            u128::from((u64::from(facts[11]) << 32) | u64::from(facts[12])),
        )
    };
    Ok(Identity {
        dev,
        ino,
        len: (u64::from(facts[8]) << 32) | u64::from(facts[9]),
    })
}

/// Open the log at `path` under its [`lock`], creating it as [`ensure`] does.
///
/// # Errors
/// [`ensure`]'s.
pub fn hold(path: &Path) -> io::Result<Held> {
    let lock = lock(path)?;
    let file = open_append(path)?;
    Ok(Held { _lock: lock, file })
}

impl Held {
    /// The held file's [`Identity`] now.
    ///
    /// # Errors
    /// [`identity_of`]'s.
    pub fn identity(&self) -> io::Result<Identity> {
        identity_of(&self.file)
    }

    /// At most `max` bytes from `offset` on.
    ///
    /// # Errors
    /// A seek or read error.
    pub fn read_from(&mut self, offset: u64, max: u64) -> io::Result<Vec<u8>> {
        self.file.seek(SeekFrom::Start(offset))?;
        let mut out = Vec::new();
        (&self.file).take(max).read_to_end(&mut out)?;
        Ok(out)
    }

    /// Append `lines`, each with its newline, in one write after the delimiter an interrupted
    /// final row lacks ([`append`]); the file's length after them.
    ///
    /// # Errors
    /// A newline in any line (nothing is written), or any read, seek, write or flush error.
    pub fn append(&mut self, lines: &[&str]) -> io::Result<u64> {
        if lines.iter().any(|line| line.contains('\n')) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let len = self.file.metadata()?.len();
        let mut bytes =
            Vec::with_capacity(lines.iter().map(|line| line.len() + 1).sum::<usize>() + 1);
        if len > 0 {
            self.file.seek(SeekFrom::Start(len - 1))?;
            let mut last = [0];
            self.file.read_exact(&mut last)?;
            if &last != b"\n" {
                bytes.push(b'\n');
            }
        }
        for line in lines {
            bytes.extend_from_slice(line.as_bytes());
            bytes.push(b'\n');
        }
        self.file.write_all(&bytes)?;
        self.file.flush()?;
        Ok(len + bytes.len() as u64)
    }

    /// The held file, for a flush ([`aterm_durable::ordered`]).
    #[must_use]
    pub const fn file(&self) -> &File {
        &self.file
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    let regular = metadata.is_file();
    #[cfg(windows)]
    let regular = {
        use std::os::windows::fs::MetadataExt as _;
        regular && metadata.file_attributes() & 0x0400 == 0 // FILE_ATTRIBUTE_REPARSE_POINT
    };
    if !regular {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok(file)
}

#[cfg(test)]
#[path = "line_log_tests.rs"]
mod tests;
