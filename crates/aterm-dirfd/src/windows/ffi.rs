// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows half's FFI, every declaration in this one file: kernel32
//! (`QueryDosDeviceW` and `CreateFileW` for a volume root, the by-handle
//! information calls, `GetFinalPathNameByHandleW` for a handle's volume,
//! `SetThreadErrorMode`, `DeviceIoControl` to read a
//! junction's target and, for the test seam only, to set one), ntdll (`NtCreateFile` relative to a directory handle,
//! `RtlNtStatusToDosError`, this thread's cloud-placeholder mode), both
//! system DLLs std already links, and advapi32
//! (a handle's owner, the process token's user: the disk reclaim's other-user
//! fence), a system DLL every Windows process maps; for the test seam only,
//! cldapi (a scratch cloud sync root and its placeholders, the Cloud Files
//! API's system DLL since Windows 10 1709). The crate's
//! tests declare no FFI of their own: what they need reaches them through
//! `windows::seam`.
//!
//! Struct layouts are transcribed from the Windows SDK (`winnt.h`,
//! `winbase.h`, `minwinbase.h`, `ntdef.h`, `winternl.h`) and pinned below by
//! compile-time assertions of their sizes and field offsets, so a transcription
//! slip fails the build instead of reading a neighbouring field. Handles are
//! `std`'s `RawHandle` (`*mut c_void`), so they pass to and from
//! `OwnedHandle` without a cast.

// Win32 ABI names are kept verbatim (PascalCase fields, SCREAMING names) so
// they can be checked against the SDK headers line by line.
#![allow(
    non_snake_case,
    non_camel_case_types,
    non_upper_case_globals,
    clippy::upper_case_acronyms
)]

use std::ffi::c_void;
use std::mem::{offset_of, size_of};

/// A Win32/NT `HANDLE`.
pub(crate) type HANDLE = *mut c_void;
/// An `NTSTATUS`: negative is failure.
pub(crate) type NTSTATUS = i32;

/// `INVALID_HANDLE_VALUE`, `CreateFileW`'s failure.
pub(crate) const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;

// -- access rights (winnt.h) -------------------------------------------------

/// `FILE_READ_DATA` / `FILE_LIST_DIRECTORY`.
pub(crate) const FILE_LIST_DIRECTORY: u32 = 0x0001;
/// `FILE_TRAVERSE` (on a directory; `FILE_EXECUTE` on a file).
pub(crate) const FILE_TRAVERSE: u32 = 0x0020;
/// `FILE_READ_ATTRIBUTES`: every by-handle query here needs it.
pub(crate) const FILE_READ_ATTRIBUTES: u32 = 0x0080;
/// `DELETE`: the one right a disposition needs.
pub(crate) const DELETE: u32 = 0x0001_0000;
/// `READ_CONTROL`: read the security descriptor (its owner, here).
pub(crate) const READ_CONTROL: u32 = 0x0002_0000;
/// `SYNCHRONIZE`: required with `FILE_SYNCHRONOUS_IO_NONALERT`.
pub(crate) const SYNCHRONIZE: u32 = 0x0010_0000;
/// `FILE_GENERIC_READ`: `STANDARD_RIGHTS_READ | FILE_READ_DATA |
/// FILE_READ_ATTRIBUTES | FILE_READ_EA | SYNCHRONIZE`.
pub(crate) const FILE_GENERIC_READ: u32 = 0x0012_0089;
/// `FILE_GENERIC_WRITE`: `STANDARD_RIGHTS_WRITE | FILE_WRITE_DATA |
/// FILE_WRITE_ATTRIBUTES | FILE_WRITE_EA | FILE_APPEND_DATA | SYNCHRONIZE`.
pub(crate) const FILE_GENERIC_WRITE: u32 = 0x0012_0116;

/// `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`: every handle
/// this crate opens shares everything, so holding one never makes another
/// process's open, rename or delete fail.
pub(crate) const FILE_SHARE_ALL: u32 = 0x7;

// -- NtCreateFile (wdm.h / ntifs.h) -----------------------------------------

/// `CreateDisposition`: open what exists, fail otherwise.
pub(crate) const FILE_OPEN: u32 = 1;
/// `CreateDisposition`: create, fail if the name exists (`O_CREAT|O_EXCL`).
pub(crate) const FILE_CREATE: u32 = 2;
/// `CreateDisposition`: open, or create when absent (`O_CREAT`).
pub(crate) const FILE_OPEN_IF: u32 = 3;

