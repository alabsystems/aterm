// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Raw Win32 declarations for the Windows volume, drive-kind and identity readers (`crate::volumes`) and for the one reader of another process's parameters (`crate::peb`) — the ONLY FFI of this crate on Windows, kept in one auditable file, each call behind a safe wrapper here.

use crate::volumes::FileId;
use std::ffi::{OsStr, OsString, c_void};
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::io::{AsHandle as _, AsRawHandle as _, BorrowedHandle, FromRawHandle as _};
use std::os::windows::io::{OwnedHandle, RawHandle};
use std::path::{Path, PathBuf};

/// Win32 `HANDLE`.
type Handle = *mut c_void;

/// `GetLastError` after `FindNextVolumeW` read the last volume.
const ERROR_NO_MORE_FILES: i32 = 18;
/// `GetLastError`: the buffer was too small (`GetVolumePathNamesForVolumeNameW`).
const ERROR_MORE_DATA: i32 = 234;
/// `GetLastError`: the buffer was too small (`QueryDosDeviceW`).
const ERROR_INSUFFICIENT_BUFFER: i32 = 122;

/// `SetThreadErrorMode`: the system does not display the critical-error box
/// (no disk in the drive); the call fails instead.
const SEM_FAILCRITICALERRORS: u32 = 0x0001;
/// `SetThreadErrorMode`: no message box when a file cannot be found.
const SEM_NOOPENFILEERRORBOX: u32 = 0x8000;

/// `winnt.h`: `FILE_READ_ATTRIBUTES` — all an identity query needs.
const FILE_READ_ATTRIBUTES: u32 = 0x0080;
/// `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`: the look stands
/// in nobody's way, a delete included.
const FILE_SHARE_ALL: u32 = 0x0007;
/// Required to open a directory.
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
/// Open a link (a symbolic link, junction or mount point) as itself.
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
/// `FILE_INFO_BY_HANDLE_CLASS::FileIdInfo`.
const FILE_ID_INFO_CLASS: i32 = 18;
/// `GetVolumeInformationW` flag: the volume is read-only.
pub(crate) const FILE_READ_ONLY_VOLUME: u32 = 0x0008_0000;

/// The most volumes one enumeration reads before it calls itself cut.
const MAX_VOLUMES: usize = 4 * crate::volumes::MAX_MOUNTS;
/// The longest `MULTI_SZ` answer (UTF-16 units) a buffer grows to.
const MULTI_SZ_CAP: u32 = 1 << 16;

/// `FILE_ID_INFO`: the 64-bit volume serial and the 128-bit file id (`FILE_ID_128`, a byte array) — 24 bytes, 8-aligned.
#[repr(C)]
pub(crate) struct FileIdInfo {
    /// `VolumeSerialNumber`.
    pub(crate) volume_serial_number: u64,
    /// `FileId.Identifier`.
    pub(crate) file_id: [u8; 16],
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn FindFirstVolumeW(volume_name: *mut u16, buffer_length: u32) -> Handle;
    fn FindNextVolumeW(find_volume: Handle, volume_name: *mut u16, buffer_length: u32) -> i32;
    fn FindVolumeClose(find_volume: Handle) -> i32;
    fn GetVolumePathNamesForVolumeNameW(
        volume_name: *const u16,
        volume_path_names: *mut u16,
        buffer_length: u32,
        return_length: *mut u32,
    ) -> i32;
    fn QueryDosDeviceW(device_name: *const u16, target_path: *mut u16, max: u32) -> u32;
    fn GetLogicalDrives() -> u32;
    fn GetDiskFreeSpaceExW(
        directory_name: *const u16,
        free_bytes_available_to_caller: *mut u64,
        total_number_of_bytes: *mut u64,
        total_number_of_free_bytes: *mut u64,
    ) -> i32;
    fn GetVolumeInformationW(
        root_path_name: *const u16,
        volume_name_buffer: *mut u16,
        volume_name_size: u32,
        volume_serial_number: *mut u32,
        maximum_component_length: *mut u32,
        file_system_flags: *mut u32,
        file_system_name_buffer: *mut u16,
        file_system_name_size: u32,
    ) -> i32;
    fn GetFileInformationByHandleEx(
        file: Handle,
        file_information_class: i32,
        file_information: *mut c_void,
        buffer_size: u32,
    ) -> i32;
    fn GetThreadErrorMode() -> u32;
    fn SetThreadErrorMode(new_mode: u32, old_mode: *mut u32) -> i32;
    fn GetDriveTypeW(root_path_name: *const u16) -> u32;
    /// `short_path` with each 8.3 short name replaced by its long name
    /// (`crate::volumes::long_name`): the length written, NUL excluded; the
    /// length needed, NUL included, when `buffer` is too small; 0 on failure.
    fn GetLongPathNameW(short_path: *const u16, long_path: *mut u16, buffer: u32) -> u32;
    /// A handle to process `process_id`, null on failure (`crate::peb`).
    fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> Handle;
    /// `WAIT_OBJECT_0` at `milliseconds` 0: the process has exited.
    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    /// The image a process runs, by path (`flags` 0: the Win32 spelling).
    fn QueryFullProcessImageNameW(
        process: Handle,
        flags: u32,
        exe_name: *mut u16,
        size: *mut u32,
    ) -> i32;
    /// Copies `size` bytes of another process's memory into `buffer`: the
    /// process environment block's head, its parameters and their strings.
    fn ReadProcessMemory(
        process: Handle,
        base_address: *const c_void,
        buffer: *mut c_void,
        size: usize,
        number_of_bytes_read: *mut usize,
    ) -> i32;
}

