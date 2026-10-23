// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows implementation of `aterm-dirfd`: NT handle-relative opens ([`openat`], [`pin`]),
//! metadata ([`fstat`], [`file_id`], [`FileId`], [`volume_guid`]), directory iteration ([`Dir`],
//! [`DirEntry`]), ownership ([`owner`], [`Sid`]), and identity-checked [`unlinkat`].
//!
//! * [`OwnedFd`], [`BorrowedFd`], and [`AsFd`] wrap `std` handle types; single-component [`Arg`]
//!   names open with `FILE_OPEN_REPARSE_POINT | OBJ_DONT_REPARSE` under [`OFlags::NOFOLLOW`].
//! * [`ExposedPlaceholders`] ([`expose_placeholders`]) exposes Cloud Files placeholders so foreign
//!   reparse points and mount points fail closed ([`Errno::FOREIGN_REPARSE`],
//!   [`Errno::MOUNT_POINT`], [`Errno::LOOP`]).
//! * [`Errno`] maps Win32 and NT statuses, reserving [`Errno::NOENT`] for `ERROR_FILE_NOT_FOUND`.

mod ffi;

use std::ffi::{OsStr, OsString, c_void};
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::windows::io::FromRawHandle as _;
use std::path::{Component, Path, Prefix};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime};

pub use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, OwnedHandle};
/// The unix half's names for `std`'s handle types, so a call site reads the same.
pub use std::os::windows::io::{
    AsHandle as AsFd, BorrowedHandle as BorrowedFd, OwnedHandle as OwnedFd,
};

/// A Win32 error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Errno(pub i32);

/// Bit 29 of a Win32 error code: the customer bit, set by no system code, so
const CUSTOMER: i32 = 0x2000_0000;

impl Errno {
    /// `ERROR_FILE_NOT_FOUND`.
    pub const NOENT: Self = Self(ffi::ERROR_FILE_NOT_FOUND);
    /// `ERROR_PATH_NOT_FOUND`.
    pub const PATH_NOT_FOUND: Self = Self(ffi::ERROR_PATH_NOT_FOUND);
    /// `ERROR_INVALID_NAME`.
    pub const INVAL: Self = Self(ffi::ERROR_INVALID_NAME);
    /// `ERROR_ACCESS_DENIED`.
    pub const ACCESS_DENIED: Self = Self(ffi::ERROR_ACCESS_DENIED);
    /// `ERROR_SHARING_VIOLATION`.
    pub const SHARING_VIOLATION: Self = Self(ffi::ERROR_SHARING_VIOLATION);
    /// `ERROR_DIR_NOT_EMPTY`.
    pub const NOTEMPTY: Self = Self(ffi::ERROR_DIR_NOT_EMPTY);
    /// `ERROR_DIRECTORY`.
    pub const NOTDIR: Self = Self(ffi::ERROR_DIRECTORY);
    /// `ERROR_DIRECTORY_NOT_SUPPORTED`.
    pub const ISDIR: Self = Self(ffi::ERROR_DIRECTORY_NOT_SUPPORTED);
    /// `ERROR_DELETE_PENDING`.
    pub const DELETE_PENDING: Self = Self(ffi::ERROR_DELETE_PENDING);
    /// `ERROR_REPARSE_POINT_ENCOUNTERED`.
    pub const LOOP: Self = Self(ffi::ERROR_REPARSE_POINT_ENCOUNTERED);
    /// This crate's own: the name is a reparse point that is not a link.
    pub const FOREIGN_REPARSE: Self = Self(CUSTOMER | 1);
    /// This crate's own: the name no longer carries the identity the caller expected ([`unlinkat`])
    pub const CHANGED: Self = Self(CUSTOMER | 2);
    /// This crate's own: [`pin`] was handed a path that does not start at a local volume's own root
    pub const NOT_VOLUME_ROOT: Self = Self(CUSTOMER | 3);
    /// This crate's own: the name is a folder a volume is mounted.
    pub const MOUNT_POINT: Self = Self(CUSTOMER | 4);
    /// This crate's own: this thread's cloud placeholders could not be exposed.
    pub const PLACEHOLDERS_HIDDEN: Self = Self(CUSTOMER | 5);

    /// The raw code.
    #[must_use]
    pub const fn raw_os_error(self) -> i32 {
        self.0
    }

    /// Return `true` when this is [`Self::NOENT`].
    #[must_use]
    pub const fn is_noent(self) -> bool {
        self.0 == ffi::ERROR_FILE_NOT_FOUND
    }

    /// Return `true` when this is [`Self::INVAL`].
    #[must_use]
    pub const fn is_inval(self) -> bool {
        self.0 == ffi::ERROR_INVALID_NAME
    }

    /// This thread's `GetLastError`, read immediately after a failed call.
    fn last() -> Self {
        Self(
            std::io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(ffi::ERROR_INVALID_FUNCTION),
        )
    }

    /// A failed NTSTATUS as a Win32 code (module docs: the two kept apart).
    fn from_status(status: ffi::NTSTATUS) -> Self {
        match status as u32 {
            ffi::STATUS_DELETE_PENDING => Self::DELETE_PENDING,
            ffi::STATUS_FILE_IS_A_DIRECTORY => Self::ISDIR,
            // SAFETY: A pure table lookup on a plain integer.
            _ => Self(unsafe { ffi::RtlNtStatusToDosError(status) } as i32),
        }
    }

    /// This crate's own text for its own codes.
    const fn own_text(self) -> Option<&'static str> {
        match self {
            Self::FOREIGN_REPARSE => Some(
                "a reparse point that is not a link (a cloud placeholder, a deduplicated file, \
                 an app alias): never entered or removed",
            ),
            Self::CHANGED => Some(
                "the name now names another file than the one expected (renamed into its place)",
            ),
            Self::NOT_VOLUME_ROOT => Some(
                "the path does not start at a local volume's own root (a network path or drive, \
                 or a subst letter): never walked",
            ),
            Self::MOUNT_POINT => Some(
                "a folder a volume is mounted on: never removed, which would unmount the volume",
            ),
            Self::PLACEHOLDERS_HIDDEN => Some(
                "this thread's cloud placeholders could not be exposed, so one would read as a \
                 plain file: nothing was opened or removed",
            ),
            _ => None,
        }
    }
}

impl std::fmt::Display for Errno {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.own_text() {
            Some(text) => f.write_str(text),
            None => std::io::Error::from_raw_os_error(self.0).fmt(f),
        }
    }
}

impl std::error::Error for Errno {}

/// A system code becomes `io::Error::from_raw_os_error`, so its kind is
impl From<Errno> for std::io::Error {
    fn from(e: Errno) -> Self {
        if e.own_text().is_some() {
            return Self::other(e);
        }
        let io = Self::from_raw_os_error(e.0);
        if io.kind() == std::io::ErrorKind::NotFound && !e.is_noent() {
            Self::other(e)
        } else {
            io
        }
    }
}

/// This crate's result type.
pub type Result<T> = std::result::Result<T, Errno>;

/// A name handed to a handle-relative call.
pub trait Arg {
    /// Encode the name as UTF-16 (empty for `.`) and run `f`.
    /// # Errors
    /// [`Errno::INVAL`] for a name refused above.
    fn with_name<T>(self, f: impl FnOnce(&[u16]) -> Result<T>) -> Result<T>;
}

impl<A: AsRef<OsStr>> Arg for A {
    fn with_name<R>(self, f: impl FnOnce(&[u16]) -> Result<R>) -> Result<R> {
        let name = self.as_ref();
        if name == "." {
            return f(&[]);
        }
        let wide: Vec<u16> = name.encode_wide().collect();
        let refused = |c: &u16| [0, u16::from(b'\\'), u16::from(b'/'), u16::from(b':')].contains(c);
        if wide.is_empty()
            || name == ".."
            || wide.iter().any(refused)
            || wide.len() > usize::from(u16::MAX) / 2
        {
            return Err(Errno::INVAL);
        }
        f(&wide)
    }
}