/// `CreateOptions`: the name must be a directory (`STATUS_NOT_A_DIRECTORY`
/// otherwise).
pub(crate) const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;
/// `CreateOptions`: synchronous I/O, not alertable, so `std::fs::File` reads,
/// writes and `LockFileEx` complete in the call.
pub(crate) const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x0000_0020;
// `FILE_OPEN_FOR_BACKUP_INTENT` (0x4000) is deliberately absent: an NT open
// needs no backup intent to open a directory (only Win32's `CreateFileW`
// wants `FILE_FLAG_BACKUP_SEMANTICS` for that), and its one effect — a token
// holding `SeBackupPrivilege`/`SeRestorePrivilege` ENABLED bypasses the DACL —
// would let a reclaim list and delete what the ACL keeps from its user.
/// `CreateOptions`: open a reparse point AS ITSELF instead of reparsing it
/// (`FILE_FLAG_OPEN_REPARSE_POINT`).
pub(crate) const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// `OBJECT_ATTRIBUTES.Attributes`: case-insensitive name lookup, what
/// `CreateFileW` always passes (a case-sensitive directory stays sensitive).
pub(crate) const OBJ_CASE_INSENSITIVE: u32 = 0x0000_0040;
/// `OBJECT_ATTRIBUTES.Attributes`: fail rather than reparse at ANY component
/// (Windows 10 1709 and later; an older kernel answers
/// `STATUS_INVALID_PARAMETER`, see `windows::nt_open`).
pub(crate) const OBJ_DONT_REPARSE: u32 = 0x0000_1000;

// -- CreateFileW (fileapi.h / winbase.h) -------------------------------------

/// `dwCreationDisposition`: open what exists.
pub(crate) const OPEN_EXISTING: u32 = 3;
/// `dwFlagsAndAttributes`: open a directory. It asks for backup intent too,
/// harmless on the one `CreateFileW` here: the root is opened for
/// `FILE_READ_ATTRIBUTES` only, and every later open is a relative NT open
/// checked against its own file's DACL.
pub(crate) const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
/// `dwFlagsAndAttributes`: open a reparse point as itself.
pub(crate) const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// `SetThreadErrorMode`: no "insert a disk" critical-error box for a volume
/// root with no medium.
pub(crate) const SEM_FAILCRITICALERRORS: u32 = 0x0001;
/// `SetThreadErrorMode`: no open-file error box either.
pub(crate) const SEM_NOOPENFILEERRORBOX: u32 = 0x8000;

/// `GetFinalPathNameByHandleW`'s `dwFlags`: the path through the volume's
/// GUID name (`\\?\Volume{…}\…`), never a drive letter or a mount folder
/// (`VOLUME_NAME_GUID`; `FILE_NAME_NORMALIZED`, 0, is the rest).
pub(crate) const VOLUME_NAME_GUID: u32 = 0x1;

// -- cloud placeholders (ntifs.h) --------------------------------------------

/// `PHCM_EXPOSE_PLACEHOLDERS`: this thread sees a Cloud Files placeholder
/// as what it is — a reparse point with its cloud tag — where the default,
/// `PHCM_DISGUISE_PLACEHOLDER` (1), shows a placeholder inside a sync root as
/// a plain file or folder (`windows::ExposedPlaceholders`).
pub(crate) const PHCM_EXPOSE_PLACEHOLDERS: i8 = 2;

// -- file attributes and reparse tags (winnt.h) ------------------------------

/// `FILE_ATTRIBUTE_READONLY`.
pub(crate) const FILE_ATTRIBUTE_READONLY: u32 = 0x0001;
/// `FILE_ATTRIBUTE_DIRECTORY`.
pub(crate) const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0010;
/// `FILE_ATTRIBUTE_REPARSE_POINT`.
pub(crate) const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
/// `IsReparseTagNameSurrogate`'s bit: the tag names ANOTHER entity — a
/// symbolic link, a junction or volume mount point, a WSL symlink. Such a
/// reparse point is a link, removed as itself — but a volume mount point
/// (`windows::mounts_volume`).
pub(crate) const REPARSE_TAG_NAME_SURROGATE: u32 = 0x2000_0000;
/// `IO_REPARSE_TAG_MOUNT_POINT`: a junction AND a volume mount point — one
/// tag, told apart only by the target its data names.
pub(crate) const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;

// -- FILE_INFO_BY_HANDLE_CLASS (minwinbase.h) --------------------------------

pub(crate) const FileBasicInfo: i32 = 0;
pub(crate) const FileStandardInfo: i32 = 1;
pub(crate) const FileNameInfo: i32 = 2;
pub(crate) const FileDispositionInfo: i32 = 4;
pub(crate) const FileAttributeTagInfo: i32 = 9;
pub(crate) const FileIdBothDirectoryInfo: i32 = 10;
pub(crate) const FileIdBothDirectoryRestartInfo: i32 = 11;
pub(crate) const FileIdInfo: i32 = 18;
pub(crate) const FileIdExtdDirectoryInfo: i32 = 19;
pub(crate) const FileIdExtdDirectoryRestartInfo: i32 = 20;
pub(crate) const FileDispositionInfoEx: i32 = 21;