#[link(name = "ntdll")]
unsafe extern "system" {
    /// One class of facts about a process, read through a handle: where its
    /// environment block is (`ProcessBasicInformation`), or its 32-bit one
    /// under WOW64 (`ProcessWow64Information`).
    fn NtQueryInformationProcess(
        process: Handle,
        class: u32,
        information: *mut c_void,
        length: u32,
        return_length: *mut u32,
    ) -> i32;
}

/// `GetDriveTypeW`: the root does not exist or names no volume.
const DRIVE_NO_ROOT_DIR: u32 = 1;
/// `GetDriveTypeW`: a fixed disk — the one kind a reclaim may touch.
pub(crate) const DRIVE_FIXED: u32 = 3;

#[cfg(test)]
#[link(name = "ntdll")]
unsafe extern "system" {
    fn RtlUpcaseUnicodeChar(source_character: u16) -> u16;
}

/// The system's uppercase of the UTF-16 unit `unit` (`RtlUpcaseUnicodeChar`: the table NTFS writes a new volume's `$UpCase` from).
#[cfg(test)]
pub(crate) fn upcase(unit: u16) -> u16 {
    // SAFETY: a pure table lookup of a value; no memory is touched.
    unsafe { RtlUpcaseUnicodeChar(unit) }
}

/// The last Win32 error of this thread, as std reads it.
fn last_error() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// `s` as a NUL-terminated UTF-16 string.
fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// The text of `buf` up to its first NUL.
fn until_nul(buf: &[u16]) -> &[u16] {
    &buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())]
}

/// Are this thread's critical-error and open-file system boxes both off now (`GetThreadErrorMode` holds `SEM_FAILCRITICALERRORS` and `SEM_NOOPENFILEERRORBOX`)? [`crate::errors_quiet`], a test seam.
#[cfg(any(test, debug_assertions))]
pub(crate) fn errors_quiet() -> bool {
    let both = SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX;
    // SAFETY: no arguments; it reads this thread's error mode.
    let mode = unsafe { GetThreadErrorMode() };
    mode & both == both
}

/// The existing `path` with each 8.3 short name replaced by the long name its own directory lists for it (`\\?\C:\Users\Long Folder` of `\\?\C:\Users\LONGFO~1`): `GetLongPathNameW`, under [`quiet`], every other part of the spelling kept and no link followed (`crate::volumes::long_name`).
pub(crate) fn long_path(path: &Path) -> Result<PathBuf, i32> {
    let _quiet = quiet();
    let short = wide(path.as_os_str());
    let mut buf = vec![0u16; 32768];
    let cap = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    // SAFETY: `short` is NUL-terminated; `buf` is `cap` writable units, and the call writes at most that many (or answers the length it needs, which is then at least `cap` and refused below).
    let n = unsafe { GetLongPathNameW(short.as_ptr(), buf.as_mut_ptr(), cap) };
    if n == 0 {
        return Err(last_error());
    }
    let long = usize::try_from(n)
        .ok()
        .filter(|_| n < cap)
        .and_then(|n| buf.get(..n))
        .ok_or(ERROR_INSUFFICIENT_BUFFER)?;
    Ok(PathBuf::from(OsString::from_wide(long)))
}

#[cfg(any(test, debug_assertions))]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetShortPathNameW(long_path: *const u16, short_path: *mut u16, buffer: u32) -> u32;
}

/// The 8.3 short spelling of the existing `path` as the file system generated it (`GetShortPathNameW`), for a test that must make a process or a tab hold a short-spelled path (`crate::short_spelling`).
#[cfg(any(test, debug_assertions))]
pub(crate) fn short_path(path: &Path) -> Option<PathBuf> {
    let long = wide(path.as_os_str());
    let mut buf = vec![0u16; 32768];
    let cap = u32::try_from(buf.len()).ok()?;
    // SAFETY: `long` is NUL-terminated; `buf` is `cap` writable units, and the call writes at most that many (or answers the length it needs, which is then more than `cap` and refused below).
    let n = unsafe { GetShortPathNameW(long.as_ptr(), buf.as_mut_ptr(), cap) };
    let short = buf.get(..usize::try_from(n).ok()?).filter(|_| n != 0)?;
    let short = PathBuf::from(OsString::from_wide(short));
    (short.as_os_str() != path.as_os_str()).then_some(short)
}