flags! {
    /// How [`openat`] opens: the unix flag names, read as Windows rights and options.
    OFlags(u32) {
        /// Read: a file's data and attributes (`FILE_GENERIC_READ`).
        RDONLY = 0;
        /// Write a file (`FILE_GENERIC_WRITE`).
        WRONLY = 1;
        /// Read and write a file.
        RDWR = 2;
        /// Create the file when absent (`FILE_OPEN_IF`).
        CREATE = 0x40;
        /// With [`Self::CREATE`], fail if the name exists (`FILE_CREATE`).
        EXCL = 0x80;
        /// Accepted for the unix call shape.
        NONBLOCK = 0x800;
        /// The name must be a directory (`FILE_DIRECTORY_FILE`), else [`Errno::NOTDIR`].
        DIRECTORY = 0x1_0000;
        /// Refuse a reparse point at the name.
        NOFOLLOW = 0x2_0000;
        /// Accepted for the unix call shape.
        CLOEXEC = 0x8_0000;
        /// Open for the file's facts.
        PATH = 0x20_0000;
    }
}

impl OFlags {
    /// Whether every bit of `other` is set here.
    const fn has(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// The access-mode field (`RDONLY`, `WRONLY` or `RDWR`).
    const fn access(self) -> u32 {
        self.0 & 3
    }
}

flags! {
    /// [`unlinkat`]'s flags.
    AtFlags(u32) {
        /// Remove a directory (it must be empty), as `rmdir(2)`.
        REMOVEDIR = 0x200;
        /// Accepted for the unix call shape.
        SYMLINK_NOFOLLOW = 0x100;
    }
}

impl AtFlags {
    /// Whether every bit of `other` is set here.
    const fn has(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

flags! {
    /// File mode bits, accepted for the unix call shape and ignored.
    Mode(u32) {
        /// 0o700.
        RWXU = 0o700;
        /// 0o400.
        RUSR = 0o400;
        /// 0o200.
        WUSR = 0o200;
        /// 0o100.
        XUSR = 0o100;
        /// 0o040.
        RGRP = 0o040;
        /// 0o020.
        WGRP = 0o020;
        /// 0o004.
        ROTH = 0o004;
        /// 0o002.
        WOTH = 0o002;
    }
}

/// This thread switched to seeing cloud placeholders as what they are (`PHCM_EXPOSE_PLACEHOLDERS`).
#[must_use = "placeholders are exposed only while the guard lives"]
#[derive(Debug)]
pub struct ExposedPlaceholders {
    /// The mode to put back.
    previous: Option<i8>,
    _thread: std::marker::PhantomData<*const ()>,
}

/// Switch this thread to seeing cloud placeholders as reparse points until the returned guard.
/// # Errors
/// [`Errno::PLACEHOLDERS_HIDDEN`] when the system refused the mode (it
/// answers so only for an invalid mode or a thread with no TEB): the caller
/// then judges nothing a placeholder might be.
pub fn expose_placeholders() -> Result<ExposedPlaceholders> {
    #[cfg(any(test, debug_assertions))]
    if DISGUISED.with(std::cell::Cell::get) {
        return Ok(ExposedPlaceholders {
            previous: None,
            _thread: std::marker::PhantomData,
        });
    }
    // SAFETY: Writes one field of this thread's own TEB; no pointer.
    let previous =
        unsafe { ffi::RtlSetThreadPlaceholderCompatibilityMode(ffi::PHCM_EXPOSE_PLACEHOLDERS) };
    if previous < 0 {
        return Err(Errno::PLACEHOLDERS_HIDDEN);
    }
    Ok(ExposedPlaceholders {
        previous: Some(previous),
        _thread: std::marker::PhantomData,
    })
}

impl Drop for ExposedPlaceholders {
    fn drop(&mut self) {
        if let Some(previous) = self.previous {
            // SAFETY: As in `expose_placeholders`: this thread's own TEB, the mode it answered.
            unsafe { ffi::RtlSetThreadPlaceholderCompatibilityMode(previous) };
        }
    }
}

/// A file's identity: `FILE_ID_INFO`'s volume serial number and file id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileId {
    /// `FILE_ID_INFO.VolumeSerialNumber`.
    pub dev: u64,
    /// `FILE_ID_INFO.FileId`, little-endian.
    pub ino: u128,
}

/// What a handle says about its file, read through the handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    /// [`FileId::dev`].
    pub st_dev: u64,
    /// [`FileId::ino`].
    pub st_ino: u128,
    /// Hard-link count (`FILE_STANDARD_INFO.NumberOfLinks`).
    pub st_nlink: u64,
    /// `FILE_ATTRIBUTE_*` bits.
    pub attributes: u32,
    /// The reparse tag, or 0 when this is not a reparse point.
    pub reparse_tag: u32,
    /// Logical size (`EndOfFile`).
    pub size: u64,
    /// Space allocated on the volume (`AllocationSize`).
    pub allocation: u64,
    /// Last write, in filetime ticks (100 ns since 1601-01-01 UTC).
    pub last_write: i64,
    /// Last change of data or metadata (unix's `ctime`), in filetime ticks.
    pub change_time: i64,
    /// A legacy delete is pending.
    pub delete_pending: bool,
}

/// FILETIME ticks from 1601-01-01 to the unix epoch.
const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;

impl Stat {
    /// The identity.
    #[must_use]
    pub const fn id(&self) -> FileId {
        FileId {
            dev: self.st_dev,
            ino: self.st_ino,
        }
    }

    /// What kind of entry.
    #[must_use]
    pub const fn file_type(&self) -> FileType {
        FileType::from_attributes(self.attributes, self.reparse_tag)
    }

    /// [`Self::last_write`] as a `SystemTime`, `None` before the unix epoch.
    #[must_use]
    pub fn modified(&self) -> Option<SystemTime> {
        let ticks = u64::try_from(self.last_write.checked_sub(FILETIME_UNIX_EPOCH)?).ok()?;
        SystemTime::UNIX_EPOCH.checked_add(Duration::from_nanos(ticks.checked_mul(100)?))
    }
}

/// An entry's kind, from its attributes and reparse tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileType {
    attributes: u32,
    reparse_tag: u32,
}

impl FileType {
    /// The kind `attributes` and `reparse_tag` (0 when not a reparse point) describe.
    #[must_use]
    pub const fn from_attributes(attributes: u32, reparse_tag: u32) -> Self {
        Self {
            attributes,
            reparse_tag,
        }
    }

    /// A reparse point of any kind.
    #[must_use]
    pub const fn is_reparse(self) -> bool {
        self.attributes & ffi::FILE_ATTRIBUTE_REPARSE_POINT != 0
    }

    /// A link: a name-surrogate reparse point (module docs), removed as itself.
    #[must_use]
    pub const fn is_link(self) -> bool {
        self.is_reparse() && self.reparse_tag & ffi::REPARSE_TAG_NAME_SURROGATE != 0
    }

    /// A reparse point that is not a link.
    #[must_use]
    pub const fn is_foreign_reparse(self) -> bool {
        self.is_reparse() && !self.is_link()
    }

    /// A real directory (not a reparse point).
    #[must_use]
    pub const fn is_dir(self) -> bool {
        self.attributes & ffi::FILE_ATTRIBUTE_DIRECTORY != 0 && !self.is_reparse()
    }

    /// A plain file (neither a directory nor a reparse point).
    #[must_use]
    pub const fn is_file(self) -> bool {
        self.attributes & ffi::FILE_ATTRIBUTE_DIRECTORY == 0 && !self.is_reparse()
    }

    /// `FILE_ATTRIBUTE_READONLY`, which [`unlinkat`] deletes.
    #[must_use]
    pub const fn is_readonly(self) -> bool {
        self.attributes & ffi::FILE_ATTRIBUTE_READONLY != 0
    }
}

