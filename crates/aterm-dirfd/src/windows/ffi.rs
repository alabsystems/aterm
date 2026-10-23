// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows FFI declarations for `kernel32`, `ntdll`, `advapi32`, and test-only `cldapi`.

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
/// `FILE_GENERIC_READ`: `STANDARD_RIGHTS_READ | FILE_READ_DATA | FILE_READ_ATTRIBUTES | FILE_READ_EA | SYNCHRONIZE`.
pub(crate) const FILE_GENERIC_READ: u32 = 0x0012_0089;
/// `FILE_GENERIC_WRITE`: `STANDARD_RIGHTS_WRITE | FILE_WRITE_DATA | FILE_WRITE_ATTRIBUTES | FILE_WRITE_EA | FILE_APPEND_DATA | SYNCHRONIZE`.
pub(crate) const FILE_GENERIC_WRITE: u32 = 0x0012_0116;

/// `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`.
pub(crate) const FILE_SHARE_ALL: u32 = 0x7;

/// `CreateDisposition`: open what exists, fail otherwise.
pub(crate) const FILE_OPEN: u32 = 1;
/// `CreateDisposition`: create, fail if the name exists (`O_CREAT|O_EXCL`).
pub(crate) const FILE_CREATE: u32 = 2;
/// `CreateDisposition`: open, or create when absent (`O_CREAT`).
pub(crate) const FILE_OPEN_IF: u32 = 3;

/// `CreateOptions`: the name must be a directory (`STATUS_NOT_A_DIRECTORY` otherwise).
pub(crate) const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;
/// `CreateOptions`: non-alertable synchronous I/O.
pub(crate) const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x0000_0020;
/// `CreateOptions`: open a reparse point itself (`FILE_FLAG_OPEN_REPARSE_POINT`).
pub(crate) const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// `OBJECT_ATTRIBUTES.Attributes`: case-insensitive name lookup.
pub(crate) const OBJ_CASE_INSENSITIVE: u32 = 0x0000_0040;
/// `OBJECT_ATTRIBUTES.Attributes`: fail rather than reparse at any component (Windows 10 1709+).
pub(crate) const OBJ_DONT_REPARSE: u32 = 0x0000_1000;

/// `dwCreationDisposition`: open what exists.
pub(crate) const OPEN_EXISTING: u32 = 3;
/// `dwFlagsAndAttributes`: open a directory.
pub(crate) const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
/// `dwFlagsAndAttributes`: open a reparse point as itself.
pub(crate) const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// `SetThreadErrorMode`: no "insert a disk" critical-error box for a volume root with no medium.
pub(crate) const SEM_FAILCRITICALERRORS: u32 = 0x0001;
/// `SetThreadErrorMode`: no open-file error box either.
pub(crate) const SEM_NOOPENFILEERRORBOX: u32 = 0x8000;

/// `GetFinalPathNameByHandleW` flag returning the volume GUID path (`\\?\Volume{…}\…`).
pub(crate) const VOLUME_NAME_GUID: u32 = 0x1;

/// `PHCM_EXPOSE_PLACEHOLDERS`: expose Cloud Files placeholders as reparse points on this thread.
pub(crate) const PHCM_EXPOSE_PLACEHOLDERS: i8 = 2;

/// `FILE_ATTRIBUTE_READONLY`.
pub(crate) const FILE_ATTRIBUTE_READONLY: u32 = 0x0001;
/// `FILE_ATTRIBUTE_DIRECTORY`.
pub(crate) const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0010;
/// `FILE_ATTRIBUTE_REPARSE_POINT`.
pub(crate) const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
/// `IsReparseTagNameSurrogate` bit: tag names another entity (symlink, junction, or mount point).
pub(crate) const REPARSE_TAG_NAME_SURROGATE: u32 = 0x2000_0000;
/// `IO_REPARSE_TAG_MOUNT_POINT`: junction or volume mount point tag.
pub(crate) const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;

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
/// `FILE_DISPOSITION_INFO_EX.Flags`: unlink immediately with POSIX semantics while handles remain.
pub(crate) const FILE_DISPOSITION_FLAG_POSIX_SEMANTICS: u32 = 0x0002;
/// `FILE_DISPOSITION_INFO_EX.Flags`: delete a read-only file too.
pub(crate) const FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE: u32 = 0x0010;