/// This thread's error mode with the critical-error and open-file boxes off, restored when dropped ([`quiet`]): on the thread that made it, so neither `Send` nor `Sync`.
pub(crate) struct Quiet {
    /// The mode to restore; `None` when setting it failed.
    old: Option<u32>,
    /// Keeps the guard on the thread whose mode it holds.
    _this_thread: std::marker::PhantomData<*const ()>,
}

/// Turn this thread's critical-error and open-file system boxes off until the answer drops: a query on an empty removable or optical drive then fails with `ERROR_NOT_READY` instead of waiting on a box nobody asked for.
pub(crate) fn quiet() -> Quiet {
    // SAFETY: no arguments; it reads this thread's error mode.
    let now = unsafe { GetThreadErrorMode() };
    let mut old = 0u32;
    // SAFETY: `old` is a writable u32 for the previous mode; the call sets only the calling thread's mode.
    let ok = unsafe {
        SetThreadErrorMode(
            now | SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX,
            &raw mut old,
        )
    } != 0;
    Quiet {
        old: ok.then_some(old),
        _this_thread: std::marker::PhantomData,
    }
}

impl Drop for Quiet {
    fn drop(&mut self) {
        if let Some(old) = self.old {
            // SAFETY: a null old-mode pointer is allowed; it restores the mode this guard replaced on this same thread.
            unsafe { SetThreadErrorMode(old, std::ptr::null_mut()) };
        }
    }
}

/// This thread's error mode now (`GetThreadErrorMode`), for a test of [`quiet`].
#[cfg(test)]
pub(crate) fn thread_error_mode() -> u32 {
    // SAFETY: no arguments; it reads this thread's error mode.
    unsafe { GetThreadErrorMode() }
}

/// Set this thread's error mode to `mode`, for a test of [`quiet`] that must start from a mode without its bits; `false` when the call failed.
#[cfg(test)]
pub(crate) fn set_thread_error_mode(mode: u32) -> bool {
    // SAFETY: a null old-mode pointer is allowed; it sets only the calling thread's mode.
    unsafe { SetThreadErrorMode(mode, std::ptr::null_mut()) != 0 }
}

/// The two bits [`quiet`] sets, for its test.
#[cfg(test)]
pub(crate) const QUIET_BITS: u32 = SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX;

/// Every volume the mount manager knows (`\\?\Volume{…}\`), and whether the enumeration finished: `FindFirstVolumeW` / `FindNextVolumeW` until `ERROR_NO_MORE_FILES`.
pub(crate) fn volume_names() -> (Vec<String>, bool) {
    let mut buf = [0u16; 260];
    let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    // SAFETY: `buf` is `len` writable UTF-16 units; the call writes at most that many, NUL included.
    let find = unsafe { FindFirstVolumeW(buf.as_mut_ptr(), len) };
    if find.addr() == usize::MAX {
        return (Vec::new(), false);
    }
    let mut names = Vec::new();
    let complete = loop {
        let Ok(name) = String::from_utf16(until_nul(&buf)) else {
            break false;
        };
        names.push(name);
        if names.len() > MAX_VOLUMES {
            break false;
        }
        // SAFETY: `find` is the open search handle; `buf` as above.
        if unsafe { FindNextVolumeW(find, buf.as_mut_ptr(), len) } == 0 {
            break last_error() == ERROR_NO_MORE_FILES;
        }
    };
    // SAFETY: `find` is the open search handle, closed once, here.
    unsafe { FindVolumeClose(find) };
    (names, complete)
}

/// Every path volume `name` (`\\?\Volume{…}\`) is mounted at — drive letters and folder mount points (`C:\`, `C:\mnt\data\`) — from the mount manager, which asks no file system.
pub(crate) fn volume_path_names(name: &str) -> Option<Vec<PathBuf>> {
    let name = wide(OsStr::new(name));
    let mut buf = vec![0u16; 1024];
    for _ in 0..2 {
        let len = u32::try_from(buf.len()).ok()?;
        let mut need = 0u32;
        // SAFETY: `name` is NUL-terminated; `buf` is `len` writable units; `need` is a writable u32 for the length written or wanted.
        let ok = unsafe {
            GetVolumePathNamesForVolumeNameW(name.as_ptr(), buf.as_mut_ptr(), len, &raw mut need)
        };
        if ok != 0 {
            let used = usize::try_from(need).map_or(buf.len(), |n| n.min(buf.len()));
            return Some(
                crate::volumes::multi_sz(&buf[..used])
                    .into_iter()
                    .map(|p| PathBuf::from(OsString::from_wide(p)))
                    .collect(),
            );
        }
        if last_error() != ERROR_MORE_DATA || need == 0 || need > MULTI_SZ_CAP {
            return None;
        }
        buf = vec![0u16; usize::try_from(need).ok()?];
    }
    None
}