/// `FILE_DISPOSITION_INFO_EX.Flags`: delete.
pub(crate) const FILE_DISPOSITION_FLAG_DELETE: u32 = 0x0001;
/// `FILE_DISPOSITION_INFO_EX.Flags`: unlink the name NOW, as `unlink(2)`
/// does, while other handles stay open (they keep the file, not the name).
pub(crate) const FILE_DISPOSITION_FLAG_POSIX_SEMANTICS: u32 = 0x0002;
/// `FILE_DISPOSITION_INFO_EX.Flags`: delete a read-only file too.
pub(crate) const FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE: u32 = 0x0010;

// -- NTSTATUS values (ntstatus.h) the mapping special-cases ------------------

/// `STATUS_INVALID_PARAMETER`: also an older kernel's answer to
/// `OBJ_DONT_REPARSE`.
pub(crate) const STATUS_INVALID_PARAMETER: u32 = 0xC000_000D;
/// `STATUS_DELETE_PENDING`, which `RtlNtStatusToDosError` would flatten into
/// `ERROR_ACCESS_DENIED`.
pub(crate) const STATUS_DELETE_PENDING: u32 = 0xC000_0056;
/// `STATUS_FILE_IS_A_DIRECTORY`, which `RtlNtStatusToDosError` would flatten
/// into `ERROR_ACCESS_DENIED`.
pub(crate) const STATUS_FILE_IS_A_DIRECTORY: u32 = 0xC000_00BA;

// -- Win32 error codes (winerror.h) -----------------------------------------

// -- security (accctrl.h, winnt.h) -------------------------------------------

/// `SE_OBJECT_TYPE::SE_FILE_OBJECT`: `GetSecurityInfo` on a file handle.
pub(crate) const SE_FILE_OBJECT: u32 = 1;
/// `SECURITY_INFORMATION`: the owner, and nothing else of the descriptor.
pub(crate) const OWNER_SECURITY_INFORMATION: u32 = 0x0000_0001;
/// `TOKEN_QUERY`: read a token's facts (its user).
pub(crate) const TOKEN_QUERY: u32 = 0x0008;
/// `TOKEN_INFORMATION_CLASS::TokenUser`.
pub(crate) const TokenUser: i32 = 1;

pub(crate) const ERROR_INVALID_FUNCTION: i32 = 1;
pub(crate) const ERROR_FILE_NOT_FOUND: i32 = 2;
pub(crate) const ERROR_PATH_NOT_FOUND: i32 = 3;
pub(crate) const ERROR_ACCESS_DENIED: i32 = 5;
pub(crate) const ERROR_INVALID_DATA: i32 = 13;
pub(crate) const ERROR_NO_MORE_FILES: i32 = 18;
pub(crate) const ERROR_SHARING_VIOLATION: i32 = 32;
pub(crate) const ERROR_NOT_SUPPORTED: i32 = 50;
pub(crate) const ERROR_INVALID_PARAMETER: i32 = 87;
pub(crate) const ERROR_INSUFFICIENT_BUFFER: i32 = 122;
pub(crate) const ERROR_INVALID_NAME: i32 = 123;
pub(crate) const ERROR_INVALID_LEVEL: i32 = 124;
pub(crate) const ERROR_DIR_NOT_EMPTY: i32 = 145;
pub(crate) const ERROR_MORE_DATA: i32 = 234;
pub(crate) const ERROR_DIRECTORY: i32 = 267;
pub(crate) const ERROR_DELETE_PENDING: i32 = 303;
pub(crate) const ERROR_DIRECTORY_NOT_SUPPORTED: i32 = 336;
pub(crate) const ERROR_REPARSE_POINT_ENCOUNTERED: i32 = 4395;

// -- structs ----------------------------------------------------------------

/// `UNICODE_STRING`: a counted (NOT NUL-terminated) UTF-16 name. Lengths are
/// in BYTES.
#[repr(C)]
pub(crate) struct UNICODE_STRING {
    pub Length: u16,
    pub MaximumLength: u16,
    pub Buffer: *mut u16,
}

/// `OBJECT_ATTRIBUTES`: the name, the directory handle it is relative to, and
/// the `OBJ_*` attributes.
#[repr(C)]
pub(crate) struct OBJECT_ATTRIBUTES {
    pub Length: u32,
    pub RootDirectory: HANDLE,
    pub ObjectName: *mut UNICODE_STRING,
    pub Attributes: u32,
    pub SecurityDescriptor: *mut c_void,
    pub SecurityQualityOfService: *mut c_void,
}

/// `IO_STATUS_BLOCK`: the NTSTATUS/PVOID union is pointer-sized.
#[repr(C)]
pub(crate) struct IO_STATUS_BLOCK {
    pub Status: isize,
    pub Information: usize,
}

/// `FILE_BASIC_INFO`: the four times (FILETIME ticks: 100 ns since 1601) and
/// the attributes.
#[repr(C)]
pub(crate) struct FILE_BASIC_INFO {
    pub CreationTime: i64,
    pub LastAccessTime: i64,
    pub LastWriteTime: i64,
    pub ChangeTime: i64,
    pub FileAttributes: u32,
}