/// A by-handle information struct: its class, every field a plain integer.
/// # Safety
/// `CLASS` must be the `FILE_INFO_BY_HANDLE_CLASS` whose output layout is
/// exactly `Self`, and every bit pattern must be a valid `Self`.
unsafe trait Info: Sized {
    const CLASS: i32;
}

// SAFETY: Each is the SDK layout of its class (pinned in `ffi`), integers.
unsafe impl Info for ffi::FILE_BASIC_INFO {
    const CLASS: i32 = ffi::FileBasicInfo;
}
// SAFETY: As above.
unsafe impl Info for ffi::FILE_STANDARD_INFO {
    const CLASS: i32 = ffi::FileStandardInfo;
}
// SAFETY: As above.
unsafe impl Info for ffi::FILE_ATTRIBUTE_TAG_INFO {
    const CLASS: i32 = ffi::FileAttributeTagInfo;
}
// SAFETY: As above.
unsafe impl Info for ffi::FILE_ID_INFO {
    const CLASS: i32 = ffi::FileIdInfo;
}

/// Read `T`'s class through `fd` into `out`.
fn query<T: Info>(fd: BorrowedHandle<'_>, mut out: T) -> Result<T> {
    let len = u32::try_from(size_of::<T>()).map_err(|_| Errno(ffi::ERROR_INVALID_PARAMETER))?;
    // SAFETY: `fd` is live for the call.
    let ok = unsafe {
        ffi::GetFileInformationByHandleEx(
            fd.as_raw_handle(),
            T::CLASS,
            (&raw mut out).cast::<c_void>(),
            len,
        )
    };
    if ok == 0 { Err(Errno::last()) } else { Ok(out) }
}

/// The kind of what `fd` is open on.
fn kind_of(fd: BorrowedHandle<'_>) -> Result<FileType> {
    let tag = query(
        fd,
        ffi::FILE_ATTRIBUTE_TAG_INFO {
            FileAttributes: 0,
            ReparseTag: 0,
        },
    )?;
    Ok(FileType::from_attributes(
        tag.FileAttributes,
        tag.ReparseTag,
    ))
}

/// The identity of what `fd` is open.
/// # Errors
/// The query's code — `ERROR_INVALID_PARAMETER` on a file system with no
/// `FileIdInfo` (FAT, exFAT), which has no identity to give.
pub fn file_id<Fd: AsHandle>(fd: Fd) -> Result<FileId> {
    let _exposed = expose_placeholders()?;
    let info = query(
        fd.as_handle(),
        ffi::FILE_ID_INFO {
            VolumeSerialNumber: 0,
            FileId: [0; 16],
        },
    )?;
    Ok(FileId {
        dev: info.VolumeSerialNumber,
        ino: u128::from_le_bytes(info.FileId),
    })
}

/// Identity, kind, sizes, link count and times of an already-open handle.
/// # Errors
/// The first failed query's code; see [`file_id`] for a file system with no
/// identity.
pub fn fstat<Fd: AsHandle>(fd: Fd) -> Result<Stat> {
    let _exposed = expose_placeholders()?;
    let fd = fd.as_handle();
    let id = file_id(fd)?;
    let basic = query(
        fd,
        ffi::FILE_BASIC_INFO {
            CreationTime: 0,
            LastAccessTime: 0,
            LastWriteTime: 0,
            ChangeTime: 0,
            FileAttributes: 0,
        },
    )?;
    let standard = query(
        fd,
        ffi::FILE_STANDARD_INFO {
            AllocationSize: 0,
            EndOfFile: 0,
            NumberOfLinks: 0,
            DeletePending: 0,
            Directory: 0,
        },
    )?;
    let tag = query(
        fd,
        ffi::FILE_ATTRIBUTE_TAG_INFO {
            FileAttributes: 0,
            ReparseTag: 0,
        },
    )?;
    let reparse = tag.FileAttributes & ffi::FILE_ATTRIBUTE_REPARSE_POINT != 0;
    Ok(Stat {
        st_dev: id.dev,
        st_ino: id.ino,
        st_nlink: u64::from(standard.NumberOfLinks),
        attributes: tag.FileAttributes,
        reparse_tag: if reparse { tag.ReparseTag } else { 0 },
        size: u64::try_from(standard.EndOfFile).unwrap_or(0),
        allocation: u64::try_from(standard.AllocationSize).unwrap_or(0),
        last_write: basic.LastWriteTime,
        change_time: basic.ChangeTime,
        delete_pending: standard.DeletePending != 0,
    })
}

/// The GUID of the volume the file `fd` is open on, as the mount manager names the volume.
/// # Errors
/// The query's code (a volume the mount manager does not name answers one);
/// `ERROR_INVALID_DATA` for an answer that names no volume GUID.
pub fn volume_guid<Fd: AsHandle>(fd: Fd) -> Result<String> {
    let raw = fd.as_handle().as_raw_handle();
    let mut buf = vec![0u16; 512];
    let path = loop {
        let len = u32::try_from(buf.len()).map_err(|_| Errno(ffi::ERROR_INVALID_DATA))?;
        // SAFETY: `fd` is live for the call; `buf` is writable for `len` units and outlives it.
        let got = unsafe {
            ffi::GetFinalPathNameByHandleW(raw, buf.as_mut_ptr(), len, ffi::VOLUME_NAME_GUID)
        };
        match got as usize {
            0 => return Err(Errno::last()),
            wanted if wanted > buf.len() => {
                if buf.len() > 512 {
                    return Err(Errno(ffi::ERROR_INVALID_DATA));
                }
                buf = vec![0; wanted];
            }
            stored => break buf.get(..stored).unwrap_or_default(),
        }
    };
    guid_of_volume_path(path).ok_or(Errno(ffi::ERROR_INVALID_DATA))
}

/// The lowercase `{…}` GUID a `\\?\Volume{…}\…` path (UTF-16) starts with,
fn guid_of_volume_path(path: &[u16]) -> Option<String> {
    const HEAD: &str = r"\\?\Volume{";
    let text = String::from_utf16(path).ok()?;
    text.get(..HEAD.len())
        .filter(|head| head.eq_ignore_ascii_case(HEAD))?;
    let guid = text.get(HEAD.len() - 1..HEAD.len() + 37)?;
    let shaped = guid.ends_with('}')
        && guid[1..37].char_indices().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        });
    shaped.then(|| guid.to_ascii_lowercase())
}

/// A security identifier (`SID`) in its binary form.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Sid(Vec<u8>);

impl Sid {
    /// `BUILTIN\Administrators` (`S-1-5-32-544`).
    #[must_use]
    pub fn administrators() -> Self {
        // Revision 1, two sub-authorities, SECURITY_NT_AUTHORITY (5),
        Self(vec![1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 0x20, 0x02, 0, 0])
    }

    /// `NT AUTHORITY\SYSTEM` (`S-1-5-18`).
    #[must_use]
    pub fn local_system() -> Self {
        // Revision 1, one sub-authority, SECURITY_NT_AUTHORITY (5),
        Self(vec![1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0])
    }

    /// The user of this process's token (`GetTokenInformation(TokenUser)`).
    /// # Errors
    /// The token could not be opened or read.
    pub fn current_user() -> Result<Self> {
        let mut raw: ffi::HANDLE = std::ptr::null_mut();
        // SAFETY: The process pseudo-handle needs no closing.
        if unsafe {
            ffi::OpenProcessToken(ffi::GetCurrentProcess(), ffi::TOKEN_QUERY, &raw mut raw)
        } == 0
        {
            return Err(Errno::last());
        }
        // SAFETY: A successful OpenProcessToken returned a fresh handle this function now owns.
        let token = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut buf = [0u64; 16];
        let mut used = 0u32;
        // SAFETY: `token` is live.
        let ok = unsafe {
            ffi::GetTokenInformation(
                token.as_raw_handle(),
                ffi::TokenUser,
                buf.as_mut_ptr().cast::<c_void>(),
                size_of_val(&buf) as u32,
                &raw mut used,
            )
        };
        if ok == 0 {
            return Err(Errno::last());
        }
        // SAFETY: On success the buffer starts with a TOKEN_USER (aligned, as `buf` is u64s).
        let sid = unsafe { (*buf.as_ptr().cast::<ffi::TOKEN_USER>()).User.Sid };
        // SAFETY: `sid` points at the token's SID, inside `buf`.
        unsafe { Self::copied(sid) }
    }