/// What the object manager says `name` (`Volume{…}`, `C:`) points at (`\Device\HarddiskVolume3`, `\??\C:\work` for a `subst`, a redirector's device for a network letter): `QueryDosDeviceW`'s first answer, in this session's view (a per-session `subst` or `net use` first).
pub(crate) fn dos_device(name: &str) -> Option<String> {
    let name = wide(OsStr::new(name));
    let mut buf = vec![0u16; 1024];
    for _ in 0..2 {
        let len = u32::try_from(buf.len()).ok()?;
        // SAFETY: `name` is NUL-terminated; `buf` is `len` writable units, and the call writes at most that many.
        let n = unsafe { QueryDosDeviceW(name.as_ptr(), buf.as_mut_ptr(), len) };
        if n != 0 {
            let used = usize::try_from(n).map_or(buf.len(), |n| n.min(buf.len()));
            let first = crate::volumes::multi_sz(&buf[..used]).into_iter().next()?;
            return String::from_utf16(first).ok();
        }
        if last_error() != ERROR_INSUFFICIENT_BUFFER {
            return None;
        }
        buf = vec![0u16; usize::try_from(MULTI_SZ_CAP).ok()?];
    }
    None
}

/// The drive letters this process sees, bit 0 `A:`: `GetLogicalDrives`.
pub(crate) fn logical_drives() -> u32 {
    // SAFETY: no arguments.
    unsafe { GetLogicalDrives() }
}

/// The bytes free to this caller (its quota honoured: the `f_bavail` analogue) and the volume's size, through `root` (a verbatim directory, `\\?\Volume{…}\`): `GetDiskFreeSpaceExW`.
pub(crate) fn disk_free(root: &Path) -> Result<(u64, u64), i32> {
    let root = wide(root.as_os_str());
    let (mut free, mut total) = (0u64, 0u64);
    let _quiet = quiet();
    // SAFETY: `root` is NUL-terminated; `free` and `total` are writable u64s (`ULARGE_INTEGER`'s layout); the total-free out-param is optional.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            root.as_ptr(),
            &raw mut free,
            &raw mut total,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    Ok((free, total))
}

/// The file-system flags (`FILE_READ_ONLY_VOLUME`, …) and the file system's name (`NTFS`, `ReFS`, `FAT32`, `exFAT`) of the volume at `root` (a verbatim root, `\\?\Volume{…}\`): `GetVolumeInformationW`.
pub(crate) fn volume_info(root: &Path) -> Result<(u32, String), i32> {
    let root = wide(root.as_os_str());
    let mut flags = 0u32;
    let mut name = [0u16; 64];
    let len = u32::try_from(name.len()).unwrap_or(0);
    let _quiet = quiet();
    // SAFETY: `root` is NUL-terminated; `flags` is a writable u32; `name` is `len` writable units; every other out-param is optional and null.
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut flags,
            name.as_mut_ptr(),
            len,
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    Ok((flags, String::from_utf16_lossy(until_nul(&name))))
}

/// What kind of drive the volume at `root` (a verbatim root, `\\?\Volume{…}\`) is: `GetDriveTypeW`, which asks the volume's device and reads no file system — `DRIVE_FIXED`, `DRIVE_REMOVABLE`, `DRIVE_CDROM`, `DRIVE_RAMDISK`, `DRIVE_REMOTE`.
pub(crate) fn drive_type(root: &Path) -> Option<u32> {
    let root = wide(root.as_os_str());
    let _quiet = quiet();
    // SAFETY: `root` is NUL-terminated and outlives the call, which only reads it.
    let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
    (kind > DRIVE_NO_ROOT_DIR).then_some(kind)
}

/// The identity of the file or directory at `path` (`crate::volumes:: file_id`): opened for `FILE_READ_ATTRIBUTES` alone, every sharing mode granted, `OPEN_EXISTING`, `FILE_FLAG_BACKUP_SEMANTICS`, plus `FILE_FLAG_OPEN_REPARSE_POINT` unless `follow` (a link answers as itself). std's open spells a long path verbatim itself.
pub(crate) fn file_id(path: &Path, follow: bool) -> Result<FileId, i32> {
    let _quiet = quiet();
    let flags = FILE_FLAG_BACKUP_SEMANTICS
        | if follow {
            0
        } else {
            FILE_FLAG_OPEN_REPARSE_POINT
        };
    let f = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_ALL)
        .custom_flags(flags)
        .open(path)
        .map_err(|e| e.raw_os_error().unwrap_or(0))?;
    file_id_of_handle(f.as_handle())
}