/// `FILE_STANDARD_INFO`: sizes, link count, and whether a delete is pending.
#[repr(C)]
pub(crate) struct FILE_STANDARD_INFO {
    pub AllocationSize: i64,
    pub EndOfFile: i64,
    pub NumberOfLinks: u32,
    /// `BOOLEAN`.
    pub DeletePending: u8,
    /// `BOOLEAN`.
    pub Directory: u8,
}

/// `FILE_ATTRIBUTE_TAG_INFO`: the attributes and, on a reparse point, its tag.
#[repr(C)]
pub(crate) struct FILE_ATTRIBUTE_TAG_INFO {
    pub FileAttributes: u32,
    pub ReparseTag: u32,
}

/// `FILE_ID_INFO`: the 64-bit volume serial number and the 128-bit file id
/// (`FILE_ID_128`, an opaque 16-byte identifier).
#[repr(C)]
pub(crate) struct FILE_ID_INFO {
    pub VolumeSerialNumber: u64,
    pub FileId: [u8; 16],
}

/// `FILE_NAME_INFO`: the open file's path within its volume (`\` for the
/// volume's root), `FileNameLength` BYTES of UTF-16 from `FileName` on, not
/// NUL-terminated. Read by offset out of a buffer, as the listing entries are.
#[repr(C)]
pub(crate) struct FILE_NAME_INFO {
    pub FileNameLength: u32,
    pub FileName: [u16; 1],
}

/// `FILE_DISPOSITION_INFO`: the legacy delete-on-close disposition.
#[repr(C)]
pub(crate) struct FILE_DISPOSITION_INFO {
    /// `BOOLEAN`.
    pub DeleteFile: u8,
}

/// `FILE_DISPOSITION_INFO_EX`: the disposition with flags (Windows 10 1607;
/// `IGNORE_READONLY_ATTRIBUTE` from 1809).
#[repr(C)]
pub(crate) struct FILE_DISPOSITION_INFO_EX {
    pub Flags: u32,
}

/// `FILE_ID_EXTD_DIR_INFO`: one directory entry with its 128-bit file id and
/// reparse tag. Entries are variable-length (`FileName` runs on for
/// `FileNameLength` bytes) and chained by `NextEntryOffset`; the walker reads
/// fields at these offsets out of the byte buffer, never through a reference
/// to this struct (the last entry can end before `size_of` does).
#[repr(C)]
pub(crate) struct FILE_ID_EXTD_DIR_INFO {
    pub NextEntryOffset: u32,
    pub FileIndex: u32,
    pub CreationTime: i64,
    pub LastAccessTime: i64,
    pub LastWriteTime: i64,
    pub ChangeTime: i64,
    pub EndOfFile: i64,
    pub AllocationSize: i64,
    pub FileAttributes: u32,
    pub FileNameLength: u32,
    pub EaSize: u32,
    pub ReparsePointTag: u32,
    pub FileId: [u8; 16],
    pub FileName: [u16; 1],
}

/// `FILE_ID_BOTH_DIR_INFO`: the fallback entry for a file system without the
/// extended class. Its file id is 64-bit, and a reparse point's tag rides in
/// `EaSize`. Read by offset, as [`FILE_ID_EXTD_DIR_INFO`] is.
#[repr(C)]
pub(crate) struct FILE_ID_BOTH_DIR_INFO {
    pub NextEntryOffset: u32,
    pub FileIndex: u32,
    pub CreationTime: i64,
    pub LastAccessTime: i64,
    pub LastWriteTime: i64,
    pub ChangeTime: i64,
    pub EndOfFile: i64,
    pub AllocationSize: i64,
    pub FileAttributes: u32,
    pub FileNameLength: u32,
    pub EaSize: u32,
    /// `CCHAR`.
    pub ShortNameLength: i8,
    pub ShortName: [u16; 12],
    pub FileId: i64,
    pub FileName: [u16; 1],
}

/// `SID_AND_ATTRIBUTES`: a pointer to a SID inside the same buffer, and its
/// group attributes.
#[repr(C)]
pub(crate) struct SID_AND_ATTRIBUTES {
    pub Sid: *mut c_void,
    pub Attributes: u32,
}

/// `TOKEN_USER`: `GetTokenInformation(TokenUser)`'s answer, the SID it points
/// at following it in the same buffer.
#[repr(C)]
pub(crate) struct TOKEN_USER {
    pub User: SID_AND_ATTRIBUTES,
}

// -- layouts, pinned against the SDK ----------------------------------------

/// Pointer-width-dependent sizes: (64-bit, 32-bit).
const fn by_width(wide: usize, narrow: usize) -> usize {
    if size_of::<usize>() == 8 {
        wide
    } else {
        narrow
    }
}