    /// The SID at `sid`, copied out.
    /// # Safety
    /// `sid` must be null or point at memory readable for a SID's length.
    unsafe fn copied(sid: *const c_void) -> Result<Self> {
        // SAFETY: The caller's contract; IsValidSid reads the header only after its own checks.
        if sid.is_null() || unsafe { ffi::IsValidSid(sid) } == 0 {
            return Err(Errno(ffi::ERROR_INVALID_DATA));
        }
        // SAFETY: A valid SID's length is what GetLengthSid answers.
        let len = unsafe { ffi::GetLengthSid(sid) } as usize;
        // SAFETY: As above: `len` readable bytes at `sid`, copied.
        Ok(Self(
            unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), len) }.to_vec(),
        ))
    }

    /// The binary SID.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Display for Sid {
    /// `S-<revision>-<authority>-<sub-authority>…`, the authority in decimal
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (Some(&revision), Some(authority)) = (self.0.first(), self.0.get(2..8)) else {
            return f.write_str("S-?");
        };
        let authority = authority
            .iter()
            .fold(0u64, |acc, &byte| (acc << 8) | u64::from(byte));
        if authority >> 32 == 0 {
            write!(f, "S-{revision}-{authority}")?;
        } else {
            write!(f, "S-{revision}-0x{authority:012X}")?;
        }
        for sub in self.0.get(8..).unwrap_or_default().chunks_exact(4) {
            write!(
                f,
                "-{}",
                u32::from_le_bytes([sub[0], sub[1], sub[2], sub[3]])
            )?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for Sid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sid({self})")
    }
}