/// The identity of the open `handle`: `GetFileInformationByHandleEx (FileIdInfo)` — the 64-bit volume serial and the 128-bit file id, read little-endian (an NTFS id's low 64 bits are its file reference number).
pub(crate) fn file_id_of_handle(handle: BorrowedHandle<'_>) -> Result<FileId, i32> {
    let mut info = FileIdInfo {
        volume_serial_number: 0,
        file_id: [0; 16],
    };
    let size = u32::try_from(std::mem::size_of::<FileIdInfo>()).unwrap_or(0);
    // SAFETY: `handle` is open for the borrow; `info` is a writable `FILE_ID_INFO` of exactly `size` bytes.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle.as_raw_handle(),
            FILE_ID_INFO_CLASS,
            (&raw mut info).cast(),
            size,
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    Ok(FileId {
        dev: info.volume_serial_number,
        ino: u128::from_le_bytes(info.file_id),
    })
}

/// `OpenProcess` right: the image, where the environment block is.
pub(crate) const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
/// `OpenProcess` right: the environment block and its strings.
pub(crate) const PROCESS_VM_READ: u32 = 0x0010;
/// `OpenProcess` right: a wait on the handle reads the process's exit.
pub(crate) const SYNCHRONIZE: u32 = 0x0010_0000;
/// A wait on a signalled (exited) process.
const WAIT_OBJECT_0: u32 = 0;
/// `NtQueryInformationProcess` class: where a process's PEB is.
const PROCESS_BASIC_INFORMATION: u32 = 0;
/// `NtQueryInformationProcess` class: a WOW64 process's `PEB32`, else null.
const PROCESS_WOW64_INFORMATION: u32 = 26;

/// `PROCESS_BASIC_INFORMATION`: where a process's environment block is (`NtQueryInformationProcess`, class 0).
#[repr(C)]
#[derive(Default)]
pub(crate) struct ProcessBasicInformation {
    /// `ExitStatus`.
    pub(crate) exit_status: isize,
    /// `PebBaseAddress`.
    pub(crate) peb_base_address: usize,
    /// `AffinityMask`.
    pub(crate) affinity_mask: usize,
    /// `BasePriority`.
    pub(crate) base_priority: isize,
    /// `UniqueProcessId`.
    pub(crate) unique_process_id: usize,
    /// `InheritedFromUniqueProcessId`.
    pub(crate) inherited_from_unique_process_id: usize,
}

/// A structure of plain integers, every byte pattern of which is a valid value: what a read of ANOTHER process's memory may fill whole ([`read_plain`]).
/// # Safety
/// Implement it only for a `#[repr(C)]` structure whose fields are integers, arrays of integers or other `Plain` structures.
pub(crate) unsafe trait Plain: Copy {}

/// The head of a 64-bit process's environment block (`PEB`), up to its `ProcessParameters` pointer: where its `RTL_USER_PROCESS_PARAMETERS` are.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct PebHead64 {
    /// `InheritedAddressSpace`, `ReadImageFileExecOptions`, `BeingDebugged`, `BitField`, and the 4 bytes of padding before `Mutant`.
    pub(crate) head: [u8; 8],
    /// `Mutant`.
    pub(crate) mutant: u64,
    /// `ImageBaseAddress`.
    pub(crate) image_base_address: u64,
    /// `Ldr`.
    pub(crate) ldr: u64,
    /// `ProcessParameters`.
    pub(crate) process_parameters: u64,
}

/// [`PebHead64`] of a 32-bit process under WOW64 (`PEB32`): every pointer 4 bytes.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct PebHead32 {
    /// As [`PebHead64::head`], no padding.
    pub(crate) head: [u8; 4],
    /// `Mutant`.
    pub(crate) mutant: u32,
    /// `ImageBaseAddress`.
    pub(crate) image_base_address: u32,
    /// `Ldr`.
    pub(crate) ldr: u32,
    /// `ProcessParameters`.
    pub(crate) process_parameters: u32,
}

/// A `UNICODE_STRING` in a 64-bit process's memory: `length` bytes of UTF-16 at the address `buffer` THERE.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct RemoteString64 {
    /// `Length`, in bytes, no terminating NUL.
    pub(crate) length: u16,
    /// `MaximumLength`.
    pub(crate) maximum_length: u16,
    /// The padding before `Buffer`.
    pub(crate) _pad: u32,
    /// `Buffer`: an address in that process.
    pub(crate) buffer: u64,
}

/// [`RemoteString64`] in a WOW64 process (`UNICODE_STRING32`).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct RemoteString32 {
    /// `Length`, in bytes.
    pub(crate) length: u16,
    /// `MaximumLength`.
    pub(crate) maximum_length: u16,
    /// `Buffer`.
    pub(crate) buffer: u32,
}