const _: () = {
    assert!(size_of::<UNICODE_STRING>() == by_width(16, 8));
    assert!(offset_of!(UNICODE_STRING, Buffer) == by_width(8, 4));
    assert!(size_of::<OBJECT_ATTRIBUTES>() == by_width(48, 24));
    assert!(offset_of!(OBJECT_ATTRIBUTES, RootDirectory) == by_width(8, 4));
    assert!(offset_of!(OBJECT_ATTRIBUTES, ObjectName) == by_width(16, 8));
    assert!(offset_of!(OBJECT_ATTRIBUTES, Attributes) == by_width(24, 12));
    assert!(offset_of!(OBJECT_ATTRIBUTES, SecurityQualityOfService) == by_width(40, 20));
    assert!(size_of::<IO_STATUS_BLOCK>() == by_width(16, 8));
    assert!(size_of::<FILE_BASIC_INFO>() == 40);
    assert!(offset_of!(FILE_BASIC_INFO, LastWriteTime) == 16);
    assert!(offset_of!(FILE_BASIC_INFO, FileAttributes) == 32);
    assert!(size_of::<FILE_STANDARD_INFO>() == 24);
    assert!(offset_of!(FILE_STANDARD_INFO, NumberOfLinks) == 16);
    assert!(offset_of!(FILE_STANDARD_INFO, DeletePending) == 20);
    assert!(offset_of!(FILE_STANDARD_INFO, Directory) == 21);
    assert!(size_of::<FILE_ATTRIBUTE_TAG_INFO>() == 8);
    assert!(size_of::<FILE_ID_INFO>() == 24);
    assert!(offset_of!(FILE_NAME_INFO, FileName) == 4);
    assert!(size_of::<FILE_NAME_INFO>() == 8);
    assert!(offset_of!(FILE_ID_INFO, FileId) == 8);
    assert!(size_of::<FILE_DISPOSITION_INFO>() == 1);
    assert!(size_of::<FILE_DISPOSITION_INFO_EX>() == 4);
    assert!(offset_of!(FILE_ID_EXTD_DIR_INFO, EndOfFile) == 40);
    assert!(offset_of!(FILE_ID_EXTD_DIR_INFO, AllocationSize) == 48);
    assert!(offset_of!(FILE_ID_EXTD_DIR_INFO, FileAttributes) == 56);
    assert!(offset_of!(FILE_ID_EXTD_DIR_INFO, FileNameLength) == 60);
    assert!(offset_of!(FILE_ID_EXTD_DIR_INFO, ReparsePointTag) == 68);
    assert!(offset_of!(FILE_ID_EXTD_DIR_INFO, FileId) == 72);
    assert!(offset_of!(FILE_ID_EXTD_DIR_INFO, FileName) == 88);
    assert!(size_of::<FILE_ID_EXTD_DIR_INFO>() == 96);
    assert!(offset_of!(FILE_ID_BOTH_DIR_INFO, EaSize) == 64);
    assert!(offset_of!(FILE_ID_BOTH_DIR_INFO, ShortNameLength) == 68);
    assert!(offset_of!(FILE_ID_BOTH_DIR_INFO, ShortName) == 70);
    assert!(offset_of!(FILE_ID_BOTH_DIR_INFO, FileId) == 96);
    assert!(offset_of!(FILE_ID_BOTH_DIR_INFO, FileName) == 104);
    assert!(size_of::<FILE_ID_BOTH_DIR_INFO>() == 112);
    assert!(size_of::<SID_AND_ATTRIBUTES>() == by_width(16, 8));
    assert!(offset_of!(SID_AND_ATTRIBUTES, Attributes) == by_width(8, 4));
    assert!(size_of::<TOKEN_USER>() == by_width(16, 8));
};

// -- functions ----------------------------------------------------------------