/// `STATUS_INVALID_PARAMETER`: also an older kernel's answer to `OBJ_DONT_REPARSE`.
pub(crate) const STATUS_INVALID_PARAMETER: u32 = 0xC000_000D;
/// `STATUS_DELETE_PENDING` (flattened to `ERROR_ACCESS_DENIED` by `RtlNtStatusToDosError`).
pub(crate) const STATUS_DELETE_PENDING: u32 = 0xC000_0056;
/// `STATUS_FILE_IS_A_DIRECTORY` (flattened to `ERROR_ACCESS_DENIED` by `RtlNtStatusToDosError`).
pub(crate) const STATUS_FILE_IS_A_DIRECTORY: u32 = 0xC000_00BA;

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

/// `UNICODE_STRING`: a counted (NOT NUL-terminated) UTF-16 name.
#[repr(C)]
pub(crate) struct UNICODE_STRING {
    pub Length: u16,
    pub MaximumLength: u16,
    pub Buffer: *mut u16,
}

/// `OBJECT_ATTRIBUTES`: object name, root directory handle, and `OBJ_*` attributes.
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

/// `FILE_BASIC_INFO`: the four times (FILETIME ticks: 100 ns since 1601) and the attributes.
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

/// `FILE_ID_INFO`: 64-bit volume serial number and 128-bit file ID (`FILE_ID_128`).
#[repr(C)]
pub(crate) struct FILE_ID_INFO {
    pub VolumeSerialNumber: u64,
    pub FileId: [u8; 16],
}

/// `FILE_NAME_INFO`: volume-relative UTF-16 path (`FileNameLength` bytes, not NUL-terminated).
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

/// `FILE_DISPOSITION_INFO_EX`: extended delete disposition flags.
#[repr(C)]
pub(crate) struct FILE_DISPOSITION_INFO_EX {
    pub Flags: u32,
}

/// `FILE_ID_EXTD_DIR_INFO`: one directory entry with its 128-bit file id and reparse tag.
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

/// `FILE_ID_BOTH_DIR_INFO`: the fallback entry for a file system without the extended class.
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

/// `SID_AND_ATTRIBUTES`: a pointer to a SID inside the same buffer, and its group attributes.
#[repr(C)]
pub(crate) struct SID_AND_ATTRIBUTES {
    pub Sid: *mut c_void,
    pub Attributes: u32,
}

/// `TOKEN_USER`: `GetTokenInformation(TokenUser)` result header followed by its SID.
#[repr(C)]
pub(crate) struct TOKEN_USER {
    pub User: SID_AND_ATTRIBUTES,
}

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

#[link(name = "kernel32")]
unsafe extern "system" {
    /// Queries the object-manager target of DOS device `lpDeviceName` (`C:`, `Volume{…}`).
    pub(crate) fn QueryDosDeviceW(
        lpDeviceName: *const u16,
        lpTargetPath: *mut u16,
        ucchMax: u32,
    ) -> u32;
    /// Opens a volume root by its verbatim (`\\?\`) path.
    pub(crate) fn CreateFileW(
        lpFileName: *const u16,
        dwDesiredAccess: u32,
        dwShareMode: u32,
        lpSecurityAttributes: *const c_void,
        dwCreationDisposition: u32,
        dwFlagsAndAttributes: u32,
        hTemplateFile: HANDLE,
    ) -> HANDLE;
    /// One class of facts about an open handle, or one batch of a directory's entries.
    pub(crate) fn GetFileInformationByHandleEx(
        hFile: HANDLE,
        FileInformationClass: i32,
        lpFileInformation: *mut c_void,
        dwBufferSize: u32,
    ) -> i32;
    /// Sets one class of facts on an open handle: here, its delete disposition.
    pub(crate) fn SetFileInformationByHandle(
        hFile: HANDLE,
        FileInformationClass: i32,
        lpFileInformation: *const c_void,
        dwBufferSize: u32,
    ) -> i32;
    /// This thread's critical-error mode; returns a `BOOL`, the old mode in `lpOldMode`.
    pub(crate) fn SetThreadErrorMode(dwNewMode: u32, lpOldMode: *mut u32) -> i32;
    /// Queries the final path of `hFile` formatted according to `dwFlags`.
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
    /// Reads `SecurityInfo` fields of `handle`'s security descriptor (freed with `LocalFree`).
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
    /// Opens the access token of `ProcessHandle`.
    pub(crate) fn OpenProcessToken(
        ProcessHandle: HANDLE,
        DesiredAccess: u32,
        TokenHandle: *mut HANDLE,
    ) -> i32;
    /// Queries `TokenInformationClass` from `TokenHandle` into `TokenInformation`.
    pub(crate) fn GetTokenInformation(
        TokenHandle: HANDLE,
        TokenInformationClass: i32,
        TokenInformation: *mut c_void,
        TokenInformationLength: u32,
        ReturnLength: *mut u32,
    ) -> i32;
    /// Whether `pSid` points at a well-formed SID.
    pub(crate) fn IsValidSid(pSid: *const c_void) -> i32;
    /// The length in bytes of the valid SID at `pSid`.
    pub(crate) fn GetLengthSid(pSid: *const c_void) -> u32;
}