/// `RTL_DRIVE_LETTER_CURDIR` (64-bit).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct DriveLetterCurdir64 {
    /// `Flags`.
    pub(crate) flags: u16,
    /// `Length`.
    pub(crate) length: u16,
    /// `TimeStamp`.
    pub(crate) time_stamp: u32,
    /// `DosPath`.
    pub(crate) dos_path: RemoteString64,
}

/// `RTL_DRIVE_LETTER_CURDIR` (WOW64).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct DriveLetterCurdir32 {
    /// `Flags`.
    pub(crate) flags: u16,
    /// `Length`.
    pub(crate) length: u16,
    /// `TimeStamp`.
    pub(crate) time_stamp: u32,
    /// `DosPath`.
    pub(crate) dos_path: RemoteString32,
}

/// `RTL_USER_PROCESS_PARAMETERS` of a 64-bit process, through `EnvironmentVersion` (the structure goes on past it on Windows 8 and later, and every Windows this crate runs on lays at least this much): its working directory (`CurrentDirectory.DosPath`), its command line, and its environment block with that block's size in bytes.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct ProcessParameters64 {
    /// `MaximumLength`.
    pub(crate) maximum_length: u32,
    /// `Length`: how much of the structure the process lays.
    pub(crate) length: u32,
    /// `RTL_USER_PROC_PARAMS_NORMALIZED` (`0x1`) once its strings' `Buffer`s are addresses (before, offsets from the structure).
    pub(crate) flags: u32,
    /// `DebugFlags`.
    pub(crate) debug_flags: u32,
    /// `ConsoleHandle`.
    pub(crate) console_handle: u64,
    /// `ConsoleFlags`.
    pub(crate) console_flags: u32,
    /// The padding before `StandardInput`.
    pub(crate) _pad0: u32,
    /// `StandardInput`.
    pub(crate) standard_input: u64,
    /// `StandardOutput`.
    pub(crate) standard_output: u64,
    /// `StandardError`.
    pub(crate) standard_error: u64,
    /// `CurrentDirectory.DosPath` (`CURDIR`'s first field).
    pub(crate) current_directory: RemoteString64,
    /// `CurrentDirectory.Handle`.
    pub(crate) current_directory_handle: u64,
    /// `DllPath`.
    pub(crate) dll_path: RemoteString64,
    /// `ImagePathName`.
    pub(crate) image_path_name: RemoteString64,
    /// `CommandLine`.
    pub(crate) command_line: RemoteString64,
    /// `Environment`: the block's address.
    pub(crate) environment: u64,
    /// `StartingX` through `ShowWindowFlags`.
    pub(crate) window: [u32; 9],
    /// The padding before `WindowTitle`.
    pub(crate) _pad1: u32,
    /// `WindowTitle`.
    pub(crate) window_title: RemoteString64,
    /// `DesktopInfo`.
    pub(crate) desktop_info: RemoteString64,
    /// `ShellInfo`.
    pub(crate) shell_info: RemoteString64,
    /// `RuntimeData`.
    pub(crate) runtime_data: RemoteString64,
    /// `CurrentDirectores` (sic, the SDK's spelling).
    pub(crate) current_directores: [DriveLetterCurdir64; 32],
    /// `EnvironmentSize`, in bytes.
    pub(crate) environment_size: u64,
    /// `EnvironmentVersion`.
    pub(crate) environment_version: u64,
}

/// [`ProcessParameters64`] of a WOW64 process (`RTL_USER_PROCESS_PARAMETERS32`).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct ProcessParameters32 {
    /// `MaximumLength`.
    pub(crate) maximum_length: u32,
    /// `Length`.
    pub(crate) length: u32,
    /// As [`ProcessParameters64::flags`].
    pub(crate) flags: u32,
    /// `DebugFlags`.
    pub(crate) debug_flags: u32,
    /// `ConsoleHandle`.
    pub(crate) console_handle: u32,
    /// `ConsoleFlags`.
    pub(crate) console_flags: u32,
    /// `StandardInput`.
    pub(crate) standard_input: u32,
    /// `StandardOutput`.
    pub(crate) standard_output: u32,
    /// `StandardError`.
    pub(crate) standard_error: u32,
    /// `CurrentDirectory.DosPath`.
    pub(crate) current_directory: RemoteString32,
    /// `CurrentDirectory.Handle`.
    pub(crate) current_directory_handle: u32,
    /// `DllPath`.
    pub(crate) dll_path: RemoteString32,
    /// `ImagePathName`.
    pub(crate) image_path_name: RemoteString32,
    /// `CommandLine`.
    pub(crate) command_line: RemoteString32,
    /// `Environment`.
    pub(crate) environment: u32,
    /// `StartingX` through `ShowWindowFlags`.
    pub(crate) window: [u32; 9],
    /// `WindowTitle`.
    pub(crate) window_title: RemoteString32,
    /// `DesktopInfo`.
    pub(crate) desktop_info: RemoteString32,
    /// `ShellInfo`.
    pub(crate) shell_info: RemoteString32,
    /// `RuntimeData`.
    pub(crate) runtime_data: RemoteString32,
    /// `CurrentDirectores`.
    pub(crate) current_directores: [DriveLetterCurdir32; 32],
    /// `EnvironmentSize`, in bytes.
    pub(crate) environment_size: u32,
    /// `EnvironmentVersion`.
    pub(crate) environment_version: u32,
}