#[link(name = "kernel32")]
unsafe extern "system" {
    /// The object-manager target of the DOS device `lpDeviceName` (`C:`,
    /// `Volume{…}`): NUL-terminated strings, the current target first, ended
    /// by an extra NUL. Reads no file system, so it never waits on a network.
    /// Returns the `u16`s stored, or 0 with `ERROR_FILE_NOT_FOUND` (no such
    /// device) or `ERROR_INSUFFICIENT_BUFFER` in `GetLastError`.
    pub(crate) fn QueryDosDeviceW(
        lpDeviceName: *const u16,
        lpTargetPath: *mut u16,
        ucchMax: u32,
    ) -> u32;
    /// Opens a volume root by its verbatim (`\\?\`) spelling — the one
    /// absolute-path open this crate makes. Returns `INVALID_HANDLE_VALUE` on
    /// failure, with the reason in `GetLastError`.
    pub(crate) fn CreateFileW(
        lpFileName: *const u16,
        dwDesiredAccess: u32,
        dwShareMode: u32,
        lpSecurityAttributes: *const c_void,
        dwCreationDisposition: u32,
        dwFlagsAndAttributes: u32,
        hTemplateFile: HANDLE,
    ) -> HANDLE;
    /// One class of facts about an open handle, or one batch of a directory's
    /// entries. Returns a `BOOL` (0 = failure, reason in `GetLastError`).
    pub(crate) fn GetFileInformationByHandleEx(
        hFile: HANDLE,
        FileInformationClass: i32,
        lpFileInformation: *mut c_void,
        dwBufferSize: u32,
    ) -> i32;
    /// Sets one class of facts on an open handle: here, its delete
    /// disposition. Returns a `BOOL`.
    pub(crate) fn SetFileInformationByHandle(
        hFile: HANDLE,
        FileInformationClass: i32,
        lpFileInformation: *const c_void,
        dwBufferSize: u32,
    ) -> i32;
    /// This thread's critical-error mode; returns a `BOOL`, the old mode in
    /// `lpOldMode`.
    pub(crate) fn SetThreadErrorMode(dwNewMode: u32, lpOldMode: *mut u32) -> i32;
    /// The path of the file `hFile` is open on, spelled as `dwFlags` says
    /// (here through its volume's GUID name). Returns the units stored, NUL
    /// not counted; or, when `cchFilePath` is too small, the units needed,
    /// NUL counted; or 0 with the reason in `GetLastError`.
    pub(crate) fn GetFinalPathNameByHandleW(
        hFile: HANDLE,
        lpszFilePath: *mut u16,
        cchFilePath: u32,
        dwFlags: u32,
    ) -> u32;
    /// The pseudo-handle of this process (never closed).
    pub(crate) fn GetCurrentProcess() -> HANDLE;
    /// Frees what `GetSecurityInfo` allocated; returns NULL on success.
    pub(crate) fn LocalFree(hMem: *mut c_void) -> *mut c_void;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    /// The parts `SecurityInfo` names of the security descriptor of the object
    /// `handle` is open on (it needs `READ_CONTROL`): here the owner, as a
    /// pointer into the descriptor returned in `ppSecurityDescriptor`, which
    /// the caller frees with `LocalFree`. Returns a Win32 error code (0 = ok),
    /// NOT a `BOOL`.
    pub(crate) fn GetSecurityInfo(
        handle: HANDLE,
        ObjectType: u32,
        SecurityInfo: u32,
        ppsidOwner: *mut *mut c_void,
        ppsidGroup: *mut *mut c_void,
        ppDacl: *mut *mut c_void,
        ppSacl: *mut *mut c_void,
        ppSecurityDescriptor: *mut *mut c_void,
    ) -> u32;
    /// Opens the access token of `ProcessHandle`. Returns a `BOOL`.
    pub(crate) fn OpenProcessToken(
        ProcessHandle: HANDLE,
        DesiredAccess: u32,
        TokenHandle: *mut HANDLE,
    ) -> i32;
    /// One class of a token's facts into `TokenInformation`; with a buffer too
    /// small, fails with `ERROR_INSUFFICIENT_BUFFER` and the length wanted in
    /// `ReturnLength`. Returns a `BOOL`.
    pub(crate) fn GetTokenInformation(
        TokenHandle: HANDLE,
        TokenInformationClass: i32,
        TokenInformation: *mut c_void,
        TokenInformationLength: u32,
        ReturnLength: *mut u32,
    ) -> i32;
    /// Whether `pSid` points at a well-formed SID. Returns a `BOOL`.
    pub(crate) fn IsValidSid(pSid: *const c_void) -> i32;
    /// The length in bytes of the valid SID at `pSid`.
    pub(crate) fn GetLengthSid(pSid: *const c_void) -> u32;
}

#[link(name = "ntdll")]
unsafe extern "system" {
    /// Opens (or creates) `ObjectName` relative to `RootDirectory` — the
    /// handle-relative open Win32 has no call for. Returns an NTSTATUS.
    pub(crate) fn NtCreateFile(
        FileHandle: *mut HANDLE,
        DesiredAccess: u32,
        ObjectAttributes: *const OBJECT_ATTRIBUTES,
        IoStatusBlock: *mut IO_STATUS_BLOCK,
        AllocationSize: *const i64,
        FileAttributes: u32,
        ShareAccess: u32,
        CreateDisposition: u32,
        CreateOptions: u32,
        EaBuffer: *const c_void,
        EaLength: u32,
    ) -> NTSTATUS;
    /// The Win32 error code an NTSTATUS maps to.
    pub(crate) fn RtlNtStatusToDosError(Status: NTSTATUS) -> u32;
    /// Sets this thread's cloud-placeholder mode (a `PHCM_*` value, a C
    /// `CHAR`) and returns the previous one, or a negative
    /// `PHCM_ERROR_*` (an invalid mode; a thread with no TEB) having changed
    /// nothing. A field of the thread's TEB: no system call, and no other
    /// thread sees it (Windows 10 1803; aterm needs 1809 for its console).
    pub(crate) fn RtlSetThreadPlaceholderCompatibilityMode(Mode: i8) -> i8;
    /// This thread's cloud-placeholder mode: `PHCM_APPLICATION_DEFAULT` (0,
    /// the process's mode applies) or the one it set.
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn RtlQueryThreadPlaceholderCompatibilityMode() -> i8;
}