#[link(name = "ntdll")]
unsafe extern "system" {
    /// Opens or creates `ObjectName` relative to `RootDirectory`.
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
    /// Sets this thread's `PHCM_*` cloud-placeholder mode and returns the previous mode.
    pub(crate) fn RtlSetThreadPlaceholderCompatibilityMode(Mode: i8) -> i8;
    /// Returns this thread's `PHCM_*` cloud-placeholder mode.
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn RtlQueryThreadPlaceholderCompatibilityMode() -> i8;
}

/// `FSCTL_GET_REPARSE_POINT` control code.
pub(crate) const FSCTL_GET_REPARSE_POINT: u32 = 0x0009_00A8;
/// `MAXIMUM_REPARSE_DATA_BUFFER_SIZE` (winnt.h): no reparse point's data is longer.
pub(crate) const MAXIMUM_REPARSE_DATA_BUFFER_SIZE: usize = 16 * 1024;

/// `REPARSE_DATA_BUFFER` prefix up to `MountPointReparseBuffer.PathBuffer`.
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
    /// Sends a synchronous control code (`FSCTL_GET_REPARSE_POINT` / `FSCTL_SET_REPARSE_POINT`).
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

/// `FSCTL_SET_REPARSE_POINT` (winioctl.h).
#[cfg(any(test, debug_assertions))]
pub(crate) const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;

/// `REPARSE_GUID_DATA_BUFFER` header preceding a third-party reparse point's payload.
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

/// An `HRESULT`: negative is failure.
#[cfg(any(test, debug_assertions))]
pub(crate) type HRESULT = i32;

/// `CF_SYNC_REGISTRATION`: who provides a sync root.
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

/// `CF_SYNC_POLICIES`: hydration, population, and management policies for a sync root.
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
/// `CF_POPULATION_POLICY_ALWAYS_FULL`: directories are fully populated on disk.
#[cfg(any(test, debug_assertions))]
pub(crate) const CF_POPULATION_POLICY_ALWAYS_FULL: u16 = 3;
/// `CF_CONVERT_FLAG_MARK_IN_SYNC`: mark the converted placeholder as in sync.
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
    /// Registers `SyncRootPath` as a per-user Cloud Files sync root.
    pub(crate) fn CfRegisterSyncRoot(
        SyncRootPath: *const u16,
        Registration: *const CF_SYNC_REGISTRATION,
        Policies: *const CF_SYNC_POLICIES,
        RegisterFlags: u32,
    ) -> HRESULT;
    /// Unregisters the Cloud Files sync root at `SyncRootPath`.
    pub(crate) fn CfUnregisterSyncRoot(SyncRootPath: *const u16) -> HRESULT;
    /// Converts the open `FileHandle` into a Cloud Files placeholder.
    pub(crate) fn CfConvertToPlaceholder(
        FileHandle: HANDLE,
        FileIdentity: *const c_void,
        FileIdentityLength: u32,
        ConvertFlags: u32,
        ConvertUsn: *mut i64,
        Overlapped: *mut c_void,
    ) -> HRESULT;
}