// SAFETY: each is `#[repr(C)]` over integers, integer arrays and `Plain` structures only: every byte pattern is a valid value.
unsafe impl Plain for PebHead64 {}
// SAFETY: as above.
unsafe impl Plain for PebHead32 {}
// SAFETY: as above.
unsafe impl Plain for RemoteString64 {}
// SAFETY: as above.
unsafe impl Plain for RemoteString32 {}
// SAFETY: as above.
unsafe impl Plain for DriveLetterCurdir64 {}
// SAFETY: as above.
unsafe impl Plain for DriveLetterCurdir32 {}
// SAFETY: as above.
unsafe impl Plain for ProcessParameters64 {}
// SAFETY: as above.
unsafe impl Plain for ProcessParameters32 {}

/// Process `pid` opened with `access`, or `None` when the open is refused (no such process, another account's, or one that refuses this access).
pub(crate) fn open_process(pid: u32, access: u32) -> Option<OwnedHandle> {
    // SAFETY: a plain open by pid; a handle it returns is owned below.
    let h = unsafe { OpenProcess(access, 0, pid) };
    if h.is_null() {
        return None;
    }
    // SAFETY: `h` is a live handle this call opened, owned by nothing else: the `OwnedHandle` closes it once.
    Some(unsafe { OwnedHandle::from_raw_handle(h as RawHandle) })
}

/// Has the process `h` exited? Only a signalled handle says so (one opened with [`SYNCHRONIZE`]); a wait that fails answers "running", so a read that failed leaves its process unknown, never exited.
pub(crate) fn exited(h: BorrowedHandle<'_>) -> bool {
    // SAFETY: `h` is a live process handle for the borrow.
    unsafe { WaitForSingleObject(h.as_raw_handle(), 0) == WAIT_OBJECT_0 }
}

/// The image `h` runs, as `QueryFullProcessImageNameW` spells it (`C:\...\x.exe`), or `None` when the call fails.
pub(crate) fn image_path(h: BorrowedHandle<'_>) -> Option<PathBuf> {
    let mut buf = vec![0u16; 32768];
    let mut len = u32::try_from(buf.len()).ok()?;
    // SAFETY: `h` is live; `buf` is `len` writable units and `len` a live local the call updates to the length written.
    let ok =
        unsafe { QueryFullProcessImageNameW(h.as_raw_handle(), 0, buf.as_mut_ptr(), &raw mut len) };
    let name = buf.get(..usize::try_from(len).ok()?).filter(|_| ok != 0)?;
    Some(PathBuf::from(OsString::from_wide(name)))
}

/// The PEB base of a native process (`ProcessBasicInformation`), or `None`.
pub(crate) fn basic_peb(h: BorrowedHandle<'_>) -> Option<u64> {
    let mut info = ProcessBasicInformation::default();
    let mut ret: u32 = 0;
    let size = u32::try_from(std::mem::size_of::<ProcessBasicInformation>()).ok()?;
    // SAFETY: `h` is live; `info` is exactly one structure of `size` bytes.
    let st = unsafe {
        NtQueryInformationProcess(
            h.as_raw_handle(),
            PROCESS_BASIC_INFORMATION,
            (&raw mut info).cast(),
            size,
            &raw mut ret,
        )
    };
    (st >= 0 && info.peb_base_address != 0).then_some(info.peb_base_address as u64)
}

/// The 32-bit PEB base of a WOW64 process, or `None` when the process is native (`ProcessWow64Information` answers a null pointer) or the query fails (read as native, where [`basic_peb`] then decides).
pub(crate) fn wow64_peb(h: BorrowedHandle<'_>) -> Option<u64> {
    let mut peb32: usize = 0;
    let mut ret: u32 = 0;
    let size = u32::try_from(std::mem::size_of::<usize>()).ok()?;
    // SAFETY: `h` is live; the out buffer is one pointer-sized integer.
    let st = unsafe {
        NtQueryInformationProcess(
            h.as_raw_handle(),
            PROCESS_WOW64_INFORMATION,
            (&raw mut peb32).cast(),
            size,
            &raw mut ret,
        )
    };
    (st >= 0 && peb32 != 0).then_some(peb32 as u64)
}