// -- reparse data (winioctl.h, winnt.h, ntifs.h) ----------------------------

/// `FSCTL_GET_REPARSE_POINT` (winioctl.h): a reparse point's data, read
/// through a handle opened on it as itself (`FILE_ANY_ACCESS`).
pub(crate) const FSCTL_GET_REPARSE_POINT: u32 = 0x0009_00A8;
/// `MAXIMUM_REPARSE_DATA_BUFFER_SIZE` (winnt.h): no reparse point's data is
/// longer.
pub(crate) const MAXIMUM_REPARSE_DATA_BUFFER_SIZE: usize = 16 * 1024;

/// `REPARSE_DATA_BUFFER` (ntifs.h) up to `MountPointReparseBuffer.PathBuffer`:
/// what an `IO_REPARSE_TAG_MOUNT_POINT` reparse point's data starts with. The
/// names' offsets and lengths are in bytes, counted from `PathBuffer` (this
/// header's end); a length counts no terminating NUL.
#[repr(C)]
pub(crate) struct MOUNT_POINT_REPARSE_HEADER {
    pub ReparseTag: u32,
    /// The bytes after the first eight (tag, this length, `Reserved`).
    pub ReparseDataLength: u16,
    pub Reserved: u16,
    pub SubstituteNameOffset: u16,
    pub SubstituteNameLength: u16,
    pub PrintNameOffset: u16,
    pub PrintNameLength: u16,
}

const _: () = {
    assert!(offset_of!(MOUNT_POINT_REPARSE_HEADER, ReparseDataLength) == 4);
    assert!(offset_of!(MOUNT_POINT_REPARSE_HEADER, Reserved) == 6);
    assert!(offset_of!(MOUNT_POINT_REPARSE_HEADER, SubstituteNameOffset) == 8);
    assert!(offset_of!(MOUNT_POINT_REPARSE_HEADER, SubstituteNameLength) == 10);
    assert!(offset_of!(MOUNT_POINT_REPARSE_HEADER, PrintNameOffset) == 12);
    assert!(offset_of!(MOUNT_POINT_REPARSE_HEADER, PrintNameLength) == 14);
    assert!(size_of::<MOUNT_POINT_REPARSE_HEADER>() == 16);
};

#[link(name = "kernel32")]
unsafe extern "system" {
    /// Sends a control code to the file `hDevice` is open on, synchronously:
    /// `FSCTL_GET_REPARSE_POINT` (and, for the test seam only,
    /// `FSCTL_SET_REPARSE_POINT`). Returns a `BOOL`.
    pub(crate) fn DeviceIoControl(
        hDevice: HANDLE,
        dwIoControlCode: u32,
        lpInBuffer: *const c_void,
        nInBufferSize: u32,
        lpOutBuffer: *mut c_void,
        nOutBufferSize: u32,
        lpBytesReturned: *mut u32,
        lpOverlapped: *mut c_void,
    ) -> i32;
}

// -- the test seam's own (`windows::seam::set_reparse_tag`) ------------------

/// `FSCTL_SET_REPARSE_POINT` (winioctl.h).
#[cfg(any(test, debug_assertions))]
pub(crate) const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;

/// `REPARSE_GUID_DATA_BUFFER` up to its data (winnt.h's
/// `REPARSE_GUID_DATA_BUFFER_HEADER_SIZE`): what a third party's reparse point
/// carries ahead of its payload. The seam sets one with no payload, so this
/// header is the whole buffer it passes.
#[cfg(any(test, debug_assertions))]
#[repr(C)]
pub(crate) struct REPARSE_GUID_DATA_BUFFER {
    pub ReparseTag: u32,
    pub ReparseDataLength: u16,
    pub Reserved: u16,
    /// A `GUID`'s 16 bytes, as they lie in memory.
    pub ReparseGuid: [u8; 16],
}

#[cfg(any(test, debug_assertions))]
const _: () = {
    assert!(offset_of!(REPARSE_GUID_DATA_BUFFER, ReparseDataLength) == 4);
    assert!(offset_of!(REPARSE_GUID_DATA_BUFFER, Reserved) == 6);
    assert!(offset_of!(REPARSE_GUID_DATA_BUFFER, ReparseGuid) == 8);
    assert!(size_of::<REPARSE_GUID_DATA_BUFFER>() == 24);
};

// -- the test seam's cloud sync root (`windows::seam::SyncRoot`, cfapi.h) ----

/// An `HRESULT`: negative is failure.
#[cfg(any(test, debug_assertions))]
pub(crate) type HRESULT = i32;