/// The owner of the file or directory `fd` is open.
/// # Errors
/// The query's code (`ERROR_ACCESS_DENIED` without `READ_CONTROL`), or
/// `ERROR_INVALID_DATA` for a descriptor with no owner.
pub fn owner<Fd: AsHandle>(fd: Fd) -> Result<Sid> {
    let _exposed = expose_placeholders()?;
    let mut owner: *mut c_void = std::ptr::null_mut();
    let mut descriptor: *mut c_void = std::ptr::null_mut();
    // SAFETY: `fd` is live for the call.
    let code = unsafe {
        ffi::GetSecurityInfo(
            fd.as_handle().as_raw_handle(),
            ffi::SE_FILE_OBJECT,
            ffi::OWNER_SECURITY_INFORMATION,
            &raw mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    if code != 0 {
        return Err(Errno(code as i32));
    }
    // SAFETY: On success `owner` is null or points into `descriptor`, which stays allocated.
    let sid = unsafe { Sid::copied(owner) };
    // SAFETY: `descriptor` was allocated by GetSecurityInfo for this caller and is freed.
    unsafe { ffi::LocalFree(descriptor) };
    sid
}

/// Whether `OBJ_DONT_REPARSE` is still passed: cleared once a kernel refuses
static DONT_REPARSE: AtomicU32 = AtomicU32::new(ffi::OBJ_DONT_REPARSE);

/// `NtCreateFile` of the one-component `name` (empty: `dir` itself) relative
fn nt_open(
    dir: BorrowedHandle<'_>,
    name: &[u16],
    access: u32,
    disposition: u32,
    options: u32,
) -> Result<OwnedHandle> {
    let bytes = u16::try_from(name.len() * 2).map_err(|_| Errno::INVAL)?;
    let attempt = |extra: u32| -> std::result::Result<OwnedHandle, ffi::NTSTATUS> {
        let mut object_name = ffi::UNICODE_STRING {
            Length: bytes,
            MaximumLength: bytes,
            Buffer: name.as_ptr().cast_mut(),
        };
        let attributes = ffi::OBJECT_ATTRIBUTES {
            Length: size_of::<ffi::OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: dir.as_raw_handle(),
            ObjectName: &raw mut object_name,
            Attributes: ffi::OBJ_CASE_INSENSITIVE | extra,
            SecurityDescriptor: std::ptr::null_mut(),
            SecurityQualityOfService: std::ptr::null_mut(),
        };
        let mut io = ffi::IO_STATUS_BLOCK {
            Status: 0,
            Information: 0,
        };
        let mut raw: ffi::HANDLE = std::ptr::null_mut();
        // SAFETY: Every pointer is to a live local.
        let status = unsafe {
            ffi::NtCreateFile(
                &raw mut raw,
                access | ffi::SYNCHRONIZE,
                &raw const attributes,
                &raw mut io,
                std::ptr::null(),
                0,
                ffi::FILE_SHARE_ALL,
                disposition,
                options | ffi::FILE_OPEN_REPARSE_POINT | ffi::FILE_SYNCHRONOUS_IO_NONALERT,
                std::ptr::null(),
                0,
            )
        };
        if status < 0 {
            return Err(status);
        }
        // SAFETY: A successful NtCreateFile returned a fresh handle this function now owns.
        Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
    };
    let extra = DONT_REPARSE.load(Ordering::Relaxed);
    match attempt(extra) {
        Ok(handle) => Ok(handle),
        Err(status) if status as u32 == ffi::STATUS_INVALID_PARAMETER && extra != 0 => {
            let retried = attempt(0);
            if retried
                .as_ref()
                .err()
                .is_none_or(|&s| s as u32 != ffi::STATUS_INVALID_PARAMETER)
            {
                DONT_REPARSE.store(0, Ordering::Relaxed);
            }
            retried.map_err(Errno::from_status)
        }
        Err(status) => Err(Errno::from_status(status)),
    }
}

/// Refuse a reparse point `kind` describes: a link as [`Errno::LOOP`], any
const fn refuse_reparse(kind: FileType) -> Result<()> {
    if kind.is_link() {
        Err(Errno::LOOP)
    } else if kind.is_reparse() {
        Err(Errno::FOREIGN_REPARSE)
    } else {
        Ok(())
    }
}

/// `openat(2)`'s twin: open `name` relative to the directory `dirfd`, never following anything.
/// # Errors
/// The open's code; [`Errno::LOOP`]/[`Errno::FOREIGN_REPARSE`] for a reparse
/// point under [`OFlags::NOFOLLOW`]; [`Errno::ISDIR`] for a directory opened
/// to write; [`Errno::INVAL`] for a refused name or flag combination
/// ([`OFlags::CREATE`] with [`OFlags::DIRECTORY`] or [`OFlags::PATH`], a write
/// mode with [`OFlags::PATH`]).
pub fn openat<Fd: AsHandle, P: Arg>(
    dirfd: Fd,
    name: P,
    flags: OFlags,
    mode: Mode,
) -> Result<OwnedHandle> {
    let _ = mode;
    let directory = flags.has(OFlags::DIRECTORY);
    let write = flags.access() != OFlags::RDONLY.0;
    let facts = flags.has(OFlags::PATH);
    if (directory || facts) && flags.has(OFlags::CREATE) {
        return Err(Errno::INVAL);
    }
    if facts && write {
        return Err(Errno::INVAL);
    }
    if directory && write {
        return Err(Errno::ISDIR);
    }
    let access = ffi::FILE_READ_ATTRIBUTES
        | if facts {
            ffi::READ_CONTROL
        } else if directory {
            ffi::FILE_LIST_DIRECTORY | ffi::FILE_TRAVERSE
        } else {
            match flags.access() {
                1 => ffi::FILE_GENERIC_WRITE,
                2 => ffi::FILE_GENERIC_READ | ffi::FILE_GENERIC_WRITE,
                _ => ffi::FILE_GENERIC_READ,
            }
        };
    let disposition = match (flags.has(OFlags::CREATE), flags.has(OFlags::EXCL)) {
        (true, true) => ffi::FILE_CREATE,
        (true, false) => ffi::FILE_OPEN_IF,
        (false, _) => ffi::FILE_OPEN,
    };
    let options = if directory {
        ffi::FILE_DIRECTORY_FILE
    } else {
        0
    };
    let _exposed = expose_placeholders()?;
    name.with_name(|name| {
        let handle = nt_open(dirfd.as_handle(), name, access, disposition, options)?;
        let kind = kind_of(handle.as_handle())?;
        if flags.has(OFlags::NOFOLLOW) {
            refuse_reparse(kind)?;
        }
        if write && kind.is_dir() {
            return Err(Errno::ISDIR);
        }
        Ok(handle)
    })
}

/// Open the directory at the absolute `path` from its local volume's root, component by component.
/// # Errors
/// [`Errno::NOT_VOLUME_ROOT`] for a UNC path, a network drive or a `subst`
/// letter; [`Errno::PATH_NOT_FOUND`] for a letter or volume with no device
/// (never [`Errno::NOENT`]: a drive that is not there says nothing about a
/// directory on it); [`Errno::INVAL`] for a path of any other shape
/// (relative or drive-relative, a `..` component, a verbatim path's `.`,
/// `\\.\`); [`Errno::LOOP`]/[`Errno::FOREIGN_REPARSE`] for a reparse point;
/// [`Errno::NOTDIR`] for a file; or the open's code.
pub fn pin(path: &Path) -> Result<OwnedHandle> {
    let _exposed = expose_placeholders()?;
    let mut parts = path.components();
    let Some(Component::Prefix(prefix)) = parts.next() else {
        return Err(Errno::INVAL);
    };
    let (device, verbatim) = match prefix.kind() {
        Prefix::Disk(letter) => (OsString::from(format!("{}:", char::from(letter))), false),
        Prefix::VerbatimDisk(letter) => (OsString::from(format!("{}:", char::from(letter))), true),
        Prefix::Verbatim(volume)
            if volume
                .as_encoded_bytes()
                .get(..7)
                .is_some_and(|head| head.eq_ignore_ascii_case(b"Volume{")) =>
        {
            (volume.to_owned(), true)
        }
        Prefix::UNC(..) | Prefix::VerbatimUNC(..) => return Err(Errno::NOT_VOLUME_ROOT),
        _ => return Err(Errno::INVAL),
    };
    if parts.next() != Some(Component::RootDir) {
        return Err(Errno::INVAL);
    }
    volume_device(&device)?;
    let mut root = OsString::from(r"\\?\");
    root.push(&device);
    root.push(r"\");
    let mut fd = open_root(&root)?;
    for part in parts {
        let Component::Normal(name) = part else {
            return Err(Errno::INVAL);
        };
        let bytes = name.as_encoded_bytes();
        if !verbatim && (bytes.ends_with(b".") || bytes.ends_with(b" ")) {
            return Err(Errno::INVAL);
        }
        fd = name.with_name(|name| {
            if name.is_empty() {
                return Err(Errno::INVAL);
            }
            let handle = nt_open(
                fd.as_handle(),
                name,
                ffi::FILE_READ_ATTRIBUTES,
                ffi::FILE_OPEN,
                ffi::FILE_DIRECTORY_FILE,
            )?;
            refuse_reparse(kind_of(handle.as_handle())?)?;
            Ok(handle)
        })?;
    }
    Ok(fd)
}

/// Check that the DOS device `name` (`C:`, `Volume{…}`) is a device and not
fn volume_device(name: &OsStr) -> Result<()> {
    let wide: Vec<u16> = name.encode_wide().chain([0]).collect();
    let mut target = vec![0u16; 1024];
    // SAFETY: `wide` is NUL-terminated and `target` writable for the length passed.
    let stored =
        unsafe { ffi::QueryDosDeviceW(wide.as_ptr(), target.as_mut_ptr(), target.len() as u32) };
    if stored == 0 {
        return Err(match Errno::last() {
            Errno::NOENT => Errno::PATH_NOT_FOUND,
            Errno(ffi::ERROR_INSUFFICIENT_BUFFER) => Errno::NOT_VOLUME_ROOT,
            other => other,
        });
    }
    let current = target
        .get(..stored as usize)
        .unwrap_or_default()
        .split(|&unit| unit == 0)
        .next()
        .unwrap_or_default();
    if is_volume_device(current) {
        Ok(())
    } else {
        Err(Errno::NOT_VOLUME_ROOT)
    }
}

/// Whether the object-manager target `target` names a device itself:
fn is_volume_device(target: &[u16]) -> bool {
    let head: Vec<u16> = r"\device\".encode_utf16().collect();
    let lower = |unit: u16| {
        if (u16::from(b'A')..=u16::from(b'Z')).contains(&unit) {
            unit + 32
        } else {
            unit
        }
    };
    target.len() > head.len()
        && target
            .iter()
            .zip(&head)
            .all(|(&unit, &want)| lower(unit) == want)
        && !target[head.len()..].contains(&u16::from(b'\\'))
}

/// Whether `fd` is open on the root directory of its volume: its
fn is_volume_root(fd: BorrowedHandle<'_>) -> Result<bool> {
    let mut buf = [0u32; 1 + 32];
    // SAFETY: `fd` is live.
    let ok = unsafe {
        ffi::GetFileInformationByHandleEx(
            fd.as_raw_handle(),
            ffi::FileNameInfo,
            buf.as_mut_ptr().cast::<c_void>(),
            size_of_val(&buf) as u32,
        )
    };
    if ok == 0 {
        let err = Errno::last();
        return if err.0 == ffi::ERROR_MORE_DATA {
            Ok(false)
        } else {
            Err(err)
        };
    }
    let bytes: Vec<u8> = buf.iter().flat_map(|word| word.to_ne_bytes()).collect();
    let name_at = offset_of!(ffi::FILE_NAME_INFO, FileName);
    let name_len = u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let name: Vec<u16> = bytes
        .get(name_at..name_at + name_len)
        .ok_or(Errno(ffi::ERROR_INVALID_DATA))?
        .chunks_exact(2)
        .map(|pair| u16::from_ne_bytes([pair[0], pair[1]]))
        .collect();
    Ok(name == [u16::from(b'\\')])
}

/// Open the volume root spelled `root` (verbatim, trailing `\`) as itself,
fn open_root(root: &OsStr) -> Result<OwnedHandle> {
    let wide: Vec<u16> = root.encode_wide().chain([0]).collect();
    let mut old = 0;
    // SAFETY: `old` is a writable u32.
    let quiet = unsafe {
        ffi::SetThreadErrorMode(
            ffi::SEM_FAILCRITICALERRORS | ffi::SEM_NOOPENFILEERRORBOX,
            &raw mut old,
        )
    } != 0;
    // SAFETY: `wide` is NUL-terminated and outlives the call; no security attributes or template.
    let raw = unsafe {
        ffi::CreateFileW(
            wide.as_ptr(),
            ffi::FILE_READ_ATTRIBUTES | ffi::SYNCHRONIZE,
            ffi::FILE_SHARE_ALL,
            std::ptr::null(),
            ffi::OPEN_EXISTING,
            ffi::FILE_FLAG_BACKUP_SEMANTICS | ffi::FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    let err = Errno::last();
    if quiet {
        // SAFETY: Restores this thread's previous mode; no out-pointer.
        unsafe { ffi::SetThreadErrorMode(old, std::ptr::null_mut()) };
    }
    if raw == ffi::INVALID_HANDLE_VALUE || raw.is_null() {
        return Err(err);
    }
    // SAFETY: A successful CreateFileW returned a fresh handle this function now owns exclusively.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    if !is_volume_root(handle.as_handle())? {
        return Err(Errno::NOT_VOLUME_ROOT);
    }
    let kind = kind_of(handle.as_handle())?;
    refuse_reparse(kind)?;
    if !kind.is_dir() {
        return Err(Errno::NOTDIR);
    }
    Ok(handle)
}

/// `unlinkat(2)`'s twin, with the identity the name must still carry.
/// # Errors
/// * [`Errno::CHANGED`] — the name now names another file than `expect`;
/// * [`Errno::FOREIGN_REPARSE`] — a reparse point that is not a link: kept;
/// * [`Errno::MOUNT_POINT`] — a folder a volume is mounted on: kept (its
///   target unreadable: that read's code, kept too);
/// * [`Errno::ISDIR`] — a real directory without [`AtFlags::REMOVEDIR`];
/// * [`Errno::NOTDIR`] — a file or a link with [`AtFlags::REMOVEDIR`];
/// * [`Errno::SHARING_VIOLATION`] — held open without delete sharing;
/// * [`Errno::NOTEMPTY`] — a directory that still holds entries;
/// * [`Errno::NOENT`] — the name is gone; anything else as the system said.
pub fn unlinkat<Fd: AsHandle, P: Arg>(
    dirfd: Fd,
    name: P,
    flags: AtFlags,
    expect: FileId,
) -> Result<()> {
    let rmdir = flags.has(AtFlags::REMOVEDIR);
    let _exposed = expose_placeholders()?;
    name.with_name(|name| {
        if name.is_empty() {
            return Err(Errno::INVAL);
        }
        let handle = nt_open(
            dirfd.as_handle(),
            name,
            ffi::DELETE | ffi::FILE_READ_ATTRIBUTES,
            ffi::FILE_OPEN,
            if rmdir { ffi::FILE_DIRECTORY_FILE } else { 0 },
        )?;
        if file_id(&handle)? != expect {
            return Err(Errno::CHANGED);
        }
        let kind = kind_of(handle.as_handle())?;
        if kind.is_foreign_reparse() {
            return Err(Errno::FOREIGN_REPARSE);
        }
        if rmdir && kind.is_link() {
            return Err(Errno::NOTDIR);
        }
        if !rmdir && kind.is_dir() {
            return Err(Errno::ISDIR);
        }
        if mounts_volume(handle.as_handle(), kind)? {
            return Err(Errno::MOUNT_POINT);
        }
        dispose(handle.as_handle())
    })
}

/// Whether the reparse point `fd` is open on (as itself, of kind `kind`) is
/// # Errors
/// `FSCTL_GET_REPARSE_POINT`'s code, or `ERROR_INVALID_DATA` for data that
/// does not hold the name it says it does: the caller keeps what it could
/// not judge.
fn mounts_volume(fd: BorrowedHandle<'_>, kind: FileType) -> Result<bool> {
    if !kind.is_link() || kind.reparse_tag != ffi::IO_REPARSE_TAG_MOUNT_POINT {
        return Ok(false);
    }
    let mut buf = vec![0u32; ffi::MAXIMUM_REPARSE_DATA_BUFFER_SIZE / 4];
    let mut returned = 0u32;
    // SAFETY: `fd` is live.
    let ok = unsafe {
        ffi::DeviceIoControl(
            fd.as_raw_handle(),
            ffi::FSCTL_GET_REPARSE_POINT,
            std::ptr::null(),
            0,
            buf.as_mut_ptr().cast::<c_void>(),
            size_of_val(buf.as_slice()) as u32,
            &raw mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(Errno::last());
    }
    let bytes: Vec<u8> = buf
        .iter()
        .flat_map(|word| word.to_ne_bytes())
        .take(returned as usize)
        .collect();
    let invalid = Errno(ffi::ERROR_INVALID_DATA);
    let field = |at: usize| {
        bytes
            .get(at..at + 2)
            .map(|b| usize::from(u16::from_ne_bytes([b[0], b[1]])))
            .ok_or(invalid)
    };
    let tag = bytes
        .get(..4)
        .map(|b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or(invalid)?;
    if tag != ffi::IO_REPARSE_TAG_MOUNT_POINT {
        return Err(invalid);
    }
    let offset = field(offset_of!(
        ffi::MOUNT_POINT_REPARSE_HEADER,
        SubstituteNameOffset
    ))?;
    let length = field(offset_of!(
        ffi::MOUNT_POINT_REPARSE_HEADER,
        SubstituteNameLength
    ))?;
    let start = size_of::<ffi::MOUNT_POINT_REPARSE_HEADER>() + offset;
    let target: Vec<u16> = bytes
        .get(start..start + length)
        .ok_or(invalid)?
        .chunks_exact(2)
        .map(|pair| u16::from_ne_bytes([pair[0], pair[1]]))
        .collect();
    Ok(is_volume_name(&target))
}

/// Whether the NT name `target` is a volume's GUID name, `\??\Volume{…`
fn is_volume_name(target: &[u16]) -> bool {
    let head: Vec<u16> = r"\??\volume{".encode_utf16().collect();
    target.len() > head.len()
        && target.iter().zip(&head).all(|(&unit, &want)| {
            let lower = if (u16::from(b'A')..=u16::from(b'Z')).contains(&unit) {
                unit + 32
            } else {
                unit
            };
            lower == want
        })
}

/// Delete the file `handle` is open on: the POSIX disposition, or the legacy
fn dispose(handle: BorrowedHandle<'_>) -> Result<()> {
    let set = |class: i32, info: *const c_void, len: usize| -> Result<()> {
        // SAFETY: `handle` is live and `info` points at `len` readable bytes of `class`'s input.
        let ok = unsafe {
            ffi::SetFileInformationByHandle(handle.as_raw_handle(), class, info, len as u32)
        };
        if ok == 0 { Err(Errno::last()) } else { Ok(()) }
    };
    if !legacy() {
        let ex = ffi::FILE_DISPOSITION_INFO_EX {
            Flags: ffi::FILE_DISPOSITION_FLAG_DELETE
                | ffi::FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
                | ffi::FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
        };
        match set(
            ffi::FileDispositionInfoEx,
            (&raw const ex).cast(),
            size_of::<ffi::FILE_DISPOSITION_INFO_EX>(),
        ) {
            Err(e) if unsupported(e) => {}
            done => return done,
        }
    }
    let plain = ffi::FILE_DISPOSITION_INFO { DeleteFile: 1 };
    set(
        ffi::FileDispositionInfo,
        (&raw const plain).cast(),
        size_of::<ffi::FILE_DISPOSITION_INFO>(),
    )
}

/// The codes a file system answers for an information class it lacks.
fn unsupported(e: Errno) -> bool {
    [
        ffi::ERROR_INVALID_PARAMETER,
        ffi::ERROR_INVALID_FUNCTION,
        ffi::ERROR_NOT_SUPPORTED,
        ffi::ERROR_INVALID_LEVEL,
    ]
    .contains(&e.0)
}

/// One directory entry, as the directory index lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    name: OsString,
    ino: u128,
    attributes: u32,
    reparse_tag: u32,
    size: u64,
    allocation: u64,
}

impl DirEntry {
    /// The entry's exact name.
    #[must_use]
    pub fn file_name(&self) -> &OsStr {
        &self.name
    }

    /// The listed file id ([`FileId::ino`]'s reading).
    #[must_use]
    pub const fn ino(&self) -> u128 {
        self.ino
    }

    /// What kind of entry.
    #[must_use]
    pub const fn file_type(&self) -> FileType {
        FileType::from_attributes(self.attributes, self.reparse_tag)
    }

    /// `FILE_ATTRIBUTE_*` bits.
    #[must_use]
    pub const fn attributes(&self) -> u32 {
        self.attributes
    }

    /// The reparse tag, or 0 when this is not a reparse point.
    #[must_use]
    pub const fn reparse_tag(&self) -> u32 {
        self.reparse_tag
    }

    /// Logical size (`EndOfFile`).
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }

    /// Space allocated on the volume (`AllocationSize`).
    #[must_use]
    pub const fn allocation(&self) -> u64 {
        self.allocation
    }
}

/// Which listing class a [`Dir`] reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// `FileIdExtdDirectoryInfo`.
    Extd,
    /// `FileIdBothDirectoryInfo`.
    Both,
}

impl Class {
    const fn info(self, restart: bool) -> i32 {
        match (self, restart) {
            (Self::Extd, true) => ffi::FileIdExtdDirectoryRestartInfo,
            (Self::Extd, false) => ffi::FileIdExtdDirectoryInfo,
            (Self::Both, true) => ffi::FileIdBothDirectoryRestartInfo,
            (Self::Both, false) => ffi::FileIdBothDirectoryInfo,
        }
    }
}

/// Bytes of listing one query reads.
const LISTING: usize = 64 * 1024;

/// A directory being listed, over its own handle.
pub struct Dir {
    /// `None`: the directory was being deleted when opened.
    handle: Option<OwnedHandle>,
    /// The query buffer, as `u64`s so it is 8-byte aligned as the classes require.
    buf: Vec<u64>,
    /// The next entry's byte offset in `buf`, `None` when a query is due.
    at: Option<usize>,
    class: Class,
    started: bool,
    /// Latched at the end, or by the first failure (which is yielded once).
    done: bool,
}

impl Dir {
    /// Open a listing of the directory `fd` is open on, positioned at its start.
    /// # Errors
    /// The re-open's code.
    pub fn read_from<Fd: AsHandle>(fd: Fd) -> Result<Self> {
        let _exposed = expose_placeholders()?;
        let handle = match nt_open(
            fd.as_handle(),
            &[],
            ffi::FILE_LIST_DIRECTORY | ffi::FILE_READ_ATTRIBUTES,
            ffi::FILE_OPEN,
            ffi::FILE_DIRECTORY_FILE,
        ) {
            Ok(handle) => Some(handle),
            Err(Errno::DELETE_PENDING) => None,
            Err(e) => return Err(e),
        };
        Ok(Self {
            done: handle.is_none(),
            handle,
            buf: vec![0; LISTING / 8],
            at: None,
            class: if legacy() { Class::Both } else { Class::Extd },
            started: false,
        })
    }

    /// Run the next listing query into `buf`: `Ok(false)` at the end. Each
    fn fill(&mut self) -> Result<bool> {
        let Some(handle) = self.handle.as_ref() else {
            return Ok(false);
        };
        let _exposed = expose_placeholders()?;
        loop {
            let restart = !self.started;
            // SAFETY: `handle` is live.
            let ok = unsafe {
                ffi::GetFileInformationByHandleEx(
                    handle.as_raw_handle(),
                    self.class.info(restart),
                    self.buf.as_mut_ptr().cast::<c_void>(),
                    LISTING as u32,
                )
            };
            if ok != 0 {
                self.started = true;
                return Ok(true);
            }
            let err = Errno::last();
            match err.0 {
                ffi::ERROR_NO_MORE_FILES => return Ok(false),
                ffi::ERROR_FILE_NOT_FOUND if restart => return Ok(false),
                _ if restart && self.class == Class::Extd && unsupported(err) => {
                    self.class = Class::Both;
                }
                _ => return Err(err),
            }
        }
    }

    /// The entry at byte `at` of the buffer, and the next entry's offset.
    fn parse(&self, at: usize) -> Result<(DirEntry, Option<usize>)> {
        // SAFETY: Any `u64` buffer is a valid `u8` buffer of eight times the length.
        let bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(self.buf.as_ptr().cast::<u8>(), self.buf.len() * 8)
        };
        let bad = || Errno(ffi::ERROR_INVALID_DATA);
        let field = |off: usize, len: usize| bytes.get(at + off..at + off + len).ok_or_else(bad);
        let u32_at =
            |off| field(off, 4).map(|b| u32::from_le_bytes(b.try_into().unwrap_or_default()));
        let i64_at =
            |off| field(off, 8).map(|b| i64::from_le_bytes(b.try_into().unwrap_or_default()));
        type E = ffi::FILE_ID_EXTD_DIR_INFO;
        type B = ffi::FILE_ID_BOTH_DIR_INFO;
        let (attributes, name_len, tag, ino, name_at) = match self.class {
            Class::Extd => (
                u32_at(offset_of!(E, FileAttributes))?,
                u32_at(offset_of!(E, FileNameLength))?,
                u32_at(offset_of!(E, ReparsePointTag))?,
                u128::from_le_bytes(
                    field(offset_of!(E, FileId), 16)?
                        .try_into()
                        .unwrap_or_default(),
                ),
                offset_of!(E, FileName),
            ),
            Class::Both => (
                u32_at(offset_of!(B, FileAttributes))?,
                u32_at(offset_of!(B, FileNameLength))?,
                u32_at(offset_of!(B, EaSize))?,
                u128::from(u64::from_le_bytes(
                    field(offset_of!(B, FileId), 8)?
                        .try_into()
                        .unwrap_or_default(),
                )),
                offset_of!(B, FileName),
            ),
        };
        let size = i64_at(offset_of!(E, EndOfFile))?;
        let allocation = i64_at(offset_of!(E, AllocationSize))?;
        let next = u32_at(offset_of!(E, NextEntryOffset))? as usize;
        let name: Vec<u16> = field(name_at, name_len as usize)?
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let reparse = attributes & ffi::FILE_ATTRIBUTE_REPARSE_POINT != 0;
        Ok((
            DirEntry {
                name: OsString::from_wide(&name),
                ino,
                attributes,
                reparse_tag: if reparse { tag } else { 0 },
                size: u64::try_from(size).unwrap_or(0),
                allocation: u64::try_from(allocation).unwrap_or(0),
            },
            (next != 0).then(|| at + next),
        ))
    }
}

impl Iterator for Dir {
    type Item = Result<DirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.done {
                return None;
            }
            let at = match self.at {
                Some(at) => at,
                None => match self.fill() {
                    Ok(true) => 0,
                    Ok(false) => {
                        self.done = true;
                        return None;
                    }
                    Err(e) => {
                        self.done = true;
                        return Some(Err(e));
                    }
                },
            };
            let (entry, next) = match self.parse(at) {
                Ok(parsed) => parsed,
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
            };
            self.at = next;
            if entry.name != "." && entry.name != ".." {
                return Some(Ok(entry));
            }
        }
    }
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static LEGACY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// [`seam::leave_placeholders_disguised`].
    static DISGUISED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether this thread reads listings with `FileIdBothDirectoryInfo` and
fn legacy() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        LEGACY.with(std::cell::Cell::get)
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

/// What a test drives that this machine's ntfs never reaches on its own.
#[cfg(any(test, debug_assertions))]
pub mod seam {
    use super::{AsHandle, AsRawHandle as _, Errno, Result, ffi};
    use std::os::windows::ffi::OsStrExt as _;
    use std::path::Path;

    /// On this thread, read listings with `FileIdBothDirectoryInfo` and delete with the legacy.
    pub fn legacy_classes(on: bool) {
        super::LEGACY.with(|legacy| legacy.set(on));
    }

    /// On this thread, every [`super::ExposedPlaceholders`] leaves the thread's mode as it.
    pub fn leave_placeholders_disguised(on: bool) {
        super::DISGUISED.with(|disguised| disguised.set(on));
    }

    /// This thread's cloud-placeholder mode as the system holds it now.
    #[must_use]
    pub fn thread_placeholder_mode() -> i8 {
        // SAFETY: Reads one field of this thread's own TEB.
        unsafe { ffi::RtlQueryThreadPlaceholderCompatibilityMode() }
    }

    /// [`super::volume_guid`]'s reading of the path the system answered.
    #[must_use]
    pub fn guid_of_volume_path(path: &str) -> Option<String> {
        super::guid_of_volume_path(&path.encode_utf16().collect::<Vec<u16>>())
    }

    /// A cloud sync root registered on a test's own scratch folder.
    pub struct SyncRoot {
        /// The folder, NUL-terminated, as registered.
        path: Vec<u16>,
    }

    impl SyncRoot {
        /// Register the existing folder `path` (absolute; on ntfs).
        /// # Errors
        /// The `HRESULT` refusing it, as the Win32 code it carries (or the
        /// raw `HRESULT`).
        pub fn register(path: &Path) -> Result<SyncRoot> {
            let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
            let name: Vec<u16> = "aterm-dirfd test".encode_utf16().chain([0]).collect();
            let version: Vec<u16> = "1".encode_utf16().chain([0]).collect();
            let registration = ffi::CF_SYNC_REGISTRATION {
                StructSize: size_of::<ffi::CF_SYNC_REGISTRATION>() as u32,
                ProviderName: name.as_ptr(),
                ProviderVersion: version.as_ptr(),
                SyncRootIdentity: std::ptr::null(),
                SyncRootIdentityLength: 0,
                FileIdentity: std::ptr::null(),
                FileIdentityLength: 0,
                ProviderId: [0x6174_6572, 0x6d2d_6469, 0x7266_642d, 0x7465_7374],
            };
            let policies = ffi::CF_SYNC_POLICIES {
                StructSize: size_of::<ffi::CF_SYNC_POLICIES>() as u32,
                HydrationPrimary: ffi::CF_HYDRATION_POLICY_FULL,
                HydrationModifier: 0,
                PopulationPrimary: ffi::CF_POPULATION_POLICY_ALWAYS_FULL,
                PopulationModifier: 0,
                InSync: 0,
                HardLink: 0,
                PlaceholderManagement: 0,
            };
            // SAFETY: `wide`, `name` and `version` are NUL-terminated and outlive the call.
            let hr = unsafe {
                ffi::CfRegisterSyncRoot(
                    wide.as_ptr(),
                    &raw const registration,
                    &raw const policies,
                    0,
                )
            };
            hresult(hr)?;
            Ok(SyncRoot { path: wide })
        }

        /// Make the existing file or folder `path`, under this root, a placeholder of it.
        /// # Errors
        /// The open's code, or the `HRESULT` refusing the conversion.
        pub fn convert(&self, path: &Path) -> Result<()> {
            use std::os::windows::fs::OpenOptionsExt as _;
            /// `FILE_FLAG_BACKUP_SEMANTICS`: what opens a folder.
            const BACKUP_SEMANTICS: u32 = 0x0200_0000;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(BACKUP_SEMANTICS)
                .open(path)
                .map_err(|e| Errno(e.raw_os_error().unwrap_or(ffi::ERROR_INVALID_FUNCTION)))?;
            let identity = b"aterm-dirfd test";
            let mut usn = 0i64;
            // SAFETY: `file` is open for writing and live for the call.
            let hr = unsafe {
                ffi::CfConvertToPlaceholder(
                    file.as_raw_handle(),
                    identity.as_ptr().cast(),
                    identity.len() as u32,
                    ffi::CF_CONVERT_FLAG_MARK_IN_SYNC,
                    &raw mut usn,
                    std::ptr::null_mut(),
                )
            };
            hresult(hr)
        }
    }

    impl Drop for SyncRoot {
        fn drop(&mut self) {
            // SAFETY: `path` is the NUL-terminated folder this root was registered on, alive.
            let hr = unsafe { ffi::CfUnregisterSyncRoot(self.path.as_ptr()) };
            if hr < 0 {
                let path = String::from_utf16_lossy(&self.path);
                if std::thread::panicking() {
                    eprintln!("CfUnregisterSyncRoot({path}) answered {hr:#x}");
                } else {
                    panic!("CfUnregisterSyncRoot({path}) answered {hr:#x}");
                }
            }
        }
    }

    /// An `HRESULT` as this crate's result: a failure carrying a Win32 code
    fn hresult(hr: ffi::HRESULT) -> Result<()> {
        if hr >= 0 {
            Ok(())
        } else if (hr as u32) & 0xFFFF_0000 == 0x8007_0000 {
            Err(Errno((hr as u32 & 0xFFFF) as i32))
        } else {
            Err(Errno(hr))
        }
    }

    /// The SDK name of the listing class `dir` reads with now.
    #[must_use]
    pub fn listing_class(dir: &super::Dir) -> &'static str {
        match dir.class {
            super::Class::Extd => "FileIdExtdDirectoryInfo",
            super::Class::Both => "FileIdBothDirectoryInfo",
        }
    }

    /// [`super::pin`]'s device check on an object-manager target as `QueryDosDeviceW` spells it.
    #[must_use]
    pub fn is_volume_device(target: &str) -> bool {
        super::is_volume_device(&target.encode_utf16().collect::<Vec<u16>>())
    }

    /// [`super::pin`]'s device check on the live dos device `name`.
    /// # Errors
    /// [`Errno::NOT_VOLUME_ROOT`] for a device that is not a volume's,
    /// [`Errno::PATH_NOT_FOUND`] for no such device.
    pub fn volume_device(name: &str) -> Result<()> {
        super::volume_device(std::ffi::OsStr::new(name))
    }

    /// [`super::pin`]'s check on an opened root.
    /// # Errors
    /// The `FileNameInfo` query's code.
    pub fn is_volume_root<Fd: AsHandle>(fd: Fd) -> Result<bool> {
        super::is_volume_root(fd.as_handle())
    }

    /// Make the file `fd` is open on a reparse point with `tag` and `guid` and no payload.
    /// # Errors
    /// `FSCTL_SET_REPARSE_POINT`'s code.
    pub fn set_reparse_tag<Fd: AsHandle>(fd: Fd, tag: u32, guid: [u8; 16]) -> Result<()> {
        let buffer = ffi::REPARSE_GUID_DATA_BUFFER {
            ReparseTag: tag,
            ReparseDataLength: 0,
            Reserved: 0,
            ReparseGuid: guid,
        };
        let mut returned = 0u32;
        // SAFETY: A live handle.
        let ok = unsafe {
            ffi::DeviceIoControl(
                fd.as_handle().as_raw_handle(),
                ffi::FSCTL_SET_REPARSE_POINT,
                (&raw const buffer).cast(),
                size_of::<ffi::REPARSE_GUID_DATA_BUFFER>() as u32,
                std::ptr::null_mut(),
                0,
                &raw mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 { Err(Errno::last()) } else { Ok(()) }
    }

    /// Make the empty directory `fd` is open on an `IO_REPARSE_TAG_MOUNT_POINT` reparse point.
    /// # Errors
    /// [`Errno::INVAL`] for a name too long for reparse data;
    /// `FSCTL_SET_REPARSE_POINT`'s code.
    pub fn set_mount_point<Fd: AsHandle>(fd: Fd, substitute: &str) -> Result<()> {
        let target: Vec<u16> = substitute.encode_utf16().collect();
        let field = |bytes: usize| u16::try_from(bytes).map_err(|_| Errno::INVAL);
        let names = (target.len() + 1 + 1) * 2;
        let header = size_of::<ffi::MOUNT_POINT_REPARSE_HEADER>();
        let fields = [
            field(header - 8 + names)?,
            0,
            0,
            field(target.len() * 2)?,
            field((target.len() + 1) * 2)?,
            0,
        ];
        let mut data: Vec<u8> = Vec::with_capacity(header + names);
        data.extend(ffi::IO_REPARSE_TAG_MOUNT_POINT.to_ne_bytes());
        for value in fields.iter().chain(&target).chain(&[0, 0]) {
            data.extend(value.to_ne_bytes());
        }
        let mut returned = 0u32;
        // SAFETY: A live handle.
        let ok = unsafe {
            ffi::DeviceIoControl(
                fd.as_handle().as_raw_handle(),
                ffi::FSCTL_SET_REPARSE_POINT,
                data.as_ptr().cast(),
                u32::try_from(data.len()).map_err(|_| Errno::INVAL)?,
                std::ptr::null_mut(),
                0,
                &raw mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 { Err(Errno::last()) } else { Ok(()) }
    }
}