/// One `T` of `h`'s memory at `addr`, read whole, or `None`.
pub(crate) fn read_plain<T: Plain>(h: BorrowedHandle<'_>, addr: u64) -> Option<T> {
    let size = std::mem::size_of::<T>();
    let mut out = std::mem::MaybeUninit::<T>::zeroed();
    let mut got: usize = 0;
    let at = usize::try_from(addr).ok()? as *const c_void;
    // SAFETY: `h` is live and opened with PROCESS_VM_READ; `out` is `size` writable bytes; `at` is an address in `h`'s process, which the kernel reads and this process never dereferences.
    let ok = unsafe {
        ReadProcessMemory(
            h.as_raw_handle(),
            at,
            out.as_mut_ptr().cast(),
            size,
            &raw mut got,
        )
    };
    if ok == 0 || got != size {
        return None;
    }
    // SAFETY: zeroed, then every byte written by the read; `T: Plain` makes every byte pattern a valid `T`.
    Some(unsafe { out.assume_init() })
}

/// The string of `len` bytes at `addr` in `h`, as UTF-16 units, read whole: empty for no bytes; `None` for an odd length (no UTF-16 string), a null address with a length, or a read that fails or comes up short.
pub(crate) fn read_units(h: BorrowedHandle<'_>, addr: u64, len: usize) -> Option<Vec<u16>> {
    if len == 0 {
        return Some(Vec::new());
    }
    if addr == 0 || len & 1 != 0 {
        return None;
    }
    let mut units = vec![0u16; len / 2];
    let mut got: usize = 0;
    let at = usize::try_from(addr).ok()? as *const c_void;
    // SAFETY: `h` is live and PROCESS_VM_READ; `units` is `len` writable bytes; `at` is read by the kernel, never dereferenced here.
    let ok = unsafe {
        ReadProcessMemory(
            h.as_raw_handle(),
            at,
            units.as_mut_ptr().cast(),
            len,
            &raw mut got,
        )
    };
    (ok != 0 && got == len).then_some(units)
}

#[cfg(test)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn DeviceIoControl(
        device: Handle,
        io_control_code: u32,
        in_buffer: *const c_void,
        in_buffer_size: u32,
        out_buffer: *mut c_void,
        out_buffer_size: u32,
        bytes_returned: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
}

/// `FSCTL_SET_REPARSE_POINT` (winioctl.h).
#[cfg(test)]
const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
/// `IO_REPARSE_TAG_MOUNT_POINT`: a junction (and a volume mount point).
#[cfg(test)]
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;

/// Make `link`, a new directory, a junction to the directory `target` (a verbatim or plain drive path): the reparse data `mklink /J` writes — an `IO_REPARSE_TAG_MOUNT_POINT` whose substitute name is `\??\C:\…` and whose print name is the plain path — set on the empty directory through `FSCTL_SET_REPARSE_POINT`, which needs no privilege on a directory this account owns.
/// # Errors
/// Making the directory, opening it, or the control code failed.
#[cfg(test)]
pub(crate) fn make_junction(link: &Path, target: &Path) -> std::io::Result<()> {
    let plain = crate::paths::plain(target).into_owned();
    let print: Vec<u16> = plain.as_os_str().encode_wide().collect();
    let substitute: Vec<u16> = OsStr::new(r"\??\")
        .encode_wide()
        .chain(plain.as_os_str().encode_wide())
        .collect();
    let bytes =
        |n: usize| u16::try_from(n).map_err(|_| std::io::Error::other("a name too long to link"));
    let names = (substitute.len() + 1 + print.len() + 1) * 2;
    let header = 8 + 8;
    let fields = [
        bytes(header - 8 + names)?,
        0,
        0,
        bytes(substitute.len() * 2)?,
        bytes((substitute.len() + 1) * 2)?,
        bytes(print.len() * 2)?,
    ];
    let mut data: Vec<u8> = Vec::with_capacity(header + names);
    data.extend(IO_REPARSE_TAG_MOUNT_POINT.to_ne_bytes());
    for unit in fields
        .iter()
        .chain(&substitute)
        .chain(&[0])
        .chain(&print)
        .chain(&[0])
    {
        data.extend(unit.to_ne_bytes());
    }
    std::fs::create_dir(link)?;
    let dir = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(link)?;
    let size = u32::try_from(data.len()).map_err(std::io::Error::other)?;
    let mut returned = 0u32;
    // SAFETY: `dir` is a live handle open for writing; the input is `data`, alive for the call and exactly `size` bytes; no output buffer; a synchronous call (no OVERLAPPED).
    let ok = unsafe {
        DeviceIoControl(
            dir.as_raw_handle(),
            FSCTL_SET_REPARSE_POINT,
            data.as_ptr().cast(),
            size,
            std::ptr::null_mut(),
            0,
            &raw mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