/// `CF_SYNC_REGISTRATION`: who provides a sync root. Every pointer is to a
/// NUL-terminated name or an identity blob the call copies.
#[cfg(any(test, debug_assertions))]
#[repr(C)]
pub(crate) struct CF_SYNC_REGISTRATION {
    pub StructSize: u32,
    pub ProviderName: *const u16,
    pub ProviderVersion: *const u16,
    pub SyncRootIdentity: *const c_void,
    pub SyncRootIdentityLength: u32,
    pub FileIdentity: *const c_void,
    pub FileIdentityLength: u32,
    /// A `GUID`'s 16 bytes, as they lie in memory (4-aligned).
    pub ProviderId: [u32; 4],
}

/// `CF_SYNC_POLICIES`: how the platform treats the root's placeholders —
/// each policy an enum or a pair of `USHORT`s (`CF_HYDRATION_POLICY`,
/// `CF_POPULATION_POLICY`: primary, then modifier).
#[cfg(any(test, debug_assertions))]
#[repr(C)]
pub(crate) struct CF_SYNC_POLICIES {
    pub StructSize: u32,
    pub HydrationPrimary: u16,
    pub HydrationModifier: u16,
    pub PopulationPrimary: u16,
    pub PopulationModifier: u16,
    pub InSync: u32,
    pub HardLink: u32,
    pub PlaceholderManagement: u32,
}

/// `CF_HYDRATION_POLICY_FULL`: a placeholder is hydrated whole when read.
#[cfg(any(test, debug_assertions))]
pub(crate) const CF_HYDRATION_POLICY_FULL: u16 = 2;
/// `CF_POPULATION_POLICY_ALWAYS_FULL`: every folder is whole on disk, so a
/// listing never asks a provider for its entries (none is connected).
#[cfg(any(test, debug_assertions))]
pub(crate) const CF_POPULATION_POLICY_ALWAYS_FULL: u16 = 3;
/// `CF_CONVERT_FLAG_MARK_IN_SYNC`: the converted placeholder is in sync,
/// its data local (a hydrated placeholder, as a synced file is).
#[cfg(any(test, debug_assertions))]
pub(crate) const CF_CONVERT_FLAG_MARK_IN_SYNC: u32 = 0x1;

#[cfg(any(test, debug_assertions))]
const _: () = {
    assert!(offset_of!(CF_SYNC_REGISTRATION, ProviderName) == by_width(8, 4));
    assert!(offset_of!(CF_SYNC_REGISTRATION, SyncRootIdentityLength) == by_width(32, 16));
    assert!(offset_of!(CF_SYNC_REGISTRATION, FileIdentity) == by_width(40, 20));
    assert!(offset_of!(CF_SYNC_REGISTRATION, FileIdentityLength) == by_width(48, 24));
    assert!(offset_of!(CF_SYNC_REGISTRATION, ProviderId) == by_width(52, 28));
    assert!(size_of::<CF_SYNC_REGISTRATION>() == by_width(72, 44));
    assert!(offset_of!(CF_SYNC_POLICIES, HydrationPrimary) == 4);
    assert!(offset_of!(CF_SYNC_POLICIES, PopulationPrimary) == 8);
    assert!(offset_of!(CF_SYNC_POLICIES, InSync) == 12);
    assert!(offset_of!(CF_SYNC_POLICIES, PlaceholderManagement) == 20);
    assert!(size_of::<CF_SYNC_POLICIES>() == 24);
};

#[cfg(any(test, debug_assertions))]
#[link(name = "cldapi")]
unsafe extern "system" {
    /// Registers the folder `SyncRootPath` as a cloud sync root (per user;
    /// no administrator, no registry entry — measured 2026-10-20): the
    /// folder becomes a reparse point of the cloud tag, and files under it
    /// may be made placeholders. No provider need connect for that.
    pub(crate) fn CfRegisterSyncRoot(
        SyncRootPath: *const u16,
        Registration: *const CF_SYNC_REGISTRATION,
        Policies: *const CF_SYNC_POLICIES,
        RegisterFlags: u32,
    ) -> HRESULT;
    /// Unregisters the sync root at `SyncRootPath`: the folder and every
    /// placeholder under it read as plain files and folders again
    /// (measured 2026-10-20).
    pub(crate) fn CfUnregisterSyncRoot(SyncRootPath: *const u16) -> HRESULT;
    /// Makes the file or folder `FileHandle` is open on (with write access)
    /// a placeholder of its sync root, as a sync client does once it has
    /// uploaded it. No `OVERLAPPED`: synchronous.
    pub(crate) fn CfConvertToPlaceholder(
        FileHandle: HANDLE,
        FileIdentity: *const c_void,
        FileIdentityLength: u32,
        ConvertFlags: u32,
        ConvertUsn: *mut i64,
        Overlapped: *mut c_void,
    ) -> HRESULT;
}
